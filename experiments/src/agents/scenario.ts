import { generateText, stepCountIs, type LanguageModel, type ModelMessage } from 'ai';
import { MAX_STEPS_PER_REQUEST } from '../config.js';
import type { RequestMetrics } from '../metrics.js';
import type { ScenarioDefinition } from './scenarios.js';
import type { DynamicToolRegistry, ToolkitFactory } from './toolkit.js';

export interface ScenarioOptions {
  definition: ScenarioDefinition;
  /** Creates a fresh toolkit instance (new database / log store) for this run. */
  makeToolkit: ToolkitFactory;
  model: LanguageModel;
  userRequests: string[];
  onProgress?: (line: string) => void;
}

export interface ScenarioResult {
  definition: Pick<ScenarioDefinition, 'id' | 'label' | 'description'>;
  requests: RequestMetrics[];
  /** Names of the dynamic tools the agent created (empty unless the scenario allows it). */
  createdTools: string[];
}

interface StepMetrics {
  usage: {
    inputTokens?: number | undefined;
    outputTokens?: number | undefined;
    reasoningTokens?: number | undefined;
    cachedInputTokens?: number | undefined;
  };
  toolCalls: { toolName: string; input: unknown }[];
}

/**
 * Runs one full conversation: the scripted passenger requests in order, with the
 * message history carried across requests (like a real help-desk shift).
 *
 * The tool loop is stepped manually (one model step per generateText call) so that the
 * toolset can be refreshed between steps: a dynamic tool the agent creates in step N is
 * callable from step N+1 onwards. Scenario A simply never changes its toolset.
 */
export async function runScenario(options: ScenarioOptions): Promise<ScenarioResult> {
  const { definition } = options;
  // Fresh but byte-identical toolkit state per scenario; the agent starts knowing nothing.
  const toolkit = options.makeToolkit();
  const registry: DynamicToolRegistry = {};
  const baseTools = {
    ...toolkit.baseTools,
    ...(definition.withMetaTool ? { [toolkit.metaTool.name]: toolkit.metaTool.make(registry) } : {}),
  };
  const currentTools = () => ({ ...baseTools, ...registry });

  const messages: ModelMessage[] = [];
  const requests: RequestMetrics[] = [];

  for (const [index, userRequest] of options.userRequests.entries()) {
    messages.push({ role: 'user', content: userRequest });

    const startedAt = performance.now();
    const steps: StepMetrics[] = [];
    let finalText = '';

    for (let step = 0; step < MAX_STEPS_PER_REQUEST; step++) {
      const result = await withRetries(() =>
        generateText({
          model: options.model,
          system: definition.systemPrompt,
          messages,
          tools: currentTools(),
          stopWhen: stepCountIs(1),
        }),
      );
      messages.push(...result.response.messages);
      finalText = result.text;

      steps.push({
        usage: result.usage,
        toolCalls: result.toolCalls
          .filter((call) => call != null)
          .map((call) => ({ toolName: String(call.toolName), input: call.input as unknown })),
      });

      if (result.finishReason !== 'tool-calls') break;
    }

    const durationMs = Math.round(performance.now() - startedAt);
    const metrics = collectRequestMetrics(index + 1, userRequest, steps, finalText, durationMs);
    requests.push(metrics);
    options.onProgress?.(formatProgressLine(definition.id, metrics));
  }

  return {
    definition: { id: definition.id, label: definition.label, description: definition.description },
    requests,
    createdTools: Object.keys(registry),
  };
}

function collectRequestMetrics(
  index: number,
  userRequest: string,
  steps: StepMetrics[],
  finalText: string,
  durationMs: number,
): RequestMetrics {
  let inputTokens = 0;
  let outputTokens = 0;
  let reasoningTokens = 0;
  let cachedInputTokens = 0;
  let codeCharsAuthored = 0;
  let longestCode = '';
  const toolCalls: Record<string, number> = {};

  for (const step of steps) {
    inputTokens += step.usage.inputTokens ?? 0;
    outputTokens += step.usage.outputTokens ?? 0;
    reasoningTokens += step.usage.reasoningTokens ?? 0;
    cachedInputTokens += step.usage.cachedInputTokens ?? 0;

    for (const call of step.toolCalls) {
      toolCalls[call.toolName] = (toolCalls[call.toolName] ?? 0) + 1;
      const code = authoredCode(call.toolName, call.input);
      if (code !== null) {
        codeCharsAuthored += code.length;
        if (code.length > longestCode.length) longestCode = code;
      }
    }
  }

  return {
    index,
    userRequest,
    steps: steps.length,
    toolCalls,
    inputTokens,
    outputTokens,
    reasoningTokens,
    cachedInputTokens,
    codeCharsAuthored,
    longestCode,
    durationMs,
    finalAnswer: finalText,
  };
}

/** Code the model had to write out, character by character, in this tool call (null if none). */
function authoredCode(toolName: string, input: unknown): string | null {
  if (toolName === 'runQuery' || toolName === 'createQueryTool') {
    const sql = (input as { sql?: unknown }).sql;
    return typeof sql === 'string' ? sql : null;
  }
  if (toolName === 'runScript' || toolName === 'createFunctionTool') {
    const script = (input as { script?: unknown }).script;
    return typeof script === 'string' ? script : null;
  }
  return null;
}

function formatProgressLine(id: string, metrics: RequestMetrics): string {
  const tools = Object.entries(metrics.toolCalls)
    .map(([name, count]) => `${name}×${count}`)
    .join(', ');
  return [
    `[${id}] request ${metrics.index}: ${metrics.steps} steps,`,
    `in=${metrics.inputTokens} out=${metrics.outputTokens} (reasoning=${metrics.reasoningTokens}, cached=${metrics.cachedInputTokens}),`,
    `codeChars=${metrics.codeCharsAuthored}, ${(metrics.durationMs / 1000).toFixed(1)}s | ${tools}`,
  ].join(' ');
}

async function withRetries<T>(fn: () => Promise<T>, attempts = 3, baseDelayMs = 5_000): Promise<T> {
  let lastError: unknown;
  for (let attempt = 1; attempt <= attempts; attempt++) {
    try {
      return await fn();
    } catch (error) {
      lastError = error;
      if (attempt < attempts) await sleep(baseDelayMs * attempt);
    }
  }
  throw lastError;
}

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

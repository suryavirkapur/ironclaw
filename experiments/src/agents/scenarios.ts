import {
  SYSTEM_PROMPT_SCRIPT_A,
  SYSTEM_PROMPT_SCRIPT_B,
  SYSTEM_PROMPT_REPOSITORY_A,
  SYSTEM_PROMPT_REPOSITORY_B,
  SYSTEM_PROMPT_SQL_A,
  SYSTEM_PROMPT_SQL_B,
} from './prompts.js';

/** What a scenario gives the agent beyond the toolkit's base tools. */
export interface ScenarioDefinition {
  id: string;
  label: string;
  /** One-line description used in the report. */
  description: string;
  systemPrompt: string;
  /** Add the meta-tool (createQueryTool / createFunctionTool) for runtime synthesis. */
  withMetaTool: boolean;
}

interface ScenarioFlags {
  withMetaTool: boolean;
}

const FLAGS_A: ScenarioFlags = { withMetaTool: false };
const FLAGS_B: ScenarioFlags = { withMetaTool: true };

const DESCRIPTIONS = {
  A: 'Generic tools only. The agent must author the complex query/transformation on every request.',
  B: 'Base tools plus the synthesis meta-tool; an explicit operating policy tells the agent to register the repeated logic once and reuse it.',
} as const;

function scenario(id: 'A' | 'B', label: string, systemPrompt: string, flags: ScenarioFlags): ScenarioDefinition {
  return { id, label, description: DESCRIPTIONS[id], systemPrompt, ...flags };
}

/** SQL domain: airline booking database, 7-table cancellation lookup. */
export const SQL_SCENARIOS = {
  A: scenario('A', 'Raw SQL only', SYSTEM_PROMPT_SQL_A, FLAGS_A),
  B: scenario('B', 'Dynamic tool synthesis (policy)', SYSTEM_PROMPT_SQL_B, FLAGS_B),
} as const;

/** Script domain: legacy ops-log parsing via JavaScript function synthesis. */
export const SCRIPT_SCENARIOS = {
  A: scenario('A', 'Inline scripts only', SYSTEM_PROMPT_SCRIPT_A, FLAGS_A),
  B: scenario('B', 'Dynamic tool synthesis (policy)', SYSTEM_PROMPT_SCRIPT_B, FLAGS_B),
} as const;

/** Repository domain: repeated configuration-compliance audits. */
export const REPOSITORY_SCENARIOS = {
  A: scenario('A', 'Generic tools only', SYSTEM_PROMPT_REPOSITORY_A, FLAGS_A),
  B: scenario('B', 'Dynamic tool synthesis', SYSTEM_PROMPT_REPOSITORY_B, FLAGS_B),
} as const;

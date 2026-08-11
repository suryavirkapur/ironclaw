import { MODEL_ID, PRICING_PER_MILLION_TOKENS } from './config.js';
import type { ScenarioResult } from './agents/scenario.js';
import {
  computeSteadyStateAverage,
  computeTotals,
  type RequestMetrics,
  type SteadyStateAverage,
  type Totals,
} from './metrics.js';

export interface ExperimentResult {
  id: string;
  title: string;
  /** Markdown lines describing the workload and what the experiment tests. */
  description: string[];
  userRequests: string[];
  scenarios: ScenarioResult[];
}

/** Renders the full detailed comparison as RESULTS.md. */
export function buildReport(experiments: ExperimentResult[], generatedAt: Date): string {
  return [
    '# Dynamic Tool Synthesis vs. No Synthesis — Experiment Results',
    '',
    `_Generated ${generatedAt.toISOString()} · model \`${MODEL_ID}\` · deterministic fixtures_`,
    '',
    '## Testbed',
    '',
    'Three domains, same question: is it cheaper to let an agent author complex task logic',
    'from scratch every time, or to let it **synthesize a reusable',
    'tool once** via `dynamicTool()` and call that?',
    '',
    '- **SQL domain (E1, E2):** an airline help-desk agent answers cancellation requests over a',
    '  7-table booking database; each request needs the same 7-table join. The agent starts',
    '  knowing nothing about the schema.',
    '- **Script domain (E3, E4):** an operations analyst extracts cancellation events from daily',
    '  ops-log exports in a hostile legacy text format (shuffled fields, mixed EUR/USD, noise',
    '  lines); each request needs the same parsing logic. Scripts run in a `node:vm` sandbox',
    '  with explicit inputs and bounded outputs, mirroring the IronClaw execution envelope.',
    '- **Repository domain (E5):** a supply-chain engineer audits six repository snapshots',
    '  against seven dependency, secret, container, CI, runtime, and debug policies. This',
    '  software-engineering scenario uses neither SQL nor logs.',
    '',
    'All scenarios in an experiment use identical data, identical requests, and the same model.',
    'Token counts are summed over all steps of a request (each step resends the conversation,',
    'so this is what actually gets billed). "Code authored" counts the SQL or JavaScript',
    'characters the model had to write out.',
    '',
    ...experiments.flatMap((experiment) => experimentSection(experiment)),
    onePageQuerySection(experiments),
    '',
    answersSection(experiments),
    '',
    costSection(experiments),
    '',
  ].join('\n');
}

function experimentSection(experiment: ExperimentResult): string[] {
  const { scenarios } = experiment;
  return [
    `## ${experiment.id}: ${experiment.title}`,
    '',
    ...displayDescription(experiment),
    '',
    '### Scenario matrix',
    '',
    scenarioMatrix(scenarios),
    '',
    '### Head-to-head (all requests)',
    '',
    headToHeadTable(scenarios),
    '',
    '### Steady state: average per request, requests 2–6',
    '',
    'Request 1 includes initial task discovery. Requests 2–6 show the repeated-work regime',
    '(including one-time registration where applicable):',
    '',
    steadyStateTable(scenarios),
    '',
    '### Code the model had to author (characters per request)',
    '',
    sqlTable(scenarios),
    '',
    ...scenarios.flatMap((scenario) => [
      `### Per-request detail — Scenario ${scenario.definition.id} (${scenario.definition.label})`,
      '',
      requestTable(scenario.requests),
      '',
    ]),
  ];
}

function displayDescription(experiment: ExperimentResult): string[] {
  const descriptions: Record<string, string[]> = {
    E1: [
      'Six passenger cancellation requests all require the same seven-table lookup.',
      'A has generic SQL access with no synthesis; B creates and reuses a dynamic query tool.',
    ],
    E2: [
      'Only requests 1, 3, and 5 repeat the cancellation lookup; the remaining requests',
      'are one-off analytics tasks.',
    ],
    E3: [
      'Six hostile legacy log exports require the same cancellation parser.',
      'A authors inline JavaScript; B creates and reuses a dynamic parser tool.',
    ],
    E4: [
      'Only requests 1, 3, and 5 repeat the parser; the remaining requests require',
      'different one-off transformations.',
    ],
    E5: [
      'Six repository snapshots are audited against the same seven compliance rules.',
      'A authors audit logic inline; B creates and reuses a dynamic audit tool.',
    ],
  };
  return descriptions[experiment.id] ?? experiment.description;
}

function scenarioMatrix(scenarios: ScenarioResult[]): string {
  return table(
    ['ID', 'Label', 'Description', 'Tools created by the agent'],
    scenarios.map((s) => [
      s.definition.id,
      s.definition.label,
      s.definition.description,
      s.createdTools.length > 0 ? s.createdTools.map((t) => `\`${t}\``).join(', ') : '—',
    ]),
  );
}

function headToHeadTable(scenarios: ScenarioResult[]): string {
  const totals = scenarios.map((s) => computeTotals(s.requests));
  const baseline = totals[0];
  if (!baseline) return '(no scenarios)';

  const metric = (
    name: string,
    pick: (t: Totals) => number,
    format: (n: number) => string = fmt,
  ): string[] => [
    name,
    ...totals.map((t, i) => (i === 0 ? format(pick(t)) : `${format(pick(t))} (${delta(pick(baseline), pick(t))})`)),
  ];

  return table(
    ['Metric', ...scenarios.map((s) => `${s.definition.id}: ${s.definition.label}`)],
    [
      metric('Input tokens', (t) => t.inputTokens),
      metric('Output tokens', (t) => t.outputTokens),
      metric('  … of which reasoning', (t) => t.reasoningTokens),
      metric('Cached input tokens', (t) => t.cachedInputTokens),
      metric('Code authored (chars)', (t) => t.codeCharsAuthored),
      metric('Model steps', (t) => t.steps),
      metric('Tool calls', (t) => t.toolCallCount),
      metric('Wall time', (t) => t.durationMs, fmtDuration),
    ],
  );
}

function steadyStateTable(scenarios: ScenarioResult[]): string {
  const averages = scenarios.map((s) => computeSteadyStateAverage(s.requests));
  const baseline = averages[0];
  if (!baseline) return '(no scenarios)';

  const row = (
    name: string,
    pick: (a: SteadyStateAverage) => number,
    format: (n: number) => string = (n) => fmt(Math.round(n)),
  ): string[] => [
    name,
    ...averages.map((a, i) => (i === 0 ? format(pick(a)) : `${format(pick(a))} (${delta(pick(baseline), pick(a))})`)),
  ];

  return table(
    ['Metric (avg / request)', ...scenarios.map((s) => `${s.definition.id}: ${s.definition.label}`)],
    [
      row('Input tokens', (a) => a.inputTokens),
      row('Output tokens', (a) => a.outputTokens),
      row('Code authored (chars)', (a) => a.codeCharsAuthored),
      row('Wall time', (a) => a.durationMs, fmtDuration),
    ],
  );
}

function sqlTable(scenarios: ScenarioResult[]): string {
  const first = scenarios[0];
  if (!first) return '(no scenarios)';
  return table(
    ['Request', ...scenarios.map((s) => `${s.definition.id}`)],
    first.requests.map((request, i) => [
      String(request.index),
      ...scenarios.map((s) => {
        const r = s.requests[i];
        return r ? fmt(r.codeCharsAuthored) : '—';
      }),
    ]),
  );
}

function requestTable(requests: RequestMetrics[]): string {
  return table(
    ['#', 'Steps', 'Tool calls', 'Input tok', 'Output tok', 'Reasoning tok', 'Cached in', 'Code chars', 'Time'],
    requests.map((r) => [
      String(r.index),
      String(r.steps),
      Object.entries(r.toolCalls)
        .map(([name, count]) => `${name}×${count}`)
        .join(', '),
      fmt(r.inputTokens),
      fmt(r.outputTokens),
      fmt(r.reasoningTokens),
      fmt(r.cachedInputTokens),
      fmt(r.codeCharsAuthored),
      fmtDuration(r.durationMs),
    ]),
  );
}

function onePageQuerySection(experiments: ExperimentResult[]): string {
  let longest = '';
  for (const experiment of experiments) {
    for (const scenario of experiment.scenarios) {
      for (const request of scenario.requests) {
        if (request.longestCode.length > longest.length) longest = request.longestCode;
      }
    }
  }
  return [
    '## The "one-page" artifact',
    '',
    'The longest single piece of code (SQL or JavaScript) the model authored across all runs:',
    '',
    '<details><summary>Show code</summary>',
    '',
    '```',
    longest || '(none)',
    '',
    '```',
    '</details>',
  ].join('\n');
}

function answersSection(experiments: ExperimentResult[]): string {
  return [
    '## Final answers',
    '',
    ...experiments.flatMap((experiment) =>
      experiment.scenarios.flatMap((scenario) => [
        `<details><summary>${experiment.id} · Scenario ${scenario.definition.id} (${scenario.definition.label})</summary>`,
        '',
        scenario.requests
          .map((r) => `**Request ${r.index}:**\n\n${truncate(r.finalAnswer, 900) || '_(no final text)_'}`)
          .join('\n\n'),
        '',
        '</details>',
        '',
      ]),
    ),
  ].join('\n');
}

function costSection(experiments: ExperimentResult[]): string {
  const { input, output } = PRICING_PER_MILLION_TOKENS;
  if (input === null || output === null) {
    return [
      '## Cost estimate',
      '',
      '_Set `PRICING_PER_MILLION_TOKENS` in `src/config.ts` (see',
      'https://api-docs.deepseek.com/quick_start/pricing) to get dollar figures. Cost scales',
      'linearly with the token counts above; DeepSeek additionally charges cached input tokens',
      'at a much lower rate._',
    ].join('\n');
  }
  const rows = experiments.flatMap((experiment) =>
    experiment.scenarios.map((scenario) => {
      const totals = computeTotals(scenario.requests);
      const cost = (totals.inputTokens / 1e6) * input + (totals.outputTokens / 1e6) * output;
      return [`${experiment.id} · ${scenario.definition.id}`, `$${cost.toFixed(4)}`];
    }),
  );
  return ['## Cost estimate', '', table(['Experiment · Scenario', 'Cost (USD)'], rows)].join('\n');
}

function table(header: string[], rows: string[][]): string {
  return [
    `| ${header.join(' | ')} |`,
    `| ${header.map(() => '---').join(' | ')} |`,
    ...rows.map((row) => `| ${row.join(' | ')} |`),
  ].join('\n');
}

function fmt(n: number): string {
  return Math.round(n).toLocaleString('en-US');
}

function fmtDuration(ms: number): string {
  return `${(ms / 1000).toFixed(1)}s`;
}

/** Percentage change relative to baseline (first scenario); negative means cheaper. */
function delta(baseline: number, value: number): string {
  if (baseline === 0) return '—';
  const change = ((value - baseline) / baseline) * 100;
  return `${change >= 0 ? '+' : ''}${change.toFixed(1)}%`;
}

function truncate(text: string, maxChars: number): string {
  return text.length <= maxChars ? text : `${text.slice(0, maxChars)}…`;
}

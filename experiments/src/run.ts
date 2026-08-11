import { readFileSync, writeFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import {
  buildScriptMixedRequests,
  buildScriptRepetitiveRequests,
  buildRepositoryAuditRequests,
  buildSqlMixedRequests,
  buildSqlRepetitiveRequests,
} from './agents/prompts.js';
import { runScenario, type ScenarioResult } from './agents/scenario.js';
import {
  REPOSITORY_SCENARIOS,
  SCRIPT_SCENARIOS,
  SQL_SCENARIOS,
  type ScenarioDefinition,
} from './agents/scenarios.js';
import type { ToolkitFactory } from './agents/toolkit.js';
import { createScriptToolkit } from './agents/toolkits/script.js';
import { createRepositoryToolkit } from './agents/toolkits/repository.js';
import { createSqlToolkit } from './agents/toolkits/sql.js';
import { MODEL_ID, createModel } from './config.js';
import { createSeededDatabase, pickScenarioCustomers } from './db/seed.js';
import { computeTotals } from './metrics.js';
import { buildReport, type ExperimentResult } from './report.js';

const EXPERIMENTS_DIR = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');

interface ExperimentPlan {
  id: string;
  title: string;
  description: string[];
  userRequests: string[];
  makeToolkit: ToolkitFactory;
  scenarios: ScenarioDefinition[];
}

async function main(): Promise<void> {
  // `--report-only`: regenerate RESULTS.md from the existing results.json (no API calls).
  if (process.argv.includes('--report-only')) {
    const previous = readPreviousExperiments();
    if (previous.length === 0) throw new Error('No results.json to report from.');
    writeFileSync(path.join(EXPERIMENTS_DIR, 'RESULTS.md'), buildReport(previous, new Date()));
    console.log('Regenerated RESULTS.md from results.json');
    return;
  }

  const model = createModel();

  // Pick the passengers/bookings the scripted SQL requests will talk about.
  // All data is deterministic, so every scenario sees identical inputs.
  const customers = pickScenarioCustomers(createSeededDatabase());

  const plans: ExperimentPlan[] = [
    {
      id: 'E1',
      title: 'SQL · Repetitive workload — every request needs the same 7-table join',
      description: [
        'Six passenger cancellation requests in a row; all six require the same complex',
        'lookup with different filter values. This is the regime dynamic tool synthesis is',
        'made for. Scenario **A** has generic SQL capabilities but no synthesis; scenario',
        '**B** creates and reuses a dynamic query tool under an explicit policy.',
      ],
      userRequests: buildSqlRepetitiveRequests(customers),
      makeToolkit: createSqlToolkit,
      scenarios: [SQL_SCENARIOS.A, SQL_SCENARIOS.B],
    },
    {
      id: 'E2',
      title: 'SQL · Mixed workload — only some requests repeat',
      description: [
        'Six requests of which only three (1, 3, 5) are the repeated cancellation lookup;',
        'the rest are one-off analytics questions. Tests whether dynamic tool synthesis',
        'still pays off when the workload is only partially repetitive.',
      ],
      userRequests: buildSqlMixedRequests(customers),
      makeToolkit: createSqlToolkit,
      scenarios: [SQL_SCENARIOS.A, SQL_SCENARIOS.B],
    },
    {
      id: 'E3',
      title: 'Code synthesis · Repetitive workload — same parser for six log exports',
      description: [
        'Six daily ops-log exports in a hostile legacy text format (shuffled key=value',
        'fields, mixed EUR/USD amounts, per-file fx rate, noise lines); every request asks',
        'for the same cancellation extraction over a new file. **No SQL involved**: scenario',
        'A writes inline JavaScript per request, B synthesizes a JavaScript parser tool once',
        'via `createFunctionTool` and reuses it.',
      ],
      userRequests: buildScriptRepetitiveRequests(),
      makeToolkit: createScriptToolkit,
      scenarios: [SCRIPT_SCENARIOS.A, SCRIPT_SCENARIOS.B],
    },
    {
      id: 'E4',
      title: 'Code synthesis · Mixed workload — only some requests repeat the parser',
      description: [
        'Six requests of which only three (1, 3, 5) repeat the cancellation extraction;',
        'the rest are one-off questions (booking revenue, delay lists, refund frequency).',
      ],
      userRequests: buildScriptMixedRequests(),
      makeToolkit: createScriptToolkit,
      scenarios: [SCRIPT_SCENARIOS.A, SCRIPT_SCENARIOS.B],
    },
    {
      id: 'E5',
      title: 'Repository compliance · Repetitive workload — six configuration audits',
      description: [
        'Six repositories are audited against the same seven supply-chain and deployment',
        'policies. This domain uses neither SQL nor logs. Scenario A authors the audit logic',
        'inline for each repository; scenario B registers it once as `auditRepository` and',
        'reuses it with a repository name.',
      ],
      userRequests: buildRepositoryAuditRequests(),
      makeToolkit: createRepositoryToolkit,
      scenarios: [REPOSITORY_SCENARIOS.A, REPOSITORY_SCENARIOS.B],
    },
  ];

  console.log(`Model: ${MODEL_ID}`);
  console.log(`SQL customers: ${customers.map((c) => `${c.firstName} ${c.lastName}/${c.reference}`).join(', ')}`);

  // Optional CLI filter: `npm run experiment -- E3 E4:B` runs experiment E3 fully and
  // only scenario B of E4.
  const filter = process.argv.slice(2).map((arg) => arg.toUpperCase());
  const matches = (planId: string, scenarioId: string): boolean =>
    filter.length === 0 ||
    filter.some((f) => f === planId || f === `${planId}:${scenarioId}`);
  const selected = plans
    .map((plan) => ({
      ...plan,
      scenarios: plan.scenarios.filter((s) => matches(plan.id, s.id)),
    }))
    .filter((plan) => plan.scenarios.length > 0);
  if (selected.length === 0) throw new Error(`No experiments matched filter: ${filter.join(', ')}`);

  const experiments: ExperimentResult[] = [];
  for (const plan of selected) {
    const results: ScenarioResult[] = [];
    for (const definition of plan.scenarios) {
      console.log(`\n=== ${plan.id} · Scenario ${definition.id}: ${definition.label} ===`);
      results.push(
        await runScenario({
          definition,
          makeToolkit: plan.makeToolkit,
          model,
          userRequests: plan.userRequests,
          onProgress: (line) => console.log(line),
        }),
      );
    }
    experiments.push({
      id: plan.id,
      title: plan.title,
      description: plan.description,
      userRequests: plan.userRequests,
      scenarios: results,
    });
  }

  const generatedAt = new Date();
  // Experiments not re-run in this invocation are carried over from the previous
  // results.json, so RESULTS.md always reflects the full suite.
  const merged = mergeExperiments(experiments, readPreviousExperiments());

  writeFileSync(path.join(EXPERIMENTS_DIR, 'RESULTS.md'), buildReport(merged, generatedAt));
  writeFileSync(
    path.join(EXPERIMENTS_DIR, 'results.json'),
    JSON.stringify({ model: MODEL_ID, generatedAt, customers, experiments: merged }, null, 2),
  );

  console.log('\n=== Summary ===');
  for (const experiment of merged) {
    for (const scenario of experiment.scenarios) {
      const t = computeTotals(scenario.requests);
      console.log(
        `${experiment.id}·${scenario.definition.id}: in=${t.inputTokens} out=${t.outputTokens} steps=${t.steps} codeChars=${t.codeCharsAuthored} time=${(t.durationMs / 1000).toFixed(1)}s created=[${scenario.createdTools.join(', ')}]`,
      );
    }
  }
  console.log('\nWrote RESULTS.md and results.json');
}

function readPreviousExperiments(): ExperimentResult[] {
  try {
    const raw = readFileSync(path.join(EXPERIMENTS_DIR, 'results.json'), 'utf8');
    const parsed = JSON.parse(raw) as { experiments?: ExperimentResult[] };
    return (parsed.experiments ?? []).map((experiment) => ({
      ...experiment,
      scenarios: experiment.scenarios.filter((scenario) => scenario.definition.id === 'A' || scenario.definition.id === 'B'),
    }));
  } catch {
    return [];
  }
}

function mergeExperiments(fresh: ExperimentResult[], previous: ExperimentResult[]): ExperimentResult[] {
  const byId = new Map<string, ExperimentResult>();
  for (const experiment of previous) byId.set(experiment.id, experiment);
  for (const experiment of fresh) {
    const prior = byId.get(experiment.id);
    if (!prior) {
      byId.set(experiment.id, experiment);
      continue;
    }
    // Scenario-level merge: freshly run scenarios replace prior ones, the rest carry over.
    const scenarios = [
      ...experiment.scenarios,
      ...prior.scenarios.filter(
        (p) => !experiment.scenarios.some((f) => f.definition.id === p.definition.id),
      ),
    ].sort((a, b) => a.definition.id.localeCompare(b.definition.id));
    byId.set(experiment.id, { ...experiment, scenarios });
  }
  return [...byId.values()].sort((a, b) => a.id.localeCompare(b.id));
}

main().catch((error: unknown) => {
  console.error(error instanceof Error ? error.message : error);
  process.exit(1);
});

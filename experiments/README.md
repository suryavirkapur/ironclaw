# Experiments — Are Dynamic Tools Worth It?

TypeScript experiments with the [AI SDK](https://ai-sdk.dev) `dynamicTool()` API, running on
`deepseek-v4-pro`. Built as the utility evaluation for the IronClaw thesis
(`../Report Thesis Suryavir Kapur`).

## The question

An agent with generic capabilities (raw SQL or inline scripts) can do anything — but if a
frequent task means authoring **complex logic from scratch every time**, each request
pays full price in generated tokens and steps.
If the agent instead writes that artifact **once** and registers it as a small
parameterized **dynamic tool**, every repeat becomes a tiny tool call.

Is that actually cheaper in practice — when does it pay, and when not? Five experiments
across three domains measure it.

## Testbeds

**SQL domain (E1, E2)** — airline help-desk agent over a 7-table booking database
(`node:sqlite`, deterministic seed). Every cancellation request needs the same 7-table
join:

```
passengers ──< bookings ──< booking_segments >── flights
                   │               │
                payments        tickets ──< refund_requests
```

**Script domain (E3, E4)** — operations analyst parsing daily ops-log exports in a
hostile legacy text format (shuffled `key=value` fields, quoted names, amounts in mixed
EUR/USD with a per-file fx rate in the header, comment/`PING` noise lines). Scripts run
in a `node:vm` sandbox with explicit inputs, an explicit capability allow-list
(`readFile(name)`), 1s timeout, and bounded output — mirroring IronClaw's execution
envelope. **No SQL anywhere.**

**Repository-compliance domain (E5)** — a supply-chain engineer audits six repository
snapshots against seven dependency, secret, Dockerfile, CI, runtime, and production
configuration policies. This is a software-engineering task with **neither SQL nor logs**.

The agent knows **nothing** about the schema/format up front. Token counts are summed
over all steps of a request (each step resends the conversation — that is what actually
gets billed). "Code authored" counts the SQL or JavaScript characters the model wrote.

## Scenarios

| ID | Label | Beyond the base tools | Prompt |
| --- | --- | --- | --- |
| **A** | Raw only | — | answer with `runQuery` / `runScript` |
| **B** | Dynamic tool synthesis (policy) | `createQueryTool` / `createFunctionTool` | explicit policy: register the repeated logic once, then reuse it |

## E1 — SQL · repetitive (all 6 requests need the join)

| Metric | A: no synthesis | B: dynamic synthesis |
| --- | --- | --- |
| Input tokens | 190,031 | 88,566 (**-53%**) |
| Output tokens | 6,783 | 5,526 |
| Model steps | 28 | 15 |
| Code authored | 3,142 | 1,800 |

## E2 — SQL · mixed (only requests 1, 3, 5 repeat the join)

| Metric | A | B |
| --- | --- | --- |
| Input tokens | 73,452 | 87,442 (+19%) |
| Output tokens | 4,938 | 4,692 |

## E3 — Script · repetitive (same parser for 6 daily logs)

| Metric | A: no synthesis | B: dynamic synthesis |
| --- | --- | --- |
| Input tokens | 193,817 | 74,865 (**-61%**) |
| Output tokens | 8,757 | 3,606 (-59%) |
| Reasoning tokens | 4,244 | 1,011 (-76%) |
| Code authored | 6,109 | 1,196 (-80%) |

Steady state (avg/request, 2–6): input A 37,320 → B 14,101 (**-62%**);
output A 1,402 → B 502.

## E4 — Script · mixed (only requests 1, 3, 5 repeat the parse)

| Metric | A | B |
| --- | --- | --- |
| Input tokens | 64,452 | 106,131 (+65%) |
| Output tokens | 4,061 | 4,269 |

## E5 — Repository compliance · repetitive (six repository audits)

| Metric | A: no synthesis | B: dynamic synthesis |
| --- | --- | --- |
| Input tokens | 140,472 | 65,146 (**-54%**) |
| Output tokens | 12,429 | 4,826 (**-61%**) |
| Reasoning tokens | 1,911 | 538 (**-72%**) |
| Model steps | 19 | 13 (**-32%**) |
| Code authored | 29,176 | 9,112 (**-69%**) |
| Wall time | 134.0 s | 61.4 s (**-54%**) |

## Findings

1. **Repetitive workloads: synthesis wins across all three domains** (−53% billed input
   tokens in SQL, −61% in script, and −54% in repository compliance). The saving comes from step-count collapse and
   eliminating per-request artifact authoring, not from the artifact text itself.
2. **The effect generalizes to software-engineering work.** E5 applies reusable audit
   logic across multi-file repository snapshots and preserves exact ground-truth results.
3. **Payoff scales with reuse; mixed workloads are net-negative.** With
   only 3 of 6 requests repeating, B lands at +19% (SQL) and +65% (script) input tokens
   in these runs — the creation transcript inflates history for all later requests.
4. **The enabling condition is tool-side data access.** In an earlier E3 design where
   created tools had to receive the full file text as a parameter (bulk data through
   the conversation), synthesis was net-negative (+12% input). Letting the created tool
   load data itself via a session capability (`readFile(name)` inside the sandbox) made
   inputs tiny and flipped E3 to −61%. Rule of thumb: **pass lookup keys, not data.**
5. **Answer quality is preserved** — A/B answers were validated against deterministic
   ground truth across all three domains.
6. **Tool availability is per-step, not per-call.** A tool created mid-`generateText`
   is invisible until the next model call; the runner steps the tool loop manually so
   a tool created in step N is callable in step N+1.

## Run it

```bash
export DEEPSEEK_API_KEY=...   # https://platform.deepseek.com/api_keys
npm install
npm run experiment                  # full suite E1–E5
npm run experiment -- E3            # only E3
npm run experiment -- E3:B          # only scenario B of E3 (merged into results.json)
npm run experiment -- --report-only # regenerate RESULTS.md from results.json
npm run typecheck
```

To get dollar figures in the report, set `PRICING_PER_MILLION_TOKENS` in
`src/config.ts` from <https://api-docs.deepseek.com/quick_start/pricing>.

## Structure

```
src/
  config.ts           model, step/row limits, optional pricing
  sandbox.ts          node:vm executor: explicit inputs + capability allow-list
  db/
    schema.ts         7-table airline schema (SQL domain)
    seed.ts           deterministic data + scenario-customer selection
    database.ts       read-only in-memory SQLite wrapper (node:sqlite)
  logs/
    opslogs.ts        legacy ops-log generator + reference parser (script domain)
  agents/
    prompts.ts        system prompts (A/B per domain) + workloads
    scenarios.ts      scenario definitions (SQL_SCENARIOS, SCRIPT_SCENARIOS)
    toolkit.ts        toolkit interface + shared helpers
    toolkits/
      sql.ts          listTables/describeTable/runQuery, createQueryTool
      script.ts       listFiles/readFile/runScript, createFunctionTool
      repository.ts   repository snapshots, runScript, createFunctionTool
    scenario.ts       manual per-step tool loop + metrics collection
  metrics.ts          token/code-char aggregation
  report.ts           RESULTS.md generation
  repositories/       deterministic repository-compliance fixtures
  run.ts              orchestrates E1–E5 with CLI filters
```

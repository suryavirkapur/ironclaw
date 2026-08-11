import type { ScenarioCustomer } from '../db/seed.js';
import { fileName } from '../logs/opslogs.js';
import { REPOSITORY_NAMES } from '../repositories/compliance.js';

// ===========================================================================
// SQL domain: airline booking help desk
// ===========================================================================

const SQL_PERSONA = `You are a customer-service agent at the help desk of the airline IronAir.
Passengers contact you to cancel flights and ask about refunds.

You have access to IronAir's booking database through the provided tools, but you know
NOTHING about its schema in advance. Discover the schema with the tools before writing
queries.

Rules:
- ALWAYS look up the real booking in the database before answering. Never invent data.
- Only read-only queries are allowed; you cannot modify the database, so explain what
  the cancellation WOULD mean (refund eligibility, amounts, flight status) rather than
  performing it.
- Answer concisely with concrete details: flights, ticket refundability, amounts in USD
  (price_cents / 100), payment status, and any existing refund requests.`;

export const SYSTEM_PROMPT_SQL_A =
  SQL_PERSONA +
  `

Use the runQuery tool to write whatever SELECT queries you need. Each passenger request
is independent: answer it with the data you find.`;

export const SYSTEM_PROMPT_SQL_B =
  SQL_PERSONA +
  `

You additionally have a createQueryTool tool. It turns a parameterized SQL query into a
new reusable tool that becomes available immediately and stays available for all future
passenger requests.

OPERATING POLICY — follow it strictly:
1. Answer the FIRST passenger request as normal: discover the schema and work out the
   full lookup query you need with runQuery.
2. Once you have the data for that first answer, register the lookup query ONCE via
   createQueryTool (with :named parameters for the values that change between
   passengers, e.g. last name and booking reference).
3. Registering a tool is bookkeeping, never a substitute for answering: the LAST step
   of every turn MUST be your complete answer to the current passenger, with the
   actual data. Never end a turn with a note about the tool.
4. The created tool is callable from the very next step after its creation. For every
   LATER passenger request, call it with just the parameter values instead of
   rewriting the full SQL. Use runQuery only for genuinely new, one-off questions the
   created tool cannot answer.`;

/**
 * Repetitive workload: six passenger requests that ALL require the same 7-table
 * cancellation lookup (passenger -> booking -> segments -> flights -> tickets ->
 * payment -> refund requests), just with different passengers / booking references.
 */
export function buildSqlRepetitiveRequests(customers: ScenarioCustomer[]): string[] {
  const name = (c: ScenarioCustomer) => `${c.firstName} ${c.lastName}`;
  const [c1, c2, c3, c4, c5, c6] = customers;
  if (!c1 || !c2 || !c3 || !c4 || !c5 || !c6) {
    throw new Error(`buildSqlRepetitiveRequests needs 6 customers, got ${customers.length}`);
  }

  return [
    `Hi, this is ${name(c1)}. I need to cancel booking ${c1.reference}. What are my options and how much money would I get back?`,
    `Hello, ${name(c2)} here — booking ${c2.reference}. My plans changed and I want to cancel the whole trip. How much will I be refunded?`,
    `Can you check the cancellation terms for booking ${c3.reference} (${name(c3)})? Also, is my flight still on schedule?`,
    `This is ${name(c4)}, booking reference ${c4.reference}. My meeting got moved — please walk me through cancelling and what it means for my payment.`,
    `Hey, booking ${c5.reference} under ${name(c5)}. Is my ticket refundable, and has any refund already been requested for it?`,
    `I'm ${name(c6)}, booking ${c6.reference}. I want to cancel everything — confirm what happens to my payment and whether my fare allows a refund.`,
  ];
}

/**
 * Mixed workload: only requests 1, 3 and 5 are the repeated cancellation lookup;
 * the others are genuinely different one-off analytics questions. This tests whether
 * dynamic tool synthesis still pays off when the workload is only partially repetitive.
 */
export function buildSqlMixedRequests(customers: ScenarioCustomer[]): string[] {
  const name = (c: ScenarioCustomer) => `${c.firstName} ${c.lastName}`;
  const [c1, c2, c3] = customers;
  if (!c1 || !c2 || !c3) {
    throw new Error(`buildSqlMixedRequests needs at least 3 customers, got ${customers.length}`);
  }

  return [
    `Hi, this is ${name(c1)}. I need to cancel booking ${c1.reference}. What are my options and how much money would I get back?`,
    `Quick operations question: which IronAir flights are currently delayed, and on which routes?`,
    `Hello, ${name(c2)} here — booking ${c2.reference}. My plans changed and I want to cancel the whole trip. How much will I be refunded?`,
    `For a report: how many passengers do we have in each loyalty tier, and how much captured revenue (captured payments) does each tier represent?`,
    `Can you check the cancellation terms for booking ${c3.reference} (${name(c3)})? Also, is my flight still on schedule?`,
    `Last one: list all pending refund requests with the passenger's name, the flight number, and the ticket price.`,
  ];
}

// ===========================================================================
// Script domain: legacy ops-log parsing
// ===========================================================================

const SCRIPT_PERSONA = `You are an operations analyst at the airline IronAir. Every night the
operations system exports a log file in a legacy text format (one file per day), and you
are given analysis tasks over these export files.

The legacy format has quirks:
- key=value fields in ARBITRARY order per line; values may be quoted and contain
  commas or spaces, e.g. pax="Fischer, Ravi"
- amounts in USD or EUR; each file's header carries THAT file's EUR->USD fx rate
  (a line like "## fx: 1 EUR = 1.072 USD") — use it to convert, and round to 2 decimals
- comment lines starting with # and PING heartbeat lines are noise
- event types include BKG (booking), CXL (cancellation), RFD (refund), DLY (delay)

Rules:
- ALWAYS work from the real file contents before answering. Never invent data.
- Answer concisely with concrete structured details (tables where helpful).

Sandbox: scripts executed via tools can call \`readFile(name)\` internally to load an
ops-log file themselves (same session capability as the readFile tool).`;

export const SYSTEM_PROMPT_SCRIPT_A =
  SCRIPT_PERSONA +
  `

Use readFile to fetch a file and runScript to execute inline JavaScript over it. Write
whatever inline code each task needs.`;

export const SYSTEM_PROMPT_SCRIPT_B =
  SCRIPT_PERSONA +
  `

You additionally have a createFunctionTool tool. It turns a JavaScript function into a
new reusable tool that becomes available immediately and stays available for all future
tasks.

OPERATING POLICY — follow it strictly:
1. Answer the FIRST task as normal with runScript. Do NOT create any tool in this
   turn — just answer it completely.
2. At the START of the SECOND task, BEFORE answering it, register the parsing logic
   ONCE via createFunctionTool. Give the tool a small lookup-style parameter such as
   the file name — the script can load the file itself with readFile(name), so bulk
   file text should NOT be a tool parameter.
3. From then on, for every task of the same shape, call the created tool with just the
   file name. Use runScript only for genuinely new, one-off questions the created tool
   cannot answer.
4. Always finish every turn with the complete answer, with the actual data.`;

const CXL_TASK = (file: string) =>
  `From ${file}: list all cancellation events — booking reference, passenger, amount converted to USD, and flight — plus the total cancelled amount in USD.`;

/** Repetitive script workload: the same cancellation-extraction task over six daily files. */
export function buildScriptRepetitiveRequests(): string[] {
  return [
    CXL_TASK(fileName(1)),
    `Same exercise for ${fileName(2)}: every cancellation with reference, passenger, USD amount, flight, and the daily total in USD.`,
    `Now ${fileName(3)} — all cancellations (reference, passenger, USD amount, flight) and the total cancelled in USD.`,
    `Please do ${fileName(4)} next: cancellation events with reference, passenger, amount in USD, flight, and the total.`,
    `${fileName(5)} today: list the cancellations (ref, passenger, USD amount, flight) and total them in USD.`,
    `Finally ${fileName(6)}: all cancellation events — reference, passenger, USD amount, flight — and the total cancelled amount in USD.`,
  ];
}

/**
 * Mixed script workload: only requests 1, 3 and 5 are the repeated cancellation
 * extraction; the others are one-off questions the created tool cannot answer.
 */
export function buildScriptMixedRequests(): string[] {
  return [
    CXL_TASK(fileName(1)),
    `In ${fileName(2)}, what is the total revenue from new bookings (BKG events) in USD? Just the total.`,
    `Same cancellation exercise for ${fileName(3)}: every cancellation with reference, passenger, USD amount, flight, and the daily total in USD.`,
    `List all DLY events in ${fileName(4)}: flight number and delay minutes, longest delay first.`,
    `${fileName(5)}: cancellation events with reference, passenger, amount in USD, flight, and the total.`,
    `In ${fileName(6)}, which passenger appears most often in RFD events, and how many times?`,
  ];
}

// ===========================================================================
// Repository domain: configuration-compliance auditing
// ===========================================================================

const REPOSITORY_PERSONA = `You are a software supply-chain engineer auditing repository
configuration snapshots. Each snapshot contains package.json, Dockerfile, a GitHub
Actions workflow, and .env.production.

Apply exactly these policy rules:
- HIGH: a production environment file contains a key whose name includes SECRET, TOKEN,
  or API_KEY and whose value is non-empty
- HIGH: lodash is older than 4.17.21
- MEDIUM: the Dockerfile has no USER instruction
- MEDIUM: the Dockerfile has no HEALTHCHECK instruction
- MEDIUM: a workflow action uses a mutable ref such as @main or @master
- LOW: Node.js is older than 20
- LOW: DEBUG=true in .env.production

Return every violation as rule, severity, file, and evidence, followed by counts by
severity. Do not invent findings. Scripts can call readRepository(name) internally and
receive {name, files}.`;

export const SYSTEM_PROMPT_REPOSITORY_A =
  REPOSITORY_PERSONA +
  `

Use runScript to audit each requested repository. Author the complete audit logic needed
for the current request as inline JavaScript.`;

export const SYSTEM_PROMPT_REPOSITORY_B =
  REPOSITORY_PERSONA +
  `

You additionally have createFunctionTool, which registers reusable JavaScript.

OPERATING POLICY:
1. Audit the first repository with runScript and answer completely.
2. At the start of the second request, create one reusable auditRepository tool containing
   the same complete policy logic. Its only parameter should be the repository name; load
   files inside it with readRepository(input.repository).
3. Use that created tool for every later repository. Do not rewrite the audit code.
4. End every request with the complete findings and severity counts.`;

export function buildRepositoryAuditRequests(): string[] {
  return REPOSITORY_NAMES.map(
    (name, index) =>
      `${index === 0 ? 'Audit' : 'Now audit'} the ${name} repository for all seven policy rules. List every violation with rule, severity, file, and evidence, then give HIGH/MEDIUM/LOW counts.`,
  );
}

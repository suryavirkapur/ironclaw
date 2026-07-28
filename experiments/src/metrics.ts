/** Metrics for one passenger request (possibly several model steps). */
export interface RequestMetrics {
  /** 1-based position of the request in the conversation. */
  index: number;
  userRequest: string;
  steps: number;
  /** Tool name -> number of calls. */
  toolCalls: Record<string, number>;
  /** Summed across steps — this is what actually gets billed (each step resends context). */
  inputTokens: number;
  outputTokens: number;
  reasoningTokens: number;
  cachedInputTokens: number;
  /** Characters of code the model authored in this request (SQL or JavaScript). */
  codeCharsAuthored: number;
  /** Longest single code artifact authored in this request ('' if none) — for the report. */
  longestCode: string;
  durationMs: number;
  finalAnswer: string;
}

export interface Totals {
  requestCount: number;
  steps: number;
  toolCallCount: number;
  inputTokens: number;
  outputTokens: number;
  reasoningTokens: number;
  cachedInputTokens: number;
  codeCharsAuthored: number;
  durationMs: number;
}

export function computeTotals(requests: RequestMetrics[]): Totals {
  const totals: Totals = {
    requestCount: requests.length,
    steps: 0,
    toolCallCount: 0,
    inputTokens: 0,
    outputTokens: 0,
    reasoningTokens: 0,
    cachedInputTokens: 0,
    codeCharsAuthored: 0,
    durationMs: 0,
  };
  for (const request of requests) {
    totals.steps += request.steps;
    totals.toolCallCount += Object.values(request.toolCalls).reduce((sum, n) => sum + n, 0);
    totals.inputTokens += request.inputTokens;
    totals.outputTokens += request.outputTokens;
    totals.reasoningTokens += request.reasoningTokens;
    totals.cachedInputTokens += request.cachedInputTokens;
    totals.codeCharsAuthored += request.codeCharsAuthored;
    totals.durationMs += request.durationMs;
  }
  return totals;
}

export interface SteadyStateAverage {
  inputTokens: number;
  outputTokens: number;
  codeCharsAuthored: number;
  durationMs: number;
}

/** Average per request over a slice (default: the "steady state" requests 2..n). */
export function computeSteadyStateAverage(
  requests: RequestMetrics[],
  fromIndex = 2,
): SteadyStateAverage {
  const slice = requests.filter((r) => r.index >= fromIndex);
  if (slice.length === 0) return { inputTokens: 0, outputTokens: 0, codeCharsAuthored: 0, durationMs: 0 };
  const sum = (key: 'inputTokens' | 'outputTokens' | 'codeCharsAuthored' | 'durationMs') =>
    slice.reduce((total, r) => total + r[key], 0) / slice.length;
  return {
    inputTokens: sum('inputTokens'),
    outputTokens: sum('outputTokens'),
    codeCharsAuthored: sum('codeCharsAuthored'),
    durationMs: sum('durationMs'),
  };
}

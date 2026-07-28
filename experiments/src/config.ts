import { createDeepSeek } from '@ai-sdk/deepseek';
import type { LanguageModel } from 'ai';

/** The model under test. */
export const MODEL_ID = 'deepseek-v4-pro';

/** Hard cap on model steps per passenger request, so a confused agent cannot loop forever. */
export const MAX_STEPS_PER_REQUEST = 15;

/** Row cap for tool results, to keep one bad SELECT * from flooding the context. */
export const ROW_LIMIT = 50;

/**
 * USD per 1M tokens. Fill in from https://api-docs.deepseek.com/quick_start/pricing
 * to get dollar figures in the report; token figures are always reported.
 */
export const PRICING_PER_MILLION_TOKENS: { input: number | null; output: number | null } = {
  input: null,
  output: null,
};

export function createModel(): LanguageModel {
  const apiKey = process.env.DEEPSEEK_API_KEY;
  if (!apiKey) {
    throw new Error(
      'DEEPSEEK_API_KEY environment variable is not set. Get a key at https://platform.deepseek.com/api_keys',
    );
  }
  return createDeepSeek({ apiKey })(MODEL_ID);
}

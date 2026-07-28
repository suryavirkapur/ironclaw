import type { Tool, ToolSet } from 'ai';
import { z } from 'zod';

/** Registry of dynamic tools the agent creates at runtime (scenario B). */
export type DynamicToolRegistry = Record<string, Tool<unknown, unknown>>;

/**
 * A toolkit is everything a scenario needs beyond the model and the prompt:
 * shared base tools and the meta-tool for runtime tool synthesis. A fresh
 * instance is created per scenario run so runs never share state.
 */
export interface ToolkitInstance {
  /** Tools every scenario gets. */
  baseTools: ToolSet;
  /** Meta-tool for runtime tool synthesis (createQueryTool / createFunctionTool). */
  metaTool: { name: string; make: (registry: DynamicToolRegistry) => Tool };
}

export type ToolkitFactory = () => ToolkitInstance;

/** Builds a zod object schema at runtime from an agent's parameter declarations. */
export function buildInputSchema(parameters: { name: string; type: 'string' | 'number'; description: string }[]) {
  const shape: Record<string, z.ZodTypeAny> = {};
  for (const parameter of parameters) {
    const base = parameter.type === 'number' ? z.number() : z.string();
    shape[parameter.name] = base.describe(parameter.description);
  }
  return z.object(shape);
}

export function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

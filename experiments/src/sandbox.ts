import vm from 'node:vm';

const SCRIPT_TIMEOUT_MS = 1_000;
const MAX_OUTPUT_CHARS = 16_000;

/** Host-approved capabilities injected into the sandbox (mirrors IronClaw's allow-list). */
export interface SandboxCapabilities {
  /** Read one ops-log file by name; scoped to the log store, read-only. */
  readFile?: (name: string) => string;
  /** Read one scoped repository snapshot for configuration analysis. */
  readRepository?: (name: string) => unknown;
}

export interface ScriptResult {
  /** JSON-serializable return value of the script. */
  value: unknown;
  /** True if the serialized value was truncated to the output cap. */
  truncated: boolean;
}

/**
 * Executes an untrusted script the way IronClaw's execution envelope would:
 * explicit inputs plus an explicit capability list (no ambient authority — no
 * require, process, fs, network), a hard timeout, synchronous execution, and a
 * bounded output.
 *
 * The script is the body of a function that sees its inputs as `input` and must
 * `return` a JSON-serializable value. Approved capabilities (e.g. `readFile`)
 * appear as same-named globals, exactly like IronClaw's session allow-list.
 *
 * NOTE: node:vm is a functional sandbox, not a security boundary — in IronClaw
 * this same envelope semantics runs inside a Firecracker microVM.
 */
export function runUserScript(
  script: string,
  input: unknown,
  capabilities: SandboxCapabilities = {},
): ScriptResult {
  const wrapped = `"use strict";\n(function() {\n${script}\n})()`;
  const context = vm.createContext({ input, ...capabilities }, { name: 'tool-sandbox' });
  const value: unknown = new vm.Script(wrapped, { filename: 'tool.js' }).runInContext(context, {
    timeout: SCRIPT_TIMEOUT_MS,
  });

  const serialized = JSON.stringify(value) ?? 'null';
  if (serialized.length > MAX_OUTPUT_CHARS) {
    return { value: `${serialized.slice(0, MAX_OUTPUT_CHARS)}…TRUNCATED`, truncated: true };
  }
  return { value: JSON.parse(serialized), truncated: false };
}

/** Syntax-checks a script without executing it (throws SyntaxError if invalid). */
export function compileUserScript(script: string): void {
  new vm.Script(`"use strict";\n(function() {\n${script}\n})()`, { filename: 'tool.js' });
}

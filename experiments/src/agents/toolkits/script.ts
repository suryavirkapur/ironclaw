import { dynamicTool, tool, type FlexibleSchema } from 'ai';
import { z } from 'zod';
import { createOpsLogStore } from '../../logs/opslogs.js';
import { compileUserScript, runUserScript } from '../../sandbox.js';
import {
  buildInputSchema,
  errorMessage,
  type DynamicToolRegistry,
  type ToolkitInstance,
} from '../toolkit.js';

const SCRIPT_CONVENTION = [
  'The script is the body of a JavaScript function: it sees its inputs as the `input`',
  'variable and must `return` a JSON-serializable value. Inside the sandbox it can call',
  '`readFile(name)` to load an ops-log file itself (same session capability as the',
  'readFile tool). No require/process/network; 1s timeout; output capped.',
].join(' ');

/** Creates a fresh in-memory ops-log store plus the script toolkit around it. */
export function createScriptToolkit(): ToolkitInstance {
  const store = createOpsLogStore();
  const capabilities = { readFile: (name: string): string => store.read(name) };

  const runScript = tool({
    description: `Execute inline JavaScript in a sandbox and get back its return value. ${SCRIPT_CONVENTION}`,
    inputSchema: z.object({
      script: z.string().describe('JavaScript function body.'),
      input: z.unknown().optional().describe('Value exposed to the script as `input`.'),
    }),
    execute: async ({ script, input }) => runUserScript(script, input, capabilities),
  });

  return {
    baseTools: {
      listFiles: tool({
        description: 'List the available ops-log export files.',
        inputSchema: z.object({}),
        execute: async () => ({ files: store.list() }),
      }),

      readFile: tool({
        description: 'Read the full text of one ops-log export file.',
        inputSchema: z.object({
          name: z.string().describe('Exact file name, e.g. "day-01.log".'),
        }),
        execute: async ({ name }) => ({ name, text: store.read(name) }),
      }),

      runScript,
    },

    metaTool: {
      name: 'createFunctionTool',
      make: (registry) => createFunctionToolFactory(registry, capabilities),
    },
  };
}

// ---------------------------------------------------------------------------
// Meta-tool: createFunctionTool (scenarios B/D)
// ---------------------------------------------------------------------------

const RESERVED_TOOL_NAMES = new Set(['listFiles', 'readFile', 'runScript', 'createFunctionTool']);

function createFunctionToolFactory(
  registry: DynamicToolRegistry,
  capabilities: { readFile: (name: string) => string },
) {
  return tool({
    description: [
      'Create a reusable tool from a JavaScript function.',
      'Use it when the same parsing/transformation logic is needed repeatedly on new inputs:',
      'write the code once here; afterwards call the created tool with just the input values',
      'instead of rewriting the code. Prefer small lookup-style parameters (e.g. a file name)',
      'over bulk data; the script can load data itself via readFile(name).',
      SCRIPT_CONVENTION,
    ].join(' '),
    inputSchema: z.object({
      name: z
        .string()
        .regex(/^[a-z][a-zA-Z0-9]{1,39}$/)
        .describe('camelCase name for the new tool, e.g. "extractCancellations".'),
      description: z.string().describe('What the tool returns and when to use it.'),
      script: z.string().describe('JavaScript function body implementing the reusable logic.'),
      parameters: z.array(
        z.object({
          name: z.string().describe('Parameter name; the script reads them from `input.<name>`.'),
          type: z.enum(['string', 'number']),
          description: z.string(),
        }),
      ),
    }),
    execute: async (definition) => {
      if (RESERVED_TOOL_NAMES.has(definition.name) || definition.name in registry) {
        return {
          success: false,
          error: `A tool named "${definition.name}" already exists. Use the existing tool or pick another name.`,
        };
      }

      // Syntax-check the script before registering, so errors come back to the agent here.
      try {
        compileUserScript(definition.script);
      } catch (error) {
        return { success: false, error: `Script rejected: ${errorMessage(error)}` };
      }

      registry[definition.name] = dynamicTool({
        description: definition.description,
        inputSchema: buildInputSchema(definition.parameters) as FlexibleSchema<unknown>,
        execute: async (input) => runUserScript(definition.script, input, capabilities),
      });

      const signature = definition.parameters.map((p) => `${p.name} (${p.type})`).join(', ') || 'none';
      return {
        success: true,
        message: `Tool "${definition.name}" created and available from now on. Parameters: ${signature}.`,
      };
    },
  });
}

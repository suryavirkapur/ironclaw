import { dynamicTool, tool, type FlexibleSchema } from 'ai';
import { z } from 'zod';
import { createRepositoryStore } from '../../repositories/compliance.js';
import { compileUserScript, runUserScript } from '../../sandbox.js';
import {
  buildInputSchema,
  errorMessage,
  type DynamicToolRegistry,
  type ToolkitInstance,
} from '../toolkit.js';

const CONVENTION = [
  'The script is the body of a JavaScript function: inputs are in `input` and it must',
  'return a JSON-serializable value. It can call `readRepository(name)` to obtain',
  '`{name, files}`, where `files` maps repository-relative paths to text.',
  'No require/process/network; 1s timeout; output capped.',
].join(' ');

export function createRepositoryToolkit(): ToolkitInstance {
  const store = createRepositoryStore();
  const capabilities = { readRepository: (name: string) => store.read(name) };

  return {
    baseTools: {
      listRepositories: tool({
        description: 'List repository snapshots available for compliance auditing.',
        inputSchema: z.object({}),
        execute: async () => ({ repositories: store.list() }),
      }),
      runScript: tool({
        description: `Execute inline JavaScript against repository snapshots. ${CONVENTION}`,
        inputSchema: z.object({
          script: z.string().describe('JavaScript function body.'),
          input: z.unknown().optional(),
        }),
        execute: async ({ script, input }) => runUserScript(script, input, capabilities),
      }),
    },
    metaTool: {
      name: 'createFunctionTool',
      make: (registry) => createFunctionTool(registry, capabilities),
    },
  };
}

function createFunctionTool(
  registry: DynamicToolRegistry,
  capabilities: { readRepository: (name: string) => unknown },
) {
  return tool({
    description: [
      'Create and register a reusable JavaScript tool for repeated repository analysis.',
      'Use a repository name as the small input; load its files with readRepository(name).',
      CONVENTION,
    ].join(' '),
    inputSchema: z.object({
      name: z.string().regex(/^[a-z][a-zA-Z0-9]{1,39}$/),
      description: z.string(),
      script: z.string(),
      parameters: z.array(
        z.object({
          name: z.string(),
          type: z.enum(['string', 'number']),
          description: z.string(),
        }),
      ),
    }),
    execute: async (definition) => {
      if (definition.name in registry) {
        return { success: false, error: `Tool "${definition.name}" already exists.` };
      }
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
      return { success: true, message: `Tool "${definition.name}" created.` };
    },
  });
}

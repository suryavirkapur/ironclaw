import { dynamicTool, tool, type FlexibleSchema } from 'ai';
import { z } from 'zod';
import { Database } from '../../db/database.js';
import { createSeededDatabase } from '../../db/seed.js';
import {
  buildInputSchema,
  errorMessage,
  type DynamicToolRegistry,
  type ToolkitInstance,
} from '../toolkit.js';

/** Creates a fresh, byte-identical database plus the SQL toolkit around it. */
export function createSqlToolkit(): ToolkitInstance {
  const db = createSeededDatabase();
  return {
    baseTools: {
      listTables: tool({
        description: 'List all tables in the booking database.',
        inputSchema: z.object({}),
        execute: async () => ({ tables: db.listTables() }),
      }),

      describeTable: tool({
        description: 'Show the columns (name, type, nullability) of one table.',
        inputSchema: z.object({
          tableName: z.string().describe('Exact table name, e.g. "bookings".'),
        }),
        execute: async ({ tableName }) => ({ columns: db.describeTable(tableName) }),
      }),

      runQuery: tool({
        description:
          'Run a single read-only SELECT/WITH query against the booking database and get back rows (max 50). Use :named parameters for filter values.',
        inputSchema: z.object({
          sql: z.string().describe('One SELECT or WITH statement.'),
          params: z
            .record(z.string(), z.union([z.string(), z.number()]))
            .optional()
            .describe('Values for :named parameters used in sql, without the colon prefix.'),
        }),
        execute: async ({ sql, params }) => db.query(sql, params ?? {}),
      }),
    },

    metaTool: {
      name: 'createQueryTool',
      make: (registry) => createQueryToolFactory(db, registry),
    },
  };
}

// ---------------------------------------------------------------------------
// Meta-tool: createQueryTool (scenarios B/D)
// ---------------------------------------------------------------------------

export type CreateToolResult =
  | { success: true; message: string }
  | { success: false; error: string };

const RESERVED_TOOL_NAMES = new Set(['listTables', 'describeTable', 'runQuery', 'createQueryTool']);

function createQueryToolFactory(db: Database, registry: DynamicToolRegistry) {
  return tool({
    description: [
      'Create a reusable tool that wraps one parameterized SQL query.',
      'Use it when the same complex query is needed repeatedly with different values:',
      'write the full SQL once here with :named parameters; afterwards call the created',
      'tool with just the parameter values instead of rewriting the full SQL.',
    ].join(' '),
    inputSchema: z.object({
      name: z
        .string()
        .regex(/^[a-z][a-zA-Z0-9]{1,39}$/)
        .describe('camelCase name for the new tool, e.g. "lookupBookingForCancellation".'),
      description: z.string().describe('What the tool returns and when to use it.'),
      sql: z
        .string()
        .describe('One read-only SELECT/WITH query with :named parameters for every variable value.'),
      parameters: z.array(
        z.object({
          name: z.string().describe('Parameter name as used in sql, without the colon.'),
          type: z.enum(['string', 'number']),
          description: z.string(),
        }),
      ),
    }),
    execute: async (definition) => createDynamicQueryTool(db, registry, definition),
  });
}

interface QueryToolDefinition {
  name: string;
  description: string;
  sql: string;
  parameters: { name: string; type: 'string' | 'number'; description: string }[];
}

function createDynamicQueryTool(
  db: Database,
  registry: DynamicToolRegistry,
  definition: QueryToolDefinition,
): CreateToolResult {
  if (RESERVED_TOOL_NAMES.has(definition.name) || definition.name in registry) {
    return {
      success: false,
      error: `A tool named "${definition.name}" already exists. Use the existing tool or pick another name.`,
    };
  }

  let usedParams: string[];
  try {
    usedParams = extractNamedParams(definition.sql);
    db.validate(definition.sql);
  } catch (error) {
    return { success: false, error: `SQL rejected: ${errorMessage(error)}` };
  }

  const declared = new Set(definition.parameters.map((p) => p.name));
  const undeclared = usedParams.filter((p) => !declared.has(p));
  if (undeclared.length > 0) {
    return {
      success: false,
      error: `SQL uses :${undeclared.join(', :')} but these are not declared in "parameters".`,
    };
  }

  registry[definition.name] = dynamicTool({
    description: definition.description,
    // The schema is constructed at runtime from the agent's parameter declarations,
    // so it is typed as unknown here but fully validated for the model.
    inputSchema: buildInputSchema(definition.parameters) as FlexibleSchema<unknown>,
    execute: async (input) => {
      const values = input as Record<string, string | number | null>;
      // Only bind parameters the SQL actually uses; node:sqlite rejects unknown keys.
      const bound = Object.fromEntries(usedParams.map((p) => [p, values[p] ?? null]));
      return db.query(definition.sql, bound);
    },
  });

  const signature = definition.parameters.map((p) => `${p.name} (${p.type})`).join(', ') || 'none';
  return {
    success: true,
    message: `Tool "${definition.name}" created and available from now on. Parameters: ${signature}.`,
  };
}

/** Finds `:name` / `@name` / `$name` placeholders (deduplicated, in order of appearance). */
function extractNamedParams(sql: string): string[] {
  const matches = [...sql.matchAll(/[:@$]([A-Za-z_]\w*)/g)].map((m) => m[1] as string);
  return [...new Set(matches)];
}

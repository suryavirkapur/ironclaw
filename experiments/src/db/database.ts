import { DatabaseSync } from 'node:sqlite';
import { ROW_LIMIT } from '../config.js';

export type Row = Record<string, string | number | null>;

/** Values that can be bound to :named parameters. */
export type SqlValue = string | number | null;
export type SqlParams = Record<string, SqlValue>;

export interface QueryResult {
  rows: Row[];
  truncated: boolean;
}

/**
 * Thin wrapper around the in-memory SQLite database.
 * Only read-only single-statement SELECT/WITH queries are allowed.
 */
export class Database {
  private readonly db = new DatabaseSync(':memory:');

  exec(ddl: string): void {
    this.db.exec(ddl);
  }

  run(sql: string, params: SqlParams = {}): void {
    const stmt = this.db.prepare(sql);
    if (hasParams(params)) stmt.run(params);
    else stmt.run();
  }

  listTables(): string[] {
    const rows = this.db
      .prepare(
        "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
      )
      .all() as unknown as { name: string }[];
    return rows.map((row) => row.name);
  }

  describeTable(tableName: string): Row[] {
    if (!this.listTables().includes(tableName)) {
      throw new Error(`Unknown table "${tableName}". Call listTables to see what exists.`);
    }
    // tableName is validated against the whitelist above, so interpolation is safe.
    return this.db.prepare(`PRAGMA table_info("${tableName}")`).all() as unknown as Row[];
  }

  /** Throws if the SQL is not a single read-only statement, then checks it compiles. */
  validate(sql: string): void {
    assertReadOnly(sql);
    this.db.prepare(sql);
  }

  query(sql: string, params: SqlParams = {}): QueryResult {
    assertReadOnly(sql);
    const stmt = this.db.prepare(sql);
    const rows = (hasParams(params) ? stmt.all(params) : stmt.all()) as unknown as Row[];
    if (rows.length > ROW_LIMIT) {
      return { rows: rows.slice(0, ROW_LIMIT), truncated: true };
    }
    return { rows, truncated: false };
  }
}

function assertReadOnly(sql: string): void {
  const normalized = sql.trim().replace(/;\s*$/, '');
  if (normalized.includes(';')) {
    throw new Error('Only a single SQL statement is allowed.');
  }
  if (!/^(select|with)\b/i.test(normalized)) {
    throw new Error('Only read-only SELECT/WITH queries are allowed.');
  }
}

/** node:sqlite rejects empty binding objects, so only pass params when present. */
function hasParams(params: SqlParams): boolean {
  return Object.keys(params).length > 0;
}

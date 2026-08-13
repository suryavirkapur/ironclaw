use rusqlite::Connection;

pub fn init_db(conn: &Connection) -> rusqlite::Result<()> {
    // Base table and WAL mode (always idempotent)
    conn.execute_batch(
        "
    PRAGMA journal_mode=WAL;

    CREATE TABLE IF NOT EXISTS core_agent_memories (
        id          TEXT PRIMARY KEY,
        content     TEXT NOT NULL,
        vector      BLOB,
        metadata    TEXT,
        created_at  REAL NOT NULL,
        updated_at  REAL NOT NULL
    );
    CREATE TABLE IF NOT EXISTS core_agent_memory_schema (
        singleton      INTEGER PRIMARY KEY CHECK (singleton = 1),
        schema_version INTEGER NOT NULL
    );
    INSERT OR IGNORE INTO core_agent_memory_schema (singleton, schema_version) VALUES (1, 0);
    ",
    )?;

    // Use a component-local schema version. Ironclaw shares this SQLite file with
    // other subsystems, so PRAGMA user_version is not safe for migrations.
    let version: i32 = conn.query_row(
        "SELECT schema_version FROM core_agent_memory_schema WHERE singleton = 1",
        [],
        |row| row.get(0),
    )?;

    if version < 1 {
        // Drop old FTS and triggers, recreate with metadata-aware triggers.
        // The FTS5 content column label stays "content" but the data fed into it
        // via triggers now concatenates content + metadata JSON text, so text
        // searches match metadata values (e.g. searching "kafka" hits
        // core_agent_memories where metadata contains {"topic": "kafka"}).
        conn.execute_batch(
            "
      DROP TRIGGER IF EXISTS core_agent_memories_ai;
      DROP TRIGGER IF EXISTS core_agent_memories_ad;
      DROP TRIGGER IF EXISTS core_agent_memories_au;
      DROP TABLE IF EXISTS core_agent_memories_fts;

      CREATE VIRTUAL TABLE core_agent_memories_fts USING fts5(
          content,
          content=core_agent_memories,
          content_rowid=rowid
      );

      CREATE TRIGGER core_agent_memories_ai AFTER INSERT ON core_agent_memories BEGIN
          INSERT INTO core_agent_memories_fts(rowid, content)
          VALUES (new.rowid, new.content || ' ' || COALESCE(new.metadata, ''));
      END;

      CREATE TRIGGER core_agent_memories_ad AFTER DELETE ON core_agent_memories BEGIN
          INSERT INTO core_agent_memories_fts(core_agent_memories_fts, rowid, content)
          VALUES('delete', old.rowid, old.content || ' ' || COALESCE(old.metadata, ''));
      END;

      CREATE TRIGGER core_agent_memories_au AFTER UPDATE ON core_agent_memories BEGIN
          INSERT INTO core_agent_memories_fts(core_agent_memories_fts, rowid, content)
          VALUES('delete', old.rowid, old.content || ' ' || COALESCE(old.metadata, ''));
          INSERT INTO core_agent_memories_fts(rowid, content)
          VALUES (new.rowid, new.content || ' ' || COALESCE(new.metadata, ''));
      END;

      INSERT INTO core_agent_memories_fts(core_agent_memories_fts) VALUES('rebuild');

      UPDATE core_agent_memory_schema SET schema_version = 1 WHERE singleton = 1;
      ",
        )?;
    }

    // Re-read version after potential v0->v1 migration
    let version: i32 = conn.query_row(
        "SELECT schema_version FROM core_agent_memory_schema WHERE singleton = 1",
        [],
        |row| row.get(0),
    )?;

    if version < 2 {
        // Add access tracking columns for v0.3 ranking boost.
        // SQLite ALTER TABLE ADD COLUMN sets defaults for existing rows.
        conn.execute_batch(
            "
      ALTER TABLE core_agent_memories ADD COLUMN last_accessed REAL DEFAULT 0.0;
      ALTER TABLE core_agent_memories ADD COLUMN access_count INTEGER DEFAULT 0;
      UPDATE core_agent_memory_schema SET schema_version = 2 WHERE singleton = 1;
      ",
        )?;
    }

    // Re-read version after potential v1->v2 migration
    let version: i32 = conn.query_row(
        "SELECT schema_version FROM core_agent_memory_schema WHERE singleton = 1",
        [],
        |row| row.get(0),
    )?;

    if version < 3 {
        // Expression index on metadata $.type for fast filtered queries.
        // json_extract expression indexes are supported since SQLite 3.9.0.
        conn.execute_batch(
            "
      CREATE INDEX IF NOT EXISTS idx_core_agent_memories_type
          ON core_agent_memories(json_extract(metadata, '$.type'));
      UPDATE core_agent_memory_schema SET schema_version = 3 WHERE singleton = 1;
      ",
        )?;
    }

    Ok(())
}

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use rusqlite::functions::FunctionFlags;
use rusqlite::{params, Connection};
use serde_json::json;
use sha2::{Digest, Sha256};

const DEFAULT_EMBEDDING_DIM: usize = 384;
const DEFAULT_EMBEDDING_CACHE_TTL_MS: u64 = 86_400_000;

#[derive(Clone, Debug)]
pub struct Chunk {
    pub id: String,
    pub path: String,
    pub heading: String,
    pub ordinal: u32,
    pub content: String,
}

#[derive(Clone, Debug)]
pub struct ScoredChunk {
    pub chunk: Chunk,
    pub score: f32,
    pub lexical_score: f32,
    pub semantic_score: f32,
}

#[derive(Clone, Debug)]
pub struct NewMemory {
    pub user_id: String,
    pub importance: i64,
    pub pinned: bool,
    pub kind: String,
    pub text: String,
    pub tags_json: String,
    pub source_json: String,
}

#[derive(Clone, Debug)]
pub struct MemoryRow {
    pub id: i64,
    pub created_ms: u64,
    pub updated_ms: u64,
    pub importance: i64,
    pub pinned: bool,
    pub kind: String,
    pub text: String,
    pub tags_json: String,
}

#[derive(Clone, Debug)]
pub struct RetrievedMemory {
    pub memory: MemoryRow,
    pub score: f32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SummaryOutcome {
    pub turn_range: String,
    pub summary_id: i64,
    pub distilled_count: usize,
}

#[derive(Clone, Debug)]
pub struct HybridSearchConfig {
    pub embedding_model: String,
    pub vector_weight: f32,
    pub keyword_weight: f32,
    pub embedding_cache_size: usize,
    pub max_chunk_bytes: usize,
    pub embedding_cache_ttl_ms: u64,
}

impl Default for HybridSearchConfig {
    fn default() -> Self {
        Self {
            embedding_model: "text-embedding-3-small".to_string(),
            vector_weight: 0.7,
            keyword_weight: 0.3,
            embedding_cache_size: 1000,
            max_chunk_bytes: 2048,
            embedding_cache_ttl_ms: DEFAULT_EMBEDDING_CACHE_TTL_MS,
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("memory error: {message}")]
pub struct MemoryError {
    message: String,
}

impl MemoryError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

pub fn chunk_markdown(path: &str, input: &str, max_bytes: usize) -> Vec<Chunk> {
    let mut chunks = Vec::new();
    let mut heading_stack: Vec<String> = Vec::new();
    let mut heading = String::new();
    let mut ordinal = 0u32;
    let mut buffer = String::new();
    let target_max = max_bytes.max(128);

    for line in input.lines() {
        if let Some((level, heading_text)) = parse_heading(line) {
            if !buffer.trim().is_empty() {
                chunks.push(build_chunk(path, &heading, ordinal, &buffer));
                ordinal += 1;
                buffer.clear();
            }
            while heading_stack.len() >= level {
                let _ = heading_stack.pop();
            }
            heading_stack.push(heading_text);
            heading = heading_stack.join(" > ");
            buffer.push_str(line);
            buffer.push('\n');
            continue;
        }

        buffer.push_str(line);
        buffer.push('\n');

        if buffer.len() >= target_max {
            chunks.push(build_chunk(path, &heading, ordinal, &buffer));
            ordinal += 1;
            buffer.clear();
        }
    }

    if !buffer.trim().is_empty() {
        chunks.push(build_chunk(path, &heading, ordinal, &buffer));
    }

    chunks
}

pub fn initialize_schema(conn: &Connection) -> Result<(), MemoryError> {
    register_vector_sql_functions(conn)?;
    try_load_vector_extension(conn)?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS chunks (\
            id TEXT PRIMARY KEY,\
            path TEXT NOT NULL,\
            heading TEXT NOT NULL,\
            ordinal INTEGER NOT NULL,\
            content TEXT NOT NULL\
        );\
        CREATE VIRTUAL TABLE IF NOT EXISTS chunks_fts USING fts5(\
            content, path, heading\
        );\
        CREATE TABLE IF NOT EXISTS embeddings (\
            id TEXT PRIMARY KEY,\
            chunk_id TEXT NOT NULL,\
            vector BLOB NOT NULL,\
            model TEXT NOT NULL,\
            UNIQUE(chunk_id, model)\
        );\
        CREATE INDEX IF NOT EXISTS idx_embeddings_model ON embeddings(model);\
        CREATE TABLE IF NOT EXISTS embedding_cache (\
            id INTEGER PRIMARY KEY AUTOINCREMENT,\
            text_hash TEXT NOT NULL UNIQUE,\
            embedding BLOB NOT NULL,\
            created_at INTEGER NOT NULL\
        );\
        CREATE INDEX IF NOT EXISTS idx_embedding_cache_created_at \
            ON embedding_cache(created_at ASC);\
        CREATE TABLE IF NOT EXISTS compactions (\
            id INTEGER PRIMARY KEY AUTOINCREMENT,\
            timestamp_ms INTEGER NOT NULL,\
            summary_hash TEXT NOT NULL,\
            source_range TEXT NOT NULL,\
            affected_docs TEXT NOT NULL\
        );\
        CREATE TABLE IF NOT EXISTS memories (\
            id INTEGER PRIMARY KEY AUTOINCREMENT,\
            user_id TEXT NOT NULL,\
            created_ms INTEGER NOT NULL,\
            updated_ms INTEGER NOT NULL,\
            importance INTEGER NOT NULL,\
            pinned INTEGER NOT NULL DEFAULT 0,\
            kind TEXT NOT NULL,\
            text TEXT NOT NULL,\
            tags TEXT NOT NULL DEFAULT '[]',\
            source TEXT NOT NULL DEFAULT '{}',\
            disabled INTEGER NOT NULL DEFAULT 0\
        );\
        CREATE TABLE IF NOT EXISTS summaries (\
            id INTEGER PRIMARY KEY AUTOINCREMENT,\
            user_id TEXT NOT NULL,\
            created_ms INTEGER NOT NULL,\
            turn_range TEXT NOT NULL,\
            text TEXT NOT NULL\
        );\
        CREATE VIRTUAL TABLE IF NOT EXISTS memories_fts USING fts5(\
            text, tags, content='memories', content_rowid='id'\
        );\
        CREATE INDEX IF NOT EXISTS idx_memories_user_updated \
            ON memories(user_id, disabled, updated_ms DESC);\
        CREATE UNIQUE INDEX IF NOT EXISTS idx_memories_user_kind_text \
            ON memories(user_id, kind, text);\
        CREATE UNIQUE INDEX IF NOT EXISTS idx_summaries_user_turn_range \
            ON summaries(user_id, turn_range);",
    )
    .map_err(|err| MemoryError::new(format!("schema init failed: {err}")))?;
    Ok(())
}

pub fn index_chunks(
    conn: &Connection,
    chunks: &[Chunk],
    config: &HybridSearchConfig,
) -> Result<(), MemoryError> {
    if chunks.is_empty() {
        return Ok(());
    }

    let path_chunks = group_chunks_by_path(chunks);
    run_atomic(conn, |db| {
        for chunk in chunks {
            db.execute(
                r#"
                INSERT INTO chunks (id, path, heading, ordinal, content)
                VALUES (?1, ?2, ?3, ?4, ?5)
                ON CONFLICT(id) DO UPDATE SET
                    path = excluded.path,
                    heading = excluded.heading,
                    ordinal = excluded.ordinal,
                    content = excluded.content
                "#,
                params![
                    chunk.id,
                    chunk.path,
                    chunk.heading,
                    chunk.ordinal,
                    chunk.content
                ],
            )
            .map_err(|err| MemoryError::new(format!("chunk upsert failed: {err}")))?;
        }

        for (path, ids) in path_chunks {
            delete_stale_chunks_for_path(db, &path, &ids)?;
        }

        rebuild_chunks_fts(db)?;
        Ok(())
    })?;

    reembed_missing_vectors(conn, chunks, config)?;
    Ok(())
}

pub fn delete_chunks_by_path(conn: &Connection, path: &str) -> Result<usize, MemoryError> {
    let mut removed = 0usize;
    run_atomic(conn, |db| {
        let mut stmt = db
            .prepare("SELECT id FROM chunks WHERE path = ?1")
            .map_err(|err| MemoryError::new(format!("delete path prepare failed: {err}")))?;
        let rows = stmt
            .query_map(params![path], |row| row.get::<_, String>(0))
            .map_err(|err| MemoryError::new(format!("delete path query failed: {err}")))?;

        let mut stale_ids = Vec::new();
        for row in rows {
            let chunk_id =
                row.map_err(|err| MemoryError::new(format!("delete path row failed: {err}")))?;
            stale_ids.push(chunk_id);
        }

        for chunk_id in &stale_ids {
            db.execute(
                "DELETE FROM embeddings WHERE chunk_id = ?1",
                params![chunk_id],
            )
            .map_err(|err| MemoryError::new(format!("delete embedding by path failed: {err}")))?;
        }

        let changed = db
            .execute("DELETE FROM chunks WHERE path = ?1", params![path])
            .map_err(|err| MemoryError::new(format!("delete chunks by path failed: {err}")))?;
        removed = changed;
        rebuild_chunks_fts(db)?;
        Ok(())
    })?;
    Ok(removed)
}

pub fn reindex_markdown(
    conn: &Connection,
    path: &str,
    input: &str,
    config: &HybridSearchConfig,
) -> Result<Vec<Chunk>, MemoryError> {
    let chunks = chunk_markdown(path, input, config.max_chunk_bytes);
    index_chunks(conn, &chunks, config)?;
    Ok(chunks)
}

pub fn lexical_search(
    conn: &Connection,
    query: &str,
    limit: usize,
) -> Result<Vec<(Chunk, f32)>, MemoryError> {
    let mut stmt = conn
        .prepare(
            r#"
            SELECT chunks.id, chunks.path, chunks.heading, chunks.ordinal, chunks.content,
                   bm25(chunks_fts) as score
            FROM chunks_fts
            JOIN chunks ON chunks.rowid = chunks_fts.rowid
            WHERE chunks_fts MATCH ?1
            ORDER BY score ASC
            LIMIT ?2
            "#,
        )
        .map_err(|err| MemoryError::new(format!("fts prepare failed: {err}")))?;

    let mut rows = stmt
        .query(params![query, limit as i64])
        .map_err(|err| MemoryError::new(format!("fts query failed: {err}")))?;

    let mut results = Vec::new();
    while let Some(row) = rows
        .next()
        .map_err(|err| MemoryError::new(format!("fts row read failed: {err}")))?
    {
        let chunk = Chunk {
            id: row.get(0).map_err(map_row_err("id"))?,
            path: row.get(1).map_err(map_row_err("path"))?,
            heading: row.get(2).map_err(map_row_err("heading"))?,
            ordinal: row.get(3).map_err(map_row_err("ordinal"))?,
            content: row.get(4).map_err(map_row_err("content"))?,
        };
        let score: f32 = row.get(5).map_err(map_row_err("score"))?;
        let normalized = 1.0 / (1.0 + score.max(0.0));
        results.push((chunk, normalized));
    }

    normalize_results(results)
}

pub fn semantic_search(
    conn: &Connection,
    query_embedding: &[f32],
    model: &str,
    limit: usize,
) -> Result<Vec<(Chunk, f32)>, MemoryError> {
    if query_embedding.is_empty() {
        return Ok(Vec::new());
    }

    let query_blob = encode_embedding_blob(query_embedding);
    let mut stmt = conn
        .prepare(
            r#"
            SELECT chunks.id, chunks.path, chunks.heading, chunks.ordinal, chunks.content,
                   CASE
                       WHEN vec_norm(embeddings.vector) = 0 OR vec_norm(?1) = 0 THEN 0
                       ELSE vec_dot(embeddings.vector, ?1) /
                           (vec_norm(embeddings.vector) * vec_norm(?1))
                   END AS cosine
            FROM embeddings
            JOIN chunks ON chunks.id = embeddings.chunk_id
            WHERE embeddings.model = ?2
            ORDER BY cosine DESC
            LIMIT ?3
            "#,
        )
        .map_err(|err| MemoryError::new(format!("semantic prepare failed: {err}")))?;

    let mut rows = stmt
        .query(params![query_blob, model, limit as i64])
        .map_err(|err| MemoryError::new(format!("semantic query failed: {err}")))?;

    let mut results = Vec::new();
    while let Some(row) = rows
        .next()
        .map_err(|err| MemoryError::new(format!("semantic row read failed: {err}")))?
    {
        let chunk = Chunk {
            id: row.get(0).map_err(map_row_err("id"))?,
            path: row.get(1).map_err(map_row_err("path"))?,
            heading: row.get(2).map_err(map_row_err("heading"))?,
            ordinal: row.get(3).map_err(map_row_err("ordinal"))?,
            content: row.get(4).map_err(map_row_err("content"))?,
        };
        let cosine = row.get::<_, f32>(5).map_err(map_row_err("cosine"))?;
        let bounded = ((cosine + 1.0) / 2.0).clamp(0.0, 1.0);
        results.push((chunk, bounded));
    }

    normalize_results(results)
}

pub fn hybrid_search(
    conn: &Connection,
    query: &str,
    limit: usize,
    config: &HybridSearchConfig,
) -> Result<Vec<ScoredChunk>, MemoryError> {
    let lexical = lexical_search(conn, query, limit)?;

    let query_embedding = embedding_for_text(
        conn,
        query,
        &config.embedding_model,
        current_time_ms(),
        config.embedding_cache_ttl_ms,
        config.embedding_cache_size,
    );

    let (semantic, had_embedding_error) = match query_embedding {
        Ok(vector) => (
            semantic_search(conn, &vector, &config.embedding_model, limit)?,
            false,
        ),
        Err(_) => (Vec::new(), true),
    };

    let semantic_source = if had_embedding_error {
        like_fallback_search(conn, query, limit)?
    } else {
        semantic
    };

    let mut fused = hybrid_fusion(
        lexical,
        semantic_source,
        config.vector_weight,
        config.keyword_weight,
    );
    if fused.len() > limit {
        fused.truncate(limit);
    }
    Ok(fused)
}

pub fn hybrid_fusion(
    lexical: Vec<(Chunk, f32)>,
    semantic: Vec<(Chunk, f32)>,
    semantic_weight: f32,
    lexical_weight: f32,
) -> Vec<ScoredChunk> {
    let mut map = HashMap::new();

    for (chunk, score) in lexical {
        map.insert(
            chunk.id.clone(),
            ScoredChunk {
                chunk,
                score: 0.0,
                lexical_score: score,
                semantic_score: 0.0,
            },
        );
    }

    for (chunk, score) in semantic {
        map.entry(chunk.id.clone())
            .and_modify(|entry| entry.semantic_score = score)
            .or_insert(ScoredChunk {
                chunk,
                score: 0.0,
                lexical_score: 0.0,
                semantic_score: score,
            });
    }

    for entry in map.values_mut() {
        entry.score = semantic_weight * entry.semantic_score + lexical_weight * entry.lexical_score;
    }

    let mut results: Vec<ScoredChunk> = map.into_values().collect();
    results.sort_by(|a, b| b.score.total_cmp(&a.score));
    results
}

pub fn redact_secrets(input: &str) -> String {
    let mut out = Vec::new();
    for token in input.split_whitespace() {
        out.push(redact_token(token));
    }
    out.join(" ")
}

pub fn upsert_memory(
    conn: &Connection,
    now_ms: u64,
    memory: &NewMemory,
) -> Result<i64, MemoryError> {
    let sanitized = redact_secrets(memory.text.trim());
    if sanitized.is_empty() {
        return Err(MemoryError::new("memory text is empty"));
    }
    conn.execute(
        r#"
        INSERT INTO memories (
            user_id, created_ms, updated_ms, importance, pinned, kind, text, tags, source, disabled
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 0)
        ON CONFLICT(user_id, kind, text) DO UPDATE SET
            updated_ms = excluded.updated_ms,
            importance = MAX(memories.importance, excluded.importance),
            pinned = MAX(memories.pinned, excluded.pinned),
            tags = excluded.tags,
            source = excluded.source,
            disabled = 0
        "#,
        params![
            memory.user_id,
            now_ms as i64,
            now_ms as i64,
            memory.importance,
            if memory.pinned { 1 } else { 0 },
            memory.kind,
            sanitized,
            memory.tags_json,
            memory.source_json,
        ],
    )
    .map_err(|err| MemoryError::new(format!("memory upsert failed: {err}")))?;

    let id = conn
        .query_row(
            "SELECT id FROM memories WHERE user_id = ?1 AND kind = ?2 AND text = ?3",
            params![memory.user_id, memory.kind, sanitized],
            |row| row.get::<_, i64>(0),
        )
        .map_err(|err| MemoryError::new(format!("memory id lookup failed: {err}")))?;

    sync_memory_fts(conn, id)?;
    Ok(id)
}

pub fn list_pinned_memories(
    conn: &Connection,
    user_id: &str,
    limit: usize,
) -> Result<Vec<MemoryRow>, MemoryError> {
    let mut stmt = conn
        .prepare(
            r#"
            SELECT id, created_ms, updated_ms, importance, pinned, kind, text, tags
            FROM memories
            WHERE user_id = ?1 AND disabled = 0 AND pinned = 1
            ORDER BY importance DESC, updated_ms DESC
            LIMIT ?2
            "#,
        )
        .map_err(|err| MemoryError::new(format!("pinned prepare failed: {err}")))?;
    let mut rows = stmt
        .query(params![user_id, limit as i64])
        .map_err(|err| MemoryError::new(format!("pinned query failed: {err}")))?;
    let mut out = Vec::new();
    while let Some(row) = rows
        .next()
        .map_err(|err| MemoryError::new(format!("pinned row read failed: {err}")))?
    {
        out.push(MemoryRow {
            id: row.get(0).map_err(map_row_err("id"))?,
            created_ms: row.get::<_, i64>(1).map_err(map_row_err("created_ms"))? as u64,
            updated_ms: row.get::<_, i64>(2).map_err(map_row_err("updated_ms"))? as u64,
            importance: row.get(3).map_err(map_row_err("importance"))?,
            pinned: row.get::<_, i64>(4).map_err(map_row_err("pinned"))? == 1,
            kind: row.get(5).map_err(map_row_err("kind"))?,
            text: row.get(6).map_err(map_row_err("text"))?,
            tags_json: row.get(7).map_err(map_row_err("tags"))?,
        });
    }
    Ok(out)
}

pub fn forget_memory_by_id(conn: &Connection, user_id: &str, id: i64) -> Result<bool, MemoryError> {
    let changed = conn
        .execute(
            "UPDATE memories SET disabled = 1, updated_ms = ?1 WHERE id = ?2 AND user_id = ?3",
            params![current_time_ms() as i64, id, user_id],
        )
        .map_err(|err| MemoryError::new(format!("forget by id failed: {err}")))?;
    if changed > 0 {
        rebuild_memory_fts(conn)?;
    }
    Ok(changed > 0)
}

pub fn forget_memories_by_query(
    conn: &Connection,
    user_id: &str,
    query: &str,
    limit: usize,
) -> Result<usize, MemoryError> {
    let fts_query = fts_query_from_text(query);
    if fts_query.is_empty() {
        return Ok(0);
    }

    let mut stmt = conn
        .prepare(
            r#"
            SELECT memories.id
            FROM memories_fts
            JOIN memories ON memories.id = memories_fts.rowid
            WHERE memories_fts MATCH ?1
              AND memories.user_id = ?2
              AND memories.disabled = 0
            ORDER BY bm25(memories_fts) ASC
            LIMIT ?3
            "#,
        )
        .map_err(|err| MemoryError::new(format!("forget query prepare failed: {err}")))?;
    let ids: Result<Vec<i64>, rusqlite::Error> = stmt
        .query_map(params![fts_query, user_id, limit as i64], |row| {
            row.get::<_, i64>(0)
        })
        .map_err(|err| MemoryError::new(format!("forget query failed: {err}")))?
        .collect();
    let ids = ids.map_err(|err| MemoryError::new(format!("forget query failed: {err}")))?;

    let mut count = 0usize;
    for id in ids {
        if forget_memory_by_id(conn, user_id, id)? {
            count += 1;
        }
    }
    Ok(count)
}

pub fn retrieve_memories(
    conn: &Connection,
    user_id: &str,
    query: &str,
    limit: usize,
    now_ms: u64,
) -> Result<Vec<RetrievedMemory>, MemoryError> {
    let fts_query = fts_query_from_text(query);
    if fts_query.is_empty() {
        return Ok(Vec::new());
    }

    let mut stmt = conn
        .prepare(
            r#"
            SELECT memories.id, memories.created_ms, memories.updated_ms, memories.importance,
                   memories.pinned, memories.kind, memories.text, memories.tags,
                   bm25(memories_fts) AS rank
            FROM memories_fts
            JOIN memories ON memories.id = memories_fts.rowid
            WHERE memories_fts MATCH ?1
              AND memories.user_id = ?2
              AND memories.disabled = 0
            LIMIT ?3
            "#,
        )
        .map_err(|err| MemoryError::new(format!("memory retrieval prepare failed: {err}")))?;

    let mut rows = stmt
        .query(params![fts_query, user_id, limit as i64])
        .map_err(|err| MemoryError::new(format!("memory retrieval query failed: {err}")))?;

    let mut rescored = Vec::new();
    while let Some(row) = rows
        .next()
        .map_err(|err| MemoryError::new(format!("memory retrieval row read failed: {err}")))?
    {
        let updated_ms = row.get::<_, i64>(2).map_err(map_row_err("updated_ms"))? as u64;
        let importance = row.get::<_, i64>(3).map_err(map_row_err("importance"))?;
        let pinned = row.get::<_, i64>(4).map_err(map_row_err("pinned"))? == 1;
        let bm25 = row.get::<_, f32>(8).map_err(map_row_err("rank"))?;

        let lexical = 1.0 / (1.0 + bm25.max(0.0));
        let age_days = now_ms.saturating_sub(updated_ms) as f32 / 86_400_000.0;
        let recency = 1.0 / (1.0 + age_days / 7.0);
        let importance_score = (importance.clamp(0, 100) as f32) / 100.0;
        let pin_boost = if pinned { 0.35 } else { 0.0 };
        let score = lexical * 0.55 + recency * 0.25 + importance_score * 0.20 + pin_boost;

        rescored.push(RetrievedMemory {
            memory: MemoryRow {
                id: row.get(0).map_err(map_row_err("id"))?,
                created_ms: row.get::<_, i64>(1).map_err(map_row_err("created_ms"))? as u64,
                updated_ms,
                importance,
                pinned,
                kind: row.get(5).map_err(map_row_err("kind"))?,
                text: row.get(6).map_err(map_row_err("text"))?,
                tags_json: row.get(7).map_err(map_row_err("tags"))?,
            },
            score,
        });
    }

    rescored.sort_by(|a, b| b.score.total_cmp(&a.score));
    Ok(rescored)
}

pub fn build_memory_block(memories: &[RetrievedMemory], budget_chars: usize) -> String {
    if memories.is_empty() {
        return String::new();
    }

    let target = budget_chars.clamp(2_000, 4_000);
    let mut out = String::from("memory context:\n");
    for item in memories {
        let mut text = item.memory.text.replace('\n', " ");
        if text.len() > 280 {
            text.truncate(280);
        }
        let line = format!(
            "- id={} kind={} importance={} pinned={} text={}\n",
            item.memory.id,
            item.memory.kind,
            item.memory.importance,
            item.memory.pinned,
            text.trim()
        );
        if out.len() + line.len() > target {
            break;
        }
        out.push_str(&line);
    }
    out
}

pub fn maybe_summarize_session(
    conn: &Connection,
    user_id: &str,
    session_id: &str,
    user_messages: &[String],
    now_ms: u64,
) -> Result<Option<SummaryOutcome>, MemoryError> {
    let cadence = 20usize;
    if user_messages.is_empty() || user_messages.len() % cadence != 0 {
        return Ok(None);
    }

    let end = user_messages.len();
    let start = end.saturating_sub(cadence).saturating_add(1);
    let turn_range = format!("{session_id}:{start}-{end}");

    let exists: i64 = conn
        .query_row(
            "SELECT COUNT(1) FROM summaries WHERE user_id = ?1 AND turn_range = ?2",
            params![user_id, turn_range],
            |row| row.get(0),
        )
        .map_err(|err| MemoryError::new(format!("summary lookup failed: {err}")))?;
    if exists > 0 {
        return Ok(None);
    }

    let slice = &user_messages[user_messages.len() - cadence..];
    let summary_text = build_summary_text(slice);
    conn.execute(
        "INSERT INTO summaries (user_id, created_ms, turn_range, text) VALUES (?1, ?2, ?3, ?4)",
        params![
            user_id,
            now_ms as i64,
            turn_range,
            redact_secrets(&summary_text)
        ],
    )
    .map_err(|err| MemoryError::new(format!("summary insert failed: {err}")))?;

    let summary_id = conn.last_insert_rowid();
    let candidates = distill_durable_facts(slice, 3);
    for candidate in &candidates {
        let source = json!({
            "kind": "summary_distill",
            "session_id": session_id,
            "turn_range": format!("{start}-{end}"),
            "summary_id": summary_id,
        })
        .to_string();
        upsert_memory(
            conn,
            now_ms,
            &NewMemory {
                user_id: user_id.to_string(),
                importance: 70,
                pinned: false,
                kind: "durable_fact".to_string(),
                text: candidate.to_string(),
                tags_json: "[\"auto\",\"durable\"]".to_string(),
                source_json: source,
            },
        )?;
    }

    Ok(Some(SummaryOutcome {
        turn_range: format!("{start}-{end}"),
        summary_id,
        distilled_count: candidates.len(),
    }))
}

pub fn distill_durable_facts(messages: &[String], limit: usize) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    let prefixes = [
        "i am ",
        "i'm ",
        "my name is ",
        "i prefer ",
        "please remember ",
        "we are building ",
        "i am working on ",
        "project ",
        "constraint:",
    ];

    for raw in messages {
        let normalized = raw.trim();
        if normalized.is_empty() {
            continue;
        }
        let lowered = normalized.to_lowercase();
        if !prefixes.iter().any(|prefix| lowered.contains(prefix)) {
            continue;
        }
        let mut candidate = redact_secrets(normalized);
        if candidate.len() > 220 {
            candidate.truncate(220);
        }
        if seen.insert(candidate.clone()) {
            out.push(candidate);
        }
        if out.len() >= limit {
            break;
        }
    }

    out
}

fn sync_memory_fts(conn: &Connection, id: i64) -> Result<(), MemoryError> {
    let _ = id;
    rebuild_memory_fts(conn)
}

fn build_chunk(path: &str, heading: &str, ordinal: u32, content: &str) -> Chunk {
    let trimmed = content.trim_end().to_string();
    let id = chunk_id(path, heading, ordinal, &trimmed);
    Chunk {
        id,
        path: path.to_string(),
        heading: heading.to_string(),
        ordinal,
        content: trimmed,
    }
}

fn group_chunks_by_path(chunks: &[Chunk]) -> HashMap<String, HashSet<String>> {
    let mut out = HashMap::new();
    for chunk in chunks {
        out.entry(chunk.path.clone())
            .or_insert_with(HashSet::new)
            .insert(chunk.id.clone());
    }
    out
}

fn delete_stale_chunks_for_path(
    conn: &Connection,
    path: &str,
    keep_ids: &HashSet<String>,
) -> Result<(), MemoryError> {
    let mut stale_ids = Vec::new();
    let mut stmt = conn
        .prepare("SELECT id FROM chunks WHERE path = ?1")
        .map_err(|err| MemoryError::new(format!("stale select prepare failed: {err}")))?;
    let rows = stmt
        .query_map(params![path], |row| row.get::<_, String>(0))
        .map_err(|err| MemoryError::new(format!("stale select query failed: {err}")))?;

    for row in rows {
        let id = row.map_err(|err| MemoryError::new(format!("stale select row failed: {err}")))?;
        if !keep_ids.contains(&id) {
            stale_ids.push(id);
        }
    }

    for stale_id in stale_ids {
        conn.execute(
            "DELETE FROM embeddings WHERE chunk_id = ?1",
            params![stale_id],
        )
        .map_err(|err| MemoryError::new(format!("stale embedding delete failed: {err}")))?;
        conn.execute("DELETE FROM chunks WHERE id = ?1", params![stale_id])
            .map_err(|err| MemoryError::new(format!("stale chunk delete failed: {err}")))?;
    }

    Ok(())
}

fn parse_heading(line: &str) -> Option<(usize, String)> {
    let trimmed = line.trim_start();
    if !trimmed.starts_with('#') {
        return None;
    }
    let mut level = 0usize;
    for ch in trimmed.chars() {
        if ch == '#' {
            level += 1;
            continue;
        }
        break;
    }
    if level == 0 || level > 6 {
        return None;
    }
    let text = trimmed[level..].trim();
    if text.is_empty() {
        return None;
    }
    Some((level, text.to_string()))
}

fn chunk_id(path: &str, heading: &str, ordinal: u32, content: &str) -> String {
    let content_hash = digest_hex(content.as_bytes());
    let mut hasher = Sha256::new();
    hasher.update(path.as_bytes());
    hasher.update(b"|");
    hasher.update(heading.as_bytes());
    hasher.update(b"|");
    hasher.update(ordinal.to_be_bytes());
    hasher.update(b"|");
    hasher.update(content_hash.as_bytes());
    to_hex(&hasher.finalize())
}

fn normalize_results(results: Vec<(Chunk, f32)>) -> Result<Vec<(Chunk, f32)>, MemoryError> {
    if results.is_empty() {
        return Ok(results);
    }
    let mut min = f32::MAX;
    let mut max = f32::MIN;
    for (_, score) in &results {
        min = min.min(*score);
        max = max.max(*score);
    }
    let range = (max - min).max(1e-6);
    let mut normalized = Vec::with_capacity(results.len());
    for (chunk, score) in results {
        if !score.is_finite() {
            return Err(MemoryError::new("non-finite score during normalization"));
        }
        normalized.push((chunk, (score - min) / range));
    }
    Ok(normalized)
}

fn map_row_err(field: &'static str) -> impl FnOnce(rusqlite::Error) -> MemoryError {
    move |err| MemoryError::new(format!("row field {field} failed: {err}"))
}

fn try_load_vector_extension(conn: &Connection) -> Result<(), MemoryError> {
    let path = match std::env::var("IRONCLAW_VECTOR_EXTENSION") {
        Ok(value) => PathBuf::from(value),
        Err(_) => return Ok(()),
    };
    unsafe {
        conn.load_extension_enable()
            .map_err(|err| MemoryError::new(format!("vector extension enable failed: {err}")))?;
    }

    let load_result = unsafe { conn.load_extension(&path, None::<&str>) }
        .map_err(|err| MemoryError::new(format!("vector extension load failed: {err}")));

    conn.load_extension_disable()
        .map_err(|err| MemoryError::new(format!("vector extension disable failed: {err}")))?;
    load_result
}

fn register_vector_sql_functions(conn: &Connection) -> Result<(), MemoryError> {
    conn.create_scalar_function("vec_dot", 2, FunctionFlags::SQLITE_DETERMINISTIC, |ctx| {
        let a = ctx.get_raw(0).as_blob()?;
        let b = ctx.get_raw(1).as_blob()?;
        let va = decode_embedding_blob_sql(a)?;
        let vb = decode_embedding_blob_sql(b)?;
        if va.len() != vb.len() {
            return Ok(0.0f64);
        }
        let mut dot = 0.0f64;
        for idx in 0..va.len() {
            dot += va[idx] as f64 * vb[idx] as f64;
        }
        Ok(dot)
    })
    .map_err(|err| MemoryError::new(format!("register vec_dot failed: {err}")))?;

    conn.create_scalar_function("vec_norm", 1, FunctionFlags::SQLITE_DETERMINISTIC, |ctx| {
        let a = ctx.get_raw(0).as_blob()?;
        let va = decode_embedding_blob_sql(a)?;
        let mut sum = 0.0f64;
        for value in va {
            sum += (value as f64) * (value as f64);
        }
        Ok(sum.sqrt())
    })
    .map_err(|err| MemoryError::new(format!("register vec_norm failed: {err}")))?;

    Ok(())
}

fn decode_embedding_blob_sql(blob: &[u8]) -> Result<Vec<f32>, rusqlite::types::FromSqlError> {
    if blob.len() < 4 {
        return Err(rusqlite::types::FromSqlError::InvalidBlobSize {
            expected_size: 4,
            blob_size: blob.len(),
        });
    }
    let mut len_bytes = [0u8; 4];
    len_bytes.copy_from_slice(&blob[..4]);
    let len = u32::from_le_bytes(len_bytes) as usize;
    let expected_size = 4 + len * 4;
    if blob.len() != expected_size {
        return Err(rusqlite::types::FromSqlError::InvalidBlobSize {
            expected_size,
            blob_size: blob.len(),
        });
    }

    let mut out = Vec::with_capacity(len);
    for idx in 0..len {
        let start = 4 + idx * 4;
        let mut item = [0u8; 4];
        item.copy_from_slice(&blob[start..start + 4]);
        out.push(f32::from_le_bytes(item));
    }
    Ok(out)
}

fn embedding_for_text(
    conn: &Connection,
    text: &str,
    model: &str,
    now_ms: u64,
    ttl_ms: u64,
    cache_size: usize,
) -> Result<Vec<f32>, MemoryError> {
    let text_hash = text_hash(model, text);
    if let Some(embedding) = embedding_cache_get(conn, &text_hash, now_ms, ttl_ms)? {
        return Ok(embedding);
    }

    let embedding = generate_embedding(model, text)?;
    embedding_cache_put(conn, &text_hash, &embedding, now_ms, ttl_ms, cache_size)?;
    Ok(embedding)
}

fn generate_embedding(model: &str, text: &str) -> Result<Vec<f32>, MemoryError> {
    match model {
        "text-embedding-3-small" | "simple-384" => Ok(simple_384_embedding(text)),
        _ => Err(MemoryError::new(format!(
            "unsupported embedding model: {model}"
        ))),
    }
}

fn simple_384_embedding(text: &str) -> Vec<f32> {
    let mut vector = vec![0.0f32; DEFAULT_EMBEDDING_DIM];
    for (token_idx, token) in tokenize_for_embedding(text).into_iter().enumerate() {
        let digest = Sha256::digest(token.as_bytes());
        let weight = 1.0f32 / ((token_idx + 1) as f32).sqrt();
        for offset in 0..6usize {
            let a = digest[offset * 2] as usize;
            let b = digest[offset * 2 + 1] as usize;
            let index = (a * 256 + b + token_idx + offset * 31) % DEFAULT_EMBEDDING_DIM;
            let sign = if digest[12 + offset] & 1 == 0 {
                1.0f32
            } else {
                -1.0f32
            };
            vector[index] += sign * weight;
        }
    }

    l2_normalize(vector)
}

fn tokenize_for_embedding(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    for token in text
        .split(|ch: char| !ch.is_alphanumeric() && ch != '_' && ch != '-')
        .filter(|token| !token.is_empty())
    {
        tokens.push(token.to_lowercase());
    }
    if tokens.is_empty() {
        tokens.push("empty".to_string());
    }
    tokens
}

fn l2_normalize(mut vector: Vec<f32>) -> Vec<f32> {
    let mut sum = 0.0f32;
    for value in &vector {
        sum += value * value;
    }
    let norm = sum.sqrt();
    if norm <= 1e-8 {
        return vector;
    }
    for value in &mut vector {
        *value /= norm;
    }
    vector
}

fn upsert_chunk_embedding(
    conn: &Connection,
    chunk_id: &str,
    model: &str,
    embedding: &[f32],
) -> Result<(), MemoryError> {
    let id = digest_hex(format!("{chunk_id}:{model}").as_bytes());
    let blob = encode_embedding_blob(embedding);
    conn.execute(
        r#"
        INSERT INTO embeddings (id, chunk_id, vector, model)
        VALUES (?1, ?2, ?3, ?4)
        ON CONFLICT(id) DO UPDATE SET
            vector = excluded.vector,
            model = excluded.model
        "#,
        params![id, chunk_id, blob, model],
    )
    .map_err(|err| MemoryError::new(format!("embedding upsert failed: {err}")))?;
    Ok(())
}

fn reembed_missing_vectors(
    conn: &Connection,
    chunks: &[Chunk],
    config: &HybridSearchConfig,
) -> Result<usize, MemoryError> {
    let mut inserted = 0usize;
    for chunk in chunks {
        let exists: i64 = conn
            .query_row(
                "SELECT COUNT(1) FROM embeddings WHERE chunk_id = ?1 AND model = ?2",
                params![chunk.id, config.embedding_model],
                |row| row.get(0),
            )
            .map_err(|err| MemoryError::new(format!("embedding exists check failed: {err}")))?;
        if exists > 0 {
            continue;
        }
        let embedding = embedding_for_text(
            conn,
            &chunk.content,
            &config.embedding_model,
            current_time_ms(),
            config.embedding_cache_ttl_ms,
            config.embedding_cache_size,
        )?;
        upsert_chunk_embedding(conn, &chunk.id, &config.embedding_model, &embedding)?;
        inserted += 1;
    }
    Ok(inserted)
}

fn embedding_cache_get(
    conn: &Connection,
    text_hash: &str,
    now_ms: u64,
    ttl_ms: u64,
) -> Result<Option<Vec<f32>>, MemoryError> {
    let row: Result<(Vec<u8>, i64), rusqlite::Error> = conn.query_row(
        "SELECT embedding, created_at FROM embedding_cache WHERE text_hash = ?1",
        params![text_hash],
        |row| {
            let blob = row.get::<_, Vec<u8>>(0)?;
            let created_at = row.get::<_, i64>(1)?;
            Ok((blob, created_at))
        },
    );

    match row {
        Ok((blob, created_at)) => {
            let created_at_u64 = if created_at < 0 {
                0u64
            } else {
                created_at as u64
            };
            if now_ms.saturating_sub(created_at_u64) > ttl_ms {
                conn.execute(
                    "DELETE FROM embedding_cache WHERE text_hash = ?1",
                    params![text_hash],
                )
                .map_err(|err| MemoryError::new(format!("cache ttl delete failed: {err}")))?;
                return Ok(None);
            }
            conn.execute(
                "UPDATE embedding_cache SET created_at = ?1 WHERE text_hash = ?2",
                params![now_ms as i64, text_hash],
            )
            .map_err(|err| MemoryError::new(format!("cache touch failed: {err}")))?;
            let embedding = decode_embedding_blob(&blob)?;
            Ok(Some(embedding))
        }
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(err) => Err(MemoryError::new(format!("cache get failed: {err}"))),
    }
}

fn embedding_cache_put(
    conn: &Connection,
    text_hash: &str,
    embedding: &[f32],
    now_ms: u64,
    ttl_ms: u64,
    max_entries: usize,
) -> Result<(), MemoryError> {
    let blob = encode_embedding_blob(embedding);
    conn.execute(
        r#"
        INSERT INTO embedding_cache (text_hash, embedding, created_at)
        VALUES (?1, ?2, ?3)
        ON CONFLICT(text_hash) DO UPDATE SET
            embedding = excluded.embedding,
            created_at = excluded.created_at
        "#,
        params![text_hash, blob, now_ms as i64],
    )
    .map_err(|err| MemoryError::new(format!("cache put failed: {err}")))?;

    prune_embedding_cache(conn, now_ms, ttl_ms, max_entries)
}

fn prune_embedding_cache(
    conn: &Connection,
    now_ms: u64,
    ttl_ms: u64,
    max_entries: usize,
) -> Result<(), MemoryError> {
    let cutoff = now_ms.saturating_sub(ttl_ms);
    conn.execute(
        "DELETE FROM embedding_cache WHERE created_at < ?1",
        params![cutoff as i64],
    )
    .map_err(|err| MemoryError::new(format!("cache ttl prune failed: {err}")))?;

    let count: i64 = conn
        .query_row("SELECT COUNT(1) FROM embedding_cache", [], |row| row.get(0))
        .map_err(|err| MemoryError::new(format!("cache count failed: {err}")))?;

    let count_u = if count < 0 { 0usize } else { count as usize };
    if count_u <= max_entries {
        return Ok(());
    }
    let remove = count_u - max_entries;
    conn.execute(
        "DELETE FROM embedding_cache WHERE id IN (\
            SELECT id FROM embedding_cache ORDER BY created_at ASC, id ASC LIMIT ?1\
        )",
        params![remove as i64],
    )
    .map_err(|err| MemoryError::new(format!("cache lru prune failed: {err}")))?;
    Ok(())
}

fn like_fallback_search(
    conn: &Connection,
    query: &str,
    limit: usize,
) -> Result<Vec<(Chunk, f32)>, MemoryError> {
    let like = format!("%{}%", query.trim());
    let mut stmt = conn
        .prepare(
            r#"
            SELECT id, path, heading, ordinal, content
            FROM chunks
            WHERE content LIKE ?1 OR path LIKE ?1 OR heading LIKE ?1
            ORDER BY ordinal ASC
            LIMIT ?2
            "#,
        )
        .map_err(|err| MemoryError::new(format!("like prepare failed: {err}")))?;

    let mut rows = stmt
        .query(params![like, limit as i64])
        .map_err(|err| MemoryError::new(format!("like query failed: {err}")))?;

    let mut rank = 0usize;
    let mut out = Vec::new();
    while let Some(row) = rows
        .next()
        .map_err(|err| MemoryError::new(format!("like row read failed: {err}")))?
    {
        rank += 1;
        let score = 1.0f32 / (rank as f32);
        out.push((
            Chunk {
                id: row.get(0).map_err(map_row_err("id"))?,
                path: row.get(1).map_err(map_row_err("path"))?,
                heading: row.get(2).map_err(map_row_err("heading"))?,
                ordinal: row.get(3).map_err(map_row_err("ordinal"))?,
                content: row.get(4).map_err(map_row_err("content"))?,
            },
            score,
        ));
    }
    normalize_results(out)
}

fn encode_embedding_blob(embedding: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + embedding.len() * 4);
    out.extend_from_slice(&(embedding.len() as u32).to_le_bytes());
    for value in embedding {
        out.extend_from_slice(&value.to_le_bytes());
    }
    out
}

fn decode_embedding_blob(blob: &[u8]) -> Result<Vec<f32>, MemoryError> {
    decode_embedding_blob_sql(blob)
        .map_err(|err| MemoryError::new(format!("blob decode failed: {err}")))
}

fn text_hash(model: &str, text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(model.as_bytes());
    hasher.update(b"|");
    hasher.update(text.as_bytes());
    to_hex(&hasher.finalize())
}

fn run_atomic<T, F>(conn: &Connection, op: F) -> Result<T, MemoryError>
where
    F: FnOnce(&Connection) -> Result<T, MemoryError>,
{
    conn.execute_batch("begin immediate transaction")
        .map_err(|err| MemoryError::new(format!("begin transaction failed: {err}")))?;

    let op_result = op(conn);
    match op_result {
        Ok(value) => {
            conn.execute_batch("commit")
                .map_err(|err| MemoryError::new(format!("commit failed: {err}")))?;
            Ok(value)
        }
        Err(err) => {
            let rollback_result = conn.execute_batch("rollback");
            if let Err(rollback_err) = rollback_result {
                return Err(MemoryError::new(format!(
                    "rollback failed after error: {err}; rollback error: {rollback_err}"
                )));
            }
            Err(err)
        }
    }
}

fn rebuild_chunks_fts(conn: &Connection) -> Result<(), MemoryError> {
    conn.execute("DELETE FROM chunks_fts", [])
        .map_err(|err| MemoryError::new(format!("chunks fts clear failed: {err}")))?;
    conn.execute(
        "INSERT INTO chunks_fts (rowid, content, path, heading)\
        SELECT rowid, content, path, heading FROM chunks",
        [],
    )
    .map_err(|err| MemoryError::new(format!("chunks fts repopulate failed: {err}")))?;
    Ok(())
}

fn digest_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    to_hex(&digest)
}

fn to_hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(hex_char(byte >> 4));
        out.push(hex_char(byte & 0x0f));
    }
    out
}

fn hex_char(nibble: u8) -> char {
    match nibble {
        0..=9 => (b'0' + nibble) as char,
        10..=15 => (b'a' + (nibble - 10)) as char,
        _ => '0',
    }
}

fn fts_query_from_text(input: &str) -> String {
    let mut terms = Vec::new();
    for token in input
        .split(|ch: char| !ch.is_alphanumeric() && ch != '_' && ch != '-')
        .filter(|token| token.len() >= 2)
    {
        terms.push(token.to_lowercase());
    }
    terms.join(" OR ")
}

fn build_summary_text(messages: &[String]) -> String {
    let mut lines = Vec::new();
    for (idx, message) in messages.iter().enumerate() {
        let mut trimmed = message.trim().replace('\n', " ");
        if trimmed.len() > 180 {
            trimmed.truncate(180);
        }
        lines.push(format!("{}. {}", idx + 1, trimmed));
    }
    format!(
        "rolling summary of last {} user messages: {}",
        messages.len(),
        lines.join(" | ")
    )
}

fn redact_token(token: &str) -> String {
    let lowered = token.to_lowercase();
    if lowered.starts_with("sk-") && token.len() >= 20 {
        return "[redacted]".to_string();
    }
    if lowered.starts_with("api_key=") || lowered.starts_with("apikey=") {
        return "api_key=[redacted]".to_string();
    }
    if lowered.contains("-----begin") && lowered.contains("private") {
        return "[redacted_private_key]".to_string();
    }
    token.to_string()
}

fn current_time_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};

    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(duration) => duration.as_millis() as u64,
        Err(_) => 0,
    }
}

fn rebuild_memory_fts(conn: &Connection) -> Result<(), MemoryError> {
    conn.execute(
        "INSERT INTO memories_fts(memories_fts) VALUES('rebuild')",
        [],
    )
    .map_err(|err| MemoryError::new(format!("memory fts rebuild failed: {err}")))?;
    Ok(())
}

#[cfg(test)]
mod lib_test;

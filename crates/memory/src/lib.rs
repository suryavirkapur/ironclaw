use std::collections::HashSet;
use std::path::PathBuf;

use rusqlite::{params, Connection};
use serde_json::json;
use sha2::{Digest, Sha256};

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
    let mut heading = String::new();
    let mut ordinal = 0u32;
    let mut buffer = String::new();

    for line in input.lines() {
        if is_heading(line) {
            if !buffer.trim().is_empty() {
                chunks.push(build_chunk(path, &heading, ordinal, &buffer));
                ordinal += 1;
                buffer.clear();
            }
            heading = heading_text(line);
            buffer.push_str(line);
            buffer.push('\n');
            continue;
        }

        buffer.push_str(line);
        buffer.push('\n');

        if buffer.len() >= max_bytes {
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
            chunk_id TEXT PRIMARY KEY,\
            embedding BLOB\
        );\
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

pub fn index_chunks(conn: &Connection, chunks: &[Chunk]) -> Result<(), MemoryError> {
    for chunk in chunks {
        conn.execute(
            "INSERT INTO chunks (id, path, heading, ordinal, content)\
            VALUES (?1, ?2, ?3, ?4, ?5)\
            ON CONFLICT(id) DO UPDATE SET\
                path = excluded.path,\
                heading = excluded.heading,\
                ordinal = excluded.ordinal,\
                content = excluded.content",
            params![
                chunk.id,
                chunk.path,
                chunk.heading,
                chunk.ordinal,
                chunk.content
            ],
        )
        .map_err(|err| MemoryError::new(format!("chunk upsert failed: {err}")))?;

        let rowid: i64 = conn
            .query_row(
                "SELECT rowid FROM chunks WHERE id = ?1",
                params![chunk.id],
                |row| row.get(0),
            )
            .map_err(|err| MemoryError::new(format!("chunk rowid lookup failed: {err}")))?;

        conn.execute("DELETE FROM chunks_fts WHERE rowid = ?1", params![rowid])
            .map_err(|err| MemoryError::new(format!("fts delete failed: {err}")))?;

        conn.execute(
            "INSERT INTO chunks_fts (rowid, content, path, heading)\
            VALUES (?1, ?2, ?3, ?4)",
            params![rowid, chunk.content, chunk.path, chunk.heading],
        )
        .map_err(|err| MemoryError::new(format!("fts insert failed: {err}")))?;
    }
    Ok(())
}

pub fn lexical_search(
    conn: &Connection,
    query: &str,
    limit: usize,
) -> Result<Vec<(Chunk, f32)>, MemoryError> {
    let mut stmt = conn
        .prepare(
            "SELECT chunks.id, chunks.path, chunks.heading, chunks.ordinal, chunks.content,\
            bm25(chunks_fts) as score\
            FROM chunks_fts\
            JOIN chunks ON chunks.rowid = chunks_fts.rowid\
            WHERE chunks_fts MATCH ?1\
            ORDER BY score ASC\
            LIMIT ?2",
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

pub fn semantic_search_stub(
    _conn: &Connection,
    _query_embedding: &[f32],
    _limit: usize,
) -> Result<Vec<(Chunk, f32)>, MemoryError> {
    Ok(Vec::new())
}

pub fn hybrid_fusion(
    lexical: Vec<(Chunk, f32)>,
    semantic: Vec<(Chunk, f32)>,
    semantic_weight: f32,
    lexical_weight: f32,
) -> Vec<ScoredChunk> {
    let mut map = std::collections::HashMap::new();

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
    let id = chunk_id(path, heading, ordinal, content);
    Chunk {
        id,
        path: path.to_string(),
        heading: heading.to_string(),
        ordinal,
        content: content.trim_end().to_string(),
    }
}

fn is_heading(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with('#')
}

fn heading_text(line: &str) -> String {
    line.trim_start_matches('#').trim().to_string()
}

fn chunk_id(path: &str, heading: &str, ordinal: u32, content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(path.as_bytes());
    hasher.update(b"|");
    hasher.update(heading.as_bytes());
    hasher.update(b"|");
    hasher.update(ordinal.to_be_bytes());
    hasher.update(b"|");
    hasher.update(content.as_bytes());
    to_hex(&hasher.finalize())
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
    Ok(results
        .into_iter()
        .map(|(chunk, score)| (chunk, (score - min) / range))
        .collect())
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
mod test {
    use rusqlite::Connection;

    use super::{
        initialize_schema, maybe_summarize_session, redact_secrets, retrieve_memories,
        upsert_memory, NewMemory,
    };

    #[test]
    fn summary_cadence_triggers_at_twenty_messages() {
        let conn = Connection::open_in_memory().expect("open memory db");
        initialize_schema(&conn).expect("init schema");

        let nineteen = (0..19).map(|idx| format!("m{idx}")).collect::<Vec<_>>();
        let first = maybe_summarize_session(&conn, "owner", "telegram-1", &nineteen, 1)
            .expect("maybe summarize");
        assert!(first.is_none());

        let twenty = (0..20).map(|idx| format!("m{idx}")).collect::<Vec<_>>();
        let second = maybe_summarize_session(&conn, "owner", "telegram-1", &twenty, 2)
            .expect("maybe summarize");
        assert!(second.is_some());
    }

    #[test]
    fn retrieval_includes_pinned_memory() {
        let conn = Connection::open_in_memory().expect("open memory db");
        initialize_schema(&conn).expect("init schema");

        upsert_memory(
            &conn,
            10,
            &NewMemory {
                user_id: "owner".to_string(),
                importance: 90,
                pinned: true,
                kind: "preference".to_string(),
                text: "user prefers rust and sqlite".to_string(),
                tags_json: "[\"pref\"]".to_string(),
                source_json: "{}".to_string(),
            },
        )
        .expect("upsert pinned");

        let found = retrieve_memories(&conn, "owner", "rust", 10, 100).expect("retrieve");
        assert!(!found.is_empty());
        assert!(found[0].memory.pinned);
    }

    #[test]
    fn redacts_key_like_strings() {
        let input = "token sk-abc12345678901234567890 api_key=secret";
        let redacted = redact_secrets(input);
        assert!(!redacted.contains("sk-abc12345678901234567890"));
        assert!(redacted.contains("[redacted]"));
        assert!(redacted.contains("api_key=[redacted]"));
    }
}

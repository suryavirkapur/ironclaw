use rusqlite::{params, Connection};
use sha2::{Digest, Sha256};
use std::path::PathBuf;

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
        );",
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

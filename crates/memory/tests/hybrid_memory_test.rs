use memory::{
    build_memory_block, chunk_markdown, hybrid_fusion, hybrid_search, initialize_schema,
    maybe_summarize_session, reindex_markdown, retrieve_memories, upsert_memory, Chunk,
    HybridSearchConfig, NewMemory,
};
use rusqlite::Connection;

fn cfg() -> HybridSearchConfig {
    HybridSearchConfig {
        embedding_model: "simple-384".to_string(),
        vector_weight: 0.7,
        keyword_weight: 0.3,
        embedding_cache_size: 2,
        max_chunk_bytes: 80,
        embedding_cache_ttl_ms: 60_000,
    }
}

#[test]
fn vector_embedding_blob_storage_and_retrieval() {
    let conn = match Connection::open_in_memory() {
        Ok(value) => value,
        Err(err) => panic!("db open failed: {err}"),
    };
    if let Err(err) = initialize_schema(&conn) {
        panic!("schema init failed: {err}");
    }

    if let Err(err) = reindex_markdown(&conn, "docs/a.md", "# title\nrust retrieval", &cfg()) {
        panic!("reindex failed: {err}");
    }

    let blob_len =
        match conn.query_row("select length(vector) from embeddings limit 1", [], |row| {
            row.get::<_, i64>(0)
        }) {
            Ok(value) => value,
            Err(err) => panic!("query failed: {err}"),
        };
    assert!(blob_len > 0);
}

#[test]
fn cosine_similarity_sql_returns_expected_ordering() {
    let conn = match Connection::open_in_memory() {
        Ok(value) => value,
        Err(err) => panic!("db open failed: {err}"),
    };
    if let Err(err) = initialize_schema(&conn) {
        panic!("schema init failed: {err}");
    }

    let markdown = "# first\nrust sqlite memory\n\n# second\nbanana kiwi mango";
    if let Err(err) = reindex_markdown(&conn, "docs/cosine.md", markdown, &cfg()) {
        panic!("reindex failed: {err}");
    }

    let results = match hybrid_search(&conn, "rust sqlite", 2, &cfg()) {
        Ok(value) => value,
        Err(err) => panic!("search failed: {err}"),
    };
    assert!(!results.is_empty());
    assert!(results[0].semantic_score >= 0.5);
}

#[test]
fn hybrid_fusion_scoring_uses_seventy_thirty_weights() {
    let chunk_a = Chunk {
        id: "a".to_string(),
        path: "a.md".to_string(),
        heading: "h".to_string(),
        ordinal: 0,
        content: "alpha".to_string(),
    };
    let chunk_b = Chunk {
        id: "b".to_string(),
        path: "b.md".to_string(),
        heading: "h".to_string(),
        ordinal: 1,
        content: "beta".to_string(),
    };

    let fused = hybrid_fusion(
        vec![(chunk_a.clone(), 1.0), (chunk_b.clone(), 0.1)],
        vec![(chunk_a, 0.1), (chunk_b, 1.0)],
        0.7,
        0.3,
    );

    assert_eq!(fused[0].chunk.id, "b");
    assert!((fused[0].score - (0.7 * 1.0 + 0.3 * 0.1)).abs() < 0.001);
}

#[test]
fn embedding_cache_lru_eviction_respects_max_size() {
    let conn = match Connection::open_in_memory() {
        Ok(value) => value,
        Err(err) => panic!("db open failed: {err}"),
    };
    if let Err(err) = initialize_schema(&conn) {
        panic!("schema init failed: {err}");
    }

    let config = cfg();
    if let Err(err) = reindex_markdown(&conn, "docs/1.md", "# one\na", &config) {
        panic!("index 1 failed: {err}");
    }
    if let Err(err) = reindex_markdown(&conn, "docs/2.md", "# two\nb", &config) {
        panic!("index 2 failed: {err}");
    }
    if let Err(err) = reindex_markdown(&conn, "docs/3.md", "# three\nc", &config) {
        panic!("index 3 failed: {err}");
    }

    let cache_count = match conn.query_row("select count(1) from embedding_cache", [], |row| {
        row.get::<_, i64>(0)
    }) {
        Ok(value) => value,
        Err(err) => panic!("count query failed: {err}"),
    };
    assert!(cache_count <= config.embedding_cache_size as i64);
}

#[test]
fn markdown_chunking_preserves_heading_context() {
    let text = "# root\nintro\n## child\nchild body\n### leaf\nleaf body";
    let chunks = chunk_markdown("docs/guide.md", text, 40);

    assert!(!chunks.is_empty());
    let headings = chunks
        .iter()
        .map(|chunk| chunk.heading.as_str())
        .collect::<Vec<_>>();
    assert!(headings.iter().any(|value| value.contains("root")));
    assert!(headings.iter().any(|value| value.contains("root > child")));
    assert!(headings
        .iter()
        .any(|value| value.contains("root > child > leaf")));
}

#[test]
fn full_memory_recall_flow_returns_ranked_context() {
    let conn = match Connection::open_in_memory() {
        Ok(value) => value,
        Err(err) => panic!("db open failed: {err}"),
    };
    if let Err(err) = initialize_schema(&conn) {
        panic!("schema init failed: {err}");
    }

    let memory = NewMemory {
        user_id: "owner".to_string(),
        importance: 85,
        pinned: true,
        kind: "preference".to_string(),
        text: "user prefers rust and sqlite for long-term memory".to_string(),
        tags_json: "[\"pref\"]".to_string(),
        source_json: "{}".to_string(),
    };
    if let Err(err) = upsert_memory(&conn, 1_000, &memory) {
        panic!("upsert memory failed: {err}");
    }

    let messages = (0..20)
        .map(|index| format!("message {index}"))
        .collect::<Vec<_>>();
    let summary = match maybe_summarize_session(&conn, "owner", "telegram-1", &messages, 2_000) {
        Ok(value) => value,
        Err(err) => panic!("summary failed: {err}"),
    };
    assert!(summary.is_some());

    let retrieved = match retrieve_memories(&conn, "owner", "rust sqlite", 10, 3_000) {
        Ok(value) => value,
        Err(err) => panic!("retrieve failed: {err}"),
    };
    assert!(!retrieved.is_empty());

    let block = build_memory_block(&retrieved, 2400);
    assert!(block.contains("memory context:"));
    assert!(block.contains("rust"));
}

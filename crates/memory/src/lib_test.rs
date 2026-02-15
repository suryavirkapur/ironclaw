use rusqlite::Connection;

use super::{
    chunk_markdown, hybrid_fusion, hybrid_search, initialize_schema, maybe_summarize_session,
    redact_secrets, reindex_markdown, retrieve_memories, upsert_memory, Chunk, HybridSearchConfig,
    NewMemory,
};

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
fn summary_cadence_triggers_at_twenty_messages() {
    let conn = Connection::open_in_memory().expect("open memory db");
    initialize_schema(&conn).expect("init schema");

    let nineteen = (0..19).map(|idx| format!("m{idx}")).collect::<Vec<_>>();
    let first = maybe_summarize_session(&conn, "owner", "telegram-1", &nineteen, 1)
        .expect("maybe summarize");
    assert!(first.is_none());

    let twenty = (0..20).map(|idx| format!("m{idx}")).collect::<Vec<_>>();
    let second =
        maybe_summarize_session(&conn, "owner", "telegram-1", &twenty, 2).expect("maybe summarize");
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

#[test]
fn vector_storage_and_cosine_sql_works() {
    let conn = Connection::open_in_memory().expect("open memory db");
    initialize_schema(&conn).expect("init schema");

    let markdown = "# intro\nrust sqlite memory\n\n# details\nhybrid retrieval";
    let chunks = reindex_markdown(&conn, "docs/a.md", markdown, &cfg()).expect("reindex");
    assert!(!chunks.is_empty());

    let results = hybrid_search(&conn, "rust sqlite", 5, &cfg()).expect("hybrid search");
    assert!(!results.is_empty());
    assert!(results[0].semantic_score >= 0.0);
}

#[test]
fn hybrid_fusion_uses_weighted_scores() {
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
        ordinal: 0,
        content: "beta".to_string(),
    };

    let lexical = vec![(chunk_a.clone(), 1.0), (chunk_b.clone(), 0.2)];
    let semantic = vec![(chunk_a, 0.1), (chunk_b, 1.0)];
    let fused = hybrid_fusion(lexical, semantic, 0.7, 0.3);

    assert_eq!(fused[0].chunk.id, "b");
    assert!(fused[0].score > fused[1].score);
}

#[test]
fn lru_cache_eviction_obeys_max_entries() {
    let conn = Connection::open_in_memory().expect("open memory db");
    initialize_schema(&conn).expect("init schema");

    let config = cfg();
    reindex_markdown(&conn, "docs/a.md", "# a\none", &config).expect("index a");
    reindex_markdown(&conn, "docs/b.md", "# b\ntwo", &config).expect("index b");
    reindex_markdown(&conn, "docs/c.md", "# c\nthree", &config).expect("index c");

    let count: i64 = conn
        .query_row("SELECT COUNT(1) FROM embedding_cache", [], |row| row.get(0))
        .expect("cache count query");
    assert!(count <= config.embedding_cache_size as i64);
}

#[test]
fn markdown_chunking_preserves_heading_hierarchy() {
    let input = "# root\nintro text\n## child\nchild line\n### leaf\nleaf data";
    let chunks = chunk_markdown("docs/guide.md", input, 40);

    assert!(!chunks.is_empty());
    let headings = chunks.iter().map(|c| c.heading.clone()).collect::<Vec<_>>();
    assert!(headings.iter().any(|h| h.contains("root")));
    assert!(headings.iter().any(|h| h.contains("root > child")));
    assert!(headings.iter().any(|h| h.contains("root > child > leaf")));
}

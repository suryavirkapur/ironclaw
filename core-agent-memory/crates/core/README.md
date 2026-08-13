# core-agent-memory

Private, embedded memory for autonomous agents. Each agent opens its own SQLite
database and gets persistent facts, full-text search, optional 384-dimensional
embeddings, hybrid Reciprocal Rank Fusion, deduplication, and access/recency
ranking without a memory server.

This project is a maintained adaptation of Archit Singh's MIT-licensed
[Memori](https://github.com/archit15singh/memori) core at commit
`01adf3bfae688b042f7350cd896a763d122498ef`. See `NOTICE` and
`LICENSE-MEMORI` for attribution.

## Rust

```rust
use core_agent_memory::{Memori, SearchQuery};

let memory = Memori::open("agent-memory.db")?;
memory.insert(
    "The billing service uses idempotency keys.",
    None,
    Some(serde_json::json!({"type": "architecture", "agent_id": "backend"})),
    Some(0.92),
    true,
)?;
let results = memory.search(SearchQuery {
    text: Some("billing retries".into()),
    text_only: true,
    ..Default::default()
})?;
# Ok::<(), core_agent_memory::MemoriError>(())
```

The default Rust feature enables local FastEmbed vectors. Consumers that need
zero model downloads can use `default-features = false` for FTS5-only search.

## Node.js / N-API

```bash
npm install
npm run build
```

```js
const { AgentMemory } = require('@ironclaw/core-agent-memory')

const memory = new AgentMemory('agent-memory.db')
const stored = memory.store('Customer prefers email support', JSON.stringify({ type: 'preference' }))
const matches = memory.search({ text: 'support preference', textOnly: true, limit: 5 })
console.log(stored, matches)
```

The N-API surface is synchronous because SQLite operations are local and
serialized by the binding. It can be published as `@ironclaw/core-agent-memory`;
the Rust engine can be published independently as `core-agent-memory`.

## Ironclaw isolation

Ironclaw initializes the namespaced `core_agent_memories*` tables inside each
agent's already-isolated `guest/db/ironclaw.db`. No agent shares a connection,
database file, memory index, or direct read capability with another agent.

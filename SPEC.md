# SPEC.md: ironclaw architecture & implementation guide

version: 0.1.0
codename: jailbreak
status: final / approved for implementation

---

1. executive summary

ironclaw is a sovereign, self-hosted ai agent platform built for absolute data ownership and maximum security via a strict hypervisor-agent architecture.

the system is split into:

- the warden (host): rust daemon that boots firecracker microvms, handles auth and integrations, proxies traffic to the guest over vsock, and serves the embedded frontend.
- the inmate (guest): rust agent runtime running inside the vm, executing tools and managing “sovereign memory” stored as human-readable markdown.

key decisions in v0.1.0

- single database: one sqlite db inside the vm only at /mnt/brain/db/ironclaw.db.
- host db is separate: host auth user db may exist, but is not shared with the vm.
- per-agent default model selection: no auto-router as the default policy; each agent declares its preferred model provider.
- config files use toml, not json.
- tooling packaging: rust 1.92, node 24, rsbuild, cargo workspace layout with /ui.

---

2. system architecture: “jail cell” model

graph td
  user((user)) -->|https websocket| warden[ironclawd (host daemon)]
  warden -->|spawns| vm_pool{firecracker vm pool}

  subgraph "firecracker vm (the inmate)"
    kernel[guest kernel]
    ironclaw[irowclaw (agent runtime)]
    cron[scheduler]
    ironclaw <-->|read write| brain[/mnt/brain (virtio block)]
    ironclaw -->|executes| tools[bash filesystem browser]
    ironclaw <-->|sql| db[(sqlite: /mnt/brain/db/ironclaw.db)]
  end

  vm_pool -->|vsock| ironclaw
  brain -->|persists| hoststorage[host fs ./data/users/<uid>/brain.ext4]
  warden -->|serves| ui[/ui frontend (embedded static)]

---

3. components

3.1 ironclawd (host daemon / the warden)

role: orchestrator + api gateway + integration router + ui server
tech: rust (tokio, axum), firecracker control, vsock

responsibilities

- authenticate users (host-side store can be sqlite, but separate from the vm db).
- boot stop microvms per user (fast startup, user-scoped resources).
- proxy
  - web ui websocket sessions to guest agent via vsock
  - webhooks (telegram slack etc) to guest agent via vsock
- serve frontend at http://localhost:<port>/ui from embedded static assets.

3.2 irowclaw (guest binary / the inmate runtime)

role: intelligent agent runtime inside the vm
tech: rust (rig.rs-style tool-use), sqlite retrieval, scheduler

responsibilities

- runs cognition loop (llm calls + tool execution).
- implements sovereign memory: markdown source-of-truth, sqlite-only hybrid retrieval index.
- executes tools (shell files browser) inside vm.
- runs scheduled jobs (cron-like autonomy).

3.3 /ui (frontend)

framework: solid.js + typescript
build: rsbuild
serving: static assets embedded served by ironclawd at /ui

features

- chat interface with streaming responses.
- memory inspector: browse edit markdown files in /mnt/brain; view retrieval index status.
- instruction mode: edit /mnt/brain/instructions/*.md
- cron ui: create edit schedules (stored in toml + mirrored in sqlite)

---

4. repository & build layout (cargo workspace)

everything lives in one cargo workspace; node frontend is in /ui.

repo/
  Cargo.toml
  crates/
    ironclawd/
    irowclaw/
    common/
    tools/
    memory/
  ui/
  assets/
  rootfs/
  kernels/
  scripts/

---

5. sovereign memory (file-first)

5.1 storage hierarchy (/mnt/brain)

tier, path, purpose, format

- soul: /soul.md persona + safety boundaries markdown
- identity: /identity.md agent name + user facts markdown
- instructions: /instructions/*.md durable rules markdown
- nurture: /memory.md consolidated long-term memory markdown
- stream: /logs/YYYY-MM-DD.md daily append-only stream markdown
- cron: /cron/jobs.toml schedules and job definitions toml
- config: /config/*.toml runtime config and agent profiles toml
- db: /db/ironclaw.db single db inside vm sqlite

principle: markdown is the canonical truth; db is derived index cache.

---

6. single-db hybrid retrieval (sqlite-only)

6.1 lexical search

- sqlite fts5 table for chunk content and key metadata.
- exact matching + bm25 ranking.

6.2 semantic search

- store embeddings in sqlite using a vector extension (sqlite-vec or sqlite-vss).
- ann similarity search by cosine dot.

6.3 fusion ranking

- semantic: 0.70
- lexical: 0.30
- normalize both scores to 0..1
- score = 0.7 * semantic + 0.3 * lexical
- return top-k chunks + provenance (doc path + headings)

6.4 indexing model

- parse markdown into stable, heading-aware chunks.
- chunk constraints: max tokens bytes per chunk
- stable chunk ids derived from (path, heading, ordinal, hash(content))
- on file change
  - update chunk rows
  - update fts
  - update embeddings

---

7. pre-compaction flush (context safety)

to prevent catastrophic forgetting

1. track token usage during active sessions.
2. when usage crosses limit - buffer, pause interaction.
3. summarize oldest context slice into /mnt/brain/memory.md
4. log compaction record in sqlite: timestamp, source range, summary hash, affected docs
5. evict raw context from live session state.

---

8. tooling & autonomy

8.1 tools (guest-safe)

all tools run inside the vm; host stays protected.

- bashtool: shell commands (bounded, logged).
- filetool: read write restricted to /mnt/brain and /tmp/workspace.
- browserttool: headless fetch scrape (configurable allowlist denylist).
- memorytool
  - remember(text) appends to markdown + queues indexing
  - recall(query) hybrid retrieval from sqlite

8.2 scheduler (cron)

scheduler runs inside irowclaw.

job definitions stored in toml: /mnt/brain/cron/jobs.toml
sqlite mirrors job state for ui querying: last_run, next_run, status, last_result_ref

execution flow

1. scheduler triggers job
2. agent runs job in separate task thread context
3. results written to /logs/YYYY-MM-DD.md
4. optional notification returned to ironclawd to relay to integration channel

---

9. model & provider layer (per-agent defaults)

9.1 providers

openrouter is the primary aggregator.

9.2 supported models (default set)

- gemini 3 pro
- claude opus 4.5
- kimi k2 k2.5
- deepseek v3.2

9.3 policy

default model per agent

- each agent profile declares provider and model
- no automatic routing by default in v0.1.0

---

10. configuration (toml)

10.1 host (ironclawd)

config location example: ~/.config/ironclaw/ironclawd.toml
contains

- server port bind
- firecracker paths (kernel, rootfs template)
- per-user storage dirs
- integration tokens
- ui serving settings

10.2 guest (irowclaw)

config location: /mnt/brain/config/irowclaw.toml
contains

- default agent profile name
- provider credential references
- tool policy
- indexing settings

10.3 agent profiles

location: /mnt/brain/config/agents/*.toml

- name persona file references
- default model provider
- tool permissions
- retrieval overrides

---

11. security architecture (“soul-evil” defense)

1. immutable guest rootfs: vm boots from read-only root filesystem.
2. partitioned persistence: only /mnt/brain persists.
3. soul guard: host watches changes to /mnt/brain/soul.md; large suspicious diffs trigger security halt requiring manual approval.

additional baseline controls

- network egress rules (deny-by-default optional)
- tool allowlists
- audit logs for tool calls + memory writes

---

12. protocol: warden ↔ inmate (vsock)

transport: vsock (binary framing)

messages

- UserMessage (chat input)
- ToolResult
- StreamDelta (token stream)
- JobTrigger JobStatus
- FileOpRequest

all messages include

- user_id, session_id
- monotonic msg_id
- timestamp

---

13. development configuration

- rust: 1.92
- node: 24+
- frontend build: rsbuild; output embedded into ironclawd
- database vm-only sqlite at /mnt/brain/db/ironclaw.db
- host database may exist separately

---

14. implementation roadmap (v0.1.0)

phase a: guest runtime (irowclaw)

- toml config loader
- markdown memory read write
- sqlite schema + fts5 + vector extension
- hybrid retrieval fusion
- tool execution sandbox
- scheduler + jobs.toml

phase b: host daemon (ironclawd)

- axum api + websocket streaming
- firecracker vm lifecycle + per-user disk mounts
- vsock protocol bridge
- embed serve /ui

phase c: ui (/ui)

- chat ui + streaming
- memory inspector + instruction editor
- cron job editor

---

15. final confirmations

- sqlite lives inside /mnt/brain only
- vm-only for agent memory index
- host db separate is acceptable
- default model chosen per agent profile
- toml configs everywhere

optional switch later
- embeddings computed locally inside vm vs via provider

---

16. gap analysis: ironclaw vs openclaw

ironclaw prioritizes security/isolation (vm-based) over convenience/extensibility compared to openclaw (process-based).

16.1 missing integrations
openclaw supports a wide array of messaging channels out-of-the-box.
ironclaw currently supports only telegram.

missing:
- whatsapp
- slack

16.2 missing high-level skills
openclaw has specialized skills for common productivity apps.
ironclaw relies on generic `bash` or `browser` tools for these, lacking optimized, structured interfaces.

missing:
- calendar (read/write events)
- email (send/receive/draft)
- github (issue/pr management via api, not just git cli)

16.3 dynamic extensibility
openclaw can "autonomously generate and install new skills" and has a community ecosystem.
ironclaw tools are compiled into the strict `irowclaw` guest binary.
- missing: dynamic skill loading/generation at runtime.
- missing: plugin/extension system.

16.4 platform support
openclaw runs on macos, windows, and linux.
ironclaw is strictly linux-only due to firecracker/kvm dependency.

16.5 configuration philosophy
openclaw is "conversation-first" (config via chat).
ironclaw is "configuration-as-code" (explicit toml files).
- missing: natural language configuration/setup wizard.

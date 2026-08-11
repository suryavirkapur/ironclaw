# Engineering workspace direction

The custom workspace should be a collaboration product over the farm control plane, not another
agent runtime. Telegram and the web workspace must produce the same durable tasks and observe the
same results.

## Current slice

`/ui` now provides a live roster, delivery board, task inspector, capability-aware assignment form,
and architecture view. It reads and writes the existing `/api/farm/*` endpoints; no duplicate task
state exists in the browser.

## Proposed collaboration model

```text
Workspace
├── channels
│   ├── #delivery
│   ├── #architecture
│   └── #incidents
├── messages and threads
├── @human and @agent mentions
├── task cards linked into threads
└── artifacts and approvals
          │
          ▼
Ironclaw host
├── workspace event store
├── farm task ledger
├── agent registry and capability router
├── MCP credential broker
└── VM dispatcher
```

An agent mention should not grant access. The host resolves the mentioned agent, checks the sender's
workspace identity and A2A permissions, then creates or resumes a task. Agent responses are appended
to the originating thread as immutable events.

## Next implementation steps

1. Add SQLite-backed workspaces, channels, memberships, messages, and thread events.
2. Add authenticated REST and WebSocket workspace APIs.
3. Convert `@agent` mentions into capability-checked A2A tasks.
4. Mirror task transitions and artifacts into the originating thread.
5. Add approval cards for irreversible capabilities and MCP writes.
6. Add unread state, presence, search, and notification routing to Telegram.

This keeps chat, task execution, and authorization separate: chat records intent, the task ledger
records execution, and the capability router remains the only authority.

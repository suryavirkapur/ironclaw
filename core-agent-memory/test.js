'use strict'

const assert = require('node:assert/strict')
const fs = require('node:fs')
const os = require('node:os')
const path = require('node:path')
const { AgentMemory } = require('.')

const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'core-agent-memory-'))
const database = path.join(directory, 'agent.db')

try {
  const memory = new AgentMemory(database)
  const stored = memory.store(
    'The backend uses idempotency keys for payment retries.',
    JSON.stringify({ type: 'architecture', agent_id: 'backend-engineer' }),
    0.92,
    true,
  )
  assert.equal(stored.deduplicated, false)
  assert.ok(stored.id)

  const results = memory.search({ text: 'payment retries', textOnly: true, limit: 5 })
  assert.equal(results.length, 1)
  assert.match(results[0].content, /idempotency keys/)
  assert.equal(memory.stats().count, 1)
  console.log('core-agent-memory N-API smoke test passed')
} finally {
  fs.rmSync(directory, { recursive: true, force: true })
}

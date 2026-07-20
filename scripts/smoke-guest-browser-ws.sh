#!/usr/bin/env bash
set -euo pipefail

# Smoke test for browser automation in local guest mode.
# Requires: agent-browser installed and `agent-browser install` run.
#
# Usage:
#   ./smoke-guest-browser-ws.sh              # normal run
#   AGENT_BROWSER_SKIP=1 ./smoke-guest-browser-ws.sh  # skip even if deps present

ADDR="${ADDR:-127.0.0.1:9938}"
WS_URL="${WS_URL:-ws://${ADDR}/ws}"

SKIP_REASON=""

if [[ "${AGENT_BROWSER_SKIP:-}" == "1" ]]; then
  SKIP_REASON="AGENT_BROWSER_SKIP=1"
elif ! command -v agent-browser >/dev/null 2>&1; then
  SKIP_REASON="agent-browser not found"
fi

if [[ -n "${SKIP_REASON}" ]]; then
  echo "SKIP: ${SKIP_REASON}"
  echo "To run this test: install agent-browser and run 'agent-browser install' first."
  exit 0
fi

cleanup() {
  if [[ -n "${IRONCLAWD_PID:-}" ]]; then
    kill "${IRONCLAWD_PID}" 2>/dev/null || true
    wait "${IRONCLAWD_PID}" 2>/dev/null || true
  fi
}
trap cleanup EXIT

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${ROOT_DIR}"

cargo build -q -p ironclawd

./target/debug/ironclawd >/tmp/ironclawd.local.log 2>&1 &
IRONCLAWD_PID=$!

sleep 0.3

# Send a browser_action tool call via WebSocket and capture the result.
# Uses the raw websocket toolcall protocol.
node - <<'EOF'
const url = process.argv[2] || "ws://127.0.0.1:9938/ws";
const toolName = "browser_action";
const toolInput = JSON.stringify({
  action: "navigate",
  url: "https://example.com",
});

const ws = new WebSocket(url);
let timer;

function sendMsg(payload) {
  ws.send(JSON.stringify({
    user_id: "smoke-test",
    session_id: "smoke-test-session",
    msg_id: Date.now(),
    timestamp_ms: Date.now(),
    cap_token: "",
    payload,
  }));
}

ws.addEventListener("open", () => {
  timer = setTimeout(() => {
    console.error("timeout waiting for tool response");
    ws.close();
    process.exit(1);
  }, 30000);

  sendMsg({
    ToolCallRequest: {
      call_id: "smoke-call-1",
      tool: toolName,
      input: toolInput,
    },
  });
  console.error("[browser-test] sent ToolCallRequest tool=" + toolName);
});

ws.addEventListener("message", (ev) => {
  try {
    const msg = JSON.parse(ev.data.toString());
    const resp = msg?.payload?.ToolCallResponse;
    if (resp && resp.call_id === "smoke-call-1") {
      clearTimeout(timer);
      if (resp.ok) {
        console.error("[browser-test] tool response OK, output starts with: " + resp.output.substring(0, 100));
        console.log("PASS: browser tool executed successfully");
        ws.close();
        process.exit(0);
      } else {
        console.error("[browser-test] tool FAILED: " + resp.output);
        ws.close();
        process.exit(1);
      }
    }
  } catch (e) {
    // ignore non-JSON or unrelated messages
  }
});

ws.addEventListener("error", (e) => {
  clearTimeout(timer);
  console.error("ws error", e.message || e);
  process.exit(1);
});
EOF

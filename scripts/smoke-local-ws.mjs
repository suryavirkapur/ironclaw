// Minimal smoke test: starts a websocket client and expects a StreamDelta reply.
// Usage: node scripts/smoke-local-ws.mjs ws://127.0.0.1:9938/ws

const url = process.argv[2] || "ws://127.0.0.1:9938/ws";

const ws = new WebSocket(url);

const timeoutMs = Number.parseInt(process.env.WS_TIMEOUT_MS || "5000", 10);
const timer = setTimeout(() => {
  console.error(`timeout after ${timeoutMs}ms`);
  process.exit(1);
}, timeoutMs);

ws.addEventListener("open", () => {
  ws.send("hello");
});

ws.addEventListener("message", (ev) => {
  try {
    const msg = JSON.parse(ev.data.toString());
    if (msg?.payload?.type === "StreamDelta") {
      clearTimeout(timer);
      console.log("OK", msg.payload.data);
      ws.close();
      process.exit(0);
    }
  } catch {
    // ignore
  }
});

ws.addEventListener("error", (e) => {
  clearTimeout(timer);
  console.error("ws error", e);
  process.exit(1);
});

#!/usr/bin/env node
const url = process.argv[2] || "ws://127.0.0.1:9938/ws";
const marker = process.argv[3];

if (!marker) {
  console.error("missing marker argument");
  process.exit(2);
}

const timeoutMs = Number.parseInt(process.env.WS_TIMEOUT_MS || "15000", 10);
const ws = new WebSocket(url);

const timer = setTimeout(() => {
  console.error(`timeout after ${timeoutMs}ms waiting for streamdelta`);
  process.exit(1);
}, timeoutMs);

ws.addEventListener("open", () => {
  ws.send(marker);
});

ws.addEventListener("message", (ev) => {
  try {
    const msg = JSON.parse(ev.data.toString());
    const delta = msg?.payload?.StreamDelta || msg?.payload?.streamDelta;
    if (!delta || typeof delta.delta !== "string") {
      return;
    }

    const text = delta.delta;
    if (text.includes(marker) && (text.startsWith("stub:") || text.startsWith("guest:"))) {
      clearTimeout(timer);
      console.log(`ok guest websocket response: ${text}`);
      ws.close();
      process.exit(0);
    }
  } catch {
    // ignore non-json messages
  }
});

ws.addEventListener("error", (err) => {
  clearTimeout(timer);
  console.error(`ws error: ${err?.message || "unknown"}`);
  process.exit(1);
});

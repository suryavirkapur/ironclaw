const statusEl = document.getElementById("status");
const connectBtn = document.getElementById("connect");
const sendBtn = document.getElementById("send");
const input = document.getElementById("input");
const log = document.getElementById("log");

let socket;

function setStatus(text) {
  statusEl.textContent = text;
}

function appendLine(text) {
  const line = document.createElement("div");
  line.textContent = text;
  log.appendChild(line);
}

function connect() {
  if (socket) {
    socket.close();
  }
  socket = new WebSocket(`ws://${location.host}/ws`);
  socket.addEventListener("open", () => setStatus("online"));
  socket.addEventListener("close", () => setStatus("offline"));
  socket.addEventListener("message", (event) => {
    appendLine(event.data);
  });
}

connectBtn.addEventListener("click", connect);

sendBtn.addEventListener("click", () => {
  if (!socket || socket.readyState !== WebSocket.OPEN) {
    appendLine("socket offline");
    return;
  }
  const value = input.value.trim();
  if (!value) {
    return;
  }
  socket.send(value);
  input.value = "";
});

const SPRITES = {
  bars: '<rect x="8" y="6" width="5.5" height="20" rx="2.5"/><rect x="18.5" y="6" width="5.5" height="20" rx="2.5"/>',
  peak: '<polygon points="16,5 28,26 4,26"/>',
  arch: '<path d="M7 25 V15.5 A9 9 0 0 1 25 15.5 V25" fill="none" stroke="currentColor" stroke-width="4" stroke-linecap="round"/>',
  orb: '<circle cx="16" cy="16" r="10"/><circle cx="16" cy="16" r="4" fill="#fff"/>',
  spark: '<polygon points="16,3 19,13 29,16 19,19 16,29 13,19 3,16 13,13"/>',
  hex: '<polygon points="16,4 27,10.5 27,21.5 16,28 5,21.5 5,10.5"/>',
  petal: '<circle cx="16" cy="10" r="6"/><circle cx="22" cy="18" r="6"/><circle cx="10" cy="18" r="6"/><circle cx="16" cy="16" r="3.5" fill="#fff"/>',
  bolt: '<polygon points="18,4 8,18 15,18 13,28 24,13 17,13"/>',
};

const SPRITE_ORDER = ["bars", "peak", "arch", "orb", "spark", "hex", "petal", "bolt"];
const COLOR_ORDER = ["#0F8A7A", "#6B5CE7", "#E05A67", "#D98A1C", "#2F6FED", "#3B8C4A", "#C44B8A", "#334155"];

const state = {
  agents: [],
  channels: [],
  tasks: [],
  view: "tasks",
  selectedTaskId: null,
  selectedAgentId: null,
  selectedChannelId: null,
  channelRecipientId: null,
  search: "",
  draftAgent: { sprite: "arch", color: "#6B5CE7" },
  chat: null,
  pendingFile: null,
  threads: loadThreads(),
  accessToken: loadAccessToken(),
};

const elements = {
  healthDot: document.getElementById("health-dot"),
  connectionLabel: document.getElementById("connection-label"),
  agentList: document.getElementById("agent-list"),
  agentCount: document.getElementById("agent-count"),
  channelList: document.getElementById("channel-list"),
  content: document.getElementById("content"),
  viewPrefix: document.getElementById("view-prefix"),
  viewTitle: document.getElementById("view-title"),
  viewDescription: document.getElementById("view-description"),
  viewSprite: document.getElementById("view-sprite"),
  channelMembers: document.getElementById("channel-members"),
  detailTitle: document.getElementById("detail-title"),
  detailContent: document.getElementById("detail-content"),
  newTaskButton: document.getElementById("new-task-button"),
  askAgentButton: document.getElementById("ask-agent-button"),
  addMembersButton: document.getElementById("add-members-button"),
  taskDialog: document.getElementById("task-dialog"),
  taskForm: document.getElementById("task-form"),
  requester: document.getElementById("requester"),
  capability: document.getElementById("capability"),
  taskRequest: document.getElementById("task-request"),
  formError: document.getElementById("form-error"),
  a2aDialog: document.getElementById("a2a-dialog"),
  a2aForm: document.getElementById("a2a-form"),
  a2aCapability: document.getElementById("a2a-capability"),
  a2aQuestion: document.getElementById("a2a-question"),
  a2aError: document.getElementById("a2a-error"),
  a2aRouteNote: document.getElementById("a2a-route-note"),
  agentDialog: document.getElementById("agent-dialog"),
  agentForm: document.getElementById("agent-form"),
  agentName: document.getElementById("agent-name"),
  agentRole: document.getElementById("agent-role"),
  agentError: document.getElementById("agent-error"),
  channelDialog: document.getElementById("channel-dialog"),
  channelForm: document.getElementById("channel-form"),
  channelName: document.getElementById("channel-name"),
  channelTopic: document.getElementById("channel-topic"),
  channelError: document.getElementById("channel-error"),
  membersDialog: document.getElementById("members-dialog"),
  membersForm: document.getElementById("members-form"),
  membersError: document.getElementById("members-error"),
  membersNote: document.getElementById("members-note"),
};

const terminalStates = new Set(["completed", "failed", "canceled", "rejected"]);
const MAX_UPLOAD_BYTES = 8 * 1024 * 1024;

async function api(path, options = {}) {
  const authorization = state.accessToken ? { authorization: `Bearer ${state.accessToken}` } : {};
  const response = await fetch(path, {
    headers: { "content-type": "application/json", ...authorization, ...(options.headers || {}) },
    ...options,
  });
  const body = await response.json().catch(() => ({}));
  if (!response.ok) throw new Error(body.error || `${response.status} ${response.statusText}`);
  return body;
}

function loadAccessToken() {
  return sessionStorage.getItem("ironclaw.controlPlaneToken") || "";
}

function promptForAccessToken() {
  const supplied = window.prompt("Enter your Ironclaw control-plane access token");
  if (!supplied) return false;
  state.accessToken = supplied.trim();
  sessionStorage.setItem("ironclaw.controlPlaneToken", state.accessToken);
  return Boolean(state.accessToken);
}

function node(tag, className, text) {
  const element = document.createElement(tag);
  if (className) element.className = className;
  if (text !== undefined) element.textContent = text;
  return element;
}

function hash32(value) {
  return Array.from(String(value)).reduce((acc, char) => ((acc * 31) + char.charCodeAt(0)) >>> 0, 0);
}

function defaultAppearance(id) {
  const hash = hash32(id);
  return {
    sprite: SPRITE_ORDER[hash % SPRITE_ORDER.length],
    color: COLOR_ORDER[Math.floor(hash / SPRITE_ORDER.length) % COLOR_ORDER.length],
  };
}

function appearanceOf(agent) {
  const fallback = defaultAppearance(agent.id || agent.name || "agent");
  return {
    sprite: SPRITES[agent.sprite] ? agent.sprite : fallback.sprite,
    color: /^#[0-9A-Fa-f]{6}$/.test(agent.color || "") ? agent.color : fallback.color,
  };
}

function spriteEl(sprite, color, extraClass = "") {
  const wrap = node("span", `sprite ${extraClass}`.trim());
  wrap.style.color = color;
  wrap.innerHTML = `<svg viewBox="0 0 32 32" aria-hidden="true">${SPRITES[sprite] || SPRITES.orb}</svg>`;
  return wrap;
}

function initials(name) {
  return name.split(/\s+/).map((part) => part[0]).join("").slice(0, 2).toUpperCase();
}

function taskState(task) {
  return String(task.state || "unknown").toLowerCase();
}

function activeCount(agentId) {
  return state.tasks.filter((task) => task.assignee === agentId && !terminalStates.has(taskState(task))).length;
}

function loadThreads() {
  try {
    return JSON.parse(localStorage.getItem("ironclaw.workspace.threads") || "{}") || {};
  } catch (_) {
    return {};
  }
}

function saveThreads() {
  const durable = {};
  for (const [agentId, messages] of Object.entries(state.threads)) {
    durable[agentId] = messages.slice(-100).map(({ previewUrl, ...message }) => message);
  }
  localStorage.setItem("ironclaw.workspace.threads", JSON.stringify(durable));
}

function threadKey() {
  if (state.view === "channel" && state.selectedChannelId) return `channel:${state.selectedChannelId}`;
  return state.selectedAgentId;
}

function thread(key = threadKey()) {
  if (!key) return [];
  if (!state.threads[key]) state.threads[key] = [];
  return state.threads[key];
}

function addMessage(key, message) {
  thread(key).push({ id: crypto.randomUUID(), at: Date.now(), ...message });
  saveThreads();
  if (threadKey() === key) {
    if (state.view === "chat") renderChat();
    if (state.view === "channel") renderChannel();
  }
}

function agentById(agentId) {
  return state.agents.find((agent) => agent.id === agentId);
}

function channelById(channelId) {
  return state.channels.find((channel) => channel.id === channelId);
}

function matchesSearch(text) {
  return !state.search || String(text).toLowerCase().includes(state.search);
}

function renderSidebar() {
  elements.channelList.replaceChildren();
  for (const channel of state.channels) {
    if (!matchesSearch(`${channel.name} ${channel.topic}`)) continue;
    const row = node("button", "channel room");
    row.type = "button";
    if (state.view === "channel" && state.selectedChannelId === channel.id) row.classList.add("active");
    row.addEventListener("click", () => openChannel(channel.id));
    const lead = agentById(channel.member_ids?.[0] || "") || { sprite: "bars", color: "#334155" };
    const look = appearanceOf(lead);
    row.append(spriteEl(look.sprite, look.color, "tiny"));
    const identity = node("span", "channel-identity");
    identity.append(node("strong", "", channel.name), node("small", "", channel.topic || `${channel.member_ids?.length || 0} agents`));
    row.append(identity);
    elements.channelList.append(row);
  }

  elements.agentList.replaceChildren();
  elements.agentCount.textContent = String(state.agents.length);
  for (const agent of state.agents) {
    if (!matchesSearch(`${agent.name} ${agent.role}`)) continue;
    const row = node("button", "agent-row");
    row.type = "button";
    if (state.selectedAgentId === agent.id && state.view === "chat") row.classList.add("selected");
    row.addEventListener("click", () => openAgentChat(agent.id));
    const look = appearanceOf(agent);
    row.append(spriteEl(look.sprite, look.color));
    const identity = node("span", "agent-identity");
    identity.append(node("strong", "", agent.name), node("small", "", agent.role));
    row.append(identity, node("span", activeCount(agent.id) ? "presence busy" : "presence"));
    elements.agentList.append(row);
  }
}

function renderMetrics() {
  const active = state.tasks.filter((task) => !terminalStates.has(taskState(task))).length;
  const completed = state.tasks.filter((task) => taskState(task) === "completed").length;
  const failed = state.tasks.filter((task) => ["failed", "rejected"].includes(taskState(task))).length;
  document.getElementById("metric-active").textContent = String(active);
  document.getElementById("metric-completed").textContent = String(completed);
  document.getElementById("metric-failed").textContent = String(failed);
  document.getElementById("metric-agents").textContent = String(state.agents.length);
}

function setHeader(prefix, title, description, options = {}) {
  elements.viewPrefix.textContent = prefix;
  elements.viewTitle.textContent = title;
  elements.viewDescription.textContent = description;
  elements.newTaskButton.classList.toggle("hidden", Boolean(options.hideTask));
  elements.askAgentButton.classList.toggle("hidden", !options.chatActions);
  elements.addMembersButton.classList.toggle("hidden", !options.channelActions);
  elements.viewSprite.classList.toggle("hidden", !options.sprite);
  if (options.sprite) {
    elements.viewSprite.replaceWith(spriteEl(options.sprite, options.color, "header-sprite"));
    elements.viewSprite = document.querySelector(".header-identity .sprite");
    elements.viewSprite.id = "view-sprite";
  }
  renderMemberRow(options.members || []);
}

function renderMemberRow(memberIds) {
  elements.channelMembers.replaceChildren();
  for (const agentId of memberIds) {
    const agent = agentById(agentId);
    if (!agent) continue;
    const look = appearanceOf(agent);
    const chip = spriteEl(look.sprite, look.color, "tiny");
    chip.title = agent.name;
    chip.style.cursor = "pointer";
    if (state.channelRecipientId === agent.id) chip.style.outline = "2px solid #111";
    chip.addEventListener("click", () => {
      state.channelRecipientId = agent.id;
      if (state.view === "channel") renderChannel();
      else openAgentChat(agent.id);
    });
    elements.channelMembers.append(chip);
  }
}

function renderTasks() {
  setHeader("#", "delivery", "Live A2A work across the engineering team");
  elements.content.className = "content";
  elements.content.replaceChildren();
  const tasks = [...state.tasks].sort((a, b) => b.updated_at_ms - a.updated_at_ms);
  if (!tasks.length) {
    elements.content.append(node("div", "empty-state", "No work yet. Create the first team task."));
    return;
  }
  for (const task of tasks) {
    const card = node("button", "task-card");
    card.type = "button";
    if (task.id === state.selectedTaskId) card.classList.add("selected");
    card.addEventListener("click", () => inspectTask(task));
    const top = node("div", "task-card-top");
    top.append(node("span", `state-pill ${taskState(task)}`, taskState(task).replace("_", " ")), node("time", "", new Date(task.updated_at_ms).toLocaleString()));
    card.append(top, node("h3", "", task.skill.replaceAll("_", " ")), node("p", "task-summary", task.input?.request || task.input?.question || JSON.stringify(task.input || {})), node("p", "task-route", `${task.requester}  →  ${task.assignee}`));
    elements.content.append(card);
  }
}

function renderArchitecture() {
  setHeader("#", "architecture", "One shared control plane, private agent VMs");
  elements.content.className = "content";
  elements.content.replaceChildren();
  const diagram = node("div", "architecture-card");
  diagram.append(node("div", "architecture-node human", "Workspace"), node("div", "architecture-arrow", "↓"));
  const host = node("div", "architecture-node host", "Ironclaw host");
  host.append(node("small", "", "registry · task ledger · MCP broker · VM manager"));
  diagram.append(host, node("div", "architecture-arrow", "↓ authenticated A2A tasks"));
  const vmRow = node("div", "vm-row");
  for (const agent of state.agents) {
    const vm = node("div", "architecture-node vm");
    const look = appearanceOf(agent);
    vm.append(spriteEl(look.sprite, look.color), node("strong", "", agent.name), node("small", "", `${agent.role} · private VM`));
    vmRow.append(vm);
  }
  diagram.append(vmRow);
  elements.content.append(diagram);
}

function renderChat() {
  const agent = agentById(state.selectedAgentId);
  if (!agent) {
    setHeader("●", "conversations", "Choose an agent from the sidebar", { hideTask: true });
    elements.content.className = "content";
    elements.content.replaceChildren(node("div", "empty-state", "Select an agent from the sidebar to start a private conversation."));
    return;
  }
  const look = appearanceOf(agent);
  const connected = state.chat?.agentId === agent.id && state.chat.ready;
  setHeader("", agent.name, `${agent.role} · ${connected ? "MicroVM connected" : "connecting to private MicroVM"}`, {
    hideTask: true,
    chatActions: true,
    sprite: look.sprite,
    color: look.color,
  });
  renderConversation(agent, thread(agent.id), `Talk with ${agent.name}`, `Message ${agent.name}…`, connected);
  inspectAgent(agent);
}

function renderChannel() {
  const channel = channelById(state.selectedChannelId);
  if (!channel) {
    setHeader("#", "channels", "Create a channel to group agents", { hideTask: true });
    elements.content.className = "content";
    elements.content.replaceChildren(node("div", "empty-state", "Create a channel, then add agents to it."));
    return;
  }
  const members = (channel.member_ids || []).map(agentById).filter(Boolean);
  if (!state.channelRecipientId || !members.some((agent) => agent.id === state.channelRecipientId)) {
    state.channelRecipientId = members[0]?.id || null;
  }
  const recipient = agentById(state.channelRecipientId);
  const connected = Boolean(recipient) && state.chat?.agentId === recipient.id && state.chat.ready;
  setHeader("#", channel.name, channel.topic || (recipient ? `Asking ${recipient.name}` : "Add agents to start talking"), {
    hideTask: true,
    channelActions: true,
    members: channel.member_ids || [],
  });
  const names = members.map((agent) => agent.name);
  const intro = names.length
    ? `Messages from ${names.slice(0, -1).join(", ")}${names.length > 1 ? " and " : ""}${names.at(-1)}.`
    : "Add agents to this channel to involve them in the room.";
  renderConversation(recipient, thread(`channel:${channel.id}`), channel.name, `Message ${channel.name}`, connected, intro);
}

function renderConversation(agent, messages, title, placeholder, connected, intro) {
  elements.content.className = "content chat-content";
  elements.content.replaceChildren();
  const timeline = node("div", "chat-timeline");
  timeline.id = "chat-timeline";
  if (intro) timeline.append(node("p", "attribution", intro));
  if (!messages.length) {
    const welcome = node("div", "chat-welcome");
    if (agent) {
      const look = appearanceOf(agent);
      welcome.append(spriteEl(look.sprite, look.color, "large"));
    }
    welcome.append(node("h3", "", title), node("p", "", "This conversation stays in the workspace. Agent replies use a private MicroVM when that teammate is available."));
    timeline.append(welcome);
  }
  for (const message of messages) timeline.append(renderMessage(message, agentById(message.fromAgent) || agent));
  const pending = renderPendingFile();
  const composer = node("form", "chat-composer");
  composer.id = "chat-form";
  const fileInput = node("input", "visually-hidden");
  fileInput.type = "file";
  fileInput.id = "chat-file";
  fileInput.accept = "image/*,.pdf,.txt,.md,.csv,.json,.doc,.docx,.xls,.xlsx,.ppt,.pptx";
  fileInput.addEventListener("change", () => selectFile(fileInput.files?.[0]));
  const attach = node("button", "attach-button", "+");
  attach.type = "button";
  attach.title = "Attach image or document";
  attach.disabled = !connected;
  attach.addEventListener("click", () => fileInput.click());
  const input = node("textarea", "chat-input");
  input.id = "chat-input";
  input.rows = 1;
  input.placeholder = placeholder;
  input.disabled = !connected;
  input.addEventListener("keydown", (event) => {
    if (event.key === "Enter" && !event.shiftKey) {
      event.preventDefault();
      composer.requestSubmit();
    }
  });
  const send = node("button", "send-button", connected ? "Send" : "Starting…");
  send.type = "submit";
  send.disabled = !connected;
  composer.append(fileInput, attach, input, send);
  composer.addEventListener("submit", sendChatMessage);
  elements.content.append(timeline, pending, composer);
  elements.content.addEventListener("dragover", (event) => { event.preventDefault(); elements.content.classList.add("dragging"); }, { once: true });
  elements.content.addEventListener("drop", (event) => { event.preventDefault(); elements.content.classList.remove("dragging"); selectFile(event.dataTransfer?.files?.[0]); }, { once: true });
  requestAnimationFrame(() => { timeline.scrollTop = timeline.scrollHeight; });
}

function renderMessage(message, agent) {
  const item = node("article", `chat-message ${message.role || "agent"}`);
  const meta = node("div", "message-meta");
  const speaker = message.role === "user" ? "You" : message.role === "system" ? "Workspace" : (agent?.name || "Agent");
  meta.append(node("strong", "", speaker), node("time", "", new Date(message.at).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })));
  item.append(meta);
  if (message.text) item.append(node("div", "message-text", message.text));
  if (message.attachment) {
    const attachment = node("div", "message-attachment");
    if (message.previewUrl && message.attachment.mimeType.startsWith("image/")) {
      const image = document.createElement("img");
      image.src = message.previewUrl;
      image.alt = message.attachment.name;
      attachment.append(image);
    }
    attachment.append(node("strong", "", message.attachment.name), node("small", "", `${message.attachment.mimeType || "file"} · ${formatBytes(message.attachment.size)}`));
    item.append(attachment);
  }
  if (message.taskId) {
    const provenance = node("button", "provenance", `A2A · ${message.fromAgent} → ${message.toAgent} · ${message.taskId}`);
    provenance.type = "button";
    provenance.addEventListener("click", () => {
      state.view = "tasks";
      state.selectedTaskId = message.taskId;
      const task = state.tasks.find((candidate) => candidate.id === message.taskId);
      if (task) inspectTask(task); else render();
    });
    item.append(provenance);
  }
  return item;
}

function renderPendingFile() {
  const holder = node("div", `pending-file ${state.pendingFile ? "" : "hidden"}`);
  holder.id = "pending-file";
  if (!state.pendingFile) return holder;
  if (state.pendingFile.type.startsWith("image/")) {
    const image = document.createElement("img");
    image.src = state.pendingFile.previewUrl;
    image.alt = "Attachment preview";
    holder.append(image);
  } else {
    holder.append(node("span", "file-icon", "DOC"));
  }
  const copy = node("span", "pending-file-copy");
  copy.append(node("strong", "", state.pendingFile.name), node("small", "", `${state.pendingFile.type || "application/octet-stream"} · ${formatBytes(state.pendingFile.size)}`));
  const remove = node("button", "icon-button", "×");
  remove.type = "button";
  remove.addEventListener("click", clearPendingFile);
  holder.append(copy, remove);
  return holder;
}

function selectFile(file) {
  if (!file) return;
  if (file.size > MAX_UPLOAD_BYTES) {
    addMessage(threadKey(), { role: "system", text: "Attachment rejected: files must be 8 MB or smaller." });
    return;
  }
  clearPendingFile(false);
  state.pendingFile = { file, name: file.name, type: file.type || "application/octet-stream", size: file.size, previewUrl: URL.createObjectURL(file) };
  if (state.view === "channel") renderChannel();
  else renderChat();
}

function clearPendingFile(renderNow = true) {
  if (state.pendingFile?.previewUrl) URL.revokeObjectURL(state.pendingFile.previewUrl);
  state.pendingFile = null;
  if (renderNow && state.view === "chat") renderChat();
  if (renderNow && state.view === "channel") renderChannel();
}

function formatBytes(bytes) {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
}

function render() {
  document.querySelector(".workspace-shell").classList.toggle("chat-mode", state.view === "chat" || state.view === "channel");
  document.querySelectorAll(".channel[data-view]").forEach((button) => button.classList.toggle("active", button.dataset.view === state.view));
  renderSidebar();
  renderMetrics();
  if (state.view === "chat") renderChat();
  else if (state.view === "channel") renderChannel();
  else if (state.view === "architecture") renderArchitecture();
  else renderTasks();
}

function detailRow(label, value) {
  const row = node("div", "detail-row");
  row.append(node("span", "", label), node("strong", "", value));
  return row;
}

function inspectTask(task) {
  state.selectedTaskId = task.id;
  elements.detailTitle.textContent = task.skill.replaceAll("_", " ");
  elements.detailContent.replaceChildren(detailRow("State", taskState(task)), detailRow("Requester", task.requester), detailRow("Assignee", task.assignee), detailRow("Task", task.id), detailRow("Context", task.context_id), detailRow("Depth", String(task.delegation_depth)));
  elements.detailContent.append(node("h3", "", "Input"), node("pre", "json-block", JSON.stringify(task.input, null, 2)), node("h3", "", "Output"), node("pre", "json-block", JSON.stringify(task.output, null, 2)));
  if (state.view === "tasks") renderTasks();
}

async function inspectAgent(agent) {
  const look = appearanceOf(agent);
  elements.detailTitle.textContent = agent.name;
  elements.detailContent.replaceChildren(
    detailRow("Role", agent.role),
    detailRow("Agent ID", agent.id),
    detailRow("Sprite", look.sprite),
    detailRow("Color", look.color),
    detailRow("Memory", `${agent.memory_engine || "core-agent-memory"} · Private VM`),
    detailRow("Active work", String(activeCount(agent.id))),
    detailRow("Wasm tools", String(agent.wasm_tools)),
    detailRow("MCP servers", String(agent.mcp_servers)),
  );
  try {
    const capabilities = await api(`/api/farm/agents/${encodeURIComponent(agent.id)}/capabilities`);
    elements.detailContent.append(node("h3", "", "Authorized capabilities"));
    for (const capability of capabilities) elements.detailContent.append(node("code", "capability", capability.uri));
  } catch (error) {
    elements.detailContent.append(node("p", "error", error.message));
  }
}

async function openAgentChat(agentId) {
  if (state.selectedAgentId !== agentId) clearPendingFile(false);
  state.selectedAgentId = agentId;
  state.selectedChannelId = null;
  state.view = "chat";
  render();
  await connectChat(agentId);
}

async function openChannel(channelId) {
  state.selectedChannelId = channelId;
  state.view = "channel";
  const channel = channelById(channelId);
  const recipient = channel?.member_ids?.find((id) => agentById(id)) || null;
  state.channelRecipientId = recipient;
  render();
  if (recipient) await connectChat(recipient);
}

function closeChat() {
  if (state.chat?.socket) state.chat.socket.close(1000, "switching agent");
  state.chat = null;
}

async function connectChat(agentId) {
  closeChat();
  const protocol = location.protocol === "https:" ? "wss:" : "ws:";
  const sessionId = `workspace-${agentId}`;
  const auth = await api("/api/auth/ws-ticket", { method: "POST", body: JSON.stringify({ agent_id: agentId }) });
  const socket = new WebSocket(`${protocol}//${location.host}/ws?user_id=${encodeURIComponent(agentId)}&session_id=${encodeURIComponent(sessionId)}&ticket=${encodeURIComponent(auth.ticket)}`);
  const chat = { agentId, socket, ready: false, response: "" };
  state.chat = chat;
  socket.binaryType = "arraybuffer";
  socket.addEventListener("message", async (event) => {
    if (typeof event.data !== "string") return;
    let envelope;
    try { envelope = JSON.parse(event.data); } catch (_) {
      addMessage(threadKey(), { role: "system", text: event.data, fromAgent: agentId });
      return;
    }
    const [kind, payload] = payloadEntry(envelope.payload);
    if (kind === "AuthChallenge") {
      const token = payload.cap_token;
      chat.capToken = token;
      socket.send(JSON.stringify({ user_id: agentId, session_id: sessionId, msg_id: envelope.msg_id || 0, timestamp_ms: Date.now(), cap_token: token, payload: { AuthAck: { cap_token: token } } }));
      chat.ready = true;
      if (state.selectedAgentId === agentId || state.channelRecipientId === agentId) {
        if (state.view === "chat") renderChat();
        if (state.view === "channel") renderChannel();
      }
    } else if (kind === "StreamDelta") {
      chat.response += payload.delta || "";
      if (payload.done) {
        addMessage(threadKey(), { role: "agent", text: chat.response, fromAgent: agentId });
        chat.response = "";
      }
    } else if (kind === "Artifact") {
      addMessage(threadKey(), { role: "agent", text: payload.caption || "Created an artifact.", fromAgent: agentId, attachment: { name: payload.filename, mimeType: payload.mime_type, size: byteData(payload.data).length }, previewUrl: artifactUrl(payload) });
    } else if (kind === "ToolCallResponse") {
      addMessage(threadKey(), { role: "agent", text: payload.output || (payload.ok ? "Tool completed." : "Tool failed."), fromAgent: agentId });
    }
  });
  socket.addEventListener("close", () => {
    if (state.chat === chat) {
      chat.ready = false;
      if (state.view === "chat") renderChat();
      if (state.view === "channel") renderChannel();
    }
  });
  socket.addEventListener("error", () => addMessage(threadKey(), { role: "system", text: "Could not connect to this agent’s MicroVM. It may already be in use by Telegram or an A2A task." }));
}

function payloadEntry(payload) {
  if (!payload || typeof payload !== "object") return ["", {}];
  const entry = Object.entries(payload)[0] || ["", {}];
  const aliases = { auth_challenge: "AuthChallenge", stream_delta: "StreamDelta", artifact: "Artifact", tool_call_response: "ToolCallResponse" };
  return [aliases[entry[0]] || entry[0], entry[1] || {}];
}

async function sendChatMessage(event) {
  event.preventDefault();
  const input = document.getElementById("chat-input");
  const prompt = input?.value.trim() || "";
  if (!state.chat?.ready || (!prompt && !state.pendingFile)) return;
  const agentId = state.chat.agentId;
  const key = threadKey();
  if (state.pendingFile) {
    const pending = state.pendingFile;
    const bytes = new Uint8Array(await pending.file.arrayBuffer());
    addMessage(key, { role: "user", text: prompt || "Analyze this file.", attachment: { name: pending.name, mimeType: pending.type, size: pending.size }, previewUrl: pending.previewUrl });
    state.chat.socket.send(encodeUploadEnvelope(agentId, `workspace-${agentId}`, state.chat.capToken, pending, bytes, prompt));
    state.pendingFile = null;
  } else {
    addMessage(key, { role: "user", text: prompt });
    state.chat.socket.send(prompt);
  }
  if (input) input.value = "";
  if (state.view === "channel") renderChannel();
  else renderChat();
}

function encodeUploadEnvelope(userId, sessionId, token, file, data, prompt) {
  const uploaded = concat(fieldString(1, file.name), fieldString(2, file.type), fieldBytes(3, data), fieldString(4, prompt || "Analyze this file and report the important findings."));
  return concat(fieldString(1, userId), fieldString(2, sessionId), fieldVarint(3, Date.now()), fieldVarint(4, Date.now()), fieldString(5, token), fieldBytes(21, uploaded)).buffer;
}

function varint(value) {
  let current = BigInt(value);
  const bytes = [];
  while (current > 127n) { bytes.push(Number((current & 127n) | 128n)); current >>= 7n; }
  bytes.push(Number(current));
  return new Uint8Array(bytes);
}

function fieldVarint(number, value) { return concat(varint(number << 3), varint(value)); }
function fieldString(number, value) { return fieldBytes(number, new TextEncoder().encode(value || "")); }
function fieldBytes(number, value) { const bytes = value instanceof Uint8Array ? value : new Uint8Array(value); return concat(varint((number << 3) | 2), varint(bytes.length), bytes); }
function concat(...parts) { const length = parts.reduce((sum, part) => sum + part.length, 0); const output = new Uint8Array(length); let offset = 0; for (const part of parts) { output.set(part, offset); offset += part.length; } return output; }

function byteData(value) {
  if (Array.isArray(value)) return new Uint8Array(value);
  if (typeof value === "string") return Uint8Array.from(atob(value), (char) => char.charCodeAt(0));
  return new Uint8Array();
}

function artifactUrl(artifact) {
  const bytes = byteData(artifact.data);
  return URL.createObjectURL(new Blob([bytes], { type: artifact.mime_type || "application/octet-stream" }));
}

async function loadCapabilities() {
  const requester = elements.requester.value;
  elements.capability.replaceChildren();
  if (!requester) return;
  const capabilities = await api(`/api/farm/agents/${encodeURIComponent(requester)}/capabilities`);
  for (const capability of capabilities.filter((item) => item.kind === "a2a_skill")) {
    const option = document.createElement("option");
    option.value = capability.uri;
    option.textContent = `${capability.uri.replace("agent://", "")} — ${capability.description}`;
    elements.capability.append(option);
  }
}

async function openTaskDialog() {
  elements.formError.textContent = "";
  elements.requester.replaceChildren();
  for (const agent of state.agents) {
    const option = document.createElement("option");
    option.value = agent.id;
    option.textContent = `${agent.name} — ${agent.role}`;
    elements.requester.append(option);
  }
  await loadCapabilities();
  elements.taskDialog.showModal();
}

async function submitTask(event) {
  event.preventDefault();
  elements.formError.textContent = "";
  try {
    const task = await createA2aTask(elements.requester.value, elements.capability.value, { request: elements.taskRequest.value, source: "workspace" });
    elements.taskRequest.value = "";
    elements.taskDialog.close();
    state.selectedTaskId = task.id;
    await refresh();
  } catch (error) { elements.formError.textContent = error.message; }
}

async function openA2aDialog() {
  const requester = agentById(state.selectedAgentId);
  if (!requester) return;
  elements.a2aError.textContent = "";
  elements.a2aCapability.replaceChildren();
  const capabilities = await api(`/api/farm/agents/${encodeURIComponent(requester.id)}/capabilities`);
  const assignments = capabilities.filter((capability) => capability.kind === "a2a_skill");
  for (const capability of assignments) {
    const option = document.createElement("option");
    option.value = capability.uri;
    const target = agentById(capability.uri.split("/")[2]);
    option.textContent = `${target?.name || capability.uri} — ${capability.name.replaceAll("_", " ")}`;
    elements.a2aCapability.append(option);
  }
  elements.a2aRouteNote.textContent = assignments.length ? `${requester.name} can request information only through the authorized routes below. The source agent’s private memory is never directly exposed.` : `${requester.name} has no authorized outgoing A2A routes.`;
  elements.a2aQuestion.value = "";
  elements.a2aQuestion.disabled = !assignments.length;
  elements.a2aDialog.querySelector('button[type="submit"]').disabled = !assignments.length;
  elements.a2aDialog.showModal();
}

async function submitA2aQuestion(event) {
  event.preventDefault();
  const requester = agentById(state.selectedAgentId);
  const uri = elements.a2aCapability.value;
  try {
    const match = /^agent:\/\/([^/]+)\/(.+)$/.exec(uri);
    if (!match) throw new Error("Choose an authorized teammate route.");
    const target = agentById(match[1]);
    const question = elements.a2aQuestion.value.trim();
    addMessage(requester.id, { role: "user", text: `Ask ${target.name}: ${question}` });
    elements.a2aDialog.close();
    const task = await createA2aTask(requester.id, uri, { request: question, question, purpose: "authorized_memory_request", source: "workspace_conversation" });
    addMessage(requester.id, { role: "system", text: `A2A request sent to ${target.name}. Waiting for a durable response…`, taskId: task.id, fromAgent: requester.id, toAgent: target.id });
    const completed = await waitForTask(task.id);
    const response = completed.output?.text || completed.output?.error || JSON.stringify(completed.output ?? null, null, 2);
    addMessage(requester.id, { role: "agent", text: `${target.name} replied via A2A:\n\n${response}`, taskId: completed.id, fromAgent: requester.id, toAgent: target.id });
    await refresh();
  } catch (error) {
    if (/401|bearer token/i.test(error.message) && promptForAccessToken()) {
      window.setTimeout(refresh, 0);
      return;
    }
    elements.a2aError.textContent = error.message;
    addMessage(requester.id, { role: "system", text: `A2A request failed: ${error.message}` });
  }
}

async function createA2aTask(requester, uri, input) {
  const match = /^agent:\/\/([^/]+)\/(.+)$/.exec(uri);
  if (!match) throw new Error("Choose an available assignment.");
  return api("/api/farm/tasks", { method: "POST", body: JSON.stringify({ requester, assignee: match[1], skill: match[2], input }) });
}

async function waitForTask(taskId) {
  for (let attempt = 0; attempt < 120; attempt += 1) {
    const task = await api(`/api/farm/tasks/${encodeURIComponent(taskId)}`);
    if (terminalStates.has(taskState(task))) {
      if (taskState(task) !== "completed") throw new Error(task.output?.error || `Task ${taskState(task)}`);
      return task;
    }
    await new Promise((resolve) => setTimeout(resolve, 1000));
  }
  throw new Error("A2A request timed out.");
}

function paintSpritePicker() {
  const picker = document.getElementById("sprite-picker");
  picker.replaceChildren();
  for (const sprite of SPRITE_ORDER) {
    const button = node("button", `sprite-option${state.draftAgent.sprite === sprite ? " selected" : ""}`);
    button.type = "button";
    button.title = sprite;
    button.append(spriteEl(sprite, state.draftAgent.color));
    button.addEventListener("click", () => {
      state.draftAgent.sprite = sprite;
      paintSpritePicker();
      paintColorPicker();
    });
    picker.append(button);
  }
}

function paintColorPicker() {
  const picker = document.getElementById("color-picker");
  picker.replaceChildren();
  for (const color of COLOR_ORDER) {
    const button = node("button", `color-option${state.draftAgent.color === color ? " selected" : ""}`);
    button.type = "button";
    button.title = color;
    button.style.background = color;
    button.addEventListener("click", () => {
      state.draftAgent.color = color;
      paintSpritePicker();
      paintColorPicker();
    });
    picker.append(button);
  }
}

function paintAgentChecks(container, selected = []) {
  container.replaceChildren();
  if (!state.agents.length) {
    container.append(node("p", "empty-state", "Create an agent first."));
    return;
  }
  for (const agent of state.agents) {
    const look = appearanceOf(agent);
    const label = node("label", "check-row");
    const box = document.createElement("input");
    box.type = "checkbox";
    box.value = agent.id;
    box.checked = selected.includes(agent.id);
    label.append(spriteEl(look.sprite, look.color, "tiny"));
    const copy = node("span", "agent-identity");
    copy.append(node("strong", "", agent.name), node("small", "", agent.role));
    label.append(copy, box);
    container.append(label);
  }
}

function checkedIds(container) {
  return [...container.querySelectorAll("input[type=checkbox]:checked")].map((input) => input.value);
}

function openAgentDialog() {
  elements.agentError.textContent = "";
  elements.agentName.value = "";
  elements.agentRole.value = "";
  state.draftAgent = { sprite: SPRITE_ORDER[state.agents.length % SPRITE_ORDER.length], color: COLOR_ORDER[state.agents.length % COLOR_ORDER.length] };
  paintSpritePicker();
  paintColorPicker();
  elements.agentDialog.showModal();
}

async function submitAgent(event) {
  event.preventDefault();
  elements.agentError.textContent = "";
  try {
    const agent = await api("/api/farm/agents", {
      method: "POST",
      body: JSON.stringify({
        name: elements.agentName.value.trim(),
        role: elements.agentRole.value.trim(),
        sprite: state.draftAgent.sprite,
        color: state.draftAgent.color,
      }),
    });
    elements.agentDialog.close();
    await refresh();
    await openAgentChat(agent.id);
  } catch (error) {
    elements.agentError.textContent = error.message;
  }
}

function openChannelDialog() {
  elements.channelError.textContent = "";
  elements.channelName.value = "";
  elements.channelTopic.value = "";
  paintAgentChecks(document.getElementById("channel-agent-picker"));
  elements.channelDialog.showModal();
}

async function submitChannel(event) {
  event.preventDefault();
  elements.channelError.textContent = "";
  try {
    const channel = await api("/api/workspace/channels", {
      method: "POST",
      body: JSON.stringify({
        name: elements.channelName.value.trim(),
        topic: elements.channelTopic.value.trim(),
        member_ids: checkedIds(document.getElementById("channel-agent-picker")),
      }),
    });
    elements.channelDialog.close();
    await refresh();
    await openChannel(channel.id);
  } catch (error) {
    elements.channelError.textContent = error.message;
  }
}

function openMembersDialog() {
  const channel = channelById(state.selectedChannelId);
  if (!channel) return;
  elements.membersError.textContent = "";
  elements.membersNote.textContent = `Add teammates to ${channel.name}. Existing members stay on the channel.`;
  const remaining = state.agents.filter((agent) => !(channel.member_ids || []).includes(agent.id));
  const picker = document.getElementById("members-picker");
  if (!remaining.length) {
    picker.replaceChildren(node("p", "empty-state", "Every agent is already in this channel."));
  } else {
    paintAgentChecks(picker);
    picker.querySelectorAll("input[type=checkbox]").forEach((input) => {
      if ((channel.member_ids || []).includes(input.value)) input.closest("label").remove();
    });
  }
  elements.membersDialog.showModal();
}

async function submitMembers(event) {
  event.preventDefault();
  elements.membersError.textContent = "";
  const channel = channelById(state.selectedChannelId);
  const agentIds = checkedIds(document.getElementById("members-picker"));
  if (!channel || !agentIds.length) {
    elements.membersError.textContent = "Choose at least one agent.";
    return;
  }
  try {
    await api(`/api/workspace/channels/${encodeURIComponent(channel.id)}/members`, {
      method: "POST",
      body: JSON.stringify({ agent_ids: agentIds }),
    });
    elements.membersDialog.close();
    await refresh();
    await openChannel(channel.id);
  } catch (error) {
    elements.membersError.textContent = error.message;
  }
}

async function refresh() {
  try {
    const [health, agents, tasks, channels] = await Promise.all([
      api("/api/health"),
      api("/api/farm/agents"),
      api("/api/farm/tasks"),
      api("/api/workspace/channels").catch(() => []),
    ]);
    state.agents = agents;
    state.tasks = tasks;
    state.channels = channels;
    elements.healthDot.classList.add("online");
    elements.connectionLabel.textContent = `${health.status} · live`;
    renderSidebar();
    renderMetrics();
    if (state.view !== "chat" && state.view !== "channel") render();
    if (state.selectedTaskId) {
      const selected = state.tasks.find((task) => task.id === state.selectedTaskId);
      if (selected && state.view === "tasks") inspectTask(selected);
    }
  } catch (error) {
    if (/401|bearer token/i.test(error.message) && promptForAccessToken()) {
      window.setTimeout(refresh, 0);
      return;
    }
    if (/429|rate limit/i.test(error.message) && state.agents.length) {
      elements.connectionLabel.textContent = "rate limited";
      return;
    }
    elements.healthDot.classList.remove("online");
    elements.connectionLabel.textContent = "offline";
    if (!state.agents.length && state.view !== "chat" && state.view !== "channel") {
      elements.content.replaceChildren(node("div", "empty-state error", error.message));
    }
  }
}

document.querySelectorAll(".channel[data-view]").forEach((button) => button.addEventListener("click", () => {
  state.view = button.dataset.view;
  state.selectedChannelId = null;
  if (state.view !== "chat") closeChat();
  render();
}));
document.getElementById("sidebar-search").addEventListener("input", (event) => {
  state.search = event.target.value.trim().toLowerCase();
  renderSidebar();
});
document.getElementById("create-agent-button").addEventListener("click", openAgentDialog);
document.getElementById("create-channel-button").addEventListener("click", openChannelDialog);
elements.addMembersButton.addEventListener("click", openMembersDialog);
elements.newTaskButton.addEventListener("click", openTaskDialog);
elements.askAgentButton.addEventListener("click", () => openA2aDialog().catch((error) => addMessage(state.selectedAgentId, { role: "system", text: error.message })));
document.getElementById("close-dialog").addEventListener("click", () => elements.taskDialog.close());
document.getElementById("cancel-task").addEventListener("click", () => elements.taskDialog.close());
document.getElementById("close-a2a-dialog").addEventListener("click", () => elements.a2aDialog.close());
document.getElementById("cancel-a2a").addEventListener("click", () => elements.a2aDialog.close());
document.getElementById("close-agent-dialog").addEventListener("click", () => elements.agentDialog.close());
document.getElementById("cancel-agent").addEventListener("click", () => elements.agentDialog.close());
document.getElementById("close-channel-dialog").addEventListener("click", () => elements.channelDialog.close());
document.getElementById("cancel-channel").addEventListener("click", () => elements.channelDialog.close());
document.getElementById("close-members-dialog").addEventListener("click", () => elements.membersDialog.close());
document.getElementById("cancel-members").addEventListener("click", () => elements.membersDialog.close());
elements.requester.addEventListener("change", () => loadCapabilities().catch((error) => { elements.formError.textContent = error.message; }));
elements.taskForm.addEventListener("submit", submitTask);
elements.a2aForm.addEventListener("submit", submitA2aQuestion);
elements.agentForm.addEventListener("submit", submitAgent);
elements.channelForm.addEventListener("submit", submitChannel);
elements.membersForm.addEventListener("submit", submitMembers);
window.addEventListener("beforeunload", closeChat);

refresh();
setInterval(refresh, 15000);

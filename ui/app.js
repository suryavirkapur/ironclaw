const state = {
  agents: [],
  tasks: [],
  view: "tasks",
  selectedTaskId: null,
};

const elements = {
  healthDot: document.getElementById("health-dot"),
  connectionLabel: document.getElementById("connection-label"),
  agentList: document.getElementById("agent-list"),
  agentCount: document.getElementById("agent-count"),
  content: document.getElementById("content"),
  viewTitle: document.getElementById("view-title"),
  viewDescription: document.getElementById("view-description"),
  detailTitle: document.getElementById("detail-title"),
  detailContent: document.getElementById("detail-content"),
  taskDialog: document.getElementById("task-dialog"),
  taskForm: document.getElementById("task-form"),
  requester: document.getElementById("requester"),
  capability: document.getElementById("capability"),
  taskRequest: document.getElementById("task-request"),
  formError: document.getElementById("form-error"),
};

const terminalStates = new Set(["completed", "failed", "canceled", "rejected"]);

async function api(path, options = {}) {
  const response = await fetch(path, {
    headers: { "content-type": "application/json", ...(options.headers || {}) },
    ...options,
  });
  const body = await response.json().catch(() => ({}));
  if (!response.ok) {
    throw new Error(body.error || `${response.status} ${response.statusText}`);
  }
  return body;
}

function node(tag, className, text) {
  const element = document.createElement(tag);
  if (className) element.className = className;
  if (text !== undefined) element.textContent = text;
  return element;
}

function initials(name) {
  return name
    .split(/\s+/)
    .map((part) => part[0])
    .join("")
    .slice(0, 2)
    .toUpperCase();
}

function taskState(task) {
  return String(task.state || "unknown").toLowerCase();
}

function activeCount(agentId) {
  return state.tasks.filter(
    (task) => task.assignee === agentId && !terminalStates.has(taskState(task)),
  ).length;
}

function renderSidebar() {
  elements.agentList.replaceChildren();
  elements.agentCount.textContent = String(state.agents.length);
  for (const agent of state.agents) {
    const row = node("button", "agent-row");
    row.type = "button";
    row.addEventListener("click", () => {
      state.view = "agents";
      render();
      inspectAgent(agent);
    });
    row.append(node("span", "avatar", initials(agent.name)));
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

function renderTasks() {
  elements.viewTitle.textContent = "delivery";
  elements.viewDescription.textContent = "Live A2A work across the engineering team";
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
    top.append(
      node("span", `state-pill ${taskState(task)}`, taskState(task).replace("_", " ")),
      node("time", "", new Date(task.updated_at_ms).toLocaleString()),
    );
    const request = task.input?.request || JSON.stringify(task.input || {});
    card.append(
      top,
      node("h3", "", task.skill.replaceAll("_", " ")),
      node("p", "task-summary", request),
      node("p", "task-route", `${task.requester}  →  ${task.assignee}`),
    );
    elements.content.append(card);
  }
}

function renderAgents() {
  elements.viewTitle.textContent = "team";
  elements.viewDescription.textContent = "Five isolated agents with private memory and capabilities";
  elements.content.replaceChildren();
  const grid = node("div", "team-grid");
  for (const agent of state.agents) {
    const card = node("button", "person-card");
    card.type = "button";
    card.addEventListener("click", () => inspectAgent(agent));
    card.append(
      node("span", "avatar large", initials(agent.name)),
      node("h3", "", agent.name),
      node("p", "", agent.role),
      node("small", "", `${agent.a2a_skills} skills · ${activeCount(agent.id)} active tasks`),
    );
    grid.append(card);
  }
  elements.content.append(grid);
}

function renderArchitecture() {
  elements.viewTitle.textContent = "architecture";
  elements.viewDescription.textContent = "One shared control plane, five private agent VMs";
  elements.content.replaceChildren();
  const diagram = node("div", "architecture-card");
  diagram.append(
    node("div", "architecture-node human", "Telegram + Workspace"),
    node("div", "architecture-arrow", "↓"),
  );
  const host = node("div", "architecture-node host", "Ironclaw host");
  host.append(node("small", "", "registry · task ledger · MCP broker · VM manager"));
  diagram.append(host, node("div", "architecture-arrow", "↓ authenticated A2A tasks"));
  const vmRow = node("div", "vm-row");
  for (const agent of state.agents) {
    const vm = node("div", "architecture-node vm");
    vm.append(node("strong", "", agent.name));
    vm.append(node("small", "", `${agent.role} · private VM · memory · Wasm`));
    vmRow.append(vm);
  }
  diagram.append(vmRow);
  elements.content.append(diagram);
}

function render() {
  document.querySelectorAll(".channel").forEach((button) => {
    button.classList.toggle("active", button.dataset.view === state.view);
  });
  renderSidebar();
  renderMetrics();
  if (state.view === "agents") renderAgents();
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
  elements.detailContent.replaceChildren(
    detailRow("State", taskState(task)),
    detailRow("Requester", task.requester),
    detailRow("Assignee", task.assignee),
    detailRow("Task", task.id),
    detailRow("Context", task.context_id),
    detailRow("Depth", String(task.delegation_depth)),
  );
  const input = node("pre", "json-block", JSON.stringify(task.input, null, 2));
  const output = node("pre", "json-block", JSON.stringify(task.output, null, 2));
  elements.detailContent.append(node("h3", "", "Input"), input, node("h3", "", "Output"), output);
  renderTasks();
}

async function inspectAgent(agent) {
  elements.detailTitle.textContent = agent.name;
  elements.detailContent.replaceChildren(
    detailRow("Role", agent.role),
    detailRow("Agent ID", agent.id),
    detailRow("Reports to", agent.reports_to || "team owner"),
    detailRow("Active work", String(activeCount(agent.id))),
    detailRow("Wasm tools", String(agent.wasm_tools)),
    detailRow("MCP servers", String(agent.mcp_servers)),
  );
  try {
    const capabilities = await api(`/api/farm/agents/${encodeURIComponent(agent.id)}/capabilities`);
    elements.detailContent.append(node("h3", "", "Capabilities"));
    for (const capability of capabilities) {
      elements.detailContent.append(node("code", "capability", capability.uri));
    }
  } catch (error) {
    elements.detailContent.append(node("p", "error", error.message));
  }
}

async function loadCapabilities() {
  const requester = elements.requester.value;
  elements.capability.replaceChildren();
  if (!requester) return;
  const capabilities = await api(`/api/farm/agents/${encodeURIComponent(requester)}/capabilities`);
  const assignments = capabilities.filter((capability) => capability.kind === "a2a_skill");
  for (const capability of assignments) {
    const option = document.createElement("option");
    option.value = capability.uri;
    option.textContent = `${capability.uri.replace("agent://", "")} — ${capability.description}`;
    elements.capability.append(option);
  }
  if (!assignments.length) {
    const option = document.createElement("option");
    option.textContent = "This agent cannot delegate work";
    option.disabled = true;
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
  const uri = elements.capability.value;
  const match = /^agent:\/\/([^/]+)\/(.+)$/.exec(uri);
  if (!match) {
    elements.formError.textContent = "Choose an available assignment.";
    return;
  }
  try {
    const task = await api("/api/farm/tasks", {
      method: "POST",
      body: JSON.stringify({
        requester: elements.requester.value,
        assignee: match[1],
        skill: match[2],
        input: { request: elements.taskRequest.value, source: "workspace" },
      }),
    });
    elements.taskRequest.value = "";
    elements.taskDialog.close();
    state.selectedTaskId = task.id;
    await refresh();
    const currentTask = state.tasks.find((candidate) => candidate.id === task.id);
    if (currentTask) inspectTask(currentTask);
  } catch (error) {
    elements.formError.textContent = error.message;
  }
}

async function refresh() {
  try {
    const [health, agents, tasks] = await Promise.all([
      api("/api/health"),
      api("/api/farm/agents"),
      api("/api/farm/tasks"),
    ]);
    state.agents = agents;
    state.tasks = tasks;
    elements.healthDot.classList.add("online");
    elements.connectionLabel.textContent = `${health.status} · live`;
    render();
    if (state.selectedTaskId) {
      const selectedTask = state.tasks.find((task) => task.id === state.selectedTaskId);
      if (selectedTask) inspectTask(selectedTask);
    }
  } catch (error) {
    elements.healthDot.classList.remove("online");
    elements.connectionLabel.textContent = "offline";
    elements.content.replaceChildren(node("div", "empty-state error", error.message));
  }
}

document.querySelectorAll(".channel").forEach((button) => {
  button.addEventListener("click", () => {
    state.view = button.dataset.view;
    render();
  });
});
document.getElementById("new-task-button").addEventListener("click", openTaskDialog);
document.getElementById("close-dialog").addEventListener("click", () => elements.taskDialog.close());
document.getElementById("cancel-task").addEventListener("click", () => elements.taskDialog.close());
elements.requester.addEventListener("change", () => loadCapabilities().catch((error) => {
  elements.formError.textContent = error.message;
}));
elements.taskForm.addEventListener("submit", submitTask);

refresh();
setInterval(refresh, 3000);

const statusEl = document.getElementById("status");
const heartbeatEl = document.getElementById("heartbeat");
const vmsEl = document.getElementById("vms");
const keysEl = document.getElementById("keys");
const memoryEl = document.getElementById("memory");
const refreshBtn = document.getElementById("refresh");
const keyForm = document.getElementById("key-form");
const keyNameInput = document.getElementById("key-name");
const keyValueInput = document.getElementById("key-value");

function setStatus(text) {
  statusEl.textContent = text;
}

async function fetchJson(path, options) {
  const response = await fetch(path, options);
  if (!response.ok) {
    let message = `${response.status}`;
    try {
      const body = await response.json();
      if (body && body.error) {
        message = body.error;
      }
    } catch {
      // use default
    }
    throw new Error(message);
  }
  return response.json();
}

function renderHeartbeat(data) {
  heartbeatEl.innerHTML = `
    <div class="grid">
      <div><strong>running</strong><span>${data.running}</span></div>
      <div><strong>interval</strong><span>${data.interval_seconds}s</span></div>
      <div><strong>last tick</strong><span>${data.last_tick_at || "n/a"}</span></div>
      <div><strong>total ticks</strong><span>${data.total_ticks}</span></div>
    </div>
  `;
}

function renderVms(vms) {
  if (!vms.length) {
    vmsEl.textContent = "no active vms";
    return;
  }
  vmsEl.innerHTML = vms
    .map(
      (vm) => `
      <div class="row">
        <div>
          <strong>${vm.vm_id}</strong>
          <p>user=${vm.user_id} uptime=${vm.uptime_seconds}s status=${vm.status}</p>
        </div>
        <button data-stop-vm="${vm.vm_id}">stop</button>
      </div>
    `,
    )
    .join("");
}

function renderKeys(keys) {
  if (!keys.length) {
    keysEl.textContent = "no keys stored";
    return;
  }
  keysEl.innerHTML = keys
    .map(
      (key) => `
      <div class="row">
        <div>
          <strong>${key.name}</strong>
          <p>${key.masked_value} updated=${key.updated_at}</p>
        </div>
        <button data-delete-key="${key.name}">delete</button>
      </div>
    `,
    )
    .join("");
}

function renderMemory(items) {
  if (!items.length) {
    memoryEl.textContent = "no memory entries";
    return;
  }
  memoryEl.innerHTML = items
    .map(
      (item) => `
      <div class="row memory-row">
        <div>
          <strong>#${item.id} ${item.kind}</strong>
          <p>${item.preview}</p>
        </div>
        <button data-memory-id="${item.id}">view</button>
      </div>
    `,
    )
    .join("");
}

async function loadDashboard() {
  setStatus("loading");
  try {
    const [heartbeat, vms, keys, memory] = await Promise.all([
      fetchJson("/api/admin/heartbeat"),
      fetchJson("/api/admin/vms"),
      fetchJson("/api/admin/keys"),
      fetchJson("/api/admin/memory?limit=20"),
    ]);
    renderHeartbeat(heartbeat);
    renderVms(vms);
    renderKeys(keys);
    renderMemory(memory);
    setStatus("ready");
  } catch (error) {
    setStatus(`error: ${error.message}`);
  }
}

async function stopVm(vmId) {
  await fetchJson(`/api/admin/vms/${encodeURIComponent(vmId)}/stop`, {
    method: "POST",
  });
}

async function deleteKey(name) {
  await fetchJson(`/api/admin/keys/${encodeURIComponent(name)}`, {
    method: "DELETE",
  });
}

async function storeKey(event) {
  event.preventDefault();
  const name = keyNameInput.value.trim();
  const value = keyValueInput.value.trim();
  if (!name || !value) {
    setStatus("error: key name and value are required");
    return;
  }
  await fetchJson(`/api/admin/keys/${encodeURIComponent(name)}`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ value }),
  });
  keyValueInput.value = "";
}

async function openMemory(id) {
  const detail = await fetchJson(`/api/admin/memory/${encodeURIComponent(id)}`);
  alert(
    `memory #${detail.id}\nkind=${detail.kind}\nupdated=${detail.updated_at}\n\n${detail.content}`,
  );
}

refreshBtn.addEventListener("click", () => {
  loadDashboard();
});

keyForm.addEventListener("submit", async (event) => {
  try {
    await storeKey(event);
    await loadDashboard();
  } catch (error) {
    setStatus(`error: ${error.message}`);
  }
});

document.addEventListener("click", async (event) => {
  const stopVmId = event.target?.getAttribute?.("data-stop-vm");
  const deleteKeyName = event.target?.getAttribute?.("data-delete-key");
  const memoryId = event.target?.getAttribute?.("data-memory-id");
  try {
    if (stopVmId) {
      await stopVm(stopVmId);
      await loadDashboard();
      return;
    }
    if (deleteKeyName) {
      await deleteKey(deleteKeyName);
      await loadDashboard();
      return;
    }
    if (memoryId) {
      await openMemory(memoryId);
    }
  } catch (error) {
    setStatus(`error: ${error.message}`);
  }
});

loadDashboard();
setInterval(loadDashboard, 30000);

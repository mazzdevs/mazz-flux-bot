const params = new URLSearchParams(location.search);
const projectId = params.get("id");

if (!projectId) {
  document.body.innerHTML = "<p style='padding:2rem'>No project id in URL. <a href='/'>Back to dashboard</a>.</p>";
  throw new Error("no project id");
}

async function api(path, opts) {
  const res = await fetch(path, { headers: { "Content-Type": "application/json" }, ...opts });
  const body = await res.json().catch(() => ({}));
  if (!res.ok) throw new Error(body.error || `${res.status} ${res.statusText}`);
  return body;
}

function escapeHtml(s) {
  return String(s ?? "").replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));
}

// ---- Relative time formatting ------------------------------------------

function formatRelative(iso) {
  if (!iso) return "never";
  const then = new Date(iso).getTime();
  if (Number.isNaN(then)) return iso;
  const diffMs = Date.now() - then;
  const future = diffMs < 0;
  const abs = Math.abs(diffMs);
  const sec = Math.round(abs / 1000);
  const units = [
    ["year", 31536000],
    ["month", 2592000],
    ["day", 86400],
    ["hour", 3600],
    ["minute", 60],
    ["second", 1],
  ];
  let label;
  if (sec < 5) {
    label = "just now";
    return future ? "in a moment" : label;
  }
  for (const [name, secs] of units) {
    const count = Math.floor(sec / secs);
    if (count >= 1) {
      label = `${count} ${name}${count === 1 ? "" : "s"}`;
      break;
    }
  }
  return future ? `in ${label}` : `${label} ago`;
}

function statusClass(status) {
  if (status === "running") return "status-running";
  if (status === "error") return "status-error";
  if (status === "done") return "status-done";
  if (status === "blocked") return "status-blocked";
  return "";
}

let currentProject = null;

async function loadProject() {
  const { project: p } = await api(`/api/projects/${projectId}`);
  currentProject = p;

  document.getElementById("project-name").textContent = p.name;
  document.getElementById("project-goal").textContent = p.goal;
  document.title = `mazz-flux-bot — ${p.name}`;

  const goalInput = document.getElementById("goal-input");
  if (document.activeElement !== goalInput) goalInput.value = p.goal;
  const heartbeatPromptInput = document.getElementById("heartbeat-prompt-input");
  if (document.activeElement !== heartbeatPromptInput) heartbeatPromptInput.value = p.heartbeat_prompt || "";

  document.getElementById("ov-status").innerHTML = `<span class="status-pill ${statusClass(p.status)}">${escapeHtml(p.status)}</span>`;
  document.getElementById("ov-constellation").textContent = p.constellation;
  document.getElementById("ov-instance").innerHTML = p.vape_instance_id ? `<code>${escapeHtml(p.vape_instance_id)}</code>` : "none yet";
  document.getElementById("ov-heartbeat").textContent = p.heartbeat_enabled ? "enabled" : "disabled";
  document.getElementById("ov-last-heartbeat").title = p.last_heartbeat_at || "";
  document.getElementById("ov-last-heartbeat").textContent = formatRelative(p.last_heartbeat_at);
  document.getElementById("ov-last-note").textContent = p.last_note || "";
  document.getElementById("ov-created").title = p.created_at;
  document.getElementById("ov-created").textContent = formatRelative(p.created_at);

  const intervalInput = document.getElementById("interval-input");
  const intervalUnit = document.getElementById("interval-unit");
  if (document.activeElement !== intervalInput && document.activeElement !== intervalUnit) {
    // Pick the largest unit that divides evenly into whole numbers, so e.g.
    // 900s displays as "15 minutes" instead of "900 seconds" — storage is
    // always seconds regardless of what's shown here.
    const secs = p.heartbeat_interval_secs;
    let unit = 1;
    if (secs % 3600 === 0) unit = 3600;
    else if (secs % 60 === 0) unit = 60;
    intervalInput.value = secs / unit;
    intervalUnit.value = String(unit);
  }

  const canStart = p.status !== "running";
  document.getElementById("p-start").style.display = canStart ? "" : "none";
  document.getElementById("p-pause").style.display = canStart ? "none" : "";

  return p;
}

// ---- Next-heartbeat countdown -------------------------------------------
//
// Computed client-side from last_heartbeat_at (or created_at, if it has
// never ticked) + heartbeat_interval_secs, and re-rendered every second
// independent of the 5s data poll so the countdown itself feels live. If
// heartbeat_enabled is false, there's no countdown to show.

function nextHeartbeatAt(p) {
  const base = p.last_heartbeat_at || p.created_at;
  if (!base) return null;
  return new Date(base).getTime() + p.heartbeat_interval_secs * 1000;
}

function renderCountdown() {
  const el = document.getElementById("ov-next-heartbeat");
  if (!currentProject || !currentProject.heartbeat_enabled) {
    el.textContent = "heartbeat disabled";
    return;
  }
  const next = nextHeartbeatAt(currentProject);
  if (next === null) {
    el.textContent = "unknown";
    return;
  }
  const remainingMs = next - Date.now();
  if (remainingMs <= 0) {
    el.textContent = "due now";
    return;
  }
  const totalSec = Math.ceil(remainingMs / 1000);
  const m = Math.floor(totalSec / 60);
  const s = totalSec % 60;
  el.textContent = m > 0 ? `in ${m}m ${s}s` : `in ${s}s`;
}

setInterval(renderCountdown, 1000);

const VAPE_DASHBOARD_URL = "https://vape.stable.dexus.io";

async function loadInstanceLinks(p) {
  const linksEl = document.getElementById("ov-instance-links");
  const linksRow = document.getElementById("ov-instance-links-row");
  const renameRow = document.getElementById("ov-instance-rename-row");
  if (!p.vape_instance_id) {
    linksRow.style.display = "none";
    renameRow.style.display = "none";
    return;
  }
  linksRow.style.display = "";
  renameRow.style.display = "";
  try {
    const { instance } = await api(`/api/instances/${p.vape_instance_id}`);
    const urls = instance?.urls || [];
    const items = [
      `<li><a href="${VAPE_DASHBOARD_URL}/instances/${escapeHtml(p.vape_instance_id)}" target="_blank" rel="noopener"><svg class="icon"><use href="/icons.svg#icon-dashboard"></use></svg> vape dashboard</a></li>`,
      ...urls.map((u) => `<li><a href="${escapeHtml(u)}" target="_blank" rel="noopener"><svg class="icon"><use href="/icons.svg#icon-send"></use></svg> ${escapeHtml(u)}</a></li>`),
    ];
    linksEl.innerHTML = items.join("");
  } catch (e) {
    linksEl.innerHTML = `<li class="empty">failed to load instance links: ${escapeHtml(e.message)}</li>`;
  }
}

async function loadInstanceStatus(p) {
  const body = document.getElementById("instance-status-body");
  if (!p.vape_instance_id) {
    body.textContent = "No instance yet.";
    return;
  }
  try {
    const { agent_status, pida_status } = await api(`/api/instances/${p.vape_instance_id}/status`);
    const { session } = await api(`/api/instances/${p.vape_instance_id}/session`);
    const messages = (session?.messages || []).slice(-6).reverse();

    const pidaBits = pida_status
      ? `ready: ${pida_status.ready} · streaming: ${pida_status.isStreaming} · model: ${escapeHtml(pida_status.model || "?")} · pending question: ${
          pida_status.pendingAsk ? "yes" : "no"
        }`
      : "(non-pida harness or status unavailable)";

    body.innerHTML = `
      <div class="sub">harness: ${escapeHtml(agent_status.active_harness || "?")} · state: ${escapeHtml(agent_status.state)}</div>
      <div class="sub">${pidaBits}</div>
      <div class="notes-list" style="margin-top:0.5rem">
        ${messages
          .map((m) => `<li><div class="task-meta">${escapeHtml(m.role)}</div><pre>${escapeHtml(JSON.stringify(m.content ?? m, null, 0)).slice(0, 500)}</pre></li>`)
          .join("")}
      </div>`;
  } catch (e) {
    body.textContent = `failed to load live status: ${e.message}`;
  }
}

async function loadHumanTasks() {
  const { entries } = await api(`/api/human-tasks?project_id=${projectId}&open=false`);
  const list = document.getElementById("p-human-tasks");
  list.innerHTML = (entries || []).length
    ? entries
        .map(
          (t) => `<li>
            <div class="task-desc">
              ${escapeHtml(t.description)}
              <div class="task-meta">${escapeHtml(t.status)} · ${escapeHtml(t.created_at)}${t.resolved_at ? " · resolved " + escapeHtml(t.resolved_at) : ""}</div>
            </div>
            ${t.status === "open" ? `<button class="btn-secondary" data-resolve="${t.id}"><svg class="icon"><use href="/icons.svg#icon-check"></use></svg> Resolve</button>` : ""}
          </li>`
        )
        .join("")
    : `<li class="empty">No human tasks raised for this project.</li>`;
}

document.getElementById("p-human-tasks").addEventListener("click", async (ev) => {
  const btn = ev.target.closest("button[data-resolve]");
  if (!btn) return;
  try {
    await api(`/api/human-tasks/${btn.dataset.resolve}/resolve`, { method: "POST" });
    await loadHumanTasks();
  } catch (e) {
    alert(e.message);
  }
});

async function loadNotes() {
  const { notes } = await api(`/api/projects/${projectId}/notes`);
  const list = document.getElementById("p-notes");
  list.innerHTML = (notes || []).length
    ? notes.map((n) => `<li><div class="task-meta">${escapeHtml(n.created_at)}</div><pre>${escapeHtml(n.content)}</pre></li>`).join("")
    : `<li class="empty">No notes yet.</li>`;
}

async function loadMemory() {
  const el = document.getElementById("memory-body");
  try {
    const { memory } = await api(`/api/projects/${projectId}/memory`);
    if (memory) {
      el.textContent = memory;
      el.classList.remove("empty");
    } else {
      el.textContent = "No memory yet.";
      el.classList.add("empty");
    }
  } catch (e) {
    el.textContent = `failed to load memory: ${e.message}`;
    el.classList.add("empty");
  }
}

async function loadLog() {
  const { entries } = await api(`/api/log?project_id=${projectId}&limit=100`);
  const list = document.getElementById("p-log");
  list.innerHTML = (entries || [])
    .map(
      (e) =>
        `<li><span class="action">${escapeHtml(e.action)}</span><span class="ts" title="${escapeHtml(e.created_at)}">${escapeHtml(
          formatRelative(e.created_at)
        )}</span><br/>${escapeHtml(e.error || e.result || "").slice(0, 300)}</li>`
    )
    .join("");
}

document.getElementById("p-start").addEventListener("click", async () => {
  await api(`/api/projects/${projectId}/start`, { method: "POST" });
  await tick();
});
document.getElementById("p-pause").addEventListener("click", async () => {
  await api(`/api/projects/${projectId}/pause`, { method: "POST" });
  await tick();
});
document.getElementById("p-delete").addEventListener("click", async () => {
  if (!confirm("Delete this project? (does not delete the vape instance)")) return;
  await api(`/api/projects/${projectId}`, { method: "DELETE" });
  location.href = "/";
});

document.getElementById("p-force-heartbeat").addEventListener("click", async (ev) => {
  const btn = ev.currentTarget;
  btn.disabled = true;
  const original = btn.textContent;
  btn.textContent = "Ticking…";
  try {
    await api(`/api/projects/${projectId}/heartbeat/force`, { method: "POST" });
    await tick();
  } catch (e) {
    alert(e.message);
  } finally {
    btn.disabled = false;
    btn.textContent = original;
  }
});

document.getElementById("goal-form").addEventListener("submit", async (ev) => {
  ev.preventDefault();
  const statusEl = document.getElementById("goal-status");
  const goal = document.getElementById("goal-input").value.trim();
  if (!goal) return;
  try {
    await api(`/api/projects/${projectId}/goal`, { method: "POST", body: JSON.stringify({ goal }) });
    statusEl.textContent = "saved";
    statusEl.classList.remove("error");
    await tick();
  } catch (e) {
    statusEl.textContent = e.message;
    statusEl.classList.add("error");
  }
});

document.getElementById("heartbeat-prompt-form").addEventListener("submit", async (ev) => {
  ev.preventDefault();
  const statusEl = document.getElementById("heartbeat-prompt-status");
  const heartbeatPrompt = document.getElementById("heartbeat-prompt-input").value.trim();
  try {
    await api(`/api/projects/${projectId}/heartbeat-prompt`, { method: "POST", body: JSON.stringify({ heartbeat_prompt: heartbeatPrompt }) });
    statusEl.textContent = "saved";
    statusEl.classList.remove("error");
    await tick();
  } catch (e) {
    statusEl.textContent = e.message;
    statusEl.classList.add("error");
  }
});

document.getElementById("interval-form").addEventListener("submit", async (ev) => {
  ev.preventDefault();
  const raw = parseInt(document.getElementById("interval-input").value, 10);
  const unit = parseInt(document.getElementById("interval-unit").value, 10);
  const value = raw * unit;
  if (!Number.isFinite(value) || value < 5) {
    alert("Interval must be at least 5 seconds.");
    return;
  }
  try {
    await api(`/api/projects/${projectId}/heartbeat-interval`, { method: "POST", body: JSON.stringify({ heartbeat_interval_secs: value }) });
    await tick();
  } catch (e) {
    alert(e.message);
  }
});

document.getElementById("instance-rename-form").addEventListener("submit", async (ev) => {
  ev.preventDefault();
  const input = document.getElementById("instance-rename-input");
  const statusEl = document.getElementById("instance-rename-status");
  const name = input.value.trim();
  if (!name) return;
  try {
    await api(`/api/projects/${projectId}/instance/rename`, { method: "POST", body: JSON.stringify({ name }) });
    statusEl.textContent = "renamed";
    statusEl.classList.remove("error");
    input.value = "";
    await tick();
  } catch (e) {
    statusEl.textContent = e.message;
    statusEl.classList.add("error");
  }
});

document.getElementById("message-form").addEventListener("submit", async (ev) => {
  ev.preventDefault();
  const textarea = document.getElementById("message-text");
  const message = textarea.value.trim();
  if (!message) return;
  try {
    await api(`/api/projects/${projectId}/message`, { method: "POST", body: JSON.stringify({ message }) });
    textarea.value = "";
    await tick();
  } catch (e) {
    alert(e.message);
  }
});

async function tick() {
  try {
    const p = await loadProject();
    renderCountdown();
    await Promise.all([loadInstanceStatus(p), loadInstanceLinks(p), loadHumanTasks(), loadNotes(), loadMemory(), loadLog()]);
  } catch (e) {
    console.error(e);
  }
}

tick();
setInterval(tick, 5000);

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
  if (document.activeElement !== intervalInput) {
    intervalInput.value = p.heartbeat_interval_secs;
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
            ${t.status === "open" ? `<button class="mini" data-resolve="${t.id}">Resolve</button>` : ""}
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

document.getElementById("interval-form").addEventListener("submit", async (ev) => {
  ev.preventDefault();
  const value = parseInt(document.getElementById("interval-input").value, 10);
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
    await Promise.all([loadInstanceStatus(p), loadHumanTasks(), loadNotes(), loadLog()]);
  } catch (e) {
    console.error(e);
  }
}

tick();
setInterval(tick, 5000);

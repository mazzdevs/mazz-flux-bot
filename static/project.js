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
  document.getElementById("ov-last-heartbeat").textContent = p.last_heartbeat_at || "never";
  document.getElementById("ov-last-note").textContent = p.last_note || "";
  document.getElementById("ov-created").textContent = p.created_at;

  const canStart = p.status !== "running";
  document.getElementById("p-start").style.display = canStart ? "" : "none";
  document.getElementById("p-pause").style.display = canStart ? "none" : "";

  return p;
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
        `<li><span class="action">${escapeHtml(e.action)}</span><span class="ts">${escapeHtml(e.created_at)}</span><br/>${escapeHtml(
          e.error || e.result || ""
        ).slice(0, 300)}</li>`
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
    await Promise.all([loadInstanceStatus(p), loadHumanTasks(), loadNotes(), loadLog()]);
  } catch (e) {
    console.error(e);
  }
}

tick();
setInterval(tick, 5000);

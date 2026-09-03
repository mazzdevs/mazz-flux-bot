const projectsBody = document.getElementById("projects-body");
const logList = document.getElementById("log-list");
const constellationsList = document.getElementById("constellations");

async function api(path, opts) {
  const res = await fetch(path, {
    headers: { "Content-Type": "application/json" },
    ...opts,
  });
  const body = await res.json().catch(() => ({}));
  if (!res.ok) throw new Error(body.error || `${res.status} ${res.statusText}`);
  return body;
}

function statusClass(status) {
  if (status === "running") return "status-running";
  if (status === "error") return "status-error";
  if (status === "done") return "status-done";
  return "";
}

function escapeHtml(s) {
  return String(s ?? "").replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));
}

async function loadConstellations() {
  try {
    const { constellations } = await api("/api/constellations");
    constellationsList.innerHTML = (constellations || [])
      .map((c) => `<option value="${escapeHtml(c.id)}">${escapeHtml(c.name || c.id)}</option>`)
      .join("");
  } catch (e) {
    // Non-fatal — likely not on WARP yet. The create form still works with a
    // manually typed constellation id.
    console.warn("failed to load constellations", e);
  }
}

async function loadProjects() {
  const { projects } = await api("/api/projects");
  projectsBody.innerHTML = (projects || [])
    .map((p) => {
      const instanceCell = p.vape_instance_id
        ? `<code>${escapeHtml(p.vape_instance_id)}</code>`
        : `<span class="note">none yet</span>`;
      const canStart = p.status !== "running";
      const actions = [
        canStart
          ? `<button class="mini" data-act="start" data-id="${p.id}">Start</button>`
          : `<button class="mini" data-act="pause" data-id="${p.id}">Pause</button>`,
        `<button class="mini danger" data-act="delete" data-id="${p.id}">Delete</button>`,
      ].join("");
      return `<tr>
        <td>${escapeHtml(p.name)}</td>
        <td><span class="status-pill ${statusClass(p.status)}">${escapeHtml(p.status)}</span></td>
        <td>${instanceCell}</td>
        <td class="goal">${escapeHtml(p.goal)}</td>
        <td class="note">${escapeHtml(p.last_note || "")}</td>
        <td>${actions}</td>
      </tr>`;
    })
    .join("");
}

async function loadLog() {
  const { entries } = await api("/api/log?limit=50");
  logList.innerHTML = (entries || [])
    .map(
      (e) =>
        `<li><span class="action">${escapeHtml(e.action)}</span><span class="ts">${escapeHtml(e.created_at)}</span><br/>${escapeHtml(
          e.error || e.result || ""
        ).slice(0, 200)}</li>`
    )
    .join("");
}

projectsBody.addEventListener("click", async (ev) => {
  const btn = ev.target.closest("button[data-act]");
  if (!btn) return;
  const { act, id } = btn.dataset;
  try {
    if (act === "start") await api(`/api/projects/${id}/start`, { method: "POST" });
    if (act === "pause") await api(`/api/projects/${id}/pause`, { method: "POST" });
    if (act === "delete") {
      if (!confirm("Delete this project? (does not delete the vape instance)")) return;
      await api(`/api/projects/${id}`, { method: "DELETE" });
    }
    await loadProjects();
  } catch (e) {
    alert(e.message);
  }
});

document.getElementById("create-form").addEventListener("submit", async (ev) => {
  ev.preventDefault();
  const name = document.getElementById("name").value.trim();
  const constellation = document.getElementById("constellation").value.trim();
  const goal = document.getElementById("goal").value.trim();
  try {
    await api("/api/projects", { method: "POST", body: JSON.stringify({ name, constellation, goal }) });
    ev.target.reset();
    await loadProjects();
  } catch (e) {
    alert(e.message);
  }
});

async function tick() {
  try {
    await Promise.all([loadProjects(), loadLog()]);
  } catch (e) {
    console.error(e);
  }
}

loadConstellations();
tick();
setInterval(tick, 5000);

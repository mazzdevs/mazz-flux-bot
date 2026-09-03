const projectsBody = document.getElementById("projects-body");
const projectsTiles = document.getElementById("projects-tiles");
const projectsTable = document.getElementById("projects-table");
const logList = document.getElementById("log-list");
const constellationsList = document.getElementById("constellations");
const humanTasksList = document.getElementById("human-tasks-list");
const humanTasksCount = document.getElementById("human-tasks-count");

async function api(path, opts) {
  const res = await fetch(path, {
    headers: { "Content-Type": "application/json" },
    ...opts,
  });
  const body = await res.json().catch(() => ({}));
  if (!res.ok) throw new Error(body.error || `${res.status} ${res.statusText}`);
  return body;
}

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
  if (sec < 5) return future ? "in a moment" : "just now";
  let label = `${sec}s`;
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

function projectActionsHtml(p) {
  const canStart = p.status !== "running";
  return `<div class="btn-group">${
    canStart
      ? `<button class="btn-secondary" data-act="start" data-id="${p.id}"><svg class="icon"><use href="/icons.svg#icon-play"></use></svg> Start</button>`
      : `<button class="btn-secondary" data-act="pause" data-id="${p.id}"><svg class="icon"><use href="/icons.svg#icon-pause"></use></svg> Pause</button>`
  }<button class="btn-danger" data-act="delete" data-id="${p.id}"><svg class="icon"><use href="/icons.svg#icon-trash"></use></svg></button></div>`;
}

async function loadProjects() {
  const { projects } = await api("/api/projects");
  const list = projects || [];

  projectsBody.innerHTML = list
    .map((p) => {
      const instanceCell = p.vape_instance_id
        ? `<code>${escapeHtml(p.vape_instance_id)}</code>`
        : `<span class="note">none yet</span>`;
      return `<tr>
        <td><a class="project-link" href="/project.html?id=${p.id}">${escapeHtml(p.name)}</a></td>
        <td><span class="status-pill ${statusClass(p.status)}">${escapeHtml(p.status)}</span></td>
        <td>${instanceCell}</td>
        <td class="goal">${escapeHtml(p.goal)}</td>
        <td class="note">${escapeHtml(p.last_note || "")}</td>
        <td>${projectActionsHtml(p)}</td>
      </tr>`;
    })
    .join("");

  projectsTiles.innerHTML = list
    .map((p) => {
      const instance = p.vape_instance_id ? `<code>${escapeHtml(p.vape_instance_id)}</code>` : "none yet";
      return `<div class="project-tile">
        <div class="tile-head">
          <a class="project-link" href="/project.html?id=${p.id}"><strong>${escapeHtml(p.name)}</strong></a>
          <span class="status-pill ${statusClass(p.status)}">${escapeHtml(p.status)}</span>
        </div>
        <div class="goal">${escapeHtml(p.goal)}</div>
        <div class="tile-instance">instance: ${instance}</div>
        <div class="note">${escapeHtml(p.last_note || "")}</div>
        <div class="tile-actions">${projectActionsHtml(p)}</div>
      </div>`;
    })
    .join("");
}

async function loadLog() {
  const { entries } = await api("/api/log?limit=50");
  logList.innerHTML = (entries || [])
    .map(
      (e) =>
        `<li><span class="action">${escapeHtml(e.action)}</span><span class="ts" title="${escapeHtml(e.created_at)}">${escapeHtml(
          formatRelative(e.created_at)
        )}</span><br/>${escapeHtml(e.error || e.result || "").slice(0, 200)}</li>`
    )
    .join("");
}

async function loadHumanTasks() {
  const { entries } = await api("/api/human-tasks");
  const tasks = entries || [];
  humanTasksCount.textContent = tasks.length || "";
  humanTasksCount.classList.toggle("zero", tasks.length === 0);
  humanTasksCount.classList.toggle("badge", tasks.length > 0);

  humanTasksList.innerHTML = tasks.length
    ? tasks
        .map(
          (t) => `<li>
            <div class="task-desc">
              <a class="project-link" href="/project.html?id=${t.project_id}">${escapeHtml(t.project_name || t.project_id)}</a>:
              ${escapeHtml(t.description)}
              <div class="task-meta">${escapeHtml(t.created_at)}</div>
            </div>
            <button class="btn-secondary" data-resolve="${t.id}"><svg class="icon"><use href="/icons.svg#icon-check"></use></svg> Resolve</button>
          </li>`
        )
        .join("")
    : `<li class="empty">No open human tasks.</li>`;
}

humanTasksList.addEventListener("click", async (ev) => {
  const btn = ev.target.closest("button[data-resolve]");
  if (!btn) return;
  try {
    await api(`/api/human-tasks/${btn.dataset.resolve}/resolve`, { method: "POST" });
    await Promise.all([loadHumanTasks(), loadProjects()]);
  } catch (e) {
    alert(e.message);
  }
});

function bindProjectActions(container) {
  container.addEventListener("click", async (ev) => {
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
}
bindProjectActions(projectsBody);
bindProjectActions(projectsTiles);

// ---- Create-project dialog ---------------------------------------------

const createDialog = document.getElementById("create-dialog");

document.getElementById("create-toggle").addEventListener("click", async () => {
  await loadConstellations();
  createDialog.showModal();
});
document.getElementById("create-close").addEventListener("click", () => createDialog.close());

document.getElementById("create-form").addEventListener("submit", async (ev) => {
  ev.preventDefault();
  const name = document.getElementById("name").value.trim();
  const constellation = document.getElementById("constellation").value.trim();
  const goal = document.getElementById("goal").value.trim();
  const heartbeatPrompt = document.getElementById("heartbeat-prompt").value.trim();
  const body = { constellation, goal };
  if (name) body.name = name;
  if (heartbeatPrompt) body.heartbeat_prompt = heartbeatPrompt;
  try {
    await api("/api/projects", { method: "POST", body: JSON.stringify(body) });
    ev.target.reset();
    createDialog.close();
    await loadProjects();
  } catch (e) {
    alert(e.message);
  }
});

// ---- List/tile view toggle ----------------------------------------------

const VIEW_KEY = "mfb-project-view";
const viewListBtn = document.getElementById("view-list");
const viewTilesBtn = document.getElementById("view-tiles");

function applyView(view) {
  const tiles = view === "tiles";
  projectsTable.classList.toggle("hidden", tiles);
  projectsTiles.classList.toggle("active", tiles);
  viewListBtn.classList.toggle("active", !tiles);
  viewTilesBtn.classList.toggle("active", tiles);
  localStorage.setItem(VIEW_KEY, view);
}

viewListBtn.addEventListener("click", () => applyView("list"));
viewTilesBtn.addEventListener("click", () => applyView("tiles"));
applyView(localStorage.getItem(VIEW_KEY) || "list");

async function tick() {
  try {
    await Promise.all([loadProjects(), loadLog(), loadHumanTasks()]);
  } catch (e) {
    console.error(e);
  }
}

loadConstellations();
tick();
setInterval(tick, 5000);

// ---- Settings dialog (model selection only — no API keys) --------------

const settingsDialog = document.getElementById("settings-dialog");

async function loadSettings() {
  try {
    const s = await api("/api/settings");
    document.getElementById("conductor-model").value = s.conductor_model;
    document.getElementById("instance-model").value = s.instance_model;
  } catch (e) {
    console.warn("failed to load settings", e);
  }
}

document.getElementById("settings-toggle").addEventListener("click", async () => {
  await loadSettings();
  settingsDialog.showModal();
});
document.getElementById("settings-close").addEventListener("click", () => settingsDialog.close());

document.getElementById("settings-form").addEventListener("submit", async (ev) => {
  ev.preventDefault();
  const conductorModel = document.getElementById("conductor-model").value.trim();
  const instanceModel = document.getElementById("instance-model").value.trim();
  try {
    await api("/api/settings", { method: "POST", body: JSON.stringify({ conductor_model: conductorModel, instance_model: instanceModel }) });
    settingsDialog.close();
  } catch (e) {
    alert(e.message);
  }
});

// ---- Top-level tab strip (Dashboard / Files) ---------------------------

const TAB_KEY = "mfb-active-tab";
const tabButtons = document.querySelectorAll(".tab-btn");
const tabPanels = document.querySelectorAll(".tab-panel");

function applyTab(tab) {
  tabButtons.forEach((btn) => btn.classList.toggle("active", btn.dataset.tab === tab));
  tabPanels.forEach((panel) => panel.classList.toggle("active", panel.id === `tab-panel-${tab}`));
  localStorage.setItem(TAB_KEY, tab);
  if (tab === "files" && window.filesTab) window.filesTab.onShow();
  if (tab === "archetypes" && window.archetypesTab) window.archetypesTab.onShow();
}

tabButtons.forEach((btn) => btn.addEventListener("click", () => applyTab(btn.dataset.tab)));
applyTab(localStorage.getItem(TAB_KEY) || "dashboard");

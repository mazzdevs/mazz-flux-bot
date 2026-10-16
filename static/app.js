const projectsBody = document.getElementById("projects-body");
const projectsTiles = document.getElementById("projects-tiles");
const projectsTable = document.getElementById("projects-table");
const logList = document.getElementById("log-list");
const constellationsList = document.getElementById("constellations");

async function api(path, opts) {
  const res = await fetch(path, {
    headers: { "Content-Type": "application/json" },
    ...opts,
  });
  const body = await res.json().catch(() => ({}));
  if (!res.ok) throw new Error(body.error?.message || body.error || `${res.status} ${res.statusText}`);
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
  const normalized = String(status || "").toLowerCase();
  if (normalized === "running") return "status-running";
  if (["error", "failed", "stopped"].includes(normalized)) return "status-error";
  if (normalized === "done") return "status-done";
  if (["blocked", "starting", "stopping", "provisioning"].includes(normalized)) return "status-blocked";
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
  if (p.archived_at) return `<a href="/project.html?id=${encodeURIComponent(p.id)}">Archived · Open to restore</a>`;
  const canStartProject = !p.automation.heartbeat_enabled;
  return `<div class="project-action-groups"><div class="btn-group">${
    canStartProject
      ? `<button class="btn-secondary" data-act="project-start" data-id="${p.id}" title="Start project heartbeat"><svg class="icon"><use href="/icons.svg#icon-play"></use></svg> Project</button>`
      : `<button class="btn-secondary" data-act="project-pause" data-id="${p.id}" title="Pause project heartbeat"><svg class="icon"><use href="/icons.svg#icon-pause"></use></svg> Project</button>`
  }</div><button class="btn-danger" data-act="delete" data-id="${p.id}" title="Archive project"><svg class="icon"><use href="/icons.svg#icon-trash"></use></svg></button></div>`;
}

function projectLinks(p) {
  const links = [...p.original_jira_tickets, ...p.ec_tickets].map(l => `<a href="${escapeHtml(l.url)}" target="_blank" rel="noopener">${escapeHtml(l.label || l.key || "Ticket")}</a>`).join(" · ");
  const flags = p.launchdarkly_flags || [];
  return `${links} · ${flags.filter(f => f.status?.matches_fully_enabled).length}/${flags.length} flags at target`;
}

async function loadProjects() {
  const [{ projects }, { entries }] = await Promise.all([api(`/api/v2/projects${document.getElementById("show-archived")?.checked ? "?archived=true" : ""}`), api("/api/human-tasks")]);
  const list = projects || [];
  const taskCounts = (entries || [])
    .filter((task) => task.status === "open" && !task.resolved_at)
    .reduce((counts, task) => {
      counts.set(task.project_id, (counts.get(task.project_id) || 0) + 1);
      return counts;
    }, new Map());

  projectsBody.innerHTML = list
    .map((p) => {
      const primary = p.instances.find(r => r.is_primary);
      const instanceCell = `${p.instances.length} instance(s) · ${primary ? escapeHtml(primary.label) : "No primary"}`;
      const primaryStatus = p.instances.find(r => r.is_primary)?.live;
      const instanceStatus = `${p.automation.status} · primary ${primaryStatus?.status || "unavailable"}`;
      return `<tr>
        <td><a class="project-link" href="/project.html?id=${p.id}">${escapeHtml(p.name)}</a></td>
        <td><span class="status-pill ${statusClass(p.status)}">${escapeHtml(({pending:"Pending",in_progress:"In Progress",review:"Review",merged:"Merged",flags_enabled:"Flags Enabled",flags_cleaned:"Flags Cleaned"})[p.status] || p.status)}</span></td>
        <td>${instanceCell}</td>
        <td><span class="status-pill ${statusClass(instanceStatus)}" title="${primaryStatus?.status_source === "unavailable" ? "Live VAPE status unavailable" : "Primary VAPE instance status"}">${escapeHtml(instanceStatus)}</span></td>
        <td class="goal">${escapeHtml(p.description || p.goal)}<br>${projectLinks(p)}</td>
        <td class="note">${escapeHtml(p.automation.last_note || "")}</td>
        <td>${projectActionsHtml(p)}</td>
      </tr>`;
    })
    .join("");

  projectsTiles.innerHTML = list
    .map((p) => {
      const instance = `${p.instances.length} instance(s) · ${p.instances.filter(r => String(r.live.status).toLowerCase() === "running").length} running`;
      const primaryStatus = p.instances.find(r => r.is_primary)?.live;
      const instanceStatus = `${p.automation.status} · primary ${primaryStatus?.status || "unavailable"}`;
      const taskCount = taskCounts.get(p.id) || 0;
      const taskLabel = `${taskCount} human task${taskCount === 1 ? "" : "s"}`;
      const taskBadge = taskCount
        ? `<span class="human-task-badge" aria-label="${taskLabel}" title="${taskLabel}"><svg class="icon" aria-hidden="true"><use href="/icons.svg#icon-alert"></use></svg>${taskCount}</span>`
        : "";
      return `<div class="project-tile">
        <div class="tile-head">
          <a class="project-link" href="/project.html?id=${p.id}"><strong>${escapeHtml(p.name)}</strong></a>
          <div class="tile-badges">
            <span class="status-pill ${statusClass(p.status)}">${escapeHtml(({pending:"Pending",in_progress:"In Progress",review:"Review",merged:"Merged",flags_enabled:"Flags Enabled",flags_cleaned:"Flags Cleaned"})[p.status] || p.status)}</span>
            ${taskBadge}
          </div>
        </div>
        <div class="goal">${escapeHtml(p.description || p.goal)}</div><div>${projectLinks(p)}</div>
        <div class="tile-instance">instance: ${instance} <span class="status-pill ${statusClass(instanceStatus)}">${escapeHtml(instanceStatus)}</span></div>
        <div class="note">${escapeHtml(p.automation.last_note || "")}</div>
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

function bindProjectActions(container) {
  container.addEventListener("click", async (ev) => {
    const btn = ev.target.closest("button[data-act]");
    if (!btn) return;
    const { act, id } = btn.dataset;
    try {
      const { project } = await api(`/api/v2/projects/${id}`);
      const mutation = { method: "POST", body: JSON.stringify({expected_revision:project.revision}) };
      if (act === "project-start") await api(`/api/v2/projects/${id}/start`, mutation);
      if (act === "project-pause") await api(`/api/v2/projects/${id}/pause`, mutation);

      if (act === "delete") {
        if (!confirm("Archive this project? All Flux instances are retained.")) return;
        await api(`/api/v2/projects/${id}/archive`, mutation);
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
const createForm = document.getElementById("create-form");
const createSubmit = document.getElementById("create-submit");
const createSubmitLabel = document.getElementById("create-submit-label");
const createClose = document.getElementById("create-close");
let creatingProject = false;

document.getElementById("create-toggle").addEventListener("click", async () => {
  await loadConstellations();
  createDialog.showModal();
});
createClose.addEventListener("click", () => createDialog.close());

createForm.addEventListener("submit", async (ev) => {
  ev.preventDefault();
  if (creatingProject) return;

  const name = document.getElementById("name").value.trim();
  const constellation = document.getElementById("constellation").value.trim();
  const body = { constellation, description: document.getElementById("description").value, status: document.getElementById("delivery-status").value };
  if (name) body.name = name;

  creatingProject = true;
  createForm.setAttribute("aria-busy", "true");
  createSubmit.disabled = true;
  createSubmit.classList.add("is-loading");
  createSubmitLabel.textContent = "Creating project…";
  createClose.disabled = true;

  try {
    await api("/api/v2/projects", { method: "POST", body: JSON.stringify(body) });
    createForm.reset();
    createDialog.close();
    await loadProjects();
  } catch (e) {
    alert(e.message);
  } finally {
    creatingProject = false;
    createForm.removeAttribute("aria-busy");
    createSubmit.disabled = false;
    createSubmit.classList.remove("is-loading");
    createSubmitLabel.textContent = "Create";
    createClose.disabled = false;
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
    await Promise.all([loadProjects(), loadLog()]);
  } catch (e) {
    console.error(e);
  }
}

loadConstellations();
document.getElementById("show-archived")?.addEventListener("change", tick);
tick();
setInterval(tick, 15000);

// ---- Settings dialog (models + public bot URL; no API keys) ------------

const settingsDialog = document.getElementById("settings-dialog");
const publicUrlStatus = document.getElementById("bot-public-url-status");

function showPublicUrlStatus(settings, error) {
  publicUrlStatus.classList.toggle("error", Boolean(error));
  if (error) {
    publicUrlStatus.textContent = error;
    return;
  }
  const sourceLabels = {
    settings: "Saved override",
    environment: "Environment",
    derived: "Auto-detected",
    unavailable: "Unavailable",
  };
  const source = sourceLabels[settings.bot_public_base_url_source] || settings.bot_public_base_url_source;
  publicUrlStatus.textContent = settings.effective_bot_public_base_url
    ? `${source}: ${settings.effective_bot_public_base_url}`
    : "Unavailable — configure an override before spawning workers that need live bot context.";
}

async function loadSettings() {
  try {
    const s = await api("/api/settings");
    document.getElementById("conductor-model").value = s.conductor_model;
    document.getElementById("instance-model").value = s.instance_model;
    document.getElementById("bot-public-base-url").value = s.bot_public_base_url || "";
    showPublicUrlStatus(s);
  } catch (e) {
    console.warn("failed to load settings", e);
    showPublicUrlStatus({}, e.message);
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
  const botPublicBaseUrl = document.getElementById("bot-public-base-url").value.trim();
  try {
    const settings = await api("/api/settings", {
      method: "POST",
      body: JSON.stringify({
        conductor_model: conductorModel,
        instance_model: instanceModel,
        bot_public_base_url: botPublicBaseUrl,
      }),
    });
    showPublicUrlStatus(settings);
    settingsDialog.close();
  } catch (e) {
    showPublicUrlStatus({}, e.message);
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

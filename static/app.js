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

// ---- Settings dialog --------------------------------------------------

const settingsDialog = document.getElementById("settings-dialog");

function applySettingsStatus(s) {
  const activeLabel = { anthropic: "Anthropic", openrouter: "OpenRouter", none: "none (observe-only)" }[s.active_backend] || s.active_backend;
  document.getElementById("settings-active").textContent = `Active conductor: ${activeLabel}`;

  document.getElementById("anthropic-hint").textContent = s.anthropic_key_set ? `set (${s.anthropic_key_preview})` : "not set";
  document.getElementById("anthropic-model").placeholder = s.anthropic_model;
  document.getElementById("openrouter-hint").textContent = s.openrouter_key_set ? `set (${s.openrouter_key_preview})` : "not set";
  document.getElementById("openrouter-model").placeholder = s.openrouter_model;
}

async function loadSettings() {
  try {
    applySettingsStatus(await api("/api/settings"));
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
  // Only send fields the user actually typed into — an untouched/blank
  // field means "leave unchanged", not "clear this key" (see api.rs's
  // UpdateSettingsRequest doc comment). Model fields are safe to always
  // send since they aren't secrets.
  const body = {};
  const anthropicKey = document.getElementById("anthropic-api-key").value;
  const openrouterKey = document.getElementById("openrouter-api-key").value;
  if (anthropicKey) body.anthropic_api_key = anthropicKey;
  if (openrouterKey) body.openrouter_api_key = openrouterKey;
  const anthropicModel = document.getElementById("anthropic-model").value.trim();
  const openrouterModel = document.getElementById("openrouter-model").value.trim();
  if (anthropicModel) body.anthropic_model = anthropicModel;
  if (openrouterModel) body.openrouter_model = openrouterModel;

  try {
    applySettingsStatus(await api("/api/settings", { method: "POST", body: JSON.stringify(body) }));
    document.getElementById("anthropic-api-key").value = "";
    document.getElementById("openrouter-api-key").value = "";
  } catch (e) {
    alert(e.message);
  }
});

document.querySelectorAll("#settings-dialog button[data-clear]").forEach((btn) => {
  btn.addEventListener("click", async () => {
    const key = btn.dataset.clear;
    if (!confirm(`Clear the ${key === "anthropic_api_key" ? "Anthropic" : "OpenRouter"} key?`)) return;
    try {
      applySettingsStatus(await api("/api/settings", { method: "POST", body: JSON.stringify({ [key]: "" }) }));
    } catch (e) {
      alert(e.message);
    }
  });
});

loadConstellations();
tick();
setInterval(tick, 5000);

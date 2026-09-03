// Archetypes tab: reusable agent personas (name, description, preferred
// model), stored as archetypes/{slug}.md and readable by the conductor every
// heartbeat tick. Kept separate from app.js, same reasoning as files.js.

(function () {
  const tableEl = document.getElementById("archetypes-table");
  const bodyEl = document.getElementById("archetypes-body");
  const tilesEl = document.getElementById("archetypes-tiles");
  const dialog = document.getElementById("archetype-dialog");
  const form = document.getElementById("archetype-form");
  const dialogTitle = document.getElementById("archetype-dialog-title");

  function escapeHtml(s) {
    return String(s ?? "").replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));
  }

  async function api(path, opts) {
    const res = await fetch(path, { headers: { "Content-Type": "application/json" }, ...opts });
    const body = await res.json().catch(() => ({}));
    if (!res.ok) throw new Error(body.error || `${res.status} ${res.statusText}`);
    return body;
  }

  let cache = [];

  function actionsHtml(a) {
    return `<div class="btn-group">
      <button class="btn-secondary" data-edit="${escapeHtml(a.slug)}">Edit</button>
      <button class="btn-danger" data-delete="${escapeHtml(a.slug)}"><svg class="icon"><use href="/icons.svg#icon-trash"></use></svg></button>
    </div>`;
  }

  function render() {
    bodyEl.innerHTML = cache
      .map(
        (a) => `<tr>
          <td><strong>${escapeHtml(a.name)}</strong></td>
          <td><code>${escapeHtml(a.preferred_model)}</code></td>
          <td class="goal">${escapeHtml(a.description)}</td>
          <td>${actionsHtml(a)}</td>
        </tr>`
      )
      .join("");

    tilesEl.innerHTML = cache
      .map(
        (a) => `<div class="project-tile">
          <div class="tile-head"><strong>${escapeHtml(a.name)}</strong></div>
          <div class="tile-instance"><code>${escapeHtml(a.preferred_model)}</code></div>
          <div class="goal">${escapeHtml(a.description)}</div>
          <div class="tile-actions">${actionsHtml(a)}</div>
        </div>`
      )
      .join("");
  }

  async function load() {
    try {
      const { archetypes } = await api("/api/archetypes");
      cache = archetypes || [];
      render();
    } catch (e) {
      bodyEl.innerHTML = `<tr><td colspan="4" class="empty">failed to load archetypes: ${escapeHtml(e.message)}</td></tr>`;
    }
  }

  document.querySelectorAll("#archetypes-body, #archetypes-tiles").forEach((el) => {
    el.addEventListener("click", async (ev) => {
      const editBtn = ev.target.closest("button[data-edit]");
      const deleteBtn = ev.target.closest("button[data-delete]");
      if (editBtn) {
        const a = cache.find((x) => x.slug === editBtn.dataset.edit);
        if (!a) return;
        dialogTitle.textContent = "Edit archetype";
        document.getElementById("archetype-slug").value = a.slug;
        document.getElementById("archetype-name").value = a.name;
        document.getElementById("archetype-model").value = a.preferred_model;
        document.getElementById("archetype-description").value = a.description;
        dialog.showModal();
      } else if (deleteBtn) {
        if (!confirm(`Delete archetype "${deleteBtn.dataset.delete}"?`)) return;
        try {
          await api(`/api/archetypes/${deleteBtn.dataset.delete}`, { method: "DELETE" });
          await load();
        } catch (e) {
          alert(e.message);
        }
      }
    });
  });

  document.getElementById("archetype-create-toggle").addEventListener("click", () => {
    dialogTitle.textContent = "New archetype";
    form.reset();
    document.getElementById("archetype-slug").value = "";
    dialog.showModal();
  });
  document.getElementById("archetype-close").addEventListener("click", () => dialog.close());

  form.addEventListener("submit", async (ev) => {
    ev.preventDefault();
    const slug = document.getElementById("archetype-slug").value;
    const name = document.getElementById("archetype-name").value.trim();
    const preferred_model = document.getElementById("archetype-model").value.trim();
    const description = document.getElementById("archetype-description").value.trim();
    try {
      if (slug) {
        await api(`/api/archetypes/${slug}`, { method: "POST", body: JSON.stringify({ name, description, preferred_model }) });
      } else {
        const body = { name, description };
        if (preferred_model) body.preferred_model = preferred_model;
        await api("/api/archetypes", { method: "POST", body: JSON.stringify(body) });
      }
      dialog.close();
      await load();
    } catch (e) {
      alert(e.message);
    }
  });

  // ---- List/tile view toggle (separate localStorage key from Projects') --

  const VIEW_KEY = "mfb-archetype-view";
  const viewListBtn = document.getElementById("archetype-view-list");
  const viewTilesBtn = document.getElementById("archetype-view-tiles");

  function applyView(view) {
    const tiles = view === "tiles";
    tableEl.classList.toggle("hidden", tiles);
    tilesEl.classList.toggle("active", tiles);
    viewListBtn.classList.toggle("active", !tiles);
    viewTilesBtn.classList.toggle("active", tiles);
    localStorage.setItem(VIEW_KEY, view);
  }

  viewListBtn.addEventListener("click", () => applyView("list"));
  viewTilesBtn.addEventListener("click", () => applyView("tiles"));
  applyView(localStorage.getItem(VIEW_KEY) || "list");

  let loaded = false;
  window.archetypesTab = {
    onShow: async () => {
      if (loaded) return;
      loaded = true;
      await load();
    },
  };
})();

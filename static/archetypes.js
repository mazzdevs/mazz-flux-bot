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
  const dialogDescription = document.getElementById("archetype-dialog-description");
  const modeBadge = document.getElementById("archetype-mode-badge");
  const nameInput = document.getElementById("archetype-name");
  const modelInput = document.getElementById("archetype-model");
  const descriptionInput = document.getElementById("archetype-description");
  const slugInput = document.getElementById("archetype-slug");
  const submitButton = document.getElementById("archetype-submit");
  const submitLabel = document.getElementById("archetype-submit-label");
  const submitIcon = submitButton.querySelector("use");

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

  function openDialog(mode, archetype) {
    const editing = mode === "edit";
    dialog.dataset.mode = mode;
    dialogTitle.textContent = editing ? "Edit archetype" : "New archetype";
    dialogDescription.textContent = editing
      ? `Update how the conductor uses ${archetype.name} for future work.`
      : "Define a reusable specialist the conductor can recommend for focused work.";
    modeBadge.textContent = editing ? "Editing" : "Create";
    submitLabel.textContent = editing ? "Save changes" : "Create archetype";
    submitIcon.setAttribute("href", editing ? "/icons.svg#icon-check" : "/icons.svg#icon-plus");

    if (editing) {
      slugInput.value = archetype.slug;
      nameInput.value = archetype.name;
      modelInput.value = archetype.preferred_model;
      descriptionInput.value = archetype.description;
    } else {
      form.reset();
      slugInput.value = "";
    }

    dialog.showModal();
    requestAnimationFrame(() => nameInput.focus());
  }

  document.querySelectorAll("#archetypes-body, #archetypes-tiles").forEach((el) => {
    el.addEventListener("click", async (ev) => {
      const editBtn = ev.target.closest("button[data-edit]");
      const deleteBtn = ev.target.closest("button[data-delete]");
      if (editBtn) {
        const a = cache.find((x) => x.slug === editBtn.dataset.edit);
        if (!a) return;
        openDialog("edit", a);
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

  document.getElementById("archetype-create-toggle").addEventListener("click", () => openDialog("create"));
  document.getElementById("archetype-close").addEventListener("click", () => dialog.close());
  dialog.addEventListener("click", (ev) => {
    if (ev.target === dialog) dialog.close();
  });

  form.addEventListener("submit", async (ev) => {
    ev.preventDefault();
    const slug = slugInput.value;
    const name = nameInput.value.trim();
    const preferred_model = modelInput.value.trim();
    const description = descriptionInput.value.trim();
    const idleLabel = slug ? "Save changes" : "Create archetype";
    submitButton.disabled = true;
    submitLabel.textContent = slug ? "Saving changes…" : "Creating…";
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
    } finally {
      submitButton.disabled = false;
      submitLabel.textContent = idleLabel;
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

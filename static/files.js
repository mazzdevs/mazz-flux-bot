// File browser tab: read/edit the state directory's json/markdown files
// through /api/files. Kept separate from app.js.

(function () {
  const listEl = document.getElementById("files-list");
  const editorEl = document.getElementById("files-editor");
  const breadcrumbEl = document.getElementById("files-breadcrumb");
  const refreshBtn = document.getElementById("files-refresh");

  let currentDir = "";
  let openFile = null; // { path, content, size, modified_at }
  let dirty = false;

  function escapeHtml(s) {
    return String(s ?? "").replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));
  }

  async function api(path, opts) {
    const res = await fetch(path, { headers: { "Content-Type": "application/json" }, ...opts });
    const body = await res.json().catch(() => ({}));
    if (!res.ok) throw new Error(body.error || `${res.status} ${res.statusText}`);
    return body;
  }

  function renderBreadcrumb(path) {
    const parts = path ? path.split("/").filter(Boolean) : [];
    let acc = [];
    const crumbs = [`<a href="#" data-nav=""><svg class="icon"><use href="/icons.svg#icon-folder"></use></svg> root</a>`];
    for (const part of parts) {
      acc.push(part);
      crumbs.push(`<a href="#" data-nav="${escapeHtml(acc.join("/"))}">${escapeHtml(part)}</a>`);
    }
    breadcrumbEl.innerHTML = crumbs.join('<svg class="icon"><use href="/icons.svg#icon-chevron-right"></use></svg>');
  }

  function fileIconId(name) {
    if (name.endsWith(".json")) return "icon-file-json";
    if (name.endsWith(".md")) return "icon-file-text";
    return "icon-file";
  }

  function fmtSize(n) {
    if (n < 1024) return `${n} B`;
    if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
    return `${(n / (1024 * 1024)).toFixed(1)} MB`;
  }

  async function loadDir(path) {
    currentDir = path;
    renderBreadcrumb(path);
    const result = await api(`/api/files?path=${encodeURIComponent(path)}`);
    if (result.type !== "dir") return;
    listEl.innerHTML = (result.entries || [])
      .map((e) => {
        const iconId = e.is_dir ? "icon-folder" : fileIconId(e.name);
        const meta = e.is_dir ? "" : `<span class="file-meta">${fmtSize(e.size)}</span>`;
        return `<li><a href="#" data-open="${escapeHtml(e.path)}" data-is-dir="${e.is_dir}"><svg class="icon"><use href="/icons.svg#${iconId}"></use></svg> ${escapeHtml(e.name)}</a>${meta}</li>`;
      })
      .join("") || `<li class="empty">Empty directory.</li>`;
  }

  function renderEditor() {
    if (!openFile) {
      editorEl.innerHTML = `<div class="files-editor-empty">Select a file to view or edit it.</div>`;
      return;
    }
    const isJson = openFile.path.endsWith(".json");
    editorEl.innerHTML = `
      <div class="files-editor-head">
        <code>${escapeHtml(openFile.path)}</code>
        <span class="file-meta">${fmtSize(openFile.size)} · modified ${escapeHtml(openFile.modified_at || "unknown")}</span>
      </div>
      <textarea id="files-editor-textarea" spellcheck="false">${escapeHtml(openFile.content)}</textarea>
      <div class="files-editor-actions">
        ${isJson ? `<button class="btn-secondary" id="files-format" type="button">Format JSON</button>` : ""}
        <button class="btn-secondary" id="files-revert" type="button">Revert</button>
        <button class="btn-danger" id="files-delete" type="button"><svg class="icon"><use href="/icons.svg#icon-trash"></use></svg> Delete</button>
        <button class="btn" id="files-save" type="button">Save</button>
      </div>
      <div id="files-status" class="files-status"></div>
    `;

    const textarea = document.getElementById("files-editor-textarea");
    textarea.addEventListener("input", () => {
      dirty = true;
      setStatus("unsaved changes", false);
    });

    document.getElementById("files-revert").addEventListener("click", async () => {
      await openFileAt(openFile.path);
    });

    document.getElementById("files-delete").addEventListener("click", async () => {
      if (!confirm(`Delete ${openFile.path}? This cannot be undone.`)) return;
      try {
        await api(`/api/files?path=${encodeURIComponent(openFile.path)}`, { method: "DELETE" });
        openFile = null;
        dirty = false;
        renderEditor();
        await loadDir(currentDir);
      } catch (e) {
        setStatus(e.message, true);
      }
    });

    document.getElementById("files-save").addEventListener("click", async () => {
      try {
        await api(`/api/files?path=${encodeURIComponent(openFile.path)}`, {
          method: "PUT",
          body: JSON.stringify({ content: textarea.value }),
        });
        openFile.content = textarea.value;
        dirty = false;
        setStatus("saved", false);
        await loadDir(currentDir);
      } catch (e) {
        setStatus(e.message, true);
      }
    });

    const formatBtn = document.getElementById("files-format");
    if (formatBtn) {
      formatBtn.addEventListener("click", () => {
        try {
          textarea.value = JSON.stringify(JSON.parse(textarea.value), null, 2);
          dirty = true;
          setStatus("formatted (not yet saved)", false);
        } catch (e) {
          setStatus(`invalid JSON: ${e.message}`, true);
        }
      });
    }
  }

  function setStatus(msg, isError) {
    const el = document.getElementById("files-status");
    if (!el) return;
    el.textContent = msg;
    el.classList.toggle("error", !!isError);
  }

  async function openFileAt(path) {
    if (dirty && !confirm("Discard unsaved changes?")) return;
    const result = await api(`/api/files?path=${encodeURIComponent(path)}`);
    if (result.type !== "file") return;
    openFile = result;
    dirty = false;
    renderEditor();
  }

  listEl.addEventListener("click", async (ev) => {
    const link = ev.target.closest("a[data-open]");
    if (!link) return;
    ev.preventDefault();
    const path = link.dataset.open;
    if (link.dataset.isDir === "true") {
      await loadDir(path);
    } else {
      await openFileAt(path);
    }
  });

  breadcrumbEl.addEventListener("click", async (ev) => {
    const link = ev.target.closest("a[data-nav]");
    if (!link) return;
    ev.preventDefault();
    await loadDir(link.dataset.nav);
  });

  refreshBtn.addEventListener("click", () => loadDir(currentDir));

  let loaded = false;
  window.filesTab = {
    onShow: async () => {
      if (loaded) return;
      loaded = true;
      try {
        await loadDir("");
      } catch (e) {
        listEl.innerHTML = `<li class="empty">${escapeHtml(e.message)}</li>`;
      }
    },
  };
})();

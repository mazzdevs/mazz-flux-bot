//! File-backed storage: replaces the old sqlite DB with plain JSON + markdown
//! files under a directory (see `AppState`/`main.rs` for where the root is
//! chosen — by default a sibling directory next to this repo, so it can be
//! its own separate git repo and survive this checkout being thrown away).
//!
//! Every write is read-modify-write-rename (write to a `.tmp` file, then
//! `fs::rename` over the real path) for crash-safety, serialized behind one
//! `tokio::sync::Mutex` — this tool has at most a handful of concurrent
//! callers (the HTTP server + one heartbeat loop), so a single global write
//! lock is simpler than per-file locking and cheap enough.
//!
//! Layout:
//! ```text
//! <root>/
//!   projects/<project_id>.json
//!   notes/<project_id>/<rfc3339>__<uuid8>.md
//!   human_tasks/<project_id>/<task_id>.json
//!   action_log/<yyyy-mm-dd>.ndjson
//!   settings.json
//!   cache/instances.json
//! ```

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use tokio::fs;
use tokio::sync::Mutex;

use crate::models::{ActionLogEntry, CreateProjectRequest, HumanTask, Project, ProjectNote, ProjectStatus};

pub struct Store {
    root: PathBuf,
    lock: Mutex<()>,
}

/// One entry in a directory listing returned by `Store::browse`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct FileEntry {
    pub name: String,
    /// Root-relative, forward-slash path — what callers should pass back
    /// into `browse`/`read_file`/`write_file`/`delete_file`.
    pub path: String,
    pub is_dir: bool,
    pub size: u64,
    pub modified_at: Option<String>,
}

/// Result of browsing a path in the store — either a directory listing or
/// a single file's content.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BrowseResult {
    Dir { path: String, entries: Vec<FileEntry> },
    File { path: String, content: String, size: u64, modified_at: Option<String> },
}

/// Files larger than this are refused for reading/browsing through the file
/// browser — this store only ever holds small JSON/markdown records, so
/// anything bigger is either a mistake or not something the textarea-based
/// editor should be pointed at.
const MAX_BROWSABLE_FILE_BYTES: u64 = 2 * 1024 * 1024;

fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}

fn short_uuid() -> String {
    uuid::Uuid::new_v4().to_string()[..8].to_string()
}

/// Writes are staged to a sibling `.tmp` file then renamed into place so a
/// crash mid-write never leaves a half-written file at the real path.
async fn write_atomic(path: &Path, contents: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).await.with_context(|| format!("creating {}", parent.display()))?;
    }
    let mut tmp_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("tmp").to_string();
    tmp_name.push_str(".tmp");
    let tmp = path.with_file_name(tmp_name);
    fs::write(&tmp, contents).await.with_context(|| format!("writing {}", tmp.display()))?;
    fs::rename(&tmp, path).await.with_context(|| format!("renaming into {}", path.display()))?;
    Ok(())
}

async fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<Option<T>> {
    match fs::read_to_string(path).await {
        Ok(s) => Ok(Some(serde_json::from_str(&s).with_context(|| format!("parsing {}", path.display()))?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

async fn write_json<T: serde::Serialize>(path: &Path, value: &T) -> Result<()> {
    let s = serde_json::to_string_pretty(value)?;
    write_atomic(path, &s).await
}

/// Lists files matching `*.json` directly inside `dir` (not recursive) and
/// parses each. Missing directory is treated as "no entries" rather than an
/// error (nothing has been written there yet).
async fn read_json_dir<T: serde::de::DeserializeOwned>(dir: &Path) -> Result<Vec<T>> {
    let mut out = Vec::new();
    let mut entries = match fs::read_dir(dir).await {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(e) => return Err(e).with_context(|| format!("reading dir {}", dir.display())),
    };
    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        if let Some(v) = read_json(&path).await? {
            out.push(v);
        }
    }
    Ok(out)
}

impl Store {
    /// Opens (creating if needed) a file store rooted at `root`.
    pub async fn open(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        fs::create_dir_all(&root).await.with_context(|| format!("creating data dir {}", root.display()))?;
        Ok(Self { root, lock: Mutex::new(()) })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn project_path(&self, id: &str) -> PathBuf {
        self.root.join("projects").join(format!("{id}.json"))
    }
    fn notes_dir(&self, project_id: &str) -> PathBuf {
        self.root.join("notes").join(project_id)
    }
    fn human_tasks_dir(&self, project_id: &str) -> PathBuf {
        self.root.join("human_tasks").join(project_id)
    }
    fn action_log_path(&self, day: &str) -> PathBuf {
        self.root.join("action_log").join(format!("{day}.ndjson"))
    }
    fn settings_path(&self) -> PathBuf {
        self.root.join("settings.json")
    }
    fn agent_prompt_path(&self, name: &str) -> PathBuf {
        self.root.join("agent_prompts").join(format!("{name}.md"))
    }
    fn instance_cache_path(&self) -> PathBuf {
        self.root.join("cache").join("instances.json")
    }

    // ---- Projects ---------------------------------------------------------

    pub async fn create_project(&self, req: CreateProjectRequest) -> Result<Project> {
        let _guard = self.lock.lock().await;
        let id = uuid::Uuid::new_v4().to_string();
        let ts = now();
        let project = Project {
            id: id.clone(),
            name: req.name,
            goal: req.goal,
            constellation: req.constellation.unwrap_or_else(|| "back-office".to_string()),
            status: ProjectStatus::Draft.as_str().to_string(),
            vape_instance_id: None,
            heartbeat_enabled: false,
            heartbeat_interval_secs: req.heartbeat_interval_secs.unwrap_or(crate::heartbeat::DEFAULT_HEARTBEAT_INTERVAL_SECS),
            last_note: None,
            created_at: ts.clone(),
            updated_at: ts,
            last_heartbeat_at: None,
        };
        write_json(&self.project_path(&id), &project).await?;
        Ok(project)
    }

    pub async fn list_projects(&self) -> Result<Vec<Project>> {
        let mut projects: Vec<Project> = read_json_dir(&self.root.join("projects")).await?;
        projects.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(projects)
    }

    pub async fn get_project(&self, id: &str) -> Result<Option<Project>> {
        read_json(&self.project_path(id)).await
    }

    pub async fn list_running_projects(&self) -> Result<Vec<Project>> {
        let projects = self.list_projects().await?;
        Ok(projects.into_iter().filter(|p| p.heartbeat_enabled && p.status == "running").collect())
    }

    pub async fn delete_project(&self, id: &str) -> Result<()> {
        let _guard = self.lock.lock().await;
        let path = self.project_path(id);
        match fs::remove_file(&path).await {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e).with_context(|| format!("removing {}", path.display())),
        }
    }

    async fn update_project(&self, id: &str, f: impl FnOnce(&mut Project)) -> Result<()> {
        let _guard = self.lock.lock().await;
        let path = self.project_path(id);
        let mut project: Project = read_json(&path).await?.ok_or_else(|| anyhow::anyhow!("project {id} not found"))?;
        f(&mut project);
        project.updated_at = now();
        write_json(&path, &project).await
    }

    pub async fn set_project_instance(&self, id: &str, instance_id: &str) -> Result<()> {
        self.update_project(id, |p| p.vape_instance_id = Some(instance_id.to_string())).await
    }

    pub async fn set_project_status(&self, id: &str, status: ProjectStatus, note: Option<&str>) -> Result<()> {
        let note = note.map(|s| s.to_string());
        self.update_project(id, |p| {
            p.status = status.as_str().to_string();
            p.last_note = note;
        })
        .await
    }

    /// Like `set_project_status` but leaves `last_note` untouched — see the
    /// original sqlite-era doc comment on this in db.rs's history for why
    /// start/pause use this instead.
    pub async fn set_project_status_only(&self, id: &str, status: ProjectStatus) -> Result<()> {
        self.update_project(id, |p| p.status = status.as_str().to_string()).await
    }

    pub async fn set_heartbeat_enabled(&self, id: &str, enabled: bool) -> Result<()> {
        self.update_project(id, |p| p.heartbeat_enabled = enabled).await
    }

    /// Per-project heartbeat cadence override — minimum 5 seconds, mostly to
    /// stop a fat-fingered `0` from turning a project into a busy-loop.
    pub async fn set_heartbeat_interval(&self, id: &str, interval_secs: u64) -> Result<()> {
        let interval_secs = interval_secs.max(5);
        self.update_project(id, |p| p.heartbeat_interval_secs = interval_secs).await
    }

    pub async fn touch_heartbeat(&self, id: &str, note: Option<&str>) -> Result<()> {
        let ts = now();
        let note = note.map(|s| s.to_string());
        self.update_project(id, |p| {
            p.last_heartbeat_at = Some(ts.clone());
            if let Some(n) = note {
                p.last_note = Some(n);
            }
        })
        .await
    }

    // ---- Action log (append-only, day-sharded ndjson) ----------------------

    pub async fn log_action(
        &self,
        project_id: Option<&str>,
        instance_id: Option<&str>,
        action: &str,
        detail: Option<&serde_json::Value>,
        result: Option<&str>,
        error: Option<&str>,
    ) -> Result<()> {
        let _guard = self.lock.lock().await;
        let ts = now();
        let day = &ts[..10]; // "2026-09-03T..." -> "2026-09-03"
        let entry = ActionLogEntry {
            id: uuid::Uuid::new_v4().to_string(),
            project_id: project_id.map(|s| s.to_string()),
            instance_id: instance_id.map(|s| s.to_string()),
            action: action.to_string(),
            detail: detail.map(|d| d.to_string()),
            result: result.map(|s| s.to_string()),
            error: error.map(|s| s.to_string()),
            created_at: ts.clone(),
        };
        let line = serde_json::to_string(&entry)? + "\n";
        let path = self.action_log_path(day);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).await?;
        }
        // Plain append — no atomic-rename needed for an append-only log; a
        // partial line from a crash mid-write is an acceptable, self-evident
        // failure mode here (unlike the full-file rewrites elsewhere).
        use tokio::io::AsyncWriteExt;
        let mut f = fs::OpenOptions::new().create(true).append(true).open(&path).await?;
        f.write_all(line.as_bytes()).await?;
        Ok(())
    }

    /// Reads the most recent `limit` entries, optionally filtered to one
    /// project. Scans day-shard files newest-first; each file's lines are
    /// newest-last so read within a file in reverse too.
    pub async fn list_action_log(&self, project_id: Option<&str>, limit: i64) -> Result<Vec<ActionLogEntry>> {
        let dir = self.root.join("action_log");
        let mut days: Vec<String> = match fs::read_dir(&dir).await {
            Ok(mut entries) => {
                let mut names = Vec::new();
                while let Some(e) = entries.next_entry().await? {
                    if let Some(name) = e.file_name().to_str() {
                        if let Some(day) = name.strip_suffix(".ndjson") {
                            names.push(day.to_string());
                        }
                    }
                }
                names
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(e).with_context(|| format!("reading dir {}", dir.display())),
        };
        days.sort();
        days.reverse();

        let mut out = Vec::new();
        for day in days {
            if out.len() as i64 >= limit {
                break;
            }
            let content = fs::read_to_string(self.action_log_path(&day)).await.unwrap_or_default();
            let mut lines: Vec<&str> = content.lines().collect();
            lines.reverse();
            for line in lines {
                if line.trim().is_empty() {
                    continue;
                }
                let entry: ActionLogEntry = match serde_json::from_str(line) {
                    Ok(e) => e,
                    Err(_) => continue,
                };
                if let Some(pid) = project_id {
                    if entry.project_id.as_deref() != Some(pid) {
                        continue;
                    }
                }
                out.push(entry);
                if out.len() as i64 >= limit {
                    break;
                }
            }
        }
        Ok(out)
    }

    // ---- Instance list cache -----------------------------------------------

    pub async fn cache_instance_list(&self, raw_json: &str) -> Result<()> {
        let _guard = self.lock.lock().await;
        let payload = serde_json::json!({ "raw_json": raw_json, "fetched_at": now() });
        write_json(&self.instance_cache_path(), &payload).await
    }

    pub async fn get_cached_instance_list(&self) -> Result<Option<(String, String)>> {
        let v: Option<serde_json::Value> = read_json(&self.instance_cache_path()).await?;
        Ok(v.and_then(|v| {
            let raw = v.get("raw_json")?.as_str()?.to_string();
            let fetched_at = v.get("fetched_at")?.as_str()?.to_string();
            Some((raw, fetched_at))
        }))
    }

    // ---- Agent prompts (user-authored, read-only from the bot's side) -----

    /// Reads `agent_prompts/{name}.md` if it exists — e.g. `"validation"` for
    /// the extra criteria injected into the conductor's system prompt before
    /// it's allowed to mark a project done (see `heartbeat::DecideNode`).
    /// `Ok(None)` (not an error) if the file doesn't exist — this is an
    /// opt-in customization point, most projects won't have one. Edited
    /// directly through the Files tab; no dedicated write endpoint.
    pub async fn read_agent_prompt(&self, name: &str) -> Result<Option<String>> {
        match fs::read_to_string(self.agent_prompt_path(name)).await {
            Ok(s) => Ok(Some(s)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e).with_context(|| format!("reading agent_prompts/{name}.md")),
        }
    }

    // ---- Settings (flat key/value map) -------------------------------------

    async fn read_settings(&self) -> Result<serde_json::Map<String, serde_json::Value>> {
        let v: Option<serde_json::Value> = read_json(&self.settings_path()).await?;
        Ok(match v {
            Some(serde_json::Value::Object(m)) => m,
            _ => serde_json::Map::new(),
        })
    }

    pub async fn get_setting(&self, key: &str) -> Result<Option<String>> {
        let map = self.read_settings().await?;
        Ok(map.get(key).and_then(|v| v.as_str()).map(|s| s.to_string()))
    }

    /// An empty value deletes the key (settings UI's "clear this key" action)
    /// — mirrors the old sqlite behavior where a stored empty string was
    /// treated as "not set" everywhere it's read.
    pub async fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        let _guard = self.lock.lock().await;
        let mut map = self.read_settings().await?;
        if value.is_empty() {
            map.remove(key);
        } else {
            map.insert(key.to_string(), serde_json::Value::String(value.to_string()));
        }
        write_json(&self.settings_path(), &serde_json::Value::Object(map)).await
    }

    // ---- Human tasks --------------------------------------------------------

    pub async fn create_human_task(&self, project_id: &str, description: &str) -> Result<HumanTask> {
        let _guard = self.lock.lock().await;
        let task = HumanTask {
            id: uuid::Uuid::new_v4().to_string(),
            project_id: project_id.to_string(),
            description: description.to_string(),
            status: "open".to_string(),
            created_at: now(),
            resolved_at: None,
        };
        let path = self.human_tasks_dir(project_id).join(format!("{}.json", task.id));
        write_json(&path, &task).await?;
        Ok(task)
    }

    /// `project_id = None` lists across all projects (the dashboard-wide panel).
    pub async fn list_human_tasks(&self, project_id: Option<&str>, open_only: bool) -> Result<Vec<HumanTask>> {
        let dirs: Vec<PathBuf> = match project_id {
            Some(pid) => vec![self.human_tasks_dir(pid)],
            None => {
                let root = self.root.join("human_tasks");
                let mut dirs = Vec::new();
                match fs::read_dir(&root).await {
                    Ok(mut entries) => {
                        while let Some(e) = entries.next_entry().await? {
                            if e.file_type().await?.is_dir() {
                                dirs.push(e.path());
                            }
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e).with_context(|| format!("reading dir {}", root.display())),
                }
                dirs
            }
        };

        let mut tasks = Vec::new();
        for dir in dirs {
            let mut found: Vec<HumanTask> = read_json_dir(&dir).await?;
            if open_only {
                found.retain(|t| t.status == "open");
            }
            tasks.append(&mut found);
        }
        tasks.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(tasks)
    }

    /// Human task ids are unique uuids but we don't index project_id ->
    /// task_id anywhere else, so resolving by id alone means scanning every
    /// project's task directory. Fine at this tool's scale.
    pub async fn resolve_human_task(&self, id: &str) -> Result<()> {
        let _guard = self.lock.lock().await;
        let root = self.root.join("human_tasks");
        let mut entries = match fs::read_dir(&root).await {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(anyhow::anyhow!("human task {id} not found")),
            Err(e) => return Err(e).with_context(|| format!("reading dir {}", root.display())),
        };
        while let Some(dir_entry) = entries.next_entry().await? {
            if !dir_entry.file_type().await?.is_dir() {
                continue;
            }
            let path = dir_entry.path().join(format!("{id}.json"));
            if let Some(mut task) = read_json::<HumanTask>(&path).await? {
                task.status = "resolved".to_string();
                task.resolved_at = Some(now());
                write_json(&path, &task).await?;
                return Ok(());
            }
        }
        Err(anyhow::anyhow!("human task {id} not found"))
    }

    // ---- Project notes (conductor-authored markdown) -------------------------

    pub async fn add_project_note(&self, project_id: &str, content: &str) -> Result<ProjectNote> {
        let _guard = self.lock.lock().await;
        let ts = now();
        // Filename uses epoch millis (filesystem-safe, sorts lexically by
        // time) plus a short uuid suffix to disambiguate same-millisecond
        // notes. The real rfc3339 created_at is embedded as a leading HTML
        // comment in the file so it round-trips exactly.
        let millis = chrono::Utc::now().timestamp_millis();
        let id = format!("{millis}__{}", short_uuid());
        let note = ProjectNote { id: id.clone(), project_id: project_id.to_string(), content: content.to_string(), created_at: ts.clone() };
        let path = self.notes_dir(project_id).join(format!("{id}.md"));
        let file_body = format!("<!-- created_at: {ts} -->\n{content}");
        write_atomic(&path, &file_body).await?;
        Ok(note)
    }

    pub async fn list_project_notes(&self, project_id: &str) -> Result<Vec<ProjectNote>> {
        let dir = self.notes_dir(project_id);
        let mut out = Vec::new();
        let mut entries = match fs::read_dir(&dir).await {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
            Err(e) => return Err(e).with_context(|| format!("reading dir {}", dir.display())),
        };
        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("md") {
                continue;
            }
            let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string();
            let raw = fs::read_to_string(&path).await.with_context(|| format!("reading {}", path.display()))?;
            let (created_at, content) = split_created_at_comment(&raw);
            out.push(ProjectNote { id: stem, project_id: project_id.to_string(), content, created_at });
        }
        out.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(out)
    }

    // ---- File browser (raw filesystem access for the web UI) --------------

    /// Resolves a caller-supplied relative path against the store root,
    /// rejecting anything that would escape it (`..` components, absolute
    /// paths). Returns the resolved absolute path — not guaranteed to
    /// exist yet (callers writing a new file rely on that).
    fn resolve_path(&self, rel: &str) -> Result<PathBuf> {
        let rel = rel.trim_start_matches('/');
        let mut resolved = self.root.clone();
        for component in Path::new(rel).components() {
            match component {
                std::path::Component::Normal(part) => {
                    // Blocks dotfile/dir access through this API entirely —
                    // most importantly the state repo's own `.git`, which
                    // must never be readable/writable/deletable this way.
                    if part.to_str().is_some_and(|s| s.starts_with('.')) {
                        anyhow::bail!("invalid path: {rel}");
                    }
                    resolved.push(part);
                }
                std::path::Component::CurDir => {}
                std::path::Component::ParentDir | std::path::Component::RootDir | std::path::Component::Prefix(_) => {
                    anyhow::bail!("invalid path: {rel}");
                }
            }
        }
        // Defends against symlink escape too: canonicalize what already
        // exists and confirm it's still under root. A path that doesn't
        // exist yet (new file write) can't be canonicalized — checked at
        // its nearest existing ancestor instead.
        let mut check = resolved.clone();
        loop {
            if let Ok(canon) = check.canonicalize() {
                let canon_root = self.root.canonicalize().unwrap_or_else(|_| self.root.clone());
                if !canon.starts_with(&canon_root) {
                    anyhow::bail!("path escapes store root: {rel}");
                }
                break;
            }
            match check.parent() {
                Some(p) if p != check => check = p.to_path_buf(),
                _ => break,
            }
        }
        Ok(resolved)
    }

    /// Root-relative, forward-slash path for display/API purposes.
    fn display_path(&self, abs: &Path) -> String {
        abs.strip_prefix(&self.root).unwrap_or(abs).to_string_lossy().replace('\\', "/")
    }

    /// Browses `rel` — a directory listing if it's a dir (or the root, for
    /// `rel == ""`), or a single file's content if it's a file.
    pub async fn browse(&self, rel: &str) -> Result<BrowseResult> {
        let abs = self.resolve_path(rel)?;
        let meta = fs::metadata(&abs).await.with_context(|| format!("reading {}", abs.display()))?;

        if meta.is_dir() {
            let mut entries = Vec::new();
            let mut rd = fs::read_dir(&abs).await.with_context(|| format!("reading dir {}", abs.display()))?;
            while let Some(entry) = rd.next_entry().await? {
                let name = entry.file_name().to_string_lossy().to_string();
                // Hide dotfiles/dirs (notably `.git`, the state repo's own
                // git metadata) — nothing a user should be poking at
                // through this editor.
                if name.starts_with('.') {
                    continue;
                }
                let entry_meta = entry.metadata().await?;
                let path = entry.path();
                entries.push(FileEntry {
                    name: entry.file_name().to_string_lossy().to_string(),
                    path: self.display_path(&path),
                    is_dir: entry_meta.is_dir(),
                    size: entry_meta.len(),
                    modified_at: modified_rfc3339(&entry_meta),
                });
            }
            entries.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.cmp(&b.name)));
            Ok(BrowseResult::Dir { path: self.display_path(&abs), entries })
        } else {
            if meta.len() > MAX_BROWSABLE_FILE_BYTES {
                anyhow::bail!("file too large to browse ({} bytes, max {})", meta.len(), MAX_BROWSABLE_FILE_BYTES);
            }
            let content = fs::read_to_string(&abs).await.with_context(|| format!("reading {} (not valid UTF-8?)", abs.display()))?;
            Ok(BrowseResult::File { path: self.display_path(&abs), content, size: meta.len(), modified_at: modified_rfc3339(&meta) })
        }
    }

    /// Overwrites (or creates) a file at `rel` with `content`, creating
    /// parent directories as needed. Rejects paths that resolve to an
    /// existing directory.
    pub async fn write_file(&self, rel: &str, content: &str) -> Result<()> {
        let _guard = self.lock.lock().await;
        let abs = self.resolve_path(rel)?;
        if let Ok(meta) = fs::metadata(&abs).await {
            if meta.is_dir() {
                anyhow::bail!("{rel} is a directory");
            }
        }
        write_atomic(&abs, content).await
    }

    /// Deletes a single file at `rel`. Refuses to delete directories —
    /// this is a low-risk file editor, not a bulk-delete tool.
    pub async fn delete_file(&self, rel: &str) -> Result<()> {
        let _guard = self.lock.lock().await;
        let abs = self.resolve_path(rel)?;
        let meta = fs::metadata(&abs).await.with_context(|| format!("reading {}", abs.display()))?;
        if meta.is_dir() {
            anyhow::bail!("{rel} is a directory, refusing to delete");
        }
        fs::remove_file(&abs).await.with_context(|| format!("removing {}", abs.display()))
    }
}

fn modified_rfc3339(meta: &std::fs::Metadata) -> Option<String> {
    meta.modified().ok().map(|t| chrono::DateTime::<chrono::Utc>::from(t).to_rfc3339())
}

/// Splits the leading `<!-- created_at: ... -->` line written by
/// `add_project_note` back into `(created_at, remaining_content)`. Falls
/// back to an empty timestamp (sorts last) if a note file lacks the marker.
fn split_created_at_comment(raw: &str) -> (String, String) {
    if let Some(rest) = raw.strip_prefix("<!-- created_at: ") {
        if let Some(end) = rest.find(" -->\n") {
            let created_at = rest[..end].to_string();
            let content = rest[end + 5..].to_string();
            return (created_at, content);
        }
    }
    (String::new(), raw.to_string())
}

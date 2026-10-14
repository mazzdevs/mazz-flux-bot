use super::*;
use crate::projects::*;
use sha2::{Digest, Sha256};

#[derive(Debug, serde::Serialize)]
pub struct MigrationReport {
    pub dry_run: bool,
    pub records: Vec<MigrationRecord>,
    pub errors: Vec<String>,
}
#[derive(Debug, serde::Serialize)]
pub struct MigrationRecord {
    pub project_id: String,
    pub migrated: bool,
    pub before_sha256: String,
    pub after_sha256: String,
}

impl Store {
    pub async fn open_for_migration(root: impl Into<PathBuf>) -> Result<Self> {
        Ok(Self {
            root: root.into(),
            lock: Mutex::new(()),
            project_locks: Mutex::new(HashMap::new()),
        })
    }
    /// One bot process owns a state directory. Never run two writers against it.
    pub async fn project_guard(&self, id: &str) -> Result<tokio::sync::OwnedMutexGuard<()>> {
        validate_id(id)?;
        let lock = self
            .project_locks
            .lock()
            .await
            .entry(id.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone();
        Ok(lock.lock_owned().await)
    }
    pub(super) fn relationship_path(&self, id: &str) -> PathBuf {
        self.root
            .join("project_instances")
            .join(format!("{id}.json"))
    }
    fn user_note_path(&self, project: &str, note: &str) -> PathBuf {
        self.root
            .join("user_notes")
            .join(project)
            .join(format!("{note}.json"))
    }
    pub async fn relationships(&self, id: &str) -> Result<Relationships> {
        validate_id(id)?;
        Ok(read_json(&self.relationship_path(id))
            .await?
            .unwrap_or_else(|| Relationships {
                project_id: id.into(),
                ..Default::default()
            }))
    }
    pub async fn require_project(&self, id: &str) -> Result<Project> {
        self.get_project(id)
            .await?
            .ok_or_else(|| failure("not_found", "project not found"))
    }
    pub async fn require_relationship(&self, pid: &str, rid: &str) -> Result<Relationship> {
        self.require_project(pid).await?;
        validate_id(rid)?;
        self.relationships(pid)
            .await?
            .instances
            .into_iter()
            .find(|r| r.id == rid)
            .ok_or_else(|| failure("not_found", "relationship not found in project"))
    }
    fn check_revision(p: &Project, expected: u64) -> Result<()> {
        if p.tracking.revision != expected {
            return Err(failure(
                "stale_revision",
                format!(
                    "expected revision {expected}, current revision {}",
                    p.tracking.revision
                ),
            ));
        }
        Ok(())
    }
    pub async fn check_writable(&self, id: &str, expected: u64) -> Result<Project> {
        let p = self.require_project(id).await?;
        Self::check_revision(&p, expected)?;
        if p.tracking.archived_at.is_some() {
            return Err(failure(
                "archived",
                "restore the archived project before editing",
            ));
        }
        Ok(p)
    }
    async fn save_project_revision(&self, p: &mut Project) -> Result<()> {
        p.tracking.revision += 1;
        p.updated_at = timestamp();
        write_json(&self.project_path(&p.id), p).await
    }
    pub async fn mutate_project(
        &self,
        id: &str,
        expected: u64,
        f: impl FnOnce(&mut Project) -> Result<()>,
    ) -> Result<Project> {
        let _lock = self.lock.lock().await;
        let mut p = self.check_writable(id, expected).await?;
        f(&mut p)?;
        let all = self.relationships(id).await?;
        Self::pause_without_direction(&mut p, &all);
        self.save_project_revision(&mut p).await?;
        Ok(p)
    }
    fn pause_without_direction(p: &mut Project, all: &Relationships) {
        if p.heartbeat_enabled && effective_goal(p, all.primary()).trim().is_empty() {
            p.heartbeat_enabled = false;
            p.status = "paused".into();
        }
    }
    pub async fn set_archived(&self, id: &str, expected: u64, archived: bool) -> Result<()> {
        let _lock = self.lock.lock().await;
        let mut p = self.require_project(id).await?;
        Self::check_revision(&p, expected)?;
        p.tracking.archived_at = if archived { Some(timestamp()) } else { None };
        p.heartbeat_enabled = false;
        p.status = "paused".into();
        self.save_project_revision(&mut p).await
    }
    pub async fn purge_project(&self, id: &str, expected: u64, confirmation: &str) -> Result<()> {
        let _lock = self.lock.lock().await;
        let p = self.require_project(id).await?;
        Self::check_revision(&p, expected)?;
        if p.tracking.archived_at.is_none() || confirmation != p.name {
            return Err(failure(
                "confirmation_required",
                "archive first and confirm the exact project name",
            ));
        }
        // Keep the append-only global action log, including the purge audit event.
        for path in [
            self.relationship_path(id),
            self.kanban_path(id),
            self.memory_path(id),
        ] {
            if fs::try_exists(&path).await? {
                fs::remove_file(path).await?;
            }
        }
        for dir in [
            self.notes_dir(id),
            self.human_tasks_dir(id),
            self.root.join("user_notes").join(id),
        ] {
            if fs::try_exists(&dir).await? {
                fs::remove_dir_all(dir).await?;
            }
        }
        fs::remove_file(self.project_path(id)).await?;
        Ok(())
    }
    pub async fn add_relationship(
        &self,
        pid: &str,
        expected: u64,
        r: Relationship,
        primary: bool,
    ) -> Result<Relationship> {
        let _lock = self.lock.lock().await;
        let mut p = self.check_writable(pid, expected).await?;
        if r.project_id != pid {
            return Err(failure("validation_error", "relationship project mismatch"));
        }
        if let Some(iid) = &r.vape_instance_id {
            self.check_owner(iid, None).await?;
        }
        let mut all = self.relationships(pid).await?;
        if primary && all.primary_instance_relationship_id.is_some() {
            return Err(failure(
                "conflict",
                "promote an existing relationship explicitly",
            ));
        }
        if primary {
            all.primary_instance_relationship_id = Some(r.id.clone());
        }
        all.instances.push(r.clone());
        write_json(&self.relationship_path(pid), &all).await?;
        self.save_project_revision(&mut p).await?;
        Ok(r)
    }
    async fn check_owner(&self, iid: &str, except: Option<&str>) -> Result<()> {
        validate_id(iid)?;
        let collections: Vec<Relationships> =
            read_json_dir(&self.root.join("project_instances")).await?;
        for c in collections {
            for r in c.instances {
                if r.vape_instance_id.as_deref() == Some(iid) && except != Some(r.id.as_str()) {
                    return Err(failure(
                        "conflict",
                        format!("instance already belongs to project {}", c.project_id),
                    ));
                }
            }
        }
        Ok(())
    }
    pub async fn edit_relationship(
        &self,
        pid: &str,
        rid: &str,
        expected: u64,
        primary: Option<bool>,
        f: impl FnOnce(&mut Relationship) -> Result<()>,
    ) -> Result<()> {
        let _lock = self.lock.lock().await;
        let mut p = self.check_writable(pid, expected).await?;
        let mut all = self.relationships(pid).await?;
        let old_primary = all.primary_instance_relationship_id.clone();
        let r = all
            .instances
            .iter_mut()
            .find(|r| r.id == rid)
            .ok_or_else(|| failure("not_found", "relationship not found"))?;
        f(r)?;
        r.updated_at = timestamp();
        if let Some(yes) = primary {
            if yes {
                all.primary_instance_relationship_id = Some(rid.into());
            } else if old_primary.as_deref() == Some(rid) {
                all.primary_instance_relationship_id = None;
            }
        }
        for r in &all.instances {
            if all.primary_instance_relationship_id.as_deref() != Some(&r.id)
                && (r.goal_override.is_some() || r.heartbeat_prompt_override.is_some())
            {
                return Err(failure(
                    "validation_error",
                    "clear prompt overrides before demoting a primary",
                ));
            }
        }
        if old_primary != all.primary_instance_relationship_id {
            let marker = format!(
                "\n\n## Primary promotion {}\nPrevious: {:?}; current: {:?}. Refresh the new primary's transcript before acting.\n",
                timestamp(),
                old_primary,
                all.primary_instance_relationship_id
            );
            let mut memory = self.read_memory(pid).await?.unwrap_or_default();
            memory.push_str(&marker);
            write_atomic(&self.memory_path(pid), &memory).await?;
            p.last_heartbeat_at = None;
        }
        Self::pause_without_direction(&mut p, &all);
        write_json(&self.relationship_path(pid), &all).await?;
        self.save_project_revision(&mut p).await
    }
    pub async fn detach_relationship(&self, pid: &str, rid: &str, expected: u64) -> Result<()> {
        let _lock = self.lock.lock().await;
        let mut p = self.check_writable(pid, expected).await?;
        let mut all = self.relationships(pid).await?;
        let len = all.instances.len();
        all.instances.retain(|r| r.id != rid);
        if len == all.instances.len() {
            return Err(failure("not_found", "relationship not found"));
        }
        if all.primary_instance_relationship_id.as_deref() == Some(rid) {
            all.primary_instance_relationship_id = None;
            p.heartbeat_enabled = false;
            p.status = "paused".into();
            p.last_heartbeat_at = None;
        }
        write_json(&self.relationship_path(pid), &all).await?;
        self.save_project_revision(&mut p).await
    }
    pub async fn bind_instance(&self, pid: &str, rid: &str, iid: &str) -> Result<()> {
        let _lock = self.lock.lock().await;
        let mut p = self.require_project(pid).await?;
        self.check_owner(iid, Some(rid)).await?;
        let mut all = self.relationships(pid).await?;
        let r = all
            .instances
            .iter_mut()
            .find(|r| r.id == rid)
            .ok_or_else(|| failure("not_found", "relationship detached"))?;
        r.vape_instance_id = Some(iid.into());
        r.provision_error = None;
        r.updated_at = timestamp();
        write_json(&self.relationship_path(pid), &all).await?;
        self.save_project_revision(&mut p).await
    }
    pub async fn provision_state(
        &self,
        pid: &str,
        rid: &str,
        attempted: bool,
        error: Option<String>,
    ) -> Result<()> {
        let _lock = self.lock.lock().await;
        let mut p = self.require_project(pid).await?;
        let mut all = self.relationships(pid).await?;
        let r = all
            .instances
            .iter_mut()
            .find(|r| r.id == rid)
            .ok_or_else(|| failure("not_found", "relationship detached"))?;
        r.provision_attempted = attempted;
        r.provision_error = error;
        r.updated_at = timestamp();
        write_json(&self.relationship_path(pid), &all).await?;
        self.save_project_revision(&mut p).await
    }
    pub async fn user_notes(&self, pid: &str) -> Result<Vec<UserNote>> {
        let p = self.require_project(pid).await?;
        let mut notes: Vec<UserNote> =
            read_json_dir(&self.root.join("user_notes").join(pid)).await?;
        notes.sort_by_key(|n| {
            p.tracking
                .note_order
                .iter()
                .position(|id| id == &n.id)
                .unwrap_or(usize::MAX)
        });
        for (pos, n) in notes.iter_mut().enumerate() {
            n.position = pos;
        }
        Ok(notes)
    }
    pub async fn save_user_note(
        &self,
        pid: &str,
        expected: u64,
        mut note: UserNote,
        create: bool,
    ) -> Result<UserNote> {
        let _lock = self.lock.lock().await;
        let mut p = self.check_writable(pid, expected).await?;
        validate_id(&note.id)?;
        let path = self.user_note_path(pid, &note.id);
        if !create && !fs::try_exists(&path).await? {
            return Err(failure("not_found", "note not found"));
        }
        note.updated_at = timestamp();
        if create {
            note.position = p.tracking.note_order.len();
            p.tracking.note_order.push(note.id.clone());
        }
        write_json(&path, &note).await?;
        self.save_project_revision(&mut p).await?;
        Ok(note)
    }
    pub async fn delete_user_note(&self, pid: &str, nid: &str, expected: u64) -> Result<()> {
        let _lock = self.lock.lock().await;
        let mut p = self.check_writable(pid, expected).await?;
        validate_id(nid)?;
        let path = self.user_note_path(pid, nid);
        if !fs::try_exists(&path).await? {
            return Err(failure("not_found", "note not found"));
        }
        fs::remove_file(path).await?;
        p.tracking.note_order.retain(|id| id != nid);
        self.save_project_revision(&mut p).await
    }
    pub async fn reorder_notes(&self, pid: &str, expected: u64, ids: Vec<String>) -> Result<()> {
        let notes = self.user_notes(pid).await?;
        let actual: std::collections::BTreeSet<_> = notes.iter().map(|n| n.id.clone()).collect();
        if ids.len() != actual.len()
            || ids
                .iter()
                .cloned()
                .collect::<std::collections::BTreeSet<_>>()
                != actual
        {
            return Err(failure(
                "validation_error",
                "note order must contain each note ID exactly once",
            ));
        }
        self.mutate_project(pid, expected, |p| {
            p.tracking.note_order = ids;
            Ok(())
        })
        .await?;
        Ok(())
    }
    /// Inventory both sides before writing. A resumed legacy migration must
    /// preserve the exact original target, never silently adopt other state.
    pub async fn migrate(&self, dry_run: bool) -> Result<MigrationReport> {
        let _lock = self.lock.lock().await;
        let mut report = MigrationReport {
            dry_run,
            records: vec![],
            errors: vec![],
        };
        let mut projects = HashMap::new();
        for path in json_paths(&self.root.join("projects")).await? {
            let parsed: Result<_> = async {
                let raw = fs::read_to_string(&path).await?;
                let p: Project = serde_json::from_str(&raw)?;
                validate_id(&p.id)?;
                if path != self.project_path(&p.id) {
                    anyhow::bail!("project filename/id mismatch");
                }
                Ok((p, raw))
            }
            .await;
            match parsed {
                Ok((p, raw)) => {
                    projects.insert(p.id.clone(), (p, raw));
                }
                Err(error) => report.errors.push(format!("{}: {error}", path.display())),
            }
        }
        let mut collections = HashMap::new();
        let mut owners = HashMap::new();
        let mut relationship_ids = std::collections::HashSet::new();
        for path in json_paths(&self.root.join("project_instances")).await? {
            let parsed: Result<Relationships> = async {
                let all: Relationships = read_json(&path)
                    .await?
                    .ok_or_else(|| anyhow::anyhow!("collection disappeared"))?;
                validate_id(&all.project_id)?;
                if path != self.relationship_path(&all.project_id) {
                    anyhow::bail!("relationship collection filename/project_id mismatch");
                }
                let mut ids = std::collections::HashSet::new();
                for r in &all.instances {
                    validate_id(&r.id)?;
                    if r.project_id != all.project_id {
                        anyhow::bail!("relationship project mismatch");
                    }
                    if !ids.insert(&r.id) {
                        anyhow::bail!("duplicate relationship ID");
                    }
                    if let Some(iid) = &r.vape_instance_id {
                        validate_id(iid)?;
                    }
                }
                if all.primary_instance_relationship_id.is_some() && all.primary().is_none() {
                    anyhow::bail!("missing primary relationship");
                }
                Ok(all)
            }
            .await;
            match parsed {
                Ok(all) => {
                    // Orphans still participate in ownership checks, and always
                    // block writes instead of being silently ignored.
                    if !projects.contains_key(&all.project_id) {
                        report.errors.push(format!(
                            "{}: orphan relationship collection",
                            path.display()
                        ));
                    }
                    for r in &all.instances {
                        if !relationship_ids.insert(r.id.clone()) {
                            report.errors.push(format!(
                                "{}: duplicate global relationship ID {}",
                                path.display(),
                                r.id
                            ));
                        }
                        if let Some(iid) = &r.vape_instance_id {
                            if let Some(owner) = owners.insert(iid.clone(), all.project_id.clone())
                            {
                                report.errors.push(format!(
                                    "{}: instance {iid} already owned by {owner}",
                                    path.display()
                                ));
                            }
                        }
                    }
                    collections.insert(all.project_id.clone(), all);
                }
                Err(error) => report.errors.push(format!("{}: {error}", path.display())),
            }
        }
        let mut pending = vec![];
        for (pid, (mut p, raw)) in projects {
            let migrated = p.tracking.schema_version < 2;
            let existing = collections.remove(&pid);
            let result: Result<_> = async {
                if !migrated && existing.is_none() { anyhow::bail!("v2 relationship collection is missing"); }
                let mut all = existing.unwrap_or_else(||Relationships { project_id: pid.clone(), ..Default::default() });
                if migrated {
                    if !all.instances.is_empty() {
                        if all.instances.len() != 1 || p.vape_instance_id.is_none() || all.primary().and_then(|r|r.vape_instance_id.as_ref()) != p.vape_instance_id.as_ref() {
                            anyhow::bail!("resumed relationship collection does not preserve the legacy primary instance");
                        }
                    } else if let Some(iid) = &p.vape_instance_id {
                        validate_id(iid)?;
                        if let Some(owner) = owners.insert(iid.clone(),pid.clone()) { anyhow::bail!("instance {iid} already owned by {owner}"); }
                        let mut r = Relationship::pending(&p,"Primary".into(),p.constellation.clone(),None);
                        r.id = legacy_relationship_id(&p.id);
                        validate_id(&r.id)?;
                        if !relationship_ids.insert(r.id.clone()) { anyhow::bail!("duplicate migrated relationship ID"); }
                        r.vape_instance_id = Some(iid.clone());r.created_by = "legacy-migration".into();
                        r.created_at = p.created_at.clone();r.updated_at = p.updated_at.clone();
                        all.primary_instance_relationship_id = Some(r.id.clone());all.instances.push(r);
                    } else if all.primary_instance_relationship_id.is_some() { anyhow::bail!("empty legacy project cannot have a resumed primary"); }
                }
                p.tracking.schema_version = 2;p.tracking.revision = p.tracking.revision.max(1);
                let after = serde_json::to_string_pretty(&p)?;
                let record = MigrationRecord { project_id: pid.clone(), migrated, before_sha256:format!("{:x}",Sha256::digest(raw.as_bytes())), after_sha256:format!("{:x}",Sha256::digest(after.as_bytes())) };
                Ok((p,all,record))
            }.await;
            match result {
                Ok(row) => pending.push(row),
                Err(error) => report.errors.push(format!("project {pid}: {error}")),
            }
        }
        if !dry_run && report.errors.is_empty() {
            for (p, all, record) in &pending {
                if record.migrated {
                    write_json(&self.relationship_path(&p.id), all).await?;
                    write_json(&self.project_path(&p.id), p).await?;
                }
            }
        }
        report.records = pending.into_iter().map(|(_, _, r)| r).collect();
        report
            .records
            .sort_by(|a, b| a.project_id.cmp(&b.project_id));
        report.errors.sort();
        Ok(report)
    }
}

fn legacy_relationship_id(project_id: &str) -> String {
    let readable = format!("legacy-{project_id}");
    if readable.len() <= 128 {
        readable
    } else {
        format!("legacy-{:x}", Sha256::digest(project_id.as_bytes()))
    }
}

async fn json_paths(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut entries = match fs::read_dir(dir).await {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        Err(error) => return Err(error.into()),
    };
    let mut paths = vec![];
    while let Some(entry) = entries.next_entry().await? {
        if entry.path().extension().and_then(|s| s.to_str()) == Some("json") {
            paths.push(entry.path());
        }
    }
    paths.sort();
    Ok(paths)
}

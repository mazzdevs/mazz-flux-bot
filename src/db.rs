use anyhow::Result;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};

use crate::models::{ActionLogEntry, CreateProjectRequest, HumanTask, Project, ProjectNote, ProjectStatus};

pub async fn init_db(db_path: &str) -> Result<SqlitePool> {
    let opts = SqliteConnectOptions::new()
        .filename(db_path)
        .create_if_missing(true);
    let pool = SqlitePoolOptions::new().max_connections(5).connect_with(opts).await?;

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS projects (
            id                  TEXT PRIMARY KEY,
            name                TEXT NOT NULL,
            goal                TEXT NOT NULL,
            constellation       TEXT NOT NULL,
            status              TEXT NOT NULL DEFAULT 'draft',
            vape_instance_id    TEXT,
            heartbeat_enabled   INTEGER NOT NULL DEFAULT 0,
            last_note           TEXT,
            created_at          TEXT NOT NULL,
            updated_at          TEXT NOT NULL,
            last_heartbeat_at   TEXT
        )
        "#,
    )
    .execute(&pool)
    .await?;

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS action_log (
            id          INTEGER PRIMARY KEY AUTOINCREMENT,
            project_id  TEXT,
            instance_id TEXT,
            action      TEXT NOT NULL,
            detail      TEXT,
            result      TEXT,
            error       TEXT,
            created_at  TEXT NOT NULL
        )
        "#,
    )
    .execute(&pool)
    .await?;

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS instance_cache (
            id         TEXT PRIMARY KEY,
            raw_json   TEXT NOT NULL,
            fetched_at TEXT NOT NULL
        )
        "#,
    )
    .execute(&pool)
    .await?;

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS instance_list_cache (
            singleton  INTEGER PRIMARY KEY CHECK (singleton = 1),
            raw_json   TEXT NOT NULL,
            fetched_at TEXT NOT NULL
        )
        "#,
    )
    .execute(&pool)
    .await?;

    // Key-value store for settings configured via the web UI (conductor API
    // keys/models today). Secrets live here, not in env vars, once set this
    // way — see conductor.rs's `from_sources`, which checks this table first and
    // falls back to env vars only if a key has no row here.
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS settings (
            key   TEXT PRIMARY KEY,
            value TEXT NOT NULL
        )
        "#,
    )
    .execute(&pool)
    .await?;

    // Blockers the conductor raised via `create_human_task` — see
    // heartbeat.rs's TickOutcome::Blocked.
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS human_tasks (
            id           INTEGER PRIMARY KEY AUTOINCREMENT,
            project_id   TEXT NOT NULL,
            description  TEXT NOT NULL,
            status       TEXT NOT NULL DEFAULT 'open',
            created_at   TEXT NOT NULL,
            resolved_at  TEXT
        )
        "#,
    )
    .execute(&pool)
    .await?;

    // Markdown notes the conductor chose to persist about a project. Content
    // stored directly here (not on disk) — deliberately simple for now, see
    // PLAN.md.
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS project_notes (
            id          INTEGER PRIMARY KEY AUTOINCREMENT,
            project_id  TEXT NOT NULL,
            content     TEXT NOT NULL,
            created_at  TEXT NOT NULL
        )
        "#,
    )
    .execute(&pool)
    .await?;

    Ok(pool)
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}

fn row_to_project(row: &sqlx::sqlite::SqliteRow) -> Project {
    Project {
        id: row.get("id"),
        name: row.get("name"),
        goal: row.get("goal"),
        constellation: row.get("constellation"),
        status: row.get("status"),
        vape_instance_id: row.get("vape_instance_id"),
        heartbeat_enabled: row.get::<i64, _>("heartbeat_enabled") != 0,
        last_note: row.get("last_note"),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
        last_heartbeat_at: row.get("last_heartbeat_at"),
    }
}

pub async fn create_project(pool: &SqlitePool, req: CreateProjectRequest) -> Result<Project> {
    let id = uuid::Uuid::new_v4().to_string();
    let ts = now();
    let constellation = req.constellation.unwrap_or_else(|| "back-office".to_string());

    sqlx::query(
        r#"INSERT INTO projects (id, name, goal, constellation, status, heartbeat_enabled, created_at, updated_at)
           VALUES (?1, ?2, ?3, ?4, 'draft', 0, ?5, ?5)"#,
    )
    .bind(&id)
    .bind(&req.name)
    .bind(&req.goal)
    .bind(&constellation)
    .bind(&ts)
    .execute(pool)
    .await?;

    get_project(pool, &id).await?.ok_or_else(|| anyhow::anyhow!("project vanished after insert"))
}

pub async fn list_projects(pool: &SqlitePool) -> Result<Vec<Project>> {
    let rows = sqlx::query("SELECT * FROM projects ORDER BY created_at DESC").fetch_all(pool).await?;
    Ok(rows.iter().map(row_to_project).collect())
}

pub async fn get_project(pool: &SqlitePool, id: &str) -> Result<Option<Project>> {
    let row = sqlx::query("SELECT * FROM projects WHERE id = ?1").bind(id).fetch_optional(pool).await?;
    Ok(row.map(|r| row_to_project(&r)))
}

pub async fn set_project_instance(pool: &SqlitePool, id: &str, instance_id: &str) -> Result<()> {
    sqlx::query("UPDATE projects SET vape_instance_id = ?1, updated_at = ?2 WHERE id = ?3")
        .bind(instance_id)
        .bind(now())
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn set_project_status(pool: &SqlitePool, id: &str, status: ProjectStatus, note: Option<&str>) -> Result<()> {
    sqlx::query("UPDATE projects SET status = ?1, last_note = ?2, updated_at = ?3 WHERE id = ?4")
        .bind(status.as_str())
        .bind(note)
        .bind(now())
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Only touches the boolean flag — NOT `status`. Callers set status
/// explicitly (see `set_project_status`/`set_project_status_only`). This used
/// to also force status to running/paused, which meant calling this after
/// `set_project_status(Done)` (as heartbeat.rs's mark_done path does) would
/// silently clobber "done" back to "paused" — the two are independent
/// columns and must be set independently.
pub async fn set_heartbeat_enabled(pool: &SqlitePool, id: &str, enabled: bool) -> Result<()> {
    sqlx::query("UPDATE projects SET heartbeat_enabled = ?1, updated_at = ?2 WHERE id = ?3")
        .bind(enabled as i64)
        .bind(now())
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Like `set_project_status` but leaves `last_note` untouched — for callers
/// (start/pause) that are just flipping lifecycle state, not reporting a
/// reason. `set_project_status` overwrites `last_note` unconditionally
/// (unlike `touch_heartbeat`'s COALESCE), which is correct for the
/// conductor's terminal outcomes but wrong for a plain user-initiated
/// start/pause that shouldn't erase the last thing the conductor said.
pub async fn set_project_status_only(pool: &SqlitePool, id: &str, status: ProjectStatus) -> Result<()> {
    sqlx::query("UPDATE projects SET status = ?1, updated_at = ?2 WHERE id = ?3")
        .bind(status.as_str())
        .bind(now())
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn touch_heartbeat(pool: &SqlitePool, id: &str, note: Option<&str>) -> Result<()> {
    sqlx::query("UPDATE projects SET last_heartbeat_at = ?1, last_note = COALESCE(?2, last_note), updated_at = ?1 WHERE id = ?3")
        .bind(now())
        .bind(note)
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn list_running_projects(pool: &SqlitePool) -> Result<Vec<Project>> {
    let rows = sqlx::query("SELECT * FROM projects WHERE heartbeat_enabled = 1 AND status = 'running'")
        .fetch_all(pool)
        .await?;
    Ok(rows.iter().map(row_to_project).collect())
}

pub async fn log_action(
    pool: &SqlitePool,
    project_id: Option<&str>,
    instance_id: Option<&str>,
    action: &str,
    detail: Option<&serde_json::Value>,
    result: Option<&str>,
    error: Option<&str>,
) -> Result<()> {
    sqlx::query(
        r#"INSERT INTO action_log (project_id, instance_id, action, detail, result, error, created_at)
           VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)"#,
    )
    .bind(project_id)
    .bind(instance_id)
    .bind(action)
    .bind(detail.map(|d| d.to_string()))
    .bind(result)
    .bind(error)
    .bind(now())
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn list_action_log(pool: &SqlitePool, project_id: Option<&str>, limit: i64) -> Result<Vec<ActionLogEntry>> {
    let rows = match project_id {
        Some(pid) => {
            sqlx::query("SELECT * FROM action_log WHERE project_id = ?1 ORDER BY id DESC LIMIT ?2")
                .bind(pid)
                .bind(limit)
                .fetch_all(pool)
                .await?
        }
        None => {
            sqlx::query("SELECT * FROM action_log ORDER BY id DESC LIMIT ?1")
                .bind(limit)
                .fetch_all(pool)
                .await?
        }
    };
    Ok(rows
        .iter()
        .map(|r| ActionLogEntry {
            id: r.get("id"),
            project_id: r.get("project_id"),
            instance_id: r.get("instance_id"),
            action: r.get("action"),
            detail: r.get("detail"),
            result: r.get("result"),
            error: r.get("error"),
            created_at: r.get("created_at"),
        })
        .collect())
}

/// Cache the raw `GET /api/v1/instances` response so the UI has something to
/// render instantly (and something to fall back to if WARP/vape is down).
pub async fn delete_project(pool: &SqlitePool, id: &str) -> Result<()> {
    sqlx::query("DELETE FROM projects WHERE id = ?1").bind(id).execute(pool).await?;
    Ok(())
}

pub async fn cache_instance_list(pool: &SqlitePool, raw_json: &str) -> Result<()> {
    sqlx::query(
        r#"INSERT INTO instance_list_cache (singleton, raw_json, fetched_at) VALUES (1, ?1, ?2)
           ON CONFLICT(singleton) DO UPDATE SET raw_json = excluded.raw_json, fetched_at = excluded.fetched_at"#,
    )
    .bind(raw_json)
    .bind(now())
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn get_cached_instance_list(pool: &SqlitePool) -> Result<Option<(String, String)>> {
    let row = sqlx::query("SELECT raw_json, fetched_at FROM instance_list_cache WHERE singleton = 1")
        .fetch_optional(pool)
        .await?;
    Ok(row.map(|r| (r.get("raw_json"), r.get("fetched_at"))))
}

pub async fn get_setting(pool: &SqlitePool, key: &str) -> Result<Option<String>> {
    let row = sqlx::query("SELECT value FROM settings WHERE key = ?1").bind(key).fetch_optional(pool).await?;
    Ok(row.map(|r| r.get("value")))
}

/// An empty value deletes the row (that's how the settings UI "clear this
/// key" action works) — a stored empty string would otherwise look
/// indistinguishable from "not set" everywhere else that reads it.
pub async fn set_setting(pool: &SqlitePool, key: &str, value: &str) -> Result<()> {
    if value.is_empty() {
        sqlx::query("DELETE FROM settings WHERE key = ?1").bind(key).execute(pool).await?;
    } else {
        sqlx::query("INSERT INTO settings (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value")
            .bind(key)
            .bind(value)
            .execute(pool)
            .await?;
    }
    Ok(())
}

// ---- Human tasks (conductor-raised blockers) ------------------------------

pub async fn create_human_task(pool: &SqlitePool, project_id: &str, description: &str) -> Result<HumanTask> {
    let ts = now();
    let id = sqlx::query("INSERT INTO human_tasks (project_id, description, status, created_at) VALUES (?1, ?2, 'open', ?3)")
        .bind(project_id)
        .bind(description)
        .bind(&ts)
        .execute(pool)
        .await?
        .last_insert_rowid();
    Ok(HumanTask { id, project_id: project_id.to_string(), description: description.to_string(), status: "open".to_string(), created_at: ts, resolved_at: None })
}

fn row_to_human_task(row: &sqlx::sqlite::SqliteRow) -> HumanTask {
    HumanTask {
        id: row.get("id"),
        project_id: row.get("project_id"),
        description: row.get("description"),
        status: row.get("status"),
        created_at: row.get("created_at"),
        resolved_at: row.get("resolved_at"),
    }
}

/// `project_id = None` lists across all projects (the dashboard-wide panel).
pub async fn list_human_tasks(pool: &SqlitePool, project_id: Option<&str>, open_only: bool) -> Result<Vec<HumanTask>> {
    let rows = match (project_id, open_only) {
        (Some(pid), true) => {
            sqlx::query("SELECT * FROM human_tasks WHERE project_id = ?1 AND status = 'open' ORDER BY created_at DESC").bind(pid).fetch_all(pool).await?
        }
        (Some(pid), false) => sqlx::query("SELECT * FROM human_tasks WHERE project_id = ?1 ORDER BY created_at DESC").bind(pid).fetch_all(pool).await?,
        (None, true) => sqlx::query("SELECT * FROM human_tasks WHERE status = 'open' ORDER BY created_at DESC").fetch_all(pool).await?,
        (None, false) => sqlx::query("SELECT * FROM human_tasks ORDER BY created_at DESC").fetch_all(pool).await?,
    };
    Ok(rows.iter().map(row_to_human_task).collect())
}

pub async fn resolve_human_task(pool: &SqlitePool, id: i64) -> Result<()> {
    sqlx::query("UPDATE human_tasks SET status = 'resolved', resolved_at = ?1 WHERE id = ?2").bind(now()).bind(id).execute(pool).await?;
    Ok(())
}

// ---- Project notes (conductor-authored markdown) --------------------------

pub async fn add_project_note(pool: &SqlitePool, project_id: &str, content: &str) -> Result<ProjectNote> {
    let ts = now();
    let id = sqlx::query("INSERT INTO project_notes (project_id, content, created_at) VALUES (?1, ?2, ?3)")
        .bind(project_id)
        .bind(content)
        .bind(&ts)
        .execute(pool)
        .await?
        .last_insert_rowid();
    Ok(ProjectNote { id, project_id: project_id.to_string(), content: content.to_string(), created_at: ts })
}

pub async fn list_project_notes(pool: &SqlitePool, project_id: &str) -> Result<Vec<ProjectNote>> {
    let rows = sqlx::query("SELECT * FROM project_notes WHERE project_id = ?1 ORDER BY created_at DESC").bind(project_id).fetch_all(pool).await?;
    Ok(rows
        .iter()
        .map(|r| ProjectNote { id: r.get("id"), project_id: r.get("project_id"), content: r.get("content"), created_at: r.get("created_at") })
        .collect())
}

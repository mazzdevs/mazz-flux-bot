use anyhow::Result;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};

use crate::models::{ActionLogEntry, CreateProjectRequest, Project, ProjectStatus};

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

pub async fn set_heartbeat_enabled(pool: &SqlitePool, id: &str, enabled: bool) -> Result<()> {
    let status = if enabled { ProjectStatus::Running.as_str() } else { ProjectStatus::Paused.as_str() };
    sqlx::query("UPDATE projects SET heartbeat_enabled = ?1, status = ?2, updated_at = ?3 WHERE id = ?4")
        .bind(enabled as i64)
        .bind(status)
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

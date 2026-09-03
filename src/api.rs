use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::json;

use crate::conductor;
use crate::models::CreateProjectRequest;
use crate::state_repo;
use crate::AppState;

/// Wraps anyhow::Error so handlers can just use `?` and get a sane 500 JSON
/// body instead of a compile error or a panic.
pub struct AppError(anyhow::Error);

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        tracing::error!(error = %self.0, "request failed");
        (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": self.0.to_string()}))).into_response()
    }
}

impl<E: Into<anyhow::Error>> From<E> for AppError {
    fn from(e: E) -> Self {
        AppError(e.into())
    }
}

type ApiResult<T> = Result<T, AppError>;

// ---- Projects -----------------------------------------------------------

pub async fn list_projects(State(state): State<AppState>) -> ApiResult<Json<serde_json::Value>> {
    let projects = state.store.list_projects().await?;
    Ok(Json(json!({ "projects": projects })))
}

pub async fn create_project(State(state): State<AppState>, Json(req): Json<CreateProjectRequest>) -> ApiResult<Json<serde_json::Value>> {
    let project = state.store.create_project(req).await?;
    state.store.log_action(Some(&project.id), None, "project_created", None, None, None).await?;
    Ok(Json(json!({ "project": project })))
}

pub async fn get_project(State(state): State<AppState>, Path(id): Path<String>) -> ApiResult<Response> {
    match state.store.get_project(&id).await? {
        Some(p) => Ok(Json(json!({ "project": p })).into_response()),
        None => Ok((StatusCode::NOT_FOUND, Json(json!({"error": "project not found"}))).into_response()),
    }
}

pub async fn delete_project(State(state): State<AppState>, Path(id): Path<String>) -> ApiResult<Json<serde_json::Value>> {
    state.store.delete_project(&id).await?;
    Ok(Json(json!({ "ok": true })))
}

pub async fn start_project(State(state): State<AppState>, Path(id): Path<String>) -> ApiResult<Json<serde_json::Value>> {
    // Explicit status set (not implied by set_heartbeat_enabled, which only
    // touches the boolean flag) — this is also how a Blocked/Error/Done
    // project gets manually resumed back to Running.
    state.store.set_heartbeat_enabled(&id, true).await?;
    state.store.set_project_status_only(&id, crate::models::ProjectStatus::Running).await?;
    state.store.log_action(Some(&id), None, "heartbeat_started", None, None, None).await?;
    Ok(Json(json!({ "ok": true })))
}

pub async fn pause_project(State(state): State<AppState>, Path(id): Path<String>) -> ApiResult<Json<serde_json::Value>> {
    state.store.set_heartbeat_enabled(&id, false).await?;
    state.store.set_project_status_only(&id, crate::models::ProjectStatus::Paused).await?;
    state.store.log_action(Some(&id), None, "heartbeat_paused", None, None, None).await?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
pub struct SendMessageRequest {
    pub message: String,
}

/// Manual override: bypass the conductor and send a message straight to the
/// project's instance. Useful for answering a pending question yourself
/// without waiting for the next heartbeat, or when ANTHROPIC_API_KEY isn't
/// configured at all.
pub async fn send_project_message(State(state): State<AppState>, Path(id): Path<String>, Json(req): Json<SendMessageRequest>) -> ApiResult<Json<serde_json::Value>> {
    let project = state.store.get_project(&id).await?.ok_or_else(|| anyhow::anyhow!("project not found"))?;
    let instance_id = project.vape_instance_id.ok_or_else(|| anyhow::anyhow!("project has no instance yet"))?;
    let resp = state.vape.pida_send(&instance_id, &req.message).await?;
    state.store.log_action(Some(&id), Some(&instance_id), "manual_send", Some(&json!({"message": req.message})), Some(&resp.to_string()), None).await?;
    Ok(Json(json!({ "result": resp })))
}

#[derive(Deserialize)]
pub struct LogQuery {
    pub project_id: Option<String>,
    pub limit: Option<i64>,
}

pub async fn action_log(State(state): State<AppState>, Query(q): Query<LogQuery>) -> ApiResult<Json<serde_json::Value>> {
    let entries = state.store.list_action_log(q.project_id.as_deref(), q.limit.unwrap_or(100)).await?;
    Ok(Json(json!({ "entries": entries })))
}

// ---- Vape instances (read-through cache) --------------------------------

pub async fn list_instances(State(state): State<AppState>) -> ApiResult<Json<serde_json::Value>> {
    match state.vape.list_instances().await {
        Ok((raw, instances)) => {
            let _ = state.store.cache_instance_list(&raw).await;
            Ok(Json(json!({ "instances": instances, "source": "live" })))
        }
        Err(e) => {
            tracing::warn!(error = %e, "live instance list failed, falling back to cache");
            match state.store.get_cached_instance_list().await? {
                Some((raw, fetched_at)) => {
                    let instances: serde_json::Value = serde_json::from_str(&raw).unwrap_or(json!([]));
                    Ok(Json(json!({ "instances": instances, "source": "cache", "fetched_at": fetched_at, "live_error": e.to_string() })))
                }
                None => Err(AppError(e)),
            }
        }
    }
}

pub async fn get_instance(State(state): State<AppState>, Path(id): Path<String>) -> ApiResult<Json<serde_json::Value>> {
    let instance = state.vape.get_instance(&id).await?;
    Ok(Json(json!({ "instance": instance })))
}

pub async fn get_instance_status(State(state): State<AppState>, Path(id): Path<String>) -> ApiResult<Json<serde_json::Value>> {
    let agent_status = state.vape.agent_status(&id).await?;
    let harness = agent_status.active_harness.clone().unwrap_or_else(|| "pida".to_string());
    let pida = if harness == "pida" { state.vape.pida_status(&id).await.ok() } else { None };
    Ok(Json(json!({ "agent_status": agent_status, "pida_status": pida })))
}

pub async fn get_instance_session(State(state): State<AppState>, Path(id): Path<String>) -> ApiResult<Json<serde_json::Value>> {
    let session = state.vape.pida_session(&id).await?;
    Ok(Json(json!({ "session": session })))
}

pub async fn list_constellations(State(state): State<AppState>) -> ApiResult<Json<serde_json::Value>> {
    let constellations = state.vape.list_constellations().await?;
    Ok(Json(json!({ "constellations": constellations })))
}

// ---- Conductor settings -------------------------------------------------------
//
// Never echoes a raw secret back to the browser — only whether one is set
// and a masked last-4 preview. See conductor.rs::settings_status.

pub async fn get_settings(State(state): State<AppState>) -> ApiResult<Json<conductor::SettingsStatus>> {
    Ok(Json(conductor::settings_status(&state.store).await))
}

#[derive(Deserialize, Default)]
pub struct UpdateSettingsRequest {
    /// Omitted (not present in the JSON body) means "leave unchanged" —
    /// that's how the settings form avoids clobbering an already-saved key
    /// just because its input was left blank in the UI. An explicit empty
    /// string clears the key (see `Store::set_setting`).
    #[serde(default)]
    pub anthropic_api_key: Option<String>,
    #[serde(default)]
    pub anthropic_model: Option<String>,
    #[serde(default)]
    pub openrouter_api_key: Option<String>,
    #[serde(default)]
    pub openrouter_model: Option<String>,
}

pub async fn update_settings(State(state): State<AppState>, Json(req): Json<UpdateSettingsRequest>) -> ApiResult<Json<conductor::SettingsStatus>> {
    if let Some(v) = &req.anthropic_api_key {
        state.store.set_setting("anthropic_api_key", v).await?;
    }
    if let Some(v) = &req.anthropic_model {
        state.store.set_setting("anthropic_model", v).await?;
    }
    if let Some(v) = &req.openrouter_api_key {
        state.store.set_setting("openrouter_api_key", v).await?;
    }
    if let Some(v) = &req.openrouter_model {
        state.store.set_setting("openrouter_model", v).await?;
    }
    state.store.log_action(None, None, "settings_updated", None, None, None).await?;
    Ok(Json(conductor::settings_status(&state.store).await))
}

// ---- Human tasks (conductor-raised blockers) -----------------------------

#[derive(Deserialize, Default)]
pub struct HumanTaskQuery {
    #[serde(default)]
    pub project_id: Option<String>,
    /// Default true — the dashboard-wide panel only wants open ones. Pass
    /// `?open=false` to see resolved tasks too (used on the project detail
    /// page's full history).
    pub open: Option<bool>,
}

/// Attaches `project_name` to each task for display — the frontend has no
/// other cheap way to resolve project_id -> name without an extra round trip
/// per task.
pub async fn list_human_tasks(State(state): State<AppState>, Query(q): Query<HumanTaskQuery>) -> ApiResult<Json<serde_json::Value>> {
    let tasks = state.store.list_human_tasks(q.project_id.as_deref(), q.open.unwrap_or(true)).await?;
    let mut entries = Vec::with_capacity(tasks.len());
    for t in tasks {
        let project_name = state.store.get_project(&t.project_id).await?.map(|p| p.name);
        entries.push(json!({
            "id": t.id,
            "project_id": t.project_id,
            "project_name": project_name,
            "description": t.description,
            "status": t.status,
            "created_at": t.created_at,
            "resolved_at": t.resolved_at,
        }));
    }
    Ok(Json(json!({ "entries": entries })))
}

pub async fn resolve_human_task(State(state): State<AppState>, Path(id): Path<String>) -> ApiResult<Json<serde_json::Value>> {
    state.store.resolve_human_task(&id).await?;
    state.store.log_action(None, None, "human_task_resolved", None, Some(&id), None).await?;
    Ok(Json(json!({ "ok": true })))
}

// ---- Project notes (conductor-authored markdown) --------------------------

pub async fn list_project_notes(State(state): State<AppState>, Path(id): Path<String>) -> ApiResult<Json<serde_json::Value>> {
    let notes = state.store.list_project_notes(&id).await?;
    Ok(Json(json!({ "notes": notes })))
}

// ---- State repo (persist the file store as its own git history) ----------

#[derive(Deserialize, Default)]
pub struct CommitStateRequest {
    #[serde(default)]
    pub message: Option<String>,
}

pub async fn commit_state(State(state): State<AppState>, Json(req): Json<CommitStateRequest>) -> ApiResult<Json<serde_json::Value>> {
    let message = req.message.unwrap_or_else(|| format!("manual snapshot {}", chrono::Utc::now().to_rfc3339()));
    let summary = state_repo::commit(state.store.root(), &message).await?;
    state.store.log_action(None, None, "state_committed", None, Some(&serde_json::to_string(&summary)?), None).await?;
    Ok(Json(serde_json::to_value(summary)?))
}

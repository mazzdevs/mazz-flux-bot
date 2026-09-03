use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::json;

use crate::db;
use crate::models::CreateProjectRequest;
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
    let projects = db::list_projects(&state.db).await?;
    Ok(Json(json!({ "projects": projects })))
}

pub async fn create_project(State(state): State<AppState>, Json(req): Json<CreateProjectRequest>) -> ApiResult<Json<serde_json::Value>> {
    let project = db::create_project(&state.db, req).await?;
    db::log_action(&state.db, Some(&project.id), None, "project_created", None, None, None).await?;
    Ok(Json(json!({ "project": project })))
}

pub async fn get_project(State(state): State<AppState>, Path(id): Path<String>) -> ApiResult<Response> {
    match db::get_project(&state.db, &id).await? {
        Some(p) => Ok(Json(json!({ "project": p })).into_response()),
        None => Ok((StatusCode::NOT_FOUND, Json(json!({"error": "project not found"}))).into_response()),
    }
}

pub async fn delete_project(State(state): State<AppState>, Path(id): Path<String>) -> ApiResult<Json<serde_json::Value>> {
    db::delete_project(&state.db, &id).await?;
    Ok(Json(json!({ "ok": true })))
}

pub async fn start_project(State(state): State<AppState>, Path(id): Path<String>) -> ApiResult<Json<serde_json::Value>> {
    db::set_heartbeat_enabled(&state.db, &id, true).await?;
    db::log_action(&state.db, Some(&id), None, "heartbeat_started", None, None, None).await?;
    Ok(Json(json!({ "ok": true })))
}

pub async fn pause_project(State(state): State<AppState>, Path(id): Path<String>) -> ApiResult<Json<serde_json::Value>> {
    db::set_heartbeat_enabled(&state.db, &id, false).await?;
    db::log_action(&state.db, Some(&id), None, "heartbeat_paused", None, None, None).await?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
pub struct SendMessageRequest {
    pub message: String,
}

/// Manual override: bypass the brain and send a message straight to the
/// project's instance. Useful for answering a pending question yourself
/// without waiting for the next heartbeat, or when ANTHROPIC_API_KEY isn't
/// configured at all.
pub async fn send_project_message(State(state): State<AppState>, Path(id): Path<String>, Json(req): Json<SendMessageRequest>) -> ApiResult<Json<serde_json::Value>> {
    let project = db::get_project(&state.db, &id).await?.ok_or_else(|| anyhow::anyhow!("project not found"))?;
    let instance_id = project.vape_instance_id.ok_or_else(|| anyhow::anyhow!("project has no instance yet"))?;
    let resp = state.vape.pida_send(&instance_id, &req.message).await?;
    db::log_action(&state.db, Some(&id), Some(&instance_id), "manual_send", Some(&json!({"message": req.message})), Some(&resp.to_string()), None).await?;
    Ok(Json(json!({ "result": resp })))
}

#[derive(Deserialize)]
pub struct LogQuery {
    pub project_id: Option<String>,
    pub limit: Option<i64>,
}

pub async fn action_log(State(state): State<AppState>, Query(q): Query<LogQuery>) -> ApiResult<Json<serde_json::Value>> {
    let entries = db::list_action_log(&state.db, q.project_id.as_deref(), q.limit.unwrap_or(100)).await?;
    Ok(Json(json!({ "entries": entries })))
}

// ---- Vape instances (read-through cache) --------------------------------

pub async fn list_instances(State(state): State<AppState>) -> ApiResult<Json<serde_json::Value>> {
    match state.vape.list_instances().await {
        Ok((raw, instances)) => {
            let _ = db::cache_instance_list(&state.db, &raw).await;
            Ok(Json(json!({ "instances": instances, "source": "live" })))
        }
        Err(e) => {
            tracing::warn!(error = %e, "live instance list failed, falling back to cache");
            match db::get_cached_instance_list(&state.db).await? {
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

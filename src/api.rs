use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::conductor;
use crate::models::{Archetype, CreateProjectRequest, KanbanBoard, KanbanStatus, Project};
use crate::public_url::{self, PublicUrlSource};
use uuid::Uuid;
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

pub async fn create_project(State(state): State<AppState>, Json(mut req): Json<CreateProjectRequest>) -> ApiResult<Json<serde_json::Value>> {
    let (name, name_source) = match req.name.take().filter(|n| !n.trim().is_empty()) {
        Some(n) => (n, "user_provided"),
        None => suggest_project_name(&state, &req.goal).await,
    };
    req.name = Some(name);

    let project = state.store.create_project(req).await?;
    state
        .store
        .log_action(Some(&project.id), None, "project_created", Some(&json!({"name_source": name_source})), None, None)
        .await?;
    Ok(Json(json!({ "project": project })))
}

/// Best-effort LLM-suggested project name for when the create-project form's
/// name field was left blank. Falls back to a short id-based placeholder on
/// any failure/empty response — never blocks project creation.
async fn suggest_project_name(state: &AppState, goal: &str) -> (String, &'static str) {
    let conductor = conductor::Conductor::from_sources(&state.store).await;
    if conductor.enabled() {
        if let Ok(text) = conductor.suggest_project_name(goal).await {
            let trimmed = text.trim();
            if !trimmed.is_empty() {
                return (trimmed.to_string(), "llm_suggested");
            }
        }
    }
    (format!("project-{}", &Uuid::new_v4().to_string()[..8]), "placeholder_fallback")
}

pub async fn get_project(State(state): State<AppState>, Path(id): Path<String>) -> ApiResult<Response> {
    match state.store.get_project(&id).await? {
        Some(p) => Ok(Json(json!({ "project": p })).into_response()),
        None => Ok((StatusCode::NOT_FOUND, Json(json!({"error": "project not found"}))).into_response()),
    }
}

#[derive(Serialize)]
pub struct AgentProjectContext {
    pub id: String,
    pub name: String,
    pub goal: String,
    pub heartbeat_prompt: Option<String>,
    pub status: String,
    pub vape_instance_id: Option<String>,
    pub last_note: Option<String>,
    pub last_heartbeat_at: Option<String>,
}

impl From<Project> for AgentProjectContext {
    fn from(project: Project) -> Self {
        Self {
            id: project.id,
            name: project.name,
            goal: project.goal,
            heartbeat_prompt: project.heartbeat_prompt,
            status: project.status,
            vape_instance_id: project.vape_instance_id,
            last_note: project.last_note,
            last_heartbeat_at: project.last_heartbeat_at,
        }
    }
}

#[derive(Serialize)]
pub struct AgentContextResponse {
    pub project: AgentProjectContext,
    pub kanban: KanbanBoard,
    pub archetypes: Vec<Archetype>,
}

/// Purpose-built read-only context for the pida instance assigned to this
/// project. It intentionally excludes settings, logs, transcripts, and other
/// projects.
pub async fn get_agent_context(State(state): State<AppState>, Path(id): Path<String>) -> ApiResult<Response> {
    let Some(project) = state.store.get_project(&id).await? else {
        return Ok((StatusCode::NOT_FOUND, Json(json!({"error": "project not found"}))).into_response());
    };
    let kanban = state.store.get_kanban_board(&id).await?;
    let archetypes = state.store.list_archetypes().await?;
    Ok(Json(AgentContextResponse { project: project.into(), kanban, archetypes }).into_response())
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

// ---- Project Kanban boards ----------------------------------------------

pub async fn get_kanban_board(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<Response> {
    if state.store.get_project(&id).await?.is_none() {
        return Ok((
            StatusCode::NOT_FOUND,
            Json(json!({"error": "project not found"})),
        )
            .into_response());
    }
    let board = state.store.get_kanban_board(&id).await?;
    Ok(Json(json!({ "board": board })).into_response())
}

#[derive(Deserialize)]
pub struct CreateKanbanTaskRequest {
    pub title: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub status: Option<KanbanStatus>,
}

pub async fn create_kanban_task(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<CreateKanbanTaskRequest>,
) -> ApiResult<Response> {
    if req.title.trim().is_empty() {
        return Ok((StatusCode::BAD_REQUEST, Json(json!({"error": "task title is required"}))).into_response());
    }
    let task = state
        .store
        .create_kanban_task(
            &id,
            &req.title,
            &req.description,
            req.status.unwrap_or(KanbanStatus::Assigned),
        )
        .await?;
    state
        .store
        .log_action(
            Some(&id),
            None,
            "kanban_task_created",
            Some(&json!({"task_id": task.id, "status": task.status})),
            None,
            None,
        )
        .await?;
    Ok(Json(json!({ "task": task })).into_response())
}

#[derive(Deserialize, Default)]
pub struct UpdateKanbanTaskRequest {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub status: Option<KanbanStatus>,
}

pub async fn update_kanban_task(
    State(state): State<AppState>,
    Path((project_id, task_id)): Path<(String, String)>,
    Json(req): Json<UpdateKanbanTaskRequest>,
) -> ApiResult<Response> {
    if req
        .title
        .as_ref()
        .is_some_and(|title| title.trim().is_empty())
    {
        return Ok((
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "task title cannot be empty"})),
        )
            .into_response());
    }
    match state
        .store
        .update_kanban_task(
            &project_id,
            &task_id,
            req.title.as_deref(),
            req.description.as_deref(),
            req.status,
        )
        .await?
    {
        Some(task) => {
            state
                .store
                .log_action(
                    Some(&project_id),
                    None,
                    "kanban_task_updated",
                    Some(&json!({"task_id": task.id, "status": task.status})),
                    None,
                    None,
                )
                .await?;
            Ok(Json(json!({ "task": task })).into_response())
        }
        None => Ok((
            StatusCode::NOT_FOUND,
            Json(json!({"error": "kanban task not found"})),
        )
            .into_response()),
    }
}

pub async fn delete_kanban_task(
    State(state): State<AppState>,
    Path((project_id, task_id)): Path<(String, String)>,
) -> ApiResult<Response> {
    if !state
        .store
        .delete_kanban_task(&project_id, &task_id)
        .await?
    {
        return Ok((
            StatusCode::NOT_FOUND,
            Json(json!({"error": "kanban task not found"})),
        )
            .into_response());
    }
    state
        .store
        .log_action(
            Some(&project_id),
            None,
            "kanban_task_deleted",
            Some(&json!({"task_id": task_id})),
            None,
            None,
        )
        .await?;
    Ok(Json(json!({ "ok": true })).into_response())
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

// ---- Settings (models + public callback URL; no API keys) ----------------

#[derive(Serialize)]
pub struct SettingsResponse {
    pub conductor_model: String,
    pub instance_model: String,
    pub bot_public_base_url: Option<String>,
    pub effective_bot_public_base_url: Option<String>,
    pub bot_public_base_url_source: PublicUrlSource,
}

async fn settings_response(state: &AppState) -> ApiResult<SettingsResponse> {
    let conductor_model = conductor::resolve_model(&state.store, "conductor_model", "OPENROUTER_MODEL", conductor::DEFAULT_MODEL).await;
    let instance_model = conductor::resolve_model(&state.store, "instance_model", "MAZZ_FLUX_INSTANCE_MODEL", conductor::DEFAULT_MODEL).await;
    let bot_public_base_url = state.store.get_setting(public_url::PUBLIC_URL_SETTING).await?;
    let effective = public_url::resolve_public_url(&state.store).await;
    Ok(SettingsResponse {
        conductor_model,
        instance_model,
        bot_public_base_url,
        effective_bot_public_base_url: effective.url,
        bot_public_base_url_source: effective.source,
    })
}

pub async fn get_settings(State(state): State<AppState>) -> ApiResult<Json<SettingsResponse>> {
    Ok(Json(settings_response(&state).await?))
}

#[derive(Deserialize, Default)]
pub struct UpdateSettingsRequest {
    #[serde(default)]
    pub conductor_model: Option<String>,
    #[serde(default)]
    pub instance_model: Option<String>,
    #[serde(default)]
    pub bot_public_base_url: Option<String>,
}

pub async fn update_settings(State(state): State<AppState>, Json(req): Json<UpdateSettingsRequest>) -> ApiResult<Response> {
    let normalized_public_url = match req.bot_public_base_url.as_deref() {
        Some(value) if !value.trim().is_empty() => match public_url::normalize_public_base_url(value) {
            Ok(url) => Some(url),
            Err(error) => return Ok((StatusCode::BAD_REQUEST, Json(json!({"error": error.to_string()}))).into_response()),
        },
        Some(_) => Some(String::new()),
        None => None,
    };
    if let Some(v) = &req.conductor_model {
        state.store.set_setting("conductor_model", v).await?;
    }
    if let Some(v) = &req.instance_model {
        state.store.set_setting("instance_model", v).await?;
    }
    if let Some(v) = normalized_public_url {
        state.store.set_setting(public_url::PUBLIC_URL_SETTING, &v).await?;
    }
    state.store.log_action(None, None, "settings_updated", None, None, None).await?;
    Ok(Json(settings_response(&state).await?).into_response())
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

// ---- File browser (raw read/edit access to the state directory) ----------

#[derive(Deserialize, Default)]
pub struct FilesQuery {
    #[serde(default)]
    pub path: Option<String>,
}

pub async fn browse_files(State(state): State<AppState>, Query(q): Query<FilesQuery>) -> ApiResult<Json<serde_json::Value>> {
    let result = state.store.browse(q.path.as_deref().unwrap_or("")).await?;
    Ok(Json(serde_json::to_value(result)?))
}

#[derive(Deserialize)]
pub struct WriteFileRequest {
    pub content: String,
}

pub async fn write_file(State(state): State<AppState>, Query(q): Query<FilesQuery>, Json(req): Json<WriteFileRequest>) -> ApiResult<Json<serde_json::Value>> {
    let path = q.path.ok_or_else(|| anyhow::anyhow!("path is required"))?;
    state.store.write_file(&path, &req.content).await?;
    state.store.log_action(None, None, "file_written", Some(&json!({"path": path})), None, None).await?;
    Ok(Json(json!({ "ok": true })))
}

pub async fn delete_file(State(state): State<AppState>, Query(q): Query<FilesQuery>) -> ApiResult<Json<serde_json::Value>> {
    let path = q.path.ok_or_else(|| anyhow::anyhow!("path is required"))?;
    state.store.delete_file(&path).await?;
    state.store.log_action(None, None, "file_deleted", Some(&json!({"path": path})), None, None).await?;
    Ok(Json(json!({ "ok": true })))
}

// ---- Heartbeat clock / force tick -----------------------------------------

/// Countdown info for the periodic heartbeat loop — last/next tick time and
/// its interval. Same for every project (the loop processes all running
/// projects in one pass), so this isn't project-scoped.
pub async fn heartbeat_status(State(state): State<AppState>) -> ApiResult<Json<serde_json::Value>> {
    Ok(Json(state.heartbeat_clock.status()))
}

/// Forces one heartbeat tick for this project right now, independent of the
/// periodic loop's own countdown — useful when a project is stuck on a
/// transient error (e.g. "Instance not ready") and waiting for the next
/// automatic tick is unnecessary.
pub async fn force_heartbeat(State(state): State<AppState>, Path(id): Path<String>) -> ApiResult<Json<serde_json::Value>> {
    crate::heartbeat::force_tick(&state, &id).await?;
    state.store.log_action(Some(&id), None, "heartbeat_forced", None, None, None).await?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
pub struct SetHeartbeatIntervalRequest {
    pub heartbeat_interval_secs: u64,
}

/// Per-project heartbeat cadence override (default 15 minutes, see
/// `models::default_heartbeat_interval_secs`). Takes effect on this
/// project's next due-check — no restart needed.
pub async fn set_heartbeat_interval(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<SetHeartbeatIntervalRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    state.store.set_heartbeat_interval(&id, req.heartbeat_interval_secs).await?;
    state
        .store
        .log_action(Some(&id), None, "heartbeat_interval_updated", Some(&json!({"heartbeat_interval_secs": req.heartbeat_interval_secs})), None, None)
        .await?;
    let project = state.store.get_project(&id).await?;
    Ok(Json(json!({ "project": project })))
}

#[derive(Deserialize)]
pub struct SetProjectNameRequest {
    pub name: String,
}

pub async fn set_project_name(State(state): State<AppState>, Path(id): Path<String>, Json(req): Json<SetProjectNameRequest>) -> ApiResult<Response> {
    let name = req.name.trim();
    if name.is_empty() {
        return Ok((StatusCode::BAD_REQUEST, Json(json!({"error": "project name is required"}))).into_response());
    }
    if name.chars().count() > 120 {
        return Ok((StatusCode::BAD_REQUEST, Json(json!({"error": "project name must be 120 characters or fewer"}))).into_response());
    }
    let Some(existing) = state.store.get_project(&id).await? else {
        return Ok((StatusCode::NOT_FOUND, Json(json!({"error": "project not found"}))).into_response());
    };
    state.store.set_project_name(&id, name).await?;
    state.store.log_action(Some(&id), None, "project_renamed", Some(&json!({"from": existing.name, "to": name})), None, None).await?;
    let project = state.store.get_project(&id).await?;
    Ok(Json(json!({ "project": project })).into_response())
}

#[derive(Deserialize)]
pub struct SetGoalRequest {
    pub goal: String,
}

pub async fn set_goal(State(state): State<AppState>, Path(id): Path<String>, Json(req): Json<SetGoalRequest>) -> ApiResult<Json<serde_json::Value>> {
    state.store.set_goal(&id, &req.goal).await?;
    state.store.log_action(Some(&id), None, "goal_updated", None, None, None).await?;
    let project = state.store.get_project(&id).await?;
    Ok(Json(json!({ "project": project })))
}

#[derive(Deserialize)]
pub struct SetHeartbeatPromptRequest {
    #[serde(default)]
    pub heartbeat_prompt: String,
}

/// Empty string clears it back to `None` ("use judgment") — same convention
/// as other clearable fields in this API.
pub async fn set_heartbeat_prompt(State(state): State<AppState>, Path(id): Path<String>, Json(req): Json<SetHeartbeatPromptRequest>) -> ApiResult<Json<serde_json::Value>> {
    state.store.set_heartbeat_prompt(&id, &req.heartbeat_prompt).await?;
    state.store.log_action(Some(&id), None, "heartbeat_prompt_updated", None, None, None).await?;
    let project = state.store.get_project(&id).await?;
    Ok(Json(json!({ "project": project })))
}

/// Read-only — memory is conductor-authored (overwritten every tick), not
/// user-editable like `agent_prompts/`.
pub async fn get_memory(State(state): State<AppState>, Path(id): Path<String>) -> ApiResult<Json<serde_json::Value>> {
    let memory = state.store.read_memory(&id).await?;
    Ok(Json(json!({ "memory": memory })))
}

#[derive(Deserialize)]
pub struct RenameInstanceRequest {
    pub name: String,
}

/// Renames this project's vape instance (not the project itself). 404s
/// cleanly if the project has no instance yet.
pub async fn rename_instance(State(state): State<AppState>, Path(id): Path<String>, Json(req): Json<RenameInstanceRequest>) -> ApiResult<Json<serde_json::Value>> {
    let project = state.store.get_project(&id).await?.ok_or_else(|| anyhow::anyhow!("project not found"))?;
    let instance_id = project.vape_instance_id.ok_or_else(|| anyhow::anyhow!("project has no instance yet"))?;
    let resp = state.vape.rename_instance(&instance_id, &req.name).await?;
    state
        .store
        .log_action(Some(&id), Some(&instance_id), "instance_renamed", Some(&json!({"name": req.name})), Some(&resp.to_string()), None)
        .await?;
    Ok(Json(json!({ "result": resp })))
}

// ---- Archetypes (reusable agent personas) ---------------------------------

pub async fn list_archetypes(State(state): State<AppState>) -> ApiResult<Json<serde_json::Value>> {
    let archetypes = state.store.list_archetypes().await?;
    Ok(Json(json!({ "archetypes": archetypes })))
}

#[derive(Deserialize)]
pub struct CreateArchetypeRequest {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub preferred_model: Option<String>,
}

pub async fn create_archetype(State(state): State<AppState>, Json(req): Json<CreateArchetypeRequest>) -> ApiResult<Json<serde_json::Value>> {
    let archetype = state.store.create_archetype(&req.name, &req.description, req.preferred_model.as_deref()).await?;
    state.store.log_action(None, None, "archetype_created", Some(&json!({"slug": archetype.slug})), None, None).await?;
    Ok(Json(json!({ "archetype": archetype })))
}

pub async fn get_archetype(State(state): State<AppState>, Path(slug): Path<String>) -> ApiResult<Response> {
    match state.store.get_archetype(&slug).await? {
        Some(a) => Ok(Json(json!({ "archetype": a })).into_response()),
        None => Ok((StatusCode::NOT_FOUND, Json(json!({"error": "archetype not found"}))).into_response()),
    }
}

#[derive(Deserialize, Default)]
pub struct UpdateArchetypeRequest {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub preferred_model: Option<String>,
}

pub async fn update_archetype(State(state): State<AppState>, Path(slug): Path<String>, Json(req): Json<UpdateArchetypeRequest>) -> ApiResult<Json<serde_json::Value>> {
    let archetype = state
        .store
        .update_archetype(&slug, req.name.as_deref(), req.description.as_deref(), req.preferred_model.as_deref())
        .await?;
    state.store.log_action(None, None, "archetype_updated", Some(&json!({"slug": slug})), None, None).await?;
    Ok(Json(json!({ "archetype": archetype })))
}

pub async fn delete_archetype(State(state): State<AppState>, Path(slug): Path<String>) -> ApiResult<Json<serde_json::Value>> {
    state.store.delete_archetype(&slug).await?;
    state.store.log_action(None, None, "archetype_deleted", Some(&json!({"slug": slug})), None, None).await?;
    Ok(Json(json!({ "ok": true })))
}

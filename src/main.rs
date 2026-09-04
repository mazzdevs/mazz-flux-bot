use std::path::PathBuf;
use std::sync::Arc;

use axum::routing::{get, post};
use axum::Router;
use tower_http::services::ServeDir;
use tower_http::trace::TraceLayer;

use mazz_flux_bot::conductor::Conductor;
use mazz_flux_bot::heartbeat::HeartbeatClock;
use mazz_flux_bot::store::Store;
use mazz_flux_bot::vape_client::VapeClient;
use mazz_flux_bot::{api, heartbeat, state_repo, AppState};

fn log_conductor_status() {
    let conductor = Conductor::from_env();
    if conductor.enabled() {
        tracing::info!(backend = conductor.label(), "conductor configured");
    } else {
        tracing::warn!("no conductor configured (set OPENROUTER_API_KEY) — heartbeat will observe instances but won't make steering decisions");
    }
}

/// Default data dir is a sibling of this repo's checkout (`../mazz-flux-bot-state`),
/// not a subdirectory of it — keeps it out of this git repo entirely (no nested
/// `.git`, nothing to gitignore) so it can be its own separate, persistent
/// state repo. Override with `MAZZ_FLUX_DATA_DIR` (e.g. an absolute path on a
/// persistent volume).
fn data_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("MAZZ_FLUX_DATA_DIR") {
        return PathBuf::from(dir);
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let repo_name = cwd.file_name().and_then(|n| n.to_str()).unwrap_or("mazz-flux-bot");
    cwd.parent().map(|p| p.join(format!("{repo_name}-state"))).unwrap_or_else(|| PathBuf::from("../mazz-flux-bot-state"))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();

    let dir = data_dir();

    // CLI escape hatch: `cargo run -- commit-state` snapshots the state repo
    // and exits, without booting the HTTP server or heartbeat loop. Useful
    // from cron/manual use on a flux instance.
    if std::env::args().nth(1).as_deref() == Some("commit-state") {
        let message = std::env::args().nth(2).unwrap_or_else(|| format!("manual snapshot {}", chrono::Utc::now().to_rfc3339()));
        let summary = state_repo::commit(&dir, &message).await?;
        println!("{}", serde_json::to_string_pretty(&summary)?);
        return Ok(());
    }

    let store = Arc::new(Store::open(&dir).await?);
    tracing::info!(data_dir = %dir.display(), "file store ready");
    state_repo::ensure_init(&dir).await.unwrap_or_else(|e| tracing::warn!(error = %e, "state repo init failed — commits will fail until this is fixed"));
    store.seed_default_archetypes().await.unwrap_or_else(|e| tracing::warn!(error = %e, "failed to seed default archetypes"));

    let vape = Arc::new(VapeClient::new());
    log_conductor_status();

    // Scan-loop cadence (how often we check which projects are due) —
    // distinct from each project's own heartbeat interval, which defaults to
    // 15 minutes and is editable per project (see `models::Project`).
    let interval_secs: u64 = std::env::var("HEARTBEAT_SCAN_INTERVAL_SECS").ok().and_then(|v| v.parse().ok()).unwrap_or(heartbeat::DEFAULT_SCAN_INTERVAL_SECS);
    let heartbeat_clock = Arc::new(HeartbeatClock::new(interval_secs));

    let state = AppState { store, vape, heartbeat_clock };

    tokio::spawn(heartbeat::run(state.clone()));

    let api_routes = Router::new()
        .route("/api/projects", get(api::list_projects).post(api::create_project))
        .route("/api/projects/{id}", get(api::get_project).delete(api::delete_project))
        .route("/api/projects/{id}/agent-context", get(api::get_agent_context))
        .route("/api/projects/{id}/start", post(api::start_project))
        .route("/api/projects/{id}/pause", post(api::pause_project))
        .route("/api/projects/{id}/message", post(api::send_project_message))
        .route("/api/projects/{id}/notes", get(api::list_project_notes))
        .route("/api/projects/{id}/kanban", get(api::get_kanban_board).post(api::create_kanban_task))
        .route("/api/projects/{project_id}/kanban/{task_id}", post(api::update_kanban_task).delete(api::delete_kanban_task))
        .route("/api/human-tasks", get(api::list_human_tasks))
        .route("/api/human-tasks/{id}/resolve", post(api::resolve_human_task))
        .route("/api/log", get(api::action_log))
        .route("/api/instances", get(api::list_instances))
        .route("/api/instances/{id}", get(api::get_instance))
        .route("/api/instances/{id}/status", get(api::get_instance_status))
        .route("/api/instances/{id}/session", get(api::get_instance_session))
        .route("/api/constellations", get(api::list_constellations))
        .route("/api/settings", get(api::get_settings).post(api::update_settings))
        .route("/api/state/commit", post(api::commit_state))
        .route("/api/files", get(api::browse_files).put(api::write_file).delete(api::delete_file))
        .route("/api/heartbeat/status", get(api::heartbeat_status))
        .route("/api/projects/{id}/heartbeat/force", post(api::force_heartbeat))
        .route("/api/projects/{id}/heartbeat-interval", post(api::set_heartbeat_interval))
        .route("/api/projects/{id}/name", post(api::set_project_name))
        .route("/api/projects/{id}/goal", post(api::set_goal))
        .route("/api/projects/{id}/heartbeat-prompt", post(api::set_heartbeat_prompt))
        .route("/api/projects/{id}/memory", get(api::get_memory))
        .route("/api/projects/{id}/instance/rename", post(api::rename_instance))
        .route("/api/archetypes", get(api::list_archetypes).post(api::create_archetype))
        .route("/api/archetypes/{slug}", get(api::get_archetype).post(api::update_archetype).delete(api::delete_archetype));

    let app = Router::new()
        .merge(api_routes)
        .fallback_service(ServeDir::new("static"))
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(mazz_flux_bot::public_url::DEFAULT_PORT);
    // 0.0.0.0 (not 127.0.0.1) so the port is reachable from outside the pod
    // when this runs inside a vape/flux instance — vape's port-detection
    // exposes it as a Link on the dashboard. Still fine on a laptop.
    let listener = tokio::net::TcpListener::bind(("0.0.0.0", port)).await?;
    tracing::info!(port, "mazz-flux-bot listening on http://0.0.0.0:{port}");
    axum::serve(listener, app).await?;

    Ok(())
}

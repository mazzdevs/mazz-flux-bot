use std::sync::Arc;

use axum::routing::{get, post};
use axum::Router;
use tower_http::services::ServeDir;
use tower_http::trace::TraceLayer;

use mazz_flux_bot::brain::Brain;
use mazz_flux_bot::vape_client::VapeClient;
use mazz_flux_bot::{api, db, heartbeat, AppState};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();

    let db_path = std::env::var("MAZZ_FLUX_DB_PATH").unwrap_or_else(|_| "mazz-flux-bot.db".to_string());
    let db = db::init_db(&db_path).await?;
    tracing::info!(db_path, "sqlite ready");

    let vape = Arc::new(VapeClient::new());
    let brain = Arc::new(Brain::from_env());
    match brain.as_ref() {
        Brain::Anthropic(_) => tracing::info!("brain: Anthropic"),
        Brain::OpenRouter(_) => tracing::info!("brain: OpenRouter"),
        Brain::Disabled => tracing::warn!("no ANTHROPIC_API_KEY or OPENROUTER_API_KEY set — heartbeat will observe instances but won't make steering decisions"),
    }

    let state = AppState { db, vape, brain };

    tokio::spawn(heartbeat::run(state.clone()));

    let api_routes = Router::new()
        .route("/api/projects", get(api::list_projects).post(api::create_project))
        .route("/api/projects/{id}", get(api::get_project).delete(api::delete_project))
        .route("/api/projects/{id}/start", post(api::start_project))
        .route("/api/projects/{id}/pause", post(api::pause_project))
        .route("/api/projects/{id}/message", post(api::send_project_message))
        .route("/api/log", get(api::action_log))
        .route("/api/instances", get(api::list_instances))
        .route("/api/instances/{id}", get(api::get_instance))
        .route("/api/instances/{id}/status", get(api::get_instance_status))
        .route("/api/instances/{id}/session", get(api::get_instance_session))
        .route("/api/constellations", get(api::list_constellations));

    let app = Router::new()
        .merge(api_routes)
        .fallback_service(ServeDir::new("static"))
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    let port: u16 = std::env::var("PORT").ok().and_then(|v| v.parse().ok()).unwrap_or(4270);
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
    tracing::info!(port, "mazz-flux-bot listening on http://127.0.0.1:{port}");
    axum::serve(listener, app).await?;

    Ok(())
}

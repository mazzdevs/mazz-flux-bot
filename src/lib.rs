pub mod anthropic_client;
pub mod api;
pub mod conductor;
pub mod db;
pub mod heartbeat;
pub mod models;
pub mod vape_client;

use std::sync::Arc;

use sqlx::SqlitePool;

use vape_client::VapeClient;

/// No `conductor` field here on purpose — the conductor is resolved fresh from the
/// DB/env each time it's needed (heartbeat ticks, the settings endpoint) so
/// a key saved via the settings UI takes effect immediately, with nothing to
/// go stale. See `conductor::Conductor::from_sources`.
#[derive(Clone)]
pub struct AppState {
    pub db: SqlitePool,
    pub vape: Arc<VapeClient>,
}

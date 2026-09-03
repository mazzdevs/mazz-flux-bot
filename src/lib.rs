pub mod anthropic_client;
pub mod api;
pub mod conductor;
pub mod heartbeat;
pub mod models;
pub mod state_repo;
pub mod store;
pub mod vape_client;

use std::sync::Arc;

use store::Store;
use vape_client::VapeClient;

/// No `conductor` field here on purpose — the conductor is resolved fresh from the
/// store/env each time it's needed (heartbeat ticks, the settings endpoint) so
/// a key saved via the settings UI takes effect immediately, with nothing to
/// go stale. See `conductor::Conductor::from_sources`.
#[derive(Clone)]
pub struct AppState {
    pub store: Arc<Store>,
    pub vape: Arc<VapeClient>,
}

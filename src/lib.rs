pub mod api;
pub mod conductor;
pub mod heartbeat;
pub mod models;
pub mod public_url;
pub mod state_repo;
pub mod store;
pub mod vape_client;

use std::sync::Arc;

use heartbeat::HeartbeatClock;
use store::Store;
use vape_client::VapeClient;

/// No `conductor` field here on purpose — resolved fresh from env each time
/// it's needed (heartbeat ticks). See `conductor::Conductor::from_env`.
#[derive(Clone)]
pub struct AppState {
    pub store: Arc<Store>,
    pub vape: Arc<VapeClient>,
    pub heartbeat_clock: Arc<HeartbeatClock>,
}

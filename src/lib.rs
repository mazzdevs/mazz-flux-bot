pub mod anthropic_client;
pub mod api;
pub mod brain;
pub mod db;
pub mod heartbeat;
pub mod models;
pub mod vape_client;

use std::sync::Arc;

use sqlx::SqlitePool;

use brain::Brain;
use vape_client::VapeClient;

#[derive(Clone)]
pub struct AppState {
    pub db: SqlitePool,
    pub vape: Arc<VapeClient>,
    pub brain: Arc<Brain>,
}

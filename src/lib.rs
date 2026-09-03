pub mod anthropic_client;
pub mod api;
pub mod db;
pub mod heartbeat;
pub mod models;
pub mod vape_client;

use std::sync::Arc;

use sqlx::SqlitePool;

use anthropic_client::AnthropicClient;
use vape_client::VapeClient;

#[derive(Clone)]
pub struct AppState {
    pub db: SqlitePool,
    pub vape: Arc<VapeClient>,
    pub anthropic: Arc<AnthropicClient>,
}

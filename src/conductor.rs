//! The heartbeat's "conductor" — whichever LLM backend is configured to make
//! steering decisions. Resolved fresh every heartbeat tick via
//! [`Conductor::from_sources`] (not cached in `AppState`), so a key saved through
//! the settings UI takes effect on the very next tick with no restart.
//!
//! Precedence, checked in this order — first match wins:
//! 1. `anthropic_api_key` row in the `settings` table (set via `POST
//!    /api/settings`, the web UI's Settings panel).
//! 2. `ANTHROPIC_API_KEY` env var.
//! 3. `openrouter_api_key` row in `settings`.
//! 4. `OPENROUTER_API_KEY` env var.
//! 5. Disabled — same "observe only" behavior as before.
//!
//! Anthropic (DB or env) always wins over OpenRouter (DB or env); within each
//! backend, a DB-stored key always wins over the env var of the same name.
//! Model overrides follow the same DB-then-env pattern
//! (`anthropic_model`/`ANTHROPIC_MODEL`, `openrouter_model`/`OPENROUTER_MODEL`).
//!
//! Secrets set via the UI are stored in sqlite (`settings` table) — plaintext,
//! same trust boundary as the rest of this single-user local tool's DB file;
//! never returned back to the browser once saved (see `api::get_settings`).

use anyhow::{anyhow, Context, Result};
use serde_json::json;
use crate::anthropic_client::AnthropicClient;
use crate::store::Store;

const DEFAULT_OPENROUTER_MODEL: &str = "openai/gpt-5.6-sol";
const OPENROUTER_API_URL: &str = "https://openrouter.ai/api/v1/chat/completions";

pub struct OpenRouterClient {
    http: reqwest::Client,
    api_key: Option<String>,
    model: String,
}

impl OpenRouterClient {
    pub fn new() -> Self {
        let api_key = std::env::var("OPENROUTER_API_KEY").ok().filter(|s| !s.is_empty());
        let model = std::env::var("OPENROUTER_MODEL").unwrap_or_else(|_| DEFAULT_OPENROUTER_MODEL.to_string());
        Self { http: reqwest::Client::new(), api_key, model }
    }

    /// For keys/models sourced from the settings DB rather than the process
    /// environment — see `Conductor::from_sources`.
    pub fn with_key(api_key: String, model: Option<String>) -> Self {
        Self { http: reqwest::Client::new(), api_key: Some(api_key), model: model.unwrap_or_else(|| DEFAULT_OPENROUTER_MODEL.to_string()) }
    }

    pub fn enabled(&self) -> bool {
        self.api_key.is_some()
    }

    pub async fn decide(&self, system: &str, user: &str) -> Result<String> {
        let api_key = self.api_key.as_ref().ok_or_else(|| anyhow!("OPENROUTER_API_KEY not set"))?;

        let body = json!({
            "model": self.model,
            "max_tokens": 1024,
            "messages": [
                {"role": "system", "content": system},
                {"role": "user", "content": user},
            ],
        });

        // Override for local testing against a mock server — mirrors
        // vape_client.rs's CADMIUM_VAPE_URL pattern. Not meant for production
        // use (there's only one real OpenRouter endpoint).
        let url = std::env::var("OPENROUTER_API_URL").unwrap_or_else(|_| OPENROUTER_API_URL.to_string());
        let resp = self
            .http
            .post(url)
            .bearer_auth(api_key)
            .json(&body)
            .send()
            .await
            .context("failed to send request to openrouter.ai")?;

        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(anyhow!("OpenRouter API {status}: {text}"));
        }

        let parsed: serde_json::Value = serde_json::from_str(&text).with_context(|| format!("failed to parse OpenRouter response: {text}"))?;
        let content = parsed
            .get("choices")
            .and_then(|c| c.as_array())
            .and_then(|choices| choices.first())
            .and_then(|choice| choice.get("message"))
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_str())
            .unwrap_or_default()
            .to_string();

        if content.is_empty() {
            return Err(anyhow!("OpenRouter response had no message content: {text}"));
        }
        Ok(content)
    }
}

impl Default for OpenRouterClient {
    fn default() -> Self {
        Self::new()
    }
}

/// Dispatches to whichever backend is configured. `ANTHROPIC_API_KEY` wins if
/// both are set — see module docs.
pub enum Conductor {
    Anthropic(AnthropicClient),
    OpenRouter(OpenRouterClient),
    Disabled,
}

/// A setting value, checking the DB row first and falling back to the env
/// var of the same purpose. `None` if neither is set (or the DB row is an
/// empty string, which `Store::set_setting` treats as "cleared").
async fn resolve(store: &Store, db_key: &str, env_key: &str) -> Option<String> {
    if let Ok(Some(v)) = store.get_setting(db_key).await {
        if !v.is_empty() {
            return Some(v);
        }
    }
    std::env::var(env_key).ok().filter(|s| !s.is_empty())
}

impl Conductor {
    /// Env-only construction — kept for anything that doesn't have a DB
    /// handle (e.g. quick manual testing). Runtime code should use
    /// `from_sources` so settings-UI keys are honored.
    pub fn from_env() -> Self {
        let anthropic = AnthropicClient::new();
        if anthropic.enabled() {
            return Conductor::Anthropic(anthropic);
        }
        let openrouter = OpenRouterClient::new();
        if openrouter.enabled() {
            return Conductor::OpenRouter(openrouter);
        }
        Conductor::Disabled
    }

    /// Resolves DB settings first, env vars as fallback — see module docs
    /// for the exact precedence. Called fresh every heartbeat tick.
    pub async fn from_sources(store: &Store) -> Self {
        if let Some(key) = resolve(store, "anthropic_api_key", "ANTHROPIC_API_KEY").await {
            let model = resolve(store, "anthropic_model", "ANTHROPIC_MODEL").await;
            return Conductor::Anthropic(AnthropicClient::with_key(key, model));
        }
        if let Some(key) = resolve(store, "openrouter_api_key", "OPENROUTER_API_KEY").await {
            let model = resolve(store, "openrouter_model", "OPENROUTER_MODEL").await;
            return Conductor::OpenRouter(OpenRouterClient::with_key(key, model));
        }
        Conductor::Disabled
    }

    pub fn enabled(&self) -> bool {
        !matches!(self, Conductor::Disabled)
    }

    pub fn label(&self) -> &'static str {
        match self {
            Conductor::Anthropic(_) => "anthropic",
            Conductor::OpenRouter(_) => "openrouter",
            Conductor::Disabled => "none",
        }
    }

    pub async fn decide(&self, system: &str, user: &str) -> Result<String> {
        match self {
            Conductor::Anthropic(c) => c.decide(system, user).await,
            Conductor::OpenRouter(c) => c.decide(system, user).await,
            Conductor::Disabled => Err(anyhow!("no conductor configured (set an API key via Settings, ANTHROPIC_API_KEY, or OPENROUTER_API_KEY)")),
        }
    }
}

/// Masked view of current settings for the UI — never carries a raw secret,
/// only whether one is set and (if so) a `sk-...ab12`-style last-4 preview.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SettingsStatus {
    pub active_backend: &'static str,
    pub anthropic_key_set: bool,
    pub anthropic_key_preview: Option<String>,
    pub anthropic_model: String,
    pub openrouter_key_set: bool,
    pub openrouter_key_preview: Option<String>,
    pub openrouter_model: String,
}

fn preview(key: &str) -> String {
    if key.len() <= 4 {
        "*".repeat(key.len())
    } else {
        format!("...{}", &key[key.len() - 4..])
    }
}

pub async fn settings_status(store: &Store) -> SettingsStatus {
    let anthropic_key = resolve(store, "anthropic_api_key", "ANTHROPIC_API_KEY").await;
    let openrouter_key = resolve(store, "openrouter_api_key", "OPENROUTER_API_KEY").await;
    let active_backend = if anthropic_key.is_some() { "anthropic" } else if openrouter_key.is_some() { "openrouter" } else { "none" };

    SettingsStatus {
        active_backend,
        anthropic_key_set: anthropic_key.is_some(),
        anthropic_key_preview: anthropic_key.as_deref().map(preview),
        anthropic_model: resolve(store, "anthropic_model", "ANTHROPIC_MODEL").await.unwrap_or_else(|| crate::anthropic_client::DEFAULT_MODEL.to_string()),
        openrouter_key_set: openrouter_key.is_some(),
        openrouter_key_preview: openrouter_key.as_deref().map(preview),
        openrouter_model: resolve(store, "openrouter_model", "OPENROUTER_MODEL").await.unwrap_or_else(|| DEFAULT_OPENROUTER_MODEL.to_string()),
    }
}

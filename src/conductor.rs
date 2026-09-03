//! The heartbeat's "conductor" — the LLM backend that makes steering
//! decisions. OpenRouter only, API key from `OPENROUTER_API_KEY` env var
//! only (no settings-UI key management). **Model** selection, for both the
//! conductor itself and the vape instances it spawns, is settings-UI
//! adjustable — see `resolve_model` and `api::get_settings`/`update_settings`.

use anyhow::{anyhow, Context, Result};
use serde_json::json;

use crate::store::Store;

pub const DEFAULT_MODEL: &str = "openai/gpt-5.6-sol";
const OPENROUTER_API_URL: &str = "https://openrouter.ai/api/v1/chat/completions";

pub struct OpenRouterClient {
    http: reqwest::Client,
    api_key: Option<String>,
    model: String,
}

impl OpenRouterClient {
    /// `model` resolution is env-only here — use `Conductor::from_sources`
    /// for the DB-aware (settings-UI) version used at runtime.
    pub fn new() -> Self {
        let api_key = std::env::var("OPENROUTER_API_KEY").ok().filter(|s| !s.is_empty());
        let model = std::env::var("OPENROUTER_MODEL").unwrap_or_else(|_| DEFAULT_MODEL.to_string());
        Self { http: reqwest::Client::new(), api_key, model }
    }

    pub fn with_model(model: String) -> Self {
        let api_key = std::env::var("OPENROUTER_API_KEY").ok().filter(|s| !s.is_empty());
        Self { http: reqwest::Client::new(), api_key, model }
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

/// Thin wrapper so callers (heartbeat.rs) don't need to know there's only
/// one backend today — kept as an enum in case another backend is added
/// later.
pub enum Conductor {
    OpenRouter(OpenRouterClient),
    Disabled,
}

/// A settings.json value, checking the DB row first and falling back to the
/// env var of the same purpose, then a hardcoded default. Model-only —
/// no API keys go through this path anymore.
pub async fn resolve_model(store: &Store, db_key: &str, env_key: &str, default: &str) -> String {
    if let Ok(Some(v)) = store.get_setting(db_key).await {
        if !v.is_empty() {
            return v;
        }
    }
    std::env::var(env_key).ok().filter(|s| !s.is_empty()).unwrap_or_else(|| default.to_string())
}

impl Conductor {
    /// Env-only construction — kept for anything that doesn't have a store
    /// handle (e.g. quick manual testing).
    pub fn from_env() -> Self {
        let openrouter = OpenRouterClient::new();
        if openrouter.enabled() {
            return Conductor::OpenRouter(openrouter);
        }
        Conductor::Disabled
    }

    /// Resolves the conductor's own model from `settings.json` (key
    /// `conductor_model`) with `OPENROUTER_MODEL` env fallback, default
    /// `DEFAULT_MODEL`. API key is still env-only. Called fresh every
    /// heartbeat tick so a model saved through the settings UI takes effect
    /// on the very next tick, no restart.
    pub async fn from_sources(store: &Store) -> Self {
        let model = resolve_model(store, "conductor_model", "OPENROUTER_MODEL", DEFAULT_MODEL).await;
        let client = OpenRouterClient::with_model(model);
        if client.enabled() {
            Conductor::OpenRouter(client)
        } else {
            Conductor::Disabled
        }
    }

    pub fn enabled(&self) -> bool {
        !matches!(self, Conductor::Disabled)
    }

    pub fn label(&self) -> &'static str {
        match self {
            Conductor::OpenRouter(_) => "openrouter",
            Conductor::Disabled => "none",
        }
    }

    pub async fn decide(&self, system: &str, user: &str) -> Result<String> {
        match self {
            Conductor::OpenRouter(c) => c.decide(system, user).await,
            Conductor::Disabled => Err(anyhow!("no conductor configured (set OPENROUTER_API_KEY)")),
        }
    }

    /// Best-effort: asks the conductor for a short, k8s-safe instance-name
    /// slug summarizing this project. Callers MUST still run the result
    /// through `heartbeat::slugify` before trusting it as a resource name —
    /// this only asks the model to *try* to produce something clean, it
    /// doesn't sanitize on its own. Any failure (disabled conductor, network
    /// error, empty response) should be treated as "no suggestion" by the
    /// caller, never as a hard error — instance creation must never block on
    /// this.
    pub async fn suggest_instance_slug(&self, project_name: &str, goal: &str) -> Result<String> {
        const SYSTEM: &str = "You name Kubernetes resources. Respond with ONLY a short slug: \
            lowercase letters, digits, and hyphens only, 2-4 words, no prose, no punctuation \
            besides hyphens, at most 24 characters. Summarize the project below into that slug.";
        let user = format!("Project name: {project_name}\nGoal: {goal}");
        self.decide(SYSTEM, &user).await
    }

    /// Best-effort: asks the conductor to write the FIRST message to a new
    /// coding-agent session, in its own words, directing it toward `goal` —
    /// never the raw goal text sent verbatim. Callers must fall back to
    /// `goal.to_string()` on any failure/empty response (see
    /// `heartbeat::CreateInstanceNode`) — this must never block instance
    /// creation.
    pub async fn compose_initial_prompt(&self, project_name: &str, goal: &str) -> Result<String> {
        const SYSTEM: &str = "You are opening a new coding-agent session on behalf of a developer. \
            Write the first message to the agent, in your own words, directing it toward the \
            goal below. Be concrete and actionable — give the agent clear direction on how to \
            start, not just a restatement of the goal. Respond with ONLY the message text, no \
            preamble, no quotes around it, no markdown fences.";
        let user = format!("Project name: {project_name}\nGoal: {goal}");
        self.decide(SYSTEM, &user).await
    }

    /// Best-effort: asks the conductor for a short, human-readable project
    /// name summarizing `goal`, for when the user leaves the name field
    /// blank at creation time. Callers must fall back to a placeholder on
    /// any failure/empty response — see `api::create_project`.
    pub async fn suggest_project_name(&self, goal: &str) -> Result<String> {
        const SYSTEM: &str = "Respond with ONLY a short, human-readable project name (3-6 words, \
            title case, no punctuation besides spaces and hyphens, no quotes) summarizing the \
            goal below. No prose, no preamble.";
        self.decide(SYSTEM, goal).await
    }
}

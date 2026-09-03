//! The heartbeat's "brain" — whichever LLM backend is configured to make
//! steering decisions. Two backends, selected by which API key is present:
//!
//! - `ANTHROPIC_API_KEY` set → direct `api.anthropic.com` call (see
//!   `anthropic_client.rs`). Checked first, so existing setups keep working
//!   unchanged.
//! - else `OPENROUTER_API_KEY` set → OpenRouter's OpenAI-compatible
//!   `/chat/completions`, model selectable via `OPENROUTER_MODEL` (default
//!   `openai/gpt-5.6-sol` — any OpenRouter-listed model id works, including
//!   Anthropic ones routed through OpenRouter instead of direct).
//! - neither set → disabled, same "observe only" behavior as before.

use anyhow::{anyhow, Context, Result};
use serde_json::json;

use crate::anthropic_client::AnthropicClient;

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

        let resp = self
            .http
            .post(OPENROUTER_API_URL)
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
pub enum Brain {
    Anthropic(AnthropicClient),
    OpenRouter(OpenRouterClient),
    Disabled,
}

impl Brain {
    pub fn from_env() -> Self {
        let anthropic = AnthropicClient::new();
        if anthropic.enabled() {
            return Brain::Anthropic(anthropic);
        }
        let openrouter = OpenRouterClient::new();
        if openrouter.enabled() {
            return Brain::OpenRouter(openrouter);
        }
        Brain::Disabled
    }

    pub fn enabled(&self) -> bool {
        !matches!(self, Brain::Disabled)
    }

    pub async fn decide(&self, system: &str, user: &str) -> Result<String> {
        match self {
            Brain::Anthropic(c) => c.decide(system, user).await,
            Brain::OpenRouter(c) => c.decide(system, user).await,
            Brain::Disabled => Err(anyhow!("no brain configured (set ANTHROPIC_API_KEY or OPENROUTER_API_KEY)")),
        }
    }
}

use anyhow::{anyhow, Context, Result};
use serde_json::json;

const DEFAULT_MODEL: &str = "claude-sonnet-5";
const API_URL: &str = "https://api.anthropic.com/v1/messages";

/// The "brain" the heartbeat loop consults to decide what to do about a
/// project's instance. Standard direct Anthropic API call — no internal
/// WARP-gated gateway was found (vape's own `internal/llm` package calls
/// `api.anthropic.com` directly with `ANTHROPIC_API_KEY` too, so this mirrors
/// that). Requires a normal internet connection, not WARP (WARP is only
/// needed for the vape-manager calls in vape_client.rs).
pub struct AnthropicClient {
    http: reqwest::Client,
    api_key: Option<String>,
    model: String,
}

impl AnthropicClient {
    pub fn new() -> Self {
        let api_key = std::env::var("ANTHROPIC_API_KEY").ok().filter(|s| !s.is_empty());
        let model = std::env::var("ANTHROPIC_MODEL").unwrap_or_else(|_| DEFAULT_MODEL.to_string());
        Self { http: reqwest::Client::new(), api_key, model }
    }

    pub fn enabled(&self) -> bool {
        self.api_key.is_some()
    }

    /// One-shot decision call: system prompt sets up the orchestrator's role,
    /// user prompt carries the project goal + current instance state. Returns
    /// the model's raw text response (heartbeat.rs is responsible for parsing
    /// whatever structure it asked for).
    pub async fn decide(&self, system: &str, user: &str) -> Result<String> {
        let api_key = self.api_key.as_ref().ok_or_else(|| anyhow!("ANTHROPIC_API_KEY not set"))?;

        let body = json!({
            "model": self.model,
            "max_tokens": 1024,
            "system": system,
            "messages": [{"role": "user", "content": user}],
        });

        let resp = self
            .http
            .post(API_URL)
            .header("x-api-key", api_key)
            .header("anthropic-version", "2023-06-01")
            .json(&body)
            .send()
            .await
            .context("failed to send request to api.anthropic.com")?;

        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(anyhow!("Anthropic API {status}: {text}"));
        }

        let parsed: serde_json::Value = serde_json::from_str(&text).with_context(|| format!("failed to parse Anthropic response: {text}"))?;
        let content = parsed
            .get("content")
            .and_then(|c| c.as_array())
            .map(|blocks| {
                blocks
                    .iter()
                    .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default();

        if content.is_empty() {
            return Err(anyhow!("Anthropic response had no text content: {text}"));
        }
        Ok(content)
    }
}

impl Default for AnthropicClient {
    fn default() -> Self {
        Self::new()
    }
}

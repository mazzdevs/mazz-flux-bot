use anyhow::{Context, Result, anyhow};
use serde::de::DeserializeOwned;
use tokio::process::Command;
use tracing::{info, warn};

use crate::models::{
    AgentStatus, Constellation, CreateInstanceRequest, PidaSession, PidaStatus, VapeInstance,
};

/// Only confirmed no-create failures allow another create on the same relationship.
#[derive(Debug, thiserror::Error)]
pub enum CreateFailure {
    #[error("{0}")]
    Definitive(String),
    #[error("{0}")]
    Ambiguous(String),
}
impl CreateFailure {
    pub fn is_ambiguous(&self) -> bool {
        matches!(self, Self::Ambiguous(_))
    }
}

const DEFAULT_BASE_URL: &str = "https://vape.stable.dexus.io";

/// Talks to the vape-manager REST API exactly the way `cadmium` itself does:
/// Bearer auth using `gh auth token`, no separate credential store. See
/// PLAN.md for how each endpoint below was confirmed.
pub struct VapeClient {
    http: reqwest::Client,
    base_url: String,
    /// Mutating calls (create/start/stop/delete/send) are no-ops (logged, not
    /// fired) if MAZZ_FLUX_LIVE is explicitly set to "0"/"false". Live by
    /// default — reads always fire (they're safe), and this tool's whole
    /// point is driving real vape instances, so dry-run is the opt-in
    /// escape hatch now, not the default.
    pub live: bool,
    cache: tokio::sync::Mutex<
        Option<(
            std::time::Instant,
            String,
            std::result::Result<Vec<VapeInstance>, String>,
        )>,
    >,
}

impl VapeClient {
    pub fn new() -> Self {
        // Base URL precedence: explicit test override, then the in-cluster
        // vape-manager service (present as VAPE_MANAGER_URL when running
        // *inside* a vape/flux instance — reaches vape-manager directly over
        // the cluster network, no Cloudflare WARP needed), then the public
        // WARP-gated URL for when this runs off-cluster (e.g. a laptop).
        let base_url = std::env::var("CADMIUM_VAPE_URL")
            .or_else(|_| std::env::var("VAPE_MANAGER_URL"))
            .unwrap_or_else(|_| DEFAULT_BASE_URL.to_string());
        let live = std::env::var("MAZZ_FLUX_LIVE")
            .map(|v| v != "0" && !v.eq_ignore_ascii_case("false"))
            .unwrap_or(true);
        if !live {
            warn!(
                "MAZZ_FLUX_LIVE=0 — mutating vape calls (create/start/stop/delete/send) will be logged but NOT fired"
            );
        }
        Self {
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(15))
                .build()
                .expect("HTTP client"),
            base_url,
            live,
            cache: tokio::sync::Mutex::new(None),
        }
    }

    /// Same credential cadmium uses: shell out to `gh auth token`. No token
    /// storage in this tool at all.
    async fn auth_token(&self) -> Result<String> {
        let out = Command::new("gh")
            .args(["auth", "token"])
            // Guardian may inject an ephemeral token that VAPE Manager does
            // not accept. Read the credential persisted by `gh auth login`.
            .env_remove("GH_TOKEN")
            .env_remove("GITHUB_TOKEN")
            .output()
            .await
            .context("failed to run `gh auth token` — is the GitHub CLI installed?")?;
        if !out.status.success() {
            return Err(anyhow!(
                "`gh auth token` failed ({}). Run `gh auth login` first — mazz-flux-bot uses the same credential cadmium does.",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        Ok(String::from_utf8(out.stdout)?.trim().to_string())
    }

    async fn get_text(&self, path: &str) -> Result<String> {
        let token = self.auth_token().await?;
        let url = format!("{}{}", self.base_url, path);
        let resp = self
            .http
            .get(&url)
            .bearer_auth(token)
            .send()
            .await
            .with_context(|| format!("GET {url} failed to send — are you on Cloudflare WARP?"))?;
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(anyhow!("GET {url} -> {status}: {body}"));
        }
        Ok(body)
    }

    async fn get_json<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        let body = self.get_text(path).await?;
        serde_json::from_str(&body)
            .with_context(|| format!("failed to parse JSON from {path}: {body}"))
    }

    /// Fires (or, in dry-run mode, logs and skips) a mutating call. Returns the
    /// parsed response on success, or a `{"dry_run": true, ...}` marker value
    /// when not live — callers should check for that key before treating the
    /// result as real.
    async fn mutate(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<&serde_json::Value>,
    ) -> Result<serde_json::Value> {
        if !self.live {
            info!(%path, ?body, "[dry-run] would {method} {path}");
            return Ok(serde_json::json!({
                "dry_run": true,
                "method": method.to_string(),
                "path": path,
                "body": body,
            }));
        }

        let token = self.auth_token().await?;
        let url = format!("{}{}", self.base_url, path);
        let mut req = self.http.request(method.clone(), &url).bearer_auth(token);
        if let Some(b) = body {
            req = req.json(b);
        }
        let resp = req
            .send()
            .await
            .with_context(|| format!("{method} {url} failed to send"))?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(anyhow!("{method} {url} -> {status}: {text}"));
        }
        if text.is_empty() {
            Ok(serde_json::json!({"ok": true}))
        } else {
            serde_json::from_str(&text).or_else(|_| Ok(serde_json::json!({"raw": text})))
        }
    }

    // ---- Confirmed-live reads --------------------------------------------

    pub async fn me(&self) -> Result<serde_json::Value> {
        self.get_json("/api/v1/me").await
    }

    /// Returns the raw response text (for caching) alongside the parsed list.
    pub async fn list_instances(&self) -> Result<(String, Vec<VapeInstance>)> {
        let text = self.get_text("/api/v1/instances").await?;
        let parsed: Vec<VapeInstance> = serde_json::from_str(&text)
            .with_context(|| format!("failed to parse instance list: {text}"))?;
        Ok((text, parsed))
    }

    pub async fn list_instances_cached(&self) -> Result<(Vec<VapeInstance>, String)> {
        let mut cache = self.cache.lock().await;
        if let Some((at, fetched_at, result)) = &*cache {
            if at.elapsed() < std::time::Duration::from_secs(15) {
                return result
                    .clone()
                    .map(|items| (items, fetched_at.clone()))
                    .map_err(|error| anyhow!(error));
            }
        }
        let result = self
            .list_instances()
            .await
            .map(|(_, items)| items)
            .map_err(|_| "VAPE status unavailable".to_string());
        let fetched_at = crate::projects::timestamp();
        *cache = Some((
            std::time::Instant::now(),
            fetched_at.clone(),
            result.clone(),
        ));
        result
            .map(|items| (items, fetched_at))
            .map_err(|error| anyhow!(error))
    }

    pub async fn get_instance(&self, id: &str) -> Result<VapeInstance> {
        self.get_json(&format!("/api/v1/instances/{id}")).await
    }

    pub async fn agent_status(&self, id: &str) -> Result<AgentStatus> {
        self.get_json(&format!("/api/v1/instances/{id}/agent-status"))
            .await
    }

    pub async fn list_constellations(&self) -> Result<Vec<Constellation>> {
        self.get_json("/api/v1/constellations").await
    }

    // ---- Harness-scoped reads (pida-focused; bilda paths mirror these) ---

    pub async fn pida_status(&self, id: &str) -> Result<PidaStatus> {
        self.get_json(&format!("/api/v1/instances/{id}/pida/api/status"))
            .await
    }

    pub async fn pida_session(&self, id: &str) -> Result<PidaSession> {
        self.get_json(&format!("/api/v1/instances/{id}/pida/api/session"))
            .await
    }

    pub async fn pida_job_result(&self, id: &str) -> Result<serde_json::Value> {
        self.get_json(&format!("/api/v1/instances/{id}/pida/api/job/result"))
            .await
    }

    // ---- Mutating calls (dry-run gated by MAZZ_FLUX_LIVE) ----------------

    /// Validate configuration and credentials before persisting a sent-attempt marker.
    pub async fn prepare_create(
        &self,
        req: &CreateInstanceRequest,
    ) -> std::result::Result<reqwest::Request, CreateFailure> {
        if !self.live {
            return Err(CreateFailure::Definitive(
                "VAPE mutations are disabled".into(),
            ));
        }
        let token = self.auth_token().await.map_err(|_| {
            CreateFailure::Definitive(
                "VAPE authentication unavailable; correct credentials and retry".into(),
            )
        })?;
        self.http
            .post(format!("{}/api/v1/instances", self.base_url))
            .bearer_auth(token)
            .json(req)
            .build()
            .map_err(|_| {
                CreateFailure::Definitive(
                    "invalid VAPE request configuration; correct it and retry".into(),
                )
            })
    }

    pub async fn send_create(
        &self,
        request: reqwest::Request,
    ) -> std::result::Result<serde_json::Value, CreateFailure> {
        let response = self.http.execute(request).await.map_err(|error| {
            if error.is_connect() || error.is_builder() {
                CreateFailure::Definitive(
                    "VAPE create could not connect; retry after correcting connectivity".into(),
                )
            } else {
                CreateFailure::Ambiguous(
                    "VAPE create timed out or transport outcome is unknown; reconcile before retry"
                        .into(),
                )
            }
        })?;
        let status = response.status();
        if !status.is_success() {
            let message = format!("VAPE create returned HTTP {}", status.as_u16());
            return Err(
                if matches!(
                    status.as_u16(),
                    400 | 401 | 403 | 404 | 405 | 413 | 415 | 422 | 429
                ) {
                    CreateFailure::Definitive(format!(
                        "{message}; correct the rejected request and retry"
                    ))
                } else {
                    CreateFailure::Ambiguous(format!(
                        "{message}; creation outcome uncertain, reconcile before retry"
                    ))
                },
            );
        }
        response.json().await.map_err(|_| {
            CreateFailure::Ambiguous(
                "VAPE create response could not be read; reconcile before retry".into(),
            )
        })
    }

    pub async fn start_instance(&self, id: &str) -> Result<serde_json::Value> {
        self.mutate(
            reqwest::Method::POST,
            &format!("/api/v1/instances/{id}/start"),
            None,
        )
        .await
    }

    pub async fn stop_instance(&self, id: &str) -> Result<serde_json::Value> {
        self.mutate(
            reqwest::Method::POST,
            &format!("/api/v1/instances/{id}/stop"),
            None,
        )
        .await
    }

    pub async fn delete_instance(&self, id: &str) -> Result<serde_json::Value> {
        self.mutate(
            reqwest::Method::DELETE,
            &format!("/api/v1/instances/{id}"),
            None,
        )
        .await
    }

    /// `POST /api/v1/instances/{id}/rename` — path extracted from the cadmium
    /// binary (see PLAN.md), first live-fired from the project detail page's
    /// rename control.
    pub async fn rename_instance(&self, id: &str, name: &str) -> Result<serde_json::Value> {
        let body = serde_json::json!({ "name": name });
        self.mutate(
            reqwest::Method::POST,
            &format!("/api/v1/instances/{id}/rename"),
            Some(&body),
        )
        .await
    }

    /// `POST .../pida/api/chat` — send a message into the instance's live pida
    /// session (steer it, answer a question in prose, etc).
    pub async fn pida_send(&self, id: &str, message: &str) -> Result<serde_json::Value> {
        let body = serde_json::json!({ "message": message });
        self.mutate(
            reqwest::Method::POST,
            &format!("/api/v1/instances/{id}/pida/api/chat"),
            Some(&body),
        )
        .await
    }
}

impl Default for VapeClient {
    fn default() -> Self {
        Self::new()
    }
}

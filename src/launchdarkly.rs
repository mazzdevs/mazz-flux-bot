//! Read-only LaunchDarkly REST enrichment. Tracking never grants targeting writes.
use crate::projects::FlagReference;
use serde::Serialize;
use serde_json::Value;
use std::{
    collections::HashMap,
    sync::OnceLock,
    time::{Duration, Instant},
};
use tokio::sync::Mutex;

#[derive(Debug, Clone, Serialize)]
pub struct FlagStatus {
    pub source: &'static str,
    pub available: bool,
    pub current_value: Option<Value>,
    pub targeting_on: Option<bool>,
    pub matches_fully_enabled: Option<bool>,
    pub fetched_at: String,
    pub error: Option<String>,
}
type Cache = HashMap<String, (Instant, Value, String)>;
static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
impl FlagStatus {
    pub fn unavailable(error: &str) -> Self {
        Self {
            source: "launchdarkly",
            available: false,
            current_value: None,
            targeting_on: None,
            matches_fully_enabled: None,
            fetched_at: crate::projects::timestamp(),
            error: Some(error.into()),
        }
    }
}
pub async fn status(flag: &FlagReference) -> FlagStatus {
    let token = match std::env::var("LD_ACCESS_TOKEN")
        .ok()
        .filter(|s| !s.is_empty())
    {
        Some(t) => t,
        None => return FlagStatus::unavailable("LD_ACCESS_TOKEN is not configured"),
    };
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let cache_key = format!("{}:{}:{}", flag.project_key, flag.key, flag.environment_key);
    let cached = cache
        .lock()
        .await
        .get(&cache_key)
        .filter(|(t, _, _)| t.elapsed() < Duration::from_secs(60))
        .cloned();
    let (body, fetched) = if let Some((_, body, time)) = cached {
        (body, time)
    } else {
        let client = match reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
        {
            Ok(c) => c,
            Err(_) => return FlagStatus::unavailable("provider client unavailable"),
        };
        // Keys are validated path components by the domain API.
        let url = format!(
            "https://app.launchdarkly.com/api/v2/flags/{}/{}?env={}",
            flag.project_key, flag.key, flag.environment_key
        );
        let response = match client.get(url).header("Authorization", token).send().await {
            Ok(r) => r,
            Err(_) => return FlagStatus::unavailable("LaunchDarkly request failed or timed out"),
        };
        if !response.status().is_success() {
            return FlagStatus::unavailable(&format!(
                "LaunchDarkly returned HTTP {}",
                response.status().as_u16()
            ));
        }
        let body: Value = match response.json().await {
            Ok(v) => v,
            Err(_) => return FlagStatus::unavailable("invalid LaunchDarkly response"),
        };
        let time = crate::projects::timestamp();
        let mut c = cache.lock().await;
        if c.len() >= 128 {
            c.clear();
        }
        c.insert(cache_key, (Instant::now(), body.clone(), time.clone()));
        (body, time)
    };
    evaluate(&body, flag, &fetched)
}
pub fn evaluate(item: &Value, flag: &FlagReference, fetched: &str) -> FlagStatus {
    let env = &item["environments"][&flag.environment_key];
    let Some(on) = env["on"].as_bool() else {
        return FlagStatus {
            fetched_at: fetched.into(),
            ..FlagStatus::unavailable("environment not found")
        };
    };
    let variation = if on {
        env["fallthrough"]["variation"].as_u64()
    } else {
        env["offVariation"].as_u64()
    };
    let value = variation
        .and_then(|v| item["variations"].get(v as usize))
        .and_then(|v| v.get("value"))
        .cloned();
    let Some(value) = value else {
        return FlagStatus {
            targeting_on: Some(on),
            fetched_at: fetched.into(),
            ..FlagStatus::unavailable("default is mixed, missing, or percentage-based")
        };
    };
    FlagStatus {
        source: "launchdarkly",
        available: true,
        matches_fully_enabled: Some(value == Value::Bool(flag.fully_enabled_value)),
        current_value: Some(value),
        targeting_on: Some(on),
        fetched_at: fetched.into(),
        error: None,
    }
}

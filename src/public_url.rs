use anyhow::{Result, anyhow};
use serde::Serialize;
use url::{Host, Url};

use crate::store::Store;

pub const DEFAULT_PORT: u16 = 4270;
pub const PUBLIC_URL_SETTING: &str = "bot_public_base_url";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PublicUrlSource {
    Settings,
    Environment,
    Derived,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicUrlResolution {
    pub url: Option<String>,
    pub source: PublicUrlSource,
}

#[derive(Debug, Clone, Default)]
pub struct PublicUrlInputs {
    pub configured: Option<String>,
    pub environment: Option<String>,
    pub instance_id: Option<String>,
    pub ingress_domain: Option<String>,
    pub port: Option<String>,
}

pub async fn resolve_public_url(store: &Store) -> PublicUrlResolution {
    let configured = store.get_setting(PUBLIC_URL_SETTING).await.ok().flatten();
    resolve_public_url_from(PublicUrlInputs {
        configured,
        environment: std::env::var("MAZZ_FLUX_PUBLIC_BASE_URL").ok(),
        instance_id: std::env::var("INSTANCE_ID").ok(),
        ingress_domain: std::env::var("VAPE_INGRESS_DOMAIN").ok(),
        port: std::env::var("PORT").ok(),
    })
}

pub fn resolve_public_url_from(inputs: PublicUrlInputs) -> PublicUrlResolution {
    if let Some(url) = valid_candidate(inputs.configured.as_deref()) {
        return PublicUrlResolution {
            url: Some(url),
            source: PublicUrlSource::Settings,
        };
    }
    if let Some(url) = valid_candidate(inputs.environment.as_deref()) {
        return PublicUrlResolution {
            url: Some(url),
            source: PublicUrlSource::Environment,
        };
    }

    let instance_id = inputs
        .instance_id
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty());
    let ingress_domain = inputs
        .ingress_domain
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty());
    if let (Some(instance_id), Some(ingress_domain)) = (instance_id, ingress_domain) {
        let port = inputs
            .port
            .as_deref()
            .and_then(|value| value.parse::<u16>().ok())
            .unwrap_or(DEFAULT_PORT);
        let derived = format!("https://preview-{port}--{instance_id}.{ingress_domain}");
        if let Ok(url) = normalize_public_base_url(&derived) {
            return PublicUrlResolution {
                url: Some(url),
                source: PublicUrlSource::Derived,
            };
        }
    }

    PublicUrlResolution {
        url: None,
        source: PublicUrlSource::Unavailable,
    }
}

fn valid_candidate(candidate: Option<&str>) -> Option<String> {
    let candidate = candidate?.trim();
    if candidate.is_empty() {
        return None;
    }
    normalize_public_base_url(candidate).ok()
}

pub fn normalize_public_base_url(value: &str) -> Result<String> {
    let value = value.trim();
    let parsed =
        Url::parse(value).map_err(|_| anyhow!("Bot public URL must be a valid absolute URL"))?;
    if parsed.scheme() != "https" && parsed.scheme() != "http" {
        return Err(anyhow!(
            "Bot public URL must use https (or http for loopback development)"
        ));
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(anyhow!("Bot public URL cannot contain credentials"));
    }
    if parsed.query().is_some() || parsed.fragment().is_some() {
        return Err(anyhow!(
            "Bot public URL cannot contain a query string or fragment"
        ));
    }
    if parsed.host().is_none() {
        return Err(anyhow!("Bot public URL must include a host"));
    }
    if parsed.scheme() == "http" && !is_loopback_host(parsed.host()) {
        return Err(anyhow!(
            "Bot public URL must use https unless it targets loopback development"
        ));
    }
    Ok(value.trim_end_matches('/').to_string())
}

fn is_loopback_host(host: Option<Host<&str>>) -> bool {
    match host {
        Some(Host::Domain(host)) => {
            host.eq_ignore_ascii_case("localhost")
                || host.to_ascii_lowercase().ends_with(".localhost")
        }
        Some(Host::Ipv4(ip)) => ip.is_loopback(),
        Some(Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inputs() -> PublicUrlInputs {
        PublicUrlInputs {
            configured: None,
            environment: None,
            instance_id: Some("bot123".into()),
            ingress_domain: Some("stable.dexus.io".into()),
            port: Some("4271".into()),
        }
    }

    #[test]
    fn configured_wins_over_environment_and_derivation() {
        let mut values = inputs();
        values.configured = Some("https://configured.example/".into());
        values.environment = Some("https://environment.example".into());
        let result = resolve_public_url_from(values);
        assert_eq!(result.url.as_deref(), Some("https://configured.example"));
        assert_eq!(result.source, PublicUrlSource::Settings);
    }

    #[test]
    fn environment_wins_over_derivation() {
        let mut values = inputs();
        values.environment = Some("https://environment.example/".into());
        let result = resolve_public_url_from(values);
        assert_eq!(result.url.as_deref(), Some("https://environment.example"));
        assert_eq!(result.source, PublicUrlSource::Environment);
    }

    #[test]
    fn derives_preview_url_with_runtime_port() {
        let result = resolve_public_url_from(inputs());
        assert_eq!(
            result.url.as_deref(),
            Some("https://preview-4271--bot123.stable.dexus.io")
        );
        assert_eq!(result.source, PublicUrlSource::Derived);
    }

    #[test]
    fn derives_preview_url_and_uses_default_port() {
        let mut values = inputs();
        values.port = None;
        let result = resolve_public_url_from(values);
        assert_eq!(
            result.url.as_deref(),
            Some("https://preview-4270--bot123.stable.dexus.io")
        );
        assert_eq!(result.source, PublicUrlSource::Derived);
    }

    #[test]
    fn empty_or_invalid_higher_priority_values_fall_through() {
        let mut values = inputs();
        values.configured = Some(" ".into());
        values.environment = Some("http://not-loopback.example".into());
        let result = resolve_public_url_from(values);
        assert_eq!(result.source, PublicUrlSource::Derived);
    }

    #[test]
    fn unavailable_without_override_or_derivation_inputs() {
        let result = resolve_public_url_from(PublicUrlInputs::default());
        assert_eq!(result.url, None);
        assert_eq!(result.source, PublicUrlSource::Unavailable);
    }

    #[test]
    fn normalization_rejects_unsafe_or_ambiguous_urls() {
        assert_eq!(
            normalize_public_base_url("https://example.com///").unwrap(),
            "https://example.com"
        );
        assert_eq!(
            normalize_public_base_url("http://localhost:4270/").unwrap(),
            "http://localhost:4270"
        );
        assert!(normalize_public_base_url("http://example.com").is_err());
        assert!(normalize_public_base_url("ftp://example.com").is_err());
        assert!(normalize_public_base_url("https://user:pass@example.com").is_err());
        assert!(normalize_public_base_url("https://example.com?token=x").is_err());
        assert!(normalize_public_base_url("https://example.com#part").is_err());
    }
}

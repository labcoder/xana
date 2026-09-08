use crate::config::CredentialReference;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PublicWebConsent {
    #[default]
    Ask,
    Allow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SearchProvider {
    Exa,
    ExaMcp,
    Brave,
}

impl SearchProvider {
    pub(crate) fn endpoint(self) -> &'static str {
        match self {
            Self::Exa => "https://api.exa.ai/search",
            Self::ExaMcp => "https://mcp.exa.ai/mcp",
            Self::Brave => "https://api.search.brave.com/res/v1/web/search",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SearchConnection {
    pub(crate) provider: SearchProvider,
    pub(crate) credential: Option<CredentialReference>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct WebLimits {
    pub(crate) searches: u16,
    pub(crate) attempts: u16,
    pub(crate) concurrency: u16,
    pub(crate) ingress_bytes: usize,
    pub(crate) fetch_bytes: usize,
}

impl Default for WebLimits {
    fn default() -> Self {
        Self {
            searches: 3,
            attempts: 8,
            concurrency: 2,
            ingress_bytes: 16 * 1024 * 1024,
            fetch_bytes: 2 * 1024 * 1024,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct WebConfig {
    pub(crate) default_connection: Option<String>,
    pub(crate) connections: BTreeMap<String, SearchConnection>,
    pub(crate) public_web: PublicWebConsent,
    pub(crate) limits: WebLimits,
}

impl WebConfig {
    pub(crate) fn validate(&self) -> Result<(), String> {
        let limits = &self.limits;
        if !(1..=8).contains(&limits.searches)
            || !(1..=24).contains(&limits.attempts)
            || !(1..=4).contains(&limits.concurrency)
            || !(1024..=64 * 1024 * 1024).contains(&limits.ingress_bytes)
            || !(1024..=4 * 1024 * 1024).contains(&limits.fetch_bytes)
        {
            return Err("web limits exceed supported bounds (searches 1–8, attempts 1–24, concurrency 1–4, ingress 1 KiB–64 MiB, fetch 1 KiB–4 MiB)".into());
        }
        if self.connections.len() > 8 {
            return Err("at most eight web search connections are supported".into());
        }
        if self
            .default_connection
            .as_ref()
            .is_some_and(|name| !self.connections.contains_key(name))
        {
            return Err("web.default_connection must name a configured web connection".into());
        }
        for (name, connection) in &self.connections {
            if name.is_empty()
                || name.len() > 64
                || !name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
            {
                return Err(
                    "web connection names must be 1–64 ASCII letters, digits, - or _".into(),
                );
            }
            match (&connection.provider, &connection.credential) {
                (SearchProvider::ExaMcp, None) => {}
                (SearchProvider::ExaMcp, Some(_)) => return Err("Exa hosted MCP is the explicit no-key route; use Exa API for your own key".into()),
                (_, Some(CredentialReference::Environment { variable })) if valid_reference(variable) => {}
                (_, Some(CredentialReference::Stored { id })) if valid_reference(id) => {}
                _ => return Err("web API connections require a valid environment or stored credential reference".into()),
            }
        }
        Ok(())
    }

    pub(crate) fn selected(&self) -> Option<(&str, &SearchConnection)> {
        let name = self.default_connection.as_deref()?;
        self.connections
            .get(name)
            .map(|connection| (name, connection))
    }

    pub(crate) fn review(&self) -> crate::outbound::PublicWebReview {
        let identity = serde_json::to_vec(&self.selected()).expect("web route serializes");
        let label = self.selected().map_or_else(
            || "no search provider".into(),
            |(name, connection)| format!("{name} ({:?})", connection.provider),
        );
        crate::outbound::PublicWebReview {
            route: format!("{label}; {}", blake3::hash(&identity).to_hex()),
            persisted_allow: self.public_web == PublicWebConsent::Allow,
        }
    }
}

fn valid_reference(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn defaults_have_no_external_search_recipient_or_blanket_consent() {
        let config = WebConfig::default();
        config.validate().unwrap();
        assert!(config.selected().is_none());
        assert_eq!(config.public_web, PublicWebConsent::Ask);
        assert_eq!(config.limits.fetch_bytes, 2 * 1024 * 1024);
    }

    #[test]
    fn configuration_rejects_silent_fallback_and_unbounded_limits() {
        let mut config = WebConfig {
            default_connection: Some("missing".into()),
            ..Default::default()
        };
        assert!(config.validate().is_err());
        config.default_connection = None;
        config.limits.attempts = u16::MAX;
        assert!(config.validate().is_err());
    }

    #[test]
    fn mcp_opt_in_is_explicit_and_api_credentials_remain_references() {
        let config: WebConfig = toml::from_str(
            "default_connection = 'exa-free'\n[connections.exa-free]\nprovider = 'exa_mcp'\n",
        )
        .unwrap();
        config.validate().unwrap();
        assert_eq!(
            config.selected().unwrap().1.provider,
            SearchProvider::ExaMcp
        );
        assert!(
            toml::from_str::<WebConfig>("[connections.exa]\nprovider='exa'\napi_key='secret'")
                .is_err()
        );
    }
}

//! Static, renderer-safe provider capability table.
//!
//! This module is a stable source of truth for which providers are known and
//! what capabilities their UI surfaces expose.  It never creates an HTTP
//! client, reads the database, or parses credentials.  In particular, auth
//! wire behavior (base URL, protocol, endpoint, framing) is decided per model
//! by the `/models` snapshot in `core::route_plan`, *not* by this static spec.

use super::ProviderKind;
pub use crate::core::route_plan::AuthNonStreamFraming;

/// Interactive login protocol used by a provider.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthLoginMode {
    /// Loopback callback with PKCE (Codex).
    BrowserCallback,
    /// RFC 8628 OAuth 2.0 Device Authorization Grant (Kimi Code).
    DeviceCode,
}

impl AuthLoginMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::BrowserCallback => "browser_callback",
            Self::DeviceCode => "device_code",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "browser_callback" => Some(Self::BrowserCallback),
            "device_code" => Some(Self::DeviceCode),
            _ => None,
        }
    }
}

/// Immutable capability metadata for one provider.
///
/// The struct intentionally stores only non-secret, renderer-safe values.
/// OAuth/wire details that change the shape of requests belong in the static
/// route profile resolution in `core::route_plan`, not here.
#[derive(Clone, Copy, Debug)]
pub struct ProviderSpec {
    pub kind: &'static str,
    pub display_name: &'static str,
    pub icon_key: &'static str,
    pub login_mode: AuthLoginMode,
    pub login_methods: &'static [AuthLoginMode],
    pub supports_import: bool,
    pub supports_export: bool,
    pub supports_quota: bool,
}

const CODEX: ProviderSpec = ProviderSpec {
    kind: "codex",
    display_name: "Codex",
    icon_key: "codex",
    login_mode: AuthLoginMode::BrowserCallback,
    login_methods: &[AuthLoginMode::BrowserCallback, AuthLoginMode::DeviceCode],
    supports_import: true,
    supports_export: true,
    supports_quota: true,
};

const KIMI: ProviderSpec = ProviderSpec {
    kind: "kimi",
    display_name: "Kimi Code",
    icon_key: "moonshot",
    login_mode: AuthLoginMode::DeviceCode,
    login_methods: &[AuthLoginMode::DeviceCode],
    supports_import: false,
    supports_export: false,
    supports_quota: false,
};

const GROK: ProviderSpec = ProviderSpec {
    kind: "grok",
    display_name: "Grok",
    icon_key: "grok",
    login_mode: AuthLoginMode::DeviceCode,
    login_methods: &[AuthLoginMode::DeviceCode],
    supports_import: false,
    supports_export: false,
    supports_quota: false,
};

const REGISTERED: &[&ProviderSpec] = &[&CODEX, &KIMI, &GROK];

/// Spec for an explicit (non-`Other`) provider kind, if it is a known spec.
pub fn provider_spec(kind: &ProviderKind) -> Option<&'static ProviderSpec> {
    REGISTERED
        .iter()
        .copied()
        .find(|spec| spec.kind == kind.as_str())
}

/// Spec resolved by the canonical wire string (`codex`, `kimi`).
pub fn provider_spec_by_name(name: &str) -> Option<&'static ProviderSpec> {
    let kind = ProviderKind::from(name);
    if matches!(kind, ProviderKind::Other(_)) {
        return None;
    }
    provider_spec(&kind)
}

/// The full set of registered specs, used by renderer-safe provider lists.
pub fn registered_provider_specs() -> &'static [&'static ProviderSpec] {
    REGISTERED
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_spec_exact_values() {
        let spec = provider_spec(&ProviderKind::Codex).expect("codex spec must exist");
        assert_eq!(spec.kind, "codex");
        assert_eq!(spec.display_name, "Codex");
        assert_eq!(spec.icon_key, "codex");
        assert_eq!(spec.login_mode, AuthLoginMode::BrowserCallback);
        assert_eq!(
            spec.login_methods,
            &[AuthLoginMode::BrowserCallback, AuthLoginMode::DeviceCode]
        );
        assert!(spec.supports_import);
        assert!(spec.supports_export);
        assert!(spec.supports_quota);
    }

    #[test]
    fn kimi_spec_exact_values() {
        let spec = provider_spec(&ProviderKind::Kimi).expect("kimi spec must exist");
        assert_eq!(spec.kind, "kimi");
        assert_eq!(spec.display_name, "Kimi Code");
        assert_eq!(spec.icon_key, "moonshot");
        assert_eq!(spec.login_mode, AuthLoginMode::DeviceCode);
        assert_eq!(spec.login_methods, &[AuthLoginMode::DeviceCode]);
        assert!(!spec.supports_import);
        assert!(!spec.supports_export);
        assert!(!spec.supports_quota);
    }

    #[test]
    fn grok_spec_exact_values() {
        let spec = provider_spec(&ProviderKind::Grok).expect("grok spec must exist");
        assert_eq!(spec.kind, "grok");
        assert_eq!(spec.display_name, "Grok");
        assert_eq!(spec.icon_key, "grok");
        assert_eq!(spec.login_mode, AuthLoginMode::DeviceCode);
        assert_eq!(spec.login_methods, &[AuthLoginMode::DeviceCode]);
        assert!(!spec.supports_import);
        assert!(!spec.supports_export);
        assert!(!spec.supports_quota);
    }

    #[test]
    fn unknown_provider_spec_is_none() {
        assert!(provider_spec(&ProviderKind::Other("nope".into())).is_none());
        assert!(provider_spec_by_name("nope").is_none());
    }

    #[test]
    fn provider_spec_by_name_resolves_both_providers() {
        assert_eq!(
            provider_spec_by_name("codex").map(|s| s.kind),
            Some("codex")
        );
        assert_eq!(provider_spec_by_name("kimi").map(|s| s.kind), Some("kimi"));
        assert_eq!(provider_spec_by_name("grok").map(|s| s.kind), Some("grok"));
        assert!(provider_spec_by_name("codEx").is_none());
    }

    #[test]
    fn registered_specs_contains_codex_and_kimi() {
        let names: Vec<&str> = registered_provider_specs().iter().map(|s| s.kind).collect();
        assert!(names.contains(&"codex"));
        assert!(names.contains(&"kimi"));
        assert!(names.contains(&"grok"));
        assert_eq!(registered_provider_specs().len(), 3);
    }

    #[test]
    fn auth_non_stream_framing_serializes_snake_case() {
        assert_eq!(
            serde_json::to_string(&AuthNonStreamFraming::ForcedResponsesSse).unwrap(),
            "\"forced_responses_sse\""
        );
        assert_eq!(
            serde_json::to_string(&AuthNonStreamFraming::Json).unwrap(),
            "\"json\""
        );
    }
}

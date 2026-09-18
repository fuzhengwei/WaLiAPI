//! xAI Grok OAuth 2.0 Device Authorization Grant (RFC 8628).
//!
//! Grok does not use a localhost callback.  Login discovers OAuth endpoints
//! from the fixed xAI OIDC issuer, requests a device code, and polls the token
//! endpoint.  Discovered endpoints are accepted only when they are HTTPS on
//! `x.ai` or a subdomain.  This module owns OAuth/refresh/HTTP-state
//! classification only — never protocol conversion.

use std::time::Duration;

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use chrono::{Duration as ChronoDuration, Utc};
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, CONTENT_TYPE};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::{
    LoginResult, LoginRuntime, LoginStep, ProviderError, ProviderPayload, RefreshedPayload,
};

/// Public xAI Grok CLI OAuth client ID (not a secret).
pub const GROK_CLIENT_ID: &str = "b1a00492-073a-47ea-816f-4c329264a828";
pub const GROK_ISSUER: &str = "https://auth.x.ai";
pub const GROK_DISCOVERY_URL: &str = "https://auth.x.ai/.well-known/openid-configuration";
pub const GROK_SCOPE: &str = "openid profile email offline_access grok-cli:access api:access";
pub const GROK_DEVICE_CODE_GRANT: &str = "urn:ietf:params:oauth:grant-type:device_code";
/// Fixed Grok CLI chat-proxy upstream for OAuth accounts.  Never caller-overridable.
pub const GROK_API_BASE: &str = "https://cli-chat-proxy.grok.com/v1";
pub const GROK_LOGIN_TIMEOUT: Duration = Duration::from_secs(30 * 60);
pub const GROK_HTTP_TIMEOUT: Duration = Duration::from_secs(30);
pub const GROK_DEFAULT_POLL_INTERVAL: u64 = 5;

pub const GROK_TOKEN_AUTH_HEADER: &str = "x-xai-token-auth";
pub const GROK_TOKEN_AUTH_VALUE: &str = "xai-grok-cli";
pub const GROK_CLIENT_VERSION_HEADER: &str = "x-grok-client-version";
pub const GROK_CLIENT_VERSION: &str = "0.2.120";
pub const GROK_CLIENT_IDENTIFIER_HEADER: &str = "x-grok-client-identifier";
pub const GROK_CLIENT_IDENTIFIER: &str = "grok-shell";
pub const GROK_AUTHENTICATE_RESPONSE_HEADER: &str = "x-authenticateresponse";
pub const GROK_AUTHENTICATE_RESPONSE_VALUE: &str = "authenticate-response";
pub const GROK_USER_AGENT: &str = "xai-grok-workspace/0.2.120";

/// Server-side device authorization response (RFC 8628 §3.2).
#[derive(Deserialize)]
struct DeviceAuthorizationResponse {
    user_code: String,
    device_code: String,
    #[serde(default)]
    verification_uri: Option<String>,
    #[serde(default)]
    verification_uri_complete: Option<String>,
    #[serde(default)]
    expires_in: Option<u64>,
    #[serde(default)]
    interval: Option<u64>,
}

/// Server-side token poll / refresh response.  Values are never Debug-printed.
#[derive(Deserialize)]
struct TokenWireResponse {
    #[serde(default)]
    access_token: Option<String>,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    id_token: Option<String>,
    #[serde(default)]
    expires_in: Option<Value>,
    #[serde(default)]
    token_type: Option<String>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    error_description: Option<String>,
}

impl std::fmt::Debug for DeviceAuthorizationResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeviceAuthorizationResponse")
            .field("user_code_present", &!self.user_code.is_empty())
            .field("device_code_present", &!self.device_code.is_empty())
            .field("verification_uri_present", &self.verification_uri.is_some())
            .field(
                "verification_uri_complete_present",
                &self.verification_uri_complete.is_some(),
            )
            .field("expires_in_present", &self.expires_in.is_some())
            .field("interval_present", &self.interval.is_some())
            .finish()
    }
}

impl std::fmt::Debug for TokenWireResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenWireResponse")
            .field("access_token_present", &self.access_token.is_some())
            .field("refresh_token_present", &self.refresh_token.is_some())
            .field("id_token_present", &self.id_token.is_some())
            .field("expires_in_present", &self.expires_in.is_some())
            .field("token_type_present", &self.token_type.is_some())
            .field("error_present", &self.error.is_some())
            .field(
                "error_description_present",
                &self.error_description.is_some(),
            )
            .finish()
    }
}

struct OAuthTokens {
    access_token: String,
    refresh_token: String,
    id_token: Option<String>,
    expires_in: u64,
    token_type: String,
    email: String,
    subject: String,
}

#[derive(Clone)]
pub struct GrokLogin {
    client: reqwest::Client,
    discovery_url: String,
    device_auth_url: String,
    token_url: String,
    /// When true, discovered and stored token endpoints must be HTTPS on x.ai.
    /// Test constructors that point at loopback mocks set this to false.
    strict_endpoints: bool,
    /// Optional floor (seconds) used by tests so polling does not wait 5s.
    min_poll_interval: Option<u64>,
}

impl Default for GrokLogin {
    fn default() -> Self {
        Self::new()
    }
}

impl GrokLogin {
    /// Production constructor: fixed HTTPS discovery only.
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(GROK_HTTP_TIMEOUT)
                .build()
                .expect("grok login http client"),
            discovery_url: GROK_DISCOVERY_URL.to_owned(),
            device_auth_url: String::new(),
            token_url: String::new(),
            strict_endpoints: true,
            min_poll_interval: None,
        }
    }

    /// Test constructor: local mock URLs skip x.ai host validation so coverage
    /// never touches the real xAI service.
    pub fn with_endpoints(
        device_auth_url: impl Into<String>,
        token_url: impl Into<String>,
    ) -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(GROK_HTTP_TIMEOUT)
                .build()
                .expect("grok login http client"),
            discovery_url: String::new(),
            device_auth_url: device_auth_url.into(),
            token_url: token_url.into(),
            strict_endpoints: false,
            min_poll_interval: Some(1),
        }
    }

    /// Test constructor for OIDC discovery coverage against a local mock.
    pub fn with_discovery_url(discovery_url: impl Into<String>) -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(GROK_HTTP_TIMEOUT)
                .build()
                .expect("grok login http client"),
            discovery_url: discovery_url.into(),
            device_auth_url: String::new(),
            token_url: String::new(),
            strict_endpoints: true,
            min_poll_interval: Some(1),
        }
    }

    fn oauth_headers() -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            CONTENT_TYPE,
            HeaderValue::from_static("application/x-www-form-urlencoded"),
        );
        headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
        headers
    }

    fn poll_interval(&self, advertised: Option<u64>) -> u64 {
        let advertised = advertised.unwrap_or(0);
        match self.min_poll_interval {
            Some(min) if advertised == 0 => min,
            Some(min) => advertised.max(min),
            None if advertised == 0 => GROK_DEFAULT_POLL_INTERVAL,
            None => advertised,
        }
    }

    fn slow_down_step(&self) -> u64 {
        self.min_poll_interval
            .unwrap_or(GROK_DEFAULT_POLL_INTERVAL)
            .max(1)
    }

    /// Accept only HTTPS endpoints whose host is `x.ai` or a subdomain.
    pub fn validate_oauth_endpoint(raw: &str) -> Result<String, ProviderError> {
        let raw = raw.trim();
        if raw.is_empty() {
            return Err(ProviderError::DeviceAuthorizationFailed);
        }
        let parsed = url::Url::parse(raw).map_err(|_| ProviderError::DeviceAuthorizationFailed)?;
        if parsed.scheme() != "https" {
            return Err(ProviderError::DeviceAuthorizationFailed);
        }
        let host = parsed.host_str().unwrap_or("").trim().to_ascii_lowercase();
        if host != "x.ai" && !host.ends_with(".x.ai") {
            return Err(ProviderError::DeviceAuthorizationFailed);
        }
        Ok(raw.to_owned())
    }

    fn accept_endpoint(&self, raw: &str) -> Result<String, ProviderError> {
        if self.strict_endpoints {
            Self::validate_oauth_endpoint(raw)
        } else {
            let raw = raw.trim();
            if raw.is_empty() {
                Err(ProviderError::DeviceAuthorizationFailed)
            } else {
                Ok(raw.to_owned())
            }
        }
    }

    pub async fn discover(&self) -> Result<(String, String), ProviderError> {
        if !self.device_auth_url.is_empty() && !self.token_url.is_empty() {
            return Ok((self.device_auth_url.clone(), self.token_url.clone()));
        }
        if self.discovery_url.is_empty() {
            return Err(ProviderError::DeviceAuthorizationFailed);
        }
        let response = self
            .client
            .get(&self.discovery_url)
            .header(ACCEPT, "application/json")
            .send()
            .await
            .map_err(|_| ProviderError::Retryable)?;
        if !response.status().is_success() {
            return Err(ProviderError::DeviceAuthorizationFailed);
        }
        let body: Value = response
            .json()
            .await
            .map_err(|_| ProviderError::DeviceAuthorizationFailed)?;
        let device = body
            .get("device_authorization_endpoint")
            .and_then(Value::as_str)
            .unwrap_or("");
        let token = body
            .get("token_endpoint")
            .and_then(Value::as_str)
            .unwrap_or("");
        Ok((self.accept_endpoint(device)?, self.accept_endpoint(token)?))
    }

    pub async fn login(&self, runtime: &dyn LoginRuntime) -> Result<LoginResult, ProviderError> {
        let deadline = tokio::time::Instant::now() + GROK_LOGIN_TIMEOUT;
        loop {
            if runtime.is_cancelled() {
                return Err(ProviderError::LoginCancelled);
            }
            runtime.set_step(LoginStep::Preparing).await;

            let (device_auth_url, token_url) = self.discover().await?;
            let device = self
                .request_device_authorization(runtime, &device_auth_url)
                .await?;
            let verification_url = verification_url(&device)?;
            let mut current_interval = self.poll_interval(device.interval);

            let expires_at = device
                .expires_in
                .map(|seconds| (Utc::now() + ChronoDuration::seconds(seconds as i64)).to_rfc3339());
            runtime
                .present_device_authorization(&verification_url, &device.user_code, expires_at)
                .await?;

            match runtime.open_browser(&verification_url).await {
                Ok(()) => {}
                Err(error) => {
                    if !runtime.is_cancelled() {
                        tracing::warn!("could not open Grok authorization browser: {error}");
                    }
                }
            }

            let poll_deadline = device_poll_deadline(deadline, device.expires_in);
            match self
                .poll_device_token(
                    runtime,
                    &token_url,
                    &device.device_code,
                    &mut current_interval,
                    poll_deadline,
                )
                .await
            {
                Ok(tokens) => {
                    runtime.set_step(LoginStep::Exchanging).await;
                    return Ok(self.login_result(tokens, &token_url));
                }
                Err(TokenPollError::Expired) => continue,
                Err(TokenPollError::Denied) => return Err(ProviderError::AuthorizationDenied),
                Err(TokenPollError::Timeout) => return Err(ProviderError::LoginTimeout),
                Err(TokenPollError::Cancelled) => return Err(ProviderError::LoginCancelled),
                Err(TokenPollError::ClientError) => {
                    return Err(ProviderError::TokenExchangeFailed);
                }
            }
        }
    }

    async fn request_device_authorization(
        &self,
        runtime: &dyn LoginRuntime,
        device_auth_url: &str,
    ) -> Result<DeviceAuthorizationResponse, ProviderError> {
        if runtime.is_cancelled() {
            return Err(ProviderError::LoginCancelled);
        }
        runtime.set_step(LoginStep::Authorizing).await;
        let response = self
            .client
            .post(device_auth_url)
            .headers(Self::oauth_headers())
            .form(&[("client_id", GROK_CLIENT_ID), ("scope", GROK_SCOPE)])
            .send()
            .await
            .map_err(|_| ProviderError::Retryable)?;
        if !response.status().is_success() {
            return Err(ProviderError::DeviceAuthorizationFailed);
        }
        let device: DeviceAuthorizationResponse = serde_json::from_slice(
            &response
                .bytes()
                .await
                .map_err(|_| ProviderError::Retryable)?,
        )
        .map_err(|_| ProviderError::DeviceAuthorizationFailed)?;
        if device.device_code.trim().is_empty() || device.user_code.trim().is_empty() {
            return Err(ProviderError::DeviceAuthorizationFailed);
        }
        Ok(device)
    }

    async fn poll_device_token(
        &self,
        runtime: &dyn LoginRuntime,
        token_url: &str,
        device_code: &str,
        interval: &mut u64,
        deadline: tokio::time::Instant,
    ) -> Result<OAuthTokens, TokenPollError> {
        loop {
            if runtime.is_cancelled() {
                return Err(TokenPollError::Cancelled);
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(TokenPollError::Timeout);
            }
            runtime.set_step(LoginStep::Waiting).await;
            let response = match self
                .client
                .post(token_url)
                .headers(Self::oauth_headers())
                .form(&[
                    ("grant_type", GROK_DEVICE_CODE_GRANT),
                    ("device_code", device_code),
                    ("client_id", GROK_CLIENT_ID),
                ])
                .send()
                .await
            {
                Ok(response) => response,
                Err(_) => {
                    if runtime.is_cancelled() {
                        return Err(TokenPollError::Cancelled);
                    }
                    if tokio::time::Instant::now() >= deadline {
                        return Err(TokenPollError::Timeout);
                    }
                    poll_sleep(runtime, *interval).await;
                    continue;
                }
            };

            let status = response.status();
            let wire: TokenWireResponse = match response.json().await {
                Ok(value) => value,
                Err(_) => return Err(TokenPollError::ClientError),
            };

            if let Some(error) = wire.error.as_deref() {
                match error {
                    "authorization_pending" => poll_sleep(runtime, *interval).await,
                    "slow_down" => {
                        *interval = interval.saturating_add(self.slow_down_step());
                        poll_sleep(runtime, *interval).await;
                    }
                    "expired_token" => return Err(TokenPollError::Expired),
                    "access_denied" => return Err(TokenPollError::Denied),
                    _ => return Err(TokenPollError::ClientError),
                }
                continue;
            }

            if status == reqwest::StatusCode::OK {
                return parse_success_tokens(wire, true).map_err(|_| TokenPollError::ClientError);
            }
            if status.is_server_error() {
                poll_sleep(runtime, *interval).await;
                continue;
            }
            return Err(TokenPollError::ClientError);
        }
    }

    fn login_result(&self, tokens: OAuthTokens, token_endpoint: &str) -> LoginResult {
        let now = Utc::now();
        let expires_at = (now + ChronoDuration::seconds(tokens.expires_in as i64)).to_rfc3339();
        let account_id = if !tokens.subject.is_empty() {
            tokens.subject.clone()
        } else if !tokens.email.is_empty() {
            tokens.email.clone()
        } else {
            Uuid::new_v4().simple().to_string()
        };
        let label = if tokens.email.is_empty() {
            "Grok".to_owned()
        } else {
            tokens.email.clone()
        };
        let mut payload = json!({
            "version": 1,
            "provider": "grok",
            "access_token": tokens.access_token,
            "refresh_token": tokens.refresh_token,
            "token_type": token_type_value(&tokens.token_type),
            "expires_at": expires_at,
            "expires_in": tokens.expires_in,
            "token_endpoint": token_endpoint,
        });
        if let Some(object) = payload.as_object_mut() {
            if let Some(id_token) = tokens.id_token.clone() {
                object.insert("id_token".into(), Value::String(id_token));
            }
            if !tokens.email.is_empty() {
                object.insert("email".into(), Value::String(tokens.email.clone()));
            }
            if !tokens.subject.is_empty() {
                object.insert("subject".into(), Value::String(tokens.subject.clone()));
            }
        }
        LoginResult {
            account_id,
            label,
            attributes: json!({
                "plan_type": "grok",
                "identity_source": "oidc",
                "email": if tokens.email.is_empty() { Value::Null } else { Value::String(tokens.email) },
            }),
            payload: ProviderPayload::new(payload),
            last_refreshed_at: Some(now.to_rfc3339()),
            next_refresh_after: None,
            next_retry_after: None,
        }
    }

    pub async fn refresh_payload(
        &self,
        payload: &ProviderPayload,
    ) -> Result<RefreshedPayload, ProviderError> {
        require_grok_payload(payload)?;
        let refresh_token = required_refresh(payload)?;
        let token_endpoint = self.resolve_token_endpoint(payload).await?;
        let mut attempt = 0;
        loop {
            match self.refresh_once(&refresh_token, &token_endpoint).await {
                Ok(mut tokens) => {
                    if tokens.refresh_token.is_empty() {
                        tokens.refresh_token = refresh_token;
                    }
                    let now = Utc::now();
                    let expires_at =
                        (now + ChronoDuration::seconds(tokens.expires_in as i64)).to_rfc3339();
                    let mut next = json!({
                        "version": 1,
                        "provider": "grok",
                        "access_token": tokens.access_token,
                        "refresh_token": tokens.refresh_token,
                        "token_type": token_type_value(&tokens.token_type),
                        "expires_at": expires_at,
                        "expires_in": tokens.expires_in,
                        "token_endpoint": token_endpoint,
                    });
                    if let Some(object) = next.as_object_mut() {
                        if let Some(id_token) = tokens.id_token.clone() {
                            object.insert("id_token".into(), Value::String(id_token));
                        } else if let Some(previous) =
                            payload.as_value().get("id_token").and_then(Value::as_str)
                        {
                            if !previous.is_empty() {
                                object
                                    .insert("id_token".into(), Value::String(previous.to_owned()));
                            }
                        }
                        let email = if tokens.email.is_empty() {
                            payload
                                .as_value()
                                .get("email")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_owned()
                        } else {
                            tokens.email.clone()
                        };
                        let subject = if tokens.subject.is_empty() {
                            payload
                                .as_value()
                                .get("subject")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_owned()
                        } else {
                            tokens.subject.clone()
                        };
                        if !email.is_empty() {
                            object.insert("email".into(), Value::String(email));
                        }
                        if !subject.is_empty() {
                            object.insert("subject".into(), Value::String(subject));
                        }
                    }
                    return Ok(RefreshedPayload {
                        payload: ProviderPayload::new(next),
                        last_refreshed_at: Some(now.to_rfc3339()),
                        next_refresh_after: None,
                        next_retry_after: None,
                    });
                }
                Err(RefreshError::Unauthorized) => return Err(ProviderError::Unauthorized),
                Err(RefreshError::Retryable) => {
                    attempt += 1;
                    if attempt >= 3 {
                        return Err(ProviderError::Retryable);
                    }
                    tokio::time::sleep(Duration::from_secs(attempt)).await;
                }
                Err(RefreshError::Protocol) => return Err(ProviderError::Protocol),
            }
        }
    }

    async fn resolve_token_endpoint(
        &self,
        payload: &ProviderPayload,
    ) -> Result<String, ProviderError> {
        if let Some(stored) = payload
            .as_value()
            .get("token_endpoint")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            return if self.strict_endpoints {
                Self::validate_oauth_endpoint(stored).map_err(|_| ProviderError::InvalidPayload)
            } else {
                Ok(stored.to_owned())
            };
        }
        if !self.token_url.is_empty() {
            return Ok(self.token_url.clone());
        }
        let (_device, token) = self.discover().await.map_err(|error| match error {
            ProviderError::DeviceAuthorizationFailed => ProviderError::InvalidPayload,
            other => other,
        })?;
        Ok(token)
    }

    async fn refresh_once(
        &self,
        refresh_token: &str,
        token_endpoint: &str,
    ) -> Result<OAuthTokens, RefreshError> {
        let response = self
            .client
            .post(token_endpoint)
            .headers(Self::oauth_headers())
            .form(&[
                ("grant_type", "refresh_token"),
                ("client_id", GROK_CLIENT_ID),
                ("refresh_token", refresh_token),
            ])
            .send()
            .await
            .map_err(|_| RefreshError::Retryable)?;
        let status = response.status();
        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            return Err(RefreshError::Unauthorized);
        }
        if status.is_success() {
            let wire: TokenWireResponse = match response.json().await {
                Ok(wire) => wire,
                Err(_) => return Err(RefreshError::Protocol),
            };
            if wire.error.as_deref() == Some("invalid_grant") {
                return Err(RefreshError::Unauthorized);
            }
            return parse_success_tokens(wire, false).map_err(|_| RefreshError::Protocol);
        }
        let wire: TokenWireResponse = response.json().await.unwrap_or(TokenWireResponse {
            access_token: None,
            refresh_token: None,
            id_token: None,
            expires_in: None,
            token_type: None,
            error: None,
            error_description: None,
        });
        if wire.error.as_deref() == Some("invalid_grant") {
            return Err(RefreshError::Unauthorized);
        }
        if status.is_server_error() || status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            return Err(RefreshError::Retryable);
        }
        Err(RefreshError::Protocol)
    }
}

fn verification_url(device: &DeviceAuthorizationResponse) -> Result<String, ProviderError> {
    let complete = device
        .verification_uri_complete
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let raw = if let Some(url) = complete {
        url
    } else {
        device
            .verification_uri
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or(ProviderError::DeviceAuthorizationFailed)?
    };
    // The URL shown/opened for the user is never a mock loopback; only HTTPS
    // x.ai hosts are safe to present, even when OAuth HTTP is locally mocked.
    GrokLogin::validate_oauth_endpoint(raw)
}

fn device_poll_deadline(
    overall: tokio::time::Instant,
    expires_in: Option<u64>,
) -> tokio::time::Instant {
    match expires_in {
        Some(seconds) if seconds > 0 => {
            let code_deadline = tokio::time::Instant::now() + Duration::from_secs(seconds);
            code_deadline.min(overall)
        }
        _ => overall,
    }
}

fn token_type_value(token_type: &str) -> String {
    if token_type.is_empty() {
        "Bearer".to_owned()
    } else {
        token_type.to_owned()
    }
}

fn require_grok_payload(payload: &ProviderPayload) -> Result<(), ProviderError> {
    match payload.as_value().get("provider").and_then(Value::as_str) {
        Some("grok") => Ok(()),
        _ => Err(ProviderError::InvalidPayload),
    }
}

fn required_refresh(payload: &ProviderPayload) -> Result<String, ProviderError> {
    payload
        .as_value()
        .get("refresh_token")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .ok_or(ProviderError::InvalidPayload)
}

fn parse_success_tokens(wire: TokenWireResponse, require_refresh: bool) -> Result<OAuthTokens, ()> {
    let access_token = wire.access_token.filter(|s| !s.is_empty()).ok_or(())?;
    let refresh_token = wire.refresh_token.unwrap_or_default();
    if require_refresh && refresh_token.is_empty() {
        return Err(());
    }
    let expires_in = match wire.expires_in {
        Some(Value::Number(number)) => number.as_u64().filter(|value| *value > 0).ok_or(())?,
        None => 3600,
        _ => return Err(()),
    };
    let id_token = wire.id_token.filter(|value| !value.is_empty());
    let (email, subject) = id_token
        .as_deref()
        .map(parse_jwt_identity)
        .unwrap_or_default();
    Ok(OAuthTokens {
        access_token,
        refresh_token,
        id_token,
        expires_in,
        token_type: wire.token_type.unwrap_or_else(|| "Bearer".into()),
        email,
        subject,
    })
}

fn parse_jwt_identity(token: &str) -> (String, String) {
    let Some(payload) = token.split('.').nth(1) else {
        return (String::new(), String::new());
    };
    let Ok(bytes) = URL_SAFE_NO_PAD.decode(payload) else {
        return (String::new(), String::new());
    };
    let Ok(claims) = serde_json::from_slice::<Value>(&bytes) else {
        return (String::new(), String::new());
    };
    let email = claims
        .get("email")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_owned();
    let subject = claims
        .get("sub")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_owned();
    (email, subject)
}

enum TokenPollError {
    Timeout,
    Cancelled,
    Expired,
    Denied,
    ClientError,
}

enum RefreshError {
    Unauthorized,
    Retryable,
    Protocol,
}

async fn poll_sleep(runtime: &dyn LoginRuntime, interval: u64) {
    tokio::select! {
        _ = tokio::time::sleep(Duration::from_secs(interval)) => {}
        _ = runtime.cancelled() => {}
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    };

    use axum::{extract::State, routing::get, routing::post, Json, Router};
    use serde_json::{json, Value};
    use tokio::sync::watch;

    use super::*;
    use crate::auth_provider::{LoginRuntime, LoginStep};

    const ACCESS: &str = "fixture-access-token";
    const REFRESH: &str = "fixture-refresh-token";
    const ROTATED_ACCESS: &str = "fixture-rotated-access";
    const ROTATED_REFRESH: &str = "fixture-rotated-refresh";
    const USER_CODE: &str = "WXYZ-1234";

    #[derive(Clone, Default)]
    struct MockState {
        seen: Arc<Mutex<Vec<String>>>,
        device_hits: Arc<AtomicUsize>,
        token_hits: Arc<AtomicUsize>,
        token_times: Arc<Mutex<Vec<std::time::Instant>>>,
        always_pending: Arc<std::sync::atomic::AtomicBool>,
        queue: Arc<Mutex<VecDeque<(u16, Value)>>>,
        device_codes: Arc<Mutex<VecDeque<String>>>,
        discovery: Arc<Mutex<Value>>,
        verification_uri: Arc<Mutex<Option<String>>>,
        verification_uri_complete: Arc<Mutex<Option<String>>>,
    }

    async fn device(
        State(state): State<MockState>,
        body: axum::body::Bytes,
    ) -> (axum::http::StatusCode, Json<Value>) {
        state.device_hits.fetch_add(1, Ordering::SeqCst);
        state
            .seen
            .lock()
            .unwrap()
            .push(String::from_utf8_lossy(&body).to_string());
        let mut codes = state.device_codes.lock().unwrap();
        let code = codes
            .pop_front()
            .unwrap_or_else(|| "device-code-0".to_owned());
        let verification_uri = state
            .verification_uri
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_else(|| "https://accounts.x.ai/sign-in".to_owned());
        let verification_uri_complete = state
            .verification_uri_complete
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_else(|| "https://accounts.x.ai/sign-in?user_code=WXYZ-1234".to_owned());
        (
            axum::http::StatusCode::OK,
            Json(json!({
                "device_code": code,
                "user_code": USER_CODE,
                "verification_uri": verification_uri,
                "verification_uri_complete": verification_uri_complete,
                "expires_in": 1800,
                "interval": 1,
            })),
        )
    }

    async fn token(
        State(state): State<MockState>,
        body: axum::body::Bytes,
    ) -> (axum::http::StatusCode, Json<Value>) {
        state.token_hits.fetch_add(1, Ordering::SeqCst);
        state
            .token_times
            .lock()
            .unwrap()
            .push(std::time::Instant::now());
        state
            .seen
            .lock()
            .unwrap()
            .push(String::from_utf8_lossy(&body).to_string());
        if state.always_pending.load(Ordering::SeqCst) {
            return (
                axum::http::StatusCode::BAD_REQUEST,
                Json(json!({"error": "authorization_pending"})),
            );
        }
        let mut queue = state.queue.lock().unwrap();
        if queue.is_empty() {
            return (
                axum::http::StatusCode::OK,
                Json(json!({
                    "access_token": ACCESS,
                    "refresh_token": REFRESH,
                    "token_type": "Bearer",
                    "expires_in": 3600,
                    "id_token": fake_jwt("user@example.test", "sub-fixture")
                })),
            );
        }
        let (status, value) = queue.pop_front().unwrap();
        (
            axum::http::StatusCode::from_u16(status).unwrap(),
            Json(value),
        )
    }

    async fn discovery(State(state): State<MockState>) -> (axum::http::StatusCode, Json<Value>) {
        let body = state.discovery.lock().unwrap().clone();
        (axum::http::StatusCode::OK, Json(body))
    }

    async fn mock_login() -> (GrokLogin, MockState) {
        let state = MockState::default();
        let app = Router::new()
            .route("/device", post(device))
            .route("/token", post(token))
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let login = GrokLogin::with_endpoints(
            format!("http://{addr}/device"),
            format!("http://{addr}/token"),
        );
        (login, state)
    }

    async fn mock_discovery(body: Value) -> (GrokLogin, MockState) {
        let state = MockState {
            discovery: Arc::new(Mutex::new(body)),
            ..Default::default()
        };
        let app = Router::new()
            .route("/.well-known/openid-configuration", get(discovery))
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (
            GrokLogin::with_discovery_url(format!(
                "http://{addr}/.well-known/openid-configuration"
            )),
            state,
        )
    }

    #[derive(Clone)]
    struct TestRuntime {
        steps: Arc<Mutex<Vec<String>>>,
        shown: Arc<Mutex<Vec<(String, String)>>>,
        expires: Arc<Mutex<Vec<Option<String>>>>,
        cancel: Arc<watch::Sender<bool>>,
        _rx: Arc<watch::Receiver<bool>>,
    }
    impl Default for TestRuntime {
        fn default() -> Self {
            let (tx, rx) = watch::channel(false);
            Self {
                steps: Arc::new(Mutex::new(Vec::new())),
                shown: Arc::new(Mutex::new(Vec::new())),
                expires: Arc::new(Mutex::new(Vec::new())),
                cancel: Arc::new(tx),
                _rx: Arc::new(rx),
            }
        }
    }
    impl TestRuntime {
        fn cancel(&self) {
            let _ = self.cancel.send(true);
        }
    }
    #[async_trait::async_trait]
    impl LoginRuntime for TestRuntime {
        async fn open_browser(&self, _url: &str) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn set_step(&self, step: LoginStep) {
            self.steps.lock().unwrap().push(step.as_str().to_owned());
        }
        async fn present_device_authorization(
            &self,
            url: &str,
            user_code: &str,
            expires_at: Option<String>,
        ) -> Result<(), ProviderError> {
            self.shown
                .lock()
                .unwrap()
                .push((url.to_owned(), user_code.to_owned()));
            self.expires.lock().unwrap().push(expires_at);
            Ok(())
        }
        fn is_cancelled(&self) -> bool {
            *self.cancel.borrow()
        }
        async fn cancelled(&self) {
            let mut receiver = self.cancel.subscribe();
            while !*receiver.borrow() {
                if receiver.changed().await.is_err() {
                    return;
                }
            }
        }
    }

    fn fake_jwt(email: &str, subject: &str) -> String {
        let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"none","typ":"JWT"}"#);
        let payload = URL_SAFE_NO_PAD
            .encode(format!(r#"{{"email":"{email}","sub":"{subject}"}}"#).as_bytes());
        format!("{header}.{payload}.sig")
    }

    fn grok_payload(refresh: &str, token_endpoint: &str) -> ProviderPayload {
        ProviderPayload::new(json!({
            "version": 1,
            "provider": "grok",
            "access_token": "old-access",
            "refresh_token": refresh,
            "expires_at": "2030-01-01T00:00:00Z",
            "token_endpoint": token_endpoint,
            "email": "user@example.test",
            "subject": "sub-fixture",
        }))
    }

    #[test]
    fn validate_oauth_endpoint_accepts_xai_https_hosts() {
        for url in [
            "https://auth.x.ai/oauth2/token",
            "https://accounts.x.ai/oauth2/device",
            "https://x.ai/oauth/token",
        ] {
            assert!(
                GrokLogin::validate_oauth_endpoint(url).is_ok(),
                "expected {url} to be accepted"
            );
        }
    }

    #[test]
    fn validate_oauth_endpoint_rejects_non_xai_or_non_https() {
        for url in [
            "http://auth.x.ai/oauth2/token",
            "https://evil.example/oauth/token",
            "https://notx.ai/oauth/token",
            "https://x.ai.evil.com/oauth/token",
            "https://evilx.ai/oauth/token",
            "",
            "not-a-url",
        ] {
            assert!(
                GrokLogin::validate_oauth_endpoint(url).is_err(),
                "expected {url} to be rejected"
            );
        }
    }

    #[tokio::test]
    async fn discover_accepts_xai_endpoints_from_oidc() {
        let (login, _state) = mock_discovery(json!({
            "device_authorization_endpoint": "https://auth.x.ai/oauth2/device/code",
            "token_endpoint": "https://auth.x.ai/oauth2/token",
            "issuer": "https://auth.x.ai"
        }))
        .await;
        let (device, token) = login.discover().await.unwrap();
        assert_eq!(device, "https://auth.x.ai/oauth2/device/code");
        assert_eq!(token, "https://auth.x.ai/oauth2/token");
    }

    #[tokio::test]
    async fn discover_rejects_hijacked_endpoints() {
        let (login, _state) = mock_discovery(json!({
            "device_authorization_endpoint": "https://evil.example/device",
            "token_endpoint": "https://auth.x.ai/oauth2/token"
        }))
        .await;
        assert_eq!(
            login.discover().await.unwrap_err(),
            ProviderError::DeviceAuthorizationFailed
        );
    }

    #[tokio::test]
    async fn device_auth_posts_public_client_id_and_scope() {
        let (login, state) = mock_login().await;
        login.login(&TestRuntime::default()).await.unwrap();
        let seen = state.seen.lock().unwrap();
        assert!(seen[0].contains(&format!("client_id={GROK_CLIENT_ID}")));
        assert!(seen[0].contains("scope="));
        assert!(seen[0].contains("openid"));
        assert!(!seen[0].contains("client_secret"));
        assert_eq!(state.device_hits.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn login_pending_then_success() {
        let (login, state) = mock_login().await;
        state
            .queue
            .lock()
            .unwrap()
            .push_back((400, json!({"error": "authorization_pending"})));
        let result = login.login(&TestRuntime::default()).await.unwrap();
        assert_eq!(result.account_id, "sub-fixture");
        assert_eq!(result.payload.as_value()["access_token"], ACCESS);
        assert_eq!(result.payload.as_value()["provider"], "grok");
        assert_eq!(state.token_hits.load(Ordering::SeqCst), 2);
        assert_eq!(result.attributes["email"], "user@example.test");
    }

    #[tokio::test]
    async fn login_slow_down_then_success() {
        let (login, state) = mock_login().await;
        state
            .queue
            .lock()
            .unwrap()
            .push_back((400, json!({"error": "slow_down"})));
        let result = login.login(&TestRuntime::default()).await.unwrap();
        assert_eq!(result.account_id, "sub-fixture");
        assert_eq!(state.token_hits.load(Ordering::SeqCst), 2);
        let times = state.token_times.lock().unwrap();
        let gap = times[1].saturating_duration_since(times[0]).as_secs();
        assert!(
            (1..=3).contains(&gap),
            "slow_down interval bump not honored: gap was {gap}s"
        );
    }

    #[tokio::test]
    async fn poll_hits_deadline_after_pending() {
        let (login, state) = mock_login().await;
        state.always_pending.store(true, Ordering::SeqCst);
        let mut interval = 0;
        let deadline = tokio::time::Instant::now() + Duration::from_millis(50);
        let result = login
            .poll_device_token(
                &TestRuntime::default(),
                &login.token_url,
                "code",
                &mut interval,
                deadline,
            )
            .await;
        assert!(matches!(result, Err(TokenPollError::Timeout)));
    }

    #[tokio::test]
    async fn cancel_during_polling_exits_promptly() {
        let (login, state) = mock_login().await;
        state
            .queue
            .lock()
            .unwrap()
            .push_back((400, json!({"error": "authorization_pending"})));
        let runtime = TestRuntime::default();
        let handle = tokio::spawn({
            let login = login.clone();
            let runtime = runtime.clone();
            async move { login.login(&runtime).await }
        });
        tokio::task::yield_now().await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        runtime.cancel();
        let result = handle.await.unwrap().unwrap_err();
        assert_eq!(result, ProviderError::LoginCancelled);
    }

    #[tokio::test]
    async fn login_expired_token_reissues_device_code() {
        let (login, state) = mock_login().await;
        state
            .device_codes
            .lock()
            .unwrap()
            .push_front("code-1".to_owned());
        state
            .device_codes
            .lock()
            .unwrap()
            .push_back("code-2".to_owned());
        state
            .queue
            .lock()
            .unwrap()
            .push_back((400, json!({"error": "expired_token"})));
        let result = login.login(&TestRuntime::default()).await.unwrap();
        assert_eq!(result.account_id, "sub-fixture");
        assert_eq!(state.device_hits.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn login_access_denied_returns_stable_error() {
        let (login, state) = mock_login().await;
        state
            .queue
            .lock()
            .unwrap()
            .push_back((400, json!({"error": "access_denied"})));
        assert_eq!(
            login.login(&TestRuntime::default()).await.unwrap_err(),
            ProviderError::AuthorizationDenied
        );
    }

    #[tokio::test]
    async fn login_cancelled_before_any_request() {
        let (login, _state) = mock_login().await;
        let runtime = TestRuntime::default();
        runtime.cancel();
        assert_eq!(
            login.login(&runtime).await.unwrap_err(),
            ProviderError::LoginCancelled
        );
    }

    #[tokio::test]
    async fn login_payload_excludes_device_user_code_and_raw_oauth() {
        let (login, _state) = mock_login().await;
        let result = login.login(&TestRuntime::default()).await.unwrap();
        let payload = result.payload.as_value().to_string();
        for forbidden in ["device-code", USER_CODE, "user_code=WXYZ-1234"] {
            assert!(!payload.contains(forbidden), "payload leaked {forbidden}");
        }
        let rendered = format!("{result:?}");
        assert!(!rendered.contains(ACCESS));
        assert!(!rendered.contains(REFRESH));
        assert_eq!(
            format!("{:?}", result.payload),
            "ProviderPayload(<redacted>)"
        );
    }

    #[tokio::test]
    async fn login_surfaces_verification_url_and_user_code() {
        let (login, _state) = mock_login().await;
        let runtime = TestRuntime::default();
        login.login(&runtime).await.unwrap();
        let shown = runtime.shown.lock().unwrap();
        assert_eq!(shown[0].1, USER_CODE);
        assert!(shown[0].0.contains("accounts.x.ai"));
    }

    #[test]
    fn verification_url_accepts_https_xai_hosts() {
        for url in [
            "https://accounts.x.ai/sign-in",
            "https://auth.x.ai/device",
            "https://x.ai/device",
        ] {
            let device = DeviceAuthorizationResponse {
                user_code: USER_CODE.into(),
                device_code: "device-secret".into(),
                verification_uri: Some(url.into()),
                verification_uri_complete: None,
                expires_in: Some(1800),
                interval: Some(1),
            };
            assert_eq!(verification_url(&device).unwrap(), url);
        }
    }

    #[test]
    fn verification_url_rejects_non_xai_or_non_https() {
        for url in [
            "http://auth.x.ai/device",
            "https://evil.example/verify",
            "https://notx.ai/device",
            "https://x.ai.evil.com/device",
            "http://127.0.0.1:9/callback",
        ] {
            let device = DeviceAuthorizationResponse {
                user_code: USER_CODE.into(),
                device_code: "device-secret".into(),
                verification_uri: Some("https://accounts.x.ai/sign-in".into()),
                verification_uri_complete: Some(url.into()),
                expires_in: Some(1800),
                interval: Some(1),
            };
            assert_eq!(
                verification_url(&device).unwrap_err(),
                ProviderError::DeviceAuthorizationFailed,
                "expected {url} to be rejected"
            );
        }
    }

    #[tokio::test]
    async fn login_rejects_non_xai_verification_url() {
        let (login, state) = mock_login().await;
        *state.verification_uri_complete.lock().unwrap() =
            Some("https://evil.example/verify".to_owned());
        assert_eq!(
            login.login(&TestRuntime::default()).await.unwrap_err(),
            ProviderError::DeviceAuthorizationFailed
        );
    }

    #[tokio::test]
    async fn refresh_rotates_tokens_and_preserves_identity() {
        let (login, state) = mock_login().await;
        state.queue.lock().unwrap().push_back((
            200,
            json!({
                "access_token": ROTATED_ACCESS,
                "refresh_token": ROTATED_REFRESH,
                "token_type": "Bearer",
                "expires_in": 7200
            }),
        ));
        let payload = grok_payload(REFRESH, &login.token_url);
        let refreshed = login.refresh_payload(&payload).await.unwrap();
        assert_eq!(refreshed.payload.as_value()["access_token"], ROTATED_ACCESS);
        assert_eq!(
            refreshed.payload.as_value()["refresh_token"],
            ROTATED_REFRESH
        );
        assert_eq!(refreshed.payload.as_value()["email"], "user@example.test");
        assert_eq!(refreshed.payload.as_value()["subject"], "sub-fixture");
        let seen = state.seen.lock().unwrap();
        assert!(seen[0].contains("grant_type=refresh_token"));
        assert!(seen[0].contains(&format!("client_id={GROK_CLIENT_ID}")));
        assert!(seen[0].contains(&format!("refresh_token={REFRESH}")));
    }

    #[tokio::test]
    async fn refresh_keeps_old_refresh_token_when_omitted() {
        let (login, state) = mock_login().await;
        state.queue.lock().unwrap().push_back((
            200,
            json!({
                "access_token": ROTATED_ACCESS,
                "token_type": "Bearer",
                "expires_in": 3600
            }),
        ));
        let payload = grok_payload(REFRESH, &login.token_url);
        let refreshed = login.refresh_payload(&payload).await.unwrap();
        assert_eq!(refreshed.payload.as_value()["refresh_token"], REFRESH);
        assert_eq!(refreshed.payload.as_value()["access_token"], ROTATED_ACCESS);
    }

    #[tokio::test]
    async fn refresh_401_403_and_invalid_grant_are_unauthorized() {
        for (status, body) in [
            (401, Value::Null),
            (403, Value::Null),
            (400, json!({"error": "invalid_grant"})),
        ] {
            let (login, state) = mock_login().await;
            state.queue.lock().unwrap().push_back((status, body));
            let payload = grok_payload(REFRESH, &login.token_url);
            assert_eq!(
                login.refresh_payload(&payload).await.unwrap_err(),
                ProviderError::Unauthorized
            );
        }
    }

    #[tokio::test]
    async fn refresh_5xx_retries_three_times_then_fails() {
        let (login, state) = mock_login().await;
        for _ in 0..3 {
            state
                .queue
                .lock()
                .unwrap()
                .push_back((503, "overload".into()));
        }
        let payload = grok_payload(REFRESH, &login.token_url);
        assert_eq!(
            login.refresh_payload(&payload).await.unwrap_err(),
            ProviderError::Retryable
        );
        assert_eq!(state.token_hits.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn refresh_rejects_non_xai_stored_endpoint_without_network() {
        let login = GrokLogin::new();
        let payload = grok_payload(REFRESH, "https://evil.example/oauth/token");
        let error = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(login.refresh_payload(&payload))
            .unwrap_err();
        assert_eq!(error, ProviderError::InvalidPayload);
    }

    #[test]
    fn refresh_rejects_foreign_or_incomplete_payload() {
        let login = GrokLogin::new();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let kimi = ProviderPayload::new(json!({
            "version": 1,
            "access_token": "x",
            "refresh_token": REFRESH,
            "device_id": "abc"
        }));
        assert_eq!(
            runtime.block_on(login.refresh_payload(&kimi)).unwrap_err(),
            ProviderError::InvalidPayload
        );
        let missing_refresh = ProviderPayload::new(json!({
            "version": 1,
            "provider": "grok",
            "access_token": "x",
            "token_endpoint": "https://auth.x.ai/oauth2/token"
        }));
        assert_eq!(
            runtime
                .block_on(login.refresh_payload(&missing_refresh))
                .unwrap_err(),
            ProviderError::InvalidPayload
        );
    }

    #[tokio::test]
    async fn wire_debug_prints_presence_only() {
        let wire = DeviceAuthorizationResponse {
            user_code: USER_CODE.into(),
            device_code: "device-secret".into(),
            verification_uri: Some("https://accounts.x.ai/v".into()),
            verification_uri_complete: Some("https://accounts.x.ai/v?user_code=WXYZ-1234".into()),
            expires_in: Some(1800),
            interval: Some(1),
        };
        let rendered = format!("{wire:?}");
        assert!(rendered.contains("user_code_present: true"));
        assert!(!rendered.contains(USER_CODE));
        assert!(!rendered.contains("device-secret"));

        let token = TokenWireResponse {
            access_token: Some(ACCESS.into()),
            refresh_token: Some(REFRESH.into()),
            id_token: Some(fake_jwt("user@example.test", "sub-fixture")),
            expires_in: Some(json!(3600)),
            token_type: Some("Bearer".into()),
            error: None,
            error_description: None,
        };
        let rendered = format!("{token:?}");
        assert!(rendered.contains("access_token_present: true"));
        assert!(!rendered.contains(ACCESS));
        assert!(!rendered.contains(REFRESH));
        assert!(!rendered.contains("user@example.test"));
    }

    #[test]
    fn constants_are_fixed() {
        assert_eq!(GROK_CLIENT_ID, "b1a00492-073a-47ea-816f-4c329264a828");
        assert_eq!(GROK_ISSUER, "https://auth.x.ai");
        assert_eq!(
            GROK_DISCOVERY_URL,
            "https://auth.x.ai/.well-known/openid-configuration"
        );
        assert_eq!(GROK_API_BASE, "https://cli-chat-proxy.grok.com/v1");
        assert_eq!(GROK_LOGIN_TIMEOUT, Duration::from_secs(30 * 60));
        assert!(!GROK_SCOPE.contains("secret"));
    }

    #[test]
    fn provider_error_display_is_non_secret() {
        for error in [
            ProviderError::DeviceAuthorizationFailed,
            ProviderError::TokenExchangeFailed,
            ProviderError::AuthorizationDenied,
            ProviderError::InvalidPayload,
        ] {
            let rendered = format!("{error:?}{error}");
            assert!(!rendered.contains(ACCESS));
            assert!(!rendered.contains(REFRESH));
            assert!(!rendered.contains(GROK_CLIENT_ID));
        }
    }
}

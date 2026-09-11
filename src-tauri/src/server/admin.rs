//! 服务端点 Bearer token 鉴权（KB/Wiki REST 与 MCP）。
//!
//! 凭证域划分：
//! - `/v1/*` 数据面：后台创建的 `sk-waliapi-*` 密钥（handlers 内校验）；
//! - `/admin/api/*` Web 管理面板：用户名/密码会话（`admin_routes.rs`）；
//! - RAG 查询：后台创建并授予知识库权限的 API Key；
//! - KB/Wiki 管理 REST（`/api/kb/*`、`/api/wiki/*`）：`WALIAPI_ADMIN_TOKEN`；
//! - MCP（`/mcp*`）：`WALIAPI_MCP_TOKEN`（独立凭证，MCP 客户端不获得管理面权限）。
//!
//! 普通 API Key 可查询显式授权的 RAG；未认证访问仍返回 401，管理端凭据保持兼容。

use std::sync::Arc;

use axum::{
    body::Body,
    extract::State,
    http::{header, HeaderMap, Request, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};

use super::router::SharedState;

/// token 建议最小长度（README / compose / systemd 示例均要求 ≥32 字符）。
pub const MIN_TOKEN_LEN: usize = 32;

/// 服务端点 token 对：`WALIAPI_ADMIN_TOKEN` / `WALIAPI_MCP_TOKEN`。
#[derive(Debug, Clone, Default)]
pub struct ServiceTokens {
    pub admin: Option<Arc<str>>,
    pub mcp: Option<Arc<str>>,
}

impl ServiceTokens {
    /// 从环境变量读取（trim 后空串视为未配置）。纯读取，无日志副作用，
    /// 供路由装配（`router::build_router`）与启动告警（`server::start_server`）共用。
    pub fn from_env() -> Self {
        Self {
            admin: read_token("WALIAPI_ADMIN_TOKEN"),
            mcp: read_token("WALIAPI_MCP_TOKEN"),
        }
    }

    /// token 配置本身的问题（无论绑定地址都应告警）：过短、两个 token 相同。
    pub fn config_warnings(&self) -> Vec<String> {
        let mut warnings = Vec::new();
        for (name, token) in [
            ("WALIAPI_ADMIN_TOKEN", &self.admin),
            ("WALIAPI_MCP_TOKEN", &self.mcp),
        ] {
            match token {
                None => {}
                Some(t) if t.len() < MIN_TOKEN_LEN => warnings.push(format!(
                    "{name} 长度不足 {MIN_TOKEN_LEN} 字符，存在被暴力猜解的风险，建议使用 openssl rand -hex 32 生成"
                )),
                Some(_) => {}
            }
        }
        if let (Some(admin), Some(mcp)) = (&self.admin, &self.mcp) {
            if admin == mcp {
                warnings.push(
                    "WALIAPI_ADMIN_TOKEN 与 WALIAPI_MCP_TOKEN 相同：MCP 客户端将同时获得 \
                     KB/Wiki REST 管理权限，违背最小权限划分，请设置为不同的随机值"
                        .to_string(),
                );
            }
        }
        warnings
    }

    /// 绑定非回环地址且任一必需 token 缺失时的暴露面告警（多行，启动日志醒目输出）。
    /// 返回 `None` 表示无此告警（token 齐全或仅绑定回环）。
    pub fn exposure_warning(&self, host: &str) -> Option<String> {
        if is_loopback_host(host) {
            return None;
        }
        let mut missing = Vec::new();
        if self.admin.is_none() {
            missing.push("WALIAPI_ADMIN_TOKEN（KB/Wiki REST /api/kb、/api/wiki）");
        }
        if self.mcp.is_none() {
            missing.push("WALIAPI_MCP_TOKEN（MCP 端点 /mcp）");
        }
        if missing.is_empty() {
            return None;
        }
        Some(format!(
            "服务绑定在非回环地址 {host}，但未配置：\n  - {}\n\
             缺失 token 的旧管理入口不可用；已授权的 API Key 仍可查询 RAG，匿名访问返回 401；\n\
             但 /v1 数据面（仅 sk-waliapi-* 密钥保护）与 Web 管理面板（登录会话保护）\
             将直接暴露给该网络上的所有主机。\n\
             请设置上述环境变量，或改绑 127.0.0.1 并由反向代理（Caddy/Nginx + HTTPS）对外提供服务。",
            missing.join("\n  - ")
        ))
    }

    /// 缺少环境变量 token 的旧管理入口；不影响已授权的 API Key 查询。
    pub fn disabled_endpoints(&self) -> Vec<&'static str> {
        let mut endpoints = Vec::new();
        if self.admin.is_none() {
            endpoints.push("KB/Wiki REST 管理权限");
        }
        if self.mcp.is_none() {
            endpoints.push("MCP 管理工具及旧版 SSE");
        }
        endpoints
    }
}

fn read_token(name: &str) -> Option<Arc<str>> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .map(Arc::from)
}

/// 判断监听地址是否回环（127.0.0.0/8、::1、localhost；容忍 IPv6 方括号形态）。
pub fn is_loopback_host(host: &str) -> bool {
    let trimmed = host.trim_matches(|c| c == '[' || c == ']');
    if trimmed.eq_ignore_ascii_case("localhost") {
        return true;
    }
    trimmed
        .parse::<std::net::IpAddr>()
        .map(|ip| ip.is_loopback())
        .unwrap_or(false)
}

/// 恒定时间比较：比较耗时只与两串较长者的长度相关，不随匹配位置提前返回，
/// 防止按字节探测 token 前缀的时序攻击。
pub(crate) fn token_matches(candidate: &str, expected: &str) -> bool {
    let candidate = candidate.as_bytes();
    let expected = expected.as_bytes();
    let max = candidate.len().max(expected.len());
    let mut different = candidate.len() ^ expected.len();
    for index in 0..max {
        different |=
            (*candidate.get(index).unwrap_or(&0) ^ *expected.get(index).unwrap_or(&0)) as usize;
    }
    different == 0
}

fn authorized(headers: &HeaderMap, expected: &str) -> bool {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(|candidate| token_matches(candidate, expected))
        .unwrap_or(false)
}

/// KB/Wiki 管理接受 ADMIN token；普通 API Key 仅可进入授权的 RAG 查询路由。
pub async fn require_admin(
    State(shared): State<SharedState>,
    headers: HeaderMap,
    request: Request<Body>,
    next: Next,
) -> Response {
    if shared
        .admin_token
        .as_deref()
        .is_some_and(|token| authorized(&headers, token))
    {
        next.run(request).await
    } else {
        super::knowledge_access::require_read_access(shared, request, next).await
    }
}

/// MCP 端点守卫：兼容 MCP token，API Key 仅提供无会话查询工具。
/// 独立于管理员 token，避免 MCP 客户端获得渠道/密钥/设置管理权限。
pub async fn require_mcp(
    State(shared): State<SharedState>,
    headers: HeaderMap,
    request: Request<Body>,
    next: Next,
) -> Response {
    if shared
        .mcp_token
        .as_deref()
        .is_some_and(|token| authorized(&headers, token))
    {
        return next.run(request).await;
    }
    let access = match super::knowledge_access::authenticate(&shared, &headers).await {
        Ok(access) => access,
        Err(error) => return error.into_response(),
    };
    // API Key 使用无会话的 HTTP POST，不能进入旧版共享 SSE 会话。
    if request.method() != axum::http::Method::POST {
        return (
            StatusCode::METHOD_NOT_ALLOWED,
            "API Key MCP access uses stateless HTTP POST /mcp",
        )
            .into_response();
    }
    if !matches!(request.uri().path(), "/mcp" | "/mcp/") || request.uri().query().is_some() {
        return (
            StatusCode::FORBIDDEN,
            "API Key cannot access legacy MCP sessions",
        )
            .into_response();
    }
    let mut request = request;
    request.extensions_mut().insert(access);
    next.run(request).await
}

#[cfg(test)]
mod tests {
    use super::*;

    const ADMIN: &str = "admin-token-0123456789abcdef0123456789abcdef";
    const MCP: &str = "mcp-token-fedcba9876543210fedcba9876543210";

    fn tokens(admin: Option<&str>, mcp: Option<&str>) -> ServiceTokens {
        ServiceTokens {
            admin: admin.map(Arc::from),
            mcp: mcp.map(Arc::from),
        }
    }

    #[test]
    fn token_comparison_requires_equal_values() {
        assert!(token_matches("abc", "abc"));
        assert!(!token_matches("abc", "abd"));
        assert!(!token_matches("abc", "abcd"));
        assert!(!token_matches("", "abcd"));
        assert!(token_matches("", ""));
    }

    #[test]
    fn authorization_requires_bearer_token() {
        let mut headers = HeaderMap::new();
        assert!(!authorized(&headers, "secret"));
        headers.insert(header::AUTHORIZATION, "Basic secret".parse().unwrap());
        assert!(!authorized(&headers, "secret"));
        headers.insert(header::AUTHORIZATION, "Bearer secret".parse().unwrap());
        assert!(authorized(&headers, "secret"));
        headers.insert(header::AUTHORIZATION, "Bearer other".parse().unwrap());
        assert!(!authorized(&headers, "secret"));
    }

    #[test]
    fn config_warnings_flag_short_and_identical_tokens() {
        // 齐全、够长且不同：无告警
        assert!(tokens(Some(ADMIN), Some(MCP)).config_warnings().is_empty());

        // 过短
        let warnings = tokens(Some("short"), Some(MCP)).config_warnings();
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("WALIAPI_ADMIN_TOKEN"));

        // 两个 token 相同
        let warnings = tokens(Some(ADMIN), Some(ADMIN)).config_warnings();
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("相同"));
    }

    #[test]
    fn exposure_warning_only_for_non_loopback_with_missing_token() {
        let full = tokens(Some(ADMIN), Some(MCP));
        assert!(full.exposure_warning("0.0.0.0").is_none());

        let partial = tokens(Some(ADMIN), None);
        // 回环绑定：缺 token 也不产生暴露面告警
        assert!(partial.exposure_warning("127.0.0.1").is_none());
        assert!(partial.exposure_warning("localhost").is_none());
        assert!(partial.exposure_warning("::1").is_none());
        // 非回环：告警并点名缺失项与仍暴露的面
        let warning = partial.exposure_warning("0.0.0.0").unwrap();
        assert!(warning.contains("WALIAPI_MCP_TOKEN"));
        assert!(warning.contains("/v1"));

        let none = ServiceTokens::default();
        let warning = none.exposure_warning("192.168.1.10").unwrap();
        assert!(warning.contains("WALIAPI_ADMIN_TOKEN"));
        assert!(warning.contains("WALIAPI_MCP_TOKEN"));
    }

    #[test]
    fn disabled_endpoints_reflect_missing_tokens() {
        assert!(ServiceTokens::default().disabled_endpoints().len() == 2);
        assert_eq!(
            tokens(Some(ADMIN), None).disabled_endpoints(),
            vec!["MCP 管理工具及旧版 SSE"]
        );
        assert!(tokens(Some(ADMIN), Some(MCP))
            .disabled_endpoints()
            .is_empty());
    }

    #[test]
    fn loopback_detection_covers_ipv4_ipv6_and_localhost() {
        for host in [
            "127.0.0.1",
            "127.8.8.8",
            "localhost",
            "LOCALHOST",
            "::1",
            "[::1]",
        ] {
            assert!(is_loopback_host(host), "{host} 应识别为回环");
        }
        for host in [
            "0.0.0.0",
            "192.168.1.10",
            "10.0.0.1",
            "::",
            "[::]",
            "example.com",
        ] {
            assert!(!is_loopback_host(host), "{host} 不应识别为回环");
        }
    }
}

//! Web 管理面板 REST 路由（`/admin/api/*`）。
//!
//! - `/auth/*`：登录 / 登出 / 修改密码 / 会话检查（login 公开，其余需认证）
//! - `/invoke`：与 Tauri invoke 语义 1:1 对应的单一入口，按 cmd 名分发到 commands 函数
//! - `/events`：SSE 事件桥，事件名与桌面端 `app.emit` 一致
//!
//! 除 `/auth/login` 外全部要求管理员会话（Bearer token 或 `waliapi_admin_token` Cookie）。

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use axum::{
    extract::{Extension, Request, State},
    http::{header, HeaderMap, HeaderValue, Method, StatusCode},
    middleware::{self, Next},
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse, Json, Response,
    },
    routing::{get, post},
    Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
#[cfg(feature = "desktop-ui")]
use tauri::AppHandle;
use tokio::sync::broadcast;

use super::admin_auth::{self, AdminSession};
use super::router::SharedState;
use crate::commands;
use crate::AppState;

const SESSION_COOKIE: &str = "waliapi_admin_token";
const SESSION_MAX_AGE_SECS: u64 = 7 * 24 * 3600;

/// 认证通过后注入 request extensions 的身份信息。
#[derive(Clone)]
pub struct AdminIdentity {
    pub token: String,
    pub session: AdminSession,
}

pub fn router(shared: SharedState) -> Router<SharedState> {
    let public = Router::new().route("/auth/login", post(login));

    let protected = Router::new()
        .route("/auth/logout", post(logout))
        .route("/auth/check", get(check))
        .route("/auth/change-password", post(change_password))
        .route("/auth/change-username", post(change_username))
        .route("/invoke", post(invoke_handler))
        .route("/events", get(events_handler))
        .route_layer(middleware::from_fn_with_state(shared.clone(), require_auth));

    Router::new()
        .merge(public)
        .merge(protected)
        .layer(middleware::from_fn(csrf_guard))
}

/// CSRF 防护：所有变更类方法必须带 `X-Requested-With: XMLHttpRequest` 头。
/// 浏览器跨域表单/简单请求无法携带自定义头；跨域 fetch 携带自定义头会触发预检，
/// 而 /admin/api 不下发任何 CORS 允许头，预检必然失败 —— 双重拦截跨站伪造请求。
/// GET/HEAD（含 SSE /events）无状态变更，且响应在无 CORS 头时跨域不可读，无需校验。
async fn csrf_guard(req: Request, next: Next) -> Result<Response, (StatusCode, Json<Value>)> {
    if matches!(
        *req.method(),
        Method::POST | Method::PUT | Method::PATCH | Method::DELETE
    ) {
        let has_header = req
            .headers()
            .get("x-requested-with")
            .and_then(|v| v.to_str().ok())
            .map(|v| v.eq_ignore_ascii_case("XMLHttpRequest"))
            .unwrap_or(false);
        if !has_header {
            return Err((
                StatusCode::FORBIDDEN,
                Json(json!({ "error": "CSRF 校验失败" })),
            ));
        }
    }
    Ok(next.run(req).await)
}

// ── 认证中间件 ────────────────────────────────────────────────────────────────

fn extract_token(headers: &HeaderMap) -> Option<String> {
    if let Some(value) = headers.get(header::AUTHORIZATION) {
        if let Ok(value) = value.to_str() {
            if let Some(token) = value
                .strip_prefix("Bearer ")
                .or_else(|| value.strip_prefix("bearer "))
            {
                let token = token.trim();
                if !token.is_empty() {
                    return Some(token.to_string());
                }
            }
        }
    }
    let cookie_header = headers.get(header::COOKIE)?.to_str().ok()?;
    for pair in cookie_header.split(';') {
        let pair = pair.trim();
        if let Some(value) = pair.strip_prefix(SESSION_COOKIE) {
            if let Some(value) = value.strip_prefix('=') {
                let value = value.trim();
                if !value.is_empty() {
                    return Some(value.to_string());
                }
            }
        }
    }
    None
}

async fn require_auth(
    State(shared): State<SharedState>,
    mut req: Request,
    next: Next,
) -> Result<Response, (StatusCode, Json<Value>)> {
    let token = extract_token(req.headers()).ok_or_else(unauthorized)?;
    let session = shared
        .state
        .admin_sessions
        .get(&token)
        .ok_or_else(unauthorized)?;
    req.extensions_mut().insert(AdminIdentity { token, session });
    Ok(next.run(req).await)
}

fn unauthorized() -> (StatusCode, Json<Value>) {
    (
        StatusCode::UNAUTHORIZED,
        Json(json!({ "error": "未登录或会话已过期" })),
    )
}

fn internal_error(message: String) -> (StatusCode, Json<Value>) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({ "error": message })),
    )
}

fn bad_request(message: String) -> (StatusCode, Json<Value>) {
    (StatusCode::BAD_REQUEST, Json(json!({ "error": message })))
}

fn session_cookie_header(token: &str) -> HeaderValue {
    HeaderValue::from_str(&format!(
        "{SESSION_COOKIE}={token}; Path=/; Max-Age={SESSION_MAX_AGE_SECS}; SameSite=Lax"
    ))
    .expect("session token is header-safe")
}

// ── /auth/* ───────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct LoginBody {
    username: String,
    password: String,
}

async fn login(
    State(shared): State<SharedState>,
    Json(body): Json<LoginBody>,
) -> Result<Response, (StatusCode, Json<Value>)> {
    let user = admin_auth::find_user_by_username(&shared.state.db.pool, body.username.trim())
        .await
        .map_err(internal_error)?
        .ok_or_else(|| bad_request("用户名或密码错误".to_string()))?;

    if !admin_auth::verify_password(&body.password, &user.password_hash) {
        return Err(bad_request("用户名或密码错误".to_string()));
    }

    let token = admin_auth::generate_token();
    let session = AdminSession {
        user_id: user.id,
        username: user.username,
        must_change_password: user.must_change_password,
    };
    shared
        .state
        .admin_sessions
        .insert(token.clone(), session.clone());

    let mut response = Json(json!({
        "token": token,
        "username": session.username,
        "must_change_password": session.must_change_password,
    }))
    .into_response();
    response
        .headers_mut()
        .insert(header::SET_COOKIE, session_cookie_header(&token));
    Ok(response)
}

async fn logout(
    State(shared): State<SharedState>,
    Extension(identity): Extension<AdminIdentity>,
) -> Response {
    shared.state.admin_sessions.remove(&identity.token);
    let mut response = Json(json!({ "ok": true })).into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_static("waliapi_admin_token=; Path=/; Max-Age=0; SameSite=Lax"),
    );
    response
}

async fn check(Extension(identity): Extension<AdminIdentity>) -> Json<Value> {
    Json(json!({
        "username": identity.session.username,
        "must_change_password": identity.session.must_change_password,
    }))
}

#[derive(Deserialize)]
struct ChangePasswordBody {
    old_password: String,
    new_password: String,
}

async fn change_password(
    State(shared): State<SharedState>,
    Extension(identity): Extension<AdminIdentity>,
    Json(body): Json<ChangePasswordBody>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    if body.new_password.len() < 8 {
        return Err(bad_request("新密码长度至少 8 位".to_string()));
    }
    let user = admin_auth::find_user_by_username(&shared.state.db.pool, &identity.session.username)
        .await
        .map_err(internal_error)?
        .ok_or_else(unauthorized)?;
    if !admin_auth::verify_password(&body.old_password, &user.password_hash) {
        return Err(bad_request("原密码错误".to_string()));
    }
    admin_auth::update_password(&shared.state.db.pool, &user.id, &body.new_password)
        .await
        .map_err(internal_error)?;

    // 会话保持有效，仅清除强制改密标记
    let session = AdminSession {
        must_change_password: false,
        ..identity.session.clone()
    };
    shared
        .state
        .admin_sessions
        .insert(identity.token.clone(), session);

    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct ChangeUsernameBody {
    new_username: String,
}

async fn change_username(
    State(shared): State<SharedState>,
    Extension(identity): Extension<AdminIdentity>,
    Json(body): Json<ChangeUsernameBody>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let new_username = body.new_username.trim().to_string();
    if new_username.len() < 2 || new_username.len() > 32 {
        return Err(bad_request("用户名长度须为 2-32 个字符".to_string()));
    }
    if new_username == identity.session.username {
        return Ok(Json(json!({ "ok": true, "username": new_username })));
    }
    // 唯一性校验（admin_users.username 有 UNIQUE 约束，提前给出友好错误）
    if admin_auth::find_user_by_username(&shared.state.db.pool, &new_username)
        .await
        .map_err(internal_error)?
        .is_some()
    {
        return Err(bad_request("用户名已被占用".to_string()));
    }
    admin_auth::update_username(&shared.state.db.pool, &identity.session.user_id, &new_username)
        .await
        .map_err(internal_error)?;

    // 同步更新当前会话中的用户名
    let session = AdminSession {
        username: new_username.clone(),
        ..identity.session.clone()
    };
    shared
        .state
        .admin_sessions
        .insert(identity.token.clone(), session);

    Ok(Json(json!({ "ok": true, "username": new_username })))
}

// ── /events（SSE 事件桥）─────────────────────────────────────────────────────

async fn events_handler(
    State(shared): State<SharedState>,
) -> Sse<impl futures_util::Stream<Item = Result<Event, Infallible>>> {
    let rx = shared.state.events.subscribe();
    let stream = async_stream::stream! {
        let mut rx = rx;
        loop {
            match rx.recv().await {
                Ok(event) => {
                    yield Ok(Event::default().event(event.event).data(event.payload.to_string()));
                }
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    tracing::debug!("SSE client lagged, skipped {} events", skipped);
                    continue;
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    };
    Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("ping"),
    )
}

// ── /invoke：Tauri command 语义分发 ──────────────────────────────────────────

#[derive(Deserialize)]
struct InvokeRequest {
    cmd: String,
    #[serde(default)]
    args: Value,
}

async fn invoke_handler(
    State(shared): State<SharedState>,
    Json(body): Json<InvokeRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    dispatch(&shared, &body.cmd, body.args)
        .await
        .map(Json)
        .map_err(internal_error)
}

/// 从 args 对象中按 Tauri 的 camelCase 键取参并反序列化为目标类型。
/// 缺失的键按 `null` 处理（`Option<T>` 参数因此得到 `None`）。
fn arg<T: serde::de::DeserializeOwned>(args: &Value, key: &str) -> Result<T, String> {
    serde_json::from_value(args.get(key).cloned().unwrap_or(Value::Null))
        .map_err(|e| format!("参数 {key} 无效: {e}"))
}

fn to_json<T: serde::Serialize>(result: Result<T, String>) -> Result<Value, String> {
    result.and_then(|value| serde_json::to_value(value).map_err(|e| e.to_string()))
}

async fn dispatch(shared: &SharedState, cmd: &str, args: Value) -> Result<Value, String> {
    let state = shared.state_static.clone();
    // 桌面专属命令的 AppHandle（仅 desktop-ui 构建存在）
    #[cfg(feature = "desktop-ui")]
    let desktop_app = |name: &str| -> Result<&'static AppHandle, String> {
        shared
            .desktop_app
            .ok_or_else(|| format!("{name} 仅桌面版可用"))
    };
    match cmd {
        // ── 渠道 ──
        "get_channels" => to_json(commands::channel::get_channels(state).await),
        "get_channel" => to_json(commands::channel::get_channel(arg(&args, "id")?, state).await),
        "get_channel_api_key" => {
            to_json(commands::channel::get_channel_api_key(arg(&args, "id")?, state).await)
        }
        "get_channel_presets" => to_json(commands::channel::get_channel_presets()),
        "create_channel" => {
            to_json(commands::channel::create_channel(arg(&args, "input")?, state).await)
        }
        "update_channel" => {
            to_json(commands::channel::update_channel(arg(&args, "input")?, state).await)
        }
        "toggle_channel" => {
            to_json(
                commands::channel::toggle_channel(arg(&args, "id")?, arg(&args, "status")?, state)
                    .await,
            )
        }
        "delete_channel" => {
            to_json(commands::channel::delete_channel(arg(&args, "id")?, state).await)
        }
        "test_channel" => to_json(commands::channel::test_channel(arg(&args, "id")?, state).await),
        "test_channel_draft" => {
            to_json(commands::channel::test_channel_draft(arg(&args, "input")?, state).await)
        }
        "sync_upstream_models" => {
            to_json(commands::channel::sync_upstream_models(arg(&args, "input")?, state).await)
        }
        "get_channel_stats" => to_json(commands::channel::get_channel_stats(state).await),
        "reorder_channels" => {
            to_json(commands::channel::reorder_channels(arg(&args, "orderedIds")?, state).await)
        }
        "get_channel_extra_keys" => {
            to_json(commands::channel::get_channel_extra_keys(arg(&args, "id")?, state).await)
        }
        "get_channel_extra_key_value" => {
            to_json(
                commands::channel::get_channel_extra_key_value(arg(&args, "keyId")?, state).await,
            )
        }
        "toggle_channel_extra_key" => {
            to_json(
                commands::channel::toggle_channel_extra_key(
                    arg(&args, "keyId")?,
                    arg(&args, "status")?,
                    state,
                )
                .await,
            )
        }
        "delete_channel_extra_key" => {
            to_json(commands::channel::delete_channel_extra_key(arg(&args, "keyId")?, state).await)
        }

        // ── API 密钥 ──
        "get_api_keys" => to_json(commands::api_key::get_api_keys(state).await),
        "create_api_key" => {
            to_json(commands::api_key::create_api_key(arg(&args, "input")?, state).await)
        }
        "update_api_key" => {
            to_json(commands::api_key::update_api_key(arg(&args, "input")?, state).await)
        }
        "delete_api_key" => {
            to_json(commands::api_key::delete_api_key(arg(&args, "id")?, state).await)
        }
        "get_api_key_stats" => to_json(commands::api_key::get_api_key_stats(state).await),

        // ── 日志 ──
        "get_logs" => to_json(commands::log::get_logs(arg(&args, "input")?, state).await),
        "get_log" => to_json(commands::log::get_log(arg(&args, "id")?, state).await),
        "get_log_security_findings" => {
            to_json(commands::log::get_log_security_findings(arg(&args, "logId")?, state).await)
        }
        "get_log_stats" => to_json(commands::log::get_log_stats(arg(&args, "days")?, state).await),
        "delete_log" => to_json(commands::log::delete_log(arg(&args, "id")?, state).await),
        "delete_logs_before" => {
            to_json(commands::log::delete_logs_before(arg(&args, "beforeDate")?, state).await)
        }
        "delete_all_logs" => to_json(commands::log::delete_all_logs(state).await),
        // 历史 499 日志一次性修复：默认 dry-run，input.apply=true 才写库。
        // input 缺省为 {}，让不带参数直接调用也能拿到报告。
        "repair_stream_cancel_logs" => {
            let input = args.get("input").cloned().unwrap_or_else(|| json!({}));
            let input: commands::log_repair::RepairInput =
                serde_json::from_value(input).map_err(|e| format!("参数 input 无效: {e}"))?;
            to_json(commands::log_repair::repair_stream_cancel_logs(input, state).await)
        }

        // ── Auth 账号 ──
        "auth_accounts_list" => to_json(commands::auth::auth_accounts_list(state).await),
        "auth_providers_list" => to_json(commands::auth::auth_providers_list().await),
        #[cfg(feature = "desktop-ui")]
        "auth_login" => {
            let app = desktop_app("OAuth 登录")?;
            to_json(commands::auth::auth_login(arg(&args, "provider")?, app.clone(), state).await)
        }
        #[cfg(not(feature = "desktop-ui"))]
        "auth_login" => Err("OAuth 登录仅桌面版可用，请使用 auth.json 导入".to_string()),
        // 设备码提供商（Kimi）无需桌面壳即可登录：verification URL + user code
        // 通过轮询的会话状态下发；browser-callback 提供商仍在命令内按 app 缺失拒绝。
        "auth_login_start" => {
            #[cfg(feature = "desktop-ui")]
            let app = shared.desktop_app.cloned();
            #[cfg(not(feature = "desktop-ui"))]
            let app: Option<tauri::AppHandle> = None;
            to_json(
                commands::auth::auth_login_start_with(
                    arg(&args, "provider")?,
                    arg(&args, "replaceAccountId")?,
                    app,
                    state.inner(),
                )
                .await,
            )
        }
        "auth_login_status" => {
            to_json(commands::auth::auth_login_status(arg(&args, "sessionId")?, state).await)
        }
        "auth_login_cancel" => {
            to_json(commands::auth::auth_login_cancel(arg(&args, "sessionId")?, state).await)
        }
        "auth_login_import" => {
            to_json(
                commands::auth::auth_login_import(
                    arg(&args, "provider")?,
                    arg(&args, "path")?,
                    arg(&args, "format")?,
                    state,
                )
                .await,
            )
        }
        "auth_login_import_content" => {
            to_json(
                commands::auth::auth_login_import_content(
                    arg(&args, "provider")?,
                    arg(&args, "content")?,
                    arg(&args, "format")?,
                    state,
                )
                .await,
            )
        }
        "auth_default_import_path" => to_json(commands::auth::auth_default_import_path().await),
        "auth_logout" => to_json(commands::auth::auth_logout(arg(&args, "id")?, state).await),
        "auth_refresh_token" => {
            to_json(commands::auth::auth_refresh_token(arg(&args, "id")?, state).await)
        }
        "auth_sync_models" => {
            to_json(commands::auth::auth_sync_models(arg(&args, "id")?, state).await)
        }
        "auth_export_json" => {
            to_json(
                commands::auth::auth_export_json(arg(&args, "id")?, arg(&args, "path")?, state)
                    .await,
            )
        }
        "auth_export_json_content" => {
            to_json(commands::auth::auth_export_json_content(arg(&args, "id")?, state).await)
        }
        "auth_toggle" => {
            to_json(
                commands::auth::auth_toggle(arg(&args, "id")?, arg(&args, "disabled")?, state)
                    .await,
            )
        }
        "auth_quota_status" => {
            to_json(commands::auth::auth_quota_status(arg(&args, "id")?, state).await)
        }
        "auth_update" => to_json(commands::auth::auth_update(arg(&args, "input")?, state).await),

        // ── 仪表盘 / 设置 / 服务状态 ──
        "get_dashboard_stats" => to_json(commands::stats::get_dashboard_stats(state).await),
        "get_model_stats" => to_json(commands::stats::get_model_stats(state).await),
        "get_token_trend" => to_json(commands::stats::get_token_trend(arg(&args, "hours")?, state).await),
        "get_settings" => to_json(commands::settings::get_settings(state).await),
        "save_settings" => {
            to_json(commands::settings::save_settings(arg(&args, "settings")?, state).await)
        }
        "apply_theme" => {
            to_json(commands::settings::apply_theme(arg(&args, "theme")?, state).await)
        }
        #[cfg(feature = "desktop-ui")]
        "set_auto_start" => {
            let app = desktop_app("开机自启")?;
            to_json(commands::settings::set_auto_start(arg(&args, "enabled")?, app.clone()).await)
        }
        #[cfg(not(feature = "desktop-ui"))]
        "set_auto_start" => Ok(Value::Null),
        "get_feature_flags" => to_json(commands::settings::get_feature_flags(state)),
        "get_server_status" => to_json(commands::server::get_server_status(state).await),
        #[cfg(feature = "desktop-ui")]
        "restart_server" => match shared.desktop_app {
            Some(app) => to_json(commands::server::restart_server(app.clone(), state).await),
            None => to_json(restart_server_headless(shared).await),
        },
        #[cfg(not(feature = "desktop-ui"))]
        "restart_server" => to_json(restart_server_headless(shared).await),
        "get_service_statuses" => to_json(commands::services::get_service_statuses(state).await),

        // ── 安全规则 ──
        "get_builtin_security_rules" => {
            to_json(commands::security::get_builtin_security_rules(state).await)
        }
        "update_builtin_security_rule" => {
            to_json(
                commands::security::update_builtin_security_rule(
                    arg(&args, "id")?,
                    arg(&args, "input")?,
                    state,
                )
                .await,
            )
        }
        "delete_builtin_security_rule" => {
            to_json(commands::security::delete_builtin_security_rule(arg(&args, "id")?, state).await)
        }
        "reset_builtin_security_rules" => {
            to_json(commands::security::reset_builtin_security_rules(state).await)
        }
        "get_custom_security_rules" => {
            to_json(commands::security::get_custom_security_rules(state).await)
        }
        "create_custom_security_rule" => {
            to_json(
                commands::security::create_custom_security_rule(arg(&args, "input")?, state).await,
            )
        }
        "toggle_custom_security_rule" => {
            to_json(
                commands::security::toggle_custom_security_rule(
                    arg(&args, "id")?,
                    arg(&args, "enabled")?,
                    state,
                )
                .await,
            )
        }
        "delete_custom_security_rule" => {
            to_json(commands::security::delete_custom_security_rule(arg(&args, "id")?, state).await)
        }

        // ── 导入 / 导出 ──
        "export_channels" => to_json(commands::import_export::export_channels(state).await),
        "import_walicode_backup" => {
            to_json(
                commands::import_export::import_walicode_backup(arg(&args, "content")?, state)
                    .await,
            )
        }
        "import_waliapi_export" => {
            to_json(
                commands::import_export::import_waliapi_export(arg(&args, "content")?, state).await,
            )
        }
        "scan_local_ai_configs" => to_json(commands::import_export::scan_local_ai_configs().await),
        "import_scanned_sources" => {
            to_json(
                commands::import_export::import_scanned_sources(arg(&args, "sources")?, state)
                    .await,
            )
        }
        #[cfg(feature = "desktop-ui")]
        "pick_import_file" => {
            let app = desktop_app("文件选择对话框")?;
            to_json(commands::import_export::pick_import_file(app.clone()).await)
        }
        #[cfg(not(feature = "desktop-ui"))]
        "pick_import_file" => Err("文件选择对话框仅桌面版可用".to_string()),
        #[cfg(feature = "desktop-ui")]
        "save_export_file" => {
            let app = desktop_app("文件保存对话框")?;
            to_json(
                commands::import_export::save_export_file(
                    app.clone(),
                    arg(&args, "content")?,
                    arg(&args, "defaultName")?,
                )
                .await,
            )
        }
        #[cfg(not(feature = "desktop-ui"))]
        "save_export_file" => Err("文件保存对话框仅桌面版可用".to_string()),

        // ── 知识库 ──
        "get_knowledge_bases" => {
            to_json(commands::knowledge_base::get_knowledge_bases(state).await)
        }
        "create_knowledge_base" => {
            to_json(
                commands::knowledge_base::create_knowledge_base(state, arg(&args, "input")?).await,
            )
        }
        "update_knowledge_base" => {
            to_json(
                commands::knowledge_base::update_knowledge_base(
                    state,
                    arg(&args, "id")?,
                    arg(&args, "input")?,
                )
                .await,
            )
        }
        "delete_knowledge_base" => {
            to_json(commands::knowledge_base::delete_knowledge_base(state, arg(&args, "id")?).await)
        }
        "get_kb_documents" => {
            to_json(commands::knowledge_base::get_kb_documents(state, arg(&args, "kbId")?).await)
        }
        "delete_kb_document" => {
            to_json(
                commands::knowledge_base::delete_kb_document(
                    state,
                    arg(&args, "docId")?,
                    arg(&args, "kbId")?,
                )
                .await,
            )
        }
        "reindex_kb_document" => {
            to_json(
                commands::knowledge_base::reindex_kb_document(state, arg(&args, "docId")?).await,
            )
        }
        "get_kb_tags" => {
            to_json(
                commands::knowledge_base::get_kb_tags(
                    state,
                    arg(&args, "kbId")?,
                    arg(&args, "limit")?,
                )
                .await,
            )
        }
        "search_knowledge_base" => {
            to_json(
                commands::knowledge_base::search_knowledge_base(state, arg(&args, "input")?).await,
            )
        }
        "ask_knowledge_base" => {
            to_json(
                commands::knowledge_base::ask_knowledge_base(state, arg(&args, "input")?).await,
            )
        }
        "get_kb_stats" => {
            to_json(commands::knowledge_base::get_kb_stats(state, arg(&args, "kbId")?).await)
        }
        "upload_kb_document" => {
            to_json(
                commands::knowledge_base::upload_kb_document(state, arg(&args, "input")?).await,
            )
        }
        "get_kb_conversations" => {
            to_json(
                commands::knowledge_base::get_kb_conversations(state, arg(&args, "kbId")?).await,
            )
        }
        "clear_kb_conversations" => {
            to_json(
                commands::knowledge_base::clear_kb_conversations(state, arg(&args, "kbId")?).await,
            )
        }
        "get_kb_sources" => {
            to_json(commands::knowledge_base::get_kb_sources(state, arg(&args, "kbId")?).await)
        }
        "delete_kb_source" => {
            to_json(
                commands::knowledge_base::delete_kb_source(
                    state,
                    arg(&args, "sourceId")?,
                    arg(&args, "kbId")?,
                )
                .await,
            )
        }
        "import_kb_source" => {
            to_json(
                commands::knowledge_base::import_kb_source(
                    state,
                    arg(&args, "kbId")?,
                    arg(&args, "input")?,
                )
                .await,
            )
        }
        "get_kb_index_status" => {
            to_json(
                commands::knowledge_base::get_kb_index_status(state, arg(&args, "kbId")?).await,
            )
        }
        "build_kb_index" => {
            to_json(commands::knowledge_base::build_kb_index(state, arg(&args, "kbId")?).await)
        }
        "drop_kb_index" => {
            to_json(commands::knowledge_base::drop_kb_index(state, arg(&args, "kbId")?).await)
        }
        "get_ocr_cache_info" => to_json(commands::knowledge_base::get_ocr_cache_info(state).await),
        "clear_ocr_cache" => to_json(commands::knowledge_base::clear_ocr_cache(state).await),

        // ── Wiki ──
        "get_wiki_projects" => to_json(commands::wiki::get_wiki_projects(state).await),
        "create_wiki_project" => {
            to_json(commands::wiki::create_wiki_project(state, arg(&args, "input")?).await)
        }
        "get_wiki_project" => {
            to_json(commands::wiki::get_wiki_project(state, arg(&args, "id")?).await)
        }
        "update_wiki_project" => {
            to_json(
                commands::wiki::update_wiki_project(state, arg(&args, "id")?, arg(&args, "input")?)
                    .await,
            )
        }
        "delete_wiki_project" => {
            to_json(commands::wiki::delete_wiki_project(state, arg(&args, "id")?).await)
        }
        "get_wiki_pages" => {
            to_json(commands::wiki::get_wiki_pages(state, arg(&args, "projectId")?).await)
        }
        "get_wiki_page" => {
            to_json(
                commands::wiki::get_wiki_page(state, arg(&args, "projectId")?, arg(&args, "path")?)
                    .await,
            )
        }
        "save_wiki_page" => {
            to_json(
                commands::wiki::save_wiki_page(
                    state,
                    arg(&args, "projectId")?,
                    arg(&args, "path")?,
                    arg(&args, "content")?,
                )
                .await,
            )
        }
        "get_wiki_sources" => {
            to_json(commands::wiki::get_wiki_sources(state, arg(&args, "projectId")?).await)
        }
        "add_wiki_source" => {
            to_json(
                commands::wiki::add_wiki_source(
                    state,
                    arg(&args, "projectId")?,
                    arg(&args, "input")?,
                )
                .await,
            )
        }
        "delete_wiki_source" => {
            to_json(commands::wiki::delete_wiki_source(state, arg(&args, "sourceId")?).await)
        }
        "search_wiki" => {
            to_json(
                commands::wiki::search_wiki(
                    state,
                    arg(&args, "projectId")?,
                    arg(&args, "query")?,
                    arg(&args, "topK")?,
                )
                .await,
            )
        }
        "get_wiki_graph" => {
            to_json(commands::wiki::get_wiki_graph(state, arg(&args, "projectId")?).await)
        }
        "get_wiki_tags" => {
            to_json(
                commands::wiki::get_wiki_tags(state, arg(&args, "projectId")?, arg(&args, "limit")?)
                    .await,
            )
        }
        "get_wiki_stats" => {
            to_json(commands::wiki::get_wiki_stats(state, arg(&args, "projectId")?).await)
        }
        "ingest_wiki_source" => {
            to_json(
                commands::wiki::ingest_wiki_source(
                    state,
                    arg(&args, "projectId")?,
                    arg(&args, "sourceId")?,
                )
                .await,
            )
        }
        "rescan_wiki_sources" => {
            to_json(commands::wiki::rescan_wiki_sources(state, arg(&args, "projectId")?).await)
        }

        // ── 应用配置（容器内通常全部不可用，由前端置灰）──
        "get_app_configs" => to_json(commands::app_config::get_app_configs(state).await),
        "apply_app_config" => {
            to_json(
                commands::app_config::apply_app_config(
                    arg(&args, "appName")?,
                    arg(&args, "apiKey")?,
                    arg(&args, "model")?,
                    state,
                )
                .await,
            )
        }
        "clear_app_config" => {
            to_json(commands::app_config::clear_app_config(arg(&args, "appName")?).await)
        }
        "get_app_config_content" => {
            to_json(commands::app_config::get_app_config_content(arg(&args, "appName")?).await)
        }
        "open_config_folder" => {
            to_json(commands::app_config::open_config_folder(arg(&args, "appName")?).await)
        }

        _ => Err(format!("未知命令: {cmd}")),
    }
}

/// headless 模式的服务重启：中止当前监听任务并按最新设置重新绑定端口。
async fn restart_server_headless(shared: &SharedState) -> Result<(), String> {
    let mut guard = shared.state.server_handle.write().await;
    if let Some(handle) = guard.take() {
        handle.abort();
    }
    shared
        .state
        .server_running
        .store(false, std::sync::atomic::Ordering::SeqCst);
    let state = shared.state.clone();
    let handle = tauri::async_runtime::spawn(async move {
        let _ = crate::server::start_server(state, None).await;
    });
    *guard = Some(handle);
    Ok(())
}

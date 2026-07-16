//! 本地 API 安全边界：校验回环 Host、浏览器 Origin、Bearer 令牌和 WebSocket 短票据。

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::Json;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::Serialize;
use tokio::sync::Mutex;

const ACCESS_TOKEN_BYTES: usize = 32;
const INSTANCE_ID_BYTES: usize = 16;
const WS_TICKET_BYTES: usize = 16;
const WS_TICKET_TTL: Duration = Duration::from_secs(15);
const MAX_PENDING_WS_TICKETS: usize = 128;
const ALLOWED_METHODS: &str = "GET, POST, PUT, PATCH, DELETE, OPTIONS";
const ALLOWED_HEADERS: &str = "Authorization, Content-Type, Accept";

/// 构建本地 API 安全上下文所需的显式参数。
#[derive(Debug, Clone)]
pub struct LocalApiSecurityOptions {
    pub expected_host: String,
    pub allowed_origins: Vec<String>,
    /// 仅供桌面开发壳显式允许锁定端口的 `127.0.0.1` Vite Origin。
    pub allow_loopback_dev_origins: bool,
    pub access_token: Option<String>,
}

impl LocalApiSecurityOptions {
    /// 为指定回环监听地址创建默认的强制鉴权配置。
    pub fn required(expected_host: impl Into<String>) -> Self {
        Self {
            expected_host: expected_host.into(),
            allowed_origins: Vec::new(),
            allow_loopback_dev_origins: false,
            access_token: None,
        }
    }
}

/// 桌面 UI 启动时需要的一次性运行信息。
#[derive(Debug, Clone, Serialize)]
pub struct LocalApiBootstrap {
    pub api_origin: String,
    pub access_token: String,
    pub token_type: &'static str,
    pub protocol_version: &'static str,
    pub instance_id: String,
}

/// WebSocket 短票据响应。
#[derive(Debug, Serialize)]
pub struct WsTicketResponse {
    pub ticket: String,
    pub expires_in_seconds: u64,
}

/// 鉴权后的运行状态响应。
#[derive(Debug, Serialize)]
pub struct RuntimeHealthResponse {
    pub status: &'static str,
    pub protocol_version: &'static str,
    pub instance_id: String,
}

/// 每个服务进程独享的本地 API 安全状态。
pub struct LocalApiSecurity {
    expected_host: String,
    api_origin: String,
    allowed_origins: HashSet<String>,
    access_token: String,
    instance_id: String,
    ws_tickets: Mutex<HashMap<String, Instant>>,
    ws_ticket_ttl: Duration,
}

impl LocalApiSecurity {
    /// 使用显式配置创建安全上下文。
    pub fn new(mut options: LocalApiSecurityOptions) -> Result<Arc<Self>, LocalApiSecurityError> {
        let expected_host = options.expected_host.trim().to_string();
        validate_loopback_host(&expected_host)?;

        let api_origin = format!("http://{expected_host}");
        let mut allowed_origins = HashSet::new();
        for origin in options.allowed_origins.drain(..) {
            let origin = origin.trim().to_string();
            validate_local_ui_origin(&origin, options.allow_loopback_dev_origins)?;
            allowed_origins.insert(origin);
        }

        let access_token = match options.access_token {
            Some(token) if token.trim().len() >= 32 => token.trim().to_string(),
            Some(_) => {
                return Err(LocalApiSecurityError::InvalidConfiguration(
                    "本地 API 显式访问令牌至少需要 32 个字符".to_string(),
                ));
            }
            None => random_urlsafe(ACCESS_TOKEN_BYTES)?,
        };

        Ok(Arc::new(Self {
            expected_host,
            api_origin,
            allowed_origins,
            access_token,
            instance_id: random_urlsafe(INSTANCE_ID_BYTES)?,
            ws_tickets: Mutex::new(HashMap::new()),
            ws_ticket_ttl: WS_TICKET_TTL,
        }))
    }

    /// 返回仅供可信启动通道交付给客户端的运行信息。
    pub fn bootstrap(&self) -> LocalApiBootstrap {
        LocalApiBootstrap {
            api_origin: self.api_origin.clone(),
            access_token: self.access_token.clone(),
            token_type: "Bearer",
            protocol_version: "muse-local-api/v1",
            instance_id: self.instance_id.clone(),
        }
    }

    /// 返回当前预期的 Host（含端口）。
    pub fn expected_host(&self) -> &str {
        &self.expected_host
    }

    /// 创建十五秒有效、只能消费一次的 WebSocket 票据。
    pub async fn issue_ws_ticket(&self) -> Result<WsTicketResponse, LocalApiSecurityError> {
        let now = Instant::now();
        let mut tickets = self.ws_tickets.lock().await;
        tickets.retain(|_, expires_at| *expires_at > now);
        if tickets.len() >= MAX_PENDING_WS_TICKETS {
            return Err(LocalApiSecurityError::TicketCapacityExceeded);
        }

        let ticket = random_urlsafe(WS_TICKET_BYTES)?;
        tickets.insert(ticket.clone(), now + self.ws_ticket_ttl);
        Ok(WsTicketResponse {
            ticket,
            expires_in_seconds: self.ws_ticket_ttl.as_secs(),
        })
    }

    /// 原子消费 WebSocket 票据；已消费、过期或未知票据都会返回 `false`。
    pub async fn consume_ws_ticket(&self, ticket: &str) -> bool {
        let now = Instant::now();
        self.ws_tickets
            .lock()
            .await
            .remove(ticket)
            .is_some_and(|expires_at| expires_at > now)
    }

    /// 返回不包含令牌的健康状态。
    pub fn health(&self) -> RuntimeHealthResponse {
        RuntimeHealthResponse {
            status: "ok",
            protocol_version: "muse-local-api/v1",
            instance_id: self.instance_id.clone(),
        }
    }

    fn allows_origin(&self, origin: &str) -> bool {
        self.allowed_origins.contains(origin)
    }
}

/// 为 `/api` 路由执行统一的本地安全校验。
pub async fn enforce_local_api_security(
    State(security): State<Arc<LocalApiSecurity>>,
    request: Request,
    next: Next,
) -> Response {
    let headers = request.headers();
    if !has_expected_host(headers, security.expected_host()) {
        return security_error(
            StatusCode::MISDIRECTED_REQUEST,
            "invalid_host",
            "请求 Host 与本次本地服务不匹配",
            false,
        );
    }

    let origin = match read_origin(headers) {
        Ok(origin) => origin,
        Err(message) => {
            return security_error(StatusCode::FORBIDDEN, "origin_denied", message, false);
        }
    };
    if origin
        .as_deref()
        .is_some_and(|value| !security.allows_origin(value))
    {
        return security_error(
            StatusCode::FORBIDDEN,
            "origin_denied",
            "请求来源不在本地服务允许列表中",
            false,
        );
    }

    if request.method() == Method::OPTIONS {
        let Some(origin) = origin.as_deref() else {
            return security_error(
                StatusCode::FORBIDDEN,
                "origin_required",
                "浏览器预检请求必须携带 Origin",
                false,
            );
        };
        if !valid_preflight(headers) {
            return security_error(
                StatusCode::FORBIDDEN,
                "preflight_denied",
                "预检请求包含未允许的方法或请求头",
                false,
            );
        }
        return cors_preflight_response(origin);
    }

    let websocket_ticket_handshake = is_ws_ticket_handshake(&request);
    // 除 WebSocket 短票据握手外，桌面本地 API 始终要求本次进程生成的 Bearer。
    if !websocket_ticket_handshake && !has_valid_bearer(headers, &security.access_token) {
        let mut response = security_error(
            StatusCode::UNAUTHORIZED,
            "invalid_token",
            "缺少或提供了无效的 Bearer 令牌",
            true,
        );
        if let Some(origin) = origin.as_deref() {
            apply_actual_cors_headers(response.headers_mut(), origin);
        }
        return response;
    }

    let mut response = next.run(request).await;
    if let Some(origin) = origin.as_deref() {
        apply_actual_cors_headers(response.headers_mut(), origin);
    }
    response
}

fn has_expected_host(headers: &HeaderMap, expected_host: &str) -> bool {
    let mut values = headers.get_all(header::HOST).iter();
    values
        .next()
        .and_then(|value| value.to_str().ok())
        .is_some_and(|host| host.eq_ignore_ascii_case(expected_host))
        && values.next().is_none()
}

fn read_origin(headers: &HeaderMap) -> Result<Option<String>, &'static str> {
    let mut values = headers.get_all(header::ORIGIN).iter();
    let Some(value) = values.next() else {
        return Ok(None);
    };
    if values.next().is_some() {
        return Err("请求包含多个 Origin");
    }
    let origin = value.to_str().map_err(|_| "Origin 请求头不是有效文本")?;
    if origin == "null" || origin.is_empty() {
        return Err("不接受不透明或空 Origin");
    }
    Ok(Some(origin.to_string()))
}

fn valid_preflight(headers: &HeaderMap) -> bool {
    let method_allowed = headers
        .get(header::ACCESS_CONTROL_REQUEST_METHOD)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            matches!(
                value,
                "GET" | "POST" | "PUT" | "PATCH" | "DELETE" | "OPTIONS"
            )
        });
    if !method_allowed {
        return false;
    }

    headers
        .get(header::ACCESS_CONTROL_REQUEST_HEADERS)
        .and_then(|value| value.to_str().ok())
        .map(|value| {
            value.split(',').all(|name| {
                matches!(
                    name.trim().to_ascii_lowercase().as_str(),
                    "authorization" | "content-type" | "accept"
                )
            })
        })
        .unwrap_or(true)
}

fn has_valid_bearer(headers: &HeaderMap, expected: &str) -> bool {
    let mut values = headers.get_all(header::AUTHORIZATION).iter();
    let Some(value) = values.next().and_then(|value| value.to_str().ok()) else {
        return false;
    };
    if values.next().is_some() {
        return false;
    }
    let mut parts = value.split_whitespace();
    let Some(scheme) = parts.next() else {
        return false;
    };
    let Some(token) = parts.next() else {
        return false;
    };
    scheme.eq_ignore_ascii_case("Bearer")
        && parts.next().is_none()
        && constant_time_eq(token.as_bytes(), expected.as_bytes())
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0_u8, |difference, (left, right)| {
            difference | (left ^ right)
        })
        == 0
}

fn is_ws_ticket_handshake(request: &Request) -> bool {
    let path = request.uri().path();
    let is_ws_path = path == "/ws" || path == "/api/ws";
    is_ws_path
        && request
            .uri()
            .query()
            .is_some_and(|query| query.split('&').any(|pair| pair.starts_with("ticket=")))
}

fn cors_preflight_response(origin: &str) -> Response {
    let mut response = StatusCode::NO_CONTENT.into_response();
    let headers = response.headers_mut();
    insert_header(headers, header::ACCESS_CONTROL_ALLOW_ORIGIN, origin);
    insert_header(
        headers,
        header::ACCESS_CONTROL_ALLOW_METHODS,
        ALLOWED_METHODS,
    );
    insert_header(
        headers,
        header::ACCESS_CONTROL_ALLOW_HEADERS,
        ALLOWED_HEADERS,
    );
    insert_header(headers, header::ACCESS_CONTROL_MAX_AGE, "600");
    insert_header(headers, header::VARY, "Origin");
    response
}

fn apply_actual_cors_headers(headers: &mut HeaderMap, origin: &str) {
    insert_header(headers, header::ACCESS_CONTROL_ALLOW_ORIGIN, origin);
    insert_header(headers, header::VARY, "Origin");
}

fn insert_header(headers: &mut HeaderMap, name: header::HeaderName, value: &str) {
    if let Ok(value) = HeaderValue::from_str(value) {
        headers.insert(name, value);
    }
}

fn security_error(
    status: StatusCode,
    code: &'static str,
    message: &'static str,
    authenticate: bool,
) -> Response {
    let request_id = crate::dto::next_api_request_id();
    let mut response = (
        status,
        Json(serde_json::json!({
            "error": message,
            "code": code,
            "message": message,
            "field_errors": {},
            "retryable": false,
            "request_id": request_id,
        })),
    )
        .into_response();
    if authenticate {
        response.headers_mut().insert(
            header::WWW_AUTHENTICATE,
            HeaderValue::from_static("Bearer realm=\"muse-local-api\""),
        );
    }
    response
}

fn random_urlsafe(size: usize) -> Result<String, LocalApiSecurityError> {
    let mut bytes = vec![0_u8; size];
    getrandom::fill(&mut bytes).map_err(|err| LocalApiSecurityError::Random(err.to_string()))?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

fn validate_loopback_host(host: &str) -> Result<(), LocalApiSecurityError> {
    let valid = host
        .rsplit_once(':')
        .is_some_and(|(name, port)| name == "127.0.0.1" && port.parse::<u16>().is_ok());
    if valid {
        Ok(())
    } else {
        Err(LocalApiSecurityError::InvalidConfiguration(format!(
            "本地 API 只能绑定形如 127.0.0.1:<port> 的地址，当前值为 `{host}`"
        )))
    }
}

fn validate_origin(origin: &str) -> Result<(), LocalApiSecurityError> {
    let valid_scheme = origin.starts_with("http://")
        || origin.starts_with("https://")
        || origin.starts_with("tauri://");
    let no_path = origin
        .split_once("://")
        .is_some_and(|(_, authority)| !authority.is_empty() && !authority.contains('/'));
    if valid_scheme && no_path && origin != "null" {
        Ok(())
    } else {
        Err(LocalApiSecurityError::InvalidConfiguration(format!(
            "无效的 API Origin：`{origin}`"
        )))
    }
}

fn validate_local_ui_origin(
    origin: &str,
    allow_loopback_dev_origins: bool,
) -> Result<(), LocalApiSecurityError> {
    validate_origin(origin)?;
    if matches!(
        origin,
        "tauri://localhost" | "http://tauri.localhost" | "https://tauri.localhost"
    ) || (allow_loopback_dev_origins && is_explicit_loopback_dev_origin(origin))
    {
        Ok(())
    } else {
        Err(LocalApiSecurityError::InvalidConfiguration(format!(
            "API Origin 必须是显式受信任的 Tauri 来源或开发回环来源，当前值为 `{origin}`"
        )))
    }
}

fn is_explicit_loopback_dev_origin(origin: &str) -> bool {
    origin
        .strip_prefix("http://127.0.0.1:")
        .is_some_and(|port| port.parse::<u16>().is_ok())
}

/// 本地 API 安全配置错误。
#[derive(Debug)]
pub enum LocalApiSecurityError {
    InvalidConfiguration(String),
    Random(String),
    TicketCapacityExceeded,
}

impl std::fmt::Display for LocalApiSecurityError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidConfiguration(message) => formatter.write_str(message),
            Self::Random(message) => write!(formatter, "生成本地安全随机值失败：{message}"),
            Self::TicketCapacityExceeded => formatter.write_str("待消费的 WebSocket 票据过多"),
        }
    }
}

impl std::error::Error for LocalApiSecurityError {}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Router;
    use axum::body::Body;
    use axum::http::Request;
    use axum::routing::get;
    use tower::ServiceExt;

    fn test_security() -> Arc<LocalApiSecurity> {
        LocalApiSecurity::new(LocalApiSecurityOptions {
            expected_host: "127.0.0.1:43127".to_string(),
            allowed_origins: vec!["tauri://localhost".to_string()],
            allow_loopback_dev_origins: false,
            access_token: Some("test-token-with-at-least-thirty-two-bytes".to_string()),
        })
        .expect("测试安全上下文应能创建")
    }

    fn protected_app(security: Arc<LocalApiSecurity>) -> Router {
        Router::new()
            .route("/api/probe", get(|| async { "ok" }))
            .layer(axum::middleware::from_fn_with_state(
                security,
                enforce_local_api_security,
            ))
    }

    #[tokio::test]
    async fn rejects_invalid_host_before_token_check() {
        let response = protected_app(test_security())
            .oneshot(
                Request::builder()
                    .uri("/api/probe")
                    .header(header::HOST, "127.0.0.1:9999")
                    .body(Body::empty())
                    .expect("请求应能构造"),
            )
            .await
            .expect("请求应能完成");

        assert_eq!(response.status(), StatusCode::MISDIRECTED_REQUEST);
    }

    #[tokio::test]
    async fn rejects_unknown_origin_before_token_check() {
        let response = protected_app(test_security())
            .oneshot(
                Request::builder()
                    .uri("/api/probe")
                    .header(header::HOST, "127.0.0.1:43127")
                    .header(header::ORIGIN, "https://attacker.example")
                    .body(Body::empty())
                    .expect("请求应能构造"),
            )
            .await
            .expect("请求应能完成");

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn rejects_api_self_origin_even_with_valid_bearer() {
        let response = protected_app(test_security())
            .oneshot(
                Request::builder()
                    .uri("/api/probe")
                    .header(header::HOST, "127.0.0.1:43127")
                    .header(header::ORIGIN, "http://127.0.0.1:43127")
                    .header(
                        header::AUTHORIZATION,
                        "Bearer test-token-with-at-least-thirty-two-bytes",
                    )
                    .body(Body::empty())
                    .expect("请求应能构造"),
            )
            .await
            .expect("请求应能完成");

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn rejects_origin_that_only_matches_after_trailing_slash_normalization() {
        let response = protected_app(test_security())
            .oneshot(
                Request::builder()
                    .uri("/api/probe")
                    .header(header::HOST, "127.0.0.1:43127")
                    .header(header::ORIGIN, "tauri://localhost/")
                    .header(
                        header::AUTHORIZATION,
                        "Bearer test-token-with-at-least-thirty-two-bytes",
                    )
                    .body(Body::empty())
                    .expect("请求应能构造"),
            )
            .await
            .expect("请求应能完成");

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn requires_and_accepts_exact_bearer_token() {
        let app = protected_app(test_security());
        let missing = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/probe")
                    .header(header::HOST, "127.0.0.1:43127")
                    .body(Body::empty())
                    .expect("请求应能构造"),
            )
            .await
            .expect("请求应能完成");
        assert_eq!(missing.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            missing.headers().get(header::WWW_AUTHENTICATE),
            Some(&HeaderValue::from_static("Bearer realm=\"muse-local-api\""))
        );

        let accepted = app
            .oneshot(
                Request::builder()
                    .uri("/api/probe")
                    .header(header::HOST, "127.0.0.1:43127")
                    .header(
                        header::AUTHORIZATION,
                        "Bearer test-token-with-at-least-thirty-two-bytes",
                    )
                    .body(Body::empty())
                    .expect("请求应能构造"),
            )
            .await
            .expect("请求应能完成");
        assert_eq!(accepted.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn bearer_error_keeps_exact_cors_origin_visible() {
        let response = protected_app(test_security())
            .oneshot(
                Request::builder()
                    .uri("/api/probe")
                    .header(header::HOST, "127.0.0.1:43127")
                    .header(header::ORIGIN, "tauri://localhost")
                    .body(Body::empty())
                    .expect("请求应能构造"),
            )
            .await
            .expect("请求应能完成");

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            response.headers().get(header::ACCESS_CONTROL_ALLOW_ORIGIN),
            Some(&HeaderValue::from_static("tauri://localhost"))
        );
    }

    #[tokio::test]
    async fn allows_strict_browser_preflight_without_bearer() {
        let response = protected_app(test_security())
            .oneshot(
                Request::builder()
                    .method(Method::OPTIONS)
                    .uri("/api/probe")
                    .header(header::HOST, "127.0.0.1:43127")
                    .header(header::ORIGIN, "tauri://localhost")
                    .header(header::ACCESS_CONTROL_REQUEST_METHOD, "PATCH")
                    .header(
                        header::ACCESS_CONTROL_REQUEST_HEADERS,
                        "authorization, content-type",
                    )
                    .body(Body::empty())
                    .expect("请求应能构造"),
            )
            .await
            .expect("请求应能完成");

        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert_eq!(
            response.headers().get(header::ACCESS_CONTROL_ALLOW_ORIGIN),
            Some(&HeaderValue::from_static("tauri://localhost"))
        );
    }

    #[tokio::test]
    async fn websocket_ticket_is_single_use() {
        let security = test_security();
        let ticket = security
            .issue_ws_ticket()
            .await
            .expect("应能签发票据")
            .ticket;

        assert!(security.consume_ws_ticket(&ticket).await);
        assert!(!security.consume_ws_ticket(&ticket).await);
    }

    #[test]
    fn rejects_non_local_allowed_origin() {
        let result = LocalApiSecurity::new(LocalApiSecurityOptions {
            expected_host: "127.0.0.1:43127".to_string(),
            allowed_origins: vec!["https://attacker.example".to_string()],
            allow_loopback_dev_origins: true,
            access_token: Some("test-token-with-at-least-thirty-two-bytes".to_string()),
        });

        assert!(result.is_err());
    }

    #[test]
    fn rejects_loopback_browser_origin_configuration() {
        let result = LocalApiSecurity::new(LocalApiSecurityOptions {
            expected_host: "127.0.0.1:43127".to_string(),
            allowed_origins: vec!["http://127.0.0.1:5173".to_string()],
            allow_loopback_dev_origins: false,
            access_token: Some("test-token-with-at-least-thirty-two-bytes".to_string()),
        });

        assert!(result.is_err());
    }

    #[test]
    fn accepts_exact_loopback_origin_only_with_explicit_dev_opt_in() {
        let result = LocalApiSecurity::new(LocalApiSecurityOptions {
            expected_host: "127.0.0.1:43127".to_string(),
            allowed_origins: vec!["http://127.0.0.1:5173".to_string()],
            allow_loopback_dev_origins: true,
            access_token: Some("test-token-with-at-least-thirty-two-bytes".to_string()),
        });

        assert!(result.is_ok());
        assert!(!is_explicit_loopback_dev_origin("http://localhost:5173"));
        assert!(!is_explicit_loopback_dev_origin("https://127.0.0.1:5173"));
        assert!(!is_explicit_loopback_dev_origin(
            "http://127.0.0.1:5173/path"
        ));
    }
}

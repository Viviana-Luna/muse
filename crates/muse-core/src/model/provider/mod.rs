//! 聊天模型 provider 协议适配层，统一封装上游模型 API 的消息、流式事件和工具调用。

pub mod factory;
pub mod openai;

use crate::domain::conversation::{Conversation, Message};
use crate::domain::tool::{ToolCall, ToolDef};
use crate::domain::usage::ProviderTokenUsage;
use async_trait::async_trait;
use futures::stream::Stream;
use std::pin::Pin;

/// 聊天模型 provider 共用结果类型。
pub type ChatModelResult = Result<String, ChatModelError>;

/// 聊天模型流式结果：返回模型事件分片的异步流。
pub type ChatStreamResult =
    Pin<Box<dyn Stream<Item = Result<ChatStreamEvent, ChatModelError>> + Send>>;

/// 聊天模型 provider 输出的单个流式事件。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "type", content = "data")]
pub enum ChatStreamEvent {
    /// 文本增量。
    #[serde(rename = "text")]
    Text(String),
    /// 模型显式返回的推理/思考增量。
    ///
    /// 该事件只承载模型提供器明确暴露的推理字段，不用运行底座状态伪造。
    #[serde(rename = "reasoning")]
    Reasoning(String),
    /// 情绪标签。
    /// 当前主链路主要通过前缀协议解析情绪，这个事件类型先保留给后续提供器直出能力。
    #[allow(dead_code)]
    #[serde(rename = "emotion")]
    Emotion(String),
    /// 模型提供器原生返回的结构化工具调用。
    ///
    /// 这是对齐 Codex 的主链路：工具调用不应该混在助手文本里，
    /// 而应该作为独立事件交给运行底座执行。
    #[serde(rename = "tool_call")]
    ToolCall(ToolCall),
    /// 模型提供器返回的 Token 用量。
    ///
    /// 流式场景里该事件通常是累计值，调用方需要按累计语义合并。
    #[serde(rename = "usage")]
    Usage(ProviderTokenUsage),
    /// 流结束。
    #[serde(rename = "done")]
    Done,
}

/// 聊天模型交互过程中可能出现的错误。
#[derive(Debug)]
pub enum ChatModelError {
    ApiError(String),
    NetworkError(String),
    ConfigError(String),
    UpstreamHttp {
        status: u16,
        code: &'static str,
        message: &'static str,
        retryable: bool,
    },
    UpstreamEvent {
        code: &'static str,
        message: &'static str,
        retryable: bool,
        /// 供应商公开的事件分类，仅保留短 ASCII 标识，禁止写入原始正文。
        detail: Option<String>,
    },
}

impl ChatModelError {
    /// 将上游 HTTP 失败映射为不包含响应正文的稳定错误。
    pub fn from_upstream_status(status: reqwest::StatusCode) -> Self {
        let (code, message, retryable) = match status.as_u16() {
            400 | 422 => (
                "upstream_invalid_request",
                "上游模型服务拒绝了请求，请检查模型与参数配置。",
                false,
            ),
            401 | 403 => (
                "upstream_auth_failed",
                "上游模型服务鉴权失败，请检查 API 密钥与访问权限。",
                false,
            ),
            404 => (
                "upstream_not_found",
                "上游模型或接口不存在，请检查模型名与 API 地址。",
                false,
            ),
            408 | 504 => (
                "upstream_timeout",
                "上游模型服务响应超时，请稍后重试。",
                true,
            ),
            409 => (
                "upstream_conflict",
                "上游模型服务暂时无法处理该请求，请稍后重试。",
                true,
            ),
            413 => (
                "upstream_context_too_large",
                "发送给上游模型的上下文超过限制，请缩短会话后重试。",
                false,
            ),
            429 => (
                "upstream_rate_limited",
                "上游模型服务请求过于频繁，请稍后重试。",
                true,
            ),
            500..=599 => (
                "upstream_unavailable",
                "上游模型服务暂时不可用，请稍后重试。",
                true,
            ),
            _ => ("upstream_http_error", "上游模型服务返回异常状态。", false),
        };
        Self::UpstreamHttp {
            status: status.as_u16(),
            code,
            message,
            retryable,
        }
    }

    /// 将传输错误归一化，避免把带 URL 的底层错误直接暴露给会话或前端。
    pub fn from_network_error(error: reqwest::Error) -> Self {
        let message = if error.is_timeout() {
            "请求上游模型服务超时，请稍后重试。"
        } else if error.is_connect() {
            "无法连接上游模型服务，请检查网络和 API 地址。"
        } else if error.is_request() {
            "无法发送上游模型请求，请检查模型服务配置。"
        } else {
            "上游模型连接异常，请稍后重试。"
        };
        Self::NetworkError(message.to_string())
    }

    /// 将 HTTP 200 内的供应商错误对象映射为稳定错误。
    ///
    /// 这里只读取供应商定义的 `type`/`code` 分类字段，绝不把 `message`、
    /// 原始 JSON、响应正文或 URL 带入错误，避免上游回显的密钥和用户内容泄漏。
    pub(crate) fn from_upstream_error_payload(payload: &serde_json::Value) -> Self {
        let error = payload.get("error").unwrap_or(payload);
        let error_type = error
            .get("type")
            .and_then(serde_json::Value::as_str)
            .or_else(|| payload.get("type").and_then(serde_json::Value::as_str));
        let error_code = error.get("code").and_then(serde_json::Value::as_str);
        Self::from_upstream_error_fields(error_type, error_code)
    }

    /// 检查成功 HTTP 响应或 SSE 事件是否实际承载供应商错误。
    pub(crate) fn from_payload_if_error(payload: &serde_json::Value) -> Option<Self> {
        let event_type = payload.get("type").and_then(serde_json::Value::as_str);
        let status = payload.get("status").and_then(serde_json::Value::as_str);
        let has_error = payload.get("error").is_some_and(|error| !error.is_null());
        let failed_event = event_type.is_some_and(|value| {
            let normalized = value.trim().to_ascii_lowercase();
            normalized == "error"
                || normalized.ends_with(".error")
                || normalized.ends_with(".failed")
                || normalized.contains("failed")
        });
        if has_error || failed_event || status == Some("failed") {
            Some(Self::from_upstream_error_payload(payload))
        } else {
            None
        }
    }

    fn safe_upstream_label(value: Option<&str>) -> Option<String> {
        let value = value?.trim();
        if value.is_empty()
            || value.len() > 80
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        {
            return None;
        }
        Some(value.to_string())
    }

    fn from_upstream_error_fields(error_type: Option<&str>, error_code: Option<&str>) -> Self {
        let fields = [error_type, error_code]
            .into_iter()
            .flatten()
            .map(|value| value.trim().to_ascii_lowercase())
            .collect::<Vec<_>>();
        let detail = [
            Self::safe_upstream_label(error_type).map(|value| format!("type={value}")),
            Self::safe_upstream_label(error_code).map(|value| format!("code={value}")),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
        let has = |candidates: &[&str]| {
            fields
                .iter()
                .any(|field| candidates.iter().any(|candidate| field == candidate))
        };

        let (code, message, retryable) = if has(&[
            "authentication_error",
            "invalid_api_key",
            "permission_error",
            "permission_denied",
        ]) {
            (
                "upstream_auth_failed",
                "上游模型服务鉴权失败，请检查 API 密钥与访问权限。",
                false,
            )
        } else if has(&[
            "rate_limit_error",
            "rate_limit_exceeded",
            "too_many_requests",
        ]) {
            (
                "upstream_rate_limited",
                "上游模型服务请求过于频繁，请稍后重试。",
                true,
            )
        } else if has(&[
            "context_length_exceeded",
            "context_window_exceeded",
            "prompt_too_long",
        ]) {
            (
                "upstream_context_too_large",
                "发送给上游模型的上下文超过限制，请缩短会话后重试。",
                false,
            )
        } else if has(&[
            "invalid_request_error",
            "invalid_request",
            "bad_request",
            "unprocessable_entity",
        ]) {
            (
                "upstream_invalid_request",
                "上游模型服务拒绝了请求，请检查模型与参数配置。",
                false,
            )
        } else if has(&["not_found_error", "not_found", "model_not_found"]) {
            (
                "upstream_not_found",
                "上游模型或接口不存在，请检查模型名与 API 地址。",
                false,
            )
        } else if has(&[
            "timeout_error",
            "request_timeout",
            "gateway_timeout",
            "timeout",
        ]) {
            (
                "upstream_timeout",
                "上游模型服务响应超时，请稍后重试。",
                true,
            )
        } else if has(&[
            "overloaded_error",
            "server_error",
            "service_unavailable",
            "api_error",
            "internal_error",
        ]) {
            (
                "upstream_unavailable",
                "上游模型服务暂时不可用，请稍后重试。",
                true,
            )
        } else if has(&["insufficient_quota", "billing_error"]) {
            (
                "upstream_quota_exhausted",
                "上游模型服务额度不足，请检查账户余额与配额。",
                false,
            )
        } else {
            (
                "upstream_event_error",
                "上游模型服务返回无法分类的失败事件，请重新验证当前供应商、模型和密钥。",
                false,
            )
        };
        Self::UpstreamEvent {
            code,
            message,
            retryable,
            // 已知分类已经有稳定的人类可读文案；只有未知失败事件补充供应商分类，
            // 这样既能定位 Agent Plan 的协议事件，又不会把重复信息写入日志。
            detail: (code == "upstream_event_error" && !detail.is_empty())
                .then(|| detail.join("，")),
        }
    }

    /// 构造内部协议适配失败，消息必须是维护者提供的固定文本。
    pub(crate) fn protocol_error(message: &'static str) -> Self {
        Self::ApiError(message.to_string())
    }

    /// 返回供适配层映射 `ApiError` 的稳定错误码。
    pub fn code(&self) -> &'static str {
        match self {
            Self::ApiError(_) => "provider_protocol_error",
            Self::NetworkError(_) => "provider_network_error",
            Self::ConfigError(_) => "provider_config_error",
            Self::UpstreamHttp { code, .. } => code,
            Self::UpstreamEvent { code, .. } => code,
        }
    }

    /// 当前错误在不修改配置的情况下是否值得重试。
    pub fn retryable(&self) -> bool {
        match self {
            Self::NetworkError(_) => true,
            Self::UpstreamHttp { retryable, .. } => *retryable,
            Self::UpstreamEvent { retryable, .. } => *retryable,
            Self::ApiError(_) | Self::ConfigError(_) => false,
        }
    }
}

impl std::fmt::Display for ChatModelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ChatModelError::ApiError(msg) => write!(f, "接口错误：{msg}"),
            ChatModelError::NetworkError(msg) => write!(f, "网络错误：{msg}"),
            ChatModelError::ConfigError(msg) => write!(f, "配置错误：{msg}"),
            ChatModelError::UpstreamHttp {
                status,
                code,
                message,
                ..
            } => write!(f, "接口错误（{code}，HTTP {status}）：{message}"),
            ChatModelError::UpstreamEvent {
                code,
                message,
                detail,
                ..
            } => {
                write!(f, "接口错误（{code}）：{message}")?;
                if let Some(detail) = detail {
                    write!(f, "（上游分类：{detail}）")?;
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for ChatModelError {}

/// 聊天模型 provider 抽象接口。
///
/// 每个后端（OpenAI、Anthropic、OpenAI 兼容网关等）都实现该接口。
#[async_trait]
pub trait ChatModelProvider: Send + Sync {
    /// 发送会话历史并获取文本回复。
    async fn chat(&self, conversation: &Conversation) -> ChatModelResult;

    /// 流式发送对话历史并获取服务器推送事件流。
    async fn chat_stream(&self, conversation: &Conversation) -> ChatStreamResult;

    /// 带结构化工具定义的流式对话。
    ///
    /// 默认回退到普通流式能力，具体提供器可覆盖并把工具定义转换为
    /// 上游 OpenAI/Anthropic 等协议的原生工具调用参数。
    async fn chat_stream_with_tools(
        &self,
        conversation: &Conversation,
        _tools: &[ToolDef],
    ) -> ChatStreamResult {
        self.chat_stream(conversation).await
    }

    /// 返回提供器名称，用于诊断。
    fn name(&self) -> &'static str;
}

/// 使用 OpenAI 风格结构的 provider 所需消息格式。
#[derive(Debug, Clone, serde::Serialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

impl From<&Message> for ChatMessage {
    fn from(m: &Message) -> Self {
        ChatMessage {
            role: m.role.to_string(),
            content: m.content.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ChatModelError;

    #[test]
    fn upstream_error_mapping_never_contains_response_body() {
        let error = ChatModelError::from_upstream_status(reqwest::StatusCode::UNAUTHORIZED);
        assert_eq!(error.code(), "upstream_auth_failed");
        assert!(!error.retryable());
        assert_eq!(
            error.to_string(),
            "接口错误（upstream_auth_failed，HTTP 401）：上游模型服务鉴权失败，请检查 API 密钥与访问权限。"
        );
    }

    #[test]
    fn retryable_upstream_statuses_are_explicit() {
        for status in [
            reqwest::StatusCode::REQUEST_TIMEOUT,
            reqwest::StatusCode::TOO_MANY_REQUESTS,
            reqwest::StatusCode::BAD_GATEWAY,
        ] {
            assert!(ChatModelError::from_upstream_status(status).retryable());
        }
        assert!(
            !ChatModelError::from_upstream_status(reqwest::StatusCode::PAYLOAD_TOO_LARGE)
                .retryable()
        );
    }

    #[test]
    fn provider_event_mapping_discards_message_body_and_url() {
        let payload = serde_json::json!({
            "error": {
                "type": "overloaded_error",
                "message": "secret=sk-live https://private.example/conversations/42"
            }
        });

        let error = ChatModelError::from_upstream_error_payload(&payload);

        assert_eq!(error.code(), "upstream_unavailable");
        assert!(error.retryable());
        assert_eq!(
            error.to_string(),
            "接口错误（upstream_unavailable）：上游模型服务暂时不可用，请稍后重试。"
        );
        assert!(!error.to_string().contains("sk-live"));
        assert!(!error.to_string().contains("private.example"));
    }

    #[test]
    fn nullable_error_field_does_not_turn_a_success_into_failure() {
        let payload = serde_json::json!({
            "status": "completed",
            "error": null,
        });

        assert!(ChatModelError::from_payload_if_error(&payload).is_none());
    }

    #[test]
    fn failed_provider_event_exposes_only_safe_event_classification() {
        let payload = serde_json::json!({
            "type": "response.failed",
            "error": {
                "code": "model_serving_failed",
                "message": "secret=sk-live and user content must not leak"
            }
        });

        let error = ChatModelError::from_payload_if_error(&payload).expect("应识别失败事件");
        assert_eq!(error.code(), "upstream_event_error");
        assert!(
            error
                .to_string()
                .contains("上游分类：type=response.failed，code=model_serving_failed")
        );
        assert!(!error.to_string().contains("sk-live"));
        assert!(!error.to_string().contains("user content"));
    }

    #[test]
    fn unsafe_provider_event_labels_are_discarded() {
        let payload = serde_json::json!({
            "type": "response.failed",
            "error": { "code": "bad code/with spaces" }
        });

        let error = ChatModelError::from_payload_if_error(&payload).expect("应识别失败事件");
        assert!(!error.to_string().contains("bad code"));
    }
}

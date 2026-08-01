//! 多个 API 域共享的轻量 DTO。

use serde::ser::{SerializeStruct, Serializer};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};

static API_REQUEST_ID: AtomicU64 = AtomicU64::new(1);

pub(crate) fn next_api_request_id() -> String {
    format!(
        "req-{}-{}",
        std::process::id(),
        API_REQUEST_ID.fetch_add(1, Ordering::Relaxed)
    )
}

/// 使用 revision 的删除请求。
#[derive(Deserialize)]
pub struct RevisionQuery {
    pub revision: String,
}

/// 通用状态响应体。
#[derive(Debug, Serialize)]
pub struct StatusResponse {
    pub status: String,
}

/// 通用错误响应体。
#[derive(Debug)]
pub struct ErrorResponse {
    pub error: String,
}

impl Serialize for ErrorResponse {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let code = infer_api_error_code(&self.error);
        let message = public_api_error_message(&self.error);
        let retryable = self.error.contains("超时")
            || self.error.contains("暂时")
            || self.error.contains("连接失败")
            || self.error.starts_with("provider_rate_limited")
            || self.error.starts_with("provider_timeout")
            || self.error.starts_with("provider_unreachable");
        let busy = busy_error_details(&self.error);
        let mut state =
            serializer.serialize_struct("ApiError", if busy.is_some() { 8 } else { 6 })?;
        // 兼容首个协议过渡期的旧客户端；新客户端统一读取 message/code。
        state.serialize_field("error", message)?;
        state.serialize_field("code", code)?;
        state.serialize_field("message", message)?;
        state.serialize_field("field_errors", &BTreeMap::<String, String>::new())?;
        state.serialize_field("retryable", &retryable)?;
        state.serialize_field("request_id", &next_api_request_id())?;
        if let Some((turn_id, phase)) = busy {
            state.serialize_field("turn_id", &turn_id)?;
            state.serialize_field("phase", phase)?;
        }
        state.end()
    }
}

fn busy_error_details(message: &str) -> Option<(String, &'static str)> {
    if !message.starts_with("runtime_busy") {
        return None;
    }
    let marker = "当前回合 `";
    let turn_start = message.find(marker)?.saturating_add(marker.len());
    let turn_end = message[turn_start..].find('`')?.saturating_add(turn_start);
    let phase = [
        ("Preparing", "preparing"),
        ("Running", "running"),
        ("WaitingApproval", "waiting_approval"),
        ("WaitingUser", "waiting_user"),
        ("Cancelling", "cancelling"),
        ("Finalizing", "finalizing"),
    ]
    .into_iter()
    .find_map(|(raw, normalized)| message.contains(raw).then_some(normalized))?;
    Some((message[turn_start..turn_end].to_string(), phase))
}

fn public_api_error_message(message: &str) -> &str {
    // 记忆管理错误的稳定码前缀一律不暴露给客户端 message。
    for code in MEMORY_STABLE_ERROR_CODES {
        if let Some(rest) = message.strip_prefix(code)
            && let Some(public) = rest.strip_prefix("：")
        {
            return public;
        }
    }
    for prefix in [
        "persona_required：",
        "provider_api_key_required：",
        "provider_auth_failed：",
        "provider_rate_limited：",
        "provider_not_found：",
        "provider_invalid_request：",
        "provider_timeout：",
        "provider_protocol_error：",
        "provider_unreachable：",
        "skill_conflict：",
        "skill_revision_conflict：",
        "mcp_conflict：",
        "mcp_revision_conflict：",
        "mcp_config_invalid：",
        "mcp_revision_required：",
        "mcp_source_required：",
    ] {
        if let Some(public) = message.strip_prefix(prefix) {
            return public;
        }
    }
    message
}

/// 与 `MemoryErrorCode` 一一对应的 16 个记忆稳定错误码。
const MEMORY_STABLE_ERROR_CODES: [&str; 16] = [
    "memory_invalid_request",
    "memory_invalid_state_transition",
    "memory_not_found",
    "memory_revision_conflict",
    "memory_persona_scope_mismatch",
    "memory_source_ineligible",
    "memory_sensitive_content_rejected",
    "memory_sensitivity_unavailable",
    "memory_cursor_invalid",
    "memory_cursor_expired",
    "memory_query_rejected",
    "memory_query_budget_exceeded",
    "memory_delete_confirmation_required",
    "memory_deletion_authority_unavailable",
    "memory_deletion_incomplete",
    "memory_repository_unavailable",
];

fn infer_api_error_code(message: &str) -> &'static str {
    // 记忆稳定码必须在“不存在/必须”等通用回退之前匹配，避免被宽松子串吞掉。
    for code in MEMORY_STABLE_ERROR_CODES {
        if message.starts_with(code) {
            return code;
        }
    }
    if message.starts_with("persona_required") {
        "persona_required"
    } else if message.starts_with("provider_api_key_required") {
        "provider_api_key_required"
    } else if message.starts_with("provider_auth_failed") {
        "provider_auth_failed"
    } else if message.starts_with("provider_rate_limited") {
        "provider_rate_limited"
    } else if message.starts_with("provider_not_found") {
        "provider_not_found"
    } else if message.starts_with("provider_invalid_request") {
        "provider_invalid_request"
    } else if message.starts_with("provider_timeout") {
        "provider_timeout"
    } else if message.starts_with("provider_protocol_error") {
        "provider_protocol_error"
    } else if message.starts_with("provider_unreachable") {
        "provider_unreachable"
    } else if message.starts_with("runtime_snapshot_unstable") {
        "runtime_snapshot_unstable"
    } else if message.starts_with("runtime_busy") {
        "runtime_busy"
    } else if message.starts_with("skill_conflict") {
        "skill_conflict"
    } else if message.starts_with("skill_revision_conflict") {
        "skill_revision_conflict"
    } else if message.starts_with("mcp_conflict") {
        "mcp_conflict"
    } else if message.starts_with("mcp_revision_conflict") {
        "mcp_revision_conflict"
    } else if message.starts_with("mcp_config_invalid") {
        "mcp_config_invalid"
    } else if message.starts_with("mcp_revision_required") {
        "mcp_revision_required"
    } else if message.starts_with("mcp_source_required") {
        "mcp_source_required"
    } else if message.contains("流式聊天只接受 POST") {
        "stream_post_required"
    } else if message.contains("已以不同结果处理") {
        "decision_conflict"
    } else if message.contains("已受理") {
        "duplicate_request"
    } else if message.contains("conversation_id 已过期") {
        "stale_conversation"
    } else if message.contains("不再等待")
        || message.contains("过期决策")
        || message.contains("属于其他回合")
    {
        "stale_turn"
    } else if message.contains("不存在") || message.contains("未找到") {
        "not_found"
    } else if message.contains("不能为空") || message.contains("必须") || message.contains("不支持")
    {
        "validation_error"
    } else {
        "request_failed"
    }
}

#[cfg(test)]
mod api_error_tests {
    use super::ErrorResponse;

    #[test]
    fn memory_stable_codes_map_one_to_one_and_strip_prefix() {
        // 与 MemoryErrorCode 的 16 个变体一一对应；顺序无关，关键是全覆盖。
        for (code, message) in [
            ("memory_invalid_request", "记忆请求参数无效。"),
            ("memory_invalid_state_transition", "记忆状态转换无效。"),
            ("memory_not_found", "指定记忆不存在。"),
            (
                "memory_revision_conflict",
                "记忆 revision 已变化，请刷新后重试。",
            ),
            ("memory_persona_scope_mismatch", "记忆不属于当前 Persona。"),
            ("memory_source_ineligible", "当前来源不具备形成记忆的资格。"),
            (
                "memory_sensitive_content_rejected",
                "该内容不允许进入长期记忆。",
            ),
            (
                "memory_sensitivity_unavailable",
                "敏感判定不可用，已拒绝持久化。",
            ),
            ("memory_cursor_invalid", "记忆查询游标无效。"),
            ("memory_cursor_expired", "记忆查询游标已过期。"),
            ("memory_query_rejected", "记忆查询不满足检索要求。"),
            ("memory_query_budget_exceeded", "本轮记忆查询预算已用尽。"),
            (
                "memory_delete_confirmation_required",
                "删除记忆前需要专用用户确认。",
            ),
            (
                "memory_deletion_authority_unavailable",
                "记忆删除权威不可用。",
            ),
            ("memory_deletion_incomplete", "记忆删除未完整完成。"),
            (
                "memory_repository_unavailable",
                "记忆 Repository 当前不可用。",
            ),
        ] {
            let value = serde_json::to_value(ErrorResponse {
                error: format!("{code}：{message}"),
            })
            .expect("记忆错误应能序列化");
            assert_eq!(value["code"], code, "{code} 应映射为同名稳定码");
            assert_eq!(value["message"], message, "{code} 不应暴露内部码前缀");
        }
        assert_eq!(super::MEMORY_STABLE_ERROR_CODES.len(), 16);
    }

    #[test]
    fn memory_not_found_code_wins_over_generic_fallback() {
        // “不存在”通用回退不得吞掉记忆稳定码。
        let value = serde_json::to_value(ErrorResponse {
            error: "memory_not_found：指定记忆不存在。".to_string(),
        })
        .expect("记忆错误应能序列化");
        assert_eq!(value["code"], "memory_not_found");
    }

    #[test]
    fn runtime_busy_error_includes_turn_and_phase() {
        let value = serde_json::to_value(ErrorResponse {
            error: "runtime_busy：当前回合 `turn-42` 仍处于 WaitingApproval 阶段。".to_string(),
        })
        .expect("busy 错误应能序列化");

        assert_eq!(value["code"], "runtime_busy");
        assert_eq!(value["turn_id"], "turn-42");
        assert_eq!(value["phase"], "waiting_approval");
    }

    #[test]
    fn persona_required_error_exposes_stable_code_without_internal_marker() {
        let value = serde_json::to_value(ErrorResponse {
            error: "persona_required：当前没有激活角色，请先选择角色。".to_string(),
        })
        .expect("角色门禁错误应能序列化");

        assert_eq!(value["code"], "persona_required");
        assert_eq!(value["message"], "当前没有激活角色，请先选择角色。");
        assert_eq!(value["error"], value["message"]);
    }

    #[test]
    fn provider_key_error_exposes_stable_code_without_internal_marker() {
        let value = serde_json::to_value(ErrorResponse {
            error: "provider_api_key_required：请先配置 API Key。".to_string(),
        })
        .expect("Provider 密钥错误应能序列化");

        assert_eq!(value["code"], "provider_api_key_required");
        assert_eq!(value["message"], "请先配置 API Key。");
    }

    #[test]
    fn provider_failures_expose_stable_codes_and_retryability() {
        for (raw, code, retryable) in [
            (
                "provider_auth_failed：凭据无效。",
                "provider_auth_failed",
                false,
            ),
            (
                "provider_rate_limited：请求过多。",
                "provider_rate_limited",
                true,
            ),
            (
                "provider_not_found：模型不存在。",
                "provider_not_found",
                false,
            ),
            (
                "provider_invalid_request：模型或参数无效。",
                "provider_invalid_request",
                false,
            ),
            ("provider_timeout：请求超时。", "provider_timeout", true),
            (
                "provider_protocol_error：响应格式错误。",
                "provider_protocol_error",
                false,
            ),
        ] {
            let value = serde_json::to_value(ErrorResponse {
                error: raw.to_string(),
            })
            .expect("Provider 错误应能序列化");
            assert_eq!(value["code"], code);
            assert_eq!(value["retryable"], retryable);
        }
    }
}

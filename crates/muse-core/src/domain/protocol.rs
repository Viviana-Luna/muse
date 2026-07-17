//! 运行时协议模块，定义前端操作输入和后端事件输出的稳定结构。

use serde::{Deserialize, Serialize};

use crate::domain::usage::{RuntimeContextSnapshot, RuntimeTokenUsage};

/// 前端进入运行时的统一操作协议。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RuntimeOp {
    /// 用户发起一轮对话。
    UserTurn { message: String },
    /// 用户取消当前轮。
    CancelTurn { turn_id: String },
    /// 用户允许某个等待审批的工具继续执行。
    ApproveTool { approval_id: String },
    /// 用户拒绝某个等待审批的工具。
    RejectTool {
        approval_id: String,
        reason: Option<String>,
    },
    /// 恢复指定会话。
    ResumeSession { conversation_id: String },
    /// 从已有会话分叉出新会话。
    ForkSession {
        source_conversation_id: String,
        before_user_message_index: Option<usize>,
    },
}

/// 后端向前端输出的统一运行时事件。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RuntimeEvent {
    TurnStarted {
        turn_id: String,
        conversation_id: String,
        persona_id: Option<String>,
        model_provider: String,
        model_name: String,
        #[serde(default)]
        model_source: String,
        #[serde(default)]
        model_fallback: bool,
        #[serde(default)]
        model_fallback_reason: Option<String>,
        voice_enabled: bool,
        active_voice_id: Option<String>,
        #[serde(default)]
        voice_source: String,
        #[serde(default)]
        voice_fallback: bool,
        #[serde(default)]
        voice_fallback_reason: Option<String>,
        runtime_mode: String,
        focus_phase: String,
        tool_preset: String,
    },
    Status {
        phase: String,
        message: String,
        detail: Option<String>,
        state: String,
    },
    ReasoningDelta {
        content: String,
    },
    AssistantSegmentStarted,
    AssistantDelta {
        content: String,
    },
    AssistantMessage {
        content: String,
    },
    Emotion {
        emotion: String,
    },
    ToolCall {
        call_id: String,
        name: String,
        arguments: serde_json::Value,
        risk: String,
        requires_approval: bool,
        interrupt_behavior: String,
    },
    ToolResult {
        call_id: String,
        name: String,
        success: bool,
        content: String,
        structured: Option<serde_json::Value>,
    },
    ToolOutputDelta {
        call_id: String,
        name: String,
        stream: String,
        content: String,
    },
    TokenUsage {
        usage: RuntimeTokenUsage,
    },
    ContextSnapshot {
        snapshot: RuntimeContextSnapshot,
    },
    SpeechStarted {
        call_id: String,
        text: String,
        voice_id: Option<String>,
    },
    SpeechFinished {
        call_id: String,
        success: bool,
        message: String,
    },
    ApprovalPending {
        approval_id: String,
        call_id: String,
        name: String,
        risk: String,
        message: String,
        detail: Option<String>,
        arguments: serde_json::Value,
    },
    ApprovalResolved {
        approval_id: String,
        approved: bool,
        reason: Option<String>,
    },
    UserQuestionPending {
        request_id: String,
        call_id: String,
        name: String,
        message: String,
        questions: serde_json::Value,
        arguments: serde_json::Value,
    },
    UserQuestionResolved {
        request_id: String,
        answered: bool,
        reason: Option<String>,
    },
    Error {
        message: String,
    },
    Done,
}

#[cfg(test)]
mod tests {
    use super::RuntimeEvent;

    #[test]
    fn legacy_turn_started_defaults_new_runtime_audit_fields() {
        let event: RuntimeEvent = serde_json::from_value(serde_json::json!({
            "type": "turn_started",
            "turn_id": "turn-legacy",
            "conversation_id": "conversation-legacy",
            "persona_id": "persona-legacy",
            "model_provider": "deepseek",
            "model_name": "deepseek-v4-pro",
            "voice_enabled": false,
            "active_voice_id": null,
            "runtime_mode": "daily",
            "focus_phase": "plan",
            "tool_preset": "daily"
        }))
        .expect("旧 turn_started 应继续可读");

        match event {
            RuntimeEvent::TurnStarted {
                model_source,
                model_fallback,
                voice_source,
                voice_fallback,
                ..
            } => {
                assert!(model_source.is_empty());
                assert!(!model_fallback);
                assert!(voice_source.is_empty());
                assert!(!voice_fallback);
            }
            _ => panic!("应解析为 turn_started"),
        }
    }
}

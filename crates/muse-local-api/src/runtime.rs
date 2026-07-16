//! 把核心运行时事件转换为 Web 层可推送的 SSE 事件。

use axum::response::sse::Event;
use muse_core::domain::protocol::RuntimeEvent;
use tokio::sync::mpsc;

/// 网页服务器推送运行时事件发送通道。
pub type RuntimeSseSender = mpsc::Sender<Result<Event, std::convert::Infallible>>;

/// 单轮运行输出。
#[derive(Debug, Clone)]
pub struct RuntimeTurnOutcome {
    pub reply: String,
}

/// 运行时事件发射器。
///
/// 这个发射器只由 HTTP 处理器持有，不直接拼接运行事件 JSON。
#[derive(Clone)]
pub struct RuntimeEventEmitter {
    tx: Option<RuntimeSseSender>,
}

impl RuntimeEventEmitter {
    /// 创建会向前端推送事件的发射器。
    pub fn stream(tx: RuntimeSseSender) -> Self {
        Self { tx: Some(tx) }
    }

    /// 创建仅收集结果、不推送事件的发射器。
    pub fn collect_only() -> Self {
        Self { tx: None }
    }

    /// 判断底层推送通道是否已经关闭。
    pub fn is_closed(&self) -> bool {
        self.tx.as_ref().is_some_and(|tx| tx.is_closed())
    }

    /// 判断该发射器是否绑定了会随客户端断流而关闭的 SSE 通道。
    pub fn is_streaming(&self) -> bool {
        self.tx.is_some()
    }

    /// 等待 SSE 接收端关闭。调用方应先用 `is_streaming` 排除 collect-only 发射器。
    pub async fn wait_closed(&self) {
        if let Some(tx) = self.tx.as_ref() {
            tx.closed().await;
        }
    }

    /// 返回工具输出流可复用的事件发送通道。
    pub fn tool_event_sender(&self) -> Option<&RuntimeSseSender> {
        self.tx.as_ref()
    }

    /// 发送单个运行时事件；无推送通道时静默成功。
    pub async fn emit(&self, event: RuntimeEvent) -> Result<(), ()> {
        let Some(tx) = self.tx.as_ref() else {
            return Ok(());
        };
        tx.send(Ok(
            Event::default().data(runtime_event_payload(event).to_string())
        ))
        .await
        .map_err(|_| ())
    }

    /// 发送事件，失败时转换为模型链路可感知的中断错误。
    pub async fn emit_or_die(
        &self,
        event: RuntimeEvent,
    ) -> Result<(), muse_core::model::provider::ChatModelError> {
        self.emit(event).await.map_err(|_| {
            muse_core::model::provider::ChatModelError::ApiError("客户端连接已断开。".into())
        })
    }
}

/// 将核心运行时事件转换成前端消费的稳定 JSON。
pub fn runtime_event_payload(event: RuntimeEvent) -> serde_json::Value {
    match event {
        RuntimeEvent::TurnStarted {
            turn_id,
            conversation_id,
            persona_id,
            model_provider,
            model_name,
            voice_enabled,
            active_voice_id,
            runtime_mode,
            focus_phase,
            tool_preset,
        } => {
            let model = format!("{model_provider} / {model_name}");
            serde_json::json!({
                "type": "turn_started",
                "phase": "queued",
                "turn_id": turn_id,
                "conversation_id": conversation_id,
                "persona_id": persona_id,
                "model": model,
                "model_provider": model_provider,
                "model_name": model_name,
                "voice_enabled": voice_enabled,
                "active_voice_id": active_voice_id,
                "runtime_mode": runtime_mode,
                "focus_phase": focus_phase,
                "tool_preset": tool_preset,
                "state": "active",
            })
        }
        RuntimeEvent::Status {
            phase,
            message,
            detail,
            state,
        } => serde_json::json!({
            "type": "status",
            "phase": phase,
            "message": message,
            "detail": detail,
            "state": state,
        }),
        RuntimeEvent::ReasoningDelta { content } => serde_json::json!({
            "type": "reasoning_delta",
            "phase": "reasoning",
            "content": content,
            "state": "active",
        }),
        RuntimeEvent::AssistantSegmentStarted => serde_json::json!({
            "type": "assistant_segment_started",
            "phase": "generating",
            "state": "active",
        }),
        RuntimeEvent::AssistantDelta { content } => serde_json::json!({
            "type": "assistant_delta",
            "phase": "generating",
            "content": content,
            "state": "active",
        }),
        RuntimeEvent::AssistantMessage { content } => serde_json::json!({
            "type": "assistant_message",
            "phase": "completed",
            "content": content,
            "state": "completed",
        }),
        RuntimeEvent::Emotion { emotion } => serde_json::json!({
            "type": "emotion",
            "emotion": emotion,
            "state": "active",
        }),
        RuntimeEvent::ToolCall {
            call_id,
            name,
            arguments,
            risk,
            requires_approval,
            interrupt_behavior,
        } => serde_json::json!({
            "type": "tool_call",
            "phase": "tool_running",
            "call_id": call_id,
            "name": name,
            "arguments": arguments,
            "risk": risk,
            "requires_approval": requires_approval,
            "interrupt_behavior": interrupt_behavior,
            "state": "active",
        }),
        RuntimeEvent::ToolResult {
            call_id,
            name,
            success,
            content,
            structured,
        } => serde_json::json!({
            "type": "tool_result",
            "phase": if success { "tool_completed" } else { "failed" },
            "call_id": call_id,
            "name": name,
            "success": success,
            "content": content,
            "structured": structured,
            "state": if success { "completed" } else { "error" },
        }),
        RuntimeEvent::ToolOutputDelta {
            call_id,
            name,
            stream,
            content,
        } => serde_json::json!({
            "type": "tool_output_delta",
            "phase": "tool_running",
            "call_id": call_id,
            "name": name,
            "stream": stream,
            "content": content,
            "state": "active",
        }),
        RuntimeEvent::TokenUsage { usage } => serde_json::json!({
            "type": "token_usage",
            "phase": "completed",
            "usage": usage,
            "state": "completed",
        }),
        RuntimeEvent::ContextSnapshot { snapshot } => serde_json::json!({
            "type": "context_snapshot",
            "phase": "completed",
            "snapshot": snapshot,
            "state": "completed",
        }),
        RuntimeEvent::SpeechStarted {
            call_id,
            text,
            voice_id,
        } => serde_json::json!({
            "type": "speech_started",
            "phase": "speech_started",
            "call_id": call_id,
            "text": text,
            "voice_id": voice_id,
            "message": "开始语音播报。",
            "state": "active",
        }),
        RuntimeEvent::SpeechFinished {
            call_id,
            success,
            message,
        } => serde_json::json!({
            "type": "speech_finished",
            "phase": "speech_finished",
            "call_id": call_id,
            "success": success,
            "message": message,
            "state": if success { "completed" } else { "error" },
        }),
        RuntimeEvent::ApprovalPending {
            approval_id,
            call_id,
            name,
            risk,
            message,
            detail,
            arguments,
        } => serde_json::json!({
            "type": "approval_pending",
            "phase": "approval_pending",
            "approval_id": approval_id,
            "call_id": call_id,
            "name": name,
            "risk": risk,
            "message": message,
            "detail": detail,
            "arguments": arguments,
            "state": "waiting_approval",
        }),
        RuntimeEvent::ApprovalResolved {
            approval_id,
            approved,
            reason,
        } => serde_json::json!({
            "type": "approval_resolved",
            "phase": "approval_resolved",
            "approval_id": approval_id,
            "approved": approved,
            "reason": reason,
            "state": "completed",
        }),
        RuntimeEvent::UserQuestionPending {
            request_id,
            call_id,
            name,
            message,
            questions,
            arguments,
        } => serde_json::json!({
            "type": "user_question_pending",
            "phase": "user_question_pending",
            "request_id": request_id,
            "call_id": call_id,
            "name": name,
            "message": message,
            "questions": questions,
            "arguments": arguments,
            "state": "waiting_user",
        }),
        RuntimeEvent::UserQuestionResolved {
            request_id,
            answered,
            reason,
        } => serde_json::json!({
            "type": "user_question_resolved",
            "phase": "user_question_resolved",
            "request_id": request_id,
            "answered": answered,
            "reason": reason,
            "state": "completed",
        }),
        RuntimeEvent::Error { message } => {
            let content = message.clone();
            serde_json::json!({
                "type": "error",
                "phase": "failed",
                "message": message,
                "content": content,
                "state": "error",
            })
        }
        RuntimeEvent::Done => serde_json::json!({
            "type": "done",
            "phase": "completed",
            "state": "completed",
        }),
    }
}

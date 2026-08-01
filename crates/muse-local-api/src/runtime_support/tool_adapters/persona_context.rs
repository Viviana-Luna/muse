//! Persona、模型、流式回复与上下文用量工具支持。

use super::*;

pub(in crate::runtime_support) fn tool_model_info(turn: &TurnContext) -> ToolResult {
    let provider = turn.model_provider.clone();
    let model = turn.model_name.clone();
    ToolResult {
        status: ToolResultStatus::Success,
        content: format!("当前模型 Provider：{provider}，模型：{model}。"),
        structured: Some(serde_json::json!({
            "chat": {
                "provider": provider,
                "model": model,
                "frozen_for_turn": true,
            }
        })),
    }
}
pub(in crate::runtime_support) async fn tool_persona_info(state: &Arc<AppState>) -> ToolResult {
    let persona = {
        let personas = state.personas.lock().await;
        personas.active_persona().cloned()
    };
    ToolResult {
        status: ToolResultStatus::from_success(persona.is_some()),
        content: persona
            .as_ref()
            .map(|item| format!("当前角色：{}（id: {}）。", item.name, item.id))
            .unwrap_or_else(|| "当前没有激活角色。".to_string()),
        structured: Some(serde_json::json!({ "persona": persona })),
    }
}

pub(in crate::runtime_support) async fn tool_persona_switch(
    state: &Arc<AppState>,
    call: &ToolCall,
) -> ToolResult {
    let Some(persona_id) = tool_arg_string(&call.arguments, "persona_id") else {
        return tool_failed(
            "persona_switch 缺少 persona_id 参数。",
            "missing_persona_id",
        );
    };
    let _transition = state.persona_runtime_transition_gate.lock().await;
    let persona = {
        let mut personas = state.personas.lock().await;
        let Some(persona) = personas.get(&persona_id).cloned() else {
            return tool_failed(format!("角色 `{persona_id}` 不存在。"), "persona_not_found");
        };
        if let Err(err) = personas.set_active(&persona_id) {
            return tool_failed(format!("切换角色失败：{err}"), "switch_failed");
        }
        if let Err(err) = personas.save() {
            return tool_failed(format!("保存角色状态失败：{err}"), "save_failed");
        }
        persona
    };
    reset_conversation_for_active_persona(state).await;
    state.runtime_service.touch();
    ToolResult {
        status: ToolResultStatus::Success,
        content: format!("已切换到角色：{}。", persona.name),
        structured: Some(serde_json::json!({ "persona": persona })),
    }
}

pub(in crate::runtime_support) fn tool_failed(
    message: impl Into<String>,
    reason: &str,
) -> ToolResult {
    ToolResult {
        status: ToolResultStatus::Failed,
        content: message.into(),
        structured: Some(serde_json::json!({ "reason": reason })),
    }
}

pub(in crate::runtime_support) fn tool_result_reason(result: &ToolResult) -> Option<&str> {
    result
        .structured
        .as_ref()
        .and_then(|value| value.get("reason"))
        .and_then(|value| value.as_str())
}

pub(in crate::runtime_support) async fn stream_provider_reply(
    state: &Arc<AppState>,
    provider: &Arc<dyn muse_core::model::provider::ChatModelProvider>,
    emitter: &RuntimeEventEmitter,
    conversation: &muse_core::domain::conversation::Conversation,
    tool_defs: &[ToolDef],
    cancel_token: &RuntimeTurnCancel,
) -> Result<StreamedTurn, muse_core::model::provider::ChatModelError> {
    use futures::StreamExt;

    let stream_request = provider.chat_stream_with_tools(conversation, tool_defs);
    tokio::pin!(stream_request);
    let mut stream = tokio::select! {
        stream = &mut stream_request => stream,
        _ = wait_for_turn_cancel(cancel_token) => {
            return Err(muse_core::model::provider::ChatModelError::ApiError(
                TURN_CANCELLED_MESSAGE.to_string(),
            ));
        }
    };
    emitter
        .emit_or_die(RuntimeEvent::AssistantSegmentStarted)
        .await?;
    emitter
        .emit_or_die(RuntimeEvent::Status {
            phase: "generating".to_string(),
            message: "模型正在生成回复。".to_string(),
            detail: None,
            state: "active".to_string(),
        })
        .await?;
    let mut full_reply = String::new();
    let mut full_reasoning = String::new();
    let mut items = Vec::<RuntimeModelItem>::new();
    let mut usage = None::<ProviderTokenUsage>;
    let mut prefix_state = EmotionPrefixState::default();
    let mut tool_prefix_state = ToolCallPrefixState::default();
    let mut emotion_candidate = None;

    loop {
        if cancel_token.is_cancelled() {
            return Err(muse_core::model::provider::ChatModelError::ApiError(
                TURN_CANCELLED_MESSAGE.to_string(),
            ));
        }
        let event = tokio::select! {
            event = stream.next() => event,
            _ = wait_for_turn_cancel(cancel_token) => {
                return Err(muse_core::model::provider::ChatModelError::ApiError(
                    TURN_CANCELLED_MESSAGE.to_string(),
                ));
            }
        };
        let Some(event) = event else {
            break;
        };
        match event {
            Ok(ChatStreamEvent::Reasoning(reasoning)) => {
                if !reasoning.is_empty() {
                    full_reasoning.push_str(&reasoning);
                    emitter
                        .emit_or_die(RuntimeEvent::ReasoningDelta { content: reasoning })
                        .await?;
                }
            }
            Ok(ChatStreamEvent::Text(text)) => match prefix_state.push_chunk(&text) {
                PrefixParseResult::Pending => {}
                PrefixParseResult::Resolved { emotion, text } => {
                    if let Some(emotion) = emotion {
                        let _ = state.emotion_tx.send(emotion.emotion.clone());
                        emitter
                            .emit_or_die(RuntimeEvent::Emotion {
                                emotion: emotion.emotion.clone(),
                            })
                            .await?;
                        emotion_candidate = Some(emotion);
                    }

                    if !text.is_empty() {
                        match tool_prefix_state.push_chunk(&text) {
                            ToolCallPrefixParseResult::Pending => {}
                            ToolCallPrefixParseResult::Text(content) => {
                                if !content.is_empty() {
                                    full_reply.push_str(&content);
                                    emitter
                                        .emit_or_die(RuntimeEvent::AssistantDelta { content })
                                        .await?;
                                }
                            }
                            ToolCallPrefixParseResult::ToolCall { call, text } => {
                                if !text.is_empty() {
                                    full_reply.push_str(&text);
                                    emitter
                                        .emit_or_die(RuntimeEvent::AssistantDelta { content: text })
                                        .await?;
                                }
                                push_assistant_model_item(&mut items, &mut full_reply);
                                items.push(RuntimeModelItem::ToolCall {
                                    call,
                                    reasoning_content: None,
                                });
                            }
                        }
                    }
                }
            },
            Ok(ChatStreamEvent::Emotion(emotion)) => {
                prefix_state.mark_resolved();
                let _ = state.emotion_tx.send(emotion.clone());
                emitter
                    .emit_or_die(RuntimeEvent::Emotion { emotion })
                    .await?;
            }
            Ok(ChatStreamEvent::ToolCall(call)) => {
                match tool_prefix_state.finish() {
                    ToolCallPrefixParseResult::Pending => {}
                    ToolCallPrefixParseResult::Text(tail) => {
                        if !tail.is_empty() {
                            full_reply.push_str(&tail);
                            emitter
                                .emit_or_die(RuntimeEvent::AssistantDelta { content: tail })
                                .await?;
                        }
                    }
                    ToolCallPrefixParseResult::ToolCall {
                        call: _fallback_call,
                        text,
                    } => {
                        if !text.is_empty() {
                            full_reply.push_str(&text);
                            emitter
                                .emit_or_die(RuntimeEvent::AssistantDelta { content: text })
                                .await?;
                        }
                    }
                }
                push_assistant_model_item(&mut items, &mut full_reply);
                items.push(RuntimeModelItem::ToolCall {
                    call,
                    reasoning_content: None,
                });
            }
            Ok(ChatStreamEvent::Usage(next_usage)) => {
                if let Some(current_usage) = usage.as_mut() {
                    current_usage.merge_cumulative(next_usage);
                } else {
                    usage = Some(next_usage);
                }
            }
            Ok(ChatStreamEvent::Done) => {
                if let Some(text) = prefix_state.finish()
                    && !text.is_empty()
                {
                    match tool_prefix_state.push_chunk(&text) {
                        ToolCallPrefixParseResult::Pending => {}
                        ToolCallPrefixParseResult::Text(content) => {
                            if !content.is_empty() {
                                full_reply.push_str(&content);
                                emitter
                                    .emit_or_die(RuntimeEvent::AssistantDelta { content })
                                    .await?;
                            }
                        }
                        ToolCallPrefixParseResult::ToolCall { call, text } => {
                            if !text.is_empty() {
                                full_reply.push_str(&text);
                                emitter
                                    .emit_or_die(RuntimeEvent::AssistantDelta { content: text })
                                    .await?;
                            }
                            push_assistant_model_item(&mut items, &mut full_reply);
                            items.push(RuntimeModelItem::ToolCall {
                                call,
                                reasoning_content: None,
                            });
                        }
                    }
                }
                match tool_prefix_state.finish() {
                    ToolCallPrefixParseResult::Pending => {}
                    ToolCallPrefixParseResult::Text(tail) => {
                        if !tail.is_empty() {
                            full_reply.push_str(&tail);
                            emitter
                                .emit_or_die(RuntimeEvent::AssistantDelta { content: tail })
                                .await?;
                        }
                    }
                    ToolCallPrefixParseResult::ToolCall { call, text } => {
                        if !text.is_empty() {
                            full_reply.push_str(&text);
                            emitter
                                .emit_or_die(RuntimeEvent::AssistantDelta { content: text })
                                .await?;
                        }
                        push_assistant_model_item(&mut items, &mut full_reply);
                        items.push(RuntimeModelItem::ToolCall {
                            call,
                            reasoning_content: None,
                        });
                    }
                }
                push_assistant_model_item(&mut items, &mut full_reply);
                return Ok(streamed_turn_with_reasoning(
                    items,
                    full_reasoning,
                    usage,
                    emotion_candidate,
                ));
            }
            Err(err) => {
                return Err(err);
            }
        }
    }

    push_assistant_model_item(&mut items, &mut full_reply);
    Ok(streamed_turn_with_reasoning(
        items,
        full_reasoning,
        usage,
        emotion_candidate,
    ))
}

pub(in crate::runtime_support) fn push_assistant_model_item(
    items: &mut Vec<RuntimeModelItem>,
    content: &mut String,
) {
    if content.is_empty() {
        return;
    }
    items.push(RuntimeModelItem::AssistantMessage {
        content: std::mem::take(content),
        reasoning_content: None,
    });
}

pub(in crate::runtime_support) fn streamed_turn_with_reasoning(
    mut items: Vec<RuntimeModelItem>,
    reasoning_content: String,
    usage: Option<ProviderTokenUsage>,
    emotion_candidate: Option<PersonaEmotionEffect>,
) -> StreamedTurn {
    if reasoning_content.trim().is_empty() {
        return StreamedTurn {
            items,
            usage,
            emotion_candidate,
        };
    }

    let has_tool_call = items
        .iter()
        .any(|item| matches!(item, RuntimeModelItem::ToolCall { .. }));
    if has_tool_call {
        for item in &mut items {
            if let RuntimeModelItem::ToolCall {
                reasoning_content: item_reasoning,
                ..
            } = item
            {
                *item_reasoning = Some(reasoning_content.clone());
            }
        }
    } else if let Some(RuntimeModelItem::AssistantMessage {
        reasoning_content: item_reasoning,
        ..
    }) = items.last_mut()
    {
        *item_reasoning = Some(reasoning_content);
    }

    StreamedTurn {
        items,
        usage,
        emotion_candidate,
    }
}

#[derive(Debug, Clone, Copy)]
pub(in crate::runtime_support) struct RuntimeContextProfile {
    pub(in crate::runtime_support) context_window: u64,
    pub(in crate::runtime_support) reserved_output_tokens: u64,
}

pub(in crate::runtime_support) fn runtime_context_profile(
    turn: &TurnContext,
) -> RuntimeContextProfile {
    RuntimeContextProfile {
        context_window: turn.model_context_window.max(1),
        reserved_output_tokens: u64::from(turn.model_max_output_tokens.max(1)),
    }
}

pub(in crate::runtime_support) fn estimate_text_tokens(text: &str) -> u64 {
    let mut ascii_chars = 0_u64;
    let mut non_ascii_chars = 0_u64;
    for ch in text.chars() {
        if ch.is_ascii() {
            ascii_chars = ascii_chars.saturating_add(1);
        } else {
            non_ascii_chars = non_ascii_chars.saturating_add(1);
        }
    }
    if ascii_chars == 0 && non_ascii_chars == 0 {
        return 0;
    }
    ascii_chars
        .saturating_add(3)
        .saturating_div(4)
        .saturating_add(non_ascii_chars)
        .max(1)
}

pub(in crate::runtime_support) fn estimate_json_tokens(value: &serde_json::Value) -> u64 {
    serde_json::to_string(value)
        .map(|text| estimate_text_tokens(&text))
        .unwrap_or(0)
}

pub(in crate::runtime_support) fn push_estimated_context_segment(
    segments: &mut BTreeMap<(String, String, bool, bool), RuntimeContextSegment>,
    kind: &str,
    label: &str,
    tokens: u64,
    compacted: bool,
    externalized: bool,
) {
    if tokens == 0 {
        return;
    }
    let key = (kind.to_string(), label.to_string(), compacted, externalized);
    segments
        .entry(key)
        .and_modify(|segment| {
            segment.tokens = segment.tokens.saturating_add(tokens);
        })
        .or_insert_with(|| RuntimeContextSegment {
            kind: kind.to_string(),
            label: label.to_string(),
            tokens,
            source: TokenUsageSource::LocalEstimated,
            compacted,
            externalized,
            metadata: None,
        });
}

pub(in crate::runtime_support) fn split_system_prompt_for_segments(
    turn: &TurnContext,
) -> (Option<String>, String) {
    let prompt = turn.system_prompt.trim();
    if prompt.is_empty() {
        return (None, String::new());
    }
    if turn.persona_id.is_none() {
        return (None, prompt.to_string());
    }
    let runtime_boundary = ["\n\n【运行状态】", "\n\n【运行模式】"]
        .into_iter()
        .filter_map(|marker| prompt.find(marker))
        .min();
    if let Some(index) = runtime_boundary {
        let persona = prompt[..index].trim().to_string();
        let system = prompt[index..].trim().to_string();
        return (Some(persona).filter(|value| !value.is_empty()), system);
    }
    (Some(prompt.to_string()), String::new())
}

pub(in crate::runtime_support) fn tool_context_segment_kind(
    tool_name: Option<&str>,
    content: &str,
) -> (&'static str, &'static str, bool) {
    match tool_name.unwrap_or_default() {
        "session_compact" => ("compact_summary", "压缩摘要", true),
        "mcp_read_resource" | "mcp_list_resources" | "mcp_list_resource_templates" => {
            ("mcp_resource", "MCP 资源", false)
        }
        "load_skill" | "use_skill" | "skill" => ("skill", "技能说明", false),
        "agent" | "task_stop" | "todo_write" => ("task_state", "任务状态", false),
        name if mcp::is_external_mcp_tool_name(name) => ("mcp_resource", "MCP 资源", false),
        _ if content.contains("会话已完成压缩") => ("compact_summary", "压缩摘要", true),
        _ => ("tool_result", "工具结果", false),
    }
}

fn memory_query_context_metadata(content: &str, page: usize) -> serde_json::Value {
    const STRUCTURED_MARKER: &str = "结构化结果（供模型继续决策）：";
    let structured = content
        .split_once(STRUCTURED_MARKER)
        .and_then(|(_, value)| serde_json::from_str::<serde_json::Value>(value.trim()).ok());
    let items = structured
        .as_ref()
        .and_then(|value| value.get("items"))
        .and_then(serde_json::Value::as_array);
    let memories = items
        .into_iter()
        .flatten()
        .filter_map(|item| {
            Some(serde_json::json!({
                "memory_id": item.get("memory_id")?.as_str()?,
                "revision_id": item.get("revision_id")?.as_str()?,
                "category": item.get("category").and_then(serde_json::Value::as_str),
                "importance": item.get("importance").and_then(serde_json::Value::as_str),
            }))
        })
        .collect::<Vec<_>>();
    serde_json::json!({
        "query_page": page,
        "item_count": memories.len(),
        "has_more": structured
            .as_ref()
            .and_then(|value| value.get("has_more"))
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        "memories": memories,
        "eviction_reason": null,
    })
}

pub(in crate::runtime_support) fn runtime_context_segments_from_conversation(
    turn: &TurnContext,
    conversation: &Conversation,
) -> Vec<RuntimeContextSegment> {
    let mut segments = BTreeMap::<(String, String, bool, bool), RuntimeContextSegment>::new();
    let mut memory_query_page = 0_usize;
    let (persona_prompt, runtime_prompt) = split_system_prompt_for_segments(turn);
    if let Some(persona_prompt) = persona_prompt {
        push_estimated_context_segment(
            &mut segments,
            "persona",
            "角色设定",
            estimate_text_tokens(&persona_prompt),
            false,
            false,
        );
    }
    push_estimated_context_segment(
        &mut segments,
        "system",
        "运行指令",
        estimate_text_tokens(&runtime_prompt),
        false,
        false,
    );

    for message in conversation
        .api_messages()
        .iter()
        .filter(|message| message.role != Role::System)
    {
        match message.role {
            Role::User => push_estimated_context_segment(
                &mut segments,
                "history",
                "用户消息",
                estimate_text_tokens(&message.content),
                false,
                false,
            ),
            Role::Assistant if message.tool_call_id.is_some() => {
                let mut tokens = estimate_text_tokens(message.tool_name.as_deref().unwrap_or(""));
                if let Some(arguments) = message.tool_arguments.as_ref() {
                    tokens = tokens.saturating_add(estimate_json_tokens(arguments));
                }
                if let Some(reasoning) = message.reasoning_content.as_ref() {
                    tokens = tokens.saturating_add(estimate_text_tokens(reasoning));
                }
                push_estimated_context_segment(
                    &mut segments,
                    "tool_call",
                    "工具调用参数",
                    tokens,
                    false,
                    false,
                );
            }
            Role::Assistant => {
                let mut tokens = estimate_text_tokens(&message.content);
                if let Some(reasoning) = message.reasoning_content.as_ref() {
                    tokens = tokens.saturating_add(estimate_text_tokens(reasoning));
                }
                push_estimated_context_segment(
                    &mut segments,
                    "history",
                    "助手回复",
                    tokens,
                    false,
                    false,
                );
            }
            Role::Tool => {
                if message.tool_name.as_deref()
                    == Some(muse_core::domain::memory::MEMORY_QUERY_TOOL_NAME)
                {
                    memory_query_page = memory_query_page.saturating_add(1);
                    let label = format!("长期记忆查询页 {memory_query_page}");
                    segments.insert(
                        ("memory".to_string(), label.clone(), false, false),
                        RuntimeContextSegment {
                            kind: "memory".to_string(),
                            label,
                            tokens: estimate_text_tokens(&message.content),
                            source: TokenUsageSource::LocalEstimated,
                            compacted: false,
                            externalized: false,
                            metadata: Some(memory_query_context_metadata(
                                &message.content,
                                memory_query_page,
                            )),
                        },
                    );
                    continue;
                }
                let externalized = message.content.contains("tool_result_read")
                    || message.content.contains("外置归档");
                let (kind, label, compacted) =
                    tool_context_segment_kind(message.tool_name.as_deref(), &message.content);
                push_estimated_context_segment(
                    &mut segments,
                    kind,
                    label,
                    estimate_text_tokens(&message.content),
                    compacted,
                    externalized,
                );
            }
            Role::System => {}
        }
    }

    push_estimated_context_segment(
        &mut segments,
        "task_state",
        "当前任务运行态",
        estimate_text_tokens(&format!(
            "运行模式：{}；阶段：{}；工具预设：{}。",
            turn.runtime_mode, turn.focus_phase, turn.tool_preset
        )),
        false,
        false,
    );

    segments.into_values().collect()
}

pub(in crate::runtime_support) fn build_runtime_context_snapshot(
    turn: &TurnContext,
    conversation: &Conversation,
    profile: RuntimeContextProfile,
) -> RuntimeContextSnapshot {
    let segments = runtime_context_segments_from_conversation(turn, conversation);
    let used_total_tokens = segments
        .iter()
        .fold(0_u64, |total, segment| total.saturating_add(segment.tokens));
    let compacted = segments.iter().any(|segment| segment.compacted);
    let externalized_tool_results = segments.iter().any(|segment| segment.externalized);
    let remaining_tokens = profile
        .context_window
        .saturating_sub(profile.reserved_output_tokens)
        .saturating_sub(used_total_tokens);
    let usage_percent = if profile.context_window == 0 {
        0.0
    } else {
        (used_total_tokens as f64 / profile.context_window as f64) * 100.0
    };

    RuntimeContextSnapshot {
        conversation_id: turn.conversation_id.clone(),
        turn_id: turn.turn_id.clone(),
        provider: turn.model_provider.clone(),
        model: turn.model_name.clone(),
        created_at: chrono::Utc::now().to_rfc3339(),
        context_window: profile.context_window,
        reserved_output_tokens: profile.reserved_output_tokens,
        used_input_tokens: used_total_tokens,
        used_cache_tokens: 0,
        used_total_tokens,
        remaining_tokens,
        usage_percent,
        source: TokenUsageSource::LocalEstimated,
        compacted,
        externalized_tool_results,
        segments,
    }
}

pub(in crate::runtime_support) fn estimate_streamed_turn_output_tokens(turn: &StreamedTurn) -> u64 {
    turn.items.iter().fold(0_u64, |total, item| match item {
        RuntimeModelItem::AssistantMessage {
            content,
            reasoning_content,
        } => total
            .saturating_add(estimate_text_tokens(content))
            .saturating_add(
                reasoning_content
                    .as_ref()
                    .map(|value| estimate_text_tokens(value))
                    .unwrap_or(0),
            ),
        RuntimeModelItem::ToolCall {
            call,
            reasoning_content,
        } => total
            .saturating_add(estimate_text_tokens(&call.name))
            .saturating_add(estimate_json_tokens(&call.arguments))
            .saturating_add(
                reasoning_content
                    .as_ref()
                    .map(|value| estimate_text_tokens(value))
                    .unwrap_or(0),
            ),
    })
}

pub(in crate::runtime_support) async fn emit_and_record_runtime_usage(
    _state: &Arc<AppState>,
    emitter: &RuntimeEventEmitter,
    turn: &TurnContext,
    snapshot: RuntimeContextSnapshot,
    streamed_turn: &StreamedTurn,
) {
    let provider_usage = streamed_turn.usage.clone().unwrap_or_else(|| {
        ProviderTokenUsage::local_estimated(
            snapshot.used_total_tokens,
            estimate_streamed_turn_output_tokens(streamed_turn),
        )
    });
    let snapshot = snapshot.with_provider_usage(&provider_usage);
    let usage = RuntimeTokenUsage::from_provider_usage(
        next_runtime_id("usage"),
        turn.conversation_id.clone(),
        turn.turn_id.clone(),
        turn.model_provider.clone(),
        turn.model_name.clone(),
        chrono::Utc::now().to_rfc3339(),
        provider_usage,
    );

    match RuntimeUsageStore::load_from_dir(muse_core::config::Config::config_dir()) {
        Ok(store) => {
            if let Err(err) = store.record_context_snapshot(&snapshot) {
                tracing::warn!("记录上下文快照失败：{err}");
            }
            if let Err(err) = store.record_token_usage(&usage) {
                tracing::warn!("记录 Token 用量失败：{err}");
            }
        }
        Err(err) => tracing::warn!("初始化运行时用量存储失败：{err}"),
    }

    let _ = emitter
        .emit(RuntimeEvent::ContextSnapshot { snapshot })
        .await;
    let _ = emitter.emit(RuntimeEvent::TokenUsage { usage }).await;
}

pub(in crate::runtime_support) async fn emit_json_event(
    tx: &RuntimeSseSender,
    payload: serde_json::Value,
) -> Result<(), ()> {
    tx.send(Ok(
        axum::response::sse::Event::default().data(payload.to_string())
    ))
    .await
    .map_err(|_| ())
}

#[cfg(test)]
pub(in crate::runtime_support) fn chat_status_payload(
    phase: &str,
    message: &str,
    detail: Option<&str>,
    state: &str,
) -> serde_json::Value {
    serde_json::json!({
        "type": "status",
        "phase": phase,
        "message": message,
        "detail": detail,
        "state": state
    })
}

pub(in crate::runtime_support) fn runtime_tool_call_event(
    call_id: &str,
    name: &str,
    arguments: &serde_json::Value,
    risk: &str,
    requires_approval: bool,
    interrupt_behavior: RuntimeToolInterruptBehavior,
) -> serde_json::Value {
    runtime_event_payload(RuntimeEvent::ToolCall {
        call_id: call_id.to_string(),
        name: name.to_string(),
        arguments: arguments.clone(),
        risk: risk.to_string(),
        requires_approval,
        interrupt_behavior: interrupt_behavior.as_str().to_string(),
    })
}

pub(in crate::runtime_support) fn runtime_tool_result_event(
    call_id: &str,
    name: &str,
    success: bool,
    content: &str,
    structured: Option<&serde_json::Value>,
) -> serde_json::Value {
    runtime_event_payload(RuntimeEvent::ToolResult {
        call_id: call_id.to_string(),
        name: name.to_string(),
        success,
        content: content.to_string(),
        structured: structured.cloned(),
    })
}

pub(in crate::runtime_support) fn runtime_command_output_delta_event(
    call_id: &str,
    name: &str,
    stream: &str,
    content: &str,
) -> serde_json::Value {
    runtime_event_payload(RuntimeEvent::ToolOutputDelta {
        call_id: call_id.to_string(),
        name: name.to_string(),
        stream: stream.to_string(),
        content: content.to_string(),
    })
}

pub(in crate::runtime_support) fn runtime_approval_pending_event(
    approval_id: &str,
    call_id: &str,
    tool_name: &str,
    risk: &str,
    message: &str,
    detail: Option<&str>,
    arguments: &serde_json::Value,
) -> serde_json::Value {
    runtime_event_payload(RuntimeEvent::ApprovalPending {
        approval_id: approval_id.to_string(),
        call_id: call_id.to_string(),
        name: tool_name.to_string(),
        risk: risk.to_string(),
        message: message.to_string(),
        detail: detail.map(ToString::to_string),
        arguments: arguments.clone(),
    })
}

pub(in crate::runtime_support) fn runtime_approval_resolved_event(
    approval_id: &str,
    approved: bool,
    reason: Option<&str>,
) -> serde_json::Value {
    runtime_event_payload(RuntimeEvent::ApprovalResolved {
        approval_id: approval_id.to_string(),
        approved,
        reason: reason.map(ToString::to_string),
    })
}

pub(in crate::runtime_support) fn runtime_user_question_pending_event(
    request_id: &str,
    call_id: &str,
    tool_name: &str,
    message: &str,
    questions: &serde_json::Value,
    arguments: &serde_json::Value,
) -> serde_json::Value {
    runtime_event_payload(RuntimeEvent::UserQuestionPending {
        request_id: request_id.to_string(),
        call_id: call_id.to_string(),
        name: tool_name.to_string(),
        message: message.to_string(),
        questions: questions.clone(),
        arguments: arguments.clone(),
    })
}

pub(in crate::runtime_support) fn runtime_user_question_resolved_event(
    request_id: &str,
    answered: bool,
    reason: Option<&str>,
) -> serde_json::Value {
    runtime_event_payload(RuntimeEvent::UserQuestionResolved {
        request_id: request_id.to_string(),
        answered,
        reason: reason.map(ToString::to_string),
    })
}

pub(in crate::runtime_support) fn runtime_speech_started_event(
    call_id: &str,
    text: &str,
    voice_id: Option<&str>,
) -> serde_json::Value {
    runtime_event_payload(RuntimeEvent::SpeechStarted {
        call_id: call_id.to_string(),
        text: text.to_string(),
        voice_id: voice_id.map(ToString::to_string),
    })
}

pub(in crate::runtime_support) async fn resolve_persona_visual_pack(
    state: &Arc<AppState>,
    persona: &Persona,
) -> Option<VisualPack> {
    let visual_packs = state.visual_packs.lock().await;
    resolve_persona_visual_pack_from_store(&visual_packs, persona)
}

pub(in crate::runtime_support) fn build_persona_visual_pack_from_patch(
    visual_packs: &muse_core::domain::persona::visual::store::VisualPackStore,
    persona: &mut Persona,
    patch: Option<PersonaVisualPackPatch>,
) -> Option<VisualPack> {
    let patch = patch?;
    let existing = visual_packs.get(&format!("visual-{}", persona.id));
    let portrait_path = trim_optional_patch_value(Some(patch.portrait_path))
        .or_else(|| existing.map(|pack| pack.portrait_path.clone()));
    // 背景字段保留给旧卡片兼容；显式空值代表产品已退场并清除旧引用，None 才表示保留。
    let background_path = patch
        .background_path
        .map(|value| value.trim().to_string())
        .or_else(|| existing.map(|pack| pack.background_path.clone()));
    let avatar_path = trim_optional_patch_value(patch.avatar_path)
        .or_else(|| existing.map(|pack| pack.avatar_path.clone()));

    let visual_pack_id = format!("visual-{}", persona.id);
    let portrait_path = portrait_path.unwrap_or_default();
    let background_path = background_path.unwrap_or_default();
    let avatar_path = avatar_path.unwrap_or_default();
    let theme_color = trim_optional_patch_value(patch.theme_color)
        .unwrap_or_else(|| DEFAULT_PERSONA_THEME_COLOR.to_string());
    let patch_theme_mode = trim_optional_patch_value(patch.theme_mode);
    let patch_portrait_frame = trim_optional_patch_value(patch.portrait_frame);
    let patch_portrait_fit = trim_optional_patch_value(patch.portrait_fit);

    let portrait_frame = normalize_portrait_frame(
        patch_portrait_frame
            .as_deref()
            .or_else(|| existing.map(|pack| pack.portrait_frame.as_str())),
    );
    let portrait_fit = normalize_portrait_fit(
        patch_portrait_fit
            .as_deref()
            .or_else(|| existing.map(|pack| pack.portrait_fit.as_str())),
    );
    let theme_mode = normalize_theme_mode(
        patch_theme_mode
            .as_deref()
            .or_else(|| existing.map(|pack| pack.theme_mode.as_str())),
    );
    let portrait_position_x = clamp_i32(
        patch
            .portrait_position_x
            .or_else(|| existing.map(|pack| pack.portrait_position_x))
            .unwrap_or(50),
        0,
        100,
    );
    let portrait_position_y = clamp_i32(
        patch
            .portrait_position_y
            .or_else(|| existing.map(|pack| pack.portrait_position_y))
            .unwrap_or(50),
        0,
        100,
    );
    let portrait_scale = clamp_u16(
        patch
            .portrait_scale
            .or_else(|| existing.map(|pack| pack.portrait_scale))
            .unwrap_or(100),
        70,
        180,
    );
    let visual_pack = VisualPack {
        id: visual_pack_id.clone(),
        name: format!("{}展示包", persona.name),
        portrait_path,
        background_path,
        avatar_path,
        theme_color,
        theme_mode,
        layout_mode: existing
            .map(|pack| pack.layout_mode.clone())
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| "portrait-right".to_string()),
        portrait_frame,
        portrait_fit,
        portrait_position_x,
        portrait_position_y,
        portrait_scale,
        fallback_text: existing
            .map(|pack| pack.fallback_text.clone())
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| "暂无图片".to_string()),
        version: existing
            .map(|pack| pack.version.clone())
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| "1.0.0".to_string()),
        notes: existing.map(|pack| pack.notes.clone()).unwrap_or_default(),
    };
    persona.default_visual_pack_id = visual_pack_id;
    Some(visual_pack)
}

pub(in crate::runtime_support) fn trim_optional_patch_value(
    value: Option<String>,
) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

pub(in crate::runtime_support) fn normalize_portrait_frame(value: Option<&str>) -> String {
    match value.unwrap_or_default() {
        "square" => "square".to_string(),
        "wide" => "wide".to_string(),
        "full" => "full".to_string(),
        _ => "portrait".to_string(),
    }
}

pub(in crate::runtime_support) fn normalize_theme_mode(value: Option<&str>) -> String {
    match value.unwrap_or_default() {
        "dark" => "dark".to_string(),
        "light" => "light".to_string(),
        _ => "auto".to_string(),
    }
}

pub(in crate::runtime_support) fn normalize_portrait_fit(value: Option<&str>) -> String {
    match value.unwrap_or_default() {
        "contain" => "contain".to_string(),
        _ => "cover".to_string(),
    }
}

pub(in crate::runtime_support) fn clamp_i32(value: i32, min: i32, max: i32) -> i32 {
    value.min(max).max(min)
}

pub(in crate::runtime_support) fn clamp_u16(value: u16, min: u16, max: u16) -> u16 {
    value.min(max).max(min)
}

pub(in crate::runtime_support) fn resolve_persona_visual_pack_from_store(
    visual_packs: &muse_core::domain::persona::visual::store::VisualPackStore,
    persona: &Persona,
) -> Option<VisualPack> {
    visual_packs
        .get(&persona.default_visual_pack_id)
        .or_else(|| visual_packs.get("default-visual-pack"))
        .cloned()
}

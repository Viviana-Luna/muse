async fn tool_session_list(state: &Arc<AppState>) -> ToolResult {
    let payload = match runtime_session_list_payload(state).await {
        Ok(payload) => payload,
        Err(error) => return tool_failed(error, "session_index_unavailable"),
    };
    ToolResult {
        status: ToolResultStatus::Success,
        content: if payload.exists {
            "当前存在 runtime transcript。".to_string()
        } else {
            "当前尚未写入 runtime transcript。".to_string()
        },
        structured: Some(serde_json::json!({ "sessions": payload.sessions })),
    }
}
async fn tool_session_read(state: &Arc<AppState>, call: &ToolCall) -> ToolResult {
    let conversation_id = tool_arg_string(&call.arguments, "conversation_id")
        .unwrap_or_else(|| DEFAULT_CONVERSATION_ID.to_string());
    match read_runtime_transcript_for_conversation(state, &conversation_id).await {
        Ok(content) => ToolResult {
            status: ToolResultStatus::Success,
            content: content.clone(),
            structured: Some(
                serde_json::json!({ "conversation_id": conversation_id, "path": null, "source": "virtual_aggregate", "can_resume": !content.trim().is_empty() }),
            ),
        },
        Err(_) => ToolResult {
            status: ToolResultStatus::Success,
            content: "当前没有可读取的 transcript。".to_string(),
            structured: Some(
                serde_json::json!({ "conversation_id": conversation_id, "path": null, "source": "virtual_aggregate", "can_resume": false }),
            ),
        },
    }
}

async fn tool_result_read(call: &ToolCall) -> ToolResult {
    let Some(result_id) = tool_arg_string(&call.arguments, "result_id") else {
        return tool_failed(
            "tool_result_read 缺少 result_id 参数。",
            "missing_result_id",
        );
    };
    if !crate::tool_result_archive::is_safe_result_id(&result_id) {
        return tool_failed(
            "tool_result_read result_id 含非法字符。",
            "invalid_result_id",
        );
    }
    let offset = call
        .arguments
        .get("offset")
        .and_then(|value| value.as_u64())
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or(0);
    let limit = tool_arg_limit(
        &call.arguments,
        TOOL_RESULT_READ_DEFAULT_CHARS,
        TOOL_RESULT_READ_MAX_CHARS,
    );
    let content = match crate::tool_result_archive::read(&result_id).await {
        Ok(content) => content,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return tool_failed(
                format!("外置工具结果 `{result_id}` 不存在。"),
                "result_not_found",
            );
        }
        Err(err) => {
            return tool_failed(
                format!("读取外置工具结果 `{result_id}` 失败：{err}"),
                "read_failed",
            );
        }
    };
    let Ok(record) = serde_json::from_str::<serde_json::Value>(&content) else {
        return tool_failed(
            format!("外置工具结果 `{result_id}` 内容损坏，无法解析。"),
            "invalid_archive",
        );
    };
    let original_content = record
        .get("content")
        .and_then(|value| value.as_str())
        .unwrap_or_default();
    let total_chars = original_content.chars().count();
    let (body, truncated) = slice_text_by_chars(original_content, offset, limit);
    let body_chars = body.chars().count();
    let next_offset = if truncated {
        Some(offset.saturating_add(body_chars))
    } else {
        None
    };
    let structured_text = record.get("structured").and_then(|structured| {
        if structured.is_null() {
            None
        } else {
            Some(
                serde_json::to_string_pretty(structured).unwrap_or_else(|_| structured.to_string()),
            )
        }
    });
    let structured_preview = structured_text
        .as_ref()
        .map(|value| truncate_text(value, TOOL_RESULT_READ_DEFAULT_CHARS));

    let mut response = format!(
        "外置工具结果 `{result_id}` 内容片段（offset={}，limit={}，总字符={}）：\n{}",
        offset, limit, total_chars, body
    );
    if let Some(preview) = structured_preview
        .as_ref()
        .filter(|value| !value.trim().is_empty())
    {
        response.push_str("\n\n原始结构化结果预览：\n");
        response.push_str(preview);
    }
    if let Some(next_offset) = next_offset {
        response.push_str(&format!(
            "\n\n还有后续内容，可继续调用 `tool_result_read`，offset={next_offset}。"
        ));
    }

    ToolResult {
        status: ToolResultStatus::Success,
        content: response,
        structured: Some(serde_json::json!({
            "result_id": result_id,
            "resource_uri": format!("muse://tool-result/{result_id}"),
            "tool": record.get("tool").cloned().unwrap_or(serde_json::Value::Null),
            "call_id": record.get("call_id").cloned().unwrap_or(serde_json::Value::Null),
            "offset": offset,
            "limit": limit,
            "returned_chars": body_chars,
            "total_chars": total_chars,
            "truncated": truncated,
            "next_offset": next_offset,
            "structured_preview": structured_preview,
            "structured_truncated": structured_text
                .as_ref()
                .map(|value| value.chars().count() > TOOL_RESULT_READ_DEFAULT_CHARS),
        })),
    }
}

fn runtime_default_session_list(path: &StdPath, exists: bool) -> Vec<serde_json::Value> {
    let _ = path;
    vec![serde_json::json!({
        "conversation_id": DEFAULT_CONVERSATION_ID,
        "summary": if exists { "未命名会话" } else { "新对话" },
        "path": null,
        "resource_uri": MCP_RUNTIME_TRANSCRIPT_URI,
        "exists": exists,
        "can_resume": exists,
        "records": 0,
    })]
}

fn is_empty_legacy_default_session(
    session: &serde_json::Value,
    active_conversation_id: &str,
) -> bool {
    if active_conversation_id == DEFAULT_CONVERSATION_ID {
        return false;
    }
    let conversation_id = session
        .get("conversation_id")
        .and_then(|value| value.as_str())
        .unwrap_or_default();
    conversation_id == DEFAULT_CONVERSATION_ID
        && !session
            .get("exists")
            .and_then(|value| value.as_bool())
            .unwrap_or(false)
        && !session
            .get("can_resume")
            .and_then(|value| value.as_bool())
            .unwrap_or(false)
        && session
            .get("records")
            .and_then(|value| value.as_u64())
            .unwrap_or_default()
            == 0
}

fn runtime_session_list_for_active(
    sessions: Vec<serde_json::Value>,
    active_conversation_id: &str,
) -> Vec<serde_json::Value> {
    let mut rows = sessions
        .into_iter()
        .filter(|session| !is_empty_legacy_default_session(session, active_conversation_id))
        .collect::<Vec<_>>();
    let active_exists = rows.iter().any(|session| {
        session
            .get("conversation_id")
            .and_then(|value| value.as_str())
            .is_some_and(|conversation_id| conversation_id == active_conversation_id)
    });
    if !active_exists {
        rows.insert(
            0,
            serde_json::json!({
                "conversation_id": active_conversation_id,
                "summary": "新对话",
                "exists": false,
                "can_resume": false,
                "records": 0,
            }),
        );
    }
    rows
}

fn compact_source_line(message: &muse_core::domain::conversation::Message) -> String {
    let role = message.role.to_string();
    if let Some(tool_name) = message.tool_name.as_deref()
        && message.tool_call_id.is_some()
    {
        return format!(
            "{} tool={} call_id={}: {}",
            role,
            tool_name,
            message.tool_call_id.as_deref().unwrap_or(""),
            truncate_text(&message.content, 800)
        );
    }
    format!("{}: {}", role, truncate_text(&message.content, 800))
}

fn build_rule_compact_summary(messages: &[muse_core::domain::conversation::Message]) -> String {
    let body = messages
        .iter()
        .filter(|message| message.role != Role::System)
        .map(compact_source_line)
        .collect::<Vec<_>>()
        .join("\n");
    if body.trim().is_empty() {
        "当前会话暂无可压缩内容。".to_string()
    } else {
        format!(
            "## 用户需求与对话事实\n{}\n\n## 当前任务状态\n- 已按时间顺序保留关键消息、工具调用和工具结果。\n\n## 下一步\n- 基于最近消息继续推进用户目标。",
            body
        )
    }
}

fn build_compact_prompt(messages: &[muse_core::domain::conversation::Message]) -> String {
    let transcript = messages
        .iter()
        .filter(|message| message.role != Role::System)
        .map(compact_source_line)
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "请把下面的 agent 会话压缩成一份可恢复的 compact summary，必须使用简体中文。\n\n要求：\n1. 保留用户原始需求和约束。\n2. 保留当前任务状态、已经完成的实现、仍未完成的下一步。\n3. 保留关键工具调用、工具结果、审批/取消、错误修复和验证命令。\n4. 不要编造未发生的结果；不知道就写未知。\n5. 输出 Markdown，使用这些小节：用户需求、已完成、关键工具与结果、错误与修复、当前状态、下一步。\n\n会话片段：\n{}",
        if transcript.trim().is_empty() {
            "（暂无可压缩内容）"
        } else {
            transcript.as_str()
        }
    )
}

async fn generate_model_compact_summary(
    provider: &Arc<dyn muse_core::model::provider::ChatModelProvider>,
    messages: &[muse_core::domain::conversation::Message],
) -> Result<String, String> {
    let mut compact_conversation = Conversation::new(
        "你是 agent harness 的会话压缩器，只输出可恢复 compact summary。".to_string(),
        4,
    );
    compact_conversation.add_user_message(build_compact_prompt(messages));
    let reply = provider
        .chat(&compact_conversation)
        .await
        .map_err(|err| format!("{err}"))?;
    let summary = sanitize_assistant_reply(&reply).content.trim().to_string();
    if summary.is_empty() {
        Err("模型返回了空摘要。".to_string())
    } else {
        Ok(summary)
    }
}

fn compact_conversation_in_place(conversation: &mut Conversation, summary: &str) -> usize {
    let original_messages = conversation.messages.clone();
    let system_message = original_messages
        .iter()
        .find(|message| message.role == Role::System)
        .cloned();
    let recent_messages = conversation.recent_turn_frame_messages(6);
    let compact_message = muse_core::domain::conversation::Message {
        role: Role::Assistant,
        content: format!("[会话压缩摘要]\n{summary}"),
        tool_call_id: None,
        tool_name: None,
        tool_arguments: None,
        reasoning_content: None,
    };

    let mut compacted_messages = Vec::new();
    if let Some(system_message) = system_message {
        compacted_messages.push(system_message);
    }
    compacted_messages.push(compact_message);
    compacted_messages.extend(recent_messages);
    conversation.replace_messages(compacted_messages);
    conversation.messages.len()
}

fn apply_session_compaction_result(conversation: &mut Conversation, result: &ToolResult) {
    let Some(summary) = result
        .structured
        .as_ref()
        .and_then(|value| value.get("summary"))
        .and_then(|value| value.as_str())
    else {
        return;
    };
    compact_conversation_in_place(conversation, summary);
}

async fn tool_session_compact(
    state: &Arc<AppState>,
    provider: &Arc<dyn muse_core::model::provider::ChatModelProvider>,
    conversation: &Conversation,
) -> ToolResult {
    let conversation_id = active_conversation_id(state);
    let original_messages = conversation.messages.clone();
    let original_len = original_messages.len();
    let fallback_summary = build_rule_compact_summary(&original_messages);
    let (summary, strategy, error) =
        match generate_model_compact_summary(provider, &original_messages).await {
            Ok(summary) => (summary, "model", None),
            Err(err) => (fallback_summary, "rule_fallback", Some(err)),
        };

    let mut compacted = conversation.clone();
    let compacted_len = compact_conversation_in_place(&mut compacted, &summary);

    report_transcript_failure(
        append_transcript_record(
            state,
            "compact_summary",
            serde_json::json!({
                "conversation_id": conversation_id,
                "summary": summary,
                "strategy": strategy,
                "error": error,
            }),
        )
        .await,
    );
    ToolResult {
        status: ToolResultStatus::Success,
        content: format!(
            "已压缩当前会话：{original_len} 条消息 -> {compacted_len} 条消息。\n{summary}"
        ),
        structured: Some(serde_json::json!({
            "summary": summary,
            "strategy": strategy,
            "error": error,
            "original_messages": original_len,
            "compacted_messages": compacted_len,
        })),
    }
}

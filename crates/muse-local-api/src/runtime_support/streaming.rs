//! WebSocket、流式协议和运行时共享辅助。

use super::*;
use muse_core::domain::memory::{
    MEMORY_DELETE_TOOL_NAME, MEMORY_MUTATE_TOOL_NAME, MEMORY_QUERY_TOOL_NAME, MemoryChangeType,
    MemoryDeleteParams, MemoryDeleteReceipt, MemoryErrorCode, MemoryMutateParams,
    MemoryMutationReceipt, MemoryMutationReceiptState, MemoryQueryPageReceipt, MemoryQueryParams,
};

/// 建立情绪广播 WebSocket 连接。
pub(crate) async fn handle_ws(
    ws: WebSocketUpgrade,
    headers: HeaderMap,
    State(state): State<Arc<AppState>>,
    Extension(security): Extension<Arc<LocalApiSecurity>>,
    Query(query): Query<WsTicketQuery>,
) -> Response {
    let protocol_valid = headers
        .get(header::SEC_WEBSOCKET_PROTOCOL)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value
                .split(',')
                .any(|protocol| protocol.trim() == "muse.runtime.v1")
        });
    if !protocol_valid {
        return (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: "WebSocket 必须使用子协议 muse.runtime.v1。".to_string(),
            }),
        )
            .into_response();
    }
    let Some(ticket) = query.ticket else {
        return (
            StatusCode::UNAUTHORIZED,
            Json(ErrorResponse {
                error: "WebSocket 握手缺少单次票据。".to_string(),
            }),
        )
            .into_response();
    };
    if !security.consume_ws_ticket(&ticket).await {
        return (
            StatusCode::UNAUTHORIZED,
            Json(ErrorResponse {
                error: "WebSocket 票据无效、已过期或已被消费。".to_string(),
            }),
        )
            .into_response();
    }
    ws.protocols(["muse.runtime.v1"])
        .on_upgrade(move |socket| ws_handler(socket, state))
        .into_response()
}

pub(super) async fn ws_handler(mut socket: WebSocket, state: Arc<AppState>) {
    let mut rx = state.emotion_tx.subscribe();

    let ready = serde_json::json!({"type":"ready","model":"dafeng"}).to_string();
    let _ = socket.send(WsMsg::Text(ready.into())).await;

    loop {
        tokio::select! {
            emotion = rx.recv() => {
                if let Ok(emotion) = emotion {
                    let payload = serde_json::json!({"type":"emotion","emotion":emotion}).to_string();
                    if socket.send(WsMsg::Text(payload.into())).await.is_err() {
                        break;
                    }
                }
            }
            msg = socket.recv() => {
                match msg {
                    Some(Ok(WsMsg::Close(_))) | None => break,
                    _ => {}
                }
            }
        }
    }
}

#[derive(Default)]
pub(super) struct EmotionPrefixState {
    resolved: bool,
    buffer: String,
}

pub(super) enum PrefixParseResult {
    Pending,
    Resolved {
        emotion: Option<PersonaEmotionEffect>,
        text: String,
    },
}

pub(super) enum ToolCallPrefixParseResult {
    Pending,
    Text(String),
    ToolCall { call: ToolCall, text: String },
}

pub(super) enum RuntimeModelItem {
    AssistantMessage {
        content: String,
        reasoning_content: Option<String>,
    },
    ToolCall {
        call: ToolCall,
        reasoning_content: Option<String>,
    },
}

pub(super) struct StreamedTurn {
    pub(super) items: Vec<RuntimeModelItem>,
    pub(super) usage: Option<ProviderTokenUsage>,
    pub(super) emotion_candidate: Option<PersonaEmotionEffect>,
}

impl EmotionPrefixState {
    pub(super) fn push_chunk(&mut self, chunk: &str) -> PrefixParseResult {
        if self.resolved {
            return PrefixParseResult::Resolved {
                emotion: None,
                text: chunk.to_string(),
            };
        }

        self.buffer.push_str(chunk);
        if !strip_leading_reasoning_blocks(&mut self.buffer) {
            return PrefixParseResult::Pending;
        }

        let trimmed = self.buffer.trim_start();

        if trimmed.is_empty() {
            return PrefixParseResult::Pending;
        }

        if is_partial_reasoning_tag(trimmed) {
            return PrefixParseResult::Pending;
        }

        if !trimmed.starts_with('{') {
            self.resolved = true;
            return PrefixParseResult::Resolved {
                emotion: None,
                text: std::mem::take(&mut self.buffer),
            };
        }

        if let Some((emotion, consumed)) = parse_emotion_json_prefix(trimmed) {
            self.resolved = true;
            let text = trimmed[consumed..]
                .trim_start_matches(|ch: char| ch.is_whitespace())
                .to_string();
            self.buffer.clear();
            return PrefixParseResult::Resolved { emotion, text };
        }

        let Some(line_end) = trimmed.find('\n') else {
            return PrefixParseResult::Pending;
        };

        let first_line = &trimmed[..line_end];
        let emotion = parse_emotion_json(first_line);

        self.resolved = true;

        if let Some(emotion) = emotion {
            let text = trimmed[line_end + 1..].to_string();
            self.buffer.clear();
            return PrefixParseResult::Resolved {
                emotion: Some(emotion),
                text,
            };
        }

        PrefixParseResult::Resolved {
            emotion: None,
            text: std::mem::take(&mut self.buffer),
        }
    }

    pub(super) fn finish(&mut self) -> Option<String> {
        if self.resolved || self.buffer.is_empty() {
            return None;
        }

        self.resolved = true;
        let sanitized = sanitize_assistant_reply(&std::mem::take(&mut self.buffer));
        if sanitized.content.is_empty() {
            None
        } else {
            Some(sanitized.content)
        }
    }

    pub(super) fn mark_resolved(&mut self) {
        self.resolved = true;
        self.buffer.clear();
    }
}

#[derive(Default)]
pub(super) struct ToolCallPrefixState {
    suppressed: bool,
    buffer: String,
}

impl ToolCallPrefixState {
    pub(super) fn push_chunk(&mut self, chunk: &str) -> ToolCallPrefixParseResult {
        if self.suppressed {
            return ToolCallPrefixParseResult::Text(String::new());
        }

        self.buffer.push_str(chunk);
        self.drain_ready(false)
    }

    pub(super) fn finish(&mut self) -> ToolCallPrefixParseResult {
        if self.suppressed {
            self.buffer.clear();
            return ToolCallPrefixParseResult::Text(String::new());
        }

        self.drain_ready(true)
    }

    fn drain_ready(&mut self, finalizing: bool) -> ToolCallPrefixParseResult {
        if self.buffer.is_empty() {
            return ToolCallPrefixParseResult::Pending;
        }

        let Some(start) = find_tool_call_json_start(&self.buffer) else {
            return ToolCallPrefixParseResult::Text(std::mem::take(&mut self.buffer));
        };

        if start > 0 {
            let text = self.buffer[..start].to_string();
            self.buffer = self.buffer[start..].to_string();

            if let Some((call, _consumed)) = parse_tool_call_json_prefix(&self.buffer) {
                self.suppressed = true;
                self.buffer.clear();
                return ToolCallPrefixParseResult::ToolCall { call, text };
            }

            return ToolCallPrefixParseResult::Text(text);
        }

        if let Some((call, _consumed)) = parse_tool_call_json_prefix(&self.buffer) {
            self.suppressed = true;
            self.buffer.clear();
            return ToolCallPrefixParseResult::ToolCall {
                call,
                text: String::new(),
            };
        }

        if finalizing {
            ToolCallPrefixParseResult::Text(std::mem::take(&mut self.buffer))
        } else {
            ToolCallPrefixParseResult::Pending
        }
    }
}

pub(super) fn parse_emotion_json(text: &str) -> Option<PersonaEmotionEffect> {
    let value = serde_json::from_str::<serde_json::Value>(text).ok()?;
    parse_emotion_candidate(value)
}

pub(super) fn parse_emotion_json_prefix(
    text: &str,
) -> Option<(Option<PersonaEmotionEffect>, usize)> {
    let consumed = json_object_prefix_len(text)?;
    let value = serde_json::from_str::<serde_json::Value>(&text[..consumed]).ok()?;
    value.get("emotion")?;
    Some((parse_emotion_candidate(value), consumed))
}

pub(super) fn parse_emotion_candidate(value: serde_json::Value) -> Option<PersonaEmotionEffect> {
    let candidate = serde_json::from_value::<PersonaEmotionEffect>(value).ok()?;
    candidate.validate().ok()?;
    Some(candidate)
}

pub(super) fn parse_tool_call_json_prefix(text: &str) -> Option<(ToolCall, usize)> {
    let consumed = json_object_prefix_len(text)?;
    let call = muse_core::domain::tool::ToolRegistry::parse_tool_call(&text[..consumed])?;
    Some((call, consumed))
}

pub(super) fn find_tool_call_json_start(text: &str) -> Option<usize> {
    for (index, ch) in text.char_indices() {
        if ch == '{' && looks_like_tool_call_json_start(&text[index..]) {
            return Some(index);
        }
    }
    None
}

pub(super) fn looks_like_tool_call_json_start(text: &str) -> bool {
    let Some(rest) = text.strip_prefix('{') else {
        return false;
    };
    let rest = rest.trim_start();
    [
        "\"tool_call\"",
        "\"tool_calls\"",
        "\"function_call\"",
        "\"tool_use\"",
        "\"function\"",
        "\"tool_name\"",
        "\"action\"",
        "\"name\"",
        "\"type\"",
    ]
    .iter()
    .any(|prefix| rest.starts_with(prefix))
}

pub(super) fn json_object_prefix_len(text: &str) -> Option<usize> {
    if !text.starts_with('{') {
        return None;
    }

    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for (index, ch) in text.char_indices() {
        if in_string {
            if escaped {
                escaped = false;
                continue;
            }
            match ch {
                '\\' => escaped = true,
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }

        match ch {
            '"' => in_string = true,
            '{' => depth += 1,
            '}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some(index + ch.len_utf8());
                }
            }
            _ => {}
        }
    }

    None
}

pub(super) fn sanitize_assistant_reply(raw: &str) -> SanitizedAssistantReply {
    let stripped = strip_reasoning_blocks(raw);
    let trimmed = stripped.trim_start();
    if let Some((_emotion, consumed)) = parse_emotion_json_prefix(trimmed) {
        return SanitizedAssistantReply {
            content: trimmed[consumed..]
                .trim_start_matches(|ch: char| ch.is_whitespace())
                .to_string(),
        };
    }

    SanitizedAssistantReply {
        content: trimmed.to_string(),
    }
}

pub(super) fn strip_reasoning_blocks(text: &str) -> String {
    let mut output = text.to_string();
    for tag in ["think", "thinking"] {
        strip_named_tag_blocks(&mut output, tag);
    }
    output
}

pub(super) fn strip_named_tag_blocks(text: &mut String, tag: &str) {
    let open_tag = format!("<{tag}>");
    let close_tag = format!("</{tag}>");
    loop {
        let lower = text.to_ascii_lowercase();
        let Some(start) = lower.find(&open_tag) else {
            break;
        };
        let search_from = start + open_tag.len();
        if let Some(end) = lower[search_from..].find(&close_tag) {
            let end = search_from + end + close_tag.len();
            text.replace_range(start..end, "");
        } else {
            text.replace_range(start.., "");
            break;
        }
    }
}

pub(super) fn strip_leading_reasoning_blocks(buffer: &mut String) -> bool {
    loop {
        let leading_ws = buffer.len() - buffer.trim_start().len();
        let trimmed = &buffer[leading_ws..];
        if trimmed.is_empty() {
            return true;
        }

        let Some((open_len, close_tag)) = leading_reasoning_tag(trimmed) else {
            return true;
        };

        let search_from = leading_ws + open_len;
        let lower = buffer.to_ascii_lowercase();
        let Some(end) = lower[search_from..].find(close_tag) else {
            return false;
        };
        let remove_end = search_from + end + close_tag.len();
        buffer.replace_range(leading_ws..remove_end, "");
    }
}

pub(super) fn leading_reasoning_tag(trimmed: &str) -> Option<(usize, &'static str)> {
    let lower = trimmed.to_ascii_lowercase();
    if lower.starts_with("<think>") {
        Some(("<think>".len(), "</think>"))
    } else if lower.starts_with("<thinking>") {
        Some(("<thinking>".len(), "</thinking>"))
    } else {
        None
    }
}

pub(super) fn is_partial_reasoning_tag(trimmed: &str) -> bool {
    let lower = trimmed.to_ascii_lowercase();
    ["<think>", "<thinking>"]
        .iter()
        .any(|tag| tag.starts_with(&lower) && lower.len() < tag.len())
}

pub(super) async fn refresh_mcp_tool_catalog_if_needed(
    state: &Arc<AppState>,
    active_persona: Option<&Persona>,
) -> mcp::McpToolCatalog {
    let snapshot = {
        let mut store = state.user_config.lock().await;
        if let Err(error) = store.refresh_from_disk() {
            tracing::warn!(error = %error, "刷新 MCP config.toml 工具目录失败");
        }
        store.mcp_runtime_snapshot()
    };
    let scope =
        mcp::EffectiveMcpScope::from_policy(active_persona.map(|persona| &persona.mcp_policy));
    let config_hash = format!("{}:{}", snapshot.config_hash(), scope.cache_key());
    {
        if let Some(catalog) = state
            .runtime_service
            .fresh_mcp_tool_catalog(&config_hash, Duration::from_millis(MCP_TOOL_CATALOG_TTL_MS))
            .await
        {
            return catalog;
        }
    }

    let catalog = mcp::discover_external_mcp_tools_for_scope_with_manager(
        &snapshot,
        &scope,
        state.runtime_service.mcp_client_manager(),
    )
    .await;
    state
        .runtime_service
        .replace_mcp_tool_catalog(catalog.clone())
        .await;
    catalog
}

pub(crate) async fn runtime_tool_defs_for_policy(
    state: &Arc<AppState>,
    active_persona: Option<&Persona>,
) -> Vec<ToolDef> {
    let preset = current_runtime_mode_state(state).tool_preset();
    ToolRegistry::filter_definitions_for_preset(
        runtime_frozen_tool_defs_for_policy(state, active_persona).await,
        preset,
    )
}

pub(super) async fn runtime_frozen_tool_defs_for_policy(
    state: &Arc<AppState>,
    active_persona: Option<&Persona>,
) -> Vec<ToolDef> {
    let catalog = refresh_mcp_tool_catalog_if_needed(state, active_persona).await;
    runtime_frozen_tool_defs_for_policy_with_catalog(state, active_persona, &catalog)
}

pub(super) fn runtime_frozen_tool_defs_for_policy_with_catalog(
    state: &Arc<AppState>,
    active_persona: Option<&Persona>,
    catalog: &mcp::McpToolCatalog,
) -> Vec<ToolDef> {
    let mut defs = state.tools.list_definitions();
    defs.retain(|definition| !matches!(definition.name.as_str(), "use_skill" | "skill"));
    defs.extend(catalog.tool_defs());
    if let Some(persona) = active_persona {
        match persona.mcp_policy.mode {
            muse_core::domain::persona::ResourcePolicyMode::Inherit => {}
            muse_core::domain::persona::ResourcePolicyMode::Disabled => {
                defs.retain(|definition| !definition.category.starts_with("mcp:"));
            }
            muse_core::domain::persona::ResourcePolicyMode::AllowList => {
                defs.retain(|definition| {
                    let Some(server) = definition.category.strip_prefix("mcp:") else {
                        return true;
                    };
                    persona
                        .mcp_policy
                        .allowed_servers
                        .iter()
                        .any(|allowed| allowed == server)
                });
            }
        }
        if matches!(
            persona.skill_policy.mode,
            muse_core::domain::persona::ResourcePolicyMode::Disabled
        ) {
            defs.retain(|definition| {
                !matches!(definition.name.as_str(), "load_skill" | "create_skill")
            });
        }
    }
    let mut defs = ToolRegistry::filter_definitions_for_policy(
        defs,
        active_persona.map(|persona| &persona.tool_policy),
    );
    defs.sort_by(|left, right| left.name.cmp(&right.name));
    defs
}

pub(super) async fn current_runtime_system_prompt(state: &Arc<AppState>) -> String {
    let active_persona = current_active_persona(state).await;
    let tool_defs = runtime_tool_defs_for_policy(state, active_persona.as_ref()).await;
    build_runtime_system_prompt_with_mode_state(
        &state.config,
        active_persona.as_ref(),
        &tool_defs,
        current_runtime_mode_state(state),
    )
}

pub(super) fn update_conversation_system_prompt(conv: &mut Conversation, system_prompt: String) {
    if let Some(message) = conv
        .messages
        .iter_mut()
        .find(|message| message.role == Role::System)
    {
        message.content = system_prompt;
        return;
    }
    conv.messages.insert(
        0,
        muse_core::domain::conversation::Message {
            role: Role::System,
            content: system_prompt,
            tool_call_id: None,
            tool_name: None,
            tool_arguments: None,
            reasoning_content: None,
        },
    );
}

pub(crate) async fn current_active_persona(state: &Arc<AppState>) -> Option<Persona> {
    let personas = state.personas.lock().await;
    personas.active_persona().cloned()
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(super) struct RuntimeTranscriptReplayStats {
    pub(super) source_records: usize,
    pub(super) records: usize,
    pub(super) restored_messages: usize,
    pub(super) skipped_records: usize,
    pub(super) latest_task_state: Option<String>,
    pub(super) latest_todos: Option<Vec<RuntimeTodoItem>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ReplayTurnOutcome {
    Committed,
    Aborted,
    InterruptedWithEffects,
}

pub(super) async fn persist_fork_snapshot(
    state: &Arc<AppState>,
    conversation_id: &str,
    source_conversation_id: &str,
    before_user_message_index: Option<usize>,
    conversation: &Conversation,
    inherited_todos: &[RuntimeTodoItem],
) -> Result<(), String> {
    let messages = conversation
        .messages
        .iter()
        .filter(|message| message.role != Role::System)
        .cloned()
        .collect::<Vec<_>>();
    let snapshot_material = serde_json::to_vec(&serde_json::json!({
        "source_conversation_id": source_conversation_id,
        "before_user_message_index": before_user_message_index,
        "messages": &messages,
        "todos": inherited_todos,
    }))
    .map_err(|err| format!("序列化分叉会话快照失败：{err}"))?;
    let snapshot_id = format!("{:x}", Sha256::digest(&snapshot_material));
    let store = state
        .runtime_service
        .session_store()
        .await
        .map_err(|err| err.to_string())?;
    let existing = store
        .events_for_conversation(conversation_id)
        .await
        .map_err(|err| format!("检查分叉会话快照失败：{err}"))?;
    if existing.iter().any(|event| {
        event.kind == "session_fork_snapshot"
            && event.payload["snapshot_id"].as_str() == Some(snapshot_id.as_str())
    }) {
        return Ok(());
    }
    if existing
        .iter()
        .any(|event| event.kind != muse_runtime::session_metadata::SESSION_METADATA_EVENT_KIND)
    {
        return Err(format!(
            "分叉目标会话 `{conversation_id}` 已存在其他事件，已拒绝覆盖。"
        ));
    }
    state
        .runtime_service
        .session_repository()
        .await
        .map_err(|err| err.to_string())?
        .append_event(
            conversation_id,
            None,
            "session_fork_snapshot",
            serde_json::json!({
                "conversation_id": conversation_id,
                "source_conversation_id": source_conversation_id,
                "before_user_message_index": before_user_message_index,
                "snapshot_id": snapshot_id,
                "messages": messages,
                "todos": inherited_todos,
            }),
        )
        .await
        .map_err(|err| format!("持久化分叉会话快照失败：{err}"))?;
    Ok(())
}

pub(super) async fn load_conversation_from_runtime_transcript(
    state: &Arc<AppState>,
    conversation_id: &str,
    before_user_message_index: Option<usize>,
) -> Result<(Conversation, RuntimeTranscriptReplayStats), String> {
    let content = match read_runtime_transcript_for_conversation(state, conversation_id).await {
        Ok(content) => content,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Err("当前没有可恢复的 runtime transcript。".to_string());
        }
        Err(err) => return Err(format!("读取 runtime transcript 失败：{err}")),
    };
    let system_prompt = current_runtime_system_prompt(state).await;
    let result = replay_runtime_transcript_lines(
        system_prompt,
        state.config.agent.max_history,
        &content,
        conversation_id,
        before_user_message_index,
    );
    if result.1.source_records == 0 {
        return Err(format!(
            "会话 `{conversation_id}` 不存在或没有 transcript 记录。"
        ));
    }
    Ok(result)
}

pub(super) fn replay_runtime_transcript_lines(
    system_prompt: String,
    max_history: usize,
    content: &str,
    conversation_id: &str,
    before_user_message_index: Option<usize>,
) -> (Conversation, RuntimeTranscriptReplayStats) {
    let mut conv = Conversation::new(system_prompt, max_history);
    let mut stats = RuntimeTranscriptReplayStats::default();
    let mut user_message_index = 0usize;
    let mut latest_task_state_payload = None::<serde_json::Value>;
    let mut latest_todo_state_payload = None::<serde_json::Value>;
    let mut records = Vec::new();
    for line in content
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        let Ok(record) = serde_json::from_str::<serde_json::Value>(line) else {
            stats.skipped_records += 1;
            continue;
        };
        records.push(record);
    }

    let mut turn_outcomes = HashMap::<String, ReplayTurnOutcome>::new();
    let mut effect_started = HashMap::<String, serde_json::Value>::new();
    for record in &records {
        let null_payload = serde_json::Value::Null;
        let payload = record.get("payload").unwrap_or(&null_payload);
        if transcript_record_conversation_id(record, payload) != conversation_id {
            continue;
        }
        let Some(turn_id) = transcript_record_turn_id(record, payload) else {
            continue;
        };
        if record.get("kind").and_then(|value| value.as_str()) == Some("turn_effect_started") {
            effect_started
                .entry(turn_id.clone())
                .or_insert_with(|| payload.clone());
        }
        let Some(outcome) = transcript_record_turn_outcome(record, payload) else {
            continue;
        };
        turn_outcomes
            .entry(turn_id)
            .and_modify(|current| {
                if *current != outcome {
                    // 冲突终态按最保守的 aborted 处理，绝不恢复不确定工作副本。
                    *current = ReplayTurnOutcome::Aborted;
                }
            })
            .or_insert(outcome);
    }
    let inferred_interrupted_turns = effect_started
        .keys()
        .filter(|turn_id| !turn_outcomes.contains_key(*turn_id))
        .cloned()
        .collect::<BTreeSet<_>>();
    for turn_id in &inferred_interrupted_turns {
        turn_outcomes.insert(turn_id.clone(), ReplayTurnOutcome::InterruptedWithEffects);
    }

    let mut restored_fork_snapshots = BTreeSet::<String>::new();
    let mut recovered_interrupted_turns = BTreeSet::<String>::new();
    for record in records {
        let Some(kind) = record.get("kind").and_then(|value| value.as_str()) else {
            stats.skipped_records += 1;
            continue;
        };
        let null_payload = serde_json::Value::Null;
        let payload = record.get("payload").unwrap_or(&null_payload);
        if transcript_record_conversation_id(&record, payload) != conversation_id {
            continue;
        }
        stats.source_records += 1;
        let turn_id = transcript_record_turn_id(&record, payload);
        let turn_outcome = turn_id
            .as_ref()
            .and_then(|turn_id| turn_outcomes.get(turn_id))
            .copied();
        let is_v3 = record
            .get("schema_version")
            .and_then(|value| value.as_str())
            == Some(muse_runtime::session::SESSION_EVENT_SCHEMA_VERSION);

        if turn_outcome == Some(ReplayTurnOutcome::InterruptedWithEffects)
            && let Some(turn_id) = turn_id.as_ref()
            && inferred_interrupted_turns.contains(turn_id)
            && recovered_interrupted_turns.insert(turn_id.clone())
        {
            let recovery_payload = effect_started.get(turn_id).unwrap_or(payload);
            conv.add_assistant_message(interrupted_turn_recovery_context(recovery_payload));
            stats.restored_messages += 1;
            trim_replayed_conversation(&mut conv);
        }

        if matches!(
            kind,
            "turn_committed" | "turn_aborted" | "turn_interrupted_with_effects"
        ) {
            stats.records += 1;
            if turn_outcome == Some(ReplayTurnOutcome::InterruptedWithEffects)
                && let Some(turn_id) = turn_id
                && recovered_interrupted_turns.insert(turn_id)
            {
                conv.add_assistant_message(interrupted_turn_recovery_context(payload));
                stats.restored_messages += 1;
                trim_replayed_conversation(&mut conv);
            }
            continue;
        }

        if turn_id.is_some()
            && (turn_outcome.is_some_and(|outcome| outcome != ReplayTurnOutcome::Committed)
                || (turn_outcome.is_none() && is_v3))
        {
            stats.records += 1;
            stats.skipped_records += 1;
            continue;
        }

        if kind == "session_fork_snapshot" {
            stats.records += 1;
            let snapshot_id = payload
                .get("snapshot_id")
                .and_then(|value| value.as_str())
                .map(ToString::to_string)
                .unwrap_or_else(|| format!("{:x}", Sha256::digest(payload.to_string())));
            if !restored_fork_snapshots.insert(snapshot_id) {
                continue;
            }
            match restore_fork_snapshot_messages(
                &mut conv,
                payload,
                before_user_message_index,
                &mut user_message_index,
            ) {
                Some((restored, reached_cutoff)) => {
                    stats.restored_messages = stats.restored_messages.saturating_add(restored);
                    if !reached_cutoff {
                        latest_todo_state_payload = Some(payload.clone());
                    }
                    if reached_cutoff {
                        break;
                    }
                }
                None => stats.skipped_records += 1,
            }
            continue;
        }

        if kind == "user" {
            if before_user_message_index == Some(user_message_index) {
                break;
            }
            user_message_index += 1;
        }
        if kind == "task_state" {
            latest_task_state_payload = Some(payload.clone());
            stats.records += 1;
            continue;
        }
        if kind == "todo_state" {
            latest_todo_state_payload = Some(payload.clone());
            stats.records += 1;
            continue;
        }
        stats.records += 1;
        if replay_runtime_transcript_record(&mut conv, kind, payload) {
            stats.restored_messages += 1;
            trim_replayed_conversation(&mut conv);
        } else {
            stats.skipped_records += 1;
        }
    }
    if let Some(payload) = latest_task_state_payload
        && let Some(summary) = task_state_summary_for_replay(&payload)
    {
        conv.add_assistant_message(format!("[任务状态恢复]\n{summary}"));
        stats.latest_task_state = Some(summary);
        stats.restored_messages += 1;
        trim_replayed_conversation(&mut conv);
    }
    stats.latest_todos = latest_todo_state_payload
        .as_ref()
        .and_then(todo_state_items_for_replay);

    (conv, stats)
}

pub(super) fn transcript_record_turn_id(
    record: &serde_json::Value,
    payload: &serde_json::Value,
) -> Option<String> {
    payload
        .get("turn_id")
        .or_else(|| record.get("turn_id"))
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

pub(super) fn transcript_record_turn_outcome(
    record: &serde_json::Value,
    payload: &serde_json::Value,
) -> Option<ReplayTurnOutcome> {
    let value = record
        .get("turn_outcome")
        .or_else(|| payload.get("outcome"))
        .and_then(|value| value.as_str())
        .or_else(|| record.get("kind").and_then(|kind| kind.as_str()))?;
    match value {
        "committed" | "turn_committed" => Some(ReplayTurnOutcome::Committed),
        "aborted" | "turn_aborted" => Some(ReplayTurnOutcome::Aborted),
        "interrupted_with_effects" | "turn_interrupted_with_effects" => {
            Some(ReplayTurnOutcome::InterruptedWithEffects)
        }
        _ => None,
    }
}

pub(super) fn interrupted_turn_recovery_context(payload: &serde_json::Value) -> String {
    let message = payload
        .get("message")
        .and_then(|value| value.as_str())
        .unwrap_or("上一回合在工具进入真实派发后异常终止，外部状态可能已经改变。");
    format!(
        "[结构化恢复上下文]\n{}",
        serde_json::json!({
            "kind": "turn_interrupted_with_effects",
            "message": message,
            "tool": payload.get("tool").and_then(|value| value.as_str()),
            "call_id": payload.get("call_id").and_then(|value| value.as_str()),
            "instruction": "上一回合可能已改变外部状态；继续前先核实现状，不要盲目重复工具调用。"
        })
    )
}

pub(super) fn restore_fork_snapshot_messages(
    conv: &mut Conversation,
    payload: &serde_json::Value,
    before_user_message_index: Option<usize>,
    user_message_index: &mut usize,
) -> Option<(usize, bool)> {
    let messages = serde_json::from_value::<Vec<Message>>(payload.get("messages")?.clone()).ok()?;
    if messages.iter().any(|message| message.role == Role::System) {
        return None;
    }
    let mut restored = Vec::new();
    let mut reached_cutoff = false;
    for message in messages {
        if message.role == Role::User {
            if before_user_message_index == Some(*user_message_index) {
                reached_cutoff = true;
                break;
            }
            *user_message_index = user_message_index.saturating_add(1);
        }
        restored.push(message);
    }
    let restored_count = restored.len();
    let mut candidate = conv.clone();
    candidate.messages.extend(restored);
    candidate.trim_to_budget();
    if candidate.validate_tool_protocol().is_err() {
        return None;
    }
    *conv = candidate;
    Some((restored_count, reached_cutoff))
}

pub(super) fn transcript_record_conversation_id(
    record: &serde_json::Value,
    payload: &serde_json::Value,
) -> String {
    payload
        .get("conversation_id")
        .or_else(|| record.get("conversation_id"))
        .and_then(|value| value.as_str())
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(DEFAULT_CONVERSATION_ID)
        .to_string()
}

pub(super) fn replay_runtime_transcript_record(
    conv: &mut Conversation,
    kind: &str,
    payload: &serde_json::Value,
) -> bool {
    match kind {
        "user" => {
            let Some(content) = transcript_payload_string(payload, "content") else {
                return false;
            };
            conv.add_user_message(content);
            true
        }
        "assistant" => {
            let Some(content) = transcript_payload_string(payload, "content") else {
                return false;
            };
            conv.add_assistant_message_with_reasoning(
                content,
                transcript_payload_string(payload, "reasoning_content"),
            );
            true
        }
        "tool_call" => {
            let Some(call_id) = transcript_payload_string(payload, "call_id") else {
                return false;
            };
            let Some(tool_name) = transcript_payload_string(payload, "tool") else {
                return false;
            };
            let arguments = replay_tool_arguments(payload);
            conv.add_assistant_tool_call_with_reasoning(
                call_id,
                tool_name,
                arguments,
                transcript_payload_string(payload, "reasoning_content"),
            );
            true
        }
        "tool_result" => {
            let Some(call_id) = transcript_payload_string(payload, "call_id") else {
                return false;
            };
            let Some(tool_name) = transcript_payload_string(payload, "tool") else {
                return false;
            };
            let canonical_result = payload.get("canonical_result").unwrap_or(payload);
            let success = canonical_result
                .get("success")
                .and_then(|value| value.as_bool())
                .unwrap_or(true);
            let result = if is_memory_session_redacted_tool(&tool_name) {
                // 去正文收据只保证 Tool Call/Result 配对合法；恢复上下文只保留
                // 无正文占位，绝不重新执行旧记忆 Tool。
                ToolResult {
                    status: ToolResultStatus::from_success(success),
                    content: MEMORY_SESSION_REPLAY_PLACEHOLDER.to_string(),
                    structured: None,
                }
            } else {
                ToolResult {
                    status: ToolResultStatus::from_success(success),
                    content: canonical_result
                        .get("content")
                        .and_then(|value| value.as_str())
                        .unwrap_or_default()
                        .to_string(),
                    structured: canonical_result
                        .get("structured")
                        .filter(|value| !value.is_null())
                        .cloned(),
                }
            };
            let content = tool_result_content_for_model(&tool_name, &result);
            conv.add_tool_result_with_status(call_id, tool_name, content, !result.is_success());
            true
        }
        "compact_summary" => {
            let Some(summary) = transcript_payload_string(payload, "summary") else {
                return false;
            };
            conv.add_assistant_message(format!("[会话压缩摘要]\n{summary}"));
            true
        }
        "task_state" => true,
        "todo_state" => true,
        _ => false,
    }
}

pub(super) fn replay_tool_arguments(payload: &serde_json::Value) -> serde_json::Value {
    // 记忆 Tool 的 canonical 参数是去正文收据，不是模型原始调用参数；与旧治理
    // 版本的 redacted 摘要一样降级为协议合法空对象，恢复后绝不重新执行。
    if payload
        .get("tool")
        .and_then(|value| value.as_str())
        .is_some_and(is_memory_session_redacted_tool)
    {
        return serde_json::json!({});
    }
    if let Some(arguments) = payload.get("canonical_arguments") {
        return arguments.clone();
    }
    let Some(arguments) = payload.get("arguments") else {
        return serde_json::json!({});
    };
    // 治理版本之前的事件把参数名摘要放在 arguments 中。摘要无法恢复真实
    // 调用，回放时使用协议合法的空对象，避免把 redacted/argument_names
    // 伪装成模型原始工具参数。
    if arguments.get("redacted").and_then(|value| value.as_bool()) == Some(true)
        && arguments.get("argument_names").is_some()
    {
        serde_json::json!({})
    } else {
        arguments.clone()
    }
}

pub(super) fn todo_state_items_for_replay(
    payload: &serde_json::Value,
) -> Option<Vec<RuntimeTodoItem>> {
    let todos = payload.get("todos")?.clone();
    serde_json::from_value::<Vec<RuntimeTodoItem>>(todos).ok()
}

pub(super) fn task_state_summary_for_replay(payload: &serde_json::Value) -> Option<String> {
    let status = payload
        .get("status")
        .and_then(|value| value.as_str())
        .unwrap_or("unknown");
    let summary = payload
        .get("summary")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())?;
    let active_tool = payload
        .get("active_tool")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let updated_at = payload
        .get("updated_at")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    let tool_line = active_tool
        .map(|tool| format!("\n当前工具：{tool}"))
        .unwrap_or_default();
    let time_line = if updated_at.is_empty() {
        String::new()
    } else {
        format!("\n更新时间：{updated_at}")
    };
    Some(format!(
        "状态：{status}\n摘要：{}{tool_line}{time_line}",
        truncate_text(summary, 1_200)
    ))
}

pub(super) fn transcript_payload_string(payload: &serde_json::Value, key: &str) -> Option<String> {
    payload
        .get(key)
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

pub(super) fn trim_replayed_conversation(conv: &mut Conversation) {
    conv.trim_to_budget();
}

pub(super) async fn reset_conversation_for_active_persona(state: &Arc<AppState>) {
    let system_prompt = current_runtime_system_prompt(state).await;
    let mut conv = state.runtime_service.lock_conversation().await;
    *conv = muse_core::domain::conversation::Conversation::new(
        system_prompt,
        state.config.agent.max_history,
    );
    drop(conv);
    replace_runtime_todos(state, Vec::new()).await;
    let conversation_id = next_runtime_session_id();
    let _ = set_active_conversation_id(state, &conversation_id);
    if let Err(error) = initialize_new_session_approval_mode(state, &conversation_id).await {
        tracing::warn!(%error, "初始化新会话审批模式失败，继续使用手动审批");
        let _ = set_active_approval_mode(state, ApprovalModePreset::Manual, 0);
    }
}

pub(super) fn next_runtime_id(prefix: &str) -> String {
    let seq = RUNTIME_ID_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{prefix}-{}-{seq}", chrono::Utc::now().timestamp_millis())
}

#[cfg(test)]
pub(super) fn sanitize_runtime_path_component(value: &str) -> String {
    let sanitized = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '-'
            }
        })
        .collect::<String>();
    let sanitized = sanitized.trim_matches('-');
    if sanitized.is_empty() {
        DEFAULT_CONVERSATION_ID.to_string()
    } else {
        sanitized.to_string()
    }
}

#[derive(Default)]
pub(super) struct RuntimeSessionDeleteOutcome {
    pub(super) deleted_records: usize,
    pub(super) deleted_files: usize,
}

#[cfg(test)]
pub(super) fn remove_runtime_transcript_for_conversation(
    content: &str,
    conversation_id: &str,
) -> (String, usize) {
    let mut removed = 0usize;
    let mut kept = Vec::new();
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let should_remove = serde_json::from_str::<serde_json::Value>(trimmed)
            .ok()
            .map(|record| {
                let null_payload = serde_json::Value::Null;
                let payload = record.get("payload").unwrap_or(&null_payload);
                transcript_record_conversation_id(&record, payload) == conversation_id
            })
            .unwrap_or(false);
        if should_remove {
            removed += 1;
        } else {
            kept.push(line);
        }
    }
    let mut next_content = kept.join("\n");
    if !next_content.is_empty() {
        next_content.push('\n');
    }
    (next_content, removed)
}

pub(super) async fn delete_runtime_transcript_for_conversation(
    state: &Arc<AppState>,
    conversation_id: &str,
) -> std::io::Result<RuntimeSessionDeleteOutcome> {
    let repository = state
        .runtime_service
        .session_repository()
        .await
        .map_err(std::io::Error::other)?;
    let deleted_records = repository
        .delete_conversation(conversation_id)
        .await
        .map_err(std::io::Error::other)?;
    Ok(RuntimeSessionDeleteOutcome {
        deleted_records,
        deleted_files: usize::from(deleted_records > 0),
    })
}

pub(super) async fn read_runtime_transcript_for_conversation(
    state: &Arc<AppState>,
    conversation_id: &str,
) -> std::io::Result<String> {
    let store = state
        .runtime_service
        .session_store()
        .await
        .map_err(std::io::Error::other)?;
    let events = store
        .events_for_conversation(conversation_id)
        .await
        .map_err(std::io::Error::other)?;
    if events.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "当前会话没有 transcript。",
        ));
    }
    Ok(session_events_as_jsonl(&events))
}

pub(super) async fn append_transcript_record(
    state: &Arc<AppState>,
    kind: &str,
    payload: serde_json::Value,
) -> Result<(), String> {
    let conversation_id = transcript_record_conversation_id(&serde_json::Value::Null, &payload);
    let turn_id = payload
        .get("turn_id")
        .and_then(|value| value.as_str())
        .map(ToString::to_string);
    let repository = state
        .runtime_service
        .session_repository()
        .await
        .map_err(|err| err.to_string())?;
    repository
        .append_event(conversation_id, turn_id, kind, payload)
        .await
        .map(|_| ())
        .map_err(|err| format!("写入 v3 会话事件 `{kind}` 失败：{err}"))
}

pub(super) async fn durable_turn_event<Write, WriteFuture>(write: Write) -> Result<(), String>
where
    Write: FnOnce() -> WriteFuture,
    WriteFuture: Future<Output = Result<(), String>>,
{
    write().await
}

pub(super) async fn append_required_turn_event(
    state: &Arc<AppState>,
    kind: &str,
    payload: serde_json::Value,
) -> Result<(), String> {
    durable_turn_event(|| append_transcript_record(state, kind, payload)).await
}

pub(super) fn session_events_as_jsonl(events: &[muse_runtime::session::SessionEventV3]) -> String {
    let mut content = String::new();
    for event in events {
        let record = event.legacy_record.clone().unwrap_or_else(|| {
            serde_json::json!({
                "schema_version": event.schema_version,
                "event_id": event.event_id,
                "sequence": event.commit_seq,
                "commit_seq": event.commit_seq,
                "conversation_id": event.conversation_id,
                "turn_id": event.turn_id,
                "turn_outcome": event.turn_outcome,
                "time": event.time,
                "kind": event.kind,
                "payload": event.payload,
            })
        });
        content.push_str(&record.to_string());
        content.push('\n');
    }
    content
}

pub(super) async fn append_task_state_record(
    state: &Arc<AppState>,
    turn: &TurnContext,
    status: &str,
    summary: impl Into<String>,
    active_tool: Option<&str>,
) -> Result<(), String> {
    append_transcript_record(
        state,
        "task_state",
        serde_json::json!({
            "conversation_id": turn.conversation_id.clone(),
            "turn_id": turn.turn_id.clone(),
            "status": status,
            "summary": summary.into(),
            "active_tool": active_tool,
            "updated_at": chrono::Utc::now().to_rfc3339(),
        }),
    )
    .await
}

pub(super) fn workspace_root() -> Result<PathBuf, String> {
    std::env::current_dir().map_err(|err| format!("无法获取当前工作区：{err}"))
}

#[derive(serde::Deserialize, serde::Serialize)]
pub(super) struct RuntimeWorkspaceRootsFile {
    #[serde(default)]
    pub(super) roots: Vec<String>,
    #[serde(default = "default_runtime_permission_mode")]
    pub(super) permission_mode: String,
    #[serde(default = "default_runtime_sandbox_mode")]
    pub(super) sandbox_mode: String,
}

pub(super) fn default_runtime_permission_mode() -> String {
    "request_approval".to_string()
}

pub(super) fn default_runtime_sandbox_mode() -> String {
    "workspace_write".to_string()
}

pub(super) fn normalize_runtime_permission_mode(value: &str) -> Option<String> {
    match value {
        "request_approval" | "approve_for_me" | "full_access" => Some(value.to_string()),
        _ => None,
    }
}

pub(super) fn normalize_runtime_sandbox_mode(value: &str) -> Option<String> {
    match value {
        "workspace_write" | "danger_full_access" => Some(value.to_string()),
        _ => None,
    }
}

pub(super) fn custom_allowed_file_roots_path() -> PathBuf {
    muse_core::config::Config::resolved_data_dir()
        .unwrap_or_else(|_| PathBuf::from(".muse"))
        .join("harness")
        .join("allowed-file-roots.json")
}

pub(super) fn load_runtime_harness_config() -> RuntimeWorkspaceRootsFile {
    let path = custom_allowed_file_roots_path();
    let Ok(content) = std::fs::read_to_string(path) else {
        return RuntimeWorkspaceRootsFile {
            roots: Vec::new(),
            permission_mode: default_runtime_permission_mode(),
            sandbox_mode: default_runtime_sandbox_mode(),
        };
    };
    let mut config = serde_json::from_str::<RuntimeWorkspaceRootsFile>(&content).unwrap_or(
        RuntimeWorkspaceRootsFile {
            roots: Vec::new(),
            permission_mode: default_runtime_permission_mode(),
            sandbox_mode: default_runtime_sandbox_mode(),
        },
    );
    if normalize_runtime_permission_mode(&config.permission_mode).is_none() {
        config.permission_mode = default_runtime_permission_mode();
    }
    if normalize_runtime_sandbox_mode(&config.sandbox_mode).is_none() {
        config.sandbox_mode = default_runtime_sandbox_mode();
    }
    if config.permission_mode == "full_access" {
        config.permission_mode = default_runtime_permission_mode();
        config.sandbox_mode = default_runtime_sandbox_mode();
    }
    config
}

pub(super) fn default_new_session_approval_preset() -> ApprovalModePreset {
    match load_runtime_harness_config().permission_mode.as_str() {
        "approve_for_me" => ApprovalModePreset::Auto,
        _ => ApprovalModePreset::Manual,
    }
}

pub(super) async fn save_runtime_harness_config(
    config: &RuntimeWorkspaceRootsFile,
) -> Result<(), String> {
    let path = custom_allowed_file_roots_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .await
            .map_err(|err| format!("创建工具工作区配置目录失败：{err}"))?;
    }
    let mut items = config.roots.to_vec();
    items.sort();
    items.dedup();
    let content = serde_json::to_string_pretty(&RuntimeWorkspaceRootsFile {
        roots: items,
        permission_mode: config.permission_mode.clone(),
        sandbox_mode: config.sandbox_mode.clone(),
    })
    .map_err(|err| format!("序列化工具工作区配置失败：{err}"))?;
    fs::write(path, content)
        .await
        .map_err(|err| format!("保存工具工作区配置失败：{err}"))
}

pub(super) fn runtime_workspace_root_info(
    path: PathBuf,
    kind: &str,
    label: &str,
    removable: bool,
) -> RuntimeWorkspaceRootInfo {
    let exists = path.exists();
    let writable = exists
        && path.is_dir()
        && std::fs::metadata(&path)
            .map(|metadata| !metadata.permissions().readonly())
            .unwrap_or(false);
    RuntimeWorkspaceRootInfo {
        path: path.display().to_string(),
        label: label.to_string(),
        kind: kind.to_string(),
        exists,
        writable,
        removable,
    }
}

pub(super) fn runtime_workspaces_response() -> Result<RuntimeWorkspacesResponse, String> {
    let config = load_runtime_harness_config();
    let workspace = workspace_root()?
        .canonicalize()
        .map_err(|err| format!("无法解析当前项目工作区：{err}"))?;
    let roots = vec![runtime_workspace_root_info(
        workspace,
        "project",
        "当前项目工作区",
        false,
    )];
    Ok(RuntimeWorkspacesResponse {
        roots,
        permission_mode: config.permission_mode,
        sandbox_mode: config.sandbox_mode,
    })
}

pub(super) fn runtime_workspace_error_response(
    message: String,
) -> (StatusCode, Json<ErrorResponse>) {
    (
        StatusCode::BAD_REQUEST,
        Json(ErrorResponse { error: message }),
    )
}

pub(super) fn allowed_file_roots() -> Result<Vec<PathBuf>, String> {
    Ok(vec![workspace_root()?])
}

pub(super) fn canonical_runtime_roots() -> Result<Vec<PathBuf>, String> {
    allowed_file_roots()?
        .into_iter()
        .map(|root| {
            root.canonicalize()
                .map_err(|err| format!("无法解析运行时工作区权限根：{err}"))
        })
        .collect::<Result<Vec<_>, _>>()
}

pub(super) fn set_active_approval_mode(
    state: &Arc<AppState>,
    preset: ApprovalModePreset,
    revision: u64,
) -> Result<u64, String> {
    let current_roots = state
        .runtime_service
        .execution_policy()
        .map(|policy| policy.allowed_roots)
        .unwrap_or_default();
    let roots = if current_roots.is_empty() {
        canonical_runtime_roots()?
    } else {
        current_roots
    };
    state
        .runtime_service
        .set_execution_policy(FrozenExecutionPolicy::from_preset(preset, roots, revision))
        .map_err(|error| error.to_string())
}

pub(super) async fn restore_active_approval_mode(
    state: &Arc<AppState>,
    conversation_id: &str,
) -> Result<(), String> {
    let repository = state
        .runtime_service
        .session_repository()
        .await
        .map_err(|error| error.to_string())?;
    let saved = repository
        .approval_mode_for_resume(conversation_id)
        .await
        .map_err(|error| error.to_string())?;
    set_active_approval_mode(state, saved.preset, saved.revision)?;
    Ok(())
}

pub(super) async fn initialize_new_session_approval_mode(
    state: &Arc<AppState>,
    conversation_id: &str,
) -> Result<(), String> {
    let preset = default_new_session_approval_preset();
    set_active_approval_mode(state, preset, 0)?;
    if preset == ApprovalModePreset::Manual {
        return Ok(());
    }
    let active_persona = {
        let personas = state.personas.lock().await;
        personas.active_persona().cloned()
    }
    .ok_or_else(|| "当前没有活动角色，无法保存新会话审批默认值。".to_string())?;
    let repository = state
        .runtime_service
        .session_repository()
        .await
        .map_err(|error| error.to_string())?;
    repository
        .ensure_persona_binding(
            conversation_id,
            &active_persona.id,
            &active_persona.name,
            &active_persona.version,
        )
        .await
        .map_err(|error| error.to_string())?;
    let saved = repository
        .update_approval_mode(conversation_id, preset)
        .await
        .map_err(|error| error.to_string())?;
    set_active_approval_mode(state, saved.preset, saved.revision)?;
    Ok(())
}

pub(super) fn should_require_tool_approval(
    policy: &FrozenExecutionPolicy,
    call: &ToolCall,
    _risk: &str,
    default_requires_approval: bool,
) -> bool {
    // 外部 MCP 的本地信任已经冻结在 ToolDef 中。全局完全访问模式不得再放宽
    // MCP 写操作；可信只读工具则保持本地策略给出的免审结果。
    if mcp::is_external_mcp_tool_name(&call.name) {
        return default_requires_approval;
    }
    match policy.approval_policy {
        ApprovalPolicy::Never => false,
        ApprovalPolicy::OnRequest => default_requires_approval,
    }
}

pub(super) fn can_bypass_workspace_boundary(
    policy: &FrozenExecutionPolicy,
    allow_approved_external_path: bool,
) -> bool {
    allow_approved_external_path || policy.permission_profile == PermissionProfile::DangerFullAccess
}

pub(super) fn resolve_workspace_path(
    policy: &FrozenExecutionPolicy,
    raw: &str,
    for_write: bool,
    allow_approved_external_path: bool,
) -> Result<PathBuf, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err("路径不能为空。".to_string());
    }
    let bypass_workspace_boundary =
        can_bypass_workspace_boundary(policy, allow_approved_external_path);
    let workspace = workspace_root()?;
    let candidate = crate::platform::path::expand_user_path(raw, &workspace);
    if candidate
        .components()
        .any(|part| matches!(part, std::path::Component::ParentDir))
    {
        return Err("路径不能包含上级目录跳转。".to_string());
    }
    if for_write
        && candidate.components().any(|part| {
            let value = part.as_os_str().to_string_lossy();
            value == ".git" || value == ".agent-vp-data"
        })
        && !bypass_workspace_boundary
    {
        return Err("默认禁止写入 .git 或 .agent-vp-data 目录。".to_string());
    }

    let check_path = if candidate.exists() {
        candidate.clone()
    } else if for_write {
        let mut ancestor = candidate
            .parent()
            .map(PathBuf::from)
            .ok_or_else(|| "路径缺少父目录。".to_string())?;
        while !ancestor.exists() {
            ancestor = ancestor
                .parent()
                .map(PathBuf::from)
                .ok_or_else(|| "路径没有可用的已存在父目录。".to_string())?;
        }
        ancestor
    } else {
        candidate
            .parent()
            .map(PathBuf::from)
            .ok_or_else(|| "路径缺少父目录。".to_string())?
    };
    let check_canon = check_path
        .canonicalize()
        .map_err(|err| format!("路径不在当前工作区内或父目录不存在：{err}"))?;
    if !bypass_workspace_boundary
        && !policy
            .allowed_roots
            .iter()
            .any(|root| check_canon.starts_with(root))
    {
        return Err("路径超出当前工作区，已拒绝。".to_string());
    }
    Ok(candidate)
}

pub(super) fn path_requires_workspace_boundary_approval(
    policy: &FrozenExecutionPolicy,
    raw: &str,
    for_write: bool,
) -> bool {
    if can_bypass_workspace_boundary(policy, false) {
        return false;
    }
    matches!(
        resolve_workspace_path(policy, raw, for_write, false),
        Err(err) if err == "路径超出当前工作区，已拒绝。"
    )
}

pub(super) fn tool_arg_string(arguments: &serde_json::Value, key: &str) -> Option<String> {
    arguments
        .get(key)
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

pub(super) fn parse_ask_user_question_request(
    arguments: &serde_json::Value,
) -> Result<AskUserQuestionRequest, ToolResult> {
    let request =
        serde_json::from_value::<AskUserQuestionRequest>(arguments.clone()).map_err(|err| {
            tool_failed(
                format!("ask_user_question 参数格式错误：{err}"),
                "invalid_arguments",
            )
        })?;
    if request.questions.is_empty() || request.questions.len() > 4 {
        return Err(tool_failed(
            "ask_user_question 需要提供 1 到 4 个问题。",
            "invalid_question_count",
        ));
    }

    let mut question_texts = BTreeSet::new();
    for question in &request.questions {
        let text = question.question.trim();
        if text.is_empty() {
            return Err(tool_failed("问题文本不能为空。", "empty_question"));
        }
        if !question_texts.insert(text.to_string()) {
            return Err(tool_failed("问题文本不能重复。", "duplicate_question"));
        }
        if question.header.trim().is_empty() {
            return Err(tool_failed("问题 header 不能为空。", "empty_header"));
        }
        if question.header.chars().count() > 12 {
            return Err(tool_failed(
                "问题 header 最多 12 个字符。",
                "header_too_long",
            ));
        }
        if question.options.len() < 2 || question.options.len() > 4 {
            return Err(tool_failed(
                "每个问题需要 2 到 4 个选项。",
                "invalid_option_count",
            ));
        }
        let mut option_labels = BTreeSet::new();
        for option in &question.options {
            let label = option.label.trim();
            if label.is_empty() {
                return Err(tool_failed("选项 label 不能为空。", "empty_option_label"));
            }
            let normalized = label.to_ascii_lowercase();
            if matches!(normalized.as_str(), "other" | "others") || label == "其他" {
                return Err(tool_failed(
                    "不要在选项中提供 Other/其他，前端会自动提供自定义输入。",
                    "reserved_other_option",
                ));
            }
            if !option_labels.insert(label.to_string()) {
                return Err(tool_failed(
                    "同一问题内的选项 label 不能重复。",
                    "duplicate_option_label",
                ));
            }
            if option.description.trim().is_empty() {
                return Err(tool_failed(
                    "选项 description 不能为空。",
                    "empty_option_description",
                ));
            }
        }
    }

    Ok(request)
}

pub(super) fn parse_todo_write_request(
    arguments: &serde_json::Value,
) -> Result<TodoWriteRequest, ToolResult> {
    let request = serde_json::from_value::<TodoWriteRequest>(arguments.clone()).map_err(|err| {
        tool_failed(
            format!("todo_write 参数格式错误：{err}"),
            "invalid_arguments",
        )
    })?;
    if request.todos.is_empty() {
        return Err(tool_failed(
            "todo_write 至少需要提供 1 个任务。",
            "empty_todos",
        ));
    }
    if request.todos.len() > 50 {
        return Err(tool_failed(
            "todo_write 单次最多提交 50 个任务。",
            "too_many_todos",
        ));
    }

    let mut ids = BTreeSet::new();
    for todo in &request.todos {
        if todo.content.trim().is_empty() {
            return Err(tool_failed("todo 内容不能为空。", "empty_todo_content"));
        }
        if !matches!(todo.status.trim(), "pending" | "in_progress" | "completed") {
            return Err(tool_failed(
                "todo status 只支持 pending、in_progress、completed。",
                "invalid_todo_status",
            ));
        }
        if let Some(priority) = todo.priority.as_deref()
            && !priority.trim().is_empty()
            && !matches!(priority.trim(), "high" | "medium" | "low")
        {
            return Err(tool_failed(
                "todo priority 只支持 high、medium、low。",
                "invalid_todo_priority",
            ));
        }
        if let Some(id) = todo
            .id
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            && !ids.insert(id.to_string())
        {
            return Err(tool_failed("todo id 不能重复。", "duplicate_todo_id"));
        }
    }

    Ok(request)
}

pub(super) fn parse_exit_plan_mode_request(
    arguments: &serde_json::Value,
) -> Result<ExitPlanModeRequest, ToolResult> {
    let request =
        serde_json::from_value::<ExitPlanModeRequest>(arguments.clone()).map_err(|err| {
            tool_failed(
                format!("exit_plan_mode 参数格式错误：{err}"),
                "invalid_arguments",
            )
        })?;
    if request.plan_summary.trim().is_empty() {
        return Err(tool_failed("计划摘要不能为空。", "empty_plan_summary"));
    }
    if request.steps.len() > 30 {
        return Err(tool_failed("计划步骤最多 30 项。", "too_many_plan_steps"));
    }
    if request.risks.len() > 20 {
        return Err(tool_failed("计划风险最多 20 项。", "too_many_plan_risks"));
    }
    Ok(request)
}

pub(super) fn parse_agent_task_request(
    arguments: &serde_json::Value,
) -> Result<AgentTaskRequest, ToolResult> {
    match arguments.get("task") {
        Some(serde_json::Value::String(task)) if !task.trim().is_empty() => {}
        Some(serde_json::Value::String(_)) | None => {
            return Err(tool_failed("agent task 不能为空。", "missing_task"));
        }
        Some(_) => {
            return Err(tool_failed(
                "agent task 必须是非空字符串。",
                "invalid_arguments",
            ));
        }
    }
    let request = serde_json::from_value::<AgentTaskRequest>(arguments.clone())
        .map_err(|err| tool_failed(format!("agent 参数格式错误：{err}"), "invalid_arguments"))?;
    if let Some(priority) = request.priority.as_deref()
        && !priority.trim().is_empty()
        && !matches!(priority.trim(), "high" | "medium" | "low")
    {
        return Err(tool_failed(
            "agent priority 只支持 high、medium、low。",
            "invalid_agent_priority",
        ));
    }
    Ok(request)
}

pub(super) fn mode_state_json(mode_state: RuntimeModeState) -> serde_json::Value {
    serde_json::json!({
        "mode": mode_state.mode.as_str(),
        "focus_phase": mode_state.focus_phase.as_str(),
        "tool_preset": mode_state.tool_preset().as_str(),
    })
}

pub(super) fn runtime_mode_state_from_tool_result(result: &ToolResult) -> Option<RuntimeModeState> {
    let value = result
        .structured
        .as_ref()?
        .get("runtime_mode_state")?
        .clone();
    serde_json::from_value(value).ok()
}

pub(super) fn todo_items_from_request(request: TodoWriteRequest) -> Vec<RuntimeTodoItem> {
    request
        .todos
        .into_iter()
        .enumerate()
        .map(|(index, item)| RuntimeTodoItem {
            id: item
                .id
                .map(|id| id.trim().to_string())
                .filter(|id| !id.is_empty())
                .unwrap_or_else(|| format!("todo-{}", index + 1)),
            content: item.content.trim().to_string(),
            status: item.status.trim().to_string(),
            priority: item
                .priority
                .map(|priority| priority.trim().to_string())
                .filter(|priority| !priority.is_empty()),
        })
        .collect()
}

pub(super) fn todo_items_text(todos: &[RuntimeTodoItem]) -> String {
    if todos.is_empty() {
        return "当前没有活跃任务。".to_string();
    }
    todos
        .iter()
        .map(|todo| {
            let priority = todo
                .priority
                .as_deref()
                .map(|value| format!(" · priority={value}"))
                .unwrap_or_default();
            format!(
                "- [{}] {}：{}{}",
                todo.status, todo.id, todo.content, priority
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub(super) fn exit_plan_confirmation_request(plan: &ExitPlanModeRequest) -> AskUserQuestionRequest {
    let mut description = truncate_text(plan.plan_summary.trim(), 180);
    if !plan.next_action.as_deref().unwrap_or("").trim().is_empty()
        && let Some(next_action) = plan.next_action.as_deref()
    {
        description.push_str("\n确认后第一步：");
        description.push_str(next_action.trim());
    }
    AskUserQuestionRequest {
        questions: vec![AskUserQuestionItem {
            question: "是否确认这份计划并进入专注工作预设？".to_string(),
            header: "计划确认".to_string(),
            options: vec![
                AskUserQuestionOption {
                    label: "确认计划（推荐）".to_string(),
                    description,
                },
                AskUserQuestionOption {
                    label: "继续调整计划".to_string(),
                    description: "保持计划预设，要求模型继续修改计划。".to_string(),
                },
                AskUserQuestionOption {
                    label: "取消计划".to_string(),
                    description: "不进入专注工作预设，本轮计划作废。".to_string(),
                },
            ],
            multi_select: false,
        }],
    }
}

pub(super) fn exit_plan_answer_label(decision: &UserQuestionDecision) -> Option<String> {
    let answers = decision.answers.as_ref()?.as_object()?;
    answers.values().find_map(|answer| match answer {
        serde_json::Value::String(value) => Some(value.clone()),
        serde_json::Value::Array(values) => values
            .iter()
            .find_map(|value| value.as_str().map(ToString::to_string)),
        _ => None,
    })
}

pub(super) fn ask_user_question_answers_text(
    answers: &serde_json::Value,
    annotations: Option<&serde_json::Value>,
) -> String {
    let Some(map) = answers.as_object() else {
        return "用户没有提供可读取的答案。".to_string();
    };
    let mut rows = Vec::new();
    for (question, answer) in map {
        let answer_text = match answer {
            serde_json::Value::String(value) => value.clone(),
            serde_json::Value::Array(values) => values
                .iter()
                .filter_map(|value| value.as_str().map(ToString::to_string))
                .collect::<Vec<_>>()
                .join(", "),
            value => value.to_string(),
        };
        let mut row = format!("\"{question}\"=\"{answer_text}\"");
        if let Some(notes) = annotations
            .and_then(|value| value.get(question))
            .and_then(|value| value.get("notes"))
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            row.push_str(&format!("，用户补充：{notes}"));
        }
        rows.push(row);
    }
    if rows.is_empty() {
        "用户没有提供可读取的答案。".to_string()
    } else {
        rows.join("；")
    }
}

pub(super) fn require_text_argument(call: &ToolCall, key: &str) -> Result<(), ToolResult> {
    if tool_arg_string(&call.arguments, key).is_some() {
        Ok(())
    } else {
        Err(tool_failed(
            format!("{} 缺少 {key} 参数。", call.name),
            &format!("missing_{key}"),
        ))
    }
}

pub(super) fn require_string_argument(
    call: &ToolCall,
    key: &str,
    allow_empty: bool,
) -> Result<(), ToolResult> {
    let Some(value) = call.arguments.get(key).and_then(|value| value.as_str()) else {
        return Err(tool_failed(
            format!("{} 缺少 {key} 参数。", call.name),
            &format!("missing_{key}"),
        ));
    };
    if !allow_empty && value.trim().is_empty() {
        return Err(tool_failed(
            format!("{} 缺少 {key} 参数。", call.name),
            &format!("missing_{key}"),
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CommandRiskLevel {
    Medium,
    High,
}

impl CommandRiskLevel {
    fn label(self) -> &'static str {
        match self {
            CommandRiskLevel::Medium => "中风险",
            CommandRiskLevel::High => "高风险",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct CommandRiskFinding {
    code: &'static str,
    label: &'static str,
    detail: &'static str,
    level: CommandRiskLevel,
}

pub(super) fn command_risk_findings(command: &str) -> Vec<CommandRiskFinding> {
    let normalized = command.to_lowercase();
    let mut findings = Vec::new();
    let mut push_once = |finding: CommandRiskFinding| {
        if !findings
            .iter()
            .any(|item: &CommandRiskFinding| item.code == finding.code)
        {
            findings.push(finding);
        }
    };

    if normalized.contains("rm -rf")
        || normalized.contains("rm -fr")
        || normalized.contains("rm -r ")
        || normalized.contains("rm -f ")
        || normalized.contains("rmdir ")
        || normalized.contains("remove-item")
    {
        push_once(CommandRiskFinding {
            code: "destructive_delete",
            label: "删除风险",
            detail: "命令可能删除文件或目录。",
            level: CommandRiskLevel::High,
        });
    }

    if normalized.contains(" >")
        || normalized.contains("> ")
        || normalized.contains(" tee ")
        || normalized.contains("truncate ")
        || normalized.contains("dd if=")
        || normalized.contains("dd ")
    {
        push_once(CommandRiskFinding {
            code: "overwrite_or_truncate",
            label: "覆盖风险",
            detail: "命令可能覆盖、截断或重写文件内容。",
            level: CommandRiskLevel::Medium,
        });
    }

    if normalized.contains("curl ")
        || normalized.contains("wget ")
        || normalized.contains("git clone ")
        || normalized.contains("fetch ")
    {
        push_once(CommandRiskFinding {
            code: "network_download",
            label: "网络下载",
            detail: "命令会从网络拉取内容。",
            level: CommandRiskLevel::Medium,
        });
    }

    if (normalized.contains("curl ") || normalized.contains("wget "))
        && (normalized.contains("| sh")
            || normalized.contains("| bash")
            || normalized.contains("| zsh")
            || normalized.contains("bash <")
            || normalized.contains("sh <"))
    {
        push_once(CommandRiskFinding {
            code: "download_then_execute",
            label: "下载后执行",
            detail: "命令可能把网络内容直接交给 shell 执行。",
            level: CommandRiskLevel::High,
        });
    }

    if normalized.contains("sudo ")
        || normalized.contains("chmod ")
        || normalized.contains("chown ")
        || normalized.contains("chgrp ")
        || normalized.contains("launchctl ")
    {
        push_once(CommandRiskFinding {
            code: "privilege_or_permission_change",
            label: "权限变更",
            detail: "命令可能提权、修改权限或影响系统服务。",
            level: CommandRiskLevel::High,
        });
    }

    if normalized.contains("nohup ")
        || normalized.contains("disown")
        || normalized.contains("setsid ")
        || normalized.ends_with('&')
        || normalized.contains(" & ")
    {
        push_once(CommandRiskFinding {
            code: "background_process",
            label: "后台常驻",
            detail: "命令可能启动后台或长期运行进程。",
            level: CommandRiskLevel::Medium,
        });
    }

    if normalized.contains("npm install")
        || normalized.contains("pnpm install")
        || normalized.contains("yarn add")
        || normalized.contains("pip install")
        || normalized.contains("cargo install")
        || normalized.contains("brew install")
    {
        push_once(CommandRiskFinding {
            code: "dependency_install",
            label: "依赖安装",
            detail: "命令会修改依赖、缓存或本机安装内容。",
            level: CommandRiskLevel::Medium,
        });
    }

    findings
}

pub(super) fn command_risk_safety_notes(command: &str) -> Vec<String> {
    command_risk_findings(command)
        .into_iter()
        .map(|finding| {
            format!(
                "{} · {}：{}",
                finding.level.label(),
                finding.label,
                finding.detail
            )
        })
        .collect()
}

pub(super) fn tool_arg_limit(
    arguments: &serde_json::Value,
    default_value: usize,
    max_value: usize,
) -> usize {
    arguments
        .get("limit")
        .and_then(|value| value.as_u64())
        .and_then(|value| usize::try_from(value).ok())
        .filter(|value| *value > 0)
        .map(|value| value.min(max_value))
        .unwrap_or(default_value)
}

pub(super) fn truncate_text(text: &str, max_chars: usize) -> String {
    let mut out = String::new();
    for ch in text.chars().take(max_chars) {
        out.push(ch);
    }
    if text.chars().count() > max_chars {
        out.push_str("\n...（内容已截断）");
    }
    out
}

pub(super) fn truncate_text_head_tail(text: &str, max_chars: usize) -> String {
    let total_chars = text.chars().count();
    if total_chars <= max_chars {
        return text.to_string();
    }
    const MARKER: &str = "\n...（中间内容已截断）...\n";
    let marker_chars = MARKER.chars().count();
    let body_chars = max_chars.saturating_sub(marker_chars);
    let head_chars = body_chars.div_ceil(2);
    let tail_chars = body_chars.saturating_sub(head_chars);
    let head = text.chars().take(head_chars).collect::<String>();
    let mut tail = text.chars().rev().take(tail_chars).collect::<Vec<_>>();
    tail.reverse();
    format!("{head}{MARKER}{}", tail.into_iter().collect::<String>())
}

pub(super) fn truncate_text_with_flag(text: &str, max_chars: usize) -> (String, bool) {
    let truncated = text.chars().count() > max_chars;
    (truncate_text(text, max_chars), truncated)
}

pub(super) const TOOL_RESULT_EXTERNALIZE_MIN_CHARS: usize = 16_000;
pub(super) const TOOL_RESULT_CONTENT_PREVIEW_CHARS: usize = 4_000;
pub(super) const TOOL_RESULT_STRUCTURED_PREVIEW_CHARS: usize = 4_000;
pub(super) const TOOL_RESULT_READ_DEFAULT_CHARS: usize = 20_000;
pub(super) const TOOL_RESULT_READ_MAX_CHARS: usize = 50_000;

pub(super) fn tool_result_json_text(value: &Option<serde_json::Value>) -> Option<String> {
    value.as_ref().map(|structured| {
        serde_json::to_string_pretty(structured).unwrap_or_else(|_| structured.to_string())
    })
}

pub(super) fn should_externalize_tool_result(tool_name: &str, result: &ToolResult) -> bool {
    let externalizable_tool = mcp::is_external_mcp_tool_name(tool_name)
        || matches!(
            tool_name,
            "file_read"
                | "file_list"
                | "file_search"
                | "command_run"
                | "web_fetch"
                | "web_search"
                | "mcp_list_resources"
                | "mcp_list_resource_templates"
                | "mcp_read_resource"
                | "session_read"
        );
    if !externalizable_tool {
        return false;
    }
    let content_chars = result.content.chars().count();
    let structured_chars = tool_result_json_text(&result.structured)
        .map(|value| value.chars().count())
        .unwrap_or(0);
    content_chars > TOOL_RESULT_EXTERNALIZE_MIN_CHARS
        || structured_chars > TOOL_RESULT_EXTERNALIZE_MIN_CHARS
}

pub(super) async fn maybe_externalize_tool_result(
    turn: &TurnContext,
    call: &ToolCall,
    result: &ToolResult,
) -> ToolResult {
    if !should_externalize_tool_result(&call.name, result) {
        return result.clone();
    }

    let result_id = next_runtime_id("tool-result");
    let created_at = chrono::Utc::now().to_rfc3339();
    let content_chars = result.content.chars().count();
    let structured_text = tool_result_json_text(&result.structured);
    let structured_chars = structured_text
        .as_ref()
        .map(|value| value.chars().count())
        .unwrap_or(0);
    let archive_record = serde_json::json!({
        "schema_version": 1,
        "result_id": result_id,
        "created_at": created_at,
        "conversation_id": turn.conversation_id.clone(),
        "turn_id": turn.turn_id.clone(),
        "call_id": call.call_id.clone(),
        "tool": call.name.clone(),
        "success": result.is_success(),
        "content_chars": content_chars,
        "structured_chars": structured_chars,
        "content": result.content.clone(),
        "structured": result.structured.clone(),
    });

    if crate::runtime_support::tool_adapters::result_archive::write_new(
        &result_id,
        archive_record.to_string(),
    )
    .await
    .is_err()
    {
        return result.clone();
    }

    let result_ref = serde_json::json!({
        "result_id": result_id,
        "tool": call.name.clone(),
        "call_id": call.call_id.clone(),
        "content_chars": content_chars,
        "structured_chars": structured_chars,
        "created_at": created_at,
        "read_tool": "tool_result_read",
        "resource_uri": format!("muse://tool-result/{result_id}"),
    });
    externalized_tool_result(result, result_ref, structured_text.as_deref())
}

pub(super) fn externalized_tool_result(
    result: &ToolResult,
    result_ref: serde_json::Value,
    structured_text: Option<&str>,
) -> ToolResult {
    let preview = truncate_text_head_tail(&result.content, TOOL_RESULT_CONTENT_PREVIEW_CHARS);
    let result_id = result_ref
        .get("result_id")
        .and_then(|value| value.as_str())
        .unwrap_or("unknown");
    let content = if preview.trim().is_empty() {
        format!("工具结果过大，已外置归档为 `{result_id}`。需要完整内容时调用 `tool_result_read`。")
    } else {
        format!(
            "{preview}\n\n工具结果过大，完整内容已外置归档为 `{result_id}`。需要更多内容时调用 `tool_result_read`，参数使用 result_id=`{result_id}`。"
        )
    };

    let mut structured = serde_json::Map::new();
    structured.insert("tool_result_ref".to_string(), result_ref);
    if let Some(text) = structured_text.filter(|value| !value.trim().is_empty()) {
        structured.insert(
            "structured_preview".to_string(),
            serde_json::Value::String(truncate_text(text, TOOL_RESULT_STRUCTURED_PREVIEW_CHARS)),
        );
        structured.insert(
            "structured_truncated".to_string(),
            serde_json::Value::Bool(text.chars().count() > TOOL_RESULT_STRUCTURED_PREVIEW_CHARS),
        );
    }
    if let Some(command_output) = result.structured.as_ref().and_then(|value| {
        let stdout = value.get("stdout")?;
        let stderr = value.get("stderr")?;
        Some(serde_json::json!({ "stdout": stdout, "stderr": stderr }))
    }) {
        structured.insert("command_output".to_string(), command_output);
    }

    ToolResult {
        status: result.status.clone(),
        content,
        structured: Some(serde_json::Value::Object(structured)),
    }
}

pub(super) fn slice_text_by_chars(text: &str, offset: usize, limit: usize) -> (String, bool) {
    let total = text.chars().count();
    let body = text.chars().skip(offset).take(limit).collect::<String>();
    let next_offset = offset.saturating_add(body.chars().count());
    (body, next_offset < total)
}

pub(super) fn default_tool_result_content_for_model(result: &ToolResult) -> String {
    let Some(structured) = result.structured.as_ref() else {
        return result.content.clone();
    };
    let structured_text =
        serde_json::to_string_pretty(structured).unwrap_or_else(|_| structured.to_string());
    if structured_text.trim().is_empty() {
        return result.content.clone();
    }
    format!(
        "{}\n\n结构化结果（供模型继续决策）：\n{}",
        result.content,
        truncate_text(&structured_text, 12_000)
    )
}

pub(super) fn tool_result_content_for_model(tool_name: &str, result: &ToolResult) -> String {
    let Some(handler) = runtime_tool_handler(tool_name) else {
        return default_tool_result_content_for_model(result);
    };
    let content = handler.render_result_for_model(result);
    match handler.context_effect(result) {
        Some(effect) => effect.apply_to_model_content(content),
        None => content,
    }
}

pub(super) async fn wait_for_user_question_answer(
    state: &Arc<AppState>,
    tx: Option<&RuntimeSseSender>,
    turn: &TurnContext,
    call: &ToolCall,
    request: &AskUserQuestionRequest,
    cancel_token: &RuntimeTurnCancel,
) -> Result<UserQuestionDecision, ()> {
    let Some(tx) = tx else {
        return Ok(UserQuestionDecision {
            answered: false,
            answers: None,
            annotations: None,
            reason: Some("no_event_channel".to_string()),
        });
    };
    let request_id = next_runtime_id("question");
    let summary = format!("向用户提问：{} 个问题", request.questions.len());
    let (question_tx, question_rx) = oneshot::channel::<UserQuestionDecision>();
    state
        .runtime_service
        .transition_active(&turn.turn_id, RuntimePhase::WaitingUser)
        .map_err(|_| ())?;
    state
        .runtime_service
        .register_pending_user_question(
            request_id.clone(),
            PendingUserQuestion {
                turn_id: turn.turn_id.clone(),
                tool_name: call.name.clone(),
                summary: summary.clone(),
                tx: question_tx,
            },
        )
        .await
        .map_err(|_| ())?;
    let _pending_guard = PendingInteractionGuard::user_question(state, &turn.turn_id, &request_id);
    let questions_value = serde_json::to_value(&request.questions).unwrap_or_else(|_| {
        call.arguments
            .get("questions")
            .cloned()
            .unwrap_or(serde_json::Value::Null)
    });
    append_transcript_record(
        state,
        "user_question_pending",
        serde_json::json!({
            "conversation_id": turn.conversation_id,
            "turn_id": turn.turn_id,
            "request_id": request_id,
            "call_id": call.call_id,
            "tool": call.name,
            "audit_note": "用户问题请求已登记；问题正文和参数未写入审计事件。",
        }),
    )
    .await
    .map_err(|err| {
        tracing::error!(target: "muse::transcript", error = %err, "持久化用户问题失败");
    })?;
    emit_json_event(
        tx,
        runtime_user_question_pending_event(
            &request_id,
            &call.call_id,
            &call.name,
            "模型需要你选择或补充信息后才能继续。",
            &questions_value,
            &call.arguments,
        ),
    )
    .await?;

    let decision = tokio::select! {
        result = question_rx => match result {
            Ok(decision) => decision,
            Err(_) => UserQuestionDecision {
                answered: false,
                answers: None,
                annotations: None,
                reason: Some("question_channel_dropped".to_string()),
            },
        },
        _ = tokio::time::sleep(Duration::from_secs(USER_QUESTION_TIMEOUT_SECS)) => {
            state.runtime_service.remove_pending_user_question(&request_id, &turn.turn_id).await;
            UserQuestionDecision {
                answered: false,
                answers: None,
                annotations: None,
                reason: Some("timeout".to_string()),
            }
        },
        _ = wait_for_turn_cancel(cancel_token) => {
            state.runtime_service.remove_pending_user_question(&request_id, &turn.turn_id).await;
            UserQuestionDecision {
                answered: false,
                answers: None,
                annotations: None,
                reason: Some("turn_cancelled".to_string()),
            }
        },
        _ = tx.closed() => {
            state.runtime_service.remove_pending_user_question(&request_id, &turn.turn_id).await;
            UserQuestionDecision {
                answered: false,
                answers: None,
                annotations: None,
                reason: Some("client_disconnected".to_string()),
            }
        },
    };
    if state.runtime_service.snapshot().is_ok_and(|snapshot| {
        snapshot.turn_id.as_deref() == Some(turn.turn_id.as_str())
            && snapshot.phase == RuntimePhase::WaitingUser
    }) {
        let _ = state
            .runtime_service
            .transition_active(&turn.turn_id, RuntimePhase::Running);
    }
    let reason = decision.reason.as_deref().unwrap_or(if decision.answered {
        "answered"
    } else {
        "cancelled"
    });
    append_transcript_record(
        state,
        "user_question_resolved",
        serde_json::json!({
            "conversation_id": turn.conversation_id,
            "turn_id": turn.turn_id,
            "request_id": request_id,
            "call_id": call.call_id,
            "tool": call.name,
            "answered": decision.answered,
            "audit_note": "用户问题结果已登记；答案、标注和处理说明未写入审计事件。",
        }),
    )
    .await
    .map_err(|err| {
        tracing::error!(target: "muse::transcript", error = %err, "持久化用户问题结果失败");
    })?;
    if !tx.is_closed() {
        let _ = emit_json_event(
            tx,
            runtime_user_question_resolved_event(&request_id, decision.answered, Some(reason)),
        )
        .await;
    }
    Ok(decision)
}

#[derive(Debug, Clone, Copy)]
pub(super) struct RuntimeToolExecutionPolicy {
    pub(super) requires_approval: bool,
    pub(super) requires_workspace_boundary_approval: bool,
    pub(super) is_read_only: bool,
    pub(super) is_mutating: bool,
    pub(super) concurrency_safe: bool,
    pub(super) interrupt_behavior: RuntimeToolInterruptBehavior,
}

impl RuntimeToolExecutionPolicy {
    pub(super) fn unknown() -> Self {
        Self {
            requires_approval: false,
            requires_workspace_boundary_approval: false,
            is_read_only: true,
            is_mutating: false,
            concurrency_safe: true,
            interrupt_behavior: RuntimeToolInterruptBehavior::Block,
        }
    }

    pub(super) fn from_handler(
        handler: Option<&dyn RuntimeToolHandler>,
        call: &ToolCall,
        def: &ToolDef,
        requires_approval: bool,
        requires_workspace_boundary_approval: bool,
    ) -> Self {
        let inferred_read_only = matches!(def.risk, ToolRisk::ReadOnly);
        let is_read_only = handler
            .map(|handler| handler.is_read_only(call))
            .unwrap_or(inferred_read_only);
        let is_mutating = handler
            .map(|handler| handler.is_mutating(call))
            .unwrap_or(!is_read_only);
        let concurrency_safe = handler
            .map(|handler| handler.is_concurrency_safe(call))
            .unwrap_or(is_read_only);
        let interrupt_behavior = handler
            .map(|handler| handler.interrupt_behavior(call))
            .unwrap_or(RuntimeToolInterruptBehavior::Block);

        Self {
            requires_approval,
            requires_workspace_boundary_approval,
            is_read_only,
            is_mutating,
            concurrency_safe,
            interrupt_behavior,
        }
    }
}

pub(super) const PRIVATE_SESSION_SECRET_MARKER: &str = "[MUSE_REDACTED_SECRET]";

/// 日志或只读展示使用的工具请求摘要，不参与 provider 会话恢复。
pub(super) fn tool_request_audit_metadata(arguments: &serde_json::Value) -> serde_json::Value {
    let mut argument_names = arguments
        .as_object()
        .map(|values| values.keys().cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    argument_names.sort();
    serde_json::json!({
        "redacted": true,
        "argument_names": argument_names,
        "audit_note": "该字段仅供审计展示，不得用于模型会话恢复。",
    })
}

pub(super) struct CanonicalSessionToolResult {
    pub(super) content: String,
    pub(super) structured: Option<serde_json::Value>,
    pub(super) context_effect: Option<serde_json::Value>,
}

/// 私有 canonical session 保留工具协议语义，仅清除可识别的密钥字段和值。
/// command、path、URL、查询词、stdout/stderr 与 Web 结果均不能摘要化丢失。
pub(super) fn canonical_session_tool_result(
    result: &ToolResult,
    context_effect: Option<serde_json::Value>,
) -> CanonicalSessionToolResult {
    CanonicalSessionToolResult {
        content: redact_private_session_text(&result.content),
        structured: result
            .structured
            .as_ref()
            .map(sanitize_private_session_value),
        context_effect: context_effect.as_ref().map(sanitize_private_session_value),
    }
}

pub(super) fn canonical_session_tool_arguments(arguments: &serde_json::Value) -> serde_json::Value {
    sanitize_private_session_value(arguments)
}

/// 判断工具名是否属于 F1 冻结的三个记忆 Tool。
///
/// 记忆 Tool 的原始参数与结果在任何 Session 事件、运行日志、诊断或前端事件
/// 记录前必须完成专用去正文转换，不能等通用记录器写入后再补删。
pub(super) fn is_memory_session_redacted_tool(tool_name: &str) -> bool {
    matches!(
        tool_name,
        MEMORY_QUERY_TOOL_NAME | MEMORY_MUTATE_TOOL_NAME | MEMORY_DELETE_TOOL_NAME
    )
}

/// 恢复记忆 Tool 结果时使用的协议合法占位文本，保证配对合法但不携带任何正文。
pub(super) const MEMORY_SESSION_REPLAY_PLACEHOLDER: &str =
    "[记忆工具收据：正文已按 Session 去正文契约移除，恢复时不会重新执行]";

const MEMORY_SESSION_IDENTIFIER_MAX_LEN: usize = 80;

/// Session 只能持久化运行时生成的记忆标识，不能把模型或 Tool 返回的任意字符串
/// 当作“稳定 ID”抄入收据。当前运行时 ID 形态固定为
/// `<prefix>-<13 位毫秒时间戳>-<十进制序号>`。
fn is_runtime_memory_identifier(value: &str, prefix: &str) -> bool {
    if value.is_empty() || value.len() > MEMORY_SESSION_IDENTIFIER_MAX_LEN {
        return false;
    }
    let Some(suffix) = value
        .strip_prefix(prefix)
        .and_then(|value| value.strip_prefix('-'))
    else {
        return false;
    };
    let Some((timestamp, sequence)) = suffix.split_once('-') else {
        return false;
    };
    timestamp.len() == 13
        && timestamp.bytes().all(|byte| byte.is_ascii_digit())
        && !sequence.is_empty()
        && sequence.len() <= 20
        && sequence.bytes().all(|byte| byte.is_ascii_digit())
}

fn is_valid_memory_id(value: &str) -> bool {
    is_runtime_memory_identifier(value, "memory")
}

fn is_valid_memory_revision_id(value: &str) -> bool {
    is_runtime_memory_identifier(value, "memory-rev")
}

fn is_valid_memory_deletion_id(value: &str) -> bool {
    is_runtime_memory_identifier(value, "memory-confirm")
}

/// Provider 生成的原始 call_id 也是自由字符串。记忆事件只保留稳定摘要，以便
/// SSE 与 Session 恢复继续配对，同时避免把正文伪装成调用标识持久化。
fn memory_session_call_id(call_id: &str) -> String {
    format!("memory-call-{:x}", Sha256::digest(call_id.as_bytes()))
}

fn memory_change_type_name(change_type: MemoryChangeType) -> &'static str {
    match change_type {
        MemoryChangeType::Create => "create",
        MemoryChangeType::Update => "update",
        MemoryChangeType::Correct => "correct",
    }
}

fn memory_session_invalid_call_receipt(tool_name: &'static str) -> serde_json::Value {
    serde_json::json!({
        "memory_receipt": tool_name,
        "error_code": MemoryErrorCode::InvalidRequest.as_str(),
    })
}

/// 记忆 Tool 调用的 canonical 去正文收据。
///
/// 只保留调用语义与稳定标识：query 原文、整理后 content、change_reason、
/// 敏感检测输入、游标值和排序分数一律不进入 Session。
pub(super) fn memory_session_call_receipt(
    tool_name: &str,
    arguments: &serde_json::Value,
) -> Option<serde_json::Value> {
    match tool_name {
        MEMORY_QUERY_TOOL_NAME => {
            let Ok(params) = serde_json::from_value::<MemoryQueryParams>(arguments.clone()) else {
                return Some(memory_session_invalid_call_receipt(MEMORY_QUERY_TOOL_NAME));
            };
            if params.validate().is_err()
                || params
                    .memory_id
                    .as_ref()
                    .is_some_and(|memory_id| !is_valid_memory_id(&memory_id.0))
            {
                return Some(memory_session_invalid_call_receipt(MEMORY_QUERY_TOOL_NAME));
            }
            // 查询类型只描述调用形态，便于审计区分新查询、按 ID 查询和游标续查；
            // 游标值属于不透明定位符，按契约不落盘。
            let query_kind = if params.cursor.is_some() {
                "cursor_continue"
            } else if params.memory_id.is_some() {
                "by_id"
            } else {
                "search"
            };
            let mut receipt = serde_json::json!({
                "memory_receipt": MEMORY_QUERY_TOOL_NAME,
                "query_kind": query_kind,
                "include_history": params.include_history,
            });
            if let Some(memory_id) = params.memory_id {
                receipt["memory_id"] = serde_json::Value::String(memory_id.0);
            }
            Some(receipt)
        }
        MEMORY_MUTATE_TOOL_NAME => {
            let Ok(params) = serde_json::from_value::<MemoryMutateParams>(arguments.clone()) else {
                return Some(memory_session_invalid_call_receipt(MEMORY_MUTATE_TOOL_NAME));
            };
            if params.validate().is_err()
                || params
                    .memory_id()
                    .is_some_and(|memory_id| !is_valid_memory_id(&memory_id.0))
                || params
                    .expected_revision_id()
                    .is_some_and(|revision_id| !is_valid_memory_revision_id(&revision_id.0))
            {
                return Some(memory_session_invalid_call_receipt(MEMORY_MUTATE_TOOL_NAME));
            }
            let mut receipt = serde_json::json!({
                "memory_receipt": MEMORY_MUTATE_TOOL_NAME,
                "operation": memory_change_type_name(params.operation()),
            });
            if let Some(memory_id) = params.memory_id() {
                receipt["memory_id"] = serde_json::Value::String(memory_id.0.clone());
            }
            if let Some(revision_id) = params.expected_revision_id() {
                receipt["expected_revision_id"] = serde_json::Value::String(revision_id.0.clone());
            }
            Some(receipt)
        }
        MEMORY_DELETE_TOOL_NAME => {
            let Ok(params) = serde_json::from_value::<MemoryDeleteParams>(arguments.clone()) else {
                return Some(memory_session_invalid_call_receipt(MEMORY_DELETE_TOOL_NAME));
            };
            if params.validate().is_err() {
                return Some(memory_session_invalid_call_receipt(MEMORY_DELETE_TOOL_NAME));
            }
            match params {
                MemoryDeleteParams::Memory { memory_id } if is_valid_memory_id(&memory_id.0) => {
                    Some(serde_json::json!({
                        "memory_receipt": MEMORY_DELETE_TOOL_NAME,
                        "scope": "memory",
                        "memory_id": memory_id.0,
                    }))
                }
                MemoryDeleteParams::PersonaAll => Some(serde_json::json!({
                    "memory_receipt": MEMORY_DELETE_TOOL_NAME,
                    "scope": "persona_all",
                })),
                MemoryDeleteParams::Memory { .. } => {
                    Some(memory_session_invalid_call_receipt(MEMORY_DELETE_TOOL_NAME))
                }
            }
        }
        _ => None,
    }
}

/// 记忆 Tool 结果的 canonical 去正文收据：content 是固定模板的无正文文本，
/// structured 只含白名单字段。
pub(super) struct MemorySessionResultReceipt {
    pub(super) success: bool,
    pub(super) content: String,
    pub(super) structured: serde_json::Value,
}

/// 提取可能的结构化收据来源：优先 structured；Tool adapter 也可能把收据
/// 序列化为文本正文，此时把 content 按 JSON 对象解析。
fn memory_session_result_source(result: &ToolResult) -> Option<serde_json::Value> {
    if let Some(structured) = &result.structured
        && structured.is_object()
    {
        return Some(structured.clone());
    }
    serde_json::from_str::<serde_json::Value>(&result.content)
        .ok()
        .filter(|value| value.is_object())
}

fn memory_session_failure_receipt(
    tool_name: &'static str,
    error_code: Option<MemoryErrorCode>,
) -> MemorySessionResultReceipt {
    let mut structured = serde_json::json!({
        "memory_receipt": format!("{tool_name}_result"),
        "state": "error",
    });
    if let Some(error_code) = error_code {
        structured["error_code"] = serde_json::Value::String(error_code.as_str().to_string());
    }
    let content = error_code.map_or_else(
        || "记忆工具调用失败。".to_string(),
        |error_code| format!("记忆工具调用失败：{}。", error_code.as_str()),
    );
    MemorySessionResultReceipt {
        success: false,
        content,
        structured,
    }
}

fn memory_error_code_from_source(source: Option<&serde_json::Value>) -> Option<MemoryErrorCode> {
    let source = source?;
    ["error_code", "code"].into_iter().find_map(|key| {
        source
            .get(key)
            .cloned()
            .and_then(|value| serde_json::from_value::<MemoryErrorCode>(value).ok())
    })
}

fn validate_memory_query_page_receipt(receipt: &MemoryQueryPageReceipt) -> bool {
    if receipt.has_more != receipt.next_cursor.is_some() {
        return false;
    }
    receipt.items.iter().all(|item| {
        is_valid_memory_id(&item.memory_id.0)
            && is_valid_memory_revision_id(&item.revision_id.0)
            && item
                .event_time
                .as_deref()
                .is_none_or(|value| chrono::DateTime::parse_from_rfc3339(value).is_ok())
            && chrono::DateTime::parse_from_rfc3339(&item.recorded_at).is_ok()
            && chrono::DateTime::parse_from_rfc3339(&item.valid_from).is_ok()
            && item
                .valid_to
                .as_deref()
                .is_none_or(|value| chrono::DateTime::parse_from_rfc3339(value).is_ok())
    })
}

pub(super) fn memory_session_result_receipt(
    tool_name: &str,
    result: &ToolResult,
) -> Option<MemorySessionResultReceipt> {
    if !is_memory_session_redacted_tool(tool_name) {
        return None;
    }
    let source = memory_session_result_source(result);
    if !result.is_success() {
        // 失败只保留稳定安全错误码；错误正文可能携带敏感检测输入，不落盘。
        let error_code = memory_error_code_from_source(source.as_ref());
        return Some(memory_session_failure_receipt(
            match tool_name {
                MEMORY_QUERY_TOOL_NAME => MEMORY_QUERY_TOOL_NAME,
                MEMORY_MUTATE_TOOL_NAME => MEMORY_MUTATE_TOOL_NAME,
                MEMORY_DELETE_TOOL_NAME => MEMORY_DELETE_TOOL_NAME,
                _ => unreachable!("已在函数入口排除非记忆工具"),
            },
            error_code,
        ));
    }
    match tool_name {
        MEMORY_QUERY_TOOL_NAME => {
            let Some(receipt) = source
                .and_then(|source| serde_json::from_value::<MemoryQueryPageReceipt>(source).ok())
            else {
                return Some(memory_session_failure_receipt(
                    MEMORY_QUERY_TOOL_NAME,
                    Some(MemoryErrorCode::RepositoryUnavailable),
                ));
            };
            if !validate_memory_query_page_receipt(&receipt) {
                return Some(memory_session_failure_receipt(
                    MEMORY_QUERY_TOOL_NAME,
                    Some(MemoryErrorCode::RepositoryUnavailable),
                ));
            }
            let memory_ids = receipt
                .items
                .iter()
                .map(|item| item.memory_id.0.clone())
                .collect::<Vec<_>>();
            let revision_ids = receipt
                .items
                .iter()
                .map(|item| item.revision_id.0.clone())
                .collect::<Vec<_>>();
            let returned_count = receipt.items.len();
            let receipt = serde_json::json!({
                "memory_receipt": "memory_query_result",
                "state": "completed",
                "returned_count": returned_count,
                "memory_ids": memory_ids,
                "revision_ids": revision_ids,
                "has_more": receipt.has_more,
            });
            Some(MemorySessionResultReceipt {
                success: true,
                content: format!("记忆查询收据：返回 {returned_count} 条记忆。"),
                structured: receipt,
            })
        }
        MEMORY_MUTATE_TOOL_NAME => {
            let Some(receipt) = source
                .and_then(|source| serde_json::from_value::<MemoryMutationReceipt>(source).ok())
            else {
                return Some(memory_session_failure_receipt(
                    MEMORY_MUTATE_TOOL_NAME,
                    Some(MemoryErrorCode::RepositoryUnavailable),
                ));
            };
            if receipt.state != MemoryMutationReceiptState::Staged
                || !is_valid_memory_id(&receipt.memory_id.0)
                || !is_valid_memory_revision_id(&receipt.revision_id.0)
            {
                return Some(memory_session_failure_receipt(
                    MEMORY_MUTATE_TOOL_NAME,
                    Some(MemoryErrorCode::RepositoryUnavailable),
                ));
            }
            let operation = memory_change_type_name(receipt.operation);
            let receipt = serde_json::json!({
                "memory_receipt": "memory_mutate_result",
                "operation": operation,
                "state": "staged",
                "memory_id": receipt.memory_id.0,
                "revision_id": receipt.revision_id.0,
            });
            Some(MemorySessionResultReceipt {
                success: true,
                content: format!("记忆变更收据：operation={operation}，状态 staged。"),
                structured: receipt,
            })
        }
        MEMORY_DELETE_TOOL_NAME => {
            let Some(receipt) = source
                .and_then(|source| serde_json::from_value::<MemoryDeleteReceipt>(source).ok())
            else {
                return Some(memory_session_failure_receipt(
                    MEMORY_DELETE_TOOL_NAME,
                    Some(MemoryErrorCode::RepositoryUnavailable),
                ));
            };
            if !is_valid_memory_deletion_id(&receipt.deletion_id)
                || chrono::DateTime::parse_from_rfc3339(&receipt.completed_at).is_err()
            {
                return Some(memory_session_failure_receipt(
                    MEMORY_DELETE_TOOL_NAME,
                    Some(MemoryErrorCode::RepositoryUnavailable),
                ));
            }
            let receipt = serde_json::json!({
                "memory_receipt": "memory_delete_result",
                "state": "deleted",
                "deletion_id": receipt.deletion_id,
                "deleted_memory_count": receipt.deleted_memory_count,
                "completed_at": receipt.completed_at,
            });
            Some(MemorySessionResultReceipt {
                success: true,
                content: format!(
                    "记忆删除收据：已彻底删除 {} 条记忆。",
                    receipt["deleted_memory_count"]
                ),
                structured: receipt,
            })
        }
        _ => None,
    }
}

pub(super) fn sanitize_private_session_value(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(values) => serde_json::Value::Object(
            values
                .iter()
                .map(|(key, value)| {
                    let value = if is_private_session_secret_field(key) {
                        serde_json::Value::String(PRIVATE_SESSION_SECRET_MARKER.to_string())
                    } else {
                        sanitize_private_session_value(value)
                    };
                    (key.clone(), value)
                })
                .collect(),
        ),
        serde_json::Value::Array(values) => {
            serde_json::Value::Array(values.iter().map(sanitize_private_session_value).collect())
        }
        serde_json::Value::String(value) => {
            serde_json::Value::String(redact_private_session_text(value))
        }
        _ => value.clone(),
    }
}

pub(super) fn is_private_session_secret_field(field: &str) -> bool {
    let normalized = field
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect::<String>();
    matches!(
        normalized.as_str(),
        "authorization"
            | "proxyauthorization"
            | "apikey"
            | "xapikey"
            | "xsubscriptiontoken"
            | "token"
            | "accesstoken"
            | "refreshtoken"
            | "authtoken"
            | "bearertoken"
            | "sessiontoken"
            | "idtoken"
            | "password"
            | "passwd"
            | "secret"
            | "clientsecret"
            | "privatekey"
            | "credential"
            | "credentials"
            | "cookie"
            | "setcookie"
    )
}

pub(super) fn redact_private_session_text(value: &str) -> String {
    let mut redacted = redact_url_secret_components(value);
    for marker in [
        "authorization:",
        "proxy-authorization:",
        "x-api-key:",
        "x-subscription-token:",
    ] {
        redacted = redact_ascii_value_after_marker(&redacted, marker, false);
    }
    for marker in [
        "bearer ",
        "authorization=",
        "proxy_authorization=",
        "proxy-authorization=",
        "api_key=",
        "apikey=",
        "access_token=",
        "refresh_token=",
        "auth_token=",
        "token=",
        "password=",
        "passwd=",
        "secret=",
        "client_secret=",
        "token-",
        "ghp_",
        "github_pat_",
        "sk-",
        "brv-",
    ] {
        redacted = redact_ascii_value_after_marker(&redacted, marker, true);
    }
    redacted
}

pub(super) fn redact_url_secret_components(value: &str) -> String {
    let Ok(mut url) = reqwest::Url::parse(value) else {
        return value.to_string();
    };
    if !matches!(url.scheme(), "http" | "https") {
        return value.to_string();
    }
    if !url.username().is_empty() {
        let _ = url.set_username("redacted");
    }
    if url.password().is_some() {
        let _ = url.set_password(Some("redacted"));
    }
    let query = url
        .query_pairs()
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect::<Vec<_>>();
    if !query.is_empty() {
        url.set_query(None);
        let mut pairs = url.query_pairs_mut();
        for (key, value) in query {
            if is_private_session_secret_field(&key) {
                pairs.append_pair(&key, PRIVATE_SESSION_SECRET_MARKER);
            } else {
                pairs.append_pair(&key, &value);
            }
        }
    }
    url.to_string()
}

pub(super) fn redact_ascii_value_after_marker(
    value: &str,
    marker: &str,
    stop_on_whitespace: bool,
) -> String {
    let mut output = value.to_string();
    let marker_lower = marker.to_ascii_lowercase();
    let mut search_from = 0usize;
    loop {
        let lower = output.to_ascii_lowercase();
        let Some(relative_start) = lower[search_from..].find(&marker_lower) else {
            break;
        };
        let marker_start = search_from.saturating_add(relative_start);
        let mut value_start = marker_start.saturating_add(marker.len());
        while output[value_start..].starts_with([' ', '\t']) {
            value_start = value_start.saturating_add(1);
        }
        let value_end = output[value_start..]
            .char_indices()
            .find_map(|(index, character)| {
                let delimiter = matches!(character, '\'' | '"' | '\r' | '\n' | '&' | ';')
                    || (stop_on_whitespace && character.is_whitespace());
                delimiter.then_some(value_start.saturating_add(index))
            })
            .unwrap_or(output.len());
        if value_end <= value_start {
            search_from = value_start;
            continue;
        }
        output.replace_range(value_start..value_end, PRIVATE_SESSION_SECRET_MARKER);
        search_from = value_start.saturating_add(PRIVATE_SESSION_SECRET_MARKER.len());
    }
    output
}

pub(super) async fn emit_and_record_tool_call(
    state: &Arc<AppState>,
    tx: Option<&RuntimeSseSender>,
    turn: &TurnContext,
    call: &ToolCall,
    risk: &str,
    definition: Option<&ToolDef>,
    policy: RuntimeToolExecutionPolicy,
) -> Result<(), String> {
    let is_memory_tool = is_memory_session_redacted_tool(&call.name);
    let safety_notes = if is_memory_tool {
        // 记忆 handler 的说明可能由原始参数派生；canonical 事件不保留这类自由文本。
        Vec::new()
    } else {
        runtime_tool_handler(&call.name)
            .map(|handler| handler.safety_notes(call))
            .unwrap_or_default()
    };
    let mcp_policy = turn
        .runtime_policy
        .mcp_tool_policies
        .iter()
        .find(|entry| entry.name == call.name);
    // 记忆 Tool 必须先完成专用去正文转换，再进入任何前端事件或 Session 记录。
    let memory_call_receipt = memory_session_call_receipt(&call.name, &call.arguments);
    let emitted_arguments = memory_call_receipt.as_ref().unwrap_or(&call.arguments);
    let canonical_call_id = if is_memory_tool {
        memory_session_call_id(&call.call_id)
    } else {
        call.call_id.clone()
    };
    if let Some(tx) = tx {
        emit_json_event(
            tx,
            runtime_tool_call_event(
                &canonical_call_id,
                &call.name,
                emitted_arguments,
                risk,
                policy.requires_approval,
                policy.interrupt_behavior,
            ),
        )
        .await
        .map_err(|_| "客户端连接已断开。".to_string())?;
    }
    append_required_turn_event(
        state,
        "tool_call",
        serde_json::json!({
            "conversation_id": turn.conversation_id.clone(),
            "turn_id": turn.turn_id.clone(),
            "call_id": canonical_call_id,
            "tool": call.name.clone(),
            "risk": risk,
            "execution_owner": definition.map(|definition| definition.execution_owner.clone()),
            "available": definition.map(|definition| definition.available),
            "disabled_reason": definition.and_then(|definition| definition.disabled_reason.clone()),
            "requires_approval": policy.requires_approval,
            "mcp_server_revision": mcp_policy.map(|entry| entry.server_revision.clone()),
            "mcp_annotations_hash": mcp_policy.map(|entry| entry.annotations_hash.clone()),
            "mcp_approval_policy": mcp_policy.map(|entry| entry.approval_policy.clone()),
            "mcp_approval_source": mcp_policy.map(|entry| entry.approval_source.clone()),
            "requires_workspace_boundary_approval": policy.requires_workspace_boundary_approval,
            "read_only": policy.is_read_only,
            "mutating": policy.is_mutating,
            "concurrency_safe": policy.concurrency_safe,
            "interrupt_behavior": policy.interrupt_behavior.as_str(),
            "safety_notes": safety_notes,
            "canonical_arguments": memory_call_receipt
                .clone()
                .unwrap_or_else(|| canonical_session_tool_arguments(&call.arguments)),
            "argument_audit": if is_memory_tool {
                serde_json::json!({
                    "redacted": true,
                    "audit_note": "记忆工具参数已转换为无正文收据。",
                })
            } else {
                tool_request_audit_metadata(&call.arguments)
            },
        }),
    )
    .await
    .map_err(|err| {
        tracing::error!(target: "muse::transcript", error = %err, "持久化工具调用失败");
        err
    })?;
    Ok(())
}

pub(super) async fn emit_and_record_tool_result(
    state: &Arc<AppState>,
    tx: Option<&RuntimeSseSender>,
    turn: &TurnContext,
    call: &ToolCall,
    result: &ToolResult,
) -> Result<ToolResult, String> {
    let recorded_result = maybe_externalize_tool_result(turn, call, result).await;
    // 记忆 Tool 的去正文收据先于前端事件与 Session 记录生成；返回给工具循环的
    // recorded_result 保持完整，供当前 Turn 私有工作副本继续推理。
    let memory_result_receipt = memory_session_result_receipt(&call.name, &recorded_result);
    let canonical_call_id = if is_memory_session_redacted_tool(&call.name) {
        memory_session_call_id(&call.call_id)
    } else {
        call.call_id.clone()
    };
    if let Some(tx) = tx {
        let (emitted_success, emitted_content, emitted_structured) = match &memory_result_receipt {
            Some(receipt) => (
                receipt.success,
                receipt.content.as_str(),
                Some(&receipt.structured),
            ),
            None => (
                recorded_result.is_success(),
                recorded_result.content.as_str(),
                recorded_result.structured.as_ref(),
            ),
        };
        emit_json_event(
            tx,
            runtime_tool_result_event(
                &canonical_call_id,
                &call.name,
                emitted_success,
                emitted_content,
                emitted_structured,
            ),
        )
        .await
        .map_err(|_| "客户端连接已断开。".to_string())?;
    }
    let context_effect = runtime_tool_handler(&call.name)
        .and_then(|handler| handler.context_effect(result))
        .map(|effect| effect.to_json());
    let canonical_result = canonical_session_tool_result(&recorded_result, context_effect);
    let (canonical_success, canonical_content, canonical_structured, canonical_context_effect) =
        match &memory_result_receipt {
            // 记忆 Tool 不落 context_effect：其负载不在去正文白名单内。
            Some(receipt) => (
                receipt.success,
                receipt.content.clone(),
                Some(receipt.structured.clone()),
                None,
            ),
            None => (
                recorded_result.is_success(),
                canonical_result.content,
                canonical_result.structured,
                canonical_result.context_effect,
            ),
        };
    append_required_turn_event(
        state,
        "tool_result",
        serde_json::json!({
            "conversation_id": turn.conversation_id.clone(),
            "turn_id": turn.turn_id.clone(),
            "call_id": canonical_call_id,
            "tool": call.name.clone(),
            "canonical_result": {
                "success": canonical_success,
                "content": canonical_content,
                "structured": canonical_structured,
            },
            "result_audit": {
                "success": canonical_success,
                "externalized": recorded_result
                    .structured
                    .as_ref()
                    .is_some_and(|value| value.get("tool_result_ref").is_some()),
                "audit_note": "该字段仅供审计展示，不得用于模型会话恢复。",
            },
            "context_effect": canonical_context_effect,
        }),
    )
    .await
    .map_err(|err| {
        tracing::error!(target: "muse::transcript", error = %err, "持久化工具结果失败");
        err
    })?;
    Ok(recorded_result)
}

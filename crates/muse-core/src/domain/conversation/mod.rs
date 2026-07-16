//! 会话历史模块，定义模型消息、角色类型和工具续轮所需的对话裁剪逻辑。

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// 会话中的单条消息。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: String,
    /// 工具调用标识。助手工具调用消息与工具结果消息通过它配对。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// 工具名称，仅工具调用/工具结果消息使用。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    /// 工具调用参数，仅助手工具调用消息使用。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_arguments: Option<Value>,
    /// 模型提供器暴露的推理内容。
    ///
    /// 在 DeepSeek 等思考模式模型里，工具续轮时需要把上一轮助手消息的
    /// `reasoning_content` 原样带回接口；普通模型没有该字段时保持空值。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
}

/// 会话消息角色。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum Role {
    #[serde(rename = "system")]
    System,
    #[serde(rename = "user")]
    User,
    #[serde(rename = "assistant")]
    Assistant,
    #[serde(rename = "tool")]
    Tool,
}

impl std::fmt::Display for Role {
    // 输出与模型协议一致的角色字符串。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Role::System => write!(f, "system"),
            Role::User => write!(f, "user"),
            Role::Assistant => write!(f, "assistant"),
            Role::Tool => write!(f, "tool"),
        }
    }
}

/// 完整会话历史。
#[derive(Debug, Clone)]
pub struct Conversation {
    pub messages: Vec<Message>,
    pub max_history: usize,
    /// 失败工具结果的调用标识，仅用于供应商原生协议序列化。
    ///
    /// 工具输出正文可能是任意文本，不能依靠文案推断成功状态。状态单独保存，
    /// 同时不污染 OpenAI 等只接受固定工具消息字段的协议。
    failed_tool_result_ids: BTreeSet<String>,
}

impl Conversation {
    /// 创建带系统提示词的新会话历史。
    pub fn new(system_prompt: String, max_history: usize) -> Self {
        Self {
            messages: vec![Message {
                role: Role::System,
                content: system_prompt,
                tool_call_id: None,
                tool_name: None,
                tool_arguments: None,
                reasoning_content: None,
            }],
            max_history,
            failed_tool_result_ids: BTreeSet::new(),
        }
    }

    /// 追加用户消息，并在需要时裁剪历史。
    pub fn add_user_message(&mut self, content: String) {
        self.messages.push(Message {
            role: Role::User,
            content,
            tool_call_id: None,
            tool_name: None,
            tool_arguments: None,
            reasoning_content: None,
        });
        self.trim();
    }

    /// 追加不带推理内容的助手回复。
    pub fn add_assistant_message(&mut self, content: String) {
        self.add_assistant_message_with_reasoning(content, None);
    }

    /// 记录带模型提供器推理内容的助手回复。
    pub fn add_assistant_message_with_reasoning(
        &mut self,
        content: String,
        reasoning_content: Option<String>,
    ) {
        self.messages.push(Message {
            role: Role::Assistant,
            content,
            tool_call_id: None,
            tool_name: None,
            tool_arguments: None,
            reasoning_content,
        });
        self.trim();
    }

    /// 记录不带推理内容的结构化工具调用。
    pub fn add_assistant_tool_call(
        &mut self,
        call_id: String,
        tool_name: String,
        arguments: Value,
    ) {
        self.add_assistant_tool_call_with_reasoning(call_id, tool_name, arguments, None);
    }

    /// 记录带模型提供器推理内容的结构化工具调用。
    pub fn add_assistant_tool_call_with_reasoning(
        &mut self,
        call_id: String,
        tool_name: String,
        arguments: Value,
        reasoning_content: Option<String>,
    ) {
        self.messages.push(Message {
            role: Role::Assistant,
            content: String::new(),
            tool_call_id: Some(call_id),
            tool_name: Some(tool_name),
            tool_arguments: Some(arguments),
            reasoning_content,
        });
        self.trim();
    }

    /// 记录工具执行结果，下一轮模型调用时会按模型提供器协议转换成工具输出。
    pub fn add_tool_result(&mut self, call_id: String, tool_name: String, content: String) {
        self.add_tool_result_with_status(call_id, tool_name, content, false);
    }

    /// 记录带明确失败状态的工具执行结果。
    ///
    /// Anthropic 等原生协议会把该状态序列化为 `tool_result.is_error`；其他
    /// provider 仍使用相同的工具正文，不会把状态伪装进自然语言。
    pub fn add_tool_result_with_status(
        &mut self,
        call_id: String,
        tool_name: String,
        content: String,
        is_error: bool,
    ) {
        if is_error {
            self.failed_tool_result_ids.insert(call_id.clone());
        } else {
            self.failed_tool_result_ids.remove(&call_id);
        }
        self.messages.push(Message {
            role: Role::Tool,
            content,
            tool_call_id: Some(call_id),
            tool_name: Some(tool_name),
            tool_arguments: None,
            reasoning_content: None,
        });
        self.trim();
    }

    /// 返回指定工具结果是否由执行层明确标记为失败。
    pub fn tool_result_is_error(&self, call_id: &str) -> bool {
        self.failed_tool_result_ids.contains(call_id)
    }

    /// 将历史消息限制在上限内，始终保留系统提示词和当前用户回合。
    ///
    /// `max_history` 仍然是消息条数预算，但裁剪的最小单位是一个完整用户
    /// 回合。若当前回合自身已经超过预算，会暂时允许超限，避免把工具调用
    /// 与工具结果拆开后发送给模型。
    fn trim(&mut self) {
        while self.messages.len() > self.max_history {
            let mut user_indices = self
                .messages
                .iter()
                .enumerate()
                .filter_map(|(index, message)| (message.role == Role::User).then_some(index));
            let Some(_oldest_user_index) = user_indices.next() else {
                break;
            };
            let Some(current_turn_start) = user_indices.next() else {
                // 只有当前用户回合时不再裁剪，避免产生孤立的工具消息。
                break;
            };

            self.messages = self
                .messages
                .drain(..)
                .enumerate()
                .filter_map(|(index, message)| {
                    (index >= current_turn_start || message.role == Role::System).then_some(message)
                })
                .collect();
            let retained_tool_results = self
                .messages
                .iter()
                .filter(|message| message.role == Role::Tool)
                .filter_map(|message| message.tool_call_id.as_deref())
                .collect::<BTreeSet<_>>();
            self.failed_tool_result_ids
                .retain(|call_id| retained_tool_results.contains(call_id.as_str()));
        }
    }

    /// 重新按完整用户回合应用当前消息预算。
    ///
    /// 正常通过 `add_*` 追加时会自动调用；仅在反序列化后直接替换
    /// `messages` 或动态调整 `max_history` 时需要显式调用。
    pub fn trim_to_budget(&mut self) {
        self.trim();
    }

    /// 原子替换消息集合，并同步清理不再存在的失败工具结果元数据。
    ///
    /// 会话压缩等批量重建路径必须使用该入口，避免已删除 call ID 的失败状态
    /// 残留并污染后来复用同名 ID 的 provider 协议映射。
    pub fn replace_messages(&mut self, messages: Vec<Message>) {
        let retained_tool_result_ids = messages
            .iter()
            .filter(|message| message.role == Role::Tool)
            .filter_map(|message| message.tool_call_id.as_ref())
            .cloned()
            .collect::<BTreeSet<_>>();
        self.failed_tool_result_ids
            .retain(|call_id| retained_tool_result_ids.contains(call_id));
        self.messages = messages;
        self.trim();
    }

    /// 按完整用户回合返回最近消息，至少覆盖给定消息预算。
    ///
    /// 预算只是选择多少个完整帧的目标，绝不会从一个工具结果或助手工具调用中间
    /// 截断。当前回合自身超过预算时会完整保留；系统消息不包含在返回值中。
    pub fn recent_turn_frame_messages(&self, target_messages: usize) -> Vec<Message> {
        let mut frames = Vec::<Vec<Message>>::new();
        for message in self
            .messages
            .iter()
            .filter(|message| message.role != Role::System)
        {
            if message.role == Role::User {
                frames.push(Vec::new());
            }
            if let Some(frame) = frames.last_mut() {
                frame.push(message.clone());
            }
        }

        if frames.is_empty() {
            return self
                .messages
                .iter()
                .filter(|message| message.role != Role::System)
                .cloned()
                .collect();
        }

        let target_messages = target_messages.max(1);
        let mut selected_frames = Vec::new();
        let mut selected_messages = 0usize;
        for frame in frames.into_iter().rev() {
            selected_messages = selected_messages.saturating_add(frame.len());
            selected_frames.push(frame);
            if selected_messages >= target_messages {
                break;
            }
        }
        selected_frames.reverse();
        selected_frames.into_iter().flatten().collect()
    }

    /// 校验完整会话中的工具调用与工具结果是否严格配对。
    ///
    /// 完整校验要求最后不存在尚未收到结果的工具调用，适合在持久化提交或
    /// 发起模型续轮前调用。正在执行工具时可改用
    /// [`Self::validate_tool_protocol_prefix`]。
    pub fn validate_tool_protocol(&self) -> Result<(), ConversationValidationError> {
        self.validate_tool_protocol_inner(false)
    }

    /// 校验会话前缀，允许最后一个助手工具调用仍处于等待结果状态。
    pub fn validate_tool_protocol_prefix(&self) -> Result<(), ConversationValidationError> {
        self.validate_tool_protocol_inner(true)
    }

    fn validate_tool_protocol_inner(
        &self,
        allow_pending_calls: bool,
    ) -> Result<(), ConversationValidationError> {
        let mut pending_calls = BTreeMap::<String, (String, usize)>::new();
        let mut seen_call_ids = BTreeMap::<String, usize>::new();

        for (index, message) in self.messages.iter().enumerate() {
            match message.role {
                Role::System => {}
                Role::User => {
                    if let Some((call_id, (_, call_index))) = pending_calls.iter().next() {
                        return Err(ConversationValidationError::UnresolvedToolCall {
                            call_index: *call_index,
                            call_id: call_id.clone(),
                            before_index: index,
                        });
                    }
                }
                Role::Assistant => match message.tool_call_id.as_deref() {
                    Some(call_id) => {
                        let call_id = call_id.trim();
                        if call_id.is_empty() {
                            return Err(ConversationValidationError::MissingToolCallId { index });
                        }
                        let Some(tool_name) = non_empty(message.tool_name.as_deref()) else {
                            return Err(ConversationValidationError::MissingToolName { index });
                        };
                        if message.tool_arguments.is_none() {
                            return Err(ConversationValidationError::MissingToolArguments {
                                index,
                                call_id: call_id.to_string(),
                            });
                        }
                        if let Some(first_index) = seen_call_ids.get(call_id) {
                            return Err(ConversationValidationError::DuplicateToolCallId {
                                first_index: *first_index,
                                duplicate_index: index,
                                call_id: call_id.to_string(),
                            });
                        }
                        seen_call_ids.insert(call_id.to_string(), index);
                        pending_calls.insert(call_id.to_string(), (tool_name.to_string(), index));
                    }
                    None => {
                        if let Some((call_id, (_, call_index))) = pending_calls.iter().next() {
                            return Err(ConversationValidationError::UnresolvedToolCall {
                                call_index: *call_index,
                                call_id: call_id.clone(),
                                before_index: index,
                            });
                        }
                        if message.tool_name.is_some() || message.tool_arguments.is_some() {
                            return Err(ConversationValidationError::MissingToolCallId { index });
                        }
                    }
                },
                Role::Tool => {
                    let Some(call_id) = non_empty(message.tool_call_id.as_deref()) else {
                        return Err(ConversationValidationError::MissingToolCallId { index });
                    };
                    let Some(tool_name) = non_empty(message.tool_name.as_deref()) else {
                        return Err(ConversationValidationError::MissingToolName { index });
                    };
                    let Some((expected_name, _)) = pending_calls.remove(call_id) else {
                        return Err(ConversationValidationError::OrphanToolResult {
                            index,
                            call_id: call_id.to_string(),
                        });
                    };
                    if expected_name != tool_name {
                        return Err(ConversationValidationError::ToolNameMismatch {
                            index,
                            call_id: call_id.to_string(),
                            expected: expected_name,
                            actual: tool_name.to_string(),
                        });
                    }
                }
            }
        }

        if !allow_pending_calls
            && let Some((call_id, (_, call_index))) = pending_calls.into_iter().next()
        {
            return Err(ConversationValidationError::UnresolvedToolCall {
                call_index,
                call_id,
                before_index: self.messages.len(),
            });
        }
        Ok(())
    }

    /// 返回适合发送给大模型接口的消息，不包含内部元数据。
    pub fn api_messages(&self) -> &[Message] {
        &self.messages
    }

    /// 校验工具协议后返回可发送给模型的消息。
    pub fn validated_api_messages(&self) -> Result<&[Message], ConversationValidationError> {
        self.validate_tool_protocol()?;
        Ok(&self.messages)
    }
}

fn non_empty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

/// 会话工具协议校验错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConversationValidationError {
    MissingToolCallId {
        index: usize,
    },
    MissingToolName {
        index: usize,
    },
    MissingToolArguments {
        index: usize,
        call_id: String,
    },
    DuplicateToolCallId {
        first_index: usize,
        duplicate_index: usize,
        call_id: String,
    },
    OrphanToolResult {
        index: usize,
        call_id: String,
    },
    ToolNameMismatch {
        index: usize,
        call_id: String,
        expected: String,
        actual: String,
    },
    UnresolvedToolCall {
        call_index: usize,
        call_id: String,
        before_index: usize,
    },
}

impl fmt::Display for ConversationValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingToolCallId { index } => {
                write!(formatter, "第 {index} 条消息缺少工具调用标识。")
            }
            Self::MissingToolName { index } => {
                write!(formatter, "第 {index} 条消息缺少工具名称。")
            }
            Self::MissingToolArguments { index, call_id } => write!(
                formatter,
                "第 {index} 条工具调用 `{call_id}` 缺少结构化参数。"
            ),
            Self::DuplicateToolCallId {
                first_index,
                duplicate_index,
                call_id,
            } => write!(
                formatter,
                "第 {duplicate_index} 条消息重复使用工具调用标识 `{call_id}`（首次出现在第 {first_index} 条）。"
            ),
            Self::OrphanToolResult { index, call_id } => write!(
                formatter,
                "第 {index} 条工具结果 `{call_id}` 没有对应的助手工具调用。"
            ),
            Self::ToolNameMismatch {
                index,
                call_id,
                expected,
                actual,
            } => write!(
                formatter,
                "第 {index} 条工具结果 `{call_id}` 的工具名为 `{actual}`，预期为 `{expected}`。"
            ),
            Self::UnresolvedToolCall {
                call_index,
                call_id,
                before_index,
            } => write!(
                formatter,
                "第 {call_index} 条工具调用 `{call_id}` 在第 {before_index} 条消息前仍未收到结果。"
            ),
        }
    }
}

impl std::error::Error for ConversationValidationError {}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{Conversation, ConversationValidationError, Message, Role};

    #[test]
    fn trim_removes_a_complete_user_turn_instead_of_orphaning_tool_results() {
        let mut conversation = Conversation::new("system".to_string(), 5);
        conversation.add_user_message("first".to_string());
        conversation.add_assistant_tool_call(
            "call-1".to_string(),
            "search".to_string(),
            json!({"query": "first"}),
        );
        conversation.add_tool_result(
            "call-1".to_string(),
            "search".to_string(),
            "result".to_string(),
        );
        conversation.add_assistant_message("answer".to_string());

        conversation.add_user_message("second".to_string());

        assert_eq!(conversation.messages.len(), 2);
        assert_eq!(conversation.messages[0].role, Role::System);
        assert_eq!(conversation.messages[1].role, Role::User);
        assert_eq!(conversation.messages[1].content, "second");
        assert!(conversation.validate_tool_protocol().is_ok());
    }

    #[test]
    fn trim_allows_an_oversized_active_turn_to_remain_complete() {
        let mut conversation = Conversation::new("system".to_string(), 2);
        conversation.add_user_message("question".to_string());
        conversation.add_assistant_tool_call("call-1".to_string(), "search".to_string(), json!({}));
        conversation.add_tool_result(
            "call-1".to_string(),
            "search".to_string(),
            "result".to_string(),
        );

        assert_eq!(conversation.messages.len(), 4);
        assert!(conversation.validate_tool_protocol().is_ok());
    }

    #[test]
    fn recent_turn_frames_never_start_with_a_tool_result() {
        let mut conversation = Conversation::new("system".to_string(), 30);
        conversation.add_user_message("first".to_string());
        conversation.add_assistant_message("first answer".to_string());

        conversation.add_user_message("second".to_string());
        conversation.add_assistant_tool_call("call-1".to_string(), "search".to_string(), json!({}));
        conversation.add_tool_result(
            "call-1".to_string(),
            "search".to_string(),
            "one".to_string(),
        );
        conversation.add_assistant_tool_call("call-2".to_string(), "read".to_string(), json!({}));
        conversation.add_tool_result("call-2".to_string(), "read".to_string(), "two".to_string());

        conversation.add_user_message("third".to_string());
        conversation.add_assistant_message("third answer".to_string());

        let recent = conversation.recent_turn_frame_messages(6);
        assert_eq!(recent.len(), 7, "消息预算不足时也必须扩展到完整回合边界");
        assert_eq!(
            recent.first().map(|message| &message.role),
            Some(&Role::User)
        );
        assert_eq!(
            recent.first().map(|message| message.content.as_str()),
            Some("second")
        );

        let mut replay = Conversation::new("system".to_string(), 30);
        replay.messages.extend(recent);
        assert!(replay.validate_tool_protocol().is_ok());
    }

    #[test]
    fn tool_protocol_accepts_multiple_calls_and_results() {
        let mut conversation = Conversation::new("system".to_string(), 20);
        conversation.add_user_message("question".to_string());
        conversation.add_assistant_tool_call("call-1".to_string(), "search".to_string(), json!({}));
        conversation.add_assistant_tool_call("call-2".to_string(), "read".to_string(), json!({}));
        conversation.add_tool_result(
            "call-1".to_string(),
            "search".to_string(),
            "one".to_string(),
        );
        conversation.add_tool_result("call-2".to_string(), "read".to_string(), "two".to_string());

        assert!(conversation.validate_tool_protocol().is_ok());
    }

    #[test]
    fn tool_result_failure_status_is_separate_from_arbitrary_content() {
        let mut conversation = Conversation::new("system".to_string(), 20);
        conversation.add_user_message("question".to_string());
        conversation.add_assistant_tool_call("call-1".to_string(), "search".to_string(), json!({}));
        conversation.add_tool_result_with_status(
            "call-1".to_string(),
            "search".to_string(),
            "正文没有错误关键词".to_string(),
            true,
        );

        assert!(conversation.tool_result_is_error("call-1"));
        assert!(!conversation.tool_result_is_error("unknown"));
        assert!(conversation.validate_tool_protocol().is_ok());
    }

    #[test]
    fn replacing_messages_prunes_removed_failed_tool_result_ids() {
        let mut conversation = Conversation::new("system".to_string(), 20);
        conversation.add_user_message("question".to_string());
        conversation.add_assistant_tool_call("call-1".to_string(), "search".to_string(), json!({}));
        conversation.add_tool_result_with_status(
            "call-1".to_string(),
            "search".to_string(),
            "failed".to_string(),
            true,
        );

        conversation.replace_messages(vec![Message {
            role: Role::System,
            content: "system".to_string(),
            tool_call_id: None,
            tool_name: None,
            tool_arguments: None,
            reasoning_content: None,
        }]);

        assert!(!conversation.tool_result_is_error("call-1"));
    }

    #[test]
    fn prefix_validation_allows_pending_call_but_complete_validation_rejects_it() {
        let mut conversation = Conversation::new("system".to_string(), 20);
        conversation.add_user_message("question".to_string());
        conversation.add_assistant_tool_call("call-1".to_string(), "search".to_string(), json!({}));

        assert!(conversation.validate_tool_protocol_prefix().is_ok());
        assert!(matches!(
            conversation.validate_tool_protocol(),
            Err(ConversationValidationError::UnresolvedToolCall { .. })
        ));
    }

    #[test]
    fn tool_protocol_rejects_orphan_and_mismatched_results() {
        let mut orphan = Conversation::new("system".to_string(), 20);
        orphan.add_user_message("question".to_string());
        orphan.add_tool_result(
            "missing".to_string(),
            "search".to_string(),
            "result".to_string(),
        );
        assert!(matches!(
            orphan.validate_tool_protocol(),
            Err(ConversationValidationError::OrphanToolResult { .. })
        ));

        let mut mismatch = Conversation::new("system".to_string(), 20);
        mismatch.add_user_message("question".to_string());
        mismatch.add_assistant_tool_call("call-1".to_string(), "search".to_string(), json!({}));
        mismatch.add_tool_result(
            "call-1".to_string(),
            "read".to_string(),
            "result".to_string(),
        );
        assert!(matches!(
            mismatch.validate_tool_protocol(),
            Err(ConversationValidationError::ToolNameMismatch { .. })
        ));
    }
}

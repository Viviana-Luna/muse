//! 运行时工具层基础契约和能力矩阵。
//!
//! 具体工具执行暂时仍由 `handlers` 中的运行底座处理器承接；本模块先沉淀
//! 不依赖 HTTP handler 私有状态的稳定类型，作为后续按工具族拆分的入口。

/// 工具在当前 turn 被取消时的处理策略。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RuntimeToolInterruptBehavior {
    Block,
    Cancel,
}

impl RuntimeToolInterruptBehavior {
    /// 返回写入 SSE 事件和 transcript 时使用的稳定字符串。
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            RuntimeToolInterruptBehavior::Block => "block",
            RuntimeToolInterruptBehavior::Cancel => "cancel",
        }
    }
}

/// 工具结果对后续模型上下文的影响。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RuntimeToolContextEffect {
    Append {
        title: &'static str,
        content: String,
    },
    Replace {
        reason: &'static str,
        content: String,
    },
}

impl RuntimeToolContextEffect {
    /// 将上下文效果合并到模型可读工具结果中。
    pub(crate) fn apply_to_model_content(&self, base: String) -> String {
        match self {
            RuntimeToolContextEffect::Append { title, content } => {
                let effect = content.trim();
                if effect.is_empty() {
                    base
                } else {
                    format!("{base}\n\n上下文更新（{title}）：\n{effect}")
                }
            }
            RuntimeToolContextEffect::Replace { content, .. } => content.clone(),
        }
    }

    /// 转换为 transcript 中记录的结构化上下文效果。
    pub(crate) fn to_json(&self) -> serde_json::Value {
        match self {
            RuntimeToolContextEffect::Append { title, content } => serde_json::json!({
                "mode": "append",
                "title": title,
                "content": content,
            }),
            RuntimeToolContextEffect::Replace { reason, content } => serde_json::json!({
                "mode": "replace",
                "reason": reason,
                "content": content,
            }),
        }
    }
}

/// 运行时工具能力快照，用于冻结当前 Web runtime 承接范围。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RuntimeToolCapability {
    pub(crate) name: &'static str,
    pub(crate) family: RuntimeToolFamily,
    pub(crate) mutating: bool,
    pub(crate) interrupt_behavior: RuntimeToolInterruptBehavior,
    pub(crate) writes_transcript: bool,
    pub(crate) has_context_effect: bool,
}

/// 运行时工具族，后续按该边界拆分具体实现文件。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RuntimeToolFamily {
    Planning,
    Interaction,
    Speech,
    File,
    Command,
    Network,
    Session,
    Model,
    Persona,
    Mcp,
    Skill,
    Task,
}

const RUNTIME_TOOL_CAPABILITIES: &[RuntimeToolCapability] = &[
    RuntimeToolCapability {
        name: "todo_write",
        family: RuntimeToolFamily::Planning,
        mutating: true,
        interrupt_behavior: RuntimeToolInterruptBehavior::Block,
        writes_transcript: true,
        has_context_effect: true,
    },
    RuntimeToolCapability {
        name: "enter_plan_mode",
        family: RuntimeToolFamily::Planning,
        mutating: true,
        interrupt_behavior: RuntimeToolInterruptBehavior::Block,
        writes_transcript: true,
        has_context_effect: true,
    },
    RuntimeToolCapability {
        name: "exit_plan_mode",
        family: RuntimeToolFamily::Planning,
        mutating: true,
        interrupt_behavior: RuntimeToolInterruptBehavior::Block,
        writes_transcript: true,
        has_context_effect: true,
    },
    RuntimeToolCapability {
        name: "send_user_message",
        family: RuntimeToolFamily::Interaction,
        mutating: false,
        interrupt_behavior: RuntimeToolInterruptBehavior::Block,
        writes_transcript: true,
        has_context_effect: true,
    },
    RuntimeToolCapability {
        name: "brief",
        family: RuntimeToolFamily::Interaction,
        mutating: false,
        interrupt_behavior: RuntimeToolInterruptBehavior::Block,
        writes_transcript: true,
        has_context_effect: true,
    },
    RuntimeToolCapability {
        name: "load_skill",
        family: RuntimeToolFamily::Skill,
        mutating: false,
        interrupt_behavior: RuntimeToolInterruptBehavior::Block,
        writes_transcript: true,
        has_context_effect: false,
    },
    RuntimeToolCapability {
        name: "create_skill",
        family: RuntimeToolFamily::Skill,
        mutating: true,
        interrupt_behavior: RuntimeToolInterruptBehavior::Block,
        writes_transcript: true,
        has_context_effect: true,
    },
    RuntimeToolCapability {
        name: "use_skill",
        family: RuntimeToolFamily::Skill,
        mutating: false,
        interrupt_behavior: RuntimeToolInterruptBehavior::Block,
        writes_transcript: true,
        has_context_effect: false,
    },
    RuntimeToolCapability {
        name: "skill",
        family: RuntimeToolFamily::Skill,
        mutating: false,
        interrupt_behavior: RuntimeToolInterruptBehavior::Block,
        writes_transcript: true,
        has_context_effect: false,
    },
    RuntimeToolCapability {
        name: "agent",
        family: RuntimeToolFamily::Task,
        mutating: true,
        interrupt_behavior: RuntimeToolInterruptBehavior::Block,
        writes_transcript: true,
        has_context_effect: true,
    },
    RuntimeToolCapability {
        name: "task_stop",
        family: RuntimeToolFamily::Task,
        mutating: true,
        interrupt_behavior: RuntimeToolInterruptBehavior::Block,
        writes_transcript: true,
        has_context_effect: false,
    },
    RuntimeToolCapability {
        name: "ask_user_question",
        family: RuntimeToolFamily::Interaction,
        mutating: false,
        interrupt_behavior: RuntimeToolInterruptBehavior::Block,
        writes_transcript: true,
        has_context_effect: false,
    },
    RuntimeToolCapability {
        name: "tts_speak",
        family: RuntimeToolFamily::Speech,
        mutating: true,
        interrupt_behavior: RuntimeToolInterruptBehavior::Block,
        writes_transcript: true,
        has_context_effect: false,
    },
    RuntimeToolCapability {
        name: "voice_current",
        family: RuntimeToolFamily::Speech,
        mutating: false,
        interrupt_behavior: RuntimeToolInterruptBehavior::Block,
        writes_transcript: true,
        has_context_effect: false,
    },
    RuntimeToolCapability {
        name: "file_read",
        family: RuntimeToolFamily::File,
        mutating: false,
        interrupt_behavior: RuntimeToolInterruptBehavior::Block,
        writes_transcript: true,
        has_context_effect: false,
    },
    RuntimeToolCapability {
        name: "file_list",
        family: RuntimeToolFamily::File,
        mutating: false,
        interrupt_behavior: RuntimeToolInterruptBehavior::Block,
        writes_transcript: true,
        has_context_effect: false,
    },
    RuntimeToolCapability {
        name: "file_search",
        family: RuntimeToolFamily::File,
        mutating: false,
        interrupt_behavior: RuntimeToolInterruptBehavior::Cancel,
        writes_transcript: true,
        has_context_effect: false,
    },
    RuntimeToolCapability {
        name: "file_write",
        family: RuntimeToolFamily::File,
        mutating: true,
        interrupt_behavior: RuntimeToolInterruptBehavior::Block,
        writes_transcript: true,
        has_context_effect: true,
    },
    RuntimeToolCapability {
        name: "file_edit",
        family: RuntimeToolFamily::File,
        mutating: true,
        interrupt_behavior: RuntimeToolInterruptBehavior::Block,
        writes_transcript: true,
        has_context_effect: true,
    },
    RuntimeToolCapability {
        name: "command_run",
        family: RuntimeToolFamily::Command,
        mutating: true,
        interrupt_behavior: RuntimeToolInterruptBehavior::Cancel,
        writes_transcript: true,
        has_context_effect: false,
    },
    RuntimeToolCapability {
        name: "web_fetch",
        family: RuntimeToolFamily::Network,
        mutating: false,
        interrupt_behavior: RuntimeToolInterruptBehavior::Cancel,
        writes_transcript: true,
        has_context_effect: false,
    },
    RuntimeToolCapability {
        name: "web_search",
        family: RuntimeToolFamily::Network,
        mutating: false,
        interrupt_behavior: RuntimeToolInterruptBehavior::Cancel,
        writes_transcript: true,
        has_context_effect: false,
    },
    RuntimeToolCapability {
        name: "session_list",
        family: RuntimeToolFamily::Session,
        mutating: false,
        interrupt_behavior: RuntimeToolInterruptBehavior::Block,
        writes_transcript: true,
        has_context_effect: false,
    },
    RuntimeToolCapability {
        name: "session_read",
        family: RuntimeToolFamily::Session,
        mutating: false,
        interrupt_behavior: RuntimeToolInterruptBehavior::Block,
        writes_transcript: true,
        has_context_effect: false,
    },
    RuntimeToolCapability {
        name: "tool_result_read",
        family: RuntimeToolFamily::Session,
        mutating: false,
        interrupt_behavior: RuntimeToolInterruptBehavior::Block,
        writes_transcript: true,
        has_context_effect: false,
    },
    RuntimeToolCapability {
        name: "session_compact",
        family: RuntimeToolFamily::Session,
        mutating: true,
        interrupt_behavior: RuntimeToolInterruptBehavior::Block,
        writes_transcript: true,
        has_context_effect: true,
    },
    RuntimeToolCapability {
        name: "model_info",
        family: RuntimeToolFamily::Model,
        mutating: false,
        interrupt_behavior: RuntimeToolInterruptBehavior::Block,
        writes_transcript: true,
        has_context_effect: false,
    },
    RuntimeToolCapability {
        name: "persona_info",
        family: RuntimeToolFamily::Persona,
        mutating: false,
        interrupt_behavior: RuntimeToolInterruptBehavior::Block,
        writes_transcript: true,
        has_context_effect: false,
    },
    RuntimeToolCapability {
        name: "persona_switch",
        family: RuntimeToolFamily::Persona,
        mutating: true,
        interrupt_behavior: RuntimeToolInterruptBehavior::Block,
        writes_transcript: true,
        has_context_effect: false,
    },
    RuntimeToolCapability {
        name: "mcp_list_resources",
        family: RuntimeToolFamily::Mcp,
        mutating: false,
        interrupt_behavior: RuntimeToolInterruptBehavior::Block,
        writes_transcript: true,
        has_context_effect: false,
    },
    RuntimeToolCapability {
        name: "mcp_list_resource_templates",
        family: RuntimeToolFamily::Mcp,
        mutating: false,
        interrupt_behavior: RuntimeToolInterruptBehavior::Block,
        writes_transcript: true,
        has_context_effect: false,
    },
    RuntimeToolCapability {
        name: "mcp_read_resource",
        family: RuntimeToolFamily::Mcp,
        mutating: false,
        interrupt_behavior: RuntimeToolInterruptBehavior::Block,
        writes_transcript: true,
        has_context_effect: false,
    },
];

/// 返回能力矩阵完整快照，仅用于回归测试验证处理器注册表没有漂移。
#[cfg(test)]
pub(crate) fn capabilities() -> &'static [RuntimeToolCapability] {
    RUNTIME_TOOL_CAPABILITIES
}

/// 按工具名查询运行时能力。
pub(crate) fn capability(name: &str) -> Option<&'static RuntimeToolCapability> {
    RUNTIME_TOOL_CAPABILITIES
        .iter()
        .find(|capability| capability.name == name)
}

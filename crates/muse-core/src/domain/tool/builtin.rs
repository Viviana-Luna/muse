//! 内置工具注册模块，集中声明启动时注入注册表的兼容工具和运行底座工具。

use super::{
    ExternalToolProvider, Tool, ToolExecutionOwner, ToolRegistry, ToolResult, ToolResultStatus,
    ToolRisk, WebSearchToolRequest,
};
use std::sync::Arc;

/// 注册所有内置工具到注册表。
pub fn register_all(registry: &mut ToolRegistry) {
    register_web_runtime_search_tool(registry);
    register_harness_tools(registry);
}

/// 注册由网页运行时执行的真实搜索入口。
///
/// 搜索后端与凭据由网页运行时解析，因此核心注册表不读取配置或密钥。
fn register_web_runtime_search_tool(registry: &mut ToolRegistry) {
    register_web_runtime_tool(
        registry,
        "web_search",
        "使用 Exa 搜索公开网页；默认走免费托管搜索，也可使用用户配置的 Exa API Key。联网查询会先经过当前会话审批策略。",
        "network",
        ToolRisk::Network,
        true,
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "搜索关键词或问题" },
                "limit": { "type": "integer", "description": "最多返回结果数，默认 5，最大 10" }
            },
            "required": ["query"],
            "additionalProperties": false
        }),
    );
}

/// 注册所有内置工具，并注入外部查询型工具提供器。
pub fn register_all_with_external_provider(
    registry: &mut ToolRegistry,
    external_provider: Arc<dyn ExternalToolProvider>,
) {
    register_external_query_tools(registry, external_provider);
    register_harness_tools(registry);
}

struct ToolRegistration<H>
where
    H: Fn(serde_json::Value) -> ToolResult + Send + Sync + 'static,
{
    name: String,
    description: String,
    category: String,
    risk: ToolRisk,
    requires_approval: bool,
    expose_to_model: bool,
    execution_owner: ToolExecutionOwner,
    available: bool,
    disabled_reason: Option<String>,
    parameters: serde_json::Value,
    handler: H,
}

/// 注册单个工具；兼容 dispatch 可以注册但不向新 Turn 暴露。
fn register_tool<H>(registry: &mut ToolRegistry, registration: ToolRegistration<H>)
where
    H: Fn(serde_json::Value) -> ToolResult + Send + Sync + 'static,
{
    registry.register(Tool {
        name: registration.name,
        description: registration.description,
        parameters: registration.parameters,
        category: registration.category,
        risk: registration.risk,
        requires_approval: registration.requires_approval,
        expose_to_model: registration.expose_to_model,
        execution_owner: registration.execution_owner,
        available: registration.available,
        disabled_reason: registration.disabled_reason,
        handler: Arc::new(registration.handler),
    });
}

/// 注册由网页运行底座真实执行的工具定义。
fn register_web_runtime_tool(
    registry: &mut ToolRegistry,
    name: &str,
    description: &str,
    category: &str,
    risk: ToolRisk,
    requires_approval: bool,
    parameters: serde_json::Value,
) {
    register_web_runtime_tool_with_visibility(
        registry,
        name,
        description,
        category,
        risk,
        requires_approval,
        parameters,
        true,
    );
}

#[allow(clippy::too_many_arguments)]
fn register_web_runtime_tool_with_visibility(
    registry: &mut ToolRegistry,
    name: &str,
    description: &str,
    category: &str,
    risk: ToolRisk,
    requires_approval: bool,
    parameters: serde_json::Value,
    expose_to_model: bool,
) {
    let tool_name = name.to_string();
    register_tool(
        registry,
        ToolRegistration {
            name: name.to_string(),
            description: description.to_string(),
            category: category.to_string(),
            risk,
            requires_approval,
            expose_to_model,
            execution_owner: ToolExecutionOwner::WebRuntime,
            available: true,
            disabled_reason: None,
            parameters,
            handler: move |_args| ToolResult {
                status: ToolResultStatus::Failed,
                content: format!(
                    "工具 `{tool_name}` 由 Web runtime 执行，不能通过 core 闭包路径直接运行。"
                ),
                structured: Some(serde_json::json!({
                    "name": tool_name,
                    "reason": "web_runtime_owned",
                    "execution_owner": "web_runtime"
                })),
            },
        },
    );
}

/// 注册需要外部提供器承接的查询工具。
fn register_external_query_tools(
    registry: &mut ToolRegistry,
    external_provider: Arc<dyn ExternalToolProvider>,
) {
    let web_search_available = external_provider.web_search_available();
    let web_search_disabled_reason = external_provider.web_search_disabled_reason();
    let execution_owner = if web_search_available {
        ToolExecutionOwner::ExternalProvider
    } else {
        ToolExecutionOwner::Disabled
    };
    register_tool(
        registry,
        ToolRegistration {
            name: "web_search".to_string(),
            description: "执行 Web 搜索，联网前需要用户审批。".to_string(),
            category: "network".to_string(),
            risk: ToolRisk::Network,
            requires_approval: true,
            expose_to_model: true,
            execution_owner,
            available: web_search_available,
            disabled_reason: web_search_disabled_reason,
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "搜索关键词或问题"
                    },
                    "limit": {
                        "type": "integer",
                        "description": "最多返回多少条结果，默认 5"
                    }
                },
                "required": ["query"]
            }),
            handler: move |args| {
                let query = args["query"].as_str().unwrap_or("").trim().to_string();
                if query.is_empty() {
                    return ToolResult {
                        status: ToolResultStatus::Failed,
                        content: "Web 搜索失败：缺少 query 参数。".into(),
                        structured: None,
                    };
                }

                let limit = args["limit"]
                    .as_u64()
                    .and_then(|value| usize::try_from(value).ok())
                    .filter(|value| *value > 0)
                    .unwrap_or(5);

                external_provider.web_search(&WebSearchToolRequest { query, limit })
            },
        },
    );
}

/// 注册 Codex 风格运行底座工具定义，执行逻辑由网页层结构化处理器承接。
fn register_harness_tools(registry: &mut ToolRegistry) {
    register_web_runtime_tool(
        registry,
        "todo_write",
        "写入或更新当前任务清单。模型应提交完整 todo 快照，用于持续回填任务状态。",
        "planning",
        ToolRisk::ExternalSideEffect,
        false,
        serde_json::json!({
            "type": "object",
            "properties": {
                "todos": {
                    "type": "array",
                    "description": "完整任务清单快照，按当前执行顺序排列。",
                    "items": {
                        "type": "object",
                        "properties": {
                            "id": { "type": "string", "description": "稳定任务 ID，可省略；省略时运行时自动生成。" },
                            "content": { "type": "string", "description": "任务内容，必须具体可执行。" },
                            "status": { "type": "string", "description": "任务状态：pending、in_progress、completed。" },
                            "priority": { "type": "string", "description": "优先级，可选：high、medium、low。" }
                        },
                        "required": ["content", "status"],
                        "additionalProperties": false
                    }
                },
                "summary": { "type": "string", "description": "本次任务清单更新摘要，可选。" }
            },
            "required": ["todos"],
            "additionalProperties": false
        }),
    );
    register_web_runtime_tool(
        registry,
        "enter_plan_mode",
        "进入临时计划态。计划态只允许读、查、问和写计划，不直接执行写入或命令。",
        "planning",
        ToolRisk::ExternalSideEffect,
        false,
        serde_json::json!({
            "type": "object",
            "properties": {
                "reason": { "type": "string", "description": "为什么需要先进入计划预设，可选。" }
            },
            "required": [],
            "additionalProperties": false
        }),
    );
    register_web_runtime_tool(
        registry,
        "exit_plan_mode",
        "提交当前计划并等待用户确认。用户确认后回到专注工作预设；用户拒绝或取消时保持计划预设。",
        "planning",
        ToolRisk::ExternalSideEffect,
        false,
        serde_json::json!({
            "type": "object",
            "properties": {
                "plan_summary": { "type": "string", "description": "计划摘要，必须说明目标和执行边界。" },
                "steps": {
                    "type": "array",
                    "description": "计划步骤列表。",
                    "items": { "type": "string" }
                },
                "risks": {
                    "type": "array",
                    "description": "风险、审批点或需要用户注意的事项。",
                    "items": { "type": "string" }
                },
                "next_action": { "type": "string", "description": "用户确认后第一步要执行的动作，可选。" }
            },
            "required": ["plan_summary"],
            "additionalProperties": false
        }),
    );
    register_web_runtime_tool(
        registry,
        "ask_user_question",
        "向用户提出结构化选择题，用于澄清需求、收集偏好或在多个实现方向之间让用户决策。",
        "interaction",
        ToolRisk::ReadOnly,
        false,
        serde_json::json!({
            "type": "object",
            "properties": {
                "questions": {
                    "type": "array",
                    "description": "要询问用户的问题，1 到 4 个。每个问题必须可通过选项回答。",
                    "minItems": 1,
                    "maxItems": 4,
                    "items": {
                        "type": "object",
                        "properties": {
                            "question": {
                                "type": "string",
                                "description": "完整问题文本，应该清晰、具体。"
                            },
                            "header": {
                                "type": "string",
                                "description": "短标签，最多 12 个字符，用于前端卡片标题。"
                            },
                            "options": {
                                "type": "array",
                                "description": "候选项，2 到 4 个；不要包含 Other/其他，前端会自动提供自定义输入。",
                                "minItems": 2,
                                "maxItems": 4,
                                "items": {
                                    "type": "object",
                                    "properties": {
                                        "label": {
                                            "type": "string",
                                            "description": "用户看到并选择的选项名，建议 1 到 5 个词。"
                                        },
                                        "description": {
                                            "type": "string",
                                            "description": "说明选择该项的含义、影响或取舍。"
                                        }
                                    },
                                    "required": ["label", "description"],
                                    "additionalProperties": false
                                }
                            },
                            "multiSelect": {
                                "type": "boolean",
                                "description": "是否允许多选，默认 false。"
                            }
                        },
                        "required": ["question", "header", "options"],
                        "additionalProperties": false
                    }
                }
            },
            "required": ["questions"],
            "additionalProperties": false
        }),
    );
    register_web_runtime_tool(
        registry,
        "send_user_message",
        "在长时间或多步复杂任务的思考和执行中途，向用户发出一则阶段进展汇报；需要明确继续边界时可要求用户回复确认。",
        "interaction",
        ToolRisk::ReadOnly,
        false,
        serde_json::json!({
            "type": "object",
            "properties": {
                "message": { "type": "string", "description": "要向用户汇报的阶段性进展内容" },
                "requires_reply": { "type": "boolean", "description": "是否需要等待用户进一步回复或确认才继续，默认 false" }
            },
            "required": ["message"],
            "additionalProperties": false
        }),
    );
    register_web_runtime_tool(
        registry,
        "brief",
        "send_user_message 的短别名；用于在推理过程中向用户输出阶段性结论，不等待回复。",
        "interaction",
        ToolRisk::ReadOnly,
        false,
        serde_json::json!({
            "type": "object",
            "properties": {
                "message": { "type": "string", "description": "简报内容" }
            },
            "required": ["message"],
            "additionalProperties": false
        }),
    );
    register_web_runtime_tool(
        registry,
        "load_skill",
        "动态查询并载入本地技能库或专用工作流指导文件（SKILL.md），获取任务处理的详细步骤指引。",
        "knowledge",
        ToolRisk::ReadOnly,
        false,
        serde_json::json!({
            "type": "object",
            "properties": {
                "skill_name": { "type": "string", "description": "技能名称，如 antigravity-guide 或项目专有技能名称", "pattern": "^[a-z0-9]+(?:-[a-z0-9]+)*$", "maxLength": 64 }
            },
            "required": ["skill_name"],
            "additionalProperties": false
        }),
    );
    register_web_runtime_tool(
        registry,
        "create_skill",
        "创建一个新的用户 Skill。服务端会校验并原子发布 SKILL.md；每次创建都需要用户审批，同名内容不会被覆盖，启用的新 Skill 从下一轮对话起可加载。",
        "knowledge",
        ToolRisk::WriteFile,
        true,
        serde_json::json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "Skill 名称，1-64 位小写字母、数字和单连字符；不能使用保留名称 list 或 help",
                    "pattern": "^[a-z0-9]+(?:-[a-z0-9]+)*$",
                    "maxLength": 64
                },
                "description": {
                    "type": "string",
                    "description": "说明 Skill 做什么以及何时使用，不能为空，最多 1024 个字符",
                    "maxLength": 1024
                },
                "content": {
                    "type": "string",
                    "description": "SKILL.md 的 Markdown 正文，不包含 frontmatter，不能为空"
                },
                "enabled": {
                    "type": "boolean",
                    "description": "是否立即启用，默认 true"
                }
            },
            "required": ["name", "description", "content"],
            "additionalProperties": false
        }),
    );
    register_web_runtime_tool_with_visibility(
        registry,
        "use_skill",
        "load_skill 的别名；当任务需要专用本地技能包时读取对应 SKILL.md。",
        "knowledge",
        ToolRisk::ReadOnly,
        false,
        serde_json::json!({
            "type": "object",
            "properties": {
                "skill_name": { "type": "string", "description": "技能名称，如 time-calculator 或项目专有技能名称", "pattern": "^[a-z0-9]+(?:-[a-z0-9]+)*$", "maxLength": 64 }
            },
            "required": ["skill_name"],
            "additionalProperties": false
        }),
        false,
    );
    register_web_runtime_tool_with_visibility(
        registry,
        "skill",
        "load_skill 的兼容别名；动态读取本地技能说明并注入当前轮次上下文。",
        "knowledge",
        ToolRisk::ReadOnly,
        false,
        serde_json::json!({
            "type": "object",
            "properties": {
                "skill_name": { "type": "string", "description": "技能名称，如 time-calculator 或项目专有技能名称", "pattern": "^[a-z0-9]+(?:-[a-z0-9]+)*$", "maxLength": 64 }
            },
            "required": ["skill_name"],
            "additionalProperties": false
        }),
        false,
    );
    register_web_runtime_tool(
        registry,
        "agent",
        "登记一个轻量子任务，写入当前运行时任务清单并把子任务目标回灌到上下文；本工具不启动并发子模型。",
        "planning",
        ToolRisk::ExternalSideEffect,
        false,
        serde_json::json!({
            "type": "object",
            "properties": {
                "task": { "type": "string", "description": "子任务目标，必须具体可执行。" },
                "context": { "type": "string", "description": "子任务需要继承的当前上下文，可选。" },
                "expected_output": { "type": "string", "description": "期望产出格式或完成标准，可选。" },
                "priority": {
                    "type": "string",
                    "description": "优先级，可选：high、medium、low。",
                    "enum": ["high", "medium", "low"]
                }
            },
            "required": ["task"],
            "additionalProperties": false
        }),
    );
    register_web_runtime_tool(
        registry,
        "task_stop",
        "主动请求安全停止或取消当前任务轮次，释放等待中的审批或用户确认。",
        "control",
        ToolRisk::ExternalSideEffect,
        false,
        serde_json::json!({
            "type": "object",
            "properties": {
                "task_id": { "type": "string", "description": "要停止的任务或轮次 ID，可选；省略时停止当前任务。" },
                "reason": { "type": "string", "description": "停止任务的原因说明。" }
            },
            "required": ["reason"],
            "additionalProperties": false
        }),
    );
    register_web_runtime_tool(
        registry,
        "tts_speak",
        "使用当前启用音色把指定文本说出来。模型只能决定是否朗读和朗读文本，不能指定音色。",
        "voice",
        ToolRisk::ExternalSideEffect,
        false,
        serde_json::json!({
            "type": "object",
            "properties": {
                "text": { "type": "string", "description": "要朗读给用户的文本" },
                "reason": { "type": "string", "description": "为什么需要朗读，可选" }
            },
            "required": ["text"],
            "additionalProperties": false
        }),
    );
    register_web_runtime_tool(
        registry,
        "voice_current",
        "查询当前启用音色和 TTS 可用状态。",
        "voice",
        ToolRisk::ReadOnly,
        false,
        serde_json::json!({ "type": "object", "properties": {}, "required": [] }),
    );
    register_web_runtime_tool(
        registry,
        "file_read",
        "读取允许工作区内的文件。",
        "filesystem",
        ToolRisk::ReadOnly,
        false,
        serde_json::json!({
            "type": "object",
            "properties": { "path": { "type": "string", "description": "要读取的文件路径" } },
            "required": ["path"]
        }),
    );
    register_web_runtime_tool(
        registry,
        "file_list",
        "列出允许工作区内的目录内容。",
        "filesystem",
        ToolRisk::ReadOnly,
        false,
        serde_json::json!({
            "type": "object",
            "properties": { "path": { "type": "string", "description": "要列出的目录路径，默认工作区根目录" } },
            "required": []
        }),
    );
    register_web_runtime_tool(
        registry,
        "file_search",
        "按文件名、路径片段或内容搜索本地文件/目录。适合把“docker 文件夹”这类模糊路径收敛成候选路径；默认有深度和数量限制。",
        "filesystem",
        ToolRisk::ReadOnly,
        false,
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "搜索关键词、文件名或目录名片段" },
                "base": { "type": "string", "description": "搜索起点，默认当前项目工作区；工作区外路径必须先获得用户审批" },
                "kind": { "type": "string", "description": "搜索类型：any、file、directory，默认 any" },
                "match": { "type": "string", "description": "匹配方式：name、path、content，默认 name" },
                "max_depth": { "type": "integer", "description": "最大搜索深度，默认 4，最大 8" },
                "include_hidden": { "type": "boolean", "description": "是否包含点开头隐藏文件/目录，默认 false；query 以点开头时自动包含" },
                "limit": { "type": "integer", "description": "最多返回多少条结果，默认 20，最大 100" }
            },
            "required": ["query"]
        }),
    );
    register_web_runtime_tool(
        registry,
        "file_write",
        "创建新文件或全文覆盖文件，执行前需要用户审批。",
        "filesystem",
        ToolRisk::WriteFile,
        true,
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "目标文件路径" },
                "content": { "type": "string", "description": "完整文件内容" }
            },
            "required": ["path", "content"]
        }),
    );
    register_web_runtime_tool(
        registry,
        "file_edit",
        "使用 old_string/new_string 精确替换文件内容，执行前需要用户审批。",
        "filesystem",
        ToolRisk::WriteFile,
        true,
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "目标文件路径" },
                "old_string": { "type": "string", "description": "待替换的唯一原文" },
                "new_string": { "type": "string", "description": "替换后的内容" }
            },
            "required": ["path", "old_string", "new_string"]
        }),
    );
    register_web_runtime_tool(
        registry,
        "command_run",
        "执行本地命令，执行前需要用户审批。",
        "command",
        ToolRisk::ExecuteCommand,
        true,
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": { "type": "string", "description": "要执行的命令" },
                "cwd": { "type": "string", "description": "工作目录，默认项目根目录" },
                "timeout_ms": { "type": "integer", "description": "命令超时时间，默认 120000，最大 120000" },
                "audit_output": { "type": "boolean", "description": "是否将 stdout/stderr 流式归档到 Muse 受控数据目录；默认 false，每个流最多 16 MiB，不接受自定义路径" }
            },
            "required": ["command"]
        }),
    );
    register_web_runtime_tool(
        registry,
        "web_fetch",
        "抓取指定 URL 内容，联网前需要用户审批。",
        "network",
        ToolRisk::Network,
        true,
        serde_json::json!({
            "type": "object",
            "properties": { "url": { "type": "string", "description": "要抓取的 URL" } },
            "required": ["url"]
        }),
    );
    register_web_runtime_tool(
        registry,
        "session_list",
        "查询本地会话列表。",
        "session",
        ToolRisk::ReadOnly,
        false,
        serde_json::json!({ "type": "object", "properties": {}, "required": [] }),
    );
    register_web_runtime_tool(
        registry,
        "session_read",
        "读取指定会话摘要或消息。",
        "session",
        ToolRisk::ReadOnly,
        false,
        serde_json::json!({
            "type": "object",
            "properties": { "conversation_id": { "type": "string" } },
            "required": []
        }),
    );
    register_web_runtime_tool(
        registry,
        "tool_result_read",
        "读取已外置归档的大工具结果。模型在工具返回 tool_result_ref 后，可用 result_id 拉取完整内容或指定范围片段。",
        "session",
        ToolRisk::ReadOnly,
        false,
        serde_json::json!({
            "type": "object",
            "properties": {
                "result_id": { "type": "string", "description": "工具结果引用中的 result_id" },
                "offset": { "type": "integer", "description": "从第几个字符开始读取，默认 0" },
                "limit": { "type": "integer", "description": "最多读取多少字符，默认 20000，最大 50000" }
            },
            "required": ["result_id"]
        }),
    );
    register_web_runtime_tool(
        registry,
        "session_compact",
        "压缩当前会话上下文，执行前需要用户审批。",
        "session",
        ToolRisk::ExternalSideEffect,
        true,
        serde_json::json!({ "type": "object", "properties": {}, "required": [] }),
    );
    register_web_runtime_tool(
        registry,
        "model_info",
        "查询当前模型、provider 和上下文配置。",
        "model",
        ToolRisk::ReadOnly,
        false,
        serde_json::json!({ "type": "object", "properties": {}, "required": [] }),
    );
    register_web_runtime_tool(
        registry,
        "persona_info",
        "查询当前角色、工具策略和视觉配置。",
        "persona",
        ToolRisk::ReadOnly,
        false,
        serde_json::json!({ "type": "object", "properties": {}, "required": [] }),
    );
    register_web_runtime_tool(
        registry,
        "persona_switch",
        "请求切换角色，执行前需要用户审批。",
        "persona",
        ToolRisk::ExternalSideEffect,
        true,
        serde_json::json!({
            "type": "object",
            "properties": { "persona_id": { "type": "string", "description": "目标角色 ID" } },
            "required": ["persona_id"]
        }),
    );
    register_web_runtime_tool(
        registry,
        "mcp_list_resources",
        "列出当前 harness 暴露的本地与外部 MCP 资源。",
        "mcp",
        ToolRisk::ReadOnly,
        false,
        serde_json::json!({
            "type": "object",
            "properties": {
                "server": { "type": "string", "description": "资源所属 server；省略时聚合本地 muse-local 与已配置外部 MCP server。" },
                "cursor": { "type": "string", "description": "分页游标；只能在指定单个外部 server 时使用。" }
            },
            "required": []
        }),
    );
    register_web_runtime_tool(
        registry,
        "mcp_list_resource_templates",
        "列出已配置外部 MCP server 暴露的 resource template。",
        "mcp",
        ToolRisk::ReadOnly,
        false,
        serde_json::json!({
            "type": "object",
            "properties": {
                "server": { "type": "string", "description": "外部 MCP server 名称；省略时聚合所有已配置外部 server。" },
                "cursor": { "type": "string", "description": "分页游标；只能在指定单个外部 server 时使用。" }
            },
            "required": []
        }),
    );
    register_web_runtime_tool(
        registry,
        "mcp_read_resource",
        "读取当前 harness 暴露的本地或外部 MCP 只读资源。",
        "mcp",
        ToolRisk::ReadOnly,
        false,
        serde_json::json!({
            "type": "object",
            "properties": {
                "uri": { "type": "string", "description": "资源 URI" },
                "server": { "type": "string", "description": "资源所属 server；省略时默认读取 muse-local，本地资源必须省略或填写 muse-local。" }
            },
            "required": ["uri"]
        }),
    );
}

//! 工具系统核心模块，定义模型可见工具、风险等级、审批请求和工具注册表。

pub mod builtin;

use crate::domain::persona::{ToolPolicy, ToolPolicyMode};
use crate::domain::runtime::ToolPreset;
use crate::domain::turn::TurnContext;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

/// 工具风险等级，供前端展示和运行时审批使用。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolRisk {
    ReadOnly,
    ExternalSideEffect,
    Network,
    WriteFile,
    ExecuteCommand,
}

impl ToolRisk {
    /// 判断该风险等级在默认策略下是否需要用户审批。
    pub fn requires_approval_by_default(&self) -> bool {
        matches!(
            self,
            ToolRisk::Network
                | ToolRisk::WriteFile
                | ToolRisk::ExecuteCommand
                | ToolRisk::ExternalSideEffect
        )
    }

    /// 返回写入协议和前端事件时使用的稳定风险字符串。
    pub fn as_str(&self) -> &'static str {
        match self {
            ToolRisk::ReadOnly => "read_only",
            ToolRisk::ExternalSideEffect => "external_side_effect",
            ToolRisk::Network => "network",
            ToolRisk::WriteFile => "write_file",
            ToolRisk::ExecuteCommand => "execute_command",
        }
    }
}

/// 工具执行归属，区分模型可见定义和真实执行位置。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolExecutionOwner {
    Core,
    WebRuntime,
    ExternalProvider,
    Disabled,
}

impl ToolExecutionOwner {
    /// 判断该归属是否代表工具当前可被真实执行。
    pub fn is_available(&self) -> bool {
        !matches!(self, ToolExecutionOwner::Disabled)
    }
}

/// 工具调用来源。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolCallSource {
    Native,
    TextJsonFallback,
}

/// 模型请求调用工具的结构化对象。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub call_id: String,
    pub name: String,
    pub arguments: serde_json::Value,
    pub source: ToolCallSource,
}

/// 单次工具执行上下文。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolInvocation {
    pub call: ToolCall,
    pub turn_context: TurnContext,
    pub approved: bool,
}

/// 工具审批请求。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalRequest {
    pub approval_id: String,
    pub call_id: String,
    pub name: String,
    pub risk: ToolRisk,
    pub message: String,
    pub detail: Option<String>,
    pub arguments: serde_json::Value,
}

/// 工具执行状态。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolResultStatus {
    Success,
    Failed,
}

impl ToolResultStatus {
    /// 将布尔执行结果转换为稳定的工具状态枚举。
    pub fn from_success(success: bool) -> Self {
        if success { Self::Success } else { Self::Failed }
    }

    /// 判断当前工具结果是否代表成功执行。
    pub fn is_success(&self) -> bool {
        matches!(self, Self::Success)
    }
}

/// 工具执行结果。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    /// 执行状态。
    pub status: ToolResultStatus,
    /// 返回给大模型阅读的文本内容。
    pub content: String,
    /// 给前端展示的结构化数据（可选）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub structured: Option<serde_json::Value>,
}

impl ToolResult {
    /// 构造成功的工具结果，允许附带前端可展示的结构化数据。
    pub fn success(content: impl Into<String>, structured: Option<serde_json::Value>) -> Self {
        Self {
            status: ToolResultStatus::Success,
            content: content.into(),
            structured,
        }
    }

    /// 构造失败的工具结果，允许附带错误原因或诊断结构化数据。
    pub fn failed(content: impl Into<String>, structured: Option<serde_json::Value>) -> Self {
        Self {
            status: ToolResultStatus::Failed,
            content: content.into(),
            structured,
        }
    }

    /// 判断当前工具结果是否为成功状态。
    pub fn is_success(&self) -> bool {
        self.status.is_success()
    }
}

/// 给大模型看的工具定义（用于函数调用提示词）。
#[derive(Debug, Clone, Serialize)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
    pub category: String,
    pub risk: ToolRisk,
    pub requires_approval: bool,
    pub execution_owner: ToolExecutionOwner,
    pub available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disabled_reason: Option<String>,
}

/// 网页搜索工具的标准化请求。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebSearchToolRequest {
    pub query: String,
    #[serde(default = "default_web_search_limit")]
    pub limit: usize,
}

// 返回网页搜索工具的默认结果数量限制。
fn default_web_search_limit() -> usize {
    5
}

/// 外部查询型工具的统一接入协议。
pub trait ExternalToolProvider: Send + Sync {
    /// 查询网页搜索类外部信息。
    fn web_search(&self, request: &WebSearchToolRequest) -> ToolResult;

    /// 判断网页搜索能力是否已经接入真实提供器。
    fn web_search_available(&self) -> bool {
        true
    }

    /// 返回网页搜索不可用时的稳定诊断原因。
    fn web_search_disabled_reason(&self) -> Option<String> {
        None
    }
}

/// 默认的未配置提供器，用于在首轮仅保留统一接口。
pub struct DisabledExternalToolProvider;

impl ExternalToolProvider for DisabledExternalToolProvider {
    /// 未配置网页搜索服务时返回明确占位结果，避免模型误以为真实查询成功。
    fn web_search(&self, request: &WebSearchToolRequest) -> ToolResult {
        ToolResult {
            status: ToolResultStatus::Failed,
            content: format!(
                "当前版本尚未配置 Web 搜索服务，已预留统一接入层。查询内容：{}。",
                request.query
            ),
            structured: Some(serde_json::json!({
                "provider": "disabled",
                "kind": "web_search",
                "query": request.query,
                "limit": request.limit,
            })),
        }
    }

    fn web_search_available(&self) -> bool {
        false
    }

    fn web_search_disabled_reason(&self) -> Option<String> {
        Some("当前版本尚未配置 Web 搜索服务。".to_string())
    }
}

/// 构造默认外部工具提供器，首启时用于占位搜索能力。
pub fn default_external_tool_provider() -> Arc<dyn ExternalToolProvider> {
    Arc::new(DisabledExternalToolProvider)
}

/// 工具执行函数签名。旧闭包工具仍保留，新的网页运行底座会优先使用结构化执行器。
pub type ToolHandler = Arc<dyn Fn(serde_json::Value) -> ToolResult + Send + Sync>;

/// 单个工具的定义。
pub struct Tool {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
    pub category: String,
    pub risk: ToolRisk,
    pub requires_approval: bool,
    pub expose_to_model: bool,
    pub execution_owner: ToolExecutionOwner,
    pub available: bool,
    pub disabled_reason: Option<String>,
    pub handler: ToolHandler,
}

/// 新版工具处理器接口。当前网页层先承接运行态工具，核心侧保留接口边界。
#[async_trait]
pub trait PersonaToolHandler: Send + Sync {
    /// 返回该工具暴露给模型和前端的稳定定义。
    fn spec(&self) -> ToolDef;

    /// 校验模型传入参数的结构与业务约束。
    fn validate_input(&self, _input: &serde_json::Value) -> Result<(), String> {
        Ok(())
    }

    /// 校验当前轮次上下文是否允许执行该工具。
    fn check_permissions(
        &self,
        _ctx: &TurnContext,
        _input: &serde_json::Value,
    ) -> Result<(), String> {
        Ok(())
    }

    /// 判断本次调用是否只读，用于审批与并发策略。
    fn is_read_only(&self, input: &serde_json::Value) -> bool {
        matches!(self.spec().risk, ToolRisk::ReadOnly)
            || input.get("_readonly").and_then(|value| value.as_bool()) == Some(true)
    }

    /// 执行工具并返回模型可读、前端可展示的结果。
    async fn call(&self, invocation: ToolInvocation) -> ToolResult;
}

/// 工具注册与查找的中心。
pub struct ToolRegistry {
    tools: HashMap<String, Tool>,
}

impl ToolRegistry {
    /// 创建空工具注册表。
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
        }
    }

    /// 注册一个工具。
    pub fn register(&mut self, tool: Tool) {
        self.tools.insert(tool.name.clone(), tool);
    }

    /// 获取所有工具的大模型描述（用于拼入系统提示词）。
    pub fn list_definitions(&self) -> Vec<ToolDef> {
        let mut defs: Vec<ToolDef> = self
            .tools
            .values()
            .filter(|tool| tool.expose_to_model && tool.available)
            .map(tool_definition)
            .collect();
        defs.sort_by(|a, b| a.name.cmp(&b.name));
        defs
    }

    /// 根据角色工具策略过滤可见工具定义。
    pub fn list_definitions_for_policy(&self, policy: Option<&ToolPolicy>) -> Vec<ToolDef> {
        Self::filter_definitions_for_policy(self.list_definitions(), policy)
    }

    /// 根据运行模式工具预设和角色工具策略过滤可见工具定义。
    pub fn list_definitions_for_preset_and_policy(
        &self,
        preset: ToolPreset,
        policy: Option<&ToolPolicy>,
    ) -> Vec<ToolDef> {
        Self::filter_definitions_for_preset_and_policy(self.list_definitions(), preset, policy)
    }

    /// 根据角色工具策略生成工具提示词。
    pub fn tools_prompt_for_policy(&self, policy: Option<&ToolPolicy>) -> String {
        let defs = self.list_definitions_for_policy(policy);
        Self::tools_prompt_from_definitions(&defs)
    }

    /// 按角色工具策略过滤外部合并后的工具定义。
    pub fn filter_definitions_for_policy(
        defs: Vec<ToolDef>,
        policy: Option<&ToolPolicy>,
    ) -> Vec<ToolDef> {
        defs.into_iter()
            .filter(|definition| {
                definition.available && is_tool_definition_allowed(policy, &definition.name)
            })
            .collect()
    }

    /// 按运行模式工具预设过滤外部合并后的工具定义。
    pub fn filter_definitions_for_preset(defs: Vec<ToolDef>, preset: ToolPreset) -> Vec<ToolDef> {
        defs.into_iter()
            .filter(|definition| Self::is_tool_definition_allowed_for_preset(definition, preset))
            .collect()
    }

    /// 按运行模式工具预设和角色工具策略过滤外部合并后的工具定义。
    pub fn filter_definitions_for_preset_and_policy(
        defs: Vec<ToolDef>,
        preset: ToolPreset,
        policy: Option<&ToolPolicy>,
    ) -> Vec<ToolDef> {
        Self::filter_definitions_for_policy(
            Self::filter_definitions_for_preset(defs, preset),
            policy,
        )
    }

    /// 判断工具定义是否属于当前运行模式工具预设。
    pub fn is_tool_definition_allowed_for_preset(definition: &ToolDef, preset: ToolPreset) -> bool {
        is_tool_allowed_for_preset(definition, preset)
    }

    /// 把外部合并后的工具定义渲染成模型可读提示词。
    pub fn tools_prompt_from_definitions(defs: &[ToolDef]) -> String {
        if defs.is_empty() {
            return String::new();
        }
        let mut s = String::from("【可用工具】你可以调用以下工具获取信息或请求本地动作：\n\n");
        for d in defs {
            let interrupt_behavior = tool_interrupt_behavior_for_model(&d.name);
            s.push_str(&format!(
                "- {}：{}\n  分类：{}\n  风险：{}\n  需要审批：{}\n  中断策略：{}\n  参数：{}\n\n",
                d.name,
                d.description,
                d.category,
                d.risk.as_str(),
                if d.requires_approval { "是" } else { "否" },
                interrupt_behavior,
                d.parameters
            ));
        }
        s.push_str("当需要调用工具时，优先使用 provider 原生工具调用，不要把工具参数写进普通 assistant 文本。只有在当前模型或服务不支持原生工具调用时，才使用兼容文本协议：只在回复开头输出一行 JSON：{\"tool_call\":{\"name\":\"工具名\",\"arguments\":{...}}}，不要同时写最终回复；系统会把工具调用转成结构化 ToolCall，拿到工具结果后再继续生成最终回复。需要审批的工具会先暂停等待用户确认。\n");
        s.push_str("阶段简报规则：长任务或多步任务执行中应适时调用 send_user_message 向用户同步阶段进展；只有确实需要用户立即确认才能继续时，才设置 requires_reply=true。brief 是不等待回复的短别名。\n");
        s.push_str("技能规则：先根据本轮冻结的 Skill 元数据目录判断是否适用；需要完整指引时只调用 load_skill。旧 use_skill 和 skill 仅用于历史协议兼容，不会暴露给新 Turn。\n");
        s.push_str("子任务规则：agent 只用于登记当前运行时内的轻量子任务、刷新任务清单并回灌上下文；它不会启动并发子模型，也不代表后台任务队列。\n");
        s.push_str("路径定位规则：运行环境上下文已经提供当前工作区、权限模式和沙箱模式。优先使用运行环境上下文、用户明确给出的路径、`~`/用户 Home、当前项目工作区和消息上下文解析目标路径。用户说“下载文件夹、桌面、文稿、项目目录”等模糊位置时，先推导最小候选路径；候选唯一就直接调用目标工具，让 harness 负责审批；候选多个时按 Codex 风格选择最像用户意图的路径：当前工作区优先，其次 Home 下浅层的非隐藏用户目录，再其次用户明确提到的隐藏目录或配置目录，并在最终回复中说明选择依据；只有候选含义完全等价或缺少文件名时再向用户澄清。不要为了确认权限或寻找用户目录去调用 file_list 列出 `/`、`/Users`、用户 Home 根目录等宽泛目录。\n");
        s.push_str("文件工具规则：file_list 只用于用户明确要求列目录，或在已知的狭窄目录中查找；模糊路径、文件夹名、文件名定位优先使用 file_search。对“docker 文件夹”这类描述，先在当前工作区用 file_search 搜 directory/name，找不到再在 `~` 下做有限深度搜索；默认不要包含隐藏目录，除非用户明确说隐藏目录、配置目录或点开头目录。如果返回多个候选，优先选择 Home 下浅层、非隐藏、语义上最像用户个人目录的候选；不要把 `.docker` 这类配置目录当成普通“docker 文件夹”，除非用户明确说隐藏目录或 Docker 配置目录。写入/编辑外部目录不代表被允许，harness 会根据权限模式暂停审批或拒绝。\n");
        s.push_str("用户提问规则：当你确实需要用户在多个方案、偏好或缺失信息之间做选择时，调用 ask_user_question；不要把多选题写成普通 assistant 文本。每个问题提供 2 到 4 个选项，默认按单选互斥处理；只有确实需要多选时才设置 multiSelect=true。推荐项放第一位并在 label 末尾写“（推荐）”。不要提供 Other/其他选项，前端会自动提供自定义输入。不要用 ask_user_question 询问“是否批准计划/是否继续执行”，这类继续执行边界由 harness 或专用流程处理。\n");
        s.push_str("语音规则：不要给 tts_speak 传 voice_id、voice_name 或 speaker，语音永远使用用户当前启用音色。\n");
        s
    }

    /// 判断指定工具是否被当前角色策略允许。
    pub fn is_tool_allowed(&self, policy: Option<&ToolPolicy>, name: &str) -> bool {
        self.tool_def(name).is_some_and(|definition| {
            definition.available && is_tool_definition_allowed(policy, name)
        })
    }

    /// 判断指定工具是否同时满足运行模式工具预设和角色策略。
    pub fn is_tool_allowed_for_preset_and_policy(
        &self,
        preset: ToolPreset,
        policy: Option<&ToolPolicy>,
        name: &str,
    ) -> bool {
        self.tool_def(name).is_some_and(|definition| {
            Self::is_tool_definition_allowed_for_preset(&definition, preset)
                && is_tool_definition_allowed(policy, name)
        })
    }

    /// 执行指定工具。旧路径保留，完整工具由网页运行底座执行器承接。
    pub fn execute(&self, name: &str, arguments: serde_json::Value) -> Option<ToolResult> {
        self.tools.get(name).map(|tool| {
            if !tool.available {
                return disabled_tool_result(tool);
            }
            (tool.handler)(arguments)
        })
    }

    /// 在角色权限约束下执行工具，未授权或不存在时返回失败结果。
    pub fn execute_authorized(
        &self,
        policy: Option<&ToolPolicy>,
        name: &str,
        arguments: serde_json::Value,
    ) -> ToolResult {
        if !self.tools.contains_key(name) {
            return ToolResult {
                status: ToolResultStatus::Failed,
                content: format!("工具 `{name}` 不存在，无法执行。"),
                structured: Some(serde_json::json!({
                    "name": name,
                    "reason": "not_found"
                })),
            };
        }

        if let Some(definition) = self.tool_def(name)
            && !definition.available
        {
            return ToolResult {
                status: ToolResultStatus::Failed,
                content: format!(
                    "工具 `{name}` 当前不可用：{}",
                    definition
                        .disabled_reason
                        .unwrap_or_else(|| "未提供不可用原因。".to_string())
                ),
                structured: Some(serde_json::json!({
                    "name": name,
                    "reason": "disabled",
                    "execution_owner": definition.execution_owner,
                })),
            };
        }

        if !self.is_tool_allowed(policy, name) {
            return ToolResult {
                status: ToolResultStatus::Failed,
                content: format!("当前角色未被授权调用工具 `{name}`。"),
                structured: Some(serde_json::json!({
                    "name": name,
                    "reason": "unauthorized"
                })),
            };
        }

        self.execute(name, arguments).unwrap_or(ToolResult {
            status: ToolResultStatus::Failed,
            content: format!("工具 `{name}` 执行失败。"),
            structured: Some(serde_json::json!({
                "name": name,
                "reason": "execution_failed"
            })),
        })
    }

    /// 解析文本中是否包含 tool_call JSON 指令。
    pub fn parse_tool_call(text: &str) -> Option<ToolCall> {
        let mut start = None;
        let mut depth = 0usize;
        let mut in_string = false;
        let mut escaped = false;

        for (index, ch) in text.char_indices() {
            if start.is_none() {
                if ch == '{' {
                    start = Some(index);
                    depth = 1;
                    in_string = false;
                    escaped = false;
                }
                continue;
            }

            if in_string {
                if escaped {
                    escaped = false;
                } else if ch == '\\' {
                    escaped = true;
                } else if ch == '"' {
                    in_string = false;
                }
                continue;
            }

            match ch {
                '"' => in_string = true,
                '{' => depth += 1,
                '}' => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        let Some(start_index) = start.take() else {
                            continue;
                        };
                        let end_index = index + ch.len_utf8();
                        let candidate = &text[start_index..end_index];
                        if let Ok(val) = serde_json::from_str::<serde_json::Value>(candidate)
                            && let Some(call) = parse_tool_call_value(&val)
                        {
                            return Some(call);
                        }
                    }
                }
                _ => {}
            }
        }
        None
    }

    /// 查询工具元数据。
    pub fn tool_def(&self, name: &str) -> Option<ToolDef> {
        self.tools.get(name).map(tool_definition)
    }
}

// 将内部工具记录转换为稳定的模型/前端可读定义。
fn tool_definition(tool: &Tool) -> ToolDef {
    ToolDef {
        name: tool.name.clone(),
        description: tool.description.clone(),
        parameters: tool.parameters.clone(),
        category: tool.category.clone(),
        risk: tool.risk.clone(),
        requires_approval: tool.requires_approval,
        execution_owner: tool.execution_owner.clone(),
        available: tool.available,
        disabled_reason: tool.disabled_reason.clone(),
    }
}

// 为禁用工具生成统一失败结果，避免旧执行路径误报真实调用失败。
fn disabled_tool_result(tool: &Tool) -> ToolResult {
    let reason = tool
        .disabled_reason
        .clone()
        .unwrap_or_else(|| "工具当前不可用。".to_string());
    ToolResult {
        status: ToolResultStatus::Failed,
        content: format!("工具 `{}` 当前不可用：{reason}", tool.name),
        structured: Some(serde_json::json!({
            "name": tool.name.clone(),
            "reason": "disabled",
            "execution_owner": tool.execution_owner.clone(),
        })),
    }
}

// 判断工具定义是否允许被当前角色策略暴露或执行。
fn is_tool_definition_allowed(policy: Option<&ToolPolicy>, name: &str) -> bool {
    match policy.map(|value| &value.mode) {
        // 默认角色只暴露陪伴、澄清、语音和网页搜索能力。开发者工具必须由
        // 用户在角色的显式白名单中开启，避免模型在普通对话中获得文件或命令权限。
        None | Some(ToolPolicyMode::Inherit) => is_default_assistant_tool(name),
        Some(ToolPolicyMode::Disabled) => false,
        Some(ToolPolicyMode::AllowList) => policy
            .map(|value| value.allowed_tools.iter().any(|item| item == name))
            .unwrap_or(false),
    }
}

// 默认角色助手能力，不包含文件、命令、MCP、技能和任务编排等开发者扩展。
fn is_default_assistant_tool(name: &str) -> bool {
    matches!(
        name,
        "ask_user_question"
            | "send_user_message"
            | "brief"
            | "tts_speak"
            | "voice_current"
            | "web_search"
    )
}

// 判断工具是否属于当前运行模式预设。
fn is_tool_allowed_for_preset(definition: &ToolDef, preset: ToolPreset) -> bool {
    match preset {
        ToolPreset::Daily => is_daily_tool(&definition.name),
        ToolPreset::FocusPlan => {
            is_focus_plan_tool(&definition.name)
                || (definition.name.starts_with("mcp__")
                    && matches!(definition.risk, ToolRisk::ReadOnly))
        }
        ToolPreset::FocusBuild => true,
    }
}

// 日常模式仅保留角色助手默认能力；开发者扩展需要显式白名单后才会通过权限过滤。
fn is_daily_tool(name: &str) -> bool {
    matches!(
        name,
        "ask_user_question"
            | "send_user_message"
            | "brief"
            | "tts_speak"
            | "voice_current"
            | "web_search"
    )
}

// 专注计划预设工具池：允许读、查、问和会话/MCP 只读资源，不开放写入或命令执行。
fn is_focus_plan_tool(name: &str) -> bool {
    matches!(
        name,
        "enter_plan_mode"
            | "exit_plan_mode"
            | "todo_write"
            | "ask_user_question"
            | "send_user_message"
            | "brief"
            | "load_skill"
            | "use_skill"
            | "skill"
            | "file_read"
            | "file_list"
            | "file_search"
            | "web_search"
            | "web_fetch"
            | "session_read"
            | "session_compact"
            | "mcp_list_resources"
            | "mcp_list_resource_templates"
            | "mcp_read_resource"
    )
}

// 返回模型提示词中使用的工具中断策略说明。
fn tool_interrupt_behavior_for_model(name: &str) -> &'static str {
    match name {
        "command_run" | "file_search" | "web_fetch" | "web_search" => "cancel，可随当前 turn 停止",
        _ => "block，停止时等待工具收尾",
    }
}

impl Default for ToolRegistry {
    // 提供标准 Default 入口，和显式 `new` 保持同一初始化语义。
    fn default() -> Self {
        Self::new()
    }
}

// 从任意兼容 JSON 结构中提取工具调用。
fn parse_tool_call_value(value: &serde_json::Value) -> Option<ToolCall> {
    if let Some(call) = value
        .get("tool_call")
        .and_then(parse_wrapped_tool_call_value)
        .or_else(|| {
            value
                .get("function_call")
                .and_then(parse_wrapped_tool_call_value)
        })
        .or_else(|| {
            value
                .get("tool_use")
                .and_then(parse_wrapped_tool_call_value)
        })
        .or_else(|| {
            value
                .get("tool")
                .filter(|item| item.is_object())
                .and_then(parse_wrapped_tool_call_value)
        })
    {
        return Some(call);
    }

    if let Some(call) = value
        .get("tool_calls")
        .and_then(|item| item.as_array())
        .and_then(|items| items.iter().find_map(parse_tool_call_value))
    {
        return Some(call);
    }

    if let Some(function) = value.get("function").filter(|item| item.is_object())
        && let Some(call) = parse_wrapped_tool_call_value(function)
    {
        return Some(call);
    }

    if !looks_like_direct_tool_call_value(value) {
        return None;
    }

    parse_wrapped_tool_call_value(value)
}

// 解析被 tool_call、function_call 或 tool_use 包装的调用对象。
fn parse_wrapped_tool_call_value(value: &serde_json::Value) -> Option<ToolCall> {
    let name = tool_call_name(value)?;
    let arguments = tool_call_arguments(value);
    Some(ToolCall {
        call_id: tool_call_id(value),
        name,
        arguments,
        source: ToolCallSource::TextJsonFallback,
    })
}

// 判断普通 JSON 是否足够像直接工具调用，避免误吞普通结构化文本。
fn looks_like_direct_tool_call_value(value: &serde_json::Value) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };

    let explicit_type = value
        .get("type")
        .and_then(|item| item.as_str())
        .is_some_and(|kind| {
            matches!(
                kind,
                "tool_call" | "function_call" | "tool_use" | "function"
            )
        });
    let has_tool_name_key = object.contains_key("tool_name");
    let has_action_shape = object.contains_key("action") && object.contains_key("action_input");
    let has_direct_arguments = object.contains_key("name")
        && ["arguments", "parameters", "input", "args"]
            .iter()
            .any(|key| object.contains_key(*key));

    explicit_type || has_tool_name_key || has_action_shape || has_direct_arguments
}

// 从多种兼容字段中提取工具名。
fn tool_call_name(value: &serde_json::Value) -> Option<String> {
    value
        .get("name")
        .or_else(|| value.get("tool_name"))
        .or_else(|| value.get("tool"))
        .or_else(|| value.get("action"))
        .and_then(|item| item.as_str())
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(ToString::to_string)
}

// 从多种兼容字段中提取工具参数，并兼容字符串化 JSON。
fn tool_call_arguments(value: &serde_json::Value) -> serde_json::Value {
    let Some(arguments) = value
        .get("arguments")
        .or_else(|| value.get("parameters"))
        .or_else(|| value.get("input"))
        .or_else(|| value.get("args"))
        .or_else(|| value.get("action_input"))
    else {
        return serde_json::json!({});
    };

    if let Some(text) = arguments.as_str() {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return serde_json::json!({});
        }
        return serde_json::from_str::<serde_json::Value>(trimmed)
            .unwrap_or_else(|_| serde_json::json!({ "_raw_arguments": text }));
    }

    arguments.clone()
}

// 获取模型给出的调用 ID；缺失时生成稳定前缀的本地 ID。
fn tool_call_id(value: &serde_json::Value) -> String {
    value
        .get("call_id")
        .or_else(|| value.get("tool_call_id"))
        .or_else(|| value.get("id"))
        .and_then(|item| item.as_str())
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(ToString::to_string)
        .unwrap_or_else(|| {
            format!(
                "tool-{}",
                chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
            )
        })
}

#[cfg(test)]
mod tests {
    use super::builtin;
    use super::{ToolExecutionOwner, ToolRegistry};
    use crate::domain::persona::{ToolPolicy, ToolPolicyMode};
    use crate::domain::runtime::ToolPreset;

    // 验证 allow-list 策略只暴露白名单工具。
    #[test]
    fn filters_tools_for_allow_list_policy() {
        let mut registry = ToolRegistry::new();
        builtin::register_all(&mut registry);
        let policy = ToolPolicy {
            mode: ToolPolicyMode::AllowList,
            allowed_tools: vec!["voice_current".to_string()],
        };

        let defs = registry.list_definitions_for_policy(Some(&policy));
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].name, "voice_current");
    }

    // 新 Turn 只看见 canonical 工具，旧别名仍保留在注册表供历史 dispatch 兼容。
    #[test]
    fn skill_aliases_are_dispatch_only() {
        let mut registry = ToolRegistry::new();
        builtin::register_all(&mut registry);

        let names = registry
            .list_definitions()
            .into_iter()
            .map(|definition| definition.name)
            .collect::<Vec<_>>();
        assert!(names.contains(&"load_skill".to_string()));
        assert!(!names.contains(&"use_skill".to_string()));
        assert!(!names.contains(&"skill".to_string()));
        assert!(registry.tool_def("use_skill").is_some());
        assert!(registry.tool_def("skill").is_some());
    }

    // 验证未授权工具执行会被注册表拦截。
    #[test]
    fn rejects_unauthorized_tool_execution() {
        let mut registry = ToolRegistry::new();
        builtin::register_all(&mut registry);
        let policy = ToolPolicy {
            mode: ToolPolicyMode::AllowList,
            allowed_tools: vec!["voice_current".to_string()],
        };

        let result = registry.execute_authorized(Some(&policy), "file_read", serde_json::json!({}));
        assert!(!result.is_success());
        assert!(result.content.contains("未被授权"));
    }

    // 验证日常模式只暴露低打扰工具，不把命令执行交给模型。
    #[test]
    fn daily_preset_hides_command_tools() {
        let mut registry = ToolRegistry::new();
        builtin::register_all(&mut registry);

        let defs = registry.list_definitions_for_preset_and_policy(ToolPreset::Daily, None);
        let names = defs
            .iter()
            .map(|definition| definition.name.as_str())
            .collect::<Vec<_>>();

        assert!(names.contains(&"web_search"));
        assert!(names.contains(&"send_user_message"));
        assert!(names.contains(&"tts_speak"));
        assert!(!names.contains(&"file_read"));
        assert!(!names.contains(&"load_skill"));
        assert!(!names.contains(&"agent"));
        assert!(!names.contains(&"command_run"));
        assert!(!names.contains(&"mcp_list_resources"));
    }

    // 验证专注计划预设只开放读查问和计划相关工具，不允许写文件或执行命令。
    #[test]
    fn focus_plan_preset_hides_mutating_tools() {
        let mut registry = ToolRegistry::new();
        builtin::register_all(&mut registry);

        let policy = ToolPolicy {
            mode: ToolPolicyMode::AllowList,
            allowed_tools: vec![
                "ask_user_question".to_string(),
                "todo_write".to_string(),
                "enter_plan_mode".to_string(),
                "exit_plan_mode".to_string(),
            ],
        };
        let defs =
            registry.list_definitions_for_preset_and_policy(ToolPreset::FocusPlan, Some(&policy));
        let names = defs
            .iter()
            .map(|definition| definition.name.as_str())
            .collect::<Vec<_>>();

        assert!(names.contains(&"ask_user_question"));
        assert!(names.contains(&"todo_write"));
        assert!(names.contains(&"enter_plan_mode"));
        assert!(names.contains(&"exit_plan_mode"));
        assert!(!names.contains(&"send_user_message"));
        assert!(!names.contains(&"agent"));
        assert!(!names.contains(&"mcp_read_resource"));
        assert!(!names.contains(&"file_write"));
        assert!(!names.contains(&"file_edit"));
        assert!(!names.contains(&"command_run"));
    }

    // 验证专注工作预设恢复完整工具池。
    #[test]
    fn focus_build_preset_exposes_execution_tools() {
        let mut registry = ToolRegistry::new();
        builtin::register_all(&mut registry);

        let policy = ToolPolicy {
            mode: ToolPolicyMode::AllowList,
            allowed_tools: vec![
                "file_write".to_string(),
                "file_edit".to_string(),
                "command_run".to_string(),
                "mcp_list_resources".to_string(),
                "agent".to_string(),
            ],
        };
        let defs =
            registry.list_definitions_for_preset_and_policy(ToolPreset::FocusBuild, Some(&policy));
        let names = defs
            .iter()
            .map(|definition| definition.name.as_str())
            .collect::<Vec<_>>();

        assert!(names.contains(&"file_write"));
        assert!(names.contains(&"file_edit"));
        assert!(names.contains(&"command_run"));
        assert!(names.contains(&"mcp_list_resources"));
        assert!(names.contains(&"agent"));
    }

    // 验证运行底座工具不再伪装成 core 闭包工具，而是声明真实 Web runtime 归属。
    #[test]
    fn web_runtime_tools_declare_execution_owner() {
        let mut registry = ToolRegistry::new();
        builtin::register_all(&mut registry);

        let def = registry
            .tool_def("todo_write")
            .expect("todo_write 应注册为运行底座工具");
        assert_eq!(def.execution_owner, ToolExecutionOwner::WebRuntime);
        assert!(def.available);
        assert!(def.disabled_reason.is_none());
    }

    // 验证网页搜索属于真实 Web runtime 工具；未配置凭据时由执行层返回诊断。
    #[test]
    fn web_search_is_a_visible_web_runtime_tool() {
        let mut registry = ToolRegistry::new();
        builtin::register_all(&mut registry);

        let visible_names = registry
            .list_definitions()
            .into_iter()
            .map(|definition| definition.name)
            .collect::<Vec<_>>();
        assert!(visible_names.iter().any(|name| name == "web_search"));

        let def = registry
            .tool_def("web_search")
            .expect("web_search 应注册为真实运行时入口");
        assert_eq!(def.execution_owner, ToolExecutionOwner::WebRuntime);
        assert!(def.available);
        assert!(def.requires_approval);
    }

    // 验证情绪行后面的兼容 JSON 工具调用能被解析。
    #[test]
    fn parses_tool_call_after_emotion_line() {
        let text = "{\"emotion\":\"happy\"}\n{\"tool_call\":{\"name\":\"voice_current\",\"arguments\":{}}}\n稍等一下";
        let parsed = ToolRegistry::parse_tool_call(text).expect("应能解析到工具调用");
        assert_eq!(parsed.name, "voice_current");
    }

    // 验证自然语言和代码块中的工具调用能被提取。
    #[test]
    fn parses_tool_call_embedded_after_natural_language() {
        let text = "先让我找找你的 docker 文件夹在哪里。\n```json\n{\"tool_call\":{\"name\":\"file_search\",\"arguments\":{\"query\":\"docker\",\"kind\":\"directory\"}}}\n```";
        let parsed = ToolRegistry::parse_tool_call(text).expect("应能解析到嵌入文本中的工具调用");
        assert_eq!(parsed.name, "file_search");
        assert_eq!(parsed.arguments["query"], "docker");
    }

    // 验证直接 name/arguments 形态可兼容不支持原生工具的模型。
    #[test]
    fn parses_direct_name_arguments_tool_call_for_compatible_models() {
        let text = r#"{"name":"file_read","arguments":{"path":"Cargo.toml"}}"#;
        let parsed = ToolRegistry::parse_tool_call(text).expect("应能解析直接工具调用 JSON");
        assert_eq!(parsed.name, "file_read");
        assert_eq!(parsed.arguments["path"], "Cargo.toml");
    }

    // 验证 OpenAI 风格 function_call 包装和字符串参数可被解析。
    #[test]
    fn parses_function_call_with_string_arguments() {
        let text = r#"{"function_call":{"name":"web_search","arguments":"{\"query\":\"harness 兼容性\",\"limit\":3}"}}"#;
        let parsed = ToolRegistry::parse_tool_call(text).expect("应能解析 function_call 包装");
        assert_eq!(parsed.name, "web_search");
        assert_eq!(parsed.arguments["query"], "harness 兼容性");
        assert_eq!(parsed.arguments["limit"], 3);
    }

    // 验证 ReAct 风格 action/action_input 调用可被解析。
    #[test]
    fn parses_action_input_tool_call_for_react_style_models() {
        let text = r#"{"action":"file_search","action_input":{"query":"Tool.ts","kind":"file"}}"#;
        let parsed =
            ToolRegistry::parse_tool_call(text).expect("应能解析 action/action_input 格式");
        assert_eq!(parsed.name, "file_search");
        assert_eq!(parsed.arguments["query"], "Tool.ts");
    }

    // 验证 tool_calls 数组包装里嵌套的函数调用可被解析。
    #[test]
    fn parses_tool_calls_array_wrapper() {
        let text = r#"{"tool_calls":[{"type":"function","function":{"name":"voice_current","arguments":"{}"}}]}"#;
        let parsed = ToolRegistry::parse_tool_call(text).expect("应能解析 tool_calls 数组包装");
        assert_eq!(parsed.name, "voice_current");
        assert_eq!(parsed.arguments, serde_json::json!({}));
    }

    // 验证普通 JSON 不会被误判成工具调用。
    #[test]
    fn ignores_plain_json_without_tool_call_shape() {
        let text = r#"{"name":"普通数据","content":"这不是工具调用"}"#;
        assert!(ToolRegistry::parse_tool_call(text).is_none());
    }

    // 验证未配置外部提供器时返回可见占位错误。
    #[test]
    fn disabled_external_provider_returns_placeholder_message() {
        let provider = super::default_external_tool_provider();
        let result = provider.web_search(&super::WebSearchToolRequest {
            query: "agent harness".to_string(),
            limit: 5,
        });
        assert!(!result.is_success());
        assert!(result.content.contains("尚未配置 Web 搜索服务"));
    }
}

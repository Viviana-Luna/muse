//! 单轮上下文模块，定义工具执行和运行时事件共享的轮次快照。

use crate::domain::persona::{McpPolicy, SkillPolicy, ToolPolicy};
use crate::domain::tool::ToolDef;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct RuntimePolicySnapshot {
    pub schema_version: u32,
    pub policy_version: String,
    pub persona_version: Option<String>,
    pub provider: String,
    pub model: String,
    pub tool_preset: String,
    pub tool_ids: Vec<String>,
    pub tool_policy: ToolPolicy,
    #[serde(default)]
    pub skill_policy: SkillPolicy,
    #[serde(default)]
    pub mcp_policy: McpPolicy,
    pub skill_revision: String,
    pub skill_catalog_hash: String,
    pub mcp_revision: u64,
    pub mcp_catalog_hash: String,
}

/// 单轮运行时上下文快照。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnContext {
    pub conversation_id: String,
    pub turn_id: String,
    pub persona_id: Option<String>,
    pub system_prompt: String,
    pub model_provider: String,
    pub model_name: String,
    pub tool_policy: ToolPolicy,
    pub skill_policy: SkillPolicy,
    pub mcp_policy: McpPolicy,
    pub runtime_mode: String,
    pub focus_phase: String,
    pub tool_preset: String,
    pub voice_enabled: bool,
    pub active_voice_id: Option<String>,
    pub created_at: String,
    #[serde(default)]
    pub runtime_policy: RuntimePolicySnapshot,
    /// 回合创建时冻结的模型可见工具定义；序列化 transcript 时不重复写入。
    #[serde(skip)]
    pub tool_definitions: Vec<ToolDef>,
}

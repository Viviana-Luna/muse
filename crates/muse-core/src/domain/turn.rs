//! 单轮上下文模块，定义工具执行和运行时事件共享的轮次快照。

use crate::domain::persona::{McpPolicy, SkillPolicy, ToolPolicy};
use crate::domain::tool::ToolDef;
use serde::{Deserialize, Serialize};

/// 冻结到单轮模型上下文中的 Skill 元数据；不包含正文和本机路径。
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct RuntimeSkillCatalogEntry {
    pub name: String,
    pub description: String,
    pub revision: String,
}

/// 单轮冻结的 MCP 工具审批事实；不包含远端描述、参数或本机秘密。
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct RuntimeMcpToolPolicyEntry {
    pub name: String,
    pub server: String,
    pub server_revision: String,
    pub annotations_hash: String,
    pub approval_policy: String,
    pub approval_source: String,
    pub final_risk: String,
    pub requires_approval: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct RuntimePolicySnapshot {
    pub schema_version: u32,
    pub policy_version: String,
    pub persona_version: Option<String>,
    pub provider: String,
    pub model: String,
    #[serde(default)]
    pub model_source: String,
    #[serde(default)]
    pub model_fallback: bool,
    #[serde(default)]
    pub model_fallback_reason: Option<String>,
    #[serde(default)]
    pub model_capabilities: Vec<String>,
    #[serde(default)]
    pub model_context_window: u64,
    #[serde(default)]
    pub model_max_output_tokens: u32,
    #[serde(default)]
    pub voice_id: Option<String>,
    #[serde(default)]
    pub voice_source: String,
    #[serde(default)]
    pub voice_fallback: bool,
    #[serde(default)]
    pub voice_fallback_reason: Option<String>,
    pub tool_preset: String,
    pub tool_ids: Vec<String>,
    pub tool_policy: ToolPolicy,
    #[serde(default)]
    pub skill_policy: SkillPolicy,
    #[serde(default)]
    pub mcp_policy: McpPolicy,
    pub skill_revision: String,
    pub skill_catalog_hash: String,
    #[serde(default)]
    pub skill_catalog: Vec<RuntimeSkillCatalogEntry>,
    #[serde(default)]
    pub omitted_skill_count: usize,
    pub mcp_revision: u64,
    pub mcp_catalog_hash: String,
    #[serde(default)]
    pub mcp_tool_policies: Vec<RuntimeMcpToolPolicyEntry>,
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
    #[serde(default)]
    pub model_source: String,
    #[serde(default)]
    pub model_fallback: bool,
    #[serde(default)]
    pub model_fallback_reason: Option<String>,
    #[serde(default)]
    pub model_capabilities: Vec<String>,
    #[serde(default)]
    pub model_context_window: u64,
    #[serde(default)]
    pub model_max_output_tokens: u32,
    pub tool_policy: ToolPolicy,
    pub skill_policy: SkillPolicy,
    pub mcp_policy: McpPolicy,
    pub runtime_mode: String,
    pub focus_phase: String,
    pub tool_preset: String,
    pub voice_enabled: bool,
    pub active_voice_id: Option<String>,
    #[serde(default)]
    pub voice_source: String,
    #[serde(default)]
    pub voice_fallback: bool,
    #[serde(default)]
    pub voice_fallback_reason: Option<String>,
    pub created_at: String,
    #[serde(default)]
    pub runtime_policy: RuntimePolicySnapshot,
    /// 回合创建时冻结的模型可见工具定义；序列化 transcript 时不重复写入。
    #[serde(skip)]
    pub tool_definitions: Vec<ToolDef>,
}

#[cfg(test)]
mod tests {
    use super::TurnContext;
    use crate::domain::persona::{McpPolicy, SkillPolicy, ToolPolicy};

    #[test]
    fn legacy_turn_context_defaults_new_model_and_voice_fields() {
        let current = TurnContext {
            conversation_id: "conversation-legacy".to_string(),
            turn_id: "turn-legacy".to_string(),
            persona_id: Some("persona-legacy".to_string()),
            system_prompt: "system".to_string(),
            model_provider: "deepseek".to_string(),
            model_name: "deepseek-v4-pro".to_string(),
            model_source: "global_active".to_string(),
            model_fallback: false,
            model_fallback_reason: None,
            model_capabilities: vec!["chat".to_string()],
            model_context_window: 1_000_000,
            model_max_output_tokens: 384_000,
            tool_policy: ToolPolicy::default(),
            skill_policy: SkillPolicy::default(),
            mcp_policy: McpPolicy::default(),
            runtime_mode: "daily".to_string(),
            focus_phase: "plan".to_string(),
            tool_preset: "daily".to_string(),
            voice_enabled: false,
            active_voice_id: None,
            voice_source: "unavailable".to_string(),
            voice_fallback: false,
            voice_fallback_reason: None,
            created_at: "2026-07-17T00:00:00Z".to_string(),
            runtime_policy: Default::default(),
            tool_definitions: Vec::new(),
        };
        let mut legacy = serde_json::to_value(current).expect("TurnContext 应可序列化");
        let object = legacy.as_object_mut().expect("TurnContext 应序列化为对象");
        for key in [
            "model_source",
            "model_fallback",
            "model_fallback_reason",
            "model_capabilities",
            "model_context_window",
            "model_max_output_tokens",
            "voice_source",
            "voice_fallback",
            "voice_fallback_reason",
        ] {
            object.remove(key);
        }

        let restored: TurnContext =
            serde_json::from_value(legacy).expect("旧 TurnContext 应继续可读");
        assert!(restored.model_source.is_empty());
        assert!(restored.model_capabilities.is_empty());
        assert_eq!(restored.model_context_window, 0);
        assert!(restored.voice_source.is_empty());
    }
}

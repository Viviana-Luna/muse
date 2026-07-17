//! 角色助手运行时契约。
//!
//! 本模块不依赖 HTTP 或 UI，负责单轮状态协调、冻结执行输入、硬预算控制
//! 与会话事件持久化。上层适配器只需持有协调器、回合租约和会话存储实例。

pub mod coordinator;
pub mod interactions;
pub mod service;
pub mod session;
pub mod session_metadata;

use std::collections::BTreeSet;
use std::fmt;
use std::future::Future;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use muse_core::domain::tool::ToolDef;
use muse_core::domain::turn::TurnContext;

/// 单轮执行的固定资源上限。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TurnBudget {
    /// 单轮最多请求模型的次数，包含工具结果后的续轮。
    pub max_model_requests: u8,
    /// 单轮最多执行工具的次数。
    pub max_tool_calls: u8,
    /// 单轮允许的最长执行时间。
    pub max_duration: Duration,
}

/// 回合开始时冻结的权限与 sandbox 边界。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrozenExecutionPolicy {
    pub permission_mode: String,
    pub sandbox_mode: String,
    pub allowed_roots: Vec<PathBuf>,
}

impl FrozenExecutionPolicy {
    pub fn new(
        permission_mode: impl Into<String>,
        sandbox_mode: impl Into<String>,
        allowed_roots: Vec<PathBuf>,
    ) -> Self {
        Self {
            permission_mode: permission_mode.into(),
            sandbox_mode: sandbox_mode.into(),
            allowed_roots,
        }
    }

    /// 当前设置只能收紧既有快照，不能在回合中扩大权限或 sandbox 范围。
    pub fn restricted_by(&self, current: &Self) -> Self {
        let permission_mode =
            if permission_rank(&current.permission_mode) < permission_rank(&self.permission_mode) {
                current.permission_mode.clone()
            } else {
                self.permission_mode.clone()
            };
        let sandbox_mode = if self.sandbox_mode == "workspace_write"
            || current.sandbox_mode == "workspace_write"
        {
            "workspace_write".to_string()
        } else {
            self.sandbox_mode.clone()
        };
        // 根目录求交集时必须保留更窄的一侧。比如冻结根是 `/workspace`，
        // 当前撤销后只剩 `/workspace/sub`，继续保留冻结根会把已撤销的父目录重新开放。
        let mut allowed_roots = BTreeSet::new();
        for frozen in &self.allowed_roots {
            for active in &current.allowed_roots {
                if frozen.starts_with(active) {
                    allowed_roots.insert(frozen.clone());
                } else if active.starts_with(frozen) {
                    allowed_roots.insert(active.clone());
                }
            }
        }
        Self {
            permission_mode,
            sandbox_mode,
            allowed_roots: allowed_roots.into_iter().collect(),
        }
    }
}

fn permission_rank(mode: &str) -> u8 {
    match mode {
        "full_access" => 2,
        "approve_for_me" => 1,
        _ => 0,
    }
}

impl Default for TurnBudget {
    fn default() -> Self {
        Self {
            max_model_requests: 8,
            max_tool_calls: 12,
            max_duration: Duration::from_secs(15 * 60),
        }
    }
}

/// 单轮预算执行错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnBudgetError {
    DeadlineExceeded { max_duration: Duration },
}

impl fmt::Display for TurnBudgetError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DeadlineExceeded { max_duration } => write!(
                formatter,
                "本轮已超过 {} 秒执行时限，已停止继续调用模型或工具。",
                max_duration.as_secs()
            ),
        }
    }
}

impl std::error::Error for TurnBudgetError {}

/// 已冻结的单轮运行时快照。
///
/// 角色、模型、工具定义和权限仅在创建时读取一次；后续设置变化不得改变
/// 当前回合的行为。计数器只在当前回合内递增。
#[derive(Debug, Clone)]
pub struct TurnSnapshot {
    /// 回合元数据与已渲染系统提示词。
    pub context: TurnContext,
    /// 已按当时权限过滤过的工具定义。
    pub tool_definitions: Vec<ToolDef>,
    /// 本回合适用的预算。
    pub budget: TurnBudget,
    full_tool_definitions: Vec<ToolDef>,
    execution_policy: FrozenExecutionPolicy,
    capability_epoch: u64,
    started_at: Instant,
    model_requests: u8,
    tool_calls: u8,
}

impl TurnSnapshot {
    /// 基于已冻结的回合上下文和工具列表创建快照。
    pub fn new(context: TurnContext, tool_definitions: Vec<ToolDef>) -> Self {
        Self::with_budget(context, tool_definitions, TurnBudget::default())
    }

    /// 使用显式预算创建快照，主要供测试和受限运行模式使用。
    pub fn with_budget(
        context: TurnContext,
        tool_definitions: Vec<ToolDef>,
        budget: TurnBudget,
    ) -> Self {
        Self::with_budget_started_at(context, tool_definitions, budget, Instant::now())
    }

    /// 使用从回合占位时开始计算的预算创建快照。
    ///
    /// Web/CLI 适配层应在取得回合租约前记录 `started_at`，确保角色、MCP、provider
    /// 等 Preparing 阶段也计入整回合硬期限。
    pub fn with_budget_started_at(
        context: TurnContext,
        tool_definitions: Vec<ToolDef>,
        budget: TurnBudget,
        started_at: Instant,
    ) -> Self {
        Self {
            context,
            full_tool_definitions: tool_definitions.clone(),
            tool_definitions,
            budget,
            execution_policy: FrozenExecutionPolicy::new(
                "request_approval",
                "workspace_write",
                Vec::new(),
            ),
            capability_epoch: 0,
            started_at,
            model_requests: 0,
            tool_calls: 0,
        }
    }

    /// 在回合执行前附加已经规范化的权限快照。
    pub fn with_execution_policy(mut self, policy: FrozenExecutionPolicy) -> Self {
        self.execution_policy = policy;
        self
    }

    pub fn execution_policy(&self) -> &FrozenExecutionPolicy {
        &self.execution_policy
    }

    /// 返回当前回合中指定工具的冻结定义。
    pub fn tool_definition(&self, name: &str) -> Option<&ToolDef> {
        self.tool_definitions
            .iter()
            .find(|definition| definition.name == name)
    }

    /// 返回当前能力版本下模型可见的工具定义。
    pub fn visible_tool_definitions(&self) -> &[ToolDef] {
        &self.tool_definitions
    }

    /// 返回回合创建时冻结的完整工具目录。
    pub fn full_tool_definitions(&self) -> &[ToolDef] {
        &self.full_tool_definitions
    }

    /// 返回当前工具能力版本；工具调用请求必须携带发起时观察到的版本。
    pub fn capability_epoch(&self) -> u64 {
        self.capability_epoch
    }

    /// 在不引入回合外新工具的前提下替换模型可见工具，并推进能力版本。
    ///
    /// 参数中的定义仅用于选择工具名称，最终定义始终从冻结全集克隆，避免
    /// 调用方通过同名伪造定义改变风险等级或可用状态。
    pub fn replace_visible_tool_definitions(
        &mut self,
        definitions: Vec<ToolDef>,
    ) -> Result<u64, String> {
        let next_epoch = self
            .capability_epoch
            .checked_add(1)
            .ok_or_else(|| "本轮工具能力版本已耗尽。".to_string())?;
        let mut names = BTreeSet::new();
        let mut frozen_definitions = Vec::with_capacity(definitions.len());
        for definition in &definitions {
            if !names.insert(definition.name.as_str()) {
                return Err(format!("工具 `{}` 在可见能力中重复出现。", definition.name));
            }
            let Some(frozen) = self
                .full_tool_definitions
                .iter()
                .find(|frozen| frozen.name == definition.name)
            else {
                return Err(format!(
                    "工具 `{}` 不在本轮冻结的能力目录中。",
                    definition.name
                ));
            };
            frozen_definitions.push(frozen.clone());
        }
        self.tool_definitions = frozen_definitions;
        self.capability_epoch = next_epoch;
        Ok(self.capability_epoch)
    }

    /// 原子更新本轮运行模式元数据与可见工具能力。
    ///
    /// 该方法只改变本轮执行状态，不读取全局设置；上层应先根据冻结全集计算
    /// 新的工具子集，再将模式、阶段、预设和子集一次性提交。
    pub fn transition_runtime_mode(
        &mut self,
        runtime_mode: impl Into<String>,
        focus_phase: impl Into<String>,
        tool_preset: impl Into<String>,
        definitions: Vec<ToolDef>,
    ) -> Result<u64, String> {
        let runtime_mode = runtime_mode.into();
        let focus_phase = focus_phase.into();
        let tool_preset = tool_preset.into();
        for (field, value) in [
            ("runtime_mode", runtime_mode.as_str()),
            ("focus_phase", focus_phase.as_str()),
            ("tool_preset", tool_preset.as_str()),
        ] {
            if value.trim().is_empty() {
                return Err(format!("回合模式字段 `{field}` 不能为空。"));
            }
        }

        let epoch = self.replace_visible_tool_definitions(definitions)?;
        self.context.runtime_mode = runtime_mode;
        self.context.focus_phase = focus_phase;
        self.context.tool_preset = tool_preset;
        Ok(epoch)
    }

    /// 按名称和能力版本校验工具调用，拒绝模式切换前生成的陈旧请求。
    pub fn authorize_tool_call(&self, name: &str, capability_epoch: u64) -> Result<(), String> {
        if capability_epoch != self.capability_epoch {
            return Err(format!(
                "工具调用能力版本已过期：请求版本为 {capability_epoch}，当前版本为 {}。",
                self.capability_epoch
            ));
        }
        let Some(definition) = self.tool_definition(name) else {
            return Err(format!("当前能力版本不允许调用工具 `{name}`。"));
        };
        if !definition.available {
            return Err(definition
                .disabled_reason
                .clone()
                .unwrap_or_else(|| format!("工具 `{name}` 当前不可用。")));
        }
        Ok(())
    }

    /// 记录一次模型请求，并拒绝超出回合预算的续轮。
    pub fn consume_model_request(&mut self) -> Result<(), String> {
        self.ensure_not_expired()?;
        if self.model_requests >= self.budget.max_model_requests {
            return Err(format!(
                "本轮已达到模型请求上限（{} 次）。",
                self.budget.max_model_requests
            ));
        }
        self.model_requests += 1;
        Ok(())
    }

    /// 记录一次工具调用，并拒绝超出回合预算的操作。
    pub fn consume_tool_call(&mut self) -> Result<(), String> {
        self.ensure_not_expired()?;
        if self.tool_calls >= self.budget.max_tool_calls {
            return Err(format!(
                "本轮已达到工具调用上限（{} 次）。",
                self.budget.max_tool_calls
            ));
        }
        self.tool_calls += 1;
        Ok(())
    }

    /// 确认当前回合尚未超过总时限。
    pub fn ensure_not_expired(&self) -> Result<(), String> {
        self.ensure_within_deadline()
            .map_err(|error| error.to_string())
    }

    /// 返回距离硬截止时间的剩余时长。
    pub fn remaining_duration(&self) -> Duration {
        self.budget
            .max_duration
            .saturating_sub(self.started_at.elapsed())
    }

    /// 以结构化错误确认硬截止时间尚未到达。
    pub fn ensure_within_deadline(&self) -> Result<(), TurnBudgetError> {
        if self.started_at.elapsed() >= self.budget.max_duration {
            return Err(TurnBudgetError::DeadlineExceeded {
                max_duration: self.budget.max_duration,
            });
        }
        Ok(())
    }

    /// 在本轮剩余时长内执行异步操作，超时会主动丢弃其 Future。
    pub async fn run_with_deadline<F, T>(&self, operation: F) -> Result<T, TurnBudgetError>
    where
        F: Future<Output = T>,
    {
        self.ensure_within_deadline()?;
        tokio::time::timeout(self.remaining_duration(), operation)
            .await
            .map_err(|_| TurnBudgetError::DeadlineExceeded {
                max_duration: self.budget.max_duration,
            })
    }

    pub fn model_requests(&self) -> u8 {
        self.model_requests
    }

    pub fn tool_calls(&self) -> u8 {
        self.tool_calls
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::Duration;

    use muse_core::domain::persona::ToolPolicy;
    use muse_core::domain::tool::{ToolDef, ToolExecutionOwner, ToolRisk};
    use muse_core::domain::turn::TurnContext;

    use super::{FrozenExecutionPolicy, TurnBudget, TurnBudgetError, TurnSnapshot};

    #[test]
    fn default_budget_matches_product_contract() {
        let budget = TurnBudget::default();
        assert_eq!(budget.max_model_requests, 8);
        assert_eq!(budget.max_tool_calls, 12);
        assert_eq!(budget.max_duration.as_secs(), 900);
    }

    #[tokio::test]
    async fn hard_deadline_cancels_an_in_flight_operation() {
        let snapshot = TurnSnapshot::with_budget(
            context(),
            vec![],
            TurnBudget {
                max_duration: Duration::from_millis(10),
                ..TurnBudget::default()
            },
        );

        let result = snapshot
            .run_with_deadline(tokio::time::sleep(Duration::from_secs(1)))
            .await;

        assert!(matches!(
            result,
            Err(TurnBudgetError::DeadlineExceeded { .. })
        ));
    }

    #[test]
    fn preparing_time_is_counted_when_snapshot_uses_turn_start() {
        let budget = TurnBudget {
            max_model_requests: 1,
            max_tool_calls: 1,
            max_duration: Duration::from_millis(50),
        };
        let started_at = std::time::Instant::now()
            .checked_sub(Duration::from_millis(100))
            .expect("测试 Instant 应可回退");
        let snapshot = TurnSnapshot::with_budget_started_at(
            context(),
            vec![tool("file_read")],
            budget,
            started_at,
        );

        assert!(matches!(
            snapshot.ensure_within_deadline(),
            Err(super::TurnBudgetError::DeadlineExceeded { .. })
        ));
    }

    #[test]
    fn capability_epoch_rejects_calls_created_before_a_mode_change() {
        let mut snapshot = TurnSnapshot::new(context(), vec![tool("search"), tool("write")]);
        let original_epoch = snapshot.capability_epoch();

        let current_epoch = snapshot
            .replace_visible_tool_definitions(vec![tool("search")])
            .unwrap();

        assert_eq!(current_epoch, original_epoch + 1);
        assert!(
            snapshot
                .authorize_tool_call("search", current_epoch)
                .is_ok()
        );
        assert!(
            snapshot
                .authorize_tool_call("search", original_epoch)
                .is_err()
        );
        assert!(
            snapshot
                .authorize_tool_call("write", current_epoch)
                .is_err()
        );
    }

    #[test]
    fn current_policy_can_revoke_but_cannot_expand_frozen_permissions() {
        let frozen = FrozenExecutionPolicy::new(
            "request_approval",
            "workspace_write",
            vec!["/workspace".into()],
        );
        let expanded =
            FrozenExecutionPolicy::new("full_access", "danger_full_access", vec!["/".into()]);
        let still_frozen = frozen.restricted_by(&expanded);
        assert_eq!(still_frozen.permission_mode, "request_approval");
        assert_eq!(still_frozen.sandbox_mode, "workspace_write");

        let permissive = FrozenExecutionPolicy::new(
            "full_access",
            "danger_full_access",
            vec!["/workspace".into()],
        );
        let revoked = permissive.restricted_by(&frozen);
        assert_eq!(revoked.permission_mode, "request_approval");
        assert_eq!(revoked.sandbox_mode, "workspace_write");
    }

    #[test]
    fn current_policy_root_intersection_keeps_the_narrower_root() {
        let frozen = FrozenExecutionPolicy::new(
            "full_access",
            "danger_full_access",
            vec!["/workspace".into(), "/other/narrow".into()],
        );
        let current = FrozenExecutionPolicy::new(
            "request_approval",
            "workspace_write",
            vec!["/workspace/sub".into(), "/other".into()],
        );

        let restricted = frozen.restricted_by(&current);

        assert_eq!(
            restricted.allowed_roots,
            vec![
                PathBuf::from("/other/narrow"),
                PathBuf::from("/workspace/sub")
            ]
        );
    }

    #[test]
    fn visible_tools_cannot_expand_beyond_the_frozen_catalog() {
        let mut snapshot = TurnSnapshot::new(context(), vec![tool("search")]);

        assert!(
            snapshot
                .replace_visible_tool_definitions(vec![tool("shell")])
                .is_err()
        );
        assert_eq!(snapshot.capability_epoch(), 0);
    }

    #[test]
    fn visible_tools_reuse_the_frozen_definition_instead_of_a_forged_copy() {
        let mut snapshot = TurnSnapshot::new(context(), vec![tool("search")]);
        let mut forged = tool("search");
        forged.description = "forged".to_string();
        forged.available = false;

        snapshot
            .replace_visible_tool_definitions(vec![forged])
            .unwrap();

        let visible = snapshot.tool_definition("search").unwrap();
        assert_eq!(visible.description, "search");
        assert!(visible.available);
    }

    #[test]
    fn mode_transition_updates_context_and_capabilities_together() {
        let mut snapshot = TurnSnapshot::new(context(), vec![tool("search"), tool("write")]);

        let epoch = snapshot
            .transition_runtime_mode("plan", "research", "read_only", vec![tool("search")])
            .unwrap();

        assert_eq!(epoch, 1);
        assert_eq!(snapshot.context.runtime_mode, "plan");
        assert_eq!(snapshot.context.focus_phase, "research");
        assert_eq!(snapshot.context.tool_preset, "read_only");
        assert_eq!(snapshot.visible_tool_definitions().len(), 1);
    }

    fn context() -> TurnContext {
        TurnContext {
            conversation_id: "conversation-1".to_string(),
            turn_id: "turn-1".to_string(),
            persona_id: None,
            system_prompt: "system".to_string(),
            model_provider: "test".to_string(),
            model_name: "test-model".to_string(),
            model_source: "global_active".to_string(),
            model_fallback: false,
            model_fallback_reason: None,
            model_capabilities: vec!["chat".to_string()],
            model_context_window: 128_000,
            model_max_output_tokens: 8_192,
            tool_policy: ToolPolicy::default(),
            skill_policy: Default::default(),
            mcp_policy: Default::default(),
            runtime_mode: "chat".to_string(),
            focus_phase: "idle".to_string(),
            tool_preset: "default".to_string(),
            voice_enabled: false,
            active_voice_id: None,
            voice_source: "unavailable".to_string(),
            voice_fallback: false,
            voice_fallback_reason: None,
            created_at: "2026-07-11T00:00:00Z".to_string(),
            runtime_policy: Default::default(),
            tool_definitions: vec![],
        }
    }

    fn tool(name: &str) -> ToolDef {
        ToolDef {
            name: name.to_string(),
            description: name.to_string(),
            parameters: serde_json::json!({"type": "object"}),
            category: "test".to_string(),
            risk: ToolRisk::ReadOnly,
            requires_approval: false,
            execution_owner: ToolExecutionOwner::Core,
            available: true,
            disabled_reason: None,
        }
    }
}

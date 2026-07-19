//! 运行时事实状态服务。
//!
//! 该服务独占当前会话、活动会话标识和状态协调器。HTTP、桌面或命令行适配层
//! 只能通过这里提供的窄接口读取或推进运行时状态，避免绕过状态机直接修改事实状态。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex as StdMutex;

use muse_core::domain::conversation::Conversation;
use muse_core::domain::mcp::{McpClientManager, McpServerCheckStatus, McpToolCatalog};
use muse_core::domain::runtime::{RuntimeModeState, RuntimeTodoItem};
use tokio::sync::{Mutex, MutexGuard, OnceCell};

use crate::coordinator::{
    IdleLease, RuntimeCoordinator, RuntimeCoordinatorError, RuntimePhase, RuntimeStateSnapshot,
    TurnLease,
};
use crate::interactions::{
    ApprovalDecision, InteractionRegistrationError, InteractionRegistries, InteractionResolution,
    InteractionResolveError, PendingApproval, PendingUserQuestion, UserQuestionDecision,
};
use crate::session::SessionStore;
use crate::session_metadata::{SessionRepository, SessionRepositoryError};
use crate::{ApprovalModePreset, ApprovalsReviewer, FrozenExecutionPolicy};

/// 运行时服务自身的状态访问错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeServiceError {
    ActiveConversationStatePoisoned,
    ExecutionPolicyStatePoisoned,
    RuntimeModeStatePoisoned,
}

impl std::fmt::Display for RuntimeServiceError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ActiveConversationStatePoisoned => {
                write!(formatter, "运行时 active conversation 状态已损坏。")
            }
            Self::ExecutionPolicyStatePoisoned => {
                write!(formatter, "运行时权限策略状态已损坏。")
            }
            Self::RuntimeModeStatePoisoned => write!(formatter, "运行时模式状态已损坏。"),
        }
    }
}

impl std::error::Error for RuntimeServiceError {}

/// 进程内运行时事实状态的唯一所有者。
pub struct RuntimeService {
    active_conversation_id: StdMutex<String>,
    conversation: Mutex<Conversation>,
    execution_policy: StdMutex<FrozenExecutionPolicy>,
    approval_review_failures: StdMutex<BTreeMap<String, u8>>,
    interactions: Mutex<InteractionRegistries>,
    runtime_mode: StdMutex<RuntimeModeState>,
    active_todos: Mutex<Vec<RuntimeTodoItem>>,
    active_plan: Mutex<Option<serde_json::Value>>,
    mcp_client_manager: McpClientManager,
    mcp_tool_catalog: Mutex<McpToolCatalog>,
    mcp_server_checks: Mutex<BTreeMap<String, McpServerCheckStatus>>,
    data_dir: PathBuf,
    session_repository: OnceCell<SessionRepository>,
    coordinator: RuntimeCoordinator,
}

impl RuntimeService {
    /// 使用初始逻辑会话和上下文创建运行服务。
    pub fn new(active_conversation_id: impl Into<String>, conversation: Conversation) -> Self {
        Self::new_with_data_dir(
            active_conversation_id,
            conversation,
            muse_core::config::Config::data_dir(),
        )
    }

    /// 使用启动时已经解析的稳定数据目录创建运行服务。
    pub fn new_with_data_dir(
        active_conversation_id: impl Into<String>,
        conversation: Conversation,
        data_dir: impl Into<PathBuf>,
    ) -> Self {
        let data_dir = data_dir.into();
        let mcp_config_path = data_dir.join("config.toml");
        Self {
            active_conversation_id: StdMutex::new(active_conversation_id.into()),
            conversation: Mutex::new(conversation),
            execution_policy: StdMutex::new(FrozenExecutionPolicy::new(
                "request_approval",
                "workspace_write",
                Vec::new(),
            )),
            approval_review_failures: StdMutex::new(BTreeMap::new()),
            interactions: Mutex::new(InteractionRegistries::default()),
            runtime_mode: StdMutex::new(RuntimeModeState::default()),
            active_todos: Mutex::new(Vec::new()),
            active_plan: Mutex::new(None),
            mcp_client_manager: McpClientManager::default(),
            mcp_tool_catalog: Mutex::new(McpToolCatalog::empty_for_config_path(mcp_config_path)),
            mcp_server_checks: Mutex::new(BTreeMap::new()),
            data_dir,
            session_repository: OnceCell::new(),
            coordinator: RuntimeCoordinator::new(),
        }
    }

    /// 返回启动时固定的数据目录；环境变量后续变化不得切换会话 generation。
    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    /// 惰性初始化并返回本进程唯一的 v3 会话存储。
    ///
    /// `OnceCell` 保证迁移和 generation 指针初始化只运行一次；初始化成功属于可观察
    /// 运行时事实变更，因此只在首次成功时推进一次 state revision。
    pub async fn session_repository(&self) -> Result<&SessionRepository, SessionRepositoryError> {
        self.session_repository
            .get_or_try_init(|| async {
                let (repository, _) = SessionRepository::open(&self.data_dir).await?;
                self.coordinator.touch();
                Ok(repository)
            })
            .await
    }

    /// 返回统一仓储内部的 canonical Session v3 Store，供只读详情与恢复使用。
    pub async fn session_store(&self) -> Result<&SessionStore, SessionRepositoryError> {
        Ok(self.session_repository().await?.session_store())
    }

    /// 为启动时已经校验的工作区权限设置初始内存事实状态。
    pub fn with_execution_policy(self, policy: FrozenExecutionPolicy) -> Self {
        if let Ok(mut current) = self.execution_policy.lock() {
            *current = policy;
        }
        self
    }

    /// 读取当前内存权限事实；工具执行不得重新从磁盘读取该状态。
    pub fn execution_policy(&self) -> Result<FrozenExecutionPolicy, RuntimeServiceError> {
        self.execution_policy
            .lock()
            .map(|policy| policy.clone())
            .map_err(|_| RuntimeServiceError::ExecutionPolicyStatePoisoned)
    }

    /// 在配置原子落盘后更新内存权限事实，并推进状态 revision。
    pub fn set_execution_policy(
        &self,
        policy: FrozenExecutionPolicy,
    ) -> Result<u64, RuntimeServiceError> {
        if policy.approvals_reviewer != ApprovalsReviewer::AutoReview {
            let conversation_id = self.active_conversation_id()?;
            self.approval_review_failures
                .lock()
                .map_err(|_| RuntimeServiceError::ExecutionPolicyStatePoisoned)?
                .remove(&conversation_id);
        }
        *self
            .execution_policy
            .lock()
            .map_err(|_| RuntimeServiceError::ExecutionPolicyStatePoisoned)? = policy;
        Ok(self.coordinator.touch())
    }

    /// 记录当前会话的自动审查结果；连续三次未放行时收紧为手动审批。
    pub fn record_approval_review_outcome(
        &self,
        allowed: bool,
    ) -> Result<bool, RuntimeServiceError> {
        let conversation_id = self.active_conversation_id()?;
        let mut failures = self
            .approval_review_failures
            .lock()
            .map_err(|_| RuntimeServiceError::ExecutionPolicyStatePoisoned)?;
        if allowed {
            failures.remove(&conversation_id);
            return Ok(false);
        }
        let count = failures.entry(conversation_id).or_insert(0);
        *count = count.saturating_add(1);
        if *count < 3 {
            return Ok(false);
        }
        let current = self.execution_policy()?;
        let manual = FrozenExecutionPolicy::from_preset(
            ApprovalModePreset::Manual,
            current.allowed_roots,
            current.revision.saturating_add(1),
        );
        *self
            .execution_policy
            .lock()
            .map_err(|_| RuntimeServiceError::ExecutionPolicyStatePoisoned)? = manual;
        self.coordinator.touch();
        Ok(true)
    }

    /// 读取当前运行模式事实。
    pub fn runtime_mode(&self) -> Result<RuntimeModeState, RuntimeServiceError> {
        self.runtime_mode
            .lock()
            .map(|mode| *mode)
            .map_err(|_| RuntimeServiceError::RuntimeModeStatePoisoned)
    }

    /// 原子更新运行模式并推进 revision。
    pub fn set_runtime_mode(&self, mode: RuntimeModeState) -> Result<u64, RuntimeServiceError> {
        *self
            .runtime_mode
            .lock()
            .map_err(|_| RuntimeServiceError::RuntimeModeStatePoisoned)? = mode;
        Ok(self.coordinator.touch())
    }

    /// 获取当前任务清单快照。
    pub async fn runtime_todos(&self) -> Vec<RuntimeTodoItem> {
        self.active_todos.lock().await.clone()
    }

    /// 替换当前任务清单并推进 revision。
    pub async fn replace_runtime_todos(&self, todos: Vec<RuntimeTodoItem>) -> u64 {
        *self.active_todos.lock().await = todos;
        self.coordinator.touch()
    }

    /// 追加任务并返回更新后的事实快照。
    pub async fn push_runtime_todo(&self, todo: RuntimeTodoItem) -> Vec<RuntimeTodoItem> {
        let mut todos = self.active_todos.lock().await;
        todos.push(todo);
        let snapshot = todos.clone();
        drop(todos);
        self.coordinator.touch();
        snapshot
    }

    /// 替换当前计划确认事实并推进 revision。
    pub async fn set_active_plan(&self, plan: Option<serde_json::Value>) -> u64 {
        *self.active_plan.lock().await = plan;
        self.coordinator.touch()
    }

    /// 获取仍在有效期内的 MCP 目录快照。
    pub async fn fresh_mcp_tool_catalog(
        &self,
        config_hash: &str,
        max_age: std::time::Duration,
    ) -> Option<McpToolCatalog> {
        let catalog = self.mcp_tool_catalog.lock().await;
        catalog
            .is_fresh(config_hash, max_age)
            .then(|| catalog.clone())
    }

    /// 发布新的 MCP 目录快照。
    pub async fn replace_mcp_tool_catalog(&self, catalog: McpToolCatalog) -> u64 {
        *self.mcp_tool_catalog.lock().await = catalog;
        self.coordinator.touch()
    }

    /// 发布仅与一个已保存配置 revision 对应的管理页检查状态。
    pub async fn publish_mcp_server_check(
        &self,
        name: String,
        status: McpServerCheckStatus,
    ) -> u64 {
        self.mcp_server_checks.lock().await.insert(name, status);
        self.coordinator.touch()
    }

    /// 读取指定 Server 最近一次管理页检查状态。
    pub async fn mcp_server_check(&self, name: &str) -> Option<McpServerCheckStatus> {
        self.mcp_server_checks.lock().await.get(name).cloned()
    }

    /// 配置变化或删除后清除旧检查状态。
    pub async fn clear_mcp_server_check(&self, name: &str) -> u64 {
        self.mcp_server_checks.lock().await.remove(name);
        self.coordinator.touch()
    }

    /// 返回由运行时独占的 MCP client/session manager。
    pub fn mcp_client_manager(&self) -> &McpClientManager {
        &self.mcp_client_manager
    }

    /// 读取当前活动逻辑会话标识。
    pub fn active_conversation_id(&self) -> Result<String, RuntimeServiceError> {
        self.active_conversation_id
            .lock()
            .map(|value| value.clone())
            .map_err(|_| RuntimeServiceError::ActiveConversationStatePoisoned)
    }

    /// 原子替换当前活动逻辑会话标识。
    pub fn set_active_conversation_id(
        &self,
        conversation_id: impl Into<String>,
    ) -> Result<(), RuntimeServiceError> {
        let mut active = self
            .active_conversation_id
            .lock()
            .map_err(|_| RuntimeServiceError::ActiveConversationStatePoisoned)?;
        *active = conversation_id.into();
        Ok(())
    }

    /// 获取当前会话的异步互斥访问权。
    ///
    /// 调用方应尽量缩短 guard 生命周期，尤其不能在持锁期间等待模型或工具调用。
    pub async fn lock_conversation(&self) -> MutexGuard<'_, Conversation> {
        self.conversation.lock().await
    }

    /// 克隆当前已经发布的会话，供新回合创建私有工作副本。
    pub async fn conversation_snapshot(&self) -> Conversation {
        self.conversation.lock().await.clone()
    }

    /// 仅允许仍然活跃的同一回合发布完整会话副本。
    ///
    /// 调用方必须先把 `turn_committed` 可靠写入 transcript，再调用本方法。失败或
    /// 取消路径不得发布工作副本，因此 HTTP 历史接口始终只能观察到已提交 revision。
    pub async fn publish_turn_conversation(
        &self,
        turn_id: &str,
        conversation: Conversation,
    ) -> Result<u64, RuntimeCoordinatorError> {
        let snapshot = self.coordinator.snapshot()?;
        if snapshot.turn_id.as_deref() != Some(turn_id) {
            return Err(RuntimeCoordinatorError::StaleLease {
                turn_id: turn_id.to_string(),
            });
        }
        *self.conversation.lock().await = conversation;
        Ok(self.coordinator.touch())
    }

    /// 原子占用运行时并开始新回合。
    pub fn begin_turn(
        &self,
        turn_id: impl Into<String>,
        conversation_id: impl Into<String>,
    ) -> Result<TurnLease, RuntimeCoordinatorError> {
        self.coordinator.begin_turn(turn_id, conversation_id)
    }

    /// 确认运行时当前完全空闲。
    pub fn ensure_idle(&self) -> Result<(), RuntimeCoordinatorError> {
        self.coordinator.ensure_idle()
    }

    /// 获取会话维护的独占空闲租约。
    pub fn acquire_idle_lease(
        &self,
        operation: impl Into<String>,
    ) -> Result<IdleLease, RuntimeCoordinatorError> {
        self.coordinator.acquire_idle_lease(operation)
    }

    /// 获取当前运行时状态快照。
    pub fn snapshot(&self) -> Result<RuntimeStateSnapshot, RuntimeCoordinatorError> {
        self.coordinator.snapshot()
    }

    /// 请求取消指定活动回合。
    pub fn cancel_turn(&self, turn_id: &str) -> Result<(), RuntimeCoordinatorError> {
        self.coordinator.cancel_turn(turn_id)
    }

    /// 使用 turn ID guard 推进当前活动回合。
    pub fn transition_active(
        &self,
        turn_id: &str,
        next: RuntimePhase,
    ) -> Result<(), RuntimeCoordinatorError> {
        self.coordinator.transition_active(turn_id, next)
    }

    /// 标记活动回合已经产生或可能产生外部副作用。
    pub fn mark_external_effect(&self, turn_id: &str) -> Result<(), RuntimeCoordinatorError> {
        self.coordinator.mark_external_effect(turn_id)
    }

    /// 查询活动回合是否已经越过外部副作用边界。
    pub fn active_turn_had_external_effects(
        &self,
        turn_id: &str,
    ) -> Result<bool, RuntimeCoordinatorError> {
        self.coordinator.active_turn_had_external_effects(turn_id)
    }

    /// 登记等待中的工具审批。
    pub async fn register_pending_approval(
        &self,
        approval_id: String,
        pending: PendingApproval,
    ) -> Result<u64, InteractionRegistrationError> {
        self.interactions
            .lock()
            .await
            .register_approval(approval_id, pending)?;
        Ok(self.coordinator.touch())
    }

    /// 登记等待中的用户问题。
    pub async fn register_pending_user_question(
        &self,
        request_id: String,
        pending: PendingUserQuestion,
    ) -> Result<u64, InteractionRegistrationError> {
        self.interactions
            .lock()
            .await
            .register_user_question(request_id, pending)?;
        Ok(self.coordinator.touch())
    }

    /// 按 turn ID 与 approval ID 精确处理审批，保证重试幂等且冲突决策不唤醒回合。
    pub async fn resolve_approval(
        &self,
        approval_id: &str,
        turn_id: &str,
        decision: ApprovalDecision,
    ) -> Result<InteractionResolution, InteractionResolveError> {
        let mut interactions = self.interactions.lock().await;
        let first_resolution =
            interactions.approval_requires_first_resolution(approval_id, turn_id, &decision)?;
        if first_resolution {
            let snapshot = self
                .coordinator
                .snapshot()
                .map_err(|_| InteractionResolveError::RuntimeNotWaiting)?;
            if snapshot.turn_id.as_deref() != Some(turn_id)
                || snapshot.phase != RuntimePhase::WaitingApproval
            {
                return Err(InteractionResolveError::RuntimeNotWaiting);
            }
        }
        let (resolution, sender) =
            interactions.resolve_approval(approval_id, turn_id, decision.clone())?;
        drop(interactions);
        if sender.is_some() {
            self.coordinator.touch();
        }
        if let Some(sender) = sender {
            sender
                .send(decision)
                .map_err(|_| InteractionResolveError::ReceiverClosed)?;
        }
        Ok(resolution)
    }

    /// 按 turn ID 与 request ID 精确处理用户问题。
    pub async fn resolve_user_question(
        &self,
        request_id: &str,
        turn_id: &str,
        decision: UserQuestionDecision,
    ) -> Result<InteractionResolution, InteractionResolveError> {
        let mut interactions = self.interactions.lock().await;
        let first_resolution =
            interactions.user_question_requires_first_resolution(request_id, turn_id, &decision)?;
        if first_resolution {
            let snapshot = self
                .coordinator
                .snapshot()
                .map_err(|_| InteractionResolveError::RuntimeNotWaiting)?;
            if snapshot.turn_id.as_deref() != Some(turn_id)
                || snapshot.phase != RuntimePhase::WaitingUser
            {
                return Err(InteractionResolveError::RuntimeNotWaiting);
            }
        }
        let (resolution, sender) =
            interactions.resolve_user_question(request_id, turn_id, decision.clone())?;
        drop(interactions);
        if sender.is_some() {
            self.coordinator.touch();
        }
        if let Some(sender) = sender {
            sender
                .send(decision)
                .map_err(|_| InteractionResolveError::ReceiverClosed)?;
        }
        Ok(resolution)
    }

    /// 在同步 Drop 路径中尝试精确移除审批登记。
    ///
    /// 返回值表示是否取得 registry 锁；未取得时调用方应安排异步清理。
    pub fn try_remove_pending_approval(&self, approval_id: &str, turn_id: &str) -> bool {
        let Ok(mut interactions) = self.interactions.try_lock() else {
            return false;
        };
        if interactions.remove_approval(approval_id, turn_id) {
            self.coordinator.touch();
        }
        true
    }

    /// 在同步 Drop 路径中尝试精确移除用户问题登记。
    pub fn try_remove_pending_user_question(&self, request_id: &str, turn_id: &str) -> bool {
        let Ok(mut interactions) = self.interactions.try_lock() else {
            return false;
        };
        if interactions.remove_user_question(request_id, turn_id) {
            self.coordinator.touch();
        }
        true
    }

    /// 异步精确移除审批登记。
    pub async fn remove_pending_approval(&self, approval_id: &str, turn_id: &str) -> bool {
        let removed = self
            .interactions
            .lock()
            .await
            .remove_approval(approval_id, turn_id);
        if removed {
            self.coordinator.touch();
        }
        removed
    }

    /// 异步精确移除用户问题登记。
    pub async fn remove_pending_user_question(&self, request_id: &str, turn_id: &str) -> bool {
        let removed = self
            .interactions
            .lock()
            .await
            .remove_user_question(request_id, turn_id);
        if removed {
            self.coordinator.touch();
        }
        removed
    }

    /// 在同步 Drop 路径中尝试清理指定回合的全部 pending 交互。
    pub fn try_remove_pending_interactions_for_turn(&self, turn_id: &str) -> bool {
        let Ok(mut interactions) = self.interactions.try_lock() else {
            return false;
        };
        if interactions.remove_pending_for_turn(turn_id) {
            self.coordinator.touch();
        }
        true
    }

    /// 异步清理指定回合的全部 pending 交互。
    pub async fn remove_pending_interactions_for_turn(&self, turn_id: &str) -> bool {
        let removed = self
            .interactions
            .lock()
            .await
            .remove_pending_for_turn(turn_id);
        if removed {
            self.coordinator.touch();
        }
        removed
    }

    /// 取消指定回合的全部 pending 交互并唤醒等待者，不影响其他并发标识。
    pub async fn cancel_pending_interactions_for_turn(&self, turn_id: &str, reason: &str) {
        let (approvals, questions) = self
            .interactions
            .lock()
            .await
            .cancel_pending_for_turn(turn_id);
        if !approvals.is_empty() || !questions.is_empty() {
            self.coordinator.touch();
        }
        for sender in approvals {
            let _ = sender.send(ApprovalDecision {
                approved: false,
                reason: Some(reason.to_string()),
            });
        }
        for sender in questions {
            let _ = sender.send(UserQuestionDecision {
                answered: false,
                answers: None,
                annotations: None,
                reason: Some(reason.to_string()),
            });
        }
    }

    /// 返回 pending 交互计数，仅供运行时诊断与回归测试使用。
    pub async fn pending_interaction_counts(&self) -> (usize, usize) {
        self.interactions.lock().await.pending_counts()
    }

    /// 标记协调器外、但属于运行时事实状态的变更。
    pub fn touch(&self) -> u64 {
        self.coordinator.touch()
    }
}

impl Drop for RuntimeService {
    fn drop(&mut self) {
        self.mcp_client_manager.request_close_all();
    }
}

#[cfg(test)]
mod tests {
    use muse_core::domain::conversation::Conversation;
    use tempfile::TempDir;

    use super::RuntimeService;
    use crate::coordinator::RuntimePhase;

    fn test_service() -> (TempDir, RuntimeService) {
        let temp = tempfile::tempdir().expect("应能创建测试数据目录");
        let service = RuntimeService::new_with_data_dir(
            "conversation-1",
            Conversation::new("system".to_string(), 10),
            temp.path(),
        );
        (temp, service)
    }

    #[tokio::test]
    async fn service_owns_conversation_identity_and_coordinator() {
        let (_temp, service) = test_service();

        assert_eq!(service.active_conversation_id().unwrap(), "conversation-1");
        service
            .set_active_conversation_id("conversation-2")
            .unwrap();
        {
            let mut conversation = service.lock_conversation().await;
            conversation.add_user_message("hello".to_string());
        }

        let turn = service.begin_turn("turn-1", "conversation-2").unwrap();
        turn.mark_running().unwrap();
        assert_eq!(service.snapshot().unwrap().phase, RuntimePhase::Running);
        assert_eq!(service.lock_conversation().await.messages.len(), 2);
    }

    #[tokio::test]
    async fn only_active_turn_can_publish_private_conversation_copy() {
        let (_temp, service) = test_service();
        let mut working = service.conversation_snapshot().await;
        working.add_user_message("private".to_string());
        let turn = service.begin_turn("turn-1", "conversation-1").unwrap();
        turn.mark_running().unwrap();

        let error = service
            .publish_turn_conversation("stale-turn", working.clone())
            .await
            .expect_err("过期回合不能发布会话");
        assert!(matches!(
            error,
            crate::coordinator::RuntimeCoordinatorError::StaleLease { .. }
        ));
        assert_eq!(service.conversation_snapshot().await.messages.len(), 1);

        service
            .publish_turn_conversation("turn-1", working)
            .await
            .expect("活跃回合应能发布会话工作副本");
        assert_eq!(service.conversation_snapshot().await.messages.len(), 2);
    }

    #[tokio::test]
    async fn completed_turn_cannot_overwrite_a_later_turn_conversation() {
        let (_temp, service) = test_service();
        let mut stale_working = service.conversation_snapshot().await;
        stale_working.add_user_message("过期回合".to_string());
        let first = service.begin_turn("turn-1", "conversation-1").unwrap();
        first.mark_running().unwrap();
        first.finish().unwrap();

        let mut current_working = service.conversation_snapshot().await;
        current_working.add_user_message("当前回合".to_string());
        let current = service.begin_turn("turn-2", "conversation-1").unwrap();
        current.mark_running().unwrap();
        service
            .publish_turn_conversation("turn-2", current_working)
            .await
            .expect("当前回合应能发布");

        let error = service
            .publish_turn_conversation("turn-1", stale_working)
            .await
            .expect_err("已完成的旧回合不得覆盖当前回合");
        assert!(matches!(
            error,
            crate::coordinator::RuntimeCoordinatorError::StaleLease { .. }
        ));
        let published = service.conversation_snapshot().await;
        assert_eq!(published.messages.last().unwrap().content, "当前回合");
    }

    #[test]
    fn service_owns_execution_policy_and_external_effect_state() {
        let (_temp, service) = test_service();
        let service = service.with_execution_policy(crate::FrozenExecutionPolicy::new(
            "full_access",
            "danger_full_access",
            vec![std::path::PathBuf::from("/workspace")],
        ));
        assert_eq!(
            service.execution_policy().unwrap().preset(),
            crate::ApprovalModePreset::Yolo
        );

        let turn = service.begin_turn("turn-1", "conversation-1").unwrap();
        turn.mark_running().unwrap();
        assert!(!service.active_turn_had_external_effects("turn-1").unwrap());
        service.mark_external_effect("turn-1").unwrap();
        assert!(service.active_turn_had_external_effects("turn-1").unwrap());

        service
            .set_execution_policy(crate::FrozenExecutionPolicy::new(
                "request_approval",
                "workspace_write",
                Vec::new(),
            ))
            .unwrap();
        assert_eq!(
            service.execution_policy().unwrap().preset(),
            crate::ApprovalModePreset::Manual
        );
    }
}

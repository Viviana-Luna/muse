//! 单轮运行时状态机与并发闸门。

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use tokio::sync::watch;

/// 运行时单轮所处的显式阶段。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimePhase {
    Idle,
    Preparing,
    Running,
    WaitingApproval,
    WaitingUser,
    Cancelling,
    Finalizing,
}

/// 当前运行时状态的只读快照。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeStateSnapshot {
    pub state_revision: u64,
    pub phase: RuntimePhase,
    pub turn_id: Option<String>,
    pub conversation_id: Option<String>,
    pub generation: Option<u64>,
    /// 当前占用空闲闸门的会话维护操作；存在时不能启动回合。
    pub exclusive_operation: Option<String>,
}

impl RuntimeStateSnapshot {
    /// 仅当没有回合、也没有会话维护租约时才是真正空闲。
    pub fn is_idle(&self) -> bool {
        self.phase == RuntimePhase::Idle && self.exclusive_operation.is_none()
    }
}

/// 单轮状态机错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeCoordinatorError {
    Busy {
        active_turn_id: String,
        phase: RuntimePhase,
    },
    ExclusiveOperationBusy {
        operation: String,
    },
    InvalidIdentifier {
        field: &'static str,
    },
    InvalidTransition {
        from: RuntimePhase,
        to: RuntimePhase,
    },
    StaleLease {
        turn_id: String,
    },
    StaleExclusiveLease {
        operation: String,
    },
    TurnNotFound {
        turn_id: String,
    },
    StatePoisoned,
}

impl fmt::Display for RuntimeCoordinatorError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Busy {
                active_turn_id,
                phase,
            } => write!(
                formatter,
                "当前回合 `{active_turn_id}` 仍处于 {phase:?} 阶段，暂不能启动新回合或切换会话。"
            ),
            Self::ExclusiveOperationBusy { operation } => write!(
                formatter,
                "会话维护操作 `{operation}` 正在执行，暂不能启动回合或执行其他维护操作。"
            ),
            Self::InvalidIdentifier { field } => {
                write!(formatter, "运行时字段 `{field}` 不能为空。")
            }
            Self::InvalidTransition { from, to } => {
                write!(formatter, "不允许从 {from:?} 阶段切换到 {to:?} 阶段。")
            }
            Self::StaleLease { turn_id } => {
                write!(formatter, "回合 `{turn_id}` 的执行租约已经失效。")
            }
            Self::StaleExclusiveLease { operation } => {
                write!(formatter, "会话维护操作 `{operation}` 的空闲租约已经失效。")
            }
            Self::TurnNotFound { turn_id } => {
                write!(formatter, "当前没有可取消的回合 `{turn_id}`。")
            }
            Self::StatePoisoned => write!(formatter, "运行时状态锁已损坏。"),
        }
    }
}

impl std::error::Error for RuntimeCoordinatorError {}

#[derive(Debug)]
struct ActiveTurn {
    turn_id: String,
    conversation_id: String,
    generation: u64,
    phase: RuntimePhase,
    cancel_tx: watch::Sender<bool>,
    had_external_effects: bool,
}

#[derive(Debug)]
struct ExclusiveOperation {
    operation: String,
    generation: u64,
}

#[derive(Debug, Default)]
struct CoordinatorState {
    active: Option<ActiveTurn>,
    exclusive_operation: Option<ExclusiveOperation>,
}

#[derive(Debug)]
struct CoordinatorInner {
    state: Mutex<CoordinatorState>,
    next_generation: AtomicU64,
    state_revision: AtomicU64,
}

/// 进程内单轮协调器。
///
/// `begin_turn` 在一个互斥区内完成检查和占位。恢复、分叉、清空会话等多步
/// 操作应持有 [`IdleLease`]，从检查空闲直到状态替换完成都不会插入新回合。
#[derive(Debug, Clone)]
pub struct RuntimeCoordinator {
    inner: Arc<CoordinatorInner>,
}

impl Default for RuntimeCoordinator {
    fn default() -> Self {
        Self::new()
    }
}

impl RuntimeCoordinator {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(CoordinatorInner {
                state: Mutex::new(CoordinatorState::default()),
                next_generation: AtomicU64::new(1),
                state_revision: AtomicU64::new(0),
            }),
        }
    }

    /// 原子占用运行时并返回本轮唯一执行租约。
    pub fn begin_turn(
        &self,
        turn_id: impl Into<String>,
        conversation_id: impl Into<String>,
    ) -> Result<TurnLease, RuntimeCoordinatorError> {
        let turn_id = turn_id.into();
        let conversation_id = conversation_id.into();
        if turn_id.trim().is_empty() {
            return Err(RuntimeCoordinatorError::InvalidIdentifier { field: "turn_id" });
        }
        if conversation_id.trim().is_empty() {
            return Err(RuntimeCoordinatorError::InvalidIdentifier {
                field: "conversation_id",
            });
        }

        let mut state = self.lock_state()?;
        if let Some(active) = state.active.as_ref() {
            return Err(RuntimeCoordinatorError::Busy {
                active_turn_id: active.turn_id.clone(),
                phase: active.phase,
            });
        }
        if let Some(exclusive) = state.exclusive_operation.as_ref() {
            return Err(RuntimeCoordinatorError::ExclusiveOperationBusy {
                operation: exclusive.operation.clone(),
            });
        }

        let generation = self.inner.next_generation.fetch_add(1, Ordering::Relaxed);
        let (cancel_tx, cancel_rx) = watch::channel(false);
        state.active = Some(ActiveTurn {
            turn_id: turn_id.clone(),
            conversation_id: conversation_id.clone(),
            generation,
            phase: RuntimePhase::Preparing,
            cancel_tx,
            had_external_effects: false,
        });
        self.bump_revision();
        drop(state);

        Ok(TurnLease {
            coordinator: self.clone(),
            turn_id,
            conversation_id,
            generation,
            cancellation: TurnCancellation {
                receiver: cancel_rx,
            },
            released: false,
        })
    }

    /// 快速检查当前是否空闲。
    ///
    /// 该检查不占用闸门；需要随后执行异步或多步状态替换时，应使用
    /// [`Self::acquire_idle_lease`] 消除检查与修改之间的竞态窗口。
    pub fn ensure_idle(&self) -> Result<(), RuntimeCoordinatorError> {
        let state = self.lock_state()?;
        if let Some(active) = state.active.as_ref() {
            return Err(RuntimeCoordinatorError::Busy {
                active_turn_id: active.turn_id.clone(),
                phase: active.phase,
            });
        }
        if let Some(exclusive) = state.exclusive_operation.as_ref() {
            return Err(RuntimeCoordinatorError::ExclusiveOperationBusy {
                operation: exclusive.operation.clone(),
            });
        }
        Ok(())
    }

    /// 原子获取会话维护租约，适用于恢复、分叉、切换和清空会话。
    pub fn acquire_idle_lease(
        &self,
        operation: impl Into<String>,
    ) -> Result<IdleLease, RuntimeCoordinatorError> {
        let operation = operation.into();
        if operation.trim().is_empty() {
            return Err(RuntimeCoordinatorError::InvalidIdentifier { field: "operation" });
        }
        let mut state = self.lock_state()?;
        if let Some(active) = state.active.as_ref() {
            return Err(RuntimeCoordinatorError::Busy {
                active_turn_id: active.turn_id.clone(),
                phase: active.phase,
            });
        }
        if let Some(exclusive) = state.exclusive_operation.as_ref() {
            return Err(RuntimeCoordinatorError::ExclusiveOperationBusy {
                operation: exclusive.operation.clone(),
            });
        }
        let generation = self.inner.next_generation.fetch_add(1, Ordering::Relaxed);
        state.exclusive_operation = Some(ExclusiveOperation {
            operation: operation.clone(),
            generation,
        });
        self.bump_revision();
        Ok(IdleLease {
            coordinator: self.clone(),
            operation,
            generation,
            released: false,
        })
    }

    /// 返回当前状态，不泄露内部取消通道。
    pub fn snapshot(&self) -> Result<RuntimeStateSnapshot, RuntimeCoordinatorError> {
        let state = self.lock_state()?;
        Ok(match state.active.as_ref() {
            Some(active) => RuntimeStateSnapshot {
                state_revision: self.inner.state_revision.load(Ordering::Acquire),
                phase: active.phase,
                turn_id: Some(active.turn_id.clone()),
                conversation_id: Some(active.conversation_id.clone()),
                generation: Some(active.generation),
                exclusive_operation: None,
            },
            None => RuntimeStateSnapshot {
                state_revision: self.inner.state_revision.load(Ordering::Acquire),
                phase: RuntimePhase::Idle,
                turn_id: None,
                conversation_id: None,
                generation: None,
                exclusive_operation: state
                    .exclusive_operation
                    .as_ref()
                    .map(|operation| operation.operation.clone()),
            },
        })
    }

    /// 请求取消指定活跃回合，并切换到 `Cancelling` 阶段。
    pub fn cancel_turn(&self, turn_id: &str) -> Result<(), RuntimeCoordinatorError> {
        let mut state = self.lock_state()?;
        let Some(active) = state.active.as_mut() else {
            return Err(RuntimeCoordinatorError::TurnNotFound {
                turn_id: turn_id.to_string(),
            });
        };
        if active.turn_id != turn_id {
            return Err(RuntimeCoordinatorError::TurnNotFound {
                turn_id: turn_id.to_string(),
            });
        }
        if active.phase != RuntimePhase::Cancelling {
            if !transition_allowed(active.phase, RuntimePhase::Cancelling) {
                return Err(RuntimeCoordinatorError::InvalidTransition {
                    from: active.phase,
                    to: RuntimePhase::Cancelling,
                });
            }
            active.phase = RuntimePhase::Cancelling;
            self.bump_revision();
        }
        let _ = active.cancel_tx.send(true);
        Ok(())
    }

    /// 在真实工具 dispatch 前标记当前回合已经越过外部副作用边界。
    pub fn mark_external_effect(&self, turn_id: &str) -> Result<(), RuntimeCoordinatorError> {
        let mut state = self.lock_state()?;
        let Some(active) = state.active.as_mut() else {
            return Err(RuntimeCoordinatorError::TurnNotFound {
                turn_id: turn_id.to_string(),
            });
        };
        if active.turn_id != turn_id {
            return Err(RuntimeCoordinatorError::StaleLease {
                turn_id: turn_id.to_string(),
            });
        }
        if !active.had_external_effects {
            active.had_external_effects = true;
            self.bump_revision();
        }
        Ok(())
    }

    /// 查询同一活动回合是否已经越过外部副作用边界。
    pub fn active_turn_had_external_effects(
        &self,
        turn_id: &str,
    ) -> Result<bool, RuntimeCoordinatorError> {
        let state = self.lock_state()?;
        let Some(active) = state.active.as_ref() else {
            return Ok(false);
        };
        if active.turn_id != turn_id {
            return Err(RuntimeCoordinatorError::StaleLease {
                turn_id: turn_id.to_string(),
            });
        }
        Ok(active.had_external_effects)
    }

    /// 由运行服务在等待审批、等待用户和恢复运行时推进当前回合。
    /// turn ID guard 可阻止过期异步任务改变后来回合的状态。
    pub fn transition_active(
        &self,
        turn_id: &str,
        next: RuntimePhase,
    ) -> Result<(), RuntimeCoordinatorError> {
        let mut state = self.lock_state()?;
        let Some(active) = state.active.as_mut() else {
            return Err(RuntimeCoordinatorError::TurnNotFound {
                turn_id: turn_id.to_string(),
            });
        };
        if active.turn_id != turn_id {
            return Err(RuntimeCoordinatorError::StaleLease {
                turn_id: turn_id.to_string(),
            });
        }
        if active.phase == next {
            return Ok(());
        }
        if !transition_allowed(active.phase, next) {
            return Err(RuntimeCoordinatorError::InvalidTransition {
                from: active.phase,
                to: next,
            });
        }
        active.phase = next;
        self.bump_revision();
        Ok(())
    }

    /// 标记协调器外但属于运行时事实状态的原子变更（例如模式切换）。
    pub fn touch(&self) -> u64 {
        self.bump_revision()
    }

    fn transition(
        &self,
        generation: u64,
        turn_id: &str,
        next: RuntimePhase,
    ) -> Result<(), RuntimeCoordinatorError> {
        let mut state = self.lock_state()?;
        let Some(active) = state.active.as_mut() else {
            return Err(RuntimeCoordinatorError::StaleLease {
                turn_id: turn_id.to_string(),
            });
        };
        if active.generation != generation || active.turn_id != turn_id {
            return Err(RuntimeCoordinatorError::StaleLease {
                turn_id: turn_id.to_string(),
            });
        }
        if active.phase == next {
            return Ok(());
        }
        if !transition_allowed(active.phase, next) {
            return Err(RuntimeCoordinatorError::InvalidTransition {
                from: active.phase,
                to: next,
            });
        }
        active.phase = next;
        self.bump_revision();
        Ok(())
    }

    fn release(&self, generation: u64, turn_id: &str) -> Result<(), RuntimeCoordinatorError> {
        let mut state = self.lock_state()?;
        let Some(active) = state.active.as_ref() else {
            return Err(RuntimeCoordinatorError::StaleLease {
                turn_id: turn_id.to_string(),
            });
        };
        if active.generation != generation || active.turn_id != turn_id {
            return Err(RuntimeCoordinatorError::StaleLease {
                turn_id: turn_id.to_string(),
            });
        }
        state.active = None;
        self.bump_revision();
        Ok(())
    }

    fn release_idle_lease(
        &self,
        generation: u64,
        operation: &str,
    ) -> Result<(), RuntimeCoordinatorError> {
        let mut state = self.lock_state()?;
        let Some(active_operation) = state.exclusive_operation.as_ref() else {
            return Err(RuntimeCoordinatorError::StaleExclusiveLease {
                operation: operation.to_string(),
            });
        };
        if active_operation.generation != generation || active_operation.operation != operation {
            return Err(RuntimeCoordinatorError::StaleExclusiveLease {
                operation: operation.to_string(),
            });
        }
        state.exclusive_operation = None;
        self.bump_revision();
        Ok(())
    }

    fn lock_state(
        &self,
    ) -> Result<std::sync::MutexGuard<'_, CoordinatorState>, RuntimeCoordinatorError> {
        self.inner
            .state
            .lock()
            .map_err(|_| RuntimeCoordinatorError::StatePoisoned)
    }

    fn bump_revision(&self) -> u64 {
        self.inner
            .state_revision
            .fetch_add(1, Ordering::AcqRel)
            .saturating_add(1)
    }
}

/// 会话维护操作持有的空闲闸门租约。
///
/// 租约存续期间既不能启动新回合，也不能开始第二个维护操作。
#[derive(Debug)]
pub struct IdleLease {
    coordinator: RuntimeCoordinator,
    operation: String,
    generation: u64,
    released: bool,
}

impl IdleLease {
    pub fn operation(&self) -> &str {
        &self.operation
    }

    pub fn finish(mut self) -> Result<(), RuntimeCoordinatorError> {
        self.coordinator
            .release_idle_lease(self.generation, &self.operation)?;
        self.released = true;
        Ok(())
    }
}

impl Drop for IdleLease {
    fn drop(&mut self) {
        if !self.released {
            let _ = self
                .coordinator
                .release_idle_lease(self.generation, &self.operation);
            self.released = true;
        }
    }
}

fn transition_allowed(from: RuntimePhase, to: RuntimePhase) -> bool {
    match from {
        RuntimePhase::Idle => false,
        RuntimePhase::Preparing => matches!(
            to,
            RuntimePhase::Running | RuntimePhase::Cancelling | RuntimePhase::Finalizing
        ),
        RuntimePhase::Running => matches!(
            to,
            RuntimePhase::WaitingApproval
                | RuntimePhase::WaitingUser
                | RuntimePhase::Cancelling
                | RuntimePhase::Finalizing
        ),
        RuntimePhase::WaitingApproval | RuntimePhase::WaitingUser => matches!(
            to,
            RuntimePhase::Running | RuntimePhase::Cancelling | RuntimePhase::Finalizing
        ),
        RuntimePhase::Cancelling => to == RuntimePhase::Finalizing,
        RuntimePhase::Finalizing => false,
    }
}

/// 活跃回合的唯一执行租约。
///
/// 租约不可克隆；正常路径调用 [`Self::finish`]，异常路径由 `Drop` 兜底释放。
#[derive(Debug)]
pub struct TurnLease {
    coordinator: RuntimeCoordinator,
    turn_id: String,
    conversation_id: String,
    generation: u64,
    cancellation: TurnCancellation,
    released: bool,
}

impl TurnLease {
    pub fn turn_id(&self) -> &str {
        &self.turn_id
    }

    pub fn conversation_id(&self) -> &str {
        &self.conversation_id
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn cancellation(&self) -> TurnCancellation {
        self.cancellation.clone()
    }

    pub fn transition(&self, next: RuntimePhase) -> Result<(), RuntimeCoordinatorError> {
        if next == RuntimePhase::Idle {
            return Err(RuntimeCoordinatorError::InvalidTransition {
                from: self.coordinator.snapshot()?.phase,
                to: RuntimePhase::Idle,
            });
        }
        self.coordinator
            .transition(self.generation, &self.turn_id, next)
    }

    pub fn mark_running(&self) -> Result<(), RuntimeCoordinatorError> {
        self.transition(RuntimePhase::Running)
    }

    pub fn wait_for_approval(&self) -> Result<(), RuntimeCoordinatorError> {
        self.transition(RuntimePhase::WaitingApproval)
    }

    pub fn wait_for_user(&self) -> Result<(), RuntimeCoordinatorError> {
        self.transition(RuntimePhase::WaitingUser)
    }

    pub fn begin_finalizing(&self) -> Result<(), RuntimeCoordinatorError> {
        self.transition(RuntimePhase::Finalizing)
    }

    /// 完成本轮并释放全局闸门。
    pub fn finish(mut self) -> Result<(), RuntimeCoordinatorError> {
        let phase = self.coordinator.snapshot()?.phase;
        if phase != RuntimePhase::Finalizing {
            self.begin_finalizing()?;
        }
        self.coordinator.release(self.generation, &self.turn_id)?;
        self.released = true;
        Ok(())
    }
}

impl Drop for TurnLease {
    fn drop(&mut self) {
        if !self.released {
            let _ = self.coordinator.release(self.generation, &self.turn_id);
            self.released = true;
        }
    }
}

/// 可克隆的取消观察端，用于模型流和工具执行共享取消信号。
#[derive(Debug, Clone)]
pub struct TurnCancellation {
    receiver: watch::Receiver<bool>,
}

impl TurnCancellation {
    pub fn is_cancelled(&self) -> bool {
        *self.receiver.borrow()
    }

    pub async fn cancelled(&mut self) {
        while !self.is_cancelled() {
            if self.receiver.changed().await.is_err() {
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{RuntimeCoordinator, RuntimeCoordinatorError, RuntimePhase};

    #[test]
    fn only_one_turn_can_hold_the_runtime_gate() {
        let coordinator = RuntimeCoordinator::new();
        let first = coordinator.begin_turn("turn-1", "conversation-1").unwrap();

        assert!(matches!(
            coordinator.begin_turn("turn-2", "conversation-2"),
            Err(RuntimeCoordinatorError::Busy { .. })
        ));
        assert!(matches!(
            coordinator.ensure_idle(),
            Err(RuntimeCoordinatorError::Busy { .. })
        ));

        drop(first);
        assert!(coordinator.ensure_idle().is_ok());
    }

    #[test]
    fn lease_enforces_the_explicit_transition_graph() {
        let coordinator = RuntimeCoordinator::new();
        let lease = coordinator.begin_turn("turn-1", "conversation-1").unwrap();

        assert!(matches!(
            lease.wait_for_user(),
            Err(RuntimeCoordinatorError::InvalidTransition { .. })
        ));
        lease.mark_running().unwrap();
        lease.wait_for_approval().unwrap();
        lease.mark_running().unwrap();
        lease.begin_finalizing().unwrap();
        lease.finish().unwrap();

        assert_eq!(coordinator.snapshot().unwrap().phase, RuntimePhase::Idle);
    }

    #[tokio::test]
    async fn cancellation_changes_phase_and_notifies_all_observers() {
        let coordinator = RuntimeCoordinator::new();
        let lease = coordinator.begin_turn("turn-1", "conversation-1").unwrap();
        lease.mark_running().unwrap();
        let mut cancellation = lease.cancellation();

        coordinator.cancel_turn("turn-1").unwrap();
        cancellation.cancelled().await;

        assert!(cancellation.is_cancelled());
        assert_eq!(
            coordinator.snapshot().unwrap().phase,
            RuntimePhase::Cancelling
        );
        lease.finish().unwrap();
    }

    #[test]
    fn cancellation_rejects_a_different_turn_id() {
        let coordinator = RuntimeCoordinator::new();
        let _lease = coordinator.begin_turn("turn-1", "conversation-1").unwrap();

        assert!(matches!(
            coordinator.cancel_turn("turn-2"),
            Err(RuntimeCoordinatorError::TurnNotFound { .. })
        ));
    }

    #[test]
    fn idle_lease_closes_the_resume_or_fork_race_window() {
        let coordinator = RuntimeCoordinator::new();
        let lease = coordinator.acquire_idle_lease("resume_session").unwrap();

        assert_eq!(lease.operation(), "resume_session");
        assert_eq!(
            coordinator
                .snapshot()
                .unwrap()
                .exclusive_operation
                .as_deref(),
            Some("resume_session")
        );
        assert!(matches!(
            coordinator.begin_turn("turn-1", "conversation-1"),
            Err(RuntimeCoordinatorError::ExclusiveOperationBusy { .. })
        ));
        assert!(matches!(
            coordinator.acquire_idle_lease("fork_session"),
            Err(RuntimeCoordinatorError::ExclusiveOperationBusy { .. })
        ));

        lease.finish().unwrap();
        let _turn = coordinator.begin_turn("turn-1", "conversation-1").unwrap();
    }

    #[test]
    fn active_turn_blocks_an_idle_lease() {
        let coordinator = RuntimeCoordinator::new();
        let _turn = coordinator.begin_turn("turn-1", "conversation-1").unwrap();

        assert!(matches!(
            coordinator.acquire_idle_lease("clear_session"),
            Err(RuntimeCoordinatorError::Busy { .. })
        ));
    }
}

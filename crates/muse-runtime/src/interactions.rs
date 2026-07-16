//! 运行时审批与用户问答登记。
//!
//! 这些登记属于回合事实状态，不能由 HTTP 适配层持有裸 `HashMap`。本模块只暴露
//! 决策、待处理记录和窄结果类型；实际 registry 始终由 `RuntimeService` 的单一互斥
//! 状态持有，从而保证 pending 到 resolved 的切换、幂等判断与有序淘汰原子完成。

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use tokio::sync::oneshot;

const RESOLVED_INTERACTION_LIMIT: usize = 256;
const RESOLVED_INTERACTION_TTL: Duration = Duration::from_secs(15 * 60);

/// 用户对待审批工具调用的决策。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalDecision {
    pub approved: bool,
    pub reason: Option<String>,
}

/// 用户对交互式问题的回答。
#[derive(Debug, Clone, PartialEq)]
pub struct UserQuestionDecision {
    pub answered: bool,
    pub answers: Option<serde_json::Value>,
    pub annotations: Option<serde_json::Value>,
    pub reason: Option<String>,
}

/// 等待审批恢复的工具调用记录。
#[derive(Debug)]
pub struct PendingApproval {
    pub turn_id: String,
    pub tool_name: String,
    pub risk: String,
    pub summary: String,
    pub tx: oneshot::Sender<ApprovalDecision>,
}

/// 等待用户回答恢复的工具调用记录。
#[derive(Debug)]
pub struct PendingUserQuestion {
    pub turn_id: String,
    pub tool_name: String,
    pub summary: String,
    pub tx: oneshot::Sender<UserQuestionDecision>,
}

/// 成功处理交互决策后返回给适配层的非敏感元数据。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InteractionResolution {
    pub tool_name: Option<String>,
    pub risk: Option<String>,
    pub idempotent: bool,
}

/// pending 登记失败。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InteractionRegistrationError {
    Duplicate { request_id: String },
}

impl std::fmt::Display for InteractionRegistrationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Duplicate { request_id } => {
                write!(formatter, "交互请求 `{request_id}` 已经登记或处理。")
            }
        }
    }
}

impl std::error::Error for InteractionRegistrationError {}

/// 精确 resolve 失败；调用方可把冲突映射为 HTTP 409、缺失映射为 404。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InteractionResolveError {
    NotFound,
    TurnMismatch,
    RuntimeNotWaiting,
    ConflictingDecision,
    ReceiverClosed,
}

impl std::fmt::Display for InteractionResolveError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound => formatter.write_str("交互请求不存在或已经过期。"),
            Self::TurnMismatch => formatter.write_str("交互请求属于其他回合。"),
            Self::RuntimeNotWaiting => formatter.write_str("运行时已不再等待该交互请求。"),
            Self::ConflictingDecision => formatter.write_str("交互请求已经以不同结果处理。"),
            Self::ReceiverClosed => formatter.write_str("交互请求对应的回合已不再等待。"),
        }
    }
}

impl std::error::Error for InteractionResolveError {}

#[derive(Debug, Clone)]
struct ResolvedApproval {
    turn_id: String,
    decision: ApprovalDecision,
    resolved_at: Instant,
}

#[derive(Debug, Clone)]
struct ResolvedUserQuestion {
    turn_id: String,
    decision: UserQuestionDecision,
    resolved_at: Instant,
}

/// 由 `RuntimeService` 的单一异步互斥锁持有的四类 registry。
#[derive(Debug, Default)]
pub(crate) struct InteractionRegistries {
    pending_approvals: HashMap<String, PendingApproval>,
    resolved_approvals: HashMap<String, ResolvedApproval>,
    resolved_approval_order: VecDeque<String>,
    pending_user_questions: HashMap<String, PendingUserQuestion>,
    resolved_user_questions: HashMap<String, ResolvedUserQuestion>,
    resolved_user_question_order: VecDeque<String>,
}

impl InteractionRegistries {
    pub(crate) fn approval_requires_first_resolution(
        &mut self,
        approval_id: &str,
        turn_id: &str,
        decision: &ApprovalDecision,
    ) -> Result<bool, InteractionResolveError> {
        self.purge_expired();
        if let Some(pending) = self.pending_approvals.get(approval_id) {
            return if pending.turn_id == turn_id {
                Ok(true)
            } else {
                Err(InteractionResolveError::TurnMismatch)
            };
        }
        let Some(resolved) = self.resolved_approvals.get(approval_id) else {
            return Err(InteractionResolveError::NotFound);
        };
        if resolved.turn_id != turn_id {
            return Err(InteractionResolveError::TurnMismatch);
        }
        if &resolved.decision != decision {
            return Err(InteractionResolveError::ConflictingDecision);
        }
        Ok(false)
    }

    pub(crate) fn user_question_requires_first_resolution(
        &mut self,
        request_id: &str,
        turn_id: &str,
        decision: &UserQuestionDecision,
    ) -> Result<bool, InteractionResolveError> {
        self.purge_expired();
        if let Some(pending) = self.pending_user_questions.get(request_id) {
            return if pending.turn_id == turn_id {
                Ok(true)
            } else {
                Err(InteractionResolveError::TurnMismatch)
            };
        }
        let Some(resolved) = self.resolved_user_questions.get(request_id) else {
            return Err(InteractionResolveError::NotFound);
        };
        if resolved.turn_id != turn_id {
            return Err(InteractionResolveError::TurnMismatch);
        }
        if &resolved.decision != decision {
            return Err(InteractionResolveError::ConflictingDecision);
        }
        Ok(false)
    }

    pub(crate) fn register_approval(
        &mut self,
        approval_id: String,
        pending: PendingApproval,
    ) -> Result<(), InteractionRegistrationError> {
        self.purge_expired();
        if self.pending_approvals.contains_key(&approval_id)
            || self.resolved_approvals.contains_key(&approval_id)
        {
            return Err(InteractionRegistrationError::Duplicate {
                request_id: approval_id,
            });
        }
        self.pending_approvals.insert(approval_id, pending);
        Ok(())
    }

    pub(crate) fn register_user_question(
        &mut self,
        request_id: String,
        pending: PendingUserQuestion,
    ) -> Result<(), InteractionRegistrationError> {
        self.purge_expired();
        if self.pending_user_questions.contains_key(&request_id)
            || self.resolved_user_questions.contains_key(&request_id)
        {
            return Err(InteractionRegistrationError::Duplicate { request_id });
        }
        self.pending_user_questions.insert(request_id, pending);
        Ok(())
    }

    pub(crate) fn resolve_approval(
        &mut self,
        approval_id: &str,
        turn_id: &str,
        decision: ApprovalDecision,
    ) -> Result<
        (
            InteractionResolution,
            Option<oneshot::Sender<ApprovalDecision>>,
        ),
        InteractionResolveError,
    > {
        self.purge_expired();
        if let Some(pending) = self.pending_approvals.get(approval_id) {
            if pending.turn_id != turn_id {
                return Err(InteractionResolveError::TurnMismatch);
            }
        } else if let Some(resolved) = self.resolved_approvals.get(approval_id) {
            if resolved.turn_id != turn_id {
                return Err(InteractionResolveError::TurnMismatch);
            }
            if resolved.decision != decision {
                return Err(InteractionResolveError::ConflictingDecision);
            }
            return Ok((
                InteractionResolution {
                    tool_name: None,
                    risk: None,
                    idempotent: true,
                },
                None,
            ));
        } else {
            return Err(InteractionResolveError::NotFound);
        }

        let pending = self
            .pending_approvals
            .remove(approval_id)
            .expect("前置检查已确认审批登记存在");
        let resolution = InteractionResolution {
            tool_name: Some(pending.tool_name),
            risk: Some(pending.risk),
            idempotent: false,
        };
        self.insert_resolved_approval(
            approval_id.to_string(),
            ResolvedApproval {
                turn_id: turn_id.to_string(),
                decision,
                resolved_at: Instant::now(),
            },
        );
        Ok((resolution, Some(pending.tx)))
    }

    pub(crate) fn resolve_user_question(
        &mut self,
        request_id: &str,
        turn_id: &str,
        decision: UserQuestionDecision,
    ) -> Result<
        (
            InteractionResolution,
            Option<oneshot::Sender<UserQuestionDecision>>,
        ),
        InteractionResolveError,
    > {
        self.purge_expired();
        if let Some(pending) = self.pending_user_questions.get(request_id) {
            if pending.turn_id != turn_id {
                return Err(InteractionResolveError::TurnMismatch);
            }
        } else if let Some(resolved) = self.resolved_user_questions.get(request_id) {
            if resolved.turn_id != turn_id {
                return Err(InteractionResolveError::TurnMismatch);
            }
            if resolved.decision != decision {
                return Err(InteractionResolveError::ConflictingDecision);
            }
            return Ok((
                InteractionResolution {
                    tool_name: None,
                    risk: None,
                    idempotent: true,
                },
                None,
            ));
        } else {
            return Err(InteractionResolveError::NotFound);
        }

        let pending = self
            .pending_user_questions
            .remove(request_id)
            .expect("前置检查已确认用户问题登记存在");
        let resolution = InteractionResolution {
            tool_name: Some(pending.tool_name),
            risk: None,
            idempotent: false,
        };
        self.insert_resolved_user_question(
            request_id.to_string(),
            ResolvedUserQuestion {
                turn_id: turn_id.to_string(),
                decision,
                resolved_at: Instant::now(),
            },
        );
        Ok((resolution, Some(pending.tx)))
    }

    pub(crate) fn remove_approval(&mut self, approval_id: &str, turn_id: &str) -> bool {
        if self
            .pending_approvals
            .get(approval_id)
            .is_some_and(|pending| pending.turn_id == turn_id)
        {
            self.pending_approvals.remove(approval_id);
            return true;
        }
        false
    }

    pub(crate) fn remove_user_question(&mut self, request_id: &str, turn_id: &str) -> bool {
        if self
            .pending_user_questions
            .get(request_id)
            .is_some_and(|pending| pending.turn_id == turn_id)
        {
            self.pending_user_questions.remove(request_id);
            return true;
        }
        false
    }

    pub(crate) fn remove_pending_for_turn(&mut self, turn_id: &str) -> bool {
        let before_approvals = self.pending_approvals.len();
        let before_questions = self.pending_user_questions.len();
        self.pending_approvals
            .retain(|_, pending| pending.turn_id != turn_id);
        self.pending_user_questions
            .retain(|_, pending| pending.turn_id != turn_id);
        before_approvals != self.pending_approvals.len()
            || before_questions != self.pending_user_questions.len()
    }

    pub(crate) fn cancel_pending_for_turn(
        &mut self,
        turn_id: &str,
    ) -> (
        Vec<oneshot::Sender<ApprovalDecision>>,
        Vec<oneshot::Sender<UserQuestionDecision>>,
    ) {
        let approval_ids = self
            .pending_approvals
            .iter()
            .filter(|(_, pending)| pending.turn_id == turn_id)
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        let question_ids = self
            .pending_user_questions
            .iter()
            .filter(|(_, pending)| pending.turn_id == turn_id)
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        let approvals = approval_ids
            .into_iter()
            .filter_map(|id| self.pending_approvals.remove(&id).map(|pending| pending.tx))
            .collect();
        let questions = question_ids
            .into_iter()
            .filter_map(|id| {
                self.pending_user_questions
                    .remove(&id)
                    .map(|pending| pending.tx)
            })
            .collect();
        (approvals, questions)
    }

    pub(crate) fn pending_counts(&self) -> (usize, usize) {
        (
            self.pending_approvals.len(),
            self.pending_user_questions.len(),
        )
    }

    fn insert_resolved_approval(&mut self, id: String, resolved: ResolvedApproval) {
        self.resolved_approvals.insert(id.clone(), resolved);
        self.resolved_approval_order.push_back(id);
        while self.resolved_approval_order.len() > RESOLVED_INTERACTION_LIMIT {
            if let Some(oldest) = self.resolved_approval_order.pop_front() {
                self.resolved_approvals.remove(&oldest);
            }
        }
    }

    fn insert_resolved_user_question(&mut self, id: String, resolved: ResolvedUserQuestion) {
        self.resolved_user_questions.insert(id.clone(), resolved);
        self.resolved_user_question_order.push_back(id);
        while self.resolved_user_question_order.len() > RESOLVED_INTERACTION_LIMIT {
            if let Some(oldest) = self.resolved_user_question_order.pop_front() {
                self.resolved_user_questions.remove(&oldest);
            }
        }
    }

    fn purge_expired(&mut self) {
        let now = Instant::now();
        while self
            .resolved_approval_order
            .front()
            .and_then(|id| self.resolved_approvals.get(id))
            .is_some_and(|resolved| {
                now.saturating_duration_since(resolved.resolved_at) >= RESOLVED_INTERACTION_TTL
            })
        {
            if let Some(id) = self.resolved_approval_order.pop_front() {
                self.resolved_approvals.remove(&id);
            }
        }
        while self
            .resolved_user_question_order
            .front()
            .and_then(|id| self.resolved_user_questions.get(id))
            .is_some_and(|resolved| {
                now.saturating_duration_since(resolved.resolved_at) >= RESOLVED_INTERACTION_TTL
            })
        {
            if let Some(id) = self.resolved_user_question_order.pop_front() {
                self.resolved_user_questions.remove(&id);
            }
        }
    }
}

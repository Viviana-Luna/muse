//! Turn 作用域的记忆运行时状态：变更暂存与查询预算记账。
//!
//! 本模块只持有当前 Turn 内存态，不接触持久层；Turn 取消或失败时整个状态随
//! 作用域丢弃，绝不会把暂存写入 Repository。durable 提交只能发生在
//! `publish_committed_turn` 的 `append_turn_committed` 成功之后。

use std::collections::BTreeSet;
use std::sync::Mutex;

use muse_core::domain::conversation::{Conversation, Role};
use muse_core::domain::memory::{
    MEMORY_QUERY_TOOL_NAME, MemoryCommitEnvelope, MemoryDeleteParams, MemoryError, MemoryErrorCode,
    MemoryId, MemoryPersonaScope, MemoryStagedMutation,
};

/// 一次查询页与工具调用的绑定；durable 删除后按该绑定定位工作副本中的旧结果页。
#[derive(Debug, Clone)]
pub(crate) struct MemoryQueryPageBinding {
    pub call_id: String,
    pub memory_ids: Vec<MemoryId>,
}

#[derive(Debug)]
struct MemoryTurnInner {
    staged: Vec<MemoryStagedMutation>,
    query_pages: Vec<MemoryQueryPageBinding>,
    queries_used: u32,
    query_call_budget: u32,
}

/// 单 Turn 记忆状态；工具处理器在 async 上下文中只持锁做短临界区，不跨 await。
#[derive(Debug)]
pub(crate) struct MemoryTurnState {
    inner: Mutex<MemoryTurnInner>,
}

impl MemoryTurnState {
    /// 预算值由本 Turn 的冻结服务配置注入，本切片不虚构固定数值。
    pub(crate) fn new(query_call_budget: u32) -> Self {
        Self {
            inner: Mutex::new(MemoryTurnInner {
                staged: Vec::new(),
                query_pages: Vec::new(),
                queries_used: 0,
                query_call_budget,
            }),
        }
    }

    /// 记账一次查询；预算耗尽返回 false，调用方必须转为稳定失败而不是继续检索。
    pub(crate) fn try_consume_query_budget(&self) -> bool {
        let mut inner = self.inner.lock().expect("记忆 Turn 状态锁中毒");
        if inner.queries_used >= inner.query_call_budget {
            return false;
        }
        inner.queries_used += 1;
        true
    }

    /// 暂存一条已通过第一层敏感门的变更；只入 Turn 内存，不触发任何持久写入。
    pub(crate) fn stage(&self, mutation: MemoryStagedMutation) {
        let mut inner = self.inner.lock().expect("记忆 Turn 状态锁中毒");
        inner.staged.push(mutation);
    }

    /// 在 `turn_committed` 已可靠写入后取出暂存并构造幂等提交封套。
    pub(crate) fn take_committed_envelope(
        &self,
        persona_id: Option<&str>,
        conversation_id: &str,
        turn_id: &str,
        committed_at: &str,
    ) -> Result<Option<MemoryCommitEnvelope>, MemoryError> {
        let mut inner = self.inner.lock().expect("记忆 Turn 状态锁中毒");
        if inner.staged.is_empty() {
            return Ok(None);
        }
        let Some(persona_id) = persona_id else {
            return Err(MemoryError::new(MemoryErrorCode::SourceIneligible));
        };
        let scope = MemoryPersonaScope::new(persona_id)?;
        let staged = std::mem::take(&mut inner.staged);
        MemoryCommitEnvelope::new(
            format!("memory-turn:{turn_id}"),
            scope,
            conversation_id,
            turn_id,
            committed_at,
            staged,
        )
        .map(Some)
    }

    /// 记录一次成功查询页命中的记忆 ID，供 durable 删除后替换工作副本收据。
    pub(crate) fn record_query_page(&self, call_id: String, memory_ids: Vec<MemoryId>) {
        let mut inner = self.inner.lock().expect("记忆 Turn 状态锁中毒");
        inner.query_pages.push(MemoryQueryPageBinding {
            call_id,
            memory_ids,
        });
    }

    /// 返回命中指定记忆的查询页调用 ID；None 表示 Persona 全量删除，命中所有页。
    pub(crate) fn query_call_ids_touching(&self, memory_id: Option<&MemoryId>) -> Vec<String> {
        let inner = self.inner.lock().expect("记忆 Turn 状态锁中毒");
        inner
            .query_pages
            .iter()
            .filter(|page| {
                memory_id.is_none() || memory_id.is_some_and(|id| page.memory_ids.contains(id))
            })
            .map(|page| page.call_id.clone())
            .collect()
    }

    /// durable 删除后，把当前私有工作副本中受影响的旧查询正文替换为无正文收据。
    pub(crate) fn redact_deleted_query_results(
        &self,
        conversation: &mut Conversation,
        params: &MemoryDeleteParams,
    ) -> usize {
        let memory_id = match params {
            MemoryDeleteParams::Memory { memory_id } => Some(memory_id),
            MemoryDeleteParams::PersonaAll => None,
        };
        let call_ids = self
            .query_call_ids_touching(memory_id)
            .into_iter()
            .collect::<BTreeSet<_>>();
        let mut replaced = 0;
        for message in &mut conversation.messages {
            if message.role == Role::Tool
                && message.tool_name.as_deref() == Some(MEMORY_QUERY_TOOL_NAME)
                && message
                    .tool_call_id
                    .as_ref()
                    .is_some_and(|call_id| call_ids.contains(call_id))
            {
                message.content = "该查询页命中的记忆已删除，正文不再可用。".to_string();
                replaced += 1;
            }
        }
        replaced
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use muse_core::domain::memory::{
        ConfirmedMemoryDeleteRequest, MemoryBatchCommitReceipt, MemoryCategory,
        MemoryDeleteReceipt, MemoryDeletionAuthority, MemoryImportance, MemoryImportanceAdjustment,
        MemoryImportanceAdjustmentReceipt, MemoryManagementContentMutation, MemoryMutateParams,
        MemoryMutationReceipt, MemoryMutationReceiptState, MemoryRecord, MemoryRepository,
        MemoryRevisionId, MemoryRuntimeBinding, MemorySafetyAssessment, MemorySensitivityPolicy,
        MemorySensitivityRequest, MemorySourceKind,
    };
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct AllowPolicy;

    impl MemorySensitivityPolicy for AllowPolicy {
        fn assess(&self, request: MemorySensitivityRequest<'_>) -> MemorySafetyAssessment {
            MemorySafetyAssessment::Allowed {
                stage: request.stage,
                policy_version: "test-policy-v1".to_string(),
            }
        }
    }

    struct RecordingRepository {
        calls: Arc<AtomicUsize>,
        failure: Option<MemoryErrorCode>,
    }

    impl MemoryRepository for RecordingRepository {
        fn current(
            &self,
            _scope: &MemoryPersonaScope,
            _memory_id: &MemoryId,
        ) -> Result<Option<MemoryRecord>, MemoryError> {
            unreachable!("测试不读取当前记忆")
        }

        fn apply_committed_batch(
            &self,
            envelope: &MemoryCommitEnvelope,
            _sensitivity: &dyn MemorySensitivityPolicy,
        ) -> Result<MemoryBatchCommitReceipt, MemoryError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if let Some(code) = self.failure {
                return Err(MemoryError::new(code));
            }
            let mutations = envelope
                .mutations()
                .iter()
                .map(|mutation| {
                    let mut receipt = mutation.staged_receipt();
                    receipt.state = MemoryMutationReceiptState::Durable;
                    receipt
                })
                .collect();
            Ok(MemoryBatchCommitReceipt {
                idempotency_key: envelope.idempotency_key().to_string(),
                mutations,
                durable_at: envelope.committed_at().to_string(),
            })
        }

        fn apply_management_content_mutation(
            &self,
            _mutation: &MemoryManagementContentMutation,
            _sensitivity: &dyn MemorySensitivityPolicy,
        ) -> Result<MemoryMutationReceipt, MemoryError> {
            unreachable!("测试不执行管理写入")
        }

        fn adjust_importance(
            &self,
            _adjustment: &MemoryImportanceAdjustment,
        ) -> Result<MemoryImportanceAdjustmentReceipt, MemoryError> {
            unreachable!("测试不调整重要程度")
        }

        fn delete_confirmed(
            &self,
            _request: &ConfirmedMemoryDeleteRequest,
            _authority: &dyn MemoryDeletionAuthority,
        ) -> Result<MemoryDeleteReceipt, MemoryError> {
            unreachable!("测试不删除记忆")
        }
    }

    #[test]
    fn query_budget_is_bounded_per_turn() {
        let state = MemoryTurnState::new(2);
        assert!(state.try_consume_query_budget());
        assert!(state.try_consume_query_budget());
        assert!(!state.try_consume_query_budget());
        assert!(!state.try_consume_query_budget());
    }

    #[test]
    fn zero_budget_fails_closed() {
        let state = MemoryTurnState::new(0);
        assert!(!state.try_consume_query_budget());
    }

    #[test]
    fn query_pages_are_matched_by_memory_id() {
        let state = MemoryTurnState::new(1);
        state.record_query_page("call-1".to_string(), vec![MemoryId("mem-a".to_string())]);
        state.record_query_page(
            "call-2".to_string(),
            vec![MemoryId("mem-a".to_string()), MemoryId("mem-b".to_string())],
        );
        state.record_query_page("call-3".to_string(), vec![MemoryId("mem-c".to_string())]);

        let touching_a = state.query_call_ids_touching(Some(&MemoryId("mem-a".to_string())));
        assert_eq!(touching_a, vec!["call-1".to_string(), "call-2".to_string()]);
        let touching_all = state.query_call_ids_touching(None);
        assert_eq!(
            touching_all,
            vec![
                "call-1".to_string(),
                "call-2".to_string(),
                "call-3".to_string()
            ]
        );
        let touching_none = state.query_call_ids_touching(Some(&MemoryId("mem-z".to_string())));
        assert!(touching_none.is_empty());
    }

    #[test]
    fn durable_delete_redacts_only_matching_query_pages() {
        let state = MemoryTurnState::new(2);
        state.record_query_page("call-a".to_string(), vec![MemoryId("mem-a".to_string())]);
        state.record_query_page("call-b".to_string(), vec![MemoryId("mem-b".to_string())]);
        let mut conversation = Conversation::new("system".to_string(), 20);
        conversation.add_user_message("问题".to_string());
        conversation.add_assistant_tool_call(
            "call-a".to_string(),
            MEMORY_QUERY_TOOL_NAME.to_string(),
            serde_json::json!({ "query": "a" }),
        );
        conversation.add_tool_result(
            "call-a".to_string(),
            MEMORY_QUERY_TOOL_NAME.to_string(),
            "包含 mem-a 正文".to_string(),
        );
        conversation.add_assistant_tool_call(
            "call-b".to_string(),
            MEMORY_QUERY_TOOL_NAME.to_string(),
            serde_json::json!({ "query": "b" }),
        );
        conversation.add_tool_result(
            "call-b".to_string(),
            MEMORY_QUERY_TOOL_NAME.to_string(),
            "包含 mem-b 正文".to_string(),
        );

        let replaced = state.redact_deleted_query_results(
            &mut conversation,
            &MemoryDeleteParams::Memory {
                memory_id: MemoryId("mem-a".to_string()),
            },
        );

        assert_eq!(replaced, 1);
        let contents = conversation
            .messages
            .iter()
            .filter(|message| message.role == Role::Tool)
            .map(|message| message.content.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            contents,
            vec![
                "该查询页命中的记忆已删除，正文不再可用。",
                "包含 mem-b 正文"
            ]
        );
    }

    #[test]
    fn committed_turn_drains_staged_mutations_exactly_once() {
        let state = MemoryTurnState::new(1);
        let scope = MemoryPersonaScope::new("persona-test").expect("Persona scope 应有效");
        let binding = MemoryRuntimeBinding::new(
            scope,
            "conversation-test",
            "turn-test",
            "operation-test",
            MemorySourceKind::DirectUserMessage,
            "2026-08-01T00:00:00Z",
            "2026-08-01T00:00:00Z",
            "2026-08-01T00:00:00Z",
        )
        .expect("运行时绑定应有效");
        let staged = MemoryStagedMutation::stage(
            MemoryMutateParams::Create {
                category: MemoryCategory::UserPreference,
                content: "用户喜欢夜间散步".to_string(),
                importance: MemoryImportance::Normal,
                event_time: None,
                change_reason: "用户在本轮直接说明".to_string(),
            },
            binding,
            MemoryId("memory-test".to_string()),
            MemoryRevisionId("revision-test".to_string()),
            &AllowPolicy,
        )
        .expect("普通记忆应允许暂存");
        state.stage(staged);

        let envelope = state
            .take_committed_envelope(
                Some("persona-test"),
                "conversation-test",
                "turn-test",
                "2026-08-01T00:01:00Z",
            )
            .expect("提交封套应构造成功")
            .expect("应包含一条暂存");
        assert_eq!(envelope.idempotency_key(), "memory-turn:turn-test");
        assert_eq!(envelope.mutations().len(), 1);
        assert!(
            state
                .take_committed_envelope(
                    Some("persona-test"),
                    "conversation-test",
                    "turn-test",
                    "2026-08-01T00:01:00Z",
                )
                .expect("重复收尾应安全跳过")
                .is_none()
        );
    }

    #[test]
    fn post_commit_application_preserves_success_and_failure_codes() {
        let success_calls = Arc::new(AtomicUsize::new(0));
        let success = super::super::apply_memory_commit_envelope(
            Arc::new(RecordingRepository {
                calls: Arc::clone(&success_calls),
                failure: None,
            }),
            Arc::new(AllowPolicy),
            staged_envelope("turn-success"),
        );
        assert_eq!(success, Ok(1));
        assert_eq!(success_calls.load(Ordering::SeqCst), 1);

        let failure_calls = Arc::new(AtomicUsize::new(0));
        let failure = super::super::apply_memory_commit_envelope(
            Arc::new(RecordingRepository {
                calls: Arc::clone(&failure_calls),
                failure: Some(MemoryErrorCode::RepositoryUnavailable),
            }),
            Arc::new(AllowPolicy),
            staged_envelope("turn-failure"),
        );
        assert_eq!(failure, Err(MemoryErrorCode::RepositoryUnavailable));
        assert_eq!(failure_calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn post_commit_failure_event_is_stable_and_body_free() {
        let event =
            super::super::memory_commit_result_event(Err(MemoryErrorCode::RepositoryUnavailable));
        let muse_core::domain::protocol::RuntimeEvent::Status {
            phase,
            message,
            detail,
            state,
        } = event
        else {
            panic!("记忆提交失败必须复用稳定 Status 事件")
        };
        assert_eq!(phase, "memory_commit_failed");
        assert_eq!(message, "本次记忆未保存。");
        assert_eq!(detail.as_deref(), Some("memory_repository_unavailable"));
        assert_eq!(state, "error");
        assert!(!format!("{message}{detail:?}").contains("用户喜欢夜间散步"));
    }

    fn staged_envelope(turn_id: &str) -> MemoryCommitEnvelope {
        let scope = MemoryPersonaScope::new("persona-test").expect("Persona scope 应有效");
        let binding = MemoryRuntimeBinding::new(
            scope.clone(),
            "conversation-test",
            turn_id,
            format!("operation-{turn_id}"),
            MemorySourceKind::DirectUserMessage,
            "2026-08-01T00:00:00Z",
            "2026-08-01T00:00:00Z",
            "2026-08-01T00:00:00Z",
        )
        .expect("运行时绑定应有效");
        let staged = MemoryStagedMutation::stage(
            MemoryMutateParams::Create {
                category: MemoryCategory::UserPreference,
                content: "用户喜欢夜间散步".to_string(),
                importance: MemoryImportance::Normal,
                event_time: None,
                change_reason: "用户在本轮直接说明".to_string(),
            },
            binding,
            MemoryId(format!("memory-{turn_id}")),
            MemoryRevisionId(format!("revision-{turn_id}")),
            &AllowPolicy,
        )
        .expect("普通记忆应允许暂存");
        MemoryCommitEnvelope::new(
            format!("memory-turn:{turn_id}"),
            scope,
            "conversation-test",
            turn_id,
            "2026-08-01T00:01:00Z",
            vec![staged],
        )
        .expect("提交封套应有效")
    }
}

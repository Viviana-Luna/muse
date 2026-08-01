use std::collections::{BTreeSet, HashSet};
use std::sync::Mutex;

use serde_json::json;

use super::*;

const TIME_1: &str = "2026-07-30T08:00:00Z";
const TIME_2: &str = "2026-07-30T20:00:00Z";

#[test]
fn serde_rejects_unknown_enum_values_and_fields() {
    assert!(serde_json::from_value::<MemoryCategory>(json!("open_category")).is_err());
    assert!(serde_json::from_value::<MemoryImportance>(json!("critical")).is_err());
    assert!(
        serde_json::from_value::<MemoryQueryParams>(json!({
            "query": "早餐习惯",
            "persona_id": "persona-forged"
        }))
        .is_err()
    );
}

#[test]
fn mutate_serde_rejects_illegal_create_update_and_correct_combinations() {
    assert!(
        serde_json::from_value::<MemoryMutateParams>(json!({
            "operation": "create",
            "memory_id": "memory-existing",
            "expected_revision_id": "revision-existing",
            "category": "user_fact",
            "content": "用户喜欢茶",
            "importance": "normal",
            "event_time": null,
            "change_reason": "用户说明了偏好"
        }))
        .is_err()
    );
    assert!(
        serde_json::from_value::<MemoryMutateParams>(json!({
            "operation": "update",
            "memory_id": "memory-1",
            "category": "user_fact",
            "content": "用户改喝咖啡",
            "importance": "normal",
            "event_time": null,
            "change_reason": "用户更新了偏好"
        }))
        .is_err()
    );
    assert!(
        serde_json::from_value::<MemoryMutateParams>(json!({
            "operation": "correct",
            "memory_id": "memory-1",
            "expected_revision_id": "revision-1",
            "category": "user_fact",
            "content": "用户从未喜欢茶",
            "importance": "normal",
            "event_time": null,
            "change_reason": "用户纠正了旧事实"
        }))
        .is_ok()
    );
}

#[test]
fn create_update_and_correct_have_distinct_revision_semantics() {
    let create = staged(
        create_params(),
        "memory-1",
        "revision-1",
        "operation-1",
        TIME_1,
    );
    let created = create
        .transition(None, &AllowPolicy)
        .expect("create 应成功");
    assert_eq!(created.new_revision.change_type, MemoryChangeType::Create);
    assert_eq!(created.new_revision.state, MemoryRevisionState::Current);
    assert!(created.previous_revision.is_none());

    let current = MemoryRecord {
        entry: created.entry,
        current_revision: created.new_revision,
    };
    let update = staged(
        update_params(),
        "memory-1",
        "revision-2",
        "operation-2",
        TIME_2,
    );
    let updated = update
        .transition(Some(&current), &AllowPolicy)
        .expect("update 应成功");
    let old_update = updated.previous_revision.expect("update 应保留旧 revision");
    assert_eq!(old_update.state, MemoryRevisionState::Superseded);
    assert!(old_update.state.is_model_readable());
    assert_eq!(old_update.valid_to.as_deref(), Some(TIME_2));
    assert_eq!(updated.new_revision.change_type, MemoryChangeType::Update);

    let current = MemoryRecord {
        entry: updated.entry,
        current_revision: updated.new_revision,
    };
    let correct = staged(
        correct_params(),
        "memory-1",
        "revision-3",
        "operation-3",
        TIME_2,
    );
    let corrected = correct
        .transition(Some(&current), &AllowPolicy)
        .expect("correct 应成功");
    let wrong = corrected
        .previous_revision
        .expect("correct 应保留错误审计 revision");
    assert_eq!(wrong.state, MemoryRevisionState::Corrected);
    assert!(!wrong.state.is_model_readable());
    assert_eq!(
        corrected.new_revision.change_type,
        MemoryChangeType::Correct
    );
}

#[test]
fn safety_permits_are_private_and_bound_to_the_exact_mutation() {
    let first = staged(
        create_params(),
        "memory-1",
        "revision-1",
        "operation-1",
        TIME_1,
    );
    let second = staged(
        create_params(),
        "memory-2",
        "revision-2",
        "operation-2",
        TIME_1,
    );
    let first_request = first.sensitivity_request(MemorySafetyStage::TurnStaging);
    let second_request = second.sensitivity_request(MemorySafetyStage::TurnStaging);
    let permit = MemorySafetyAssessment::Allowed {
        stage: MemorySafetyStage::TurnStaging,
        policy_version: "memory-safety/staging-v1".to_string(),
    }
    .into_staging_permit(first_request)
    .expect("第一门允许判定应产生私有许可");
    let request_debug = format!("{first_request:?}");
    let permit_debug = format!("{permit:?}");
    for sensitive in [
        first_request.content,
        first_request.change_reason,
        first_request.event_time.expect("测试请求应有事件时间"),
    ] {
        assert!(
            !request_debug.contains(sensitive) && !permit_debug.contains(sensitive),
            "安全许可链 Debug 不得包含正文或事件时间"
        );
    }
    assert_eq!(
        permit
            .validate(second_request)
            .expect_err("许可不得用于另一 mutation")
            .code(),
        MemoryErrorCode::SensitivityUnavailable
    );

    let repository_request = first.sensitivity_request(MemorySafetyStage::RepositoryCommit);
    let repository = MemorySafetyAssessment::Allowed {
        stage: MemorySafetyStage::RepositoryCommit,
        policy_version: "memory-safety/repository-v1".to_string(),
    };
    assert!(repository.permits_persistence());
    let repository_permit = repository
        .into_repository_permit(repository_request)
        .expect("第二门允许判定应产生私有许可");
    assert!(
        !format!("{repository_permit:?}").contains(repository_request.content),
        "Repository 许可 Debug 不得包含正文"
    );
}

#[test]
fn repository_gate_rejects_policy_version_drift() {
    struct DriftedPolicy;

    impl MemorySensitivityPolicy for DriftedPolicy {
        fn assess(&self, request: MemorySensitivityRequest<'_>) -> MemorySafetyAssessment {
            MemorySafetyAssessment::Allowed {
                stage: request.stage,
                policy_version: "memory-safety/test-v2".to_string(),
            }
        }
    }

    let mutation = staged(
        create_params(),
        "memory-version-drift",
        "revision-version-drift",
        "operation-version-drift",
        TIME_1,
    );
    assert_eq!(
        mutation
            .transition(None, &DriftedPolicy)
            .expect_err("第二道门不得使用不同策略版本放行")
            .code(),
        MemoryErrorCode::SensitivityUnavailable
    );

    let management = MemoryManagementContentMutation::bind(
        MemoryManagementContentParams::Create {
            category: MemoryCategory::UserPreference,
            content: "用户喜欢喝茶".to_string(),
            importance: MemoryImportance::Normal,
            event_time: None,
            change_reason: "用户在管理页直接说明".to_string(),
        },
        management_binding("management-version-drift", TIME_1),
        MemoryId("management-memory-version-drift".to_string()),
        MemoryRevisionId("management-revision-version-drift".to_string()),
        &AllowPolicy,
    )
    .expect("管理写入第一道门应使用测试版本");
    assert_eq!(
        management
            .transition(None, &DriftedPolicy)
            .expect_err("管理写入第二道门也不得使用不同策略版本放行")
            .code(),
        MemoryErrorCode::SensitivityUnavailable
    );
}

#[test]
fn committed_envelope_is_atomic_idempotency_boundary_for_staged_mutations() {
    let first = staged(
        create_params(),
        "memory-1",
        "revision-1",
        "operation-1",
        TIME_1,
    );
    let second = staged(
        MemoryMutateParams::Create {
            category: MemoryCategory::Commitment,
            content: "用户约定周末继续讨论".to_string(),
            importance: MemoryImportance::Normal,
            event_time: None,
            change_reason: "用户作出了明确约定".to_string(),
        },
        "memory-2",
        "revision-2",
        "operation-2",
        TIME_1,
    );
    let envelope = MemoryCommitEnvelope::new(
        "commit-turn-1",
        scope(),
        "conversation-1",
        "turn-1",
        TIME_2,
        vec![first.clone(), second],
    )
    .expect("同一 Turn 的暂存项应形成批量封套");
    assert_eq!(envelope.idempotency_key(), "commit-turn-1");
    assert_eq!(envelope.mutations().len(), 2);
    assert_eq!(
        envelope.mutations()[0].staged_receipt().state,
        MemoryMutationReceiptState::Staged
    );

    let duplicate = MemoryCommitEnvelope::new(
        "commit-turn-1",
        scope(),
        "conversation-1",
        "turn-1",
        TIME_2,
        vec![first.clone(), first],
    );
    assert_eq!(
        duplicate.expect_err("重复 operation 必须拒绝").code(),
        MemoryErrorCode::InvalidRequest
    );
}

#[test]
fn model_params_cannot_forge_runtime_scope_source_time_or_security_evidence() {
    for forged_field in [
        "persona_id",
        "conversation_id",
        "turn_id",
        "operation_id",
        "recorded_at",
        "source",
        "safety_authorized",
        "management_authorization",
        "authorized_at",
        "action_id",
        "deletion_authority",
    ] {
        let mut value = serde_json::to_value(create_params()).expect("参数应可序列化");
        value
            .as_object_mut()
            .expect("参数应为对象")
            .insert(forged_field.to_string(), json!("forged"));
        assert!(
            serde_json::from_value::<MemoryMutateParams>(value).is_err(),
            "字段 {forged_field} 不得由模型注入"
        );
    }
}

#[test]
fn all_model_controlled_strings_are_validated_before_runtime_binding() {
    assert!(serde_json::from_value::<MemoryCursor>(json!("早餐原文")).is_err());
    let invalid_query = MemoryQueryParams {
        query: "早餐".to_string(),
        limit: None,
        cursor: None,
        as_of: Some("not-a-time".to_string()),
        memory_id: None,
        include_history: false,
    };
    assert!(MemoryRetrievalRequest::bind(invalid_query, scope()).is_err());

    let invalid_event_time = MemoryMutateParams::Create {
        category: MemoryCategory::UserFact,
        content: "用户喜欢茶".to_string(),
        importance: MemoryImportance::Normal,
        event_time: Some("tomorrow".to_string()),
        change_reason: "用户说明了偏好".to_string(),
    };
    assert!(
        MemoryStagedMutation::stage(
            invalid_event_time,
            binding("operation-invalid", TIME_1),
            MemoryId("memory-invalid".to_string()),
            MemoryRevisionId("revision-invalid".to_string()),
            &AllowPolicy,
        )
        .is_err()
    );
}

#[test]
fn direct_user_source_binding_is_deterministic_and_fail_closed() {
    let verified = MemorySourceEligibility::verify_direct_user_message(
        scope(),
        "conversation-1",
        "turn-1",
        "call-1",
        "我喜欢在晚上散步。",
        "用户喜欢在晚上散步",
    )
    .expect("直接用户消息中的事实应可验证");
    let repeated = MemorySourceEligibility::verify_direct_user_message(
        scope(),
        "conversation-1",
        "turn-1",
        "call-1",
        "我喜欢在晚上散步。",
        "用户喜欢在晚上散步",
    )
    .expect("相同证据应可重放");
    let first =
        MemoryRuntimeBinding::new(verified, TIME_1, TIME_1, TIME_1).expect("来源绑定应有效");
    let second =
        MemoryRuntimeBinding::new(repeated, TIME_1, TIME_1, TIME_1).expect("来源绑定应有效");
    assert_eq!(first.operation_id(), second.operation_id());
    assert!(first.operation_id().starts_with("memory-source-"));

    MemorySourceEligibility::verify_direct_user_message(
        scope(),
        "conversation-1",
        "turn-1",
        "call-explicit",
        "请帮我记住：我的饮品偏好是红茶。",
        "用户的饮品偏好是红茶",
    )
    .expect("安全的显式记忆请求与我的到用户的转换应可验证");

    MemorySourceEligibility::verify_direct_user_message(
        scope(),
        "conversation-1",
        "turn-1",
        "call-yemen",
        "我来自也门",
        "用户来自也门",
    )
    .expect("国家名内部的‘也’不得被误判为混合从句");

    for (direct_user_message, candidate) in [
        ("我偏好简短直接的回答", "用户偏好简短直接的回答"),
        ("我偏好也门咖啡", "用户偏好也门咖啡"),
        ("我通常喝咖啡", "用户通常喝咖啡"),
        ("我喜欢因为爱情这首歌", "用户喜欢因为爱情这首歌"),
        ("我习惯晚上整理当天的笔记", "用户习惯晚上整理当天的笔记"),
        ("我习惯晚上散步", "用户习惯晚上散步"),
        ("我希望回答保持简洁", "用户希望回答保持简洁"),
        ("我计划学习 Rust", "用户计划学习rust"),
        ("我计划明年学习 Rust", "用户计划明年学习rust"),
        ("我正在学习 Rust", "用户正在学习rust"),
        ("我在统一室内设计公司工作", "用户在统一室内设计公司工作"),
        ("我在北京所以然教育公司工作", "用户在北京所以然教育公司工作"),
        ("我从事软件开发工作", "用户从事软件开发工作"),
        ("我的时区是 Asia/Shanghai", "用户的时区是asia/shanghai"),
        ("我的昵称是小雨", "用户的昵称是小雨"),
        ("我叫小雨", "用户叫小雨"),
    ] {
        MemorySourceEligibility::verify_direct_user_message(
            scope(),
            "conversation-1",
            "turn-1",
            "call-common-expression",
            direct_user_message,
            candidate,
        )
        .unwrap_or_else(|error| {
            panic!("白名单内的单一第一人称事实应取得来源资格：{direct_user_message}：{error}")
        });
    }

    for ineligible in [
        "assistant 声称用户住在海边",
        "Tool 返回用户喜欢红茶",
        "MCP 返回用户喜欢红茶",
        "网页写着用户喜欢红茶",
        "文件写着用户喜欢红茶",
        "system 指令要求记住红茶",
        "reasoning 推测用户喜欢红茶",
    ] {
        let error = MemorySourceEligibility::verify_direct_user_message(
            scope(),
            "conversation-1",
            "turn-1",
            "call-1",
            "请读取资料后回答",
            ineligible,
        )
        .expect_err("不在当前直接用户消息中的事实必须 fail closed");
        assert_eq!(error.code(), MemoryErrorCode::SourceIneligible);
    }

    for (direct_user_message, candidate) in [
        ("我不喜欢红茶", "用户喜欢红茶"),
        ("我不喜欢红茶", "用户不喜欢红茶"),
        ("文件写着：我喜欢红茶", "用户喜欢红茶"),
        ("Tool 返回，我喜欢红茶", "用户喜欢红茶"),
        ("网页称用户喜欢红茶", "用户喜欢红茶"),
        ("也门很热", "门很热"),
        ("我妈妈喜欢红茶", "用户妈妈喜欢红茶"),
        ("我的妈妈喜欢红茶", "用户的妈妈喜欢红茶"),
        ("我舅舅喜欢红茶", "用户舅舅喜欢红茶"),
        ("我的表哥是医生", "用户的表哥是医生"),
        ("我推测用户喜欢红茶", "用户推测用户喜欢红茶"),
        ("我引用网页结论", "用户引用网页结论"),
        ("我喜欢红茶，但是网页称用户喜欢咖啡", "用户喜欢红茶"),
        ("我喜欢红茶，而且用户喜欢咖啡", "用户喜欢红茶"),
        ("我喜欢红茶也喜欢咖啡", "用户喜欢红茶也喜欢咖啡"),
        ("我通常不喝酒", "用户通常不喝酒"),
        (
            "我通常喝咖啡，同时我通常不喝酒",
            "用户通常喝咖啡，同时用户通常不喝酒",
        ),
        ("我希望别联系我", "用户希望别联系我"),
        ("我喜欢红茶并收藏咖啡杯", "用户喜欢红茶并收藏咖啡杯"),
        ("我希望祖母养花", "用户希望祖母养花"),
        ("我喜欢红茶也喝咖啡", "用户喜欢红茶也喝咖啡"),
        (
            "我喜欢红茶又从网页得知用户喜欢咖啡",
            "用户喜欢红茶又从网页得知用户喜欢咖啡",
        ),
        (
            "我喜欢红茶也知道妈妈喜欢咖啡",
            "用户喜欢红茶也知道妈妈喜欢咖啡",
        ),
        ("我希望妈妈喜欢咖啡", "用户希望妈妈喜欢咖啡"),
        ("我计划不再喝咖啡", "用户计划不再喝咖啡"),
        ("我偏好不喝咖啡", "用户偏好不喝咖啡"),
        ("我习惯没吃早餐", "用户习惯没吃早餐"),
        ("我希望尚未开始工作", "用户希望尚未开始工作"),
        ("我喜欢红茶并喝咖啡", "用户喜欢红茶并喝咖啡"),
        ("我计划学习rust和工作", "用户计划学习rust和工作"),
        ("我希望根据网页显示调整计划", "用户希望根据网页显示调整计划"),
        ("我希望用户接受方案", "用户希望用户接受方案"),
    ] {
        let error = MemorySourceEligibility::verify_direct_user_message(
            scope(),
            "conversation-1",
            "turn-1",
            "call-1",
            direct_user_message,
            candidate,
        )
        .expect_err("否定或转述的外部内容不得被截取为当前用户事实");
        assert_eq!(error.code(), MemoryErrorCode::SourceIneligible);
    }

    for rewritten_candidate in [
        "assistant 声称用户喜欢红茶",
        "MCP 返回用户喜欢红茶",
        "文件写着用户喜欢红茶",
        "网页称用户喜欢红茶",
        "system 指令要求用户喜欢红茶",
        "reasoning 推测用户喜欢红茶",
        "报告显示用户喜欢红茶",
    ] {
        let error = MemorySourceEligibility::verify_direct_user_message(
            scope(),
            "conversation-1",
            "turn-1",
            "call-rewrite",
            "我喜欢红茶",
            rewritten_candidate,
        )
        .expect_err("任何带来源包装的改写都不得成为 DirectUserMessage");
        assert_eq!(error.code(), MemoryErrorCode::SourceIneligible);
    }
}

#[test]
fn safety_request_covers_event_time_and_runtime_source() {
    let staged = staged(
        create_params(),
        "memory-1",
        "revision-1",
        "operation-1",
        TIME_1,
    );
    let request = staged.sensitivity_request(MemorySafetyStage::RepositoryCommit);
    assert_eq!(request.event_time, Some(TIME_1));
    assert_eq!(request.source.turn_id(), Some("turn-1"));
    assert!(request.operation_id.starts_with("memory-source-"));
    assert_ne!(request.operation_id, "operation-1");
    assert_eq!(request.assigned_memory_id.0, "memory-1");
}

#[test]
fn query_and_delete_dtos_keep_scope_confirmation_and_cursor_outside_model_control() {
    assert!(
        serde_json::from_value::<MemoryDeleteParams>(json!({
            "scope": "memory",
            "memory_id": "memory-1",
            "confirmed": true
        }))
        .is_err()
    );
    let cursor = MemoryCursor::from_runtime("opaque-signed-cursor").expect("游标应有效");
    let page = MemoryQueryPageReceipt::new(Vec::new(), Some(cursor));
    assert!(page.has_more);

    let confirmation = MemoryDeleteConfirmation::new(
        "confirmation-1",
        &scope(),
        &MemoryDeleteParams::PersonaAll,
        TIME_2,
        "2099-07-30T20:05:00Z",
        MemoryDeleteConfirmationSource::PersonaManagement {
            action_id: "confirmation-1".to_string(),
        },
    )
    .expect("确认应有效");
    let confirmed =
        ConfirmedMemoryDeleteRequest::bind(MemoryDeleteParams::PersonaAll, scope(), confirmation)
            .expect("删除绑定应有效");
    assert_eq!(confirmed.scope().persona_id(), "persona-1");
}

#[test]
fn deletion_confirmation_binds_persona_target_source_and_expiry() {
    let scope_a = MemoryPersonaScope::new("Persona-A").expect("scope 应有效");
    let scope_b = MemoryPersonaScope::new("persona-b").expect("scope 应有效");
    let target = MemoryDeleteParams::Memory {
        memory_id: MemoryId("Ｍｅｍｏｒｙ－１".to_string()),
    };
    let confirmation = MemoryDeleteConfirmation::new(
        "approval-delete-1",
        &scope_a,
        &target,
        "2026-08-01T00:00:00Z",
        "2099-08-01T00:05:00Z",
        MemoryDeleteConfirmationSource::ConversationTurn {
            conversation_id: "conversation-1".to_string(),
            turn_id: "turn-1".to_string(),
            approval_id: "approval-delete-1".to_string(),
            call_id: "call-delete-1".to_string(),
        },
    )
    .expect("专用确认应有效");
    assert_eq!(confirmation.persona_id(), "Persona-A");
    assert_eq!(confirmation.expires_at(), "2099-08-01T00:05:00Z");

    ConfirmedMemoryDeleteRequest::bind(
        MemoryDeleteParams::Memory {
            memory_id: MemoryId("memory1".to_string()),
        },
        scope_a.clone(),
        confirmation.clone(),
    )
    .expect("同 Persona 的规范化同目标应允许精确绑定");
    assert_eq!(
        ConfirmedMemoryDeleteRequest::bind(target.clone(), scope_b, confirmation.clone())
            .expect_err("跨 Persona 复用必须拒绝")
            .code(),
        MemoryErrorCode::InvalidRequest
    );
    assert_eq!(
        ConfirmedMemoryDeleteRequest::bind(
            MemoryDeleteParams::Memory {
                memory_id: MemoryId("memory-2".to_string()),
            },
            scope_a,
            confirmation,
        )
        .expect_err("更换删除目标必须拒绝")
        .code(),
        MemoryErrorCode::InvalidRequest
    );

    let expired_scope = scope();
    let expired_params = MemoryDeleteParams::PersonaAll;
    let expired = MemoryDeleteConfirmation::new(
        "expired-action",
        &expired_scope,
        &expired_params,
        "2020-01-01T00:00:00Z",
        "2020-01-01T00:01:00Z",
        MemoryDeleteConfirmationSource::PersonaManagement {
            action_id: "expired-action".to_string(),
        },
    )
    .expect("过期确认可被解析但不能使用");
    assert_eq!(
        ConfirmedMemoryDeleteRequest::bind(expired_params, expired_scope, expired)
            .expect_err("过期确认必须稳定拒绝")
            .code(),
        MemoryErrorCode::DeleteConfirmationRequired
    );
}

#[test]
fn history_query_items_expose_change_semantics_without_corrected_state() {
    let item = MemoryQueryItem {
        memory_id: MemoryId("memory-1".to_string()),
        revision_id: MemoryRevisionId("revision-1".to_string()),
        category: MemoryCategory::UserPreference,
        content: "用户过去喜欢喝茶".to_string(),
        importance: MemoryImportance::Normal,
        event_time: Some(TIME_1.to_string()),
        recorded_at: TIME_1.to_string(),
        valid_from: TIME_1.to_string(),
        valid_to: Some(TIME_2.to_string()),
        change_type: MemoryChangeType::Create,
        change_reason: "用户当时说明了偏好".to_string(),
    };
    let value = serde_json::to_value(item).expect("历史项应可序列化");
    assert_eq!(value["change_type"], "create");
    assert_eq!(value["change_reason"], "用户当时说明了偏好");
    assert!(!MemoryRevisionState::Corrected.is_model_readable());
}

#[test]
fn sensitivity_failures_are_fail_closed() {
    let staged = staged(
        create_params(),
        "memory-1",
        "revision-1",
        "operation-1",
        TIME_1,
    );
    let request = staged.sensitivity_request(MemorySafetyStage::RepositoryCommit);
    let assessment = MemorySafetyAssessment::FailClosed {
        stage: MemorySafetyStage::RepositoryCommit,
        reason: MemorySafetyFailure::DecisionMissing,
    };
    assert!(!assessment.permits_persistence());
    assert_eq!(
        assessment
            .into_repository_permit(request)
            .expect_err("缺失判定必须拒绝")
            .code(),
        MemoryErrorCode::SensitivityUnavailable
    );
}

#[test]
fn deletion_authority_validates_subjects_and_requires_fixed_length_derivation_digest() {
    let derivation_key = MemoryDerivationKey::from_digest([0x5a; 32]);
    let encoded = serde_json::to_value(&derivation_key).expect("派生摘要应可序列化");
    assert_eq!(encoded.as_str().expect("摘要应编码为字符串").len(), 64);
    assert_eq!(
        serde_json::from_value::<MemoryDerivationKey>(encoded).expect("固定长度摘要应可恢复"),
        derivation_key
    );
    assert!(serde_json::from_value::<MemoryDerivationKey>(json!("用户喜欢喝茶")).is_err());
    assert!(
        serde_json::from_value::<MemoryDerivationKey>(json!(
            "5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a"
        ))
        .is_err()
    );

    let subjects = BTreeSet::from([
        MemoryDeletionSubject::Persona {
            persona_id: "persona-1".to_string(),
        },
        MemoryDeletionSubject::SourceTurn {
            persona_id: "persona-1".to_string(),
            conversation_id: "conversation-1".to_string(),
            turn_id: "turn-1".to_string(),
        },
        MemoryDeletionSubject::Derivation {
            persona_id: "persona-1".to_string(),
            derivation_key,
        },
    ]);
    let request = MemoryDeletionAuthorityRequest::new("deletion-1", subjects.clone(), TIME_2)
        .expect("应有效");
    assert_eq!(request.subjects().len(), 3);
    assert!(MemoryDeletionCheckRequest::new(subjects).is_ok());

    let invalid = BTreeSet::from([MemoryDeletionSubject::SourceTurn {
        persona_id: "persona-1".to_string(),
        conversation_id: String::new(),
        turn_id: "turn-1".to_string(),
    }]);
    assert!(MemoryDeletionCheckRequest::new(invalid).is_err());
}

#[test]
fn authenticated_management_mutations_do_not_require_a_fake_conversation_turn() {
    let policy = RecordingManagementPolicy::default();
    let create = MemoryManagementContentMutation::bind(
        MemoryManagementContentParams::Create {
            category: MemoryCategory::UserFact,
            content: "用户的纪念日是 7 月 30 日".to_string(),
            importance: MemoryImportance::Normal,
            event_time: Some(TIME_1.to_string()),
            change_reason: "用户在 Persona 管理页手工新增".to_string(),
        },
        management_binding("management-create-1", TIME_1),
        MemoryId("memory-management-1".to_string()),
        MemoryRevisionId("revision-management-1".to_string()),
        &policy,
    )
    .expect("经鉴权管理新增应可绑定");
    assert_eq!(create.staging_policy_version(), "memory-safety/test-v1");
    assert_eq!(
        policy.stages(),
        vec![MemorySafetyStage::TurnStaging],
        "形成可提交管理 mutation 前必须执行第一门"
    );
    let created = create
        .transition(None, &policy)
        .expect("管理新增应形成内容 revision");
    assert_eq!(
        policy.stages(),
        vec![
            MemorySafetyStage::TurnStaging,
            MemorySafetyStage::RepositoryCommit
        ],
        "同一管理 mutation 必须先后通过两层敏感门"
    );
    assert!(created.new_revision.source.conversation_id().is_none());
    assert!(matches!(
        &created.new_revision.source,
        MemorySourceEvidence::PersonaManagement { .. }
    ));

    let current_before_adjustment = MemoryRecord {
        entry: created.entry,
        current_revision: created.new_revision,
    };
    let adjustment = MemoryImportanceAdjustment::bind(
        management_binding("management-importance-1", TIME_2),
        current_before_adjustment.entry.memory_id.clone(),
        current_before_adjustment
            .current_revision
            .revision_id
            .clone(),
        MemoryImportance::Normal,
        MemoryImportance::High,
    )
    .expect("重要程度调整应可绑定");
    let mut concurrent = current_before_adjustment.clone();
    concurrent.entry.current_revision_id =
        MemoryRevisionId("revision-management-concurrent".to_string());
    concurrent.current_revision.revision_id =
        MemoryRevisionId("revision-management-concurrent".to_string());
    assert_eq!(
        adjustment
            .apply_to(&concurrent)
            .expect_err("内容 revision 并发变化时不得套用旧的重要程度操作")
            .code(),
        MemoryErrorCode::RevisionConflict
    );
    let adjusted = adjustment
        .apply_to(&current_before_adjustment)
        .expect("重要程度调整应只更新 entry");
    assert_eq!(adjusted.importance, MemoryImportance::High);
    assert_eq!(
        adjusted.current_revision_id,
        current_before_adjustment.entry.current_revision_id
    );

    let current = MemoryRecord {
        entry: adjusted,
        current_revision: current_before_adjustment.current_revision,
    };
    let corrected = MemoryManagementContentMutation::bind(
        MemoryManagementContentParams::Correct {
            memory_id: current.entry.memory_id.clone(),
            expected_revision_id: current.current_revision.revision_id.clone(),
            category: MemoryCategory::UserFact,
            content: "用户的纪念日是 7 月 31 日".to_string(),
            event_time: Some(TIME_2.to_string()),
            change_reason: "用户在 Persona 管理页纠正了日期".to_string(),
        },
        management_binding("management-correct-1", TIME_2),
        current.entry.memory_id.clone(),
        MemoryRevisionId("revision-management-2".to_string()),
        &AllowPolicy,
    )
    .expect("经鉴权管理纠正应可绑定")
    .transition(Some(&current), &AllowPolicy)
    .expect("管理纠正应形成内容 revision");
    assert_eq!(corrected.entry.importance, MemoryImportance::High);
    assert_eq!(
        corrected
            .previous_revision
            .expect("被纠正 revision 应保留审计记录")
            .state,
        MemoryRevisionState::Corrected
    );

    let receipt = MemoryImportanceAdjustmentReceipt {
        operation_id: "management-importance-1".to_string(),
        memory_id: current.entry.memory_id,
        previous_importance: MemoryImportance::Normal,
        importance: MemoryImportance::High,
        durable_at: TIME_2.to_string(),
    };
    let value = serde_json::to_value(receipt).expect("重要程度收据应可序列化");
    assert!(value.get("revision_id").is_none());
    assert!(value.get("content").is_none());
}

#[test]
fn stable_error_codes_are_unique_and_matchable() {
    let codes = [
        MemoryErrorCode::InvalidRequest,
        MemoryErrorCode::InvalidStateTransition,
        MemoryErrorCode::MemoryNotFound,
        MemoryErrorCode::RevisionConflict,
        MemoryErrorCode::PersonaScopeMismatch,
        MemoryErrorCode::SourceIneligible,
        MemoryErrorCode::SensitiveContentRejected,
        MemoryErrorCode::SensitivityUnavailable,
        MemoryErrorCode::InvalidCursor,
        MemoryErrorCode::CursorExpired,
        MemoryErrorCode::QueryRejected,
        MemoryErrorCode::QueryBudgetExceeded,
        MemoryErrorCode::DeleteConfirmationRequired,
        MemoryErrorCode::DeletionAuthorityUnavailable,
        MemoryErrorCode::DeletionIncomplete,
        MemoryErrorCode::RepositoryUnavailable,
    ];
    let stable = codes
        .into_iter()
        .map(MemoryErrorCode::as_str)
        .collect::<HashSet<_>>();
    assert_eq!(stable.len(), codes.len());
    assert_eq!(
        MemoryError::new(MemoryErrorCode::RevisionConflict).stable_code(),
        "memory_revision_conflict"
    );
    assert_eq!(
        serde_json::to_string(&MemoryErrorCode::RevisionConflict).expect("错误码应可序列化"),
        "\"memory_revision_conflict\""
    );
}

#[test]
fn public_ports_are_object_safe_and_minimally_implementable() {
    fn accepts_ports(
        _repository: &dyn MemoryRepository,
        _retriever: &dyn MemoryRetriever,
        _authority: &dyn MemoryDeletionAuthority,
        _sensitivity: &dyn MemorySensitivityPolicy,
    ) {
    }

    let probe = ContractProbe;
    accepts_ports(&probe, &probe, &probe, &probe);
}

fn scope() -> MemoryPersonaScope {
    MemoryPersonaScope::new("persona-1").expect("scope 应有效")
}

fn binding(operation_id: &str, at: &str) -> MemoryRuntimeBinding {
    let eligibility = MemorySourceEligibility::verify_direct_user_message(
        scope(),
        "conversation-1",
        "turn-1",
        operation_id,
        "我喜欢喝茶",
        "用户喜欢喝茶",
    )
    .expect("当前直接用户消息应能验证候选事实");
    MemoryRuntimeBinding::new(eligibility, at, at, at).expect("运行时绑定应有效")
}

fn management_binding(operation_id: &str, at: &str) -> MemoryManagementBinding {
    let authorization = MemoryManagementAuthorization::from_runtime(
        scope(),
        format!("authorized-{operation_id}"),
        at,
    )
    .expect("管理操作应已通过 runtime 鉴权");
    MemoryManagementBinding::bind(authorization, operation_id, at, at, at)
        .expect("管理来源与时间应可绑定")
}

fn staged(
    params: MemoryMutateParams,
    memory_id: &str,
    revision_id: &str,
    operation_id: &str,
    at: &str,
) -> MemoryStagedMutation {
    MemoryStagedMutation::stage(
        params,
        binding(operation_id, at),
        MemoryId(memory_id.to_string()),
        MemoryRevisionId(revision_id.to_string()),
        &AllowPolicy,
    )
    .expect("变更请求应通过第一层门禁")
}

fn create_params() -> MemoryMutateParams {
    MemoryMutateParams::Create {
        category: MemoryCategory::UserPreference,
        content: "用户喜欢喝茶".to_string(),
        importance: MemoryImportance::Normal,
        event_time: Some(TIME_1.to_string()),
        change_reason: "用户说明了自己的偏好".to_string(),
    }
}

fn update_params() -> MemoryMutateParams {
    MemoryMutateParams::Update {
        memory_id: MemoryId("memory-1".to_string()),
        expected_revision_id: MemoryRevisionId("revision-1".to_string()),
        category: MemoryCategory::UserPreference,
        content: "用户现在更喜欢喝咖啡".to_string(),
        importance: MemoryImportance::High,
        event_time: Some(TIME_2.to_string()),
        change_reason: "用户更新了自己的偏好".to_string(),
    }
}

fn correct_params() -> MemoryMutateParams {
    MemoryMutateParams::Correct {
        memory_id: MemoryId("memory-1".to_string()),
        expected_revision_id: MemoryRevisionId("revision-2".to_string()),
        category: MemoryCategory::UserFact,
        content: "用户从未喜欢喝咖啡".to_string(),
        importance: MemoryImportance::Normal,
        event_time: Some(TIME_2.to_string()),
        change_reason: "用户纠正了旧事实".to_string(),
    }
}

struct ContractProbe;

struct AllowPolicy;

impl MemorySensitivityPolicy for AllowPolicy {
    fn assess(&self, request: MemorySensitivityRequest<'_>) -> MemorySafetyAssessment {
        MemorySafetyAssessment::Allowed {
            stage: request.stage,
            policy_version: "memory-safety/test-v1".to_string(),
        }
    }
}

#[derive(Default)]
struct RecordingManagementPolicy {
    stages: Mutex<Vec<MemorySafetyStage>>,
}

impl RecordingManagementPolicy {
    fn stages(&self) -> Vec<MemorySafetyStage> {
        self.stages
            .lock()
            .expect("测试敏感门记录锁不应中毒")
            .clone()
    }
}

impl MemorySensitivityPolicy for RecordingManagementPolicy {
    fn assess(&self, request: MemorySensitivityRequest<'_>) -> MemorySafetyAssessment {
        assert_eq!(request.operation_id, "management-create-1");
        assert_eq!(request.operation, MemoryChangeType::Create);
        assert_eq!(request.assigned_memory_id.0, "memory-management-1");
        assert_eq!(request.assigned_revision_id.0, "revision-management-1");
        assert_eq!(request.content, "用户的纪念日是 7 月 30 日");
        assert_eq!(request.event_time, Some(TIME_1));
        assert!(matches!(
            request.source,
            MemorySourceEvidence::PersonaManagement { .. }
        ));
        self.stages
            .lock()
            .expect("测试敏感门记录锁不应中毒")
            .push(request.stage);
        MemorySafetyAssessment::Allowed {
            stage: request.stage,
            policy_version: "memory-safety/test-v1".to_string(),
        }
    }
}

impl MemorySensitivityPolicy for ContractProbe {
    fn assess(&self, request: MemorySensitivityRequest<'_>) -> MemorySafetyAssessment {
        MemorySafetyAssessment::FailClosed {
            stage: request.stage,
            reason: MemorySafetyFailure::PolicyUnavailable,
        }
    }
}

impl MemoryRetriever for ContractProbe {
    fn retrieve(
        &self,
        _request: &MemoryRetrievalRequest,
    ) -> Result<MemoryQueryPageReceipt, MemoryError> {
        Ok(MemoryQueryPageReceipt::new(Vec::new(), None))
    }
}

impl MemoryDeletionAuthority for ContractProbe {
    fn record(
        &self,
        _request: &MemoryDeletionAuthorityRequest,
    ) -> Result<MemoryDeletionAuthorityReceipt, MemoryError> {
        Err(MemoryError::new(
            MemoryErrorCode::DeletionAuthorityUnavailable,
        ))
    }

    fn check(
        &self,
        _request: &MemoryDeletionCheckRequest,
    ) -> Result<MemoryDeletionDecision, MemoryError> {
        Err(MemoryError::new(
            MemoryErrorCode::DeletionAuthorityUnavailable,
        ))
    }
}

impl MemoryRepository for ContractProbe {
    fn current(
        &self,
        _scope: &MemoryPersonaScope,
        _memory_id: &MemoryId,
    ) -> Result<Option<MemoryRecord>, MemoryError> {
        Ok(None)
    }

    fn apply_committed_batch(
        &self,
        _envelope: &MemoryCommitEnvelope,
        _sensitivity: &dyn MemorySensitivityPolicy,
    ) -> Result<MemoryBatchCommitReceipt, MemoryError> {
        Err(MemoryError::new(MemoryErrorCode::RepositoryUnavailable))
    }

    fn apply_management_content_mutation(
        &self,
        _mutation: &MemoryManagementContentMutation,
        _sensitivity: &dyn MemorySensitivityPolicy,
    ) -> Result<MemoryMutationReceipt, MemoryError> {
        Err(MemoryError::new(MemoryErrorCode::RepositoryUnavailable))
    }

    fn adjust_importance(
        &self,
        _adjustment: &MemoryImportanceAdjustment,
    ) -> Result<MemoryImportanceAdjustmentReceipt, MemoryError> {
        Err(MemoryError::new(MemoryErrorCode::RepositoryUnavailable))
    }

    fn delete_confirmed(
        &self,
        _request: &ConfirmedMemoryDeleteRequest,
        _authority: &dyn MemoryDeletionAuthority,
    ) -> Result<MemoryDeleteReceipt, MemoryError> {
        Err(MemoryError::new(MemoryErrorCode::RepositoryUnavailable))
    }
}

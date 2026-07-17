async fn tool_send_user_message(
    state: &Arc<AppState>,
    tx: Option<&RuntimeSseSender>,
    turn: &TurnContext,
    call: &ToolCall,
    cancel_token: &RuntimeTurnCancel,
) -> ToolResult {
    let message = call
        .arguments
        .get("message")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if message.is_empty() {
        return tool_failed(
            "send_user_message 缺少有效的 message 参数。",
            "missing_message",
        );
    }
    let requires_reply = call.name.as_str() != "brief"
        && call
            .arguments
            .get("requires_reply")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
    if let Some(tx) = tx {
        let payload = serde_json::to_value(&RuntimeEvent::Status {
            phase: "brief".to_string(),
            message: message.clone(),
            detail: Some("阶段进展简报".to_string()),
            state: "progress".to_string(),
        })
        .unwrap_or_else(|_| serde_json::json!({ "type": "status", "phase": "brief", "message": message, "state": "progress" }));
        let _ = emit_json_event(tx, payload).await;
    }

    if requires_reply {
        let request = AskUserQuestionRequest {
            questions: vec![AskUserQuestionItem {
                question: message.clone(),
                header: "阶段反馈".to_string(),
                options: vec![
                    AskUserQuestionOption {
                        label: "确认继续".to_string(),
                        description: "知悉当前阶段结论，同意继续执行后续流程。".to_string(),
                    },
                    AskUserQuestionOption {
                        label: "调整方向".to_string(),
                        description: "对当前进度有调整要求或补充说明。".to_string(),
                    },
                ],
                multi_select: false,
            }],
        };
        let decision = match wait_for_user_question_answer(
            state,
            tx,
            turn,
            call,
            &request,
            cancel_token,
        )
        .await
        {
            Ok(d) => d,
            Err(_) => return tool_failed("中途确认被中断。", "brief_interrupted"),
        };
        if !decision.answered {
            return ToolResult {
                status: ToolResultStatus::Failed,
                content: "用户取消了中途确认，可基于现有信息调整方案。".to_string(),
                structured: None,
            };
        }
        let reply = exit_plan_answer_label(&decision).unwrap_or_else(|| "确认继续".to_string());
        return ToolResult {
            status: ToolResultStatus::Success,
            content: format!("阶段简报已发出，用户确认意见：{reply}"),
            structured: Some(serde_json::json!({
                "message": message,
                "requires_reply": true,
                "user_reply": reply,
            })),
        };
    }

    ToolResult {
        status: ToolResultStatus::Success,
        content: format!("已成功向用户输出阶段反馈简报：{message}"),
        structured: Some(serde_json::json!({
            "message": message,
            "requires_reply": false,
        })),
    }
}

async fn tool_skill(state: &Arc<AppState>, turn: &TurnContext, call: &ToolCall) -> ToolResult {
    let workspace = match workspace_root() {
        Ok(w) => w,
        Err(e) => return tool_failed(format!("无法定位工作区目录：{e}"), "workspace_error"),
    };
    let skill_context = {
        let mut config = state.user_config.lock().await;
        if let Err(error) = config.refresh_from_disk() {
            return tool_failed(
                format!("刷新 Skill config.toml 失败：{error}"),
                "skill_config_read_failed",
            );
        }
        (
            state.runtime_service.data_dir().to_path_buf(),
            config.skill_preferences().clone(),
        )
    };
    tool_skill_from_workspace_with_user(
        turn,
        call,
        &workspace,
        Some((&skill_context.0, &skill_context.1)),
    )
    .await
}

#[cfg(test)]
async fn tool_skill_from_workspace(
    turn: &TurnContext,
    call: &ToolCall,
    workspace: &StdPath,
) -> ToolResult {
    tool_skill_from_workspace_with_user(turn, call, workspace, None).await
}

async fn tool_skill_from_workspace_with_user(
    turn: &TurnContext,
    call: &ToolCall,
    workspace: &StdPath,
    user_skill_context: Option<(&StdPath, &muse_core::domain::skill::SkillPreferences)>,
) -> ToolResult {
    let skill_name = call
        .arguments
        .get("skill_name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if skill_name.is_empty() {
        return tool_failed("skill 缺少有效的 skill_name 参数。", "missing_skill_name");
    }
    if !valid_skill_name(&skill_name) {
        return tool_failed(
            "skill_name 必须为 1-64 个小写字母、数字或单连字符组合，格式如 `git-release`。",
            "invalid_skill_name",
        );
    }
    match turn.skill_policy.mode {
        muse_core::domain::persona::ResourcePolicyMode::Inherit => {}
        muse_core::domain::persona::ResourcePolicyMode::Disabled => {
            return tool_failed("当前角色已禁用 Skill。", "skill_policy_disabled");
        }
        muse_core::domain::persona::ResourcePolicyMode::AllowList
            if !turn
                .skill_policy
                .allowed_skills
                .iter()
                .any(|allowed| allowed == &skill_name) =>
        {
            return tool_failed(
                format!("Skill `{skill_name}` 不在当前角色白名单中。"),
                "skill_policy_denied",
            );
        }
        muse_core::domain::persona::ResourcePolicyMode::AllowList => {}
    }

    let user_store = user_skill_context.map(|(data_dir, _)| {
        muse_core::domain::skill::SkillStore::from_data_dir(data_dir)
    });
    let frozen_catalog_entry = turn
        .runtime_policy
        .skill_catalog
        .iter()
        .find(|entry| entry.name == skill_name);
    let frozen_catalog_required = turn.runtime_policy.schema_version >= 2;
    let user_skill = user_skill_context.and_then(|(_, preferences)| {
        user_store
            .as_ref()
            .map(|store| store.get(&skill_name, preferences))
    });
    match user_skill {
        Some(Ok(skill)) if !skill.enabled => {
            return tool_failed(
                format!("Skill `{skill_name}` 已禁用，不能加载。"),
                "skill_disabled",
            );
        }
        Some(Ok(_)) if frozen_catalog_required && frozen_catalog_entry.is_none() => {
            return tool_failed(
                format!("Skill `{skill_name}` 不在当前 Turn 冻结目录中。"),
                "skill_catalog_denied",
            );
        }
        Some(Ok(ref skill))
            if frozen_catalog_entry
                .is_some_and(|entry| entry.revision != skill.revision) =>
        {
            return tool_failed(
                format!("Skill `{skill_name}` 已在当前 Turn 开始后变更，请发起新请求后重试。"),
                "skill_revision_changed",
            );
        }
        Some(Ok(skill)) => {
            let content_hash = content_hash_hex(skill.content.as_bytes());
            return ToolResult {
                status: ToolResultStatus::Success,
                content: format!(
                    "已成功载入用户 Skill `{skill_name}` 指引：\n\n{}",
                    skill.content
                ),
                structured: Some(serde_json::json!({
                    "skill_name": skill.name,
                    "description": skill.description,
                    "revision": skill.revision,
                    "content_hash": content_hash,
                    "source": "user_store",
                    "activated": true,
                })),
            };
        }
        Some(Err(error))
            if error.kind == muse_core::domain::skill::SkillStoreErrorKind::NotFound
                && frozen_catalog_entry.is_some() =>
        {
            return tool_failed(
                format!("Skill `{skill_name}` 已在当前 Turn 开始后移除，请发起新请求后重试。"),
                "skill_revision_changed",
            );
        }
        Some(Err(error))
            if error.kind != muse_core::domain::skill::SkillStoreErrorKind::NotFound =>
        {
            return tool_failed(error.to_string(), "skill_read_failed");
        }
        Some(Err(_)) | None => {}
    }
    let candidate_roots = [
        // 旧工作区技能目录仅保留兼容读取，不再作为新技能的推荐写入位置。
        workspace.join(".agent-vp-data").join("skills"),
        workspace.join(".agents").join("skills"),
        workspace.join("skills"),
        workspace.join(".agent").join("skills"),
    ];

    for root in &candidate_roots {
        let content = match read_compatibility_skill(root, &skill_name).await {
            Ok(Some(content)) => content,
            Ok(None) => continue,
            Err(result) => return result,
        };
        return ToolResult {
            status: ToolResultStatus::Success,
            content: format!("已成功载入本地技能 `{skill_name}` 指引：\n\n{content}"),
            structured: Some(serde_json::json!({
                "skill_name": skill_name,
                "source": "compatibility_read_only",
                "content_hash": content_hash_hex(content.as_bytes()),
                "activated": true,
            })),
        };
    }

    let mut available_skills = user_skill_context
        .and_then(|(_, preferences)| {
            user_store.as_ref().map(|store| store.list(preferences))
        })
        .and_then(Result::ok)
        .map(|skills| {
            skills
                .into_iter()
                .filter(|skill| skill.enabled)
                .map(|skill| skill.name)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    for root in &candidate_roots {
        let Ok(root_metadata) = tokio::fs::symlink_metadata(root).await else {
            continue;
        };
        if root_metadata.file_type().is_symlink() || !root_metadata.is_dir() {
            continue;
        }
        if let Ok(mut entries) = tokio::fs::read_dir(root).await {
            while let Ok(Some(entry)) = entries.next_entry().await {
                if let Ok(file_type) = entry.file_type().await
                    && file_type.is_dir()
                    && !file_type.is_symlink()
                {
                    let name = entry.file_name().to_string_lossy().to_string();
                    if valid_skill_name(&name)
                        && matches!(read_compatibility_skill(root, &name).await, Ok(Some(_)))
                        && !available_skills.contains(&name)
                    {
                        available_skills.push(name);
                    }
                }
            }
        }
    }

    if skill_name == "list" || skill_name == "help" {
        return ToolResult {
            status: ToolResultStatus::Success,
            content: format!("当前工作区已发现的技能列表：{:?}", available_skills),
            structured: Some(serde_json::json!({ "skills": available_skills })),
        };
    }

    ToolResult {
        status: ToolResultStatus::Failed,
        content: format!(
            "未能找到名为 `{skill_name}` 的技能说明。当前可用技能库有：{:?}。如需新建技能，请在 Muse 数据目录的 skills/<技能名>/ 目录下创建 SKILL.md。",
            available_skills,
        ),
        structured: Some(
            serde_json::json!({ "skill_name": skill_name, "available_skills": available_skills }),
        ),
    }
}

/// 安全读取旧工作区 Skill：任一层符号链接或 canonical 根逃逸都直接拒绝。
async fn read_compatibility_skill(
    root: &StdPath,
    skill_name: &str,
) -> Result<Option<String>, ToolResult> {
    let root_metadata = match tokio::fs::symlink_metadata(root).await {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(tool_failed(
                format!("检查兼容 Skill 根目录失败：{error}"),
                "skill_read_failed",
            ));
        }
    };
    if root_metadata.file_type().is_symlink() || !root_metadata.is_dir() {
        return Err(tool_failed(
            "兼容 Skill 根目录必须是真实普通目录，不能是符号链接。",
            "skill_path_boundary",
        ));
    }
    let skill_dir = root.join(skill_name);
    let skill_metadata = match tokio::fs::symlink_metadata(&skill_dir).await {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(tool_failed(
                format!("检查兼容 Skill 目录失败：{error}"),
                "skill_read_failed",
            ));
        }
    };
    if skill_metadata.file_type().is_symlink() || !skill_metadata.is_dir() {
        return Err(tool_failed(
            format!("Skill `{skill_name}` 的兼容目录不是安全的普通目录。"),
            "skill_path_boundary",
        ));
    }
    let path = skill_dir.join("SKILL.md");
    let metadata = match tokio::fs::symlink_metadata(&path).await {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(tool_failed(
                format!("检查 Skill `{skill_name}` 文档失败：{error}"),
                "skill_read_failed",
            ));
        }
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(tool_failed(
            format!("Skill `{skill_name}` 的 SKILL.md 必须是普通文件，不能是符号链接。"),
            "skill_path_boundary",
        ));
    }
    if metadata.len() > MAX_SKILL_DOCUMENT_BYTES {
        return Err(tool_failed(
            format!(
                "技能 `{skill_name}` 的 SKILL.md 超过 {} KB，已拒绝载入。",
                MAX_SKILL_DOCUMENT_BYTES / 1024
            ),
            "skill_document_too_large",
        ));
    }
    let canonical_root = tokio::fs::canonicalize(root).await.map_err(|error| {
        tool_failed(
            format!("解析兼容 Skill 根目录失败：{error}"),
            "skill_path_boundary",
        )
    })?;
    let canonical_path = tokio::fs::canonicalize(&path).await.map_err(|error| {
        tool_failed(
            format!("解析 Skill `{skill_name}` 文档失败：{error}"),
            "skill_path_boundary",
        )
    })?;
    if !canonical_path.starts_with(&canonical_root) {
        return Err(tool_failed(
            format!("Skill `{skill_name}` 的文档超出受信根目录，已拒绝载入。"),
            "skill_path_boundary",
        ));
    }
    let content = tokio::fs::read_to_string(&canonical_path)
        .await
        .map_err(|error| {
            tool_failed(
                format!("读取技能 `{skill_name}` 失败：{error}"),
                "skill_read_failed",
            )
        })?;
    let post_metadata = tokio::fs::symlink_metadata(&canonical_path)
        .await
        .map_err(|error| {
            tool_failed(
                format!("复核技能 `{skill_name}` 文档失败：{error}"),
                "skill_read_failed",
            )
        })?;
    if !post_metadata.is_file()
        || post_metadata.file_type().is_symlink()
        || post_metadata.len() != metadata.len()
        || content.len() as u64 != post_metadata.len()
    {
        return Err(tool_failed(
            format!("Skill `{skill_name}` 文档在读取期间发生变化，已拒绝使用。"),
            "skill_path_changed",
        ));
    }
    Ok(Some(content))
}

fn valid_skill_name(skill_name: &str) -> bool {
    muse_core::domain::skill::validate_skill_name(skill_name).is_ok()
}

async fn tool_agent(state: &Arc<AppState>, turn: &TurnContext, call: &ToolCall) -> ToolResult {
    let request = match parse_agent_task_request(&call.arguments) {
        Ok(request) => request,
        Err(result) => return result,
    };
    let task = request.task.trim().to_string();
    let context = request
        .context
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string);
    let expected_output = request
        .expected_output
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string);
    let priority = request
        .priority
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string);
    let task_id = next_runtime_id("agent-task");
    let todo = RuntimeTodoItem {
        id: task_id.clone(),
        content: task.clone(),
        status: "in_progress".to_string(),
        priority: priority.clone(),
    };
    let todos = state.runtime_service.push_runtime_todo(todo).await;
    let updated_at = chrono::Utc::now().to_rfc3339();

    report_transcript_failure(
        append_transcript_record(
            state,
            "agent_task",
            serde_json::json!({
                "conversation_id": turn.conversation_id,
                "turn_id": turn.turn_id,
                "tool": call.name,
                "task_id": task_id,
                "task": task,
                "context": context,
                "expected_output": expected_output,
                "priority": priority,
                "todos": todos,
                "updated_at": updated_at,
            }),
        )
        .await,
    );
    report_transcript_failure(
        append_task_state_record(
            state,
            turn,
            "agent_task_created",
            format!("已登记轻量子任务 `{task_id}`：{task}"),
            Some(&call.name),
        )
        .await,
    );

    ToolResult {
        status: ToolResultStatus::Success,
        content: format!(
            "已登记轻量子任务 `{task_id}`：{task}\n当前任务清单：\n{}",
            todo_items_text(&todos)
        ),
        structured: Some(serde_json::json!({
            "task_id": task_id,
            "task": task,
            "context": context,
            "expected_output": expected_output,
            "priority": priority,
            "todos": todos,
            "updated_at": updated_at,
        })),
    }
}

async fn tool_task_stop(state: &Arc<AppState>, turn: &TurnContext, call: &ToolCall) -> ToolResult {
    let reason = call
        .arguments
        .get("reason")
        .and_then(|v| v.as_str())
        .unwrap_or("主动请求停止任务")
        .trim();
    let task_id = call
        .arguments
        .get("task_id")
        .and_then(|v| v.as_str())
        .unwrap_or(&turn.turn_id);

    let mut stop_signal_sent = false;
    let mut stopped_current_turn = false;
    if let Ok(snapshot) = state.runtime_service.snapshot()
        && let Some(active_turn_id) = snapshot.turn_id
        && (active_turn_id == task_id || turn.conversation_id == task_id || task_id == turn.turn_id)
    {
        let _ = state.runtime_service.cancel_turn(&active_turn_id);
        stop_signal_sent = true;
        stopped_current_turn = active_turn_id == turn.turn_id || task_id == turn.turn_id;
    }

    state
        .runtime_service
        .cancel_pending_interactions_for_turn(&turn.turn_id, &format!("任务被终止：{reason}"))
        .await;

    ToolResult {
        status: ToolResultStatus::Success,
        content: format!("已发出停止信号，任务 `{task_id}` 终止执行。终止原因：{reason}"),
        structured: Some(serde_json::json!({
            "task_id": task_id,
            "reason": if stopped_current_turn { "turn_cancelled" } else { "task_stop_requested" },
            "user_reason": reason,
            "stopped": stop_signal_sent,
            "stopped_current_turn": stopped_current_turn,
        })),
    }
}

async fn tool_ask_user_question(
    state: &Arc<AppState>,
    tx: Option<&RuntimeSseSender>,
    turn: &TurnContext,
    call: &ToolCall,
    cancel_token: &RuntimeTurnCancel,
) -> ToolResult {
    let request = match parse_ask_user_question_request(&call.arguments) {
        Ok(request) => request,
        Err(result) => return result,
    };
    let decision =
        match wait_for_user_question_answer(state, tx, turn, call, &request, cancel_token).await {
            Ok(decision) => decision,
            Err(_) => {
                return tool_failed(
                    "用户问题等待被中断，已取消本次交互式工具调用。",
                    "question_interrupted",
                );
            }
        };
    if !decision.answered {
        let reason = decision.reason.unwrap_or_else(|| "cancelled".to_string());
        let content = match reason.as_str() {
            "timeout" => "用户问题等待超时，模型可以基于现有信息继续或重新提问。",
            "client_disconnected" => "前端连接断开，用户问题未能完成。",
            "turn_cancelled" => TURN_CANCELLED_MESSAGE,
            "no_event_channel" => "当前通道不支持交互式用户问题。",
            _ => "用户取消回答问题，模型可以基于现有信息继续或换一种方式提问。",
        };
        return ToolResult {
            status: ToolResultStatus::Failed,
            content: content.to_string(),
            structured: Some(serde_json::json!({
                "reason": reason,
                "questions": request.questions,
            })),
        };
    }

    let answers = decision.answers.unwrap_or_else(|| serde_json::json!({}));
    let annotations = decision.annotations;
    let answers_text = ask_user_question_answers_text(&answers, annotations.as_ref());
    ToolResult {
        status: ToolResultStatus::Success,
        content: format!("用户已经回答你的问题：{answers_text}。请基于这些答案继续。"),
        structured: Some(serde_json::json!({
            "questions": request.questions,
            "answers": answers,
            "annotations": annotations,
        })),
    }
}

async fn tool_todo_write(state: &Arc<AppState>, turn: &TurnContext, call: &ToolCall) -> ToolResult {
    let request = match parse_todo_write_request(&call.arguments) {
        Ok(request) => request,
        Err(result) => return result,
    };
    let summary = request
        .summary
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string);
    let todos = todo_items_from_request(request);
    state
        .runtime_service
        .replace_runtime_todos(todos.clone())
        .await;
    let updated_at = chrono::Utc::now().to_rfc3339();
    report_transcript_failure(
        append_transcript_record(
            state,
            "todo_state",
            serde_json::json!({
                "conversation_id": turn.conversation_id,
                "turn_id": turn.turn_id,
                "tool": call.name,
                "todos": todos,
                "summary": summary,
                "updated_at": updated_at,
            }),
        )
        .await,
    );
    report_transcript_failure(
        append_task_state_record(
            state,
            turn,
            "todo_updated",
            summary
                .clone()
                .unwrap_or_else(|| format!("任务清单已更新：{} 项。", todos.len())),
            Some(&call.name),
        )
        .await,
    );

    ToolResult {
        status: ToolResultStatus::Success,
        content: format!(
            "任务清单已更新：{} 项。\n{}",
            todos.len(),
            todo_items_text(&todos)
        ),
        structured: Some(serde_json::json!({
            "todos": todos,
            "summary": summary,
            "updated_at": updated_at,
        })),
    }
}

async fn tool_enter_plan_mode(
    state: &Arc<AppState>,
    turn: &TurnContext,
    call: &ToolCall,
) -> ToolResult {
    let mode_state = match set_runtime_mode_state(state, RuntimeModeState::focus_plan()) {
        Ok(mode_state) => mode_state,
        Err(err) => return tool_failed(err, "runtime_mode_state_poisoned"),
    };
    let reason = tool_arg_string(&call.arguments, "reason");
    report_transcript_failure(
        append_transcript_record(
            state,
            "runtime_mode_changed",
            serde_json::json!({
                "conversation_id": turn.conversation_id,
                "turn_id": turn.turn_id,
                "tool": call.name,
                "runtime_mode_state": mode_state_json(mode_state),
                "reason": reason,
                "updated_at": chrono::Utc::now().to_rfc3339(),
            }),
        )
        .await,
    );
    report_transcript_failure(
        append_task_state_record(
            state,
            turn,
            "plan_mode_entered",
            reason
                .clone()
                .unwrap_or_else(|| "已进入专注计划预设。".to_string()),
            Some(&call.name),
        )
        .await,
    );

    ToolResult {
        status: ToolResultStatus::Success,
        content: reason
            .map(|reason| format!("已进入专注计划预设：{reason}"))
            .unwrap_or_else(|| "已进入专注计划预设。".to_string()),
        structured: Some(serde_json::json!({
            "runtime_mode_state": mode_state_json(mode_state),
        })),
    }
}

async fn tool_exit_plan_mode(
    state: &Arc<AppState>,
    tx: Option<&RuntimeSseSender>,
    turn: &TurnContext,
    call: &ToolCall,
    cancel_token: &RuntimeTurnCancel,
) -> ToolResult {
    let plan = match parse_exit_plan_mode_request(&call.arguments) {
        Ok(plan) => plan,
        Err(result) => return result,
    };
    let plan_payload = serde_json::json!({
        "plan_summary": plan.plan_summary.trim(),
        "steps": plan.steps,
        "risks": plan.risks,
        "next_action": plan.next_action.as_deref().map(str::trim).filter(|value| !value.is_empty()),
    });
    state
        .runtime_service
        .set_active_plan(Some(serde_json::json!({
            "status": "pending_confirmation",
            "plan": plan_payload.clone(),
            "updated_at": chrono::Utc::now().to_rfc3339(),
        })))
        .await;
    report_transcript_failure(
        append_transcript_record(
            state,
            "plan_submitted",
            serde_json::json!({
                "conversation_id": turn.conversation_id,
                "turn_id": turn.turn_id,
                "tool": call.name,
                "plan": plan_payload,
                "updated_at": chrono::Utc::now().to_rfc3339(),
            }),
        )
        .await,
    );

    let request = exit_plan_confirmation_request(&plan);
    let decision =
        match wait_for_user_question_answer(state, tx, turn, call, &request, cancel_token).await {
            Ok(decision) => decision,
            Err(_) => {
                return tool_failed(
                    "计划确认等待被中断，仍保持专注计划预设。",
                    "plan_confirmation_interrupted",
                );
            }
        };
    if !decision.answered {
        let reason = decision.reason.unwrap_or_else(|| "cancelled".to_string());
        let mode_state = match set_runtime_mode_state(state, RuntimeModeState::focus_plan()) {
            Ok(mode_state) => mode_state,
            Err(err) => return tool_failed(err, "runtime_mode_state_poisoned"),
        };
        state
            .runtime_service
            .set_active_plan(Some(serde_json::json!({
                "status": "not_confirmed",
                "reason": reason.clone(),
                "plan": {
                    "plan_summary": plan.plan_summary.trim(),
                    "steps": plan.steps,
                    "risks": plan.risks,
                    "next_action": plan.next_action.as_deref().map(str::trim).filter(|value| !value.is_empty()),
                },
                "updated_at": chrono::Utc::now().to_rfc3339(),
            })))
            .await;
        report_transcript_failure(
            append_transcript_record(
                state,
                "plan_resolved",
                serde_json::json!({
                    "conversation_id": turn.conversation_id,
                    "turn_id": turn.turn_id,
                    "tool": call.name,
                    "confirmed": false,
                    "reason": reason,
                    "runtime_mode_state": mode_state_json(mode_state),
                }),
            )
            .await,
        );
        return ToolResult {
            status: ToolResultStatus::Failed,
            content: "计划未获得确认，已保持在专注计划预设。".to_string(),
            structured: Some(serde_json::json!({
                "confirmed": false,
                "reason": reason,
                "runtime_mode_state": mode_state_json(mode_state),
            })),
        };
    }

    let answer_label = exit_plan_answer_label(&decision).unwrap_or_default();
    let confirmed = answer_label.contains("确认计划");
    let cancelled = answer_label.contains("取消计划");
    let mode_state = if confirmed {
        match set_runtime_mode_state(state, RuntimeModeState::focus_build()) {
            Ok(mode_state) => mode_state,
            Err(err) => return tool_failed(err, "runtime_mode_state_poisoned"),
        }
    } else {
        match set_runtime_mode_state(state, RuntimeModeState::focus_plan()) {
            Ok(mode_state) => mode_state,
            Err(err) => return tool_failed(err, "runtime_mode_state_poisoned"),
        }
    };
    let status = if confirmed {
        "confirmed"
    } else if cancelled {
        "cancelled"
    } else {
        "needs_revision"
    };
    state
        .runtime_service
        .set_active_plan(Some(serde_json::json!({
            "status": status,
            "plan": {
                "plan_summary": plan.plan_summary.trim(),
                "steps": plan.steps,
                "risks": plan.risks,
                "next_action": plan.next_action.as_deref().map(str::trim).filter(|value| !value.is_empty()),
            },
            "answer": answer_label,
            "updated_at": chrono::Utc::now().to_rfc3339(),
        })))
        .await;
    report_transcript_failure(
        append_transcript_record(
            state,
            "plan_resolved",
            serde_json::json!({
                "conversation_id": turn.conversation_id,
                "turn_id": turn.turn_id,
                "tool": call.name,
                "confirmed": confirmed,
                "status": status,
                "answer": answer_label,
                "runtime_mode_state": mode_state_json(mode_state),
            }),
        )
        .await,
    );
    report_transcript_failure(
        append_task_state_record(
            state,
            turn,
            if confirmed {
                "plan_confirmed"
            } else {
                "plan_needs_revision"
            },
            if confirmed {
                "计划已确认，已回到专注工作预设。"
            } else if cancelled {
                "用户取消计划，已保持在专注计划预设。"
            } else {
                "用户要求继续调整计划，已保持在专注计划预设。"
            },
            Some(&call.name),
        )
        .await,
    );

    ToolResult {
        status: ToolResultStatus::Success,
        content: if confirmed {
            "用户已确认计划，已回到专注工作预设。请按计划继续执行。".to_string()
        } else if cancelled {
            "用户取消了计划，已保持在专注计划预设。请停止执行该计划，等待用户下一步指示。"
                .to_string()
        } else {
            "用户要求继续调整计划，已保持在专注计划预设。请根据反馈修改计划后再提交确认。"
                .to_string()
        },
        structured: Some(serde_json::json!({
            "confirmed": confirmed,
            "status": status,
            "answer": answer_label,
            "runtime_mode_state": mode_state_json(mode_state),
        })),
    }
}

async fn tool_tts_speak(state: &Arc<AppState>, turn: &TurnContext, call: &ToolCall) -> ToolResult {
    for forbidden in ["voice_id", "voice_name", "speaker"] {
        if call.arguments.get(forbidden).is_some() {
            return ToolResult {
                status: ToolResultStatus::Failed,
                content: format!("tts_speak 不接受 `{forbidden}` 参数，语音永远使用当前启用音色。"),
                structured: Some(serde_json::json!({ "reason": "forbidden_voice_argument" })),
            };
        }
    }
    let Some(text) = tool_arg_string(&call.arguments, "text") else {
        return ToolResult {
            status: ToolResultStatus::Failed,
            content: "tts_speak 缺少 text 参数。".to_string(),
            structured: Some(serde_json::json!({ "reason": "missing_text" })),
        };
    };
    if !turn.voice_enabled {
        return ToolResult {
            status: ToolResultStatus::Failed,
            content: "当前会话未开启语音播报，已跳过朗读。".to_string(),
            structured: Some(serde_json::json!({ "reason": "voice_disabled" })),
        };
    }
    if turn.active_voice_id.is_none() {
        return ToolResult {
            status: ToolResultStatus::Failed,
            content: "当前没有可用 TTS 音色或 TTS 运行时未就绪。".to_string(),
            structured: Some(serde_json::json!({ "reason": "tts_unavailable" })),
        };
    }
    report_transcript_failure(
        append_transcript_record(
            state,
            "speech",
            serde_json::json!({
                "conversation_id": turn.conversation_id,
                "turn_id": turn.turn_id,
                "call_id": call.call_id,
                "voice_id": turn.active_voice_id,
                "text": text.clone(),
            }),
        )
        .await,
    );
    ToolResult {
        status: ToolResultStatus::Success,
        content: "已请求前端使用当前启用音色朗读文本。".to_string(),
        structured: Some(serde_json::json!({
            "speech_text": text,
            "voice_id": turn.active_voice_id,
        })),
    }
}

fn tool_voice_current(turn: &TurnContext) -> ToolResult {
    let voice_id = turn.active_voice_id.as_deref();
    let tts_available = voice_id.is_some();
    let content = if let Some(voice_id) = voice_id {
        format!("当前 TTS 音色 ID：{voice_id}，TTS 可用：是。")
    } else {
        "当前回合未冻结可用的 TTS 音色，TTS 可用：否。".to_string()
    };
    ToolResult {
        status: ToolResultStatus::Success,
        content,
        structured: Some(serde_json::json!({
            "tts_available": tts_available,
            "voice_id": voice_id,
            "frozen_for_turn": true,
        })),
    }
}

//! Persona HTTP 适配实现。

use super::*;

const PERSONA_DELETION_RECOVERY_SCHEMA_VERSION: &str = "muse-persona-deletion-recovery/v1";
const PERSONA_DELETION_RECOVERY_FILE: &str = "persona-deletion-recovery.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersonaDeletionRecoveryFile {
    schema_version: String,
    pending: Option<PendingPersonaDeletion>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PendingPersonaDeletion {
    persona_id: String,
    previous_visual_pack: Option<VisualPack>,
    /// Persona 定义提交后才允许执行的记忆清理操作；缺失表示旧版恢复记录。
    #[serde(default)]
    memory_delete_operation_id: Option<String>,
}

/// 查询角色列表。
pub(crate) async fn handle_personas(
    State(state): State<Arc<AppState>>,
) -> Json<PersonaListResponse> {
    let personas = state.personas.lock().await;
    let summaries = personas.list();
    let active_persona_id = personas.active_persona_id().map(str::to_string);
    // 角色与展示包始终按 personas -> visual_packs 的顺序取锁，
    // 一次快照完成全部关联，避免列表项逐个查询或拼接不同时点的事实。
    let visual_packs = state.visual_packs.lock().await;
    let visual_pack_by_id: HashMap<&str, &VisualPack> = visual_packs
        .visual_packs()
        .iter()
        .map(|pack| (pack.id.as_str(), pack))
        .collect();
    let personas = summaries
        .into_iter()
        .map(|persona| {
            let visual_pack = visual_pack_by_id
                .get(persona.default_visual_pack_id.as_str())
                .copied();
            PersonaLibraryItem::from_summary(persona, visual_pack)
        })
        .collect();
    Json(PersonaListResponse {
        personas,
        active_persona_id,
    })
}

/// 查询当前激活角色。
pub(crate) async fn handle_active_persona(
    State(state): State<Arc<AppState>>,
) -> Result<Json<ActivePersonaResponse>, (StatusCode, Json<ErrorResponse>)> {
    let _transition = state.persona_runtime_transition_gate.lock().await;
    for _ in 0..RUNTIME_FACT_SNAPSHOT_MAX_ATTEMPTS {
        let snapshot_before = state
            .runtime_service
            .snapshot()
            .map_err(|error| internal_error(error.to_string()))?;
        let (active_persona, active_persona_id, visual_pack) =
            active_persona_visual_snapshot(&state).await;
        let snapshot_after = state
            .runtime_service
            .snapshot()
            .map_err(|error| internal_error(error.to_string()))?;
        if snapshot_before != snapshot_after {
            tokio::task::yield_now().await;
            continue;
        }
        return Ok(Json(ActivePersonaResponse {
            active_persona,
            active_persona_id,
            visual_pack,
            state_revision: snapshot_after.state_revision,
        }));
    }
    Err((
        StatusCode::CONFLICT,
        Json(ErrorResponse {
            error: "runtime_snapshot_unstable：运行时事实正在连续变化，请重试。".to_string(),
        }),
    ))
}

/// 在统一锁顺序下读取当前角色及其视觉包，避免跨角色拼接事实响应。
pub(super) async fn active_persona_visual_snapshot(
    state: &Arc<AppState>,
) -> (Option<Persona>, Option<String>, Option<VisualPack>) {
    let personas = state.personas.lock().await;
    let active_persona = personas.active_persona().cloned();
    let active_persona_id = active_persona.as_ref().map(|persona| persona.id.clone());
    let visual_pack = match active_persona.as_ref() {
        Some(persona) => {
            let visual_packs = state.visual_packs.lock().await;
            resolve_persona_visual_pack_from_store(&visual_packs, persona)
        }
        None => None,
    };
    (active_persona, active_persona_id, visual_pack)
}

/// 查询指定角色详情。
pub(crate) async fn handle_get_persona(
    Path(id): Path<String>,
    State(state): State<Arc<AppState>>,
) -> Result<Json<PersonaDetailResponse>, (StatusCode, Json<ErrorResponse>)> {
    let _transition = state.persona_runtime_transition_gate.lock().await;
    let personas = state.personas.lock().await;
    let Some(persona) = personas.get(&id).cloned() else {
        return Err((
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: format!("角色 `{id}` 不存在"),
            }),
        ));
    };
    drop(personas);
    let runtime_state = state
        .runtime_service
        .session_repository()
        .await
        .map_err(|error| internal_error(error.to_string()))?
        .persona_state(&persona.id)
        .await
        .map_err(|error| internal_error(error.to_string()))?
        .map(|projection| projection.effective_at(chrono::Utc::now()));

    Ok(Json(PersonaDetailResponse {
        visual_pack: resolve_persona_visual_pack(&state, &persona).await,
        runtime_state,
        persona,
    }))
}

/// 导出指定角色卡片。
pub(crate) async fn handle_export_persona_card(
    Path(id): Path<String>,
    Query(query): Query<PersonaCardExportQuery>,
    State(state): State<Arc<AppState>>,
) -> Result<Json<PersonaCard>, (StatusCode, Json<ErrorResponse>)> {
    let personas = state.personas.lock().await;
    let Some(persona) = personas.get(&id).cloned() else {
        return Err((
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: format!("角色 `{id}` 不存在"),
            }),
        ));
    };
    drop(personas);

    let visual_pack = {
        let visual_packs = state.visual_packs.lock().await;
        visual_packs.get(&persona.default_visual_pack_id).cloned()
    };
    Ok(Json(PersonaCard::build(
        &persona,
        visual_pack.as_ref(),
        query.level,
    )))
}

/// 导入角色卡片。
pub(crate) async fn handle_import_persona_card(
    State(state): State<Arc<AppState>>,
    Json(req): Json<PersonaCardImportRequest>,
) -> Result<(StatusCode, Json<PersonaCardImportResponse>), (StatusCode, Json<ErrorResponse>)> {
    let _transition = state.persona_runtime_transition_gate.lock().await;
    let idle_lease = acquire_runtime_idle_lease(&state, "import_persona")?;
    let personas_snapshot = {
        let personas = state.personas.lock().await;
        personas.clone()
    };
    let visual_packs_snapshot = {
        let visual_packs = state.visual_packs.lock().await;
        visual_packs.clone()
    };

    let import_plan = req
        .card
        .prepare_import(
            &personas_snapshot,
            &visual_packs_snapshot,
            req.conflict_strategy.clone(),
        )
        .map_err(persona_card_error_response)?;

    if let Some(visual_pack) = import_plan.visual_pack.clone() {
        let mut visual_packs = state.visual_packs.lock().await;
        visual_packs
            .upsert(visual_pack)
            .map_err(visual_pack_store_error_response)?;
        visual_packs
            .save()
            .map_err(visual_pack_store_error_response)?;
    }

    let (persona, runtime_reset) = {
        let mut personas = state.personas.lock().await;
        let active_persona_id_before = personas.active_persona_id().map(str::to_string);

        if personas.get(&import_plan.persona.id).is_some() {
            personas
                .update(import_plan.persona.clone())
                .map_err(persona_store_error_response)?;
        } else {
            personas
                .create(import_plan.persona.clone())
                .map_err(persona_store_error_response)?;
        }

        if req.activate_after_import {
            personas
                .set_active(&import_plan.persona.id)
                .map_err(persona_store_error_response)?;
        }

        personas.save().map_err(persona_store_error_response)?;

        (
            import_plan.persona.clone(),
            req.activate_after_import
                || active_persona_id_before.as_deref() == Some(import_plan.persona.id.as_str()),
        )
    };

    if runtime_reset {
        reset_conversation_for_active_persona(&state).await;
    }

    finish_runtime_idle_lease(idle_lease)?;
    let mutation = persona_mutation_response(&state, persona, runtime_reset).await;
    Ok((
        StatusCode::CREATED,
        Json(PersonaCardImportResponse {
            affected_persona: mutation.affected_persona,
            active_persona: mutation.active_persona,
            active_persona_id: mutation.active_persona_id,
            visual_pack: mutation.visual_pack,
            notices: import_plan.notices,
            runtime_reset,
            conversation_id: mutation.conversation_id,
            state_revision: mutation.state_revision,
        }),
    ))
}

/// 创建新角色。
pub(crate) async fn handle_create_persona(
    State(state): State<Arc<AppState>>,
    Json(req): Json<PersonaUpsertRequest>,
) -> Result<(StatusCode, Json<PersonaMutationResponse>), (StatusCode, Json<ErrorResponse>)> {
    let _transition = state.persona_runtime_transition_gate.lock().await;
    let idle_lease = acquire_runtime_idle_lease(&state, "create_persona")?;
    let activate_after_create = req.activate_after_create;
    let mut persona = req.persona;
    {
        let mut personas = state.personas.lock().await;
        if personas.get(&persona.id).is_some() {
            return Err(persona_store_error_response(
                PersonaStoreError::DuplicateId(persona.id.clone()),
            ));
        }

        let mut visual_packs = state.visual_packs.lock().await;
        let mut persona_candidate = personas.clone();
        let mut visual_pack_candidate = visual_packs.clone();
        let patched_visual_pack = build_persona_visual_pack_from_patch(
            &visual_pack_candidate,
            &mut persona,
            req.visual_pack_patch,
        );

        persona_candidate
            .create(persona.clone())
            .map_err(persona_store_error_response)?;
        if activate_after_create {
            persona_candidate
                .set_active(&persona.id)
                .map_err(persona_store_error_response)?;
        }
        if let Some(visual_pack) = patched_visual_pack.clone() {
            visual_pack_candidate
                .upsert(visual_pack)
                .map_err(visual_pack_store_error_response)?;
            visual_pack_candidate
                .save()
                .map_err(visual_pack_store_error_response)?;
        }
        persona_candidate
            .save()
            .map_err(persona_store_error_response)?;

        *visual_packs = visual_pack_candidate;
        *personas = persona_candidate;
    }

    if activate_after_create {
        reset_conversation_for_active_persona(&state).await;
    }

    finish_runtime_idle_lease(idle_lease)?;
    Ok((
        StatusCode::CREATED,
        Json(persona_mutation_response(&state, persona, activate_after_create).await),
    ))
}

/// 更新已有角色。
pub(crate) async fn handle_update_persona(
    Path(id): Path<String>,
    State(state): State<Arc<AppState>>,
    Json(req): Json<PersonaUpsertRequest>,
) -> Result<Json<PersonaMutationResponse>, (StatusCode, Json<ErrorResponse>)> {
    if id != req.persona.id {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: "路径中的角色 id 与请求体不一致".to_string(),
            }),
        ));
    }
    if req.activate_after_create {
        return Err(bad_request("activate_after_create 只允许用于创建角色。"));
    }

    let _transition = state.persona_runtime_transition_gate.lock().await;
    let idle_lease = acquire_runtime_idle_lease(&state, "update_persona")?;

    let mut persona = req.persona;
    let (should_reset_runtime, stale_asset_candidates) = {
        let mut personas = state.personas.lock().await;
        if personas.get(&id).is_none() {
            return Err(persona_store_error_response(
                PersonaStoreError::PersonaNotFound(id.clone()),
            ));
        }
        let should_reset_runtime = personas.active_persona_id() == Some(id.as_str());

        let mut visual_packs = state.visual_packs.lock().await;
        let mut persona_candidate = personas.clone();
        let mut visual_pack_candidate = visual_packs.clone();
        let previous_paths = visual_pack_candidate
            .get(&format!("visual-{}", persona.id))
            .map(visual_pack_paths)
            .unwrap_or_default();
        let patched_visual_pack = build_persona_visual_pack_from_patch(
            &visual_pack_candidate,
            &mut persona,
            req.visual_pack_patch,
        );

        persona_candidate
            .update(persona.clone())
            .map_err(persona_store_error_response)?;
        if let Some(visual_pack) = patched_visual_pack.clone() {
            visual_pack_candidate
                .upsert(visual_pack)
                .map_err(visual_pack_store_error_response)?;
            visual_pack_candidate
                .save()
                .map_err(visual_pack_store_error_response)?;
        }
        persona_candidate
            .save()
            .map_err(persona_store_error_response)?;

        *visual_packs = visual_pack_candidate;
        *personas = persona_candidate;
        (should_reset_runtime, previous_paths)
    };

    cleanup_unreferenced_uploaded_assets(&state, stale_asset_candidates).await;

    if should_reset_runtime {
        reset_conversation_for_active_persona(&state).await;
    }

    finish_runtime_idle_lease(idle_lease)?;
    Ok(Json(
        persona_mutation_response(&state, persona, should_reset_runtime).await,
    ))
}

/// 删除指定角色。
pub(crate) async fn handle_delete_persona(
    Path(id): Path<String>,
    State(state): State<Arc<AppState>>,
) -> Result<Json<PersonaMutationResponse>, (StatusCode, Json<ErrorResponse>)> {
    let _transition = state.persona_runtime_transition_gate.lock().await;
    let idle_lease = acquire_runtime_idle_lease(&state, "delete_persona")?;
    ensure_persona_deletion_recovery_slot_available(state.runtime_service.data_dir())
        .map_err(internal_error)?;
    let repository = state
        .runtime_service
        .session_repository()
        .await
        .map_err(|error| internal_error(error.to_string()))?;
    let memory_services = memory_services();
    let memory_delete_operation_id = memory_services
        .as_ref()
        .map(|_| next_runtime_id("persona-memory-delete"));
    let (deleted_active, deleted_persona, stale_asset_candidates) = {
        let mut personas = state.personas.lock().await;
        let deleted_active = personas.active_persona_id() == Some(id.as_str());
        let deleted_persona = personas.get(&id).cloned().ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                Json(ErrorResponse {
                    error: format!("角色 `{id}` 不存在"),
                }),
            )
        })?;
        let mut visual_packs = state.visual_packs.lock().await;
        let previous_personas = personas.clone();
        let previous_visual_packs = visual_packs.clone();
        let mut persona_candidate = previous_personas.clone();
        let mut visual_pack_candidate = previous_visual_packs.clone();

        if !persona_candidate.delete(&id) {
            return Err((
                StatusCode::NOT_FOUND,
                Json(ErrorResponse {
                    error: format!("角色 `{id}` 不存在"),
                }),
            ));
        }

        let (stale_asset_candidates, previous_generated_visual_pack) =
            remove_unreferenced_generated_visual_pack(
                &id,
                &persona_candidate,
                &mut visual_pack_candidate,
            );
        let visual_pack_changed = previous_generated_visual_pack.is_some();
        persist_persona_deletion_recovery(
            state.runtime_service.data_dir(),
            Some(PendingPersonaDeletion {
                persona_id: id.clone(),
                previous_visual_pack: previous_generated_visual_pack,
                memory_delete_operation_id: memory_delete_operation_id.clone(),
            }),
        )
        .map_err(internal_error)?;

        // Persona 定义是删除提交点：进程若在后续清理间退出，启动同步会按
        // “Persona 已缺失”继续收敛 VisualPack 与 SQLite 派生状态。
        if let Err(error) = persona_candidate.save() {
            clear_persona_deletion_recovery_best_effort(
                state.runtime_service.data_dir(),
                &id,
                "角色定义尚未提交",
            );
            return Err(persona_store_error_response(error));
        }
        if visual_pack_changed && let Err(error) = visual_pack_candidate.save() {
            if let Err(rollback_error) = previous_personas.save() {
                // 原子写回失败会保留删除提交后的 personas.json；内存也必须立即
                // 跟随这个持久事实，禁止在等待下次启动收敛期间继续使用幽灵角色。
                *personas = persona_candidate;
                return Err(internal_error(format!(
                    "删除角色时保存展示包失败：{error}；角色定义回滚也失败：{rollback_error}。\
                     已保留恢复记录，下次启动将按 personas.json 事实继续收敛"
                )));
            }
            clear_persona_deletion_recovery_best_effort(
                state.runtime_service.data_dir(),
                &id,
                "展示包保存失败且角色定义已恢复",
            );
            return Err(visual_pack_store_error_response(error));
        }

        if let Err(error) = repository.delete_persona_runtime_state(&id).await {
            let visual_rollback_error = visual_pack_changed
                .then(|| previous_visual_packs.save().err())
                .flatten();
            let persona_rollback_error = previous_personas.save().err();
            if visual_rollback_error.is_some() || persona_rollback_error.is_some() {
                // 每个原子写回失败时，磁盘仍保持删除阶段已发布的 candidate。
                // 分别对齐内存事实，避免 API 返回失败后继续暴露磁盘中已删除的角色，
                // 或继续引用磁盘中已经移除的展示包。
                if visual_rollback_error.is_some() {
                    *visual_packs = visual_pack_candidate;
                }
                if persona_rollback_error.is_some() {
                    *personas = persona_candidate;
                }
                return Err(internal_error(format!(
                    "删除角色时清理 SQLite 失败：{error}；文件回滚未全部完成\
                     （展示包：{}；角色定义：{}）。已保留恢复记录，下次启动将按 personas.json 事实继续收敛",
                    rollback_status(visual_rollback_error.as_ref()),
                    rollback_status(persona_rollback_error.as_ref()),
                )));
            }
            clear_persona_deletion_recovery_best_effort(
                state.runtime_service.data_dir(),
                &id,
                "SQLite 清理失败且角色文件已恢复",
            );
            return Err(internal_error(format!(
                "删除角色时清理 SQLite 失败，角色定义与展示包已恢复：{error}"
            )));
        }

        *visual_packs = visual_pack_candidate;
        *personas = persona_candidate;
        (deleted_active, deleted_persona, stale_asset_candidates)
    };

    // 只有 Persona 文件、展示包与运行时 SQLite 已完成同一删除决议后，才执行
    // 不可回滚的记忆清理。失败时角色定义保持已删除，恢复记录携带同一 operation
    // 身份，后续重试只能返回首次收据，不能重新选取并误删后来创建的记忆。
    if let (Some(services), Some(operation_id)) = (
        memory_services.as_ref(),
        memory_delete_operation_id.as_deref(),
    ) && let Err(error) = delete_all_persona_memories(services, &id, operation_id)
    {
        cleanup_unreferenced_uploaded_assets(&state, stale_asset_candidates).await;
        if deleted_active {
            reset_conversation_for_active_persona(&state).await;
        }
        finish_runtime_idle_lease(idle_lease)?;
        return Err(memory_error_response(error));
    }
    clear_persona_deletion_recovery_best_effort(
        state.runtime_service.data_dir(),
        &id,
        "Persona 删除已完成",
    );

    cleanup_unreferenced_uploaded_assets(&state, stale_asset_candidates).await;

    if deleted_active {
        reset_conversation_for_active_persona(&state).await;
    }

    finish_runtime_idle_lease(idle_lease)?;
    Ok(Json(
        persona_mutation_response(&state, deleted_persona, deleted_active).await,
    ))
}

fn remove_unreferenced_generated_visual_pack(
    persona_id: &str,
    personas: &PersonaStore,
    visual_packs: &mut VisualPackStore,
) -> (Vec<String>, Option<VisualPack>) {
    let visual_pack_id = format!("visual-{persona_id}");
    if personas
        .personas()
        .iter()
        .any(|persona| persona.default_visual_pack_id == visual_pack_id)
    {
        return (Vec::new(), None);
    }
    let previous_visual_pack = visual_packs.get(&visual_pack_id).cloned();
    let paths = previous_visual_pack
        .as_ref()
        .map(visual_pack_paths)
        .unwrap_or_default();
    if previous_visual_pack.is_some() {
        visual_packs.delete(&visual_pack_id);
    }
    (paths, previous_visual_pack)
}

fn persona_deletion_recovery_path(data_dir: &StdPath) -> PathBuf {
    data_dir
        .join("runtime")
        .join(PERSONA_DELETION_RECOVERY_FILE)
}

fn persist_persona_deletion_recovery(
    data_dir: &StdPath,
    pending: Option<PendingPersonaDeletion>,
) -> Result<(), String> {
    let path = persona_deletion_recovery_path(data_dir);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let content = serde_json::to_vec_pretty(&PersonaDeletionRecoveryFile {
        schema_version: PERSONA_DELETION_RECOVERY_SCHEMA_VERSION.to_string(),
        pending,
    })
    .map_err(|error| error.to_string())?;
    muse_core::app::storage::atomic_write_synced(&path, &content).map_err(|error| error.to_string())
}

#[cfg(test)]
pub(crate) fn persist_persona_deletion_recovery_for_test(
    data_dir: &StdPath,
    persona_id: &str,
    previous_visual_pack: Option<VisualPack>,
) -> Result<(), String> {
    persist_persona_deletion_recovery(
        data_dir,
        Some(PendingPersonaDeletion {
            persona_id: persona_id.to_string(),
            previous_visual_pack,
            memory_delete_operation_id: None,
        }),
    )
}

fn load_persona_deletion_recovery(
    data_dir: &StdPath,
) -> Result<Option<PendingPersonaDeletion>, String> {
    let path = persona_deletion_recovery_path(data_dir);
    if !path.exists() {
        return Ok(None);
    }
    let content = std::fs::read(&path).map_err(|error| error.to_string())?;
    let recovery: PersonaDeletionRecoveryFile =
        serde_json::from_slice(&content).map_err(|error| error.to_string())?;
    if recovery.schema_version != PERSONA_DELETION_RECOVERY_SCHEMA_VERSION {
        return Err(format!(
            "不支持的 Persona 删除恢复记录版本 `{}`",
            recovery.schema_version
        ));
    }
    Ok(recovery.pending)
}

fn ensure_persona_deletion_recovery_slot_available(data_dir: &StdPath) -> Result<(), String> {
    match load_persona_deletion_recovery(data_dir) {
        Ok(None) => Ok(()),
        Ok(Some(pending)) => Err(format!(
            "上次 Persona `{}` 删除恢复尚未完成，请重启应用完成收敛后再删除其他角色",
            pending.persona_id
        )),
        Err(error) => Err(format!(
            "无法核对 Persona 删除恢复记录，已禁止开始新的删除：{error}"
        )),
    }
}

fn clear_persona_deletion_recovery_best_effort(data_dir: &StdPath, persona_id: &str, reason: &str) {
    if let Err(error) = persist_persona_deletion_recovery(data_dir, None) {
        tracing::warn!(
            target: "muse::persona_delete",
            %persona_id,
            %reason,
            %error,
            "Persona 删除恢复记录暂未清除，将在下次启动幂等核对"
        );
    }
}

fn rollback_status(error: Option<&impl std::fmt::Display>) -> String {
    error.map_or_else(
        || "成功或无需恢复".to_string(),
        |error| format!("失败：{error}"),
    )
}

/// 启动时只处理删除恢复记录明确指向的展示包，避免按命名前缀误删未引用的用户数据。
/// 展示包属于可选资产；恢复失败会保留记录供下次启动重试，但不得阻断应用启动。
pub(super) async fn reconcile_pending_persona_deletion(state: &Arc<AppState>) {
    let pending = match load_persona_deletion_recovery(state.runtime_service.data_dir()) {
        Ok(Some(pending)) => pending,
        Ok(None) => return,
        Err(error) => {
            tracing::warn!(
                target: "muse::persona_delete",
                %error,
                "读取 Persona 删除恢复记录失败，跳过可选展示包收敛"
            );
            return;
        }
    };
    let persona_exists = state
        .personas
        .lock()
        .await
        .get(&pending.persona_id)
        .is_some();
    if !persona_exists && let Some(operation_id) = pending.memory_delete_operation_id.as_deref() {
        let Some(services) = memory_services() else {
            tracing::warn!(
                target: "muse::persona_delete",
                persona_id = %pending.persona_id,
                "Persona 已提交删除，但记忆服务尚未接线；保留恢复记录等待接线后重试"
            );
            return;
        };
        if let Err(error) =
            delete_all_persona_memories(&services, &pending.persona_id, operation_id)
        {
            tracing::warn!(
                target: "muse::persona_delete",
                persona_id = %pending.persona_id,
                %error,
                "启动收敛 Persona 记忆失败，保留恢复记录等待下次重试"
            );
            return;
        }
    }

    let reconciliation = async {
        let personas = state.personas.lock().await;
        let mut visual_packs = state.visual_packs.lock().await;
        let mut candidate = visual_packs.clone();
        let mut stale_asset_candidates = Vec::new();
        let mut changed = false;
        if let Some(previous_visual_pack) = pending.previous_visual_pack.as_ref() {
            if persona_exists {
                if candidate.get(&previous_visual_pack.id).is_none() {
                    candidate
                        .upsert(previous_visual_pack.clone())
                        .map_err(|error| error.to_string())?;
                    changed = true;
                }
            } else {
                let still_referenced = personas
                    .personas()
                    .iter()
                    .any(|persona| persona.default_visual_pack_id == previous_visual_pack.id);
                if !still_referenced && candidate.delete(&previous_visual_pack.id) {
                    stale_asset_candidates.extend(visual_pack_paths(previous_visual_pack));
                    changed = true;
                }
            }
        }
        if changed {
            candidate.save().map_err(|error| error.to_string())?;
            *visual_packs = candidate;
        }
        Ok::<_, String>(stale_asset_candidates)
    }
    .await;
    let stale_asset_candidates = match reconciliation {
        Ok(paths) => paths,
        Err(error) => {
            tracing::warn!(
                target: "muse::persona_delete",
                persona_id = %pending.persona_id,
                %error,
                "启动收敛 Persona 可选展示包失败，保留恢复记录等待下次重试"
            );
            return;
        }
    };
    clear_persona_deletion_recovery_best_effort(
        state.runtime_service.data_dir(),
        &pending.persona_id,
        "启动已完成展示包收敛",
    );
    cleanup_unreferenced_uploaded_assets(state, stale_asset_candidates).await;
}

pub(super) async fn persona_mutation_response(
    state: &Arc<AppState>,
    affected_persona: Persona,
    runtime_reset: bool,
) -> PersonaMutationResponse {
    state.runtime_service.touch();
    // 活动角色与视觉包必须来自同一锁定快照，禁止把受影响角色的视觉包
    // 与另一个当前活动角色拼接成无法成立的响应事实。
    let (active_persona, active_persona_id, visual_pack) =
        active_persona_visual_snapshot(state).await;
    let state_revision = state
        .runtime_service
        .snapshot()
        .map(|snapshot| snapshot.state_revision)
        .unwrap_or_default();
    PersonaMutationResponse {
        affected_persona,
        active_persona,
        active_persona_id,
        visual_pack,
        runtime_reset,
        conversation_id: active_conversation_id(state),
        active_conversation_id: active_conversation_id(state),
        session_restored: false,
        state_revision,
    }
}

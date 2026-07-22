//! Persona HTTP 适配实现。

use super::*;

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
    let (deleted_active, deleted_persona) = {
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
        if !personas.delete(&id) {
            return Err((
                StatusCode::NOT_FOUND,
                Json(ErrorResponse {
                    error: format!("角色 `{id}` 不存在"),
                }),
            ));
        }

        personas.save().map_err(persona_store_error_response)?;
        (deleted_active, deleted_persona)
    };

    let stale_asset_candidates = {
        let mut visual_packs = state.visual_packs.lock().await;
        let mut candidate = visual_packs.clone();
        let generated_visual_pack_id = format!("visual-{id}");
        let paths = candidate
            .get(&generated_visual_pack_id)
            .map(visual_pack_paths)
            .unwrap_or_default();
        if candidate.delete(&generated_visual_pack_id) {
            match candidate.save() {
                Ok(()) => *visual_packs = candidate,
                Err(error) => tracing::warn!(
                    target: "muse::persona_assets",
                    persona_id = %id,
                    error = %error,
                    "角色已删除，但展示包清理失败；保留展示包等待后续诊断"
                ),
            }
        }
        paths
    };
    cleanup_unreferenced_uploaded_assets(&state, stale_asset_candidates).await;
    state
        .runtime_service
        .session_repository()
        .await
        .map_err(|error| internal_error(error.to_string()))?
        .clear_workspace_state(&id)
        .map_err(|error| internal_error(error.to_string()))?;

    if deleted_active {
        reset_conversation_for_active_persona(&state).await;
    }

    finish_runtime_idle_lease(idle_lease)?;
    Ok(Json(
        persona_mutation_response(&state, deleted_persona, deleted_active).await,
    ))
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

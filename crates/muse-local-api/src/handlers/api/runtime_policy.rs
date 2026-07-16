/// 在回合入口冻结角色、模型、工具、Skill 与 MCP 的公共运行事实。
async fn build_turn_context(
    state: &Arc<AppState>,
    conversation_id: String,
    turn_id: String,
    active_persona: Option<&Persona>,
    voice_enabled: bool,
    mut system_prompt: String,
    tool_definitions: &[ToolDef],
) -> TurnContext {
    let mode_state = current_runtime_mode_state(state);
    let (chat, active_voice_id) = {
        let config = state.model_config.lock().await;
        let chat = config.chat().clone();
        let voice_id = config.tts().voice_id.trim();
        let active_voice_id = (!voice_id.is_empty()).then(|| voice_id.to_string());
        (chat, active_voice_id)
    };
    let created_at = chrono::Utc::now().to_rfc3339();
    let local_time = chrono::Local::now().format("%Y-%m-%d %H:%M:%S %:z");
    system_prompt.push_str(&format!(
        "\n\n【当前时间】本回合开始于 {local_time}。这是运行时提供的可信时间上下文，无需调用时间工具。"
    ));

    let (mcp_revision, mcp_catalog_hash, skill_preferences) = {
        let mut store = state.user_config.lock().await;
        if let Err(error) = store.refresh_from_disk() {
            tracing::warn!(error = %error, "刷新 MCP config.toml 运行时快照失败");
        }
        let snapshot = store.mcp_runtime_snapshot();
        (
            store.mcp_profiles().revision(),
            snapshot.config_hash().to_string(),
            store.skill_preferences().clone(),
        )
    };
    let data_dir = state.runtime_service.data_dir().to_path_buf();
    let skill_summaries = muse_core::domain::skill::SkillStore::from_data_dir(data_dir)
        .list(&skill_preferences)
        .unwrap_or_default()
        .into_iter()
        .filter(|skill| skill.enabled)
        .collect::<Vec<_>>();
    let skill_catalog_hash = format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&skill_summaries).unwrap_or_default())
    );
    let skill_revision = skill_summaries
        .iter()
        .map(|skill| skill.revision.as_str())
        .collect::<Vec<_>>()
        .join(":");
    let mut tool_ids = tool_definitions
        .iter()
        .map(|tool| tool.name.clone())
        .collect::<Vec<_>>();
    tool_ids.sort();
    tool_ids.dedup();

    let tool_policy = active_persona
        .map(|persona| persona.tool_policy.clone())
        .unwrap_or_default();
    let skill_policy = active_persona
        .map(|persona| persona.skill_policy.clone())
        .unwrap_or_default();
    let mcp_policy = active_persona
        .map(|persona| persona.mcp_policy.clone())
        .unwrap_or_default();
    let runtime_policy = muse_core::domain::turn::RuntimePolicySnapshot {
        schema_version: 1,
        policy_version: "persona-resource-policy/v1".to_string(),
        persona_version: active_persona.map(|persona| persona.version.clone()),
        provider: chat.provider.clone(),
        model: chat.model.clone(),
        tool_preset: mode_state.tool_preset().as_str().to_string(),
        tool_ids,
        tool_policy: tool_policy.clone(),
        skill_policy: skill_policy.clone(),
        mcp_policy: mcp_policy.clone(),
        skill_revision,
        skill_catalog_hash,
        mcp_revision,
        mcp_catalog_hash,
    };
    TurnContext {
        conversation_id,
        turn_id,
        persona_id: active_persona.map(|persona| persona.id.clone()),
        system_prompt,
        model_provider: chat.provider,
        model_name: chat.model,
        tool_policy,
        skill_policy,
        mcp_policy,
        runtime_mode: mode_state.mode.as_str().to_string(),
        focus_phase: mode_state.focus_phase.as_str().to_string(),
        tool_preset: mode_state.tool_preset().as_str().to_string(),
        voice_enabled,
        active_voice_id,
        created_at,
        runtime_policy,
        tool_definitions: tool_definitions.to_vec(),
    }
}

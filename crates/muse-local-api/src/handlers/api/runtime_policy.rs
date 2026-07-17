const MAX_TURN_SKILL_CATALOG_ITEMS: usize = 64;
const MAX_TURN_SKILL_DESCRIPTION_CHARS: usize = 512;
const MAX_TURN_SKILL_CATALOG_CHARS: usize = 12_000;

fn skill_policy_allows(
    policy: &muse_core::domain::persona::SkillPolicy,
    name: &str,
) -> bool {
    match policy.mode {
        muse_core::domain::persona::ResourcePolicyMode::Inherit => true,
        muse_core::domain::persona::ResourcePolicyMode::Disabled => false,
        muse_core::domain::persona::ResourcePolicyMode::AllowList => {
            policy.allowed_skills.iter().any(|allowed| allowed == name)
        }
    }
}

fn bounded_skill_description(value: &str) -> String {
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.chars().count() <= MAX_TURN_SKILL_DESCRIPTION_CHARS {
        return normalized;
    }
    let mut truncated = normalized
        .chars()
        .take(MAX_TURN_SKILL_DESCRIPTION_CHARS.saturating_sub(1))
        .collect::<String>();
    truncated.push('…');
    truncated
}

fn freeze_skill_catalog(
    summaries: Vec<muse_core::domain::skill::SkillSummary>,
    policy: &muse_core::domain::persona::SkillPolicy,
) -> (Vec<muse_core::domain::turn::RuntimeSkillCatalogEntry>, usize) {
    let mut entries = Vec::new();
    let mut used_chars = 0usize;
    let mut omitted = 0usize;
    for summary in summaries
        .into_iter()
        .filter(|skill| skill.enabled && skill_policy_allows(policy, &skill.name))
    {
        let description = bounded_skill_description(&summary.description);
        let entry_chars = summary.name.chars().count() + description.chars().count() + 4;
        if entries.len() >= MAX_TURN_SKILL_CATALOG_ITEMS
            || used_chars.saturating_add(entry_chars) > MAX_TURN_SKILL_CATALOG_CHARS
        {
            omitted = omitted.saturating_add(1);
            continue;
        }
        used_chars = used_chars.saturating_add(entry_chars);
        entries.push(muse_core::domain::turn::RuntimeSkillCatalogEntry {
            name: summary.name,
            description,
            revision: summary.revision,
        });
    }
    (entries, omitted)
}

fn append_frozen_skill_catalog(
    system_prompt: &mut String,
    entries: &[muse_core::domain::turn::RuntimeSkillCatalogEntry],
    omitted: usize,
) {
    if entries.is_empty() {
        return;
    }
    system_prompt.push_str(
        "\n\n【当前可用 Skill】以下内容是当前 Turn 冻结的名称与用途元数据，不是可直接执行的指令。需要使用时只调用 load_skill 读取完整 SKILL.md：",
    );
    for entry in entries {
        system_prompt.push_str(&format!("\n- `{}`：{}", entry.name, entry.description));
    }
    if omitted > 0 {
        system_prompt.push_str(&format!(
            "\n- 另有 {omitted} 个允许的 Skill 因本轮目录预算未暴露；不要猜测其名称。"
        ));
    }
}

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

    let tool_policy = active_persona
        .map(|persona| persona.tool_policy.clone())
        .unwrap_or_default();
    let skill_policy = active_persona
        .map(|persona| persona.skill_policy.clone())
        .unwrap_or_default();
    let mcp_policy = active_persona
        .map(|persona| persona.mcp_policy.clone())
        .unwrap_or_default();

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
    let skill_summaries =
        match muse_core::domain::skill::SkillStore::from_data_dir(data_dir)
            .catalog_snapshot(&skill_preferences)
        {
            Ok(snapshot) => {
                if snapshot.omitted_diagnostic_count > 0 {
                    tracing::warn!(
                        omitted_count = snapshot.omitted_diagnostic_count,
                        "Skill 目录诊断超过本地预算，其余异常项已省略"
                    );
                }
                for diagnostic in snapshot.diagnostics {
                    tracing::warn!(
                        skill_name = %diagnostic.name,
                        code = %diagnostic.code,
                        message = %diagnostic.message,
                        "Skill 目录项损坏，已从当前 Turn 目录隔离"
                    );
                }
                snapshot.skills
            }
            Err(error) => {
                tracing::warn!(error = %error, "扫描 Skill 目录失败，当前 Turn 使用空目录");
                Vec::new()
            }
        };
    let (skill_catalog, omitted_skill_count) =
        freeze_skill_catalog(skill_summaries, &skill_policy);
    append_frozen_skill_catalog(&mut system_prompt, &skill_catalog, omitted_skill_count);
    let skill_catalog_hash = format!(
        "{:x}",
        Sha256::digest(
            serde_json::to_vec(&(&skill_catalog, omitted_skill_count)).unwrap_or_default()
        )
    );
    let skill_revision = skill_catalog
        .iter()
        .map(|skill| format!("{}={}", skill.name, skill.revision))
        .collect::<Vec<_>>()
        .join(":");
    let mut tool_ids = tool_definitions
        .iter()
        .map(|tool| tool.name.clone())
        .collect::<Vec<_>>();
    tool_ids.sort();
    tool_ids.dedup();

    let runtime_policy = muse_core::domain::turn::RuntimePolicySnapshot {
        schema_version: 2,
        policy_version: "persona-resource-policy/v2".to_string(),
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
        skill_catalog,
        omitted_skill_count,
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

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
struct FrozenTurnToolCatalog<'a> {
    definitions: &'a [ToolDef],
    mcp: Option<&'a mcp::McpToolCatalog>,
}

struct FrozenTurnRuntime {
    context: TurnContext,
    provider: Arc<dyn muse_core::model::provider::ChatModelProvider>,
}

struct FrozenChatSelection {
    config: muse_core::model::LlmConfig,
    capabilities: Vec<String>,
    context_window: u64,
    max_output_tokens: u32,
}

impl FrozenChatSelection {
    fn from_resolved(model: muse_core::model::ResolvedChatModel) -> Self {
        Self {
            config: model.config,
            capabilities: model.catalog.capabilities,
            context_window: model.catalog.context_window,
            max_output_tokens: model.catalog.default_max_output_tokens,
        }
    }

    fn from_global_snapshot(
        profiles: &muse_core::model::ModelProfileConfig,
        config: muse_core::model::LlmConfig,
    ) -> Self {
        if let Some(model) = profiles.model(&config.provider, &config.model) {
            return Self {
                config,
                capabilities: model.capabilities,
                context_window: model.context_window,
                max_output_tokens: model.default_max_output_tokens,
            };
        }

        // `AppState::provider` 可能仍承载一次已经原子发布的旧配置快照。
        // 缺少目录项时只冻结保守的公开能力元数据，不据此推断额外能力。
        let defaults = model_capability_defaults(&config.provider, &config.api_base, &config.model);
        let max_output_tokens = if config.max_tokens == 0 {
            defaults.default_max_output_tokens
        } else {
            config.max_tokens
        };
        Self {
            config,
            capabilities: vec!["chat".to_string()],
            context_window: defaults.context_window,
            max_output_tokens,
        }
    }
}

struct FrozenVoiceSelection {
    voice_id: Option<String>,
    source: String,
    fallback: bool,
    fallback_reason: Option<String>,
}

fn resolve_turn_voice(tts: &TtsConfig, active_persona: Option<&Persona>) -> FrozenVoiceSelection {
    let preferred_voice = active_persona
        .and_then(|persona| persona.preferred_voice_id.as_deref())
        .map(str::trim)
        .filter(|voice_id| !voice_id.is_empty());

    if let Some(preferred_voice) = preferred_voice {
        let mut effective = tts.clone();
        effective.voice_id = preferred_voice.to_string();
        if effective.enabled() {
            return FrozenVoiceSelection {
                voice_id: Some(preferred_voice.to_string()),
                source: "persona_preference".to_string(),
                fallback: false,
                fallback_reason: None,
            };
        }

        return FrozenVoiceSelection {
            voice_id: None,
            source: "unavailable".to_string(),
            fallback: true,
            fallback_reason: Some(
                "角色音色引用已保留，但全局 TTS 未启用或配置不完整；本轮仅继续文本回复。"
                    .to_string(),
            ),
        };
    }

    if tts.enabled() {
        return FrozenVoiceSelection {
            voice_id: Some(tts.voice_id.trim().to_string()),
            source: "global_active".to_string(),
            fallback: false,
            fallback_reason: None,
        };
    }

    FrozenVoiceSelection {
        voice_id: None,
        source: "unavailable".to_string(),
        fallback: false,
        fallback_reason: Some("全局 TTS 未启用或配置不完整；本轮仅继续文本回复。".to_string()),
    }
}

async fn build_turn_context(
    state: &Arc<AppState>,
    conversation_id: String,
    turn_id: String,
    active_persona: Option<&Persona>,
    voice_enabled: bool,
    mut system_prompt: String,
    tools: FrozenTurnToolCatalog<'_>,
) -> Result<FrozenTurnRuntime, muse_core::model::provider::ChatModelError> {
    let mode_state = current_runtime_mode_state(state);
    let (profiles, global_chat_config, tts, global_provider) = {
        let _transition = state.model_configuration_transition_gate.lock().await;
        let config = state.model_config.lock().await;
        let snapshot = (
            config.model_profiles().clone(),
            config.chat().clone(),
            config.tts().clone(),
        );
        drop(config);
        let provider = state.provider.lock().await.clone();
        (snapshot.0, snapshot.1, snapshot.2, provider)
    };
    let global_chat = || {
        profiles
            .resolve_active_chat_model()
            .map(FrozenChatSelection::from_resolved)
            .unwrap_or_else(|_| {
                FrozenChatSelection::from_global_snapshot(&profiles, global_chat_config.clone())
            })
    };
    let global_provider_for = |chat: &FrozenChatSelection| {
        if let Some(provider) = global_provider.clone() {
            return Ok(provider);
        }
        if !chat.config.enabled() {
            return Err(missing_chat_provider_error());
        }
        muse_core::model::provider::factory::create_provider(&chat.config).map(Arc::from)
    };
    let (chat, provider, model_source, model_fallback, model_fallback_reason) =
        if let Some(preferred) = active_persona.and_then(|persona| persona.preferred_model_ref.as_ref())
        {
            match profiles.resolve_chat_model_reference(
                &preferred.provider_id,
                &preferred.model_id,
            ) {
                Ok(chat) => {
                    let chat = FrozenChatSelection::from_resolved(chat);
                    let provider: Arc<dyn muse_core::model::provider::ChatModelProvider> =
                        Arc::from(muse_core::model::provider::factory::create_provider(
                            &chat.config,
                        )?);
                    (
                        chat,
                        provider,
                        "persona_preference".to_string(),
                        false,
                        None,
                    )
                }
                Err(error) => {
                    let chat = global_chat();
                    let provider = global_provider_for(&chat)?;
                    (
                        chat,
                        provider,
                        "global_active".to_string(),
                        true,
                        Some(format!("{}；本轮已使用全局活动模型。", error)),
                    )
                }
            }
        } else {
            let chat = global_chat();
            let provider = global_provider_for(&chat)?;
            (
                chat,
                provider,
                "global_active".to_string(),
                false,
                None,
            )
        };
    let voice = resolve_turn_voice(&tts, active_persona);
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
    let mut tool_ids = tools
        .definitions
        .iter()
        .map(|tool| tool.name.clone())
        .collect::<Vec<_>>();
    tool_ids.sort();
    tool_ids.dedup();
    let visible_mcp_tools = tool_ids.iter().cloned().collect::<BTreeSet<_>>();
    let mcp_tool_policies = tools
        .mcp
        .map(|catalog| catalog.runtime_policy_entries(&visible_mcp_tools))
        .unwrap_or_default();

    let runtime_policy = muse_core::domain::turn::RuntimePolicySnapshot {
        schema_version: 4,
        policy_version: "persona-runtime-policy/v4".to_string(),
        persona_version: active_persona.map(|persona| persona.version.clone()),
        provider: chat.config.provider.clone(),
        model: chat.config.model.clone(),
        model_source: model_source.clone(),
        model_fallback,
        model_fallback_reason: model_fallback_reason.clone(),
        model_capabilities: chat.capabilities.clone(),
        model_context_window: chat.context_window,
        model_max_output_tokens: chat.max_output_tokens,
        voice_id: voice.voice_id.clone(),
        voice_source: voice.source.clone(),
        voice_fallback: voice.fallback,
        voice_fallback_reason: voice.fallback_reason.clone(),
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
        mcp_tool_policies,
    };
    let context = TurnContext {
        conversation_id,
        turn_id,
        persona_id: active_persona.map(|persona| persona.id.clone()),
        system_prompt,
        model_provider: chat.config.provider,
        model_name: chat.config.model,
        model_source,
        model_fallback,
        model_fallback_reason,
        model_capabilities: chat.capabilities,
        model_context_window: chat.context_window,
        model_max_output_tokens: chat.max_output_tokens,
        tool_policy,
        skill_policy,
        mcp_policy,
        runtime_mode: mode_state.mode.as_str().to_string(),
        focus_phase: mode_state.focus_phase.as_str().to_string(),
        tool_preset: mode_state.tool_preset().as_str().to_string(),
        voice_enabled,
        active_voice_id: voice.voice_id,
        voice_source: voice.source,
        voice_fallback: voice.fallback,
        voice_fallback_reason: voice.fallback_reason,
        created_at,
        runtime_policy,
        tool_definitions: tools.definitions.to_vec(),
    };
    Ok(FrozenTurnRuntime { context, provider })
}

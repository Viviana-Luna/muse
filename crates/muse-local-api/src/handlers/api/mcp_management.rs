// 手动 MCP 连接管理 API，统一读写受保护的 `config.toml` 事实源。

type McpApiResult<T> = Result<T, (StatusCode, Json<ErrorResponse>)>;

fn mcp_server_revision(profile: &mcp::McpServerProfile) -> String {
    let bytes = serde_json::to_vec(profile).unwrap_or_default();
    format!("{:x}", Sha256::digest(bytes))
}

fn valid_mcp_name(value: &str) -> McpApiResult<String> {
    let value = value.trim();
    if value.is_empty()
        || value.len() > 64
        || !value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-'))
    {
        return Err(bad_request(
            "MCP 连接名称只允许 1-64 个英文字母、数字、下划线或短横线。",
        ));
    }
    Ok(value.to_string())
}

fn valid_environment_name(value: &str) -> McpApiResult<String> {
    let value = value.trim();
    if value.is_empty()
        || value.len() > 128
        || !value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '_')
    {
        return Err(bad_request("环境变量名称只允许英文字母、数字和下划线。"));
    }
    Ok(value.to_string())
}

fn valid_header_name(value: &str) -> McpApiResult<String> {
    let value = value.trim();
    if value.is_empty()
        || value.len() > 256
        || value.chars().any(char::is_control)
        || reqwest::header::HeaderName::from_bytes(value.as_bytes()).is_err()
    {
        return Err(bad_request("Header 名称无效。"));
    }
    Ok(value.to_string())
}

fn mcp_config_error(
    error: muse_core::app::preferences::MuseConfigStoreError,
) -> (StatusCode, Json<ErrorResponse>) {
    use muse_core::app::preferences::MuseConfigStoreError;
    match error {
        MuseConfigStoreError::Validation(diagnostic) => (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: format!("{}：{}", diagnostic.code, diagnostic.message),
            }),
        ),
        MuseConfigStoreError::Conflict(diagnostic) => (
            StatusCode::CONFLICT,
            Json(ErrorResponse {
                error: format!("{}：{}", diagnostic.code, diagnostic.message),
            }),
        ),
        MuseConfigStoreError::UnsupportedVersion(_) => bad_request(
            "config_version_unsupported：config.toml 版本高于当前 Muse 支持范围。",
        ),
        MuseConfigStoreError::Io(_)
        | MuseConfigStoreError::Parse(_)
        | MuseConfigStoreError::Serialize(_) => internal_error(
            "config_write_failed：读写 config.toml 失败，请检查语法、权限和磁盘状态。"
                .to_string(),
        ),
    }
}

async fn invalidate_mcp_catalog(state: &AppState, config_path: PathBuf) {
    state
        .runtime_service
        .replace_mcp_tool_catalog(mcp::McpToolCatalog::empty_for_config_path(config_path))
        .await;
}

async fn load_mcp_config(
    state: &AppState,
) -> McpApiResult<(mcp::McpProfileConfig, mcp::McpRuntimeSnapshot)> {
    let (profiles, snapshot, changed, valid) = {
        let mut store = state.user_config.lock().await;
        let changed = store.refresh_from_disk().map_err(mcp_config_error)?;
        (
            store.mcp_profiles().clone(),
            store.mcp_runtime_snapshot(),
            changed,
            store.mcp_profiles_are_valid(),
        )
    };
    if !valid {
        return Err(bad_request(
            "config_mcp_profiles_invalid：config.toml 中的 mcp_servers 无法解析，请修正后重试。",
        ));
    }
    if changed {
        invalidate_mcp_catalog(state, snapshot.config_path().clone()).await;
    }
    Ok((profiles, snapshot))
}

fn secret_state(target: String, configured: bool) -> McpSecretFieldState {
    McpSecretFieldState {
        target,
        configured,
    }
}

fn mcp_detail_response(name: String, profile: &mcp::McpServerProfile) -> McpServerDetailResponse {
    let transport = match profile.transport.as_str() {
        "stdio" => McpTransportResponse::Stdio {
            command: profile.command.clone().unwrap_or_default(),
            args: profile.args.clone().unwrap_or_default(),
            cwd: profile.cwd.clone(),
            env: profile.env.clone(),
            secrets: profile
                .secret_env
                .keys()
                .cloned()
                .map(|target| secret_state(target, true))
                .collect(),
        },
        _ => {
            let bearer_token = profile
                .secret_headers
                .get("Authorization")
                .is_some_and(|value| value.starts_with("Bearer "))
                .then(|| secret_state("Authorization".to_string(), true));
            let header_secrets = profile
                .secret_headers
                .keys()
                .filter(|target| {
                    target.as_str() != "Authorization" || bearer_token.is_none()
                })
                .cloned()
                .map(|target| secret_state(target, true))
                .collect();
            McpTransportResponse::StreamableHttp {
                url: profile.url.clone().unwrap_or_default(),
                headers: profile.headers.clone(),
                header_secrets,
                bearer_token,
            }
        }
    };
    McpServerDetailResponse {
        name,
        enabled: profile.enabled,
        request_timeout_ms: profile.request_timeout_ms,
        enabled_tools: profile.enabled_tools.clone(),
        disabled_tools: profile.disabled_tools.clone(),
        approval_policy: profile.approval_policy,
        tool_approval_overrides: profile.tool_approval_overrides.clone(),
        transport,
        revision: mcp_server_revision(profile),
    }
}

fn apply_secret_update(
    field: McpSecretFieldUpdate,
    existing: &BTreeMap<String, String>,
    next: &mut BTreeMap<String, String>,
    validate_target: fn(&str) -> McpApiResult<String>,
) -> McpApiResult<()> {
    let target = validate_target(&field.target)?;
    match field.secret.action {
        SecretUpdateAction::Keep => {
            let value = existing
                .get(&target)
                .cloned()
                .ok_or_else(|| bad_request("没有可保留的 MCP API Key，请输入新值。"))?;
            next.insert(target, value);
        }
        SecretUpdateAction::Replace => {
            let value = field
                .secret
                .value
                .as_deref()
                .map(str::trim)
                .unwrap_or_default();
            if value.is_empty() {
                return Err(bad_request("替换 MCP API Key 时必须提供非空值。"));
            }
            next.insert(target, value.to_string());
        }
        SecretUpdateAction::Delete => {}
    }
    Ok(())
}

fn build_mcp_profile(
    request: McpServerCreateRequest,
    existing: Option<&mcp::McpServerProfile>,
) -> McpApiResult<(String, mcp::McpServerProfile)> {
    let name = valid_mcp_name(&request.name)?;
    if let Some(timeout) = request.request_timeout_ms
        && !(1_000..=120_000).contains(&timeout)
    {
        return Err(bad_request("MCP 请求超时必须在 1000-120000 毫秒之间。"));
    }
    let mut profile = mcp::McpServerProfile {
        transport: String::new(),
        enabled: request.enabled,
        request_timeout_ms: request.request_timeout_ms,
        enabled_tools: request.enabled_tools,
        disabled_tools: request.disabled_tools,
        approval_policy: request.approval_policy,
        tool_approval_overrides: request.tool_approval_overrides,
        command: None,
        args: None,
        cwd: None,
        url: None,
        env: BTreeMap::new(),
        secret_env: BTreeMap::new(),
        headers: BTreeMap::new(),
        secret_headers: BTreeMap::new(),
    };

    match request.transport {
        McpTransportUpdate::Stdio {
            command,
            args,
            cwd,
            mut env,
            secrets,
        } => {
            profile.transport = "stdio".to_string();
            profile.command = Some(command.trim().to_string());
            profile.args = Some(args);
            profile.cwd = cwd
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty());
            let existing_secrets = existing
                .filter(|profile| profile.transport == "stdio")
                .map(|profile| &profile.secret_env)
                .cloned()
                .unwrap_or_default();
            for field in secrets {
                let target = valid_environment_name(&field.target)?;
                env.remove(&target);
                apply_secret_update(
                    field,
                    &existing_secrets,
                    &mut profile.secret_env,
                    valid_environment_name,
                )?;
            }
            profile.env = env;
        }
        McpTransportUpdate::StreamableHttp {
            url,
            mut headers,
            header_secrets,
            bearer_token,
        } => {
            profile.transport = "streamable_http".to_string();
            profile.url = Some(url.trim().to_string());
            let existing_secrets = existing
                .filter(|profile| profile.transport == "streamable_http")
                .map(|profile| &profile.secret_headers)
                .cloned()
                .unwrap_or_default();
            for field in header_secrets {
                let target = valid_header_name(&field.target)?;
                if target.eq_ignore_ascii_case("Authorization") && bearer_token.is_some() {
                    return Err(bad_request(
                        "Authorization 不能同时配置为 Header API Key 和 Bearer Token。",
                    ));
                }
                headers.remove(&target);
                apply_secret_update(
                    field,
                    &existing_secrets,
                    &mut profile.secret_headers,
                    valid_header_name,
                )?;
            }
            if let Some(field) = bearer_token {
                headers.retain(|target, _| !target.eq_ignore_ascii_case("Authorization"));
                match field.secret.action {
                    SecretUpdateAction::Keep => {
                        let value = existing_secrets
                            .get("Authorization")
                            .cloned()
                            .ok_or_else(|| {
                                bad_request("没有可保留的 Bearer Token，请输入新值。")
                            })?;
                        profile
                            .secret_headers
                            .insert("Authorization".to_string(), value);
                    }
                    SecretUpdateAction::Replace => {
                        let value = field
                            .secret
                            .value
                            .as_deref()
                            .map(str::trim)
                            .unwrap_or_default();
                        if value.is_empty() {
                            return Err(bad_request("替换 Bearer Token 时必须提供非空值。"));
                        }
                        profile.secret_headers.insert(
                            "Authorization".to_string(),
                            format!("Bearer {value}"),
                        );
                    }
                    SecretUpdateAction::Delete => {}
                }
            }
            profile.headers = headers;
        }
    }

    profile
        .validate(&name)
        .map_err(|error| bad_request(&format!("mcp_config_invalid：{error}")))?;
    Ok((name, profile))
}

async fn commit_mcp_config(
    state: &Arc<AppState>,
    current_name: Option<&str>,
    expected_revision: Option<&str>,
    request: McpServerCreateRequest,
) -> McpApiResult<McpServerDetailResponse> {
    let (name, profile, config_path) = {
        let mut store = state.user_config.lock().await;
        store.refresh_from_disk().map_err(mcp_config_error)?;
        if !store.mcp_profiles_are_valid() {
            return Err(bad_request(
                "config_mcp_profiles_invalid：config.toml 中的 mcp_servers 无法解析，请修正后重试。",
            ));
        }
        let mut next = store.mcp_profiles().clone();
        let existing = current_name.and_then(|name| next.mcp_servers.get(name));
        if let (Some(expected), Some(existing)) = (expected_revision, existing)
            && mcp_server_revision(existing) != expected
        {
            return Err((
                StatusCode::CONFLICT,
                Json(ErrorResponse {
                    error: "mcp_revision_conflict：MCP 连接已在其他窗口中变更，请刷新后重试。"
                        .to_string(),
                }),
            ));
        }
        if current_name.is_some() && existing.is_none() {
            return Err((
                StatusCode::NOT_FOUND,
                Json(ErrorResponse {
                    error: "MCP 连接不存在。".to_string(),
                }),
            ));
        }
        let (name, profile) = build_mcp_profile(request, existing)?;
        if next.mcp_servers.contains_key(&name) && current_name != Some(name.as_str()) {
            return Err((
                StatusCode::CONFLICT,
                Json(ErrorResponse {
                    error: format!("mcp_conflict：MCP 连接 `{name}` 已存在。"),
                }),
            ));
        }
        if let Some(current_name) = current_name {
            next.mcp_servers.remove(current_name);
        }
        next.mcp_servers.insert(name.clone(), profile.clone());
        store.update_mcp_profiles(next).map_err(mcp_config_error)?;
        (name, profile, store.storage_path().to_path_buf())
    };
    invalidate_mcp_catalog(state, config_path).await;
    if let Some(current_name) = current_name {
        state
            .runtime_service
            .clear_mcp_server_check(current_name)
            .await;
    }
    state.runtime_service.clear_mcp_server_check(&name).await;
    Ok(mcp_detail_response(name, &profile))
}

pub(crate) async fn handle_mcp_servers(
    State(state): State<Arc<AppState>>,
) -> McpApiResult<Json<Vec<McpServerSummaryResponse>>> {
    let (profiles, _) = load_mcp_config(&state).await?;
    let mut output = Vec::with_capacity(profiles.mcp_servers.len());
    for (name, profile) in profiles.mcp_servers {
        let revision = mcp_server_revision(&profile);
        let check = state
            .runtime_service
            .mcp_server_check(&name)
            .await
            .filter(|check| check.revision == revision);
        let policy_reason = (!profile.enabled).then(|| "server_disabled".to_string());
        output.push(McpServerSummaryResponse {
            name,
            enabled: profile.enabled,
            transport: profile.transport,
            revision,
            status: if !profile.enabled {
                "policy_blocked".to_string()
            } else {
                check
                    .as_ref()
                    .map(|value| value.status.clone())
                    .unwrap_or_else(|| "not_tested".to_string())
            },
            tested_revision: check.as_ref().map(|value| value.revision.clone()),
            tool_count: check.as_ref().map(|value| value.tool_count).unwrap_or(0),
            resource_count: check.as_ref().map(|value| value.resource_count).unwrap_or(0),
            last_checked_at: check.as_ref().map(|value| value.checked_at.clone()),
            last_error: check.and_then(|value| value.last_error),
            policy_reason,
        });
    }
    Ok(Json(output))
}

pub(crate) async fn handle_create_mcp_server(
    State(state): State<Arc<AppState>>,
    Json(request): Json<McpServerCreateRequest>,
) -> McpApiResult<(StatusCode, Json<McpServerDetailResponse>)> {
    let detail = commit_mcp_config(&state, None, None, request).await?;
    Ok((StatusCode::CREATED, Json(detail)))
}

pub(crate) async fn handle_get_mcp_server(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> McpApiResult<Json<McpServerDetailResponse>> {
    let (profiles, _) = load_mcp_config(&state).await?;
    let profile = profiles.mcp_servers.get(&name).ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: format!("MCP 连接 `{name}` 不存在。"),
            }),
        )
    })?;
    Ok(Json(mcp_detail_response(name, profile)))
}

pub(crate) async fn handle_update_mcp_server(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    Json(request): Json<McpServerUpdateRequest>,
) -> McpApiResult<Json<McpServerDetailResponse>> {
    commit_mcp_config(&state, Some(&name), Some(&request.revision), request.server)
        .await
        .map(Json)
}

pub(crate) async fn handle_delete_mcp_server(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    Query(query): Query<RevisionQuery>,
) -> McpApiResult<StatusCode> {
    let config_path = {
        let mut store = state.user_config.lock().await;
        store.refresh_from_disk().map_err(mcp_config_error)?;
        if !store.mcp_profiles_are_valid() {
            return Err(bad_request(
                "config_mcp_profiles_invalid：config.toml 中的 mcp_servers 无法解析，请修正后重试。",
            ));
        }
        let mut next = store.mcp_profiles().clone();
        let profile = next.mcp_servers.get(&name).ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                Json(ErrorResponse {
                    error: format!("MCP 连接 `{name}` 不存在。"),
                }),
            )
        })?;
        if mcp_server_revision(profile) != query.revision {
            return Err((
                StatusCode::CONFLICT,
                Json(ErrorResponse {
                    error: "mcp_revision_conflict：MCP 连接已在其他窗口中变更，请刷新后重试。"
                        .to_string(),
                }),
            ));
        }
        next.mcp_servers.remove(&name);
        store.update_mcp_profiles(next).map_err(mcp_config_error)?;
        store.storage_path().to_path_buf()
    };
    invalidate_mcp_catalog(&state, config_path).await;
    state.runtime_service.clear_mcp_server_check(&name).await;
    Ok(StatusCode::NO_CONTENT)
}

async fn inspect_single_mcp_profile(
    name: &str,
    profile: &mcp::McpServerProfile,
    config_path: PathBuf,
    include_resources: bool,
) -> McpCatalogResponse {
    let mut test_profile = profile.clone();
    test_profile.enabled = true;
    let snapshot = mcp::McpProfileConfig {
        mcp_servers: BTreeMap::from([(name.to_string(), test_profile)]),
    }
    .runtime_snapshot(config_path);
    let manager = mcp::McpClientManager::default();
    let catalog = mcp::discover_external_mcp_tools_for_scope_with_manager(
        &snapshot,
        &mcp::EffectiveMcpScope::only(name),
        &manager,
    )
    .await;
    let tools = catalog
        .tools
        .iter()
        .filter(|tool| tool.server_name == name)
        .map(|tool| {
            serde_json::json!({
                "name": tool.original_tool_name,
                "qualified_name": tool.name,
                "description": tool.description,
                "read_only": tool.read_only,
                "annotations": tool.annotations,
                "annotations_hash": tool.annotations_hash,
                "local_approval": {
                    "policy": tool.approval_policy,
                    "source": tool.approval_source,
                    "final_risk": tool.final_risk,
                    "requires_approval": tool.requires_approval,
                },
            })
        })
        .collect::<Vec<_>>();
    let mut errors = catalog
        .errors
        .iter()
        .filter(|error| {
            error
                .get("server")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|server| server == name)
        })
        .cloned()
        .collect::<Vec<_>>();
    let resources = if include_resources && errors.is_empty() {
        match catalog.list_resources(Some(name.to_string()), None).await {
            Ok(result) => {
                errors.extend(result.errors);
                result.resources
            }
            Err(error) => {
                let diagnostic = mcp::structured_mcp_error(error);
                errors.push(serde_json::json!({
                    "server": name,
                    "code": diagnostic.code,
                    "kind": diagnostic.kind,
                    "message": diagnostic.message,
                    "retryable": diagnostic.retryable,
                    "alternatives": diagnostic.alternatives,
                }));
                Vec::new()
            }
        }
    } else {
        Vec::new()
    };
    let connection = match catalog.connection_diagnostic(name).await {
        Some(diagnostic) => Some(diagnostic),
        None => manager.last_failed_diagnostic(name),
    };
    McpCatalogResponse {
        server: name.to_string(),
        revision: mcp_server_revision(profile),
        status: if errors.is_empty() {
            "connected".to_string()
        } else {
            "failed".to_string()
        },
        tools,
        resources,
        errors,
        refreshed_at: catalog.refreshed_at,
        diagnostic: McpCatalogDiagnosticResponse {
            connection,
            policy_reason: None,
            redirect_policy: "disabled",
            proxy_policy: "direct_only",
            sensitive_headers: "exact_configured_origin_only",
        },
    }
}

async fn refresh_single_mcp_catalog(
    state: &Arc<AppState>,
    name: &str,
    include_resources: bool,
) -> McpApiResult<McpCatalogResponse> {
    let (profiles, snapshot) = load_mcp_config(state).await?;
    let profile = profiles.mcp_servers.get(name).ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: format!("MCP 连接 `{name}` 不存在。"),
            }),
        )
    })?;
    let result = inspect_single_mcp_profile(
        name,
        profile,
        snapshot.config_path().clone(),
        include_resources,
    )
    .await;
    let last_error = result
        .errors
        .first()
        .and_then(|error| error.get("message"))
        .and_then(serde_json::Value::as_str)
        .map(mcp::structured_mcp_error);
    state
        .runtime_service
        .publish_mcp_server_check(
            name.to_string(),
            mcp::McpServerCheckStatus {
                revision: result.revision.clone(),
                status: result.status.clone(),
                tool_count: result.tools.len(),
                resource_count: result.resources.len(),
                checked_at: result.refreshed_at.clone(),
                last_error,
                connection: result.diagnostic.connection.clone(),
            },
        )
        .await;
    Ok(result)
}

pub(crate) async fn handle_test_mcp_draft(
    State(state): State<Arc<AppState>>,
    Json(request): Json<McpDraftTestRequest>,
) -> McpApiResult<Json<McpCatalogResponse>> {
    let (name, profile, config_path) = {
        let mut store = state.user_config.lock().await;
        store.refresh_from_disk().map_err(mcp_config_error)?;
        if !store.mcp_profiles_are_valid() {
            return Err(bad_request(
                "config_mcp_profiles_invalid：config.toml 中的 mcp_servers 无法解析，请修正后重试。",
            ));
        }
        let existing = match request.source_name.as_deref() {
            Some(source_name) => {
                let profile = store
                    .mcp_profiles()
                    .mcp_servers
                    .get(source_name)
                    .ok_or_else(|| {
                        (
                            StatusCode::NOT_FOUND,
                            Json(ErrorResponse {
                                error: format!("MCP 连接 `{source_name}` 不存在。"),
                            }),
                        )
                    })?;
                let expected = request.source_revision.as_deref().ok_or_else(|| {
                    bad_request("mcp_revision_required：测试已保存草稿时必须提供 revision。")
                })?;
                if mcp_server_revision(profile) != expected {
                    return Err((
                        StatusCode::CONFLICT,
                        Json(ErrorResponse {
                            error: "mcp_revision_conflict：MCP 连接已在其他窗口中变更，请刷新后重试。"
                                .to_string(),
                        }),
                    ));
                }
                Some(profile)
            }
            None => {
                if request.source_revision.is_some() {
                    return Err(bad_request(
                        "mcp_source_required：提供 source_revision 时必须同时提供 source_name。",
                    ));
                }
                None
            }
        };
        let (name, profile) = build_mcp_profile(request.server, existing)?;
        (name, profile, store.storage_path().to_path_buf())
    };
    Ok(Json(
        inspect_single_mcp_profile(&name, &profile, config_path, request.include_resources).await,
    ))
}

pub(crate) async fn handle_test_mcp_server(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> McpApiResult<Json<McpCatalogResponse>> {
    refresh_single_mcp_catalog(&state, &name, false)
        .await
        .map(Json)
}

pub(crate) async fn handle_refresh_mcp_server(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> McpApiResult<Json<McpCatalogResponse>> {
    refresh_single_mcp_catalog(&state, &name, true)
        .await
        .map(Json)
}

pub(crate) async fn handle_mcp_server_tools(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> McpApiResult<Json<Vec<serde_json::Value>>> {
    refresh_single_mcp_catalog(&state, &name, false)
        .await
        .map(|result| Json(result.tools))
}

pub(crate) async fn handle_mcp_server_resources(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> McpApiResult<Json<Vec<serde_json::Value>>> {
    refresh_single_mcp_catalog(&state, &name, true)
        .await
        .map(|result| Json(result.resources))
}

#[cfg(test)]
mod mcp_management_tests {
    use super::*;

    fn replacement_secret(target: &str, value: &str) -> McpSecretFieldUpdate {
        McpSecretFieldUpdate {
            target: target.to_string(),
            secret: SecretUpdate {
                action: SecretUpdateAction::Replace,
                value: Some(value.to_string()),
            },
        }
    }

    #[test]
    fn stores_api_key_with_its_stdio_server_and_redacts_get_response() {
        let request = McpServerCreateRequest {
            name: "github".to_string(),
            enabled: true,
            request_timeout_ms: Some(30_000),
            enabled_tools: None,
            disabled_tools: Vec::new(),
            approval_policy: Default::default(),
            tool_approval_overrides: BTreeMap::new(),
            transport: McpTransportUpdate::Stdio {
                command: "npx".to_string(),
                args: vec!["-y".to_string(), "github-mcp".to_string()],
                cwd: None,
                env: BTreeMap::from([("LOG_LEVEL".to_string(), "info".to_string())]),
                secrets: vec![replacement_secret("GITHUB_TOKEN", "top-secret")],
            },
        };

        let (name, profile) = build_mcp_profile(request, None).expect("有效配置应可构造");
        assert_eq!(profile.secret_env["GITHUB_TOKEN"], "top-secret");
        let response = mcp_detail_response(name, &profile);
        let serialized = serde_json::to_string(&response).expect("响应应可序列化");
        assert!(!serialized.contains("top-secret"));
        assert!(serialized.contains("GITHUB_TOKEN"));
        assert!(serialized.contains("\"configured\":true"));
    }

    #[test]
    fn keep_replace_and_delete_use_target_as_the_secret_key() {
        let existing = mcp::McpServerProfile {
            transport: "stdio".to_string(),
            enabled: true,
            request_timeout_ms: None,
            enabled_tools: None,
            disabled_tools: Vec::new(),
            approval_policy: Default::default(),
            tool_approval_overrides: BTreeMap::new(),
            command: Some("demo".to_string()),
            args: Some(Vec::new()),
            cwd: None,
            url: None,
            env: BTreeMap::new(),
            secret_env: BTreeMap::from([("TOKEN".to_string(), "old".to_string())]),
            headers: BTreeMap::new(),
            secret_headers: BTreeMap::new(),
        };
        let request = McpServerCreateRequest {
            name: "demo".to_string(),
            enabled: true,
            request_timeout_ms: None,
            enabled_tools: None,
            disabled_tools: Vec::new(),
            approval_policy: Default::default(),
            tool_approval_overrides: BTreeMap::new(),
            transport: McpTransportUpdate::Stdio {
                command: "demo".to_string(),
                args: Vec::new(),
                cwd: None,
                env: BTreeMap::new(),
                secrets: vec![McpSecretFieldUpdate {
                    target: "TOKEN".to_string(),
                    secret: SecretUpdate {
                        action: SecretUpdateAction::Keep,
                        value: None,
                    },
                }],
            },
        };
        let (_, kept) = build_mcp_profile(request, Some(&existing)).expect("应保留旧值");
        assert_eq!(kept.secret_env["TOKEN"], "old");

        let replace_request = McpServerCreateRequest {
            name: "demo".to_string(),
            enabled: true,
            request_timeout_ms: None,
            enabled_tools: None,
            disabled_tools: Vec::new(),
            approval_policy: Default::default(),
            tool_approval_overrides: BTreeMap::new(),
            transport: McpTransportUpdate::Stdio {
                command: "demo".to_string(),
                args: Vec::new(),
                cwd: None,
                env: BTreeMap::new(),
                secrets: vec![replacement_secret("TOKEN", "new")],
            },
        };
        let (_, replaced) =
            build_mcp_profile(replace_request, Some(&existing)).expect("应替换旧值");
        assert_eq!(replaced.secret_env["TOKEN"], "new");

        let delete_request = McpServerCreateRequest {
            name: "demo".to_string(),
            enabled: true,
            request_timeout_ms: None,
            enabled_tools: None,
            disabled_tools: Vec::new(),
            approval_policy: Default::default(),
            tool_approval_overrides: BTreeMap::new(),
            transport: McpTransportUpdate::Stdio {
                command: "demo".to_string(),
                args: Vec::new(),
                cwd: None,
                env: BTreeMap::new(),
                secrets: vec![McpSecretFieldUpdate {
                    target: "TOKEN".to_string(),
                    secret: SecretUpdate {
                        action: SecretUpdateAction::Delete,
                        value: None,
                    },
                }],
            },
        };
        let (_, deleted) =
            build_mcp_profile(delete_request, Some(&existing)).expect("应删除旧值");
        assert!(!deleted.secret_env.contains_key("TOKEN"));
    }

    #[test]
    fn bearer_token_is_saved_as_secret_authorization_header() {
        let request = McpServerCreateRequest {
            name: "remote".to_string(),
            enabled: true,
            request_timeout_ms: None,
            enabled_tools: None,
            disabled_tools: Vec::new(),
            approval_policy: Default::default(),
            tool_approval_overrides: BTreeMap::new(),
            transport: McpTransportUpdate::StreamableHttp {
                url: "https://example.test/mcp".to_string(),
                headers: BTreeMap::from([("Accept".to_string(), "application/json".to_string())]),
                header_secrets: Vec::new(),
                bearer_token: Some(replacement_secret("IGNORED", "api-key")),
            },
        };

        let (_, profile) = build_mcp_profile(request, None).expect("Bearer 配置应可构造");
        assert_eq!(
            profile.secret_headers["Authorization"],
            "Bearer api-key"
        );
    }

    #[test]
    fn target_is_the_only_secret_name_field() {
        let request: McpServerCreateRequest = serde_json::from_value(serde_json::json!({
            "name": "demo",
            "transport": {
                "type": "stdio",
                "command": "demo",
                "secrets": [{
                    "target": "TOKEN",
                    "secret": {"action": "replace", "value": "api-key"}
                }]
            }
        }))
        .expect("缺少 environment_name 的新请求应可解析");
        let (_, profile) = build_mcp_profile(request, None).expect("新请求应可构造 Profile");
        assert_eq!(profile.secret_env["TOKEN"], "api-key");

        let legacy = serde_json::from_value::<McpServerCreateRequest>(serde_json::json!({
            "name": "demo",
            "transport": {
                "type": "stdio",
                "command": "demo",
                "secrets": [{
                    "target": "TOKEN",
                    "environment_name": "LEGACY_TOKEN",
                    "secret": {"action": "replace", "value": "api-key"}
                }]
            }
        }));
        assert!(legacy.is_err(), "废弃字段不得被静默忽略");
    }
}

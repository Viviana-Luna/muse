async fn tool_mcp_list_resources(
    frozen_mcp_catalog: &mcp::McpToolCatalog,
    call: &ToolCall,
) -> ToolResult {
    if let Err(result) = validate_mcp_server_argument(call) {
        return result;
    }
    let server = tool_arg_string(&call.arguments, "server");
    let cursor = tool_arg_string(&call.arguments, "cursor");
    if cursor.is_some() && server.as_deref().is_some_and(is_local_mcp_server) {
        return tool_failed(
            "本地 MCP 资源不支持 cursor 分页。",
            "local_cursor_unsupported",
        );
    }
    let include_local = server
        .as_deref()
        .is_none_or(is_local_mcp_server);
    let include_external = server
        .as_deref()
        .is_none_or(|value| !is_local_mcp_server(value));

    let mut resources = Vec::new();
    let mut errors = Vec::new();
    let mut next_cursor = None;
    let mut external_refreshed_at = None;

    let local_count = if include_local {
        let local_mcp_resources = runtime_mcp_resources()
            .iter()
            .map(RuntimeMcpResource::info_json)
            .collect::<Vec<_>>();
        let count = local_mcp_resources.len();
        resources.extend(local_mcp_resources);
        count
    } else {
        0
    };

    let external_count = if include_external {
        match frozen_mcp_catalog
            .list_resources(server.clone(), cursor.clone())
            .await
        {
            Ok(result) => {
                let count = result.resources.len();
                next_cursor = result.next_cursor;
                external_refreshed_at = Some(result.refreshed_at);
                resources.extend(result.resources);
                errors.extend(result.errors);
                count
            }
            Err(err) if server.is_some() => return tool_failed(err, "external_mcp_failed"),
            Err(err) => {
                errors.push(serde_json::json!({
                    "source": "external_mcp",
                    "message": err
                }));
                0
            }
        }
    } else {
        0
    };

    ToolResult {
        status: ToolResultStatus::Success,
        content: format!(
            "MCP resource 列表已刷新：本地 {} 个，外部 {} 个，错误 {} 个。",
            local_count,
            external_count,
            errors.len()
        ),
        structured: Some(serde_json::json!({
            "server": server,
            "local_server": LOCAL_MCP_SERVER_NAME,
            "resources": resources,
            "errors": errors,
            "next_cursor": next_cursor,
            "refreshed_at": chrono::Local::now().to_rfc3339(),
            "external_refreshed_at": external_refreshed_at,
        })),
    }
}

async fn tool_mcp_list_resource_templates(
    frozen_mcp_catalog: &mcp::McpToolCatalog,
    call: &ToolCall,
) -> ToolResult {
    if let Err(result) = validate_mcp_server_argument(call) {
        return result;
    }
    let server = tool_arg_string(&call.arguments, "server");
    let cursor = tool_arg_string(&call.arguments, "cursor");
    if server.as_deref().is_some_and(is_local_mcp_server) {
        if cursor.is_some() {
            return tool_failed(
                "本地 MCP resource template 不支持 cursor 分页。",
                "local_cursor_unsupported",
            );
        }
        return ToolResult {
            status: ToolResultStatus::Success,
            content: "本地 MCP server 当前没有 resource template。".to_string(),
            structured: Some(serde_json::json!({
                "server": LOCAL_MCP_SERVER_NAME,
                "resource_templates": [],
                "errors": [],
                "next_cursor": null,
                "refreshed_at": chrono::Local::now().to_rfc3339(),
            })),
        };
    }

    match frozen_mcp_catalog
        .list_resource_templates(server.clone(), cursor)
        .await
    {
        Ok(result) => ToolResult {
            status: ToolResultStatus::Success,
            content: format!(
                "外部 MCP resource template 列表已刷新：{} 个，错误 {} 个。",
                result.resource_templates.len(),
                result.errors.len()
            ),
            structured: Some(serde_json::json!({
                "server": server,
                "resource_templates": result.resource_templates,
                "errors": result.errors,
                "next_cursor": result.next_cursor,
                "refreshed_at": result.refreshed_at,
            })),
        },
        Err(err) => tool_failed(err, "external_mcp_failed"),
    }
}

async fn tool_mcp_read_resource(
    state: &Arc<AppState>,
    frozen_mcp_catalog: &mcp::McpToolCatalog,
    call: &ToolCall,
    conversation: &Conversation,
) -> ToolResult {
    if let Err(result) = validate_mcp_server_argument(call) {
        return result;
    }
    let Some(uri) = tool_arg_string(&call.arguments, "uri") else {
        return tool_failed("mcp_read_resource 缺少 uri 参数。", "missing_uri");
    };
    let server = tool_arg_string(&call.arguments, "server")
        .unwrap_or_else(|| LOCAL_MCP_SERVER_NAME.to_string());

    if !is_local_mcp_server(&server) {
        return match frozen_mcp_catalog.read_resource(server, uri).await {
            Ok(result) => external_mcp_resource_success(result),
            Err(err) => tool_failed(err, "external_mcp_failed"),
        };
    }

    if uri.starts_with(COMMAND_AUDIT_RESOURCE_PREFIX) {
        return match read_command_audit_resource(uri.clone()).await {
            Ok(resource) => ToolResult {
                status: ToolResultStatus::Success,
                content: resource.content,
                structured: Some(serde_json::json!({
                    "resource_uri": uri,
                    "read_only": true,
                    "captured_bytes": resource.captured_bytes,
                    "truncated": resource.truncated,
                })),
            },
            Err(err) => tool_failed(err, "command_audit_read_failed"),
        };
    }

    let normalized_uri = normalize_local_mcp_uri(&uri);
    let Some(resource) = runtime_mcp_resources()
        .into_iter()
        .find(|resource| resource.uri == normalized_uri)
    else {
        return tool_failed(
            format!("未知 MCP resource URI：{uri}。请先调用 mcp_list_resources 获取可用资源。"),
            "unknown_resource",
        );
    };

    read_runtime_mcp_resource(state, resource, conversation).await
}

fn external_mcp_resource_success(result: mcp::ExternalMcpReadResource) -> ToolResult {
    let (content, truncated) = truncate_text_with_flag(&result.content, MCP_RESOURCE_MAX_CHARS);
    let mut structured = result.structured;
    if let Some(object) = structured.as_object_mut() {
        object.insert("read_only".to_string(), serde_json::json!(true));
        object.insert("truncated".to_string(), serde_json::json!(truncated));
        object.insert("content".to_string(), serde_json::json!(content.clone()));
    }

    ToolResult {
        status: ToolResultStatus::Success,
        content,
        structured: Some(structured),
    }
}

async fn read_runtime_mcp_resource(
    state: &Arc<AppState>,
    resource: RuntimeMcpResource,
    conversation: &Conversation,
) -> ToolResult {
    match resource.source.clone() {
        RuntimeMcpResourceSource::CurrentConversation => {
            let raw = serde_json::to_string_pretty(&serde_json::json!({
                "conversation_id": active_conversation_id(state),
                "messages": conversation.messages,
            }))
            .unwrap_or_else(|err| format!("序列化当前会话失败：{err}"));
            mcp_resource_success(resource, raw, true)
        }
        RuntimeMcpResourceSource::VirtualTranscript => {
            match state.runtime_service.session_store().await {
                Ok(store) => match store.aggregate_events().await {
                    Ok(events) if !events.is_empty() => {
                        mcp_resource_success(resource, session_events_as_jsonl(&events), true)
                    }
                    Ok(_) => mcp_resource_success(
                        resource,
                        "当前尚未写入 runtime transcript。".to_string(),
                        false,
                    ),
                    Err(err) => tool_failed(
                        format!("聚合 v3 runtime transcript 失败：{err}"),
                        "read_failed",
                    ),
                },
                Err(err) => tool_failed(format!("打开 v3 会话存储失败：{err}"), "read_failed"),
            }
        }
        RuntimeMcpResourceSource::File {
            path,
            empty_message,
        } => match fs::read_to_string(path).await {
            Ok(content) if !content.trim().is_empty() => {
                mcp_resource_success(resource.clone(), content, true)
            }
            Ok(_) => mcp_resource_success(resource.clone(), (*empty_message).to_string(), false),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                mcp_resource_success(resource.clone(), (*empty_message).to_string(), false)
            }
            Err(err) => tool_failed(
                format!("读取 MCP resource `{}` 失败：{err}", resource.uri),
                "read_failed",
            ),
        },
    }
}

fn mcp_resource_success(
    resource: RuntimeMcpResource,
    raw_content: String,
    exists: bool,
) -> ToolResult {
    let (content, truncated) = truncate_text_with_flag(&raw_content, MCP_RESOURCE_MAX_CHARS);
    ToolResult {
        status: ToolResultStatus::Success,
        content: content.clone(),
        structured: Some(serde_json::json!({
            "server": resource.server,
            "uri": resource.uri,
            "name": resource.name,
            "mime_type": resource.mime_type,
            "read_only": true,
            "exists": exists,
            "truncated": truncated,
            "path": null,
            "resource_uri": resource.uri,
            "content": content,
        })),
    }
}

struct RuntimeSessionListPayload {
    exists: bool,
    sessions: Vec<serde_json::Value>,
}

async fn runtime_session_list_payload(
    state: &Arc<AppState>,
) -> Result<RuntimeSessionListPayload, String> {
    let virtual_path = state
        .runtime_service
        .data_dir()
        .join("sessions")
        .join("store.json");
    let repository = state
        .runtime_service
        .session_repository()
        .await
        .map_err(|error| format!("打开会话仓储失败：{error}"))?;
    let items = repository
        .list_sessions()
        .await
        .map_err(|error| format!("读取会话 SQLite 索引失败：{error}"))?;
    let existing_personas = {
        let personas = state.personas.lock().await;
        personas
            .personas()
            .iter()
            .map(|persona| persona.id.clone())
            .collect::<HashSet<_>>()
    };
    if items.is_empty() {
        return Ok(RuntimeSessionListPayload {
            exists: false,
            sessions: runtime_default_session_list(&virtual_path, false),
        });
    }
    Ok(RuntimeSessionListPayload {
        exists: true,
        sessions: items
            .into_iter()
            .map(|item| {
                let persona_exists = existing_personas.contains(&item.persona_id);
                serde_json::json!({
                    "conversation_id": item.conversation_id,
                    "persona_id": item.persona_id,
                    "persona_name_snapshot": item.persona_name_snapshot,
                    "persona_version_snapshot": item.persona_version_snapshot,
                    "persona_status": if persona_exists { "bound" } else { "missing" },
                    "summary": item.summary,
                    "first_prompt": null,
                    "source_conversation_id": item.source_conversation_id,
                    "path": null,
                    "resource_uri": MCP_RUNTIME_TRANSCRIPT_URI,
                    "exists": true,
                    "can_resume": item.records > 0 && persona_exists,
                    "records": item.records,
                    "created_time": item.created_time,
                    "last_time": item.last_time,
                    "archived": item.archived,
                    "metadata_updated_at": item.metadata_updated_at,
                })
            })
            .collect(),
    })
}

/// 抓取一页公开文本网页，并在每次跳转后重新执行 SSRF 边界校验。
async fn tool_web_fetch(call: &ToolCall) -> ToolResult {
    let Some(value) = tool_arg_string(&call.arguments, "url") else {
        return tool_failed("web_fetch 缺少 url 参数。", "missing_url");
    };
    let mut target = match validate_public_https_url(&value).await {
        Ok(target) => target,
        Err(err) => return tool_failed(err, "blocked_url"),
    };
    for redirect_count in 0..=WEB_FETCH_MAX_REDIRECTS {
        let client =
            match build_pinned_https_client(&target, Duration::from_secs(WEB_FETCH_TIMEOUT_SECS)) {
                Ok(client) => client,
                Err(err) => return tool_failed(err, "client_failed"),
            };
        let response = match client.get(target.url.clone()).send().await {
            Ok(response) => response,
            Err(err) => return tool_failed(format!("联网请求失败：{err}"), "request_failed"),
        };
        if response.status().is_redirection() {
            if redirect_count == WEB_FETCH_MAX_REDIRECTS {
                return tool_failed("网页跳转次数超过上限。", "too_many_redirects");
            }
            let Some(location) = response.headers().get(reqwest::header::LOCATION) else {
                return tool_failed("网页跳转响应缺少 Location。", "invalid_redirect");
            };
            let location = match location.to_str() {
                Ok(value) => value,
                Err(_) => return tool_failed("网页跳转地址不是有效文本。", "invalid_redirect"),
            };
            let next = match target.url.join(location) {
                Ok(next) => next,
                Err(err) => {
                    return tool_failed(format!("网页跳转地址无效：{err}"), "invalid_redirect");
                }
            };
            target = match validate_public_https_url(next.as_str()).await {
                Ok(target) => target,
                Err(err) => return tool_failed(err, "blocked_redirect"),
            };
            continue;
        }
        let status = response.status().as_u16();
        if !(200..300).contains(&status) {
            return tool_failed(format!("网页请求失败，HTTP {status}。"), "http_failed");
        }
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .to_ascii_lowercase();
        if !(content_type.is_empty()
            || content_type.starts_with("text/")
            || content_type.starts_with("application/json")
            || content_type.contains("xml"))
        {
            return tool_failed(
                "网页响应不是允许读取的文本类型。",
                "unsupported_content_type",
            );
        }
        let text = match read_bounded_web_response(response).await {
            Ok(text) => text,
            Err(err) => return tool_failed(err, "body_failed"),
        };
        return ToolResult::success(
            format!("【不可信外部网页内容】\n{}", truncate_text(&text, 40_000)),
            Some(serde_json::json!({
                "url": target.url.as_str(),
                "status": status,
                "content_type": content_type,
                "trust_boundary": "untrusted_external_content",
            })),
        );
    }
    tool_failed("网页跳转处理异常结束。", "redirect_failed")
}
/// 构造未配置搜索凭据时的稳定、可操作失败结果。
fn web_search_not_configured_result() -> ToolResult {
    tool_failed(
        "网页搜索尚未配置 Brave Search API 密钥。请在 Muse 设置中心的“联网搜索”中配置，或在首次启动前设置 BRAVE_SEARCH_API_KEY。",
        "search_not_configured",
    )
}

/// 使用 Brave Search API 执行结构化公开网页搜索。
async fn tool_web_search(state: &Arc<AppState>, call: &ToolCall) -> ToolResult {
    let Some(query) = tool_arg_string(&call.arguments, "query") else {
        return tool_failed("web_search 缺少 query 参数。", "missing_query");
    };
    let api_key = match state.secrets.get_optional("web-search.brave") {
        Ok(Some(key)) => key,
        Ok(None) => return web_search_not_configured_result(),
        Err(err) => {
            return tool_failed(
                format!("无法读取网页搜索凭据：{err}"),
                "secret_store_failed",
            );
        }
    };
    let limit = tool_arg_limit(&call.arguments, 5, 10);
    let target =
        match validate_public_https_url("https://api.search.brave.com/res/v1/web/search").await {
            Ok(target) => target,
            Err(err) => return tool_failed(format!("搜索服务地址校验失败：{err}"), "blocked_url"),
        };
    let client =
        match build_pinned_https_client(&target, Duration::from_secs(WEB_FETCH_TIMEOUT_SECS)) {
            Ok(client) => client,
            Err(err) => return tool_failed(format!("无法创建搜索客户端：{err}"), "client_failed"),
        };
    let response = match client
        .get(target.url)
        .header("Accept", "application/json")
        .header("X-Subscription-Token", api_key)
        .query(&[("q", query.as_str()), ("count", &limit.to_string())])
        .send()
        .await
    {
        Ok(response) => response,
        Err(err) => return tool_failed(format!("网页搜索请求失败：{err}"), "request_failed"),
    };
    let status = response.status().as_u16();
    let body = match read_bounded_web_response(response).await {
        Ok(body) => body,
        Err(err) => return tool_failed(err, "read_failed"),
    };
    if !(200..300).contains(&status) {
        return tool_failed(format!("网页搜索请求失败，HTTP {status}。"), "http_failed");
    }
    let payload: serde_json::Value = match serde_json::from_str(&body) {
        Ok(payload) => payload,
        Err(err) => return tool_failed(format!("搜索响应不是有效 JSON：{err}"), "invalid_json"),
    };
    let results = payload
        .pointer("/web/results")
        .and_then(serde_json::Value::as_array)
        .map(|items| {
            items
                .iter()
                .take(limit)
                .map(|item| {
                    serde_json::json!({
                        "title": item.get("title").and_then(serde_json::Value::as_str).unwrap_or(""),
                        "url": item.get("url").and_then(serde_json::Value::as_str).unwrap_or(""),
                        "snippet": item.get("description").and_then(serde_json::Value::as_str).unwrap_or(""),
                        "age": item.get("age").and_then(serde_json::Value::as_str),
                    })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    ToolResult::success(
        format!("网页搜索 `{query}` 返回 {} 条结果。", results.len()),
        Some(serde_json::json!({
            "query": query,
            "provider": "brave_search_api",
            "results": results,
            "trust_boundary": "untrusted_external_content",
        })),
    )
}

const LOCAL_MCP_SERVER_NAME: &str = "muse-local";
const LEGACY_LOCAL_MCP_SERVER_NAME: &str = "agent-vp-local";
const MCP_CURRENT_CONVERSATION_URI: &str = "muse://conversation/current";
const MCP_RUNTIME_TRANSCRIPT_URI: &str = "muse://session/runtime-transcript";
const MCP_WORKSPACE_POLICY_URI: &str = "muse://harness/workspace-policy";
const MCP_RESOURCE_MAX_CHARS: usize = 40_000;

#[derive(Clone)]
enum RuntimeMcpResourceSource {
    CurrentConversation,
    VirtualTranscript,
    File {
        path: PathBuf,
        empty_message: &'static str,
    },
}

#[derive(Clone)]
struct RuntimeMcpResource {
    server: &'static str,
    uri: &'static str,
    name: &'static str,
    description: &'static str,
    mime_type: &'static str,
    source: RuntimeMcpResourceSource,
}

impl RuntimeMcpResource {
    fn exists(&self) -> bool {
        match &self.source {
            RuntimeMcpResourceSource::CurrentConversation
            | RuntimeMcpResourceSource::VirtualTranscript => true,
            RuntimeMcpResourceSource::File { path, .. } => path.exists(),
        }
    }

    fn info_json(&self) -> serde_json::Value {
        let source = if matches!(&self.source, RuntimeMcpResourceSource::VirtualTranscript) {
            "virtual_aggregate"
        } else {
            "local_mcp"
        };
        serde_json::json!({
            "server": self.server,
            "uri": self.uri,
            "name": self.name,
            "description": self.description,
            "mime_type": self.mime_type,
            "read_only": true,
            "external": false,
            "source": source,
            "exists": self.exists(),
            "path": null,
            "resource_uri": self.uri,
        })
    }
}

fn runtime_mcp_resources() -> Vec<RuntimeMcpResource> {
    vec![
        RuntimeMcpResource {
            server: LOCAL_MCP_SERVER_NAME,
            uri: MCP_CURRENT_CONVERSATION_URI,
            name: "当前内存会话",
            description: "当前运行时内存中的会话消息，用于恢复模型对最近上下文的判断。",
            mime_type: "application/json",
            source: RuntimeMcpResourceSource::CurrentConversation,
        },
        RuntimeMcpResource {
            server: LOCAL_MCP_SERVER_NAME,
            uri: MCP_RUNTIME_TRANSCRIPT_URI,
            name: "Runtime Transcript",
            description: "落盘的 runtime JSONL transcript，包含用户消息、工具调用和工具结果。",
            mime_type: "application/x-ndjson",
            source: RuntimeMcpResourceSource::VirtualTranscript,
        },
        RuntimeMcpResource {
            server: LOCAL_MCP_SERVER_NAME,
            uri: MCP_WORKSPACE_POLICY_URI,
            name: "工作区与权限策略",
            description: "当前 harness 工作区根、权限模式和沙箱模式配置。",
            mime_type: "application/json",
            source: RuntimeMcpResourceSource::File {
                path: custom_allowed_file_roots_path(),
                empty_message: "当前尚未写入自定义工作区策略，运行时使用默认审批模式和工作区写入沙箱。",
            },
        },
    ]
}

fn validate_mcp_server_argument(call: &ToolCall) -> Result<(), ToolResult> {
    let Some(server) = tool_arg_string(&call.arguments, "server") else {
        return Ok(());
    };
    if is_local_mcp_server(&server) || is_valid_external_mcp_server_name(&server) {
        Ok(())
    } else {
        Err(tool_failed(
            format!(
                "{} 的 server `{server}` 无效。server 只能使用字母、数字、下划线和短横线；本地 server 为 `{LOCAL_MCP_SERVER_NAME}`。",
                call.name
            ),
            "invalid_server",
        ))
    }
}

fn is_local_mcp_server(server: &str) -> bool {
    server == LOCAL_MCP_SERVER_NAME || server == LEGACY_LOCAL_MCP_SERVER_NAME
}

fn normalize_local_mcp_uri(uri: &str) -> &str {
    match uri {
        "agent-vp://conversation/current" => MCP_CURRENT_CONVERSATION_URI,
        "agent-vp://session/runtime-transcript" => MCP_RUNTIME_TRANSCRIPT_URI,
        "agent-vp://harness/workspace-policy" => MCP_WORKSPACE_POLICY_URI,
        _ => uri,
    }
}

fn is_valid_external_mcp_server_name(server: &str) -> bool {
    !server.is_empty()
        && server
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
}

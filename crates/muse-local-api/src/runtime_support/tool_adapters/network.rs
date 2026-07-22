//! 网页抓取、联网搜索与本地 MCP 资源描述适配。

use super::*;

/// 抓取一页公开文本网页，并在每次跳转后重新执行 SSRF 边界校验。
pub(in crate::runtime_support) async fn tool_web_fetch(call: &ToolCall) -> ToolResult {
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
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::runtime_support) enum TrustedSearchEndpoint {
    ExaFreeMcp,
    ExaApi,
}

impl TrustedSearchEndpoint {
    pub(in crate::runtime_support) fn url(self) -> &'static str {
        match self {
            Self::ExaFreeMcp => "https://mcp.exa.ai/mcp",
            Self::ExaApi => "https://api.exa.ai/search",
        }
    }

    pub(in crate::runtime_support) fn expected_host(self) -> &'static str {
        match self {
            Self::ExaFreeMcp => "mcp.exa.ai",
            Self::ExaApi => "api.exa.ai",
        }
    }
}

/// 为代码内固定的搜索供应商建立客户端，不把模型输入或用户 URL 接入此路径。
///
/// 可信端点仍由 HTTPS/TLS 校验域名，并禁用环境代理与自动重定向；这里只是不预解析
/// 和固定 IP，让 macOS VPN/TUN 能处理 Fake-IP 映射。任意网页仍必须走
/// `validate_public_https_url` 与 `build_pinned_https_client`。
pub(in crate::runtime_support) fn build_trusted_search_client(
    endpoint: TrustedSearchEndpoint,
    timeout: Duration,
) -> Result<(reqwest::Client, reqwest::Url), String> {
    let url = reqwest::Url::parse(endpoint.url())
        .map_err(|err| format!("可信搜索服务地址无效：{err}"))?;
    if url.scheme() != "https"
        || url.host_str() != Some(endpoint.expected_host())
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err("可信搜索服务地址不符合固定 HTTPS 端点约束。".to_string());
    }
    let builder = reqwest::Client::builder()
        .timeout(timeout)
        .redirect(reqwest::redirect::Policy::none());
    let builder = match RESTRICTED_HTTPS_PROXY_POLICY {
        RestrictedHttpsProxyPolicy::Disabled => builder.no_proxy(),
    };
    let client = builder
        .build()
        .map_err(|err| format!("无法创建可信搜索客户端：{err}"))?;
    Ok((client, url))
}

/// 构造 API 模式未配置凭据时的稳定、可操作失败结果。
pub(in crate::runtime_support) fn web_search_not_configured_result() -> ToolResult {
    tool_failed(
        "当前选择了 Exa API Key 搜索，但尚未配置密钥。请在 Muse 设置中心的“联网搜索”中配置，或切换回默认的免费搜索。",
        "search_not_configured",
    )
}

/// 按用户配置选择 Exa 免费 MCP 或正式 Search API。
pub(in crate::runtime_support) async fn tool_web_search(
    state: &Arc<AppState>,
    call: &ToolCall,
) -> ToolResult {
    use muse_core::app::preferences::WebSearchProvider;

    let Some(query) = tool_arg_string(&call.arguments, "query") else {
        return tool_failed("web_search 缺少 query 参数。", "missing_query");
    };
    let (provider, api_key) = {
        // 配置切换和凭据发布共用同一 transition gate，单次调用只观察完整快照。
        let _transition = state.model_configuration_transition_gate.lock().await;
        let provider = state
            .user_config
            .lock()
            .await
            .web_search_preferences()
            .provider;
        let api_key = if provider == WebSearchProvider::ExaApi {
            match state.secrets.get_optional("web-search.exa") {
                Ok(value) => value,
                Err(err) => {
                    return tool_failed(
                        format!("无法读取网页搜索凭据：{err}"),
                        "secret_store_failed",
                    );
                }
            }
        } else {
            None
        };
        (provider, api_key)
    };
    let limit = tool_arg_limit(&call.arguments, 5, 10);

    match provider {
        WebSearchProvider::ExaFreeMcp => exa_free_mcp_search(&query, limit).await,
        WebSearchProvider::ExaApi => {
            let Some(api_key) = api_key else {
                return web_search_not_configured_result();
            };
            exa_api_search(&query, limit, &api_key).await
        }
    }
}

pub(in crate::runtime_support) async fn exa_free_mcp_search(
    query: &str,
    limit: usize,
) -> ToolResult {
    let (client, url) = match build_trusted_search_client(
        TrustedSearchEndpoint::ExaFreeMcp,
        Duration::from_secs(WEB_FETCH_TIMEOUT_SECS),
    ) {
        Ok(target) => target,
        Err(err) => return tool_failed(format!("无法创建搜索客户端：{err}"), "client_failed"),
    };
    let response = match client
        .post(url)
        .header("Accept", "application/json, text/event-stream")
        .json(&exa_free_mcp_request(query, limit))
        .send()
        .await
    {
        Ok(response) => response,
        Err(err) => return tool_failed(format!("免费搜索请求失败：{err}"), "request_failed"),
    };
    let status = response.status().as_u16();
    let body = match read_bounded_web_response(response).await {
        Ok(body) => body,
        Err(err) => return tool_failed(err, "read_failed"),
    };
    if !(200..300).contains(&status) {
        return exa_search_http_failure(status, true);
    }
    let content = match parse_exa_mcp_search_response(&body) {
        Ok(content) => content,
        Err(err) => {
            let normalized = err.to_ascii_lowercase();
            if normalized.contains("rate limit")
                || normalized.contains("rate-limit")
                || normalized.contains("429")
            {
                return exa_search_http_failure(429, true);
            }
            return tool_failed(err, "invalid_mcp_response");
        }
    };
    ToolResult::success(
        format!(
            "【不可信外部搜索结果】\n{}",
            truncate_text(&content, 40_000)
        ),
        Some(serde_json::json!({
            "query": query,
            "provider": "exa_free_mcp",
            "trust_boundary": "untrusted_external_content",
        })),
    )
}

pub(in crate::runtime_support) async fn exa_api_search(
    query: &str,
    limit: usize,
    api_key: &str,
) -> ToolResult {
    let (client, url) = match build_trusted_search_client(
        TrustedSearchEndpoint::ExaApi,
        Duration::from_secs(WEB_FETCH_TIMEOUT_SECS),
    ) {
        Ok(target) => target,
        Err(err) => return tool_failed(format!("无法创建搜索客户端：{err}"), "client_failed"),
    };
    let response = match client
        .post(url)
        .header("Accept", "application/json")
        .header("x-api-key", api_key)
        .json(&exa_api_search_request(query, limit))
        .send()
        .await
    {
        Ok(response) => response,
        Err(err) => return tool_failed(format!("Exa API 请求失败：{err}"), "request_failed"),
    };
    let status = response.status().as_u16();
    let body = match read_bounded_web_response(response).await {
        Ok(body) => body,
        Err(err) => return tool_failed(err, "read_failed"),
    };
    if !(200..300).contains(&status) {
        return exa_search_http_failure(status, false);
    }
    let payload: serde_json::Value = match serde_json::from_str(&body) {
        Ok(payload) => payload,
        Err(err) => {
            return tool_failed(format!("Exa API 响应不是有效 JSON：{err}"), "invalid_json");
        }
    };
    let results = normalize_exa_api_results(&payload, limit);
    let readable = results
        .iter()
        .enumerate()
        .map(|(index, item)| {
            format!(
                "{}. {}\nURL: {}\n摘要: {}",
                index + 1,
                item.get("title")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or(""),
                item.get("url")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or(""),
                item.get("snippet")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    ToolResult::success(
        format!(
            "【不可信外部搜索结果】\n网页搜索 `{query}` 返回 {} 条结果。\n\n{}",
            results.len(),
            truncate_text(&readable, 40_000)
        ),
        Some(serde_json::json!({
            "query": query,
            "provider": "exa_api",
            "results": results,
            "request_id": payload.get("requestId").cloned(),
            "cost_dollars": payload.get("costDollars").cloned(),
            "trust_boundary": "untrusted_external_content",
        })),
    )
}

pub(in crate::runtime_support) fn exa_free_mcp_request(
    query: &str,
    limit: usize,
) -> serde_json::Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": "web_search_exa",
            "arguments": {
                "query": query,
                "type": "auto",
                "numResults": limit,
                "livecrawl": "fallback",
                "contextMaxCharacters": 20_000
            }
        }
    })
}

pub(in crate::runtime_support) fn exa_api_search_request(
    query: &str,
    limit: usize,
) -> serde_json::Value {
    serde_json::json!({
        "query": query,
        "numResults": limit,
        "type": "auto",
        "contents": { "highlights": true }
    })
}

pub(in crate::runtime_support) fn normalize_exa_api_results(
    payload: &serde_json::Value,
    limit: usize,
) -> Vec<serde_json::Value> {
    payload
        .get("results")
        .and_then(serde_json::Value::as_array)
        .map(|items| {
            items
                .iter()
                .take(limit)
                .map(|item| {
                    serde_json::json!({
                        "title": item.get("title").and_then(serde_json::Value::as_str).unwrap_or(""),
                        "url": item.get("url").and_then(serde_json::Value::as_str).unwrap_or(""),
                        "snippet": exa_api_result_snippet(item),
                        "published_date": item.get("publishedDate").and_then(serde_json::Value::as_str),
                        "author": item.get("author").and_then(serde_json::Value::as_str),
                    })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

pub(in crate::runtime_support) fn exa_api_result_snippet(item: &serde_json::Value) -> String {
    if let Some(highlights) = item.get("highlights").and_then(serde_json::Value::as_array) {
        let joined = highlights
            .iter()
            .filter_map(serde_json::Value::as_str)
            .collect::<Vec<_>>()
            .join(" … ");
        if !joined.is_empty() {
            return truncate_text(&joined, 1_500).to_string();
        }
    }
    item.get("text")
        .and_then(serde_json::Value::as_str)
        .map(|text| truncate_text(text, 1_500).to_string())
        .unwrap_or_default()
}

pub(in crate::runtime_support) fn exa_search_http_failure(
    status: u16,
    free_mcp: bool,
) -> ToolResult {
    match status {
        401 | 403 if !free_mcp => tool_failed(
            "Exa API Key 无效或无权执行搜索，请在设置中心更新密钥。",
            "search_auth_failed",
        ),
        402 if !free_mcp => tool_failed(
            "Exa API 账户额度不足或需要完成付款设置，请检查 Exa 控制台。",
            "search_quota_exhausted",
        ),
        429 if free_mcp => tool_failed(
            "Exa 免费搜索当前已达到公共限流，请稍后重试，或在设置中心切换到自己的 API Key。",
            "search_free_rate_limited",
        ),
        429 => tool_failed(
            "Exa API 当前已达到账号限流，请稍后重试并检查 Exa 控制台额度。",
            "search_rate_limited",
        ),
        500..=599 => tool_failed(
            format!("Exa 搜索服务暂时不可用，HTTP {status}。"),
            "search_service_unavailable",
        ),
        _ => tool_failed(format!("Exa 搜索请求失败，HTTP {status}。"), "http_failed"),
    }
}

pub(in crate::runtime_support) fn parse_exa_mcp_search_response(
    body: &str,
) -> Result<String, String> {
    let trimmed = body.trim();
    if trimmed.starts_with('{') {
        let value = serde_json::from_str::<serde_json::Value>(trimmed)
            .map_err(|err| format!("Exa MCP 响应不是有效 JSON：{err}"))?;
        if let Some(content) = exa_mcp_payload_text(&value)? {
            return Ok(content);
        }
    }
    for line in body.lines() {
        let Some(payload) = line.strip_prefix("data:") else {
            continue;
        };
        let value: serde_json::Value = serde_json::from_str(payload.trim())
            .map_err(|err| format!("Exa MCP 的 SSE 数据不是有效 JSON：{err}"))?;
        if let Some(content) = exa_mcp_payload_text(&value)? {
            return Ok(content);
        }
    }
    Err("Exa MCP 响应中没有可读取的搜索结果。".to_string())
}

pub(in crate::runtime_support) fn exa_mcp_payload_text(
    value: &serde_json::Value,
) -> Result<Option<String>, String> {
    if let Some(error) = value.get("error") {
        let message = error
            .get("message")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("Exa MCP 返回协议错误。");
        return Err(truncate_text(message, 1_000).to_string());
    }
    let Some(result) = value.get("result") else {
        return Ok(None);
    };
    let text = result
        .get("content")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| {
            (item.get("type").and_then(serde_json::Value::as_str) == Some("text"))
                .then(|| item.get("text").and_then(serde_json::Value::as_str))
                .flatten()
        })
        .collect::<Vec<_>>()
        .join("\n");
    if result
        .get("isError")
        .or_else(|| result.get("is_error"))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
    {
        return Err(if text.is_empty() {
            "Exa MCP 搜索执行失败。".to_string()
        } else {
            truncate_text(&text, 1_000).to_string()
        });
    }
    Ok((!text.is_empty()).then_some(text))
}

pub(in crate::runtime_support) const LOCAL_MCP_SERVER_NAME: &str = "muse-local";
pub(in crate::runtime_support) const LEGACY_LOCAL_MCP_SERVER_NAME: &str = "agent-vp-local";
pub(in crate::runtime_support) const MCP_CURRENT_CONVERSATION_URI: &str =
    "muse://conversation/current";
pub(in crate::runtime_support) const MCP_RUNTIME_TRANSCRIPT_URI: &str =
    "muse://session/runtime-transcript";
pub(in crate::runtime_support) const MCP_WORKSPACE_POLICY_URI: &str =
    "muse://harness/workspace-policy";
pub(in crate::runtime_support) const MCP_RESOURCE_MAX_CHARS: usize = 40_000;

#[derive(Clone)]
pub(in crate::runtime_support) enum RuntimeMcpResourceSource {
    CurrentConversation,
    VirtualTranscript,
    File {
        path: PathBuf,
        empty_message: &'static str,
    },
}

#[derive(Clone)]
pub(in crate::runtime_support) struct RuntimeMcpResource {
    pub(in crate::runtime_support) server: &'static str,
    pub(in crate::runtime_support) uri: &'static str,
    pub(in crate::runtime_support) name: &'static str,
    pub(in crate::runtime_support) description: &'static str,
    pub(in crate::runtime_support) mime_type: &'static str,
    pub(in crate::runtime_support) source: RuntimeMcpResourceSource,
}

impl RuntimeMcpResource {
    pub(in crate::runtime_support) fn exists(&self) -> bool {
        match &self.source {
            RuntimeMcpResourceSource::CurrentConversation
            | RuntimeMcpResourceSource::VirtualTranscript => true,
            RuntimeMcpResourceSource::File { path, .. } => path.exists(),
        }
    }

    pub(in crate::runtime_support) fn info_json(&self) -> serde_json::Value {
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

pub(in crate::runtime_support) fn runtime_mcp_resources() -> Vec<RuntimeMcpResource> {
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

pub(in crate::runtime_support) fn validate_mcp_server_argument(
    call: &ToolCall,
) -> Result<(), ToolResult> {
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

pub(in crate::runtime_support) fn is_local_mcp_server(server: &str) -> bool {
    server == LOCAL_MCP_SERVER_NAME || server == LEGACY_LOCAL_MCP_SERVER_NAME
}

pub(in crate::runtime_support) fn normalize_local_mcp_uri(uri: &str) -> &str {
    match uri {
        "agent-vp://conversation/current" => MCP_CURRENT_CONVERSATION_URI,
        "agent-vp://session/runtime-transcript" => MCP_RUNTIME_TRANSCRIPT_URI,
        "agent-vp://harness/workspace-policy" => MCP_WORKSPACE_POLICY_URI,
        _ => uri,
    }
}

pub(in crate::runtime_support) fn is_valid_external_mcp_server_name(server: &str) -> bool {
    !server.is_empty()
        && server
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
}

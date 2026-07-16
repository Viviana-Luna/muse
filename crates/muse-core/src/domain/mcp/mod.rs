//! MCP 客户端与动态工具目录模块，负责外部 MCP 服务发现、资源读取和工具调用。

pub mod config;
pub mod migration;
mod store;

pub use config::{McpProfileConfig, McpRuntimeSnapshot, McpServerProfile};

use crate::domain::tool::{ToolDef, ToolExecutionOwner, ToolRisk};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use reqwest::header::{ACCEPT, CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue};
use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::process::Stdio;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::fs;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

const DEFAULT_REQUEST_TIMEOUT_MS: u64 = 30_000;
const MAX_REQUEST_TIMEOUT_MS: u64 = 120_000;
const MCP_PROTOCOL_VERSION: &str = "2025-06-18";
const MCP_PROTOCOL_VERSION_HEADER: &str = "mcp-protocol-version";
const MCP_SESSION_ID_HEADER: &str = "mcp-session-id";
const MCP_RESOURCE_TEXT_PREVIEW_CHARS: usize = 40_000;
const MCP_TOOL_TEXT_PREVIEW_CHARS: usize = 40_000;

/// 外部 MCP 资源列表结果。
#[derive(Debug, Clone)]
pub struct ExternalMcpResources {
    pub resources: Vec<Value>,
    pub errors: Vec<Value>,
    pub next_cursor: Option<String>,
    pub refreshed_at: String,
    pub config_path: PathBuf,
}

/// 外部 MCP 资源模板列表结果。
#[derive(Debug, Clone)]
pub struct ExternalMcpResourceTemplates {
    pub resource_templates: Vec<Value>,
    pub errors: Vec<Value>,
    pub next_cursor: Option<String>,
    pub refreshed_at: String,
    pub config_path: PathBuf,
}

/// 外部 MCP 资源读取结果。
#[derive(Debug, Clone)]
pub struct ExternalMcpReadResource {
    pub content: String,
    pub structured: Value,
}

/// 每轮可见的外部 MCP 动态工具目录。
#[derive(Clone)]
pub struct McpToolCatalog {
    pub tools: Vec<ExternalMcpToolDef>,
    pub errors: Vec<Value>,
    pub refreshed_at: String,
    pub refreshed_at_millis: u128,
    pub config_path: PathBuf,
    pub config_hash: String,
    // 与工具定义同时发现的不可变执行目标。当前回合调用 MCP 时只能使用这里的
    // server 配置，不能重新读取磁盘后把同名工具切换到未经审批的新 URL/命令。
    server_configs: Vec<ExternalMcpServerConfig>,
}

impl McpToolCatalog {
    /// 创建空的 MCP 工具目录，用于配置缺失或发现失败时的安全兜底。
    pub fn empty() -> Self {
        Self::empty_for_config_path(external_mcp_config_path())
    }

    /// 使用调用方已经解析的稳定数据目录创建空目录，测试和桌面启动不得再次解析环境。
    pub fn empty_for_config_path(config_path: PathBuf) -> Self {
        Self {
            tools: Vec::new(),
            errors: Vec::new(),
            refreshed_at: chrono::Local::now().to_rfc3339(),
            refreshed_at_millis: current_millis(),
            config_path,
            config_hash: String::new(),
            server_configs: Vec::new(),
        }
    }

    /// 判断缓存目录是否仍匹配当前配置哈希且未超过有效期。
    pub fn is_fresh(&self, config_hash: &str, ttl: Duration) -> bool {
        if self.config_hash != config_hash {
            return false;
        }
        current_millis().saturating_sub(self.refreshed_at_millis) <= ttl.as_millis()
    }

    /// 将外部 MCP 工具目录转换为本轮可暴露给模型的工具定义。
    pub fn tool_defs(&self) -> Vec<ToolDef> {
        self.tools
            .iter()
            .map(ExternalMcpToolDef::to_tool_def)
            .collect()
    }

    /// 按完全限定工具名查找外部 MCP 工具定义。
    pub fn find_tool(&self, name: &str) -> Option<ExternalMcpToolDef> {
        self.tools.iter().find(|tool| tool.name == name).cloned()
    }

    /// 使用本目录冻结的 server 配置调用工具。
    pub async fn call_tool(
        &self,
        tool: &ExternalMcpToolDef,
        arguments: Value,
    ) -> Result<ExternalMcpToolCallResult, String> {
        let server_config = select_single_server(&self.server_configs, &tool.server_name)?;
        call_external_mcp_tool_with_server(server_config, tool, arguments).await
    }

    /// 使用本目录冻结的 server 配置列出资源。
    pub async fn list_resources(
        &self,
        server: Option<String>,
        cursor: Option<String>,
    ) -> Result<ExternalMcpResources, String> {
        list_external_mcp_resources_with_servers(
            &self.server_configs,
            self.config_path.clone(),
            server,
            cursor,
        )
        .await
    }

    /// 使用本目录冻结的 server 配置列出资源模板。
    pub async fn list_resource_templates(
        &self,
        server: Option<String>,
        cursor: Option<String>,
    ) -> Result<ExternalMcpResourceTemplates, String> {
        list_external_mcp_resource_templates_with_servers(
            &self.server_configs,
            self.config_path.clone(),
            server,
            cursor,
        )
        .await
    }

    /// 使用本目录冻结的 server 配置读取资源。
    pub async fn read_resource(
        &self,
        server: String,
        uri: String,
    ) -> Result<ExternalMcpReadResource, String> {
        let server_config = select_single_server(&self.server_configs, &server)?;
        read_external_mcp_resource_with_server(server_config, uri).await
    }
}

/// 外部 MCP 工具定义。`name` 固定使用 `mcp__server__tool` 形式暴露给模型。
#[derive(Debug, Clone)]
pub struct ExternalMcpToolDef {
    pub name: String,
    pub server_name: String,
    pub original_tool_name: String,
    pub description: String,
    pub parameters: Value,
    pub read_only: bool,
    pub annotations: Value,
}

impl ExternalMcpToolDef {
    /// 转换为运行底座统一工具定义，保留 MCP 工具风险和参数结构。
    pub fn to_tool_def(&self) -> ToolDef {
        // MCP annotations 来自远端服务，不能作为本地授权依据。未建立本地信任
        // 配置前，所有外部 MCP 调用都按可产生副作用处理并要求用户确认。
        let risk = ToolRisk::ExternalSideEffect;
        ToolDef {
            name: self.name.clone(),
            description: self.description.clone(),
            parameters: self.parameters.clone(),
            category: format!("mcp:{}", self.server_name),
            requires_approval: true,
            execution_owner: ToolExecutionOwner::ExternalProvider,
            available: true,
            disabled_reason: None,
            risk,
        }
    }
}

/// 外部 MCP 工具调用的标准化返回。
#[derive(Debug, Clone)]
pub struct ExternalMcpToolCallResult {
    pub content: String,
    pub structured: Value,
    pub is_error: bool,
}

// 已加载的外部 MCP 配置，保留原始文本用于缓存失效判断。
#[derive(Debug, Clone)]
struct LoadedExternalMcpConfig {
    servers: Vec<ExternalMcpServerConfig>,
    config_path: PathBuf,
    raw_content: String,
}

impl LoadedExternalMcpConfig {
    // 计算配置内容和配置路径共同决定的哈希。
    fn config_hash(&self) -> String {
        let mut hasher = DefaultHasher::new();
        self.raw_content.hash(&mut hasher);
        self.config_path.display().to_string().hash(&mut hasher);
        format!("{:016x}", hasher.finish())
    }
}

// 单个外部 MCP server 的标准化配置。
#[derive(Debug, Clone)]
struct ExternalMcpServerConfig {
    name: String,
    enabled: bool,
    request_timeout_ms: Option<u64>,
    enabled_tools: Option<Vec<String>>,
    disabled_tools: Vec<String>,
    transport: ExternalMcpTransport,
}

// v1 支持的外部 MCP 传输类型。
#[derive(Debug, Clone)]
enum ExternalMcpTransport {
    StreamableHttp {
        url: String,
        headers: BTreeMap<String, String>,
    },
    Stdio {
        command: String,
        args: Vec<String>,
        env: BTreeMap<String, String>,
        cwd: Option<String>,
    },
}

// JSON-RPC 响应的最小解析结构。
#[derive(Debug, Deserialize)]
struct JsonRpcResponse {
    #[serde(default)]
    id: Option<Value>,
    #[serde(default)]
    result: Option<Value>,
    #[serde(default)]
    error: Option<JsonRpcError>,
}

// JSON-RPC 错误对象。
#[derive(Debug, Deserialize)]
struct JsonRpcError {
    code: i64,
    message: String,
}

// HTTP 调用后保留响应体和可选 MCP session id。
struct HttpJsonRpcResponse {
    text: String,
    session_id: Option<String>,
}

impl ExternalMcpServerConfig {
    // 判断工具是否通过 enabled/disabled 过滤规则。
    fn allows_tool(&self, tool_name: &str) -> bool {
        if let Some(enabled_tools) = &self.enabled_tools
            && !enabled_tools.iter().any(|item| item == tool_name)
        {
            return false;
        }
        !self.disabled_tools.iter().any(|item| item == tool_name)
    }
}

/// 返回当前唯一 MCP 配置入口路径。
pub fn external_mcp_config_path() -> PathBuf {
    crate::config::Config::config_dir().join("config.toml")
}

/// 列出所有启用 MCP 服务暴露的资源。
pub async fn list_external_mcp_resources(
    snapshot: &McpRuntimeSnapshot,
    server: Option<String>,
    cursor: Option<String>,
) -> Result<ExternalMcpResources, String> {
    let loaded = load_enabled_servers(snapshot)?;
    list_external_mcp_resources_with_servers(&loaded.servers, loaded.config_path, server, cursor)
        .await
}

async fn list_external_mcp_resources_with_servers(
    servers: &[ExternalMcpServerConfig],
    config_path: PathBuf,
    server: Option<String>,
    cursor: Option<String>,
) -> Result<ExternalMcpResources, String> {
    let refreshed_at = chrono::Local::now().to_rfc3339();
    let mut resources = Vec::new();
    let mut errors = Vec::new();
    let mut next_cursor = None;

    let targets = select_servers(servers, server.as_deref())?;
    if targets.is_empty() {
        return Ok(ExternalMcpResources {
            resources,
            errors,
            next_cursor,
            refreshed_at,
            config_path,
        });
    }

    if cursor.is_some() && targets.len() != 1 {
        return Err("cursor 只能在指定单个外部 MCP server 时使用。".to_string());
    }

    let target_count = targets.len();
    for server_config in targets {
        let params = match cursor.as_ref() {
            Some(value) => serde_json::json!({ "cursor": value }),
            None => serde_json::json!({}),
        };
        match call_external_mcp_method(server_config, "resources/list", Some(params)).await {
            Ok(result) => {
                if target_count == 1 {
                    next_cursor = result
                        .get("nextCursor")
                        .or_else(|| result.get("next_cursor"))
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned);
                }
                resources.extend(normalize_resource_entries(&server_config.name, &result));
            }
            Err(err) => errors.push(external_error_json(&server_config.name, err)),
        }
    }

    Ok(ExternalMcpResources {
        resources,
        errors,
        next_cursor,
        refreshed_at,
        config_path,
    })
}

/// 列出所有启用 MCP 服务暴露的资源模板。
pub async fn list_external_mcp_resource_templates(
    snapshot: &McpRuntimeSnapshot,
    server: Option<String>,
    cursor: Option<String>,
) -> Result<ExternalMcpResourceTemplates, String> {
    let loaded = load_enabled_servers(snapshot)?;
    list_external_mcp_resource_templates_with_servers(
        &loaded.servers,
        loaded.config_path,
        server,
        cursor,
    )
    .await
}

async fn list_external_mcp_resource_templates_with_servers(
    servers: &[ExternalMcpServerConfig],
    config_path: PathBuf,
    server: Option<String>,
    cursor: Option<String>,
) -> Result<ExternalMcpResourceTemplates, String> {
    let refreshed_at = chrono::Local::now().to_rfc3339();
    let mut resource_templates = Vec::new();
    let mut errors = Vec::new();
    let mut next_cursor = None;

    let targets = select_servers(servers, server.as_deref())?;
    if targets.is_empty() {
        return Ok(ExternalMcpResourceTemplates {
            resource_templates,
            errors,
            next_cursor,
            refreshed_at,
            config_path,
        });
    }

    if cursor.is_some() && targets.len() != 1 {
        return Err("cursor 只能在指定单个外部 MCP server 时使用。".to_string());
    }

    let target_count = targets.len();
    for server_config in targets {
        let params = match cursor.as_ref() {
            Some(value) => serde_json::json!({ "cursor": value }),
            None => serde_json::json!({}),
        };
        match call_external_mcp_method(server_config, "resources/templates/list", Some(params))
            .await
        {
            Ok(result) => {
                if target_count == 1 {
                    next_cursor = result
                        .get("nextCursor")
                        .or_else(|| result.get("next_cursor"))
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned);
                }
                resource_templates.extend(normalize_resource_template_entries(
                    &server_config.name,
                    &result,
                ));
            }
            Err(err) => errors.push(external_error_json(&server_config.name, err)),
        }
    }

    Ok(ExternalMcpResourceTemplates {
        resource_templates,
        errors,
        next_cursor,
        refreshed_at,
        config_path,
    })
}

/// 读取指定 MCP 服务上的资源内容。
pub async fn read_external_mcp_resource(
    snapshot: &McpRuntimeSnapshot,
    server: String,
    uri: String,
) -> Result<ExternalMcpReadResource, String> {
    let loaded = load_enabled_servers(snapshot)?;
    let server_config = select_single_server(&loaded.servers, &server)?;
    read_external_mcp_resource_with_server(server_config, uri).await
}

async fn read_external_mcp_resource_with_server(
    server_config: &ExternalMcpServerConfig,
    uri: String,
) -> Result<ExternalMcpReadResource, String> {
    let result = call_external_mcp_method(
        server_config,
        "resources/read",
        Some(serde_json::json!({ "uri": uri })),
    )
    .await?;
    normalize_read_resource(&server_config.name, &uri, result).await
}

/// 发现所有启用 MCP 服务暴露的工具，并隔离单个服务失败。
pub async fn discover_external_mcp_tools(snapshot: &McpRuntimeSnapshot) -> McpToolCatalog {
    let refreshed_at = chrono::Local::now().to_rfc3339();
    let refreshed_at_millis = current_millis();
    let loaded = match load_enabled_servers(snapshot) {
        Ok(loaded) => loaded,
        Err(err) => {
            let mut catalog = McpToolCatalog {
                tools: Vec::new(),
                errors: vec![external_error_json("config", err.clone())],
                refreshed_at,
                refreshed_at_millis,
                config_path: snapshot.config_path().clone(),
                config_hash: format!("error:{err}"),
                server_configs: Vec::new(),
            };
            catalog.tools.sort_by(|a, b| a.name.cmp(&b.name));
            return catalog;
        }
    };
    let config_hash = loaded.config_hash();
    let mut catalog = McpToolCatalog {
        tools: Vec::new(),
        errors: Vec::new(),
        refreshed_at,
        refreshed_at_millis,
        config_path: loaded.config_path.clone(),
        config_hash,
        server_configs: loaded.servers.clone(),
    };

    for server_config in &loaded.servers {
        match call_external_mcp_method(server_config, "tools/list", Some(serde_json::json!({})))
            .await
        {
            Ok(result) => {
                let (mut tools, mut errors) = normalize_tool_entries(&server_config.name, &result);
                tools.retain(|tool| server_config.allows_tool(&tool.original_tool_name));
                catalog.tools.append(&mut tools);
                catalog.errors.append(&mut errors);
            }
            Err(err) => catalog
                .errors
                .push(external_error_json(&server_config.name, err)),
        }
    }
    catalog.tools.sort_by(|a, b| a.name.cmp(&b.name));
    catalog
}

/// 调用完全限定名对应的外部 MCP 工具。
pub async fn call_external_mcp_tool(
    snapshot: &McpRuntimeSnapshot,
    tool: &ExternalMcpToolDef,
    arguments: Value,
) -> Result<ExternalMcpToolCallResult, String> {
    let loaded = load_enabled_servers(snapshot)?;
    let server_config = select_single_server(&loaded.servers, &tool.server_name)?;
    call_external_mcp_tool_with_server(server_config, tool, arguments).await
}

async fn call_external_mcp_tool_with_server(
    server_config: &ExternalMcpServerConfig,
    tool: &ExternalMcpToolDef,
    arguments: Value,
) -> Result<ExternalMcpToolCallResult, String> {
    let result = call_external_mcp_method(
        server_config,
        "tools/call",
        Some(serde_json::json!({
            "name": tool.original_tool_name,
            "arguments": arguments
        })),
    )
    .await?;
    normalize_tool_call_result(tool, result).await
}

// 只从共享配置 Store 冻结的不可变快照加载；运行时不得重新读取磁盘或旧 JSON。
fn load_enabled_servers(snapshot: &McpRuntimeSnapshot) -> Result<LoadedExternalMcpConfig, String> {
    Ok(LoadedExternalMcpConfig {
        servers: parse_mcp_servers_map(snapshot.runtime_entries())?,
        config_path: snapshot.config_path().clone(),
        raw_content: snapshot.config_hash().to_string(),
    })
}

// 解析 mcp_servers 映射，并过滤未启用的 server。
fn parse_mcp_servers_map(
    entries: BTreeMap<String, Value>,
) -> Result<Vec<ExternalMcpServerConfig>, String> {
    let mut servers = Vec::new();
    for (name, raw) in entries {
        let server = parse_mcp_servers_entry(name, raw)?;
        validate_server_name(&server.name)?;
        if server.enabled {
            servers.push(server);
        }
    }
    Ok(servers)
}

// 解析单个 server 配置对象并区分 transport。
fn parse_mcp_servers_entry(name: String, raw: Value) -> Result<ExternalMcpServerConfig, String> {
    let raw_object = raw
        .as_object()
        .ok_or_else(|| format!("外部 MCP server `{name}` 配置必须是对象。"))?;
    let enabled = raw_object
        .get("enabled")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let request_timeout_ms = raw_object
        .get("request_timeout_ms")
        .or_else(|| raw_object.get("timeout_ms"))
        .or_else(|| raw_object.get("startup_timeout_ms"))
        .and_then(Value::as_u64)
        .or_else(|| {
            raw_object
                .get("request_timeout_ms")
                .or_else(|| raw_object.get("timeout_ms"))
                .or_else(|| raw_object.get("startup_timeout_ms"))
                .and_then(|value| value.as_str()?.trim().parse::<u64>().ok())
        })
        .or_else(|| timeout_sec_field(raw_object.get("tool_timeout_sec")))
        .or_else(|| timeout_sec_field(raw_object.get("startup_timeout_sec")));
    let enabled_tools =
        optional_string_array_field(raw_object.get("enabled_tools"), &name, "enabled_tools")?;
    let disabled_tools =
        string_array_field(raw_object.get("disabled_tools"), &name, "disabled_tools")?;
    let transport_type = raw_object
        .get("type")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .or_else(|| {
            if raw_object.contains_key("url") {
                Some("http".to_string())
            } else if raw_object.contains_key("command") {
                Some("stdio".to_string())
            } else {
                None
            }
        })
        .ok_or_else(|| {
            format!(
                "外部 MCP server `{name}` 缺少 type 字段；v1 支持 stdio、http、streamable_http。"
            )
        })?;

    let transport = match transport_type.as_str() {
        "http" | "streamable_http" | "http_json_rpc" => {
            reject_transport_fields(
                raw_object,
                &name,
                "http/streamable_http",
                &[
                    "command",
                    "args",
                    "env",
                    "env_vars",
                    "cwd",
                    "bearer_token",
                    "bearer_token_env",
                    "bearer_token_env_var",
                    "env_http_headers",
                ],
            )?;
            let mut headers = string_map_field(raw_object.get("headers"), &name, "headers")?;
            headers.extend(string_map_field(
                raw_object.get("http_headers"),
                &name,
                "http_headers",
            )?);
            ExternalMcpTransport::StreamableHttp {
                url: string_field(raw_object.get("url"), &name, "url")?,
                headers,
            }
        }
        "stdio" => {
            reject_transport_fields(
                raw_object,
                &name,
                "stdio",
                &[
                    "url",
                    "headers",
                    "http_headers",
                    "env_http_headers",
                    "bearer_token",
                    "bearer_token_env",
                    "bearer_token_env_var",
                    "env_vars",
                ],
            )?;
            ExternalMcpTransport::Stdio {
                command: string_field(raw_object.get("command"), &name, "command")?,
                args: string_array_field(raw_object.get("args"), &name, "args")?,
                env: string_map_field(raw_object.get("env"), &name, "env")?,
                cwd: raw_object
                    .get("cwd")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(ToOwned::to_owned),
            }
        }
        "sse" | "ws" | "sdk" => {
            return Err(format!(
                "外部 MCP server `{name}` 使用 transport `{transport_type}`，Muse v1 仅支持 stdio 和 http/streamable_http；SSE、WebSocket 与 SDK transport 需要后续版本实现。"
            ));
        }
        other => {
            return Err(format!(
                "外部 MCP server `{name}` 使用未知 transport `{other}`，Muse v1 仅支持 stdio 和 http/streamable_http。"
            ));
        }
    };

    Ok(ExternalMcpServerConfig {
        name,
        enabled,
        request_timeout_ms,
        enabled_tools,
        disabled_tools,
        transport,
    })
}

// 拒绝当前 transport 不应该出现的配置字段。
fn reject_transport_fields(
    raw_object: &serde_json::Map<String, Value>,
    server_name: &str,
    transport: &str,
    fields: &[&str],
) -> Result<(), String> {
    for field in fields {
        if raw_object.contains_key(*field) {
            return Err(format!(
                "外部 MCP server `{server_name}` 的 `{field}` 字段不适用于 {transport} transport。"
            ));
        }
    }
    Ok(())
}

// 校验 server 名称能安全拼进 MCP 工具完全限定名。
fn validate_server_name(name: &str) -> Result<(), String> {
    if is_valid_server_name(name) {
        return Ok(());
    }
    Err(format!(
        "外部 MCP server 名称 `{name}` 无效，只允许字母、数字、下划线和短横线。"
    ))
}

// 读取必填字符串字段。
fn string_field(value: Option<&Value>, server_name: &str, key: &str) -> Result<String, String> {
    value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| format!("外部 MCP server `{server_name}` 缺少 `{key}` 字符串字段。"))
}

// 读取字符串数组字段；缺省时返回空数组。
fn string_array_field(
    value: Option<&Value>,
    server_name: &str,
    key: &str,
) -> Result<Vec<String>, String> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let Some(items) = value.as_array() else {
        return Err(format!(
            "外部 MCP server `{server_name}` 的 `{key}` 必须是字符串数组。"
        ));
    };
    items
        .iter()
        .map(|item| {
            item.as_str().map(ToOwned::to_owned).ok_or_else(|| {
                format!("外部 MCP server `{server_name}` 的 `{key}` 只能包含字符串。")
            })
        })
        .collect()
}

// 读取可选字符串数组字段，用于 enabled_tools 这类三态配置。
fn optional_string_array_field(
    value: Option<&Value>,
    server_name: &str,
    key: &str,
) -> Result<Option<Vec<String>>, String> {
    value
        .map(|_| string_array_field(value, server_name, key))
        .transpose()
}

// 兼容秒级超时字段并转换为毫秒。
fn timeout_sec_field(value: Option<&Value>) -> Option<u64> {
    let seconds = value.and_then(|item| match item {
        Value::Number(number) => number.as_f64(),
        Value::String(text) => text.trim().parse::<f64>().ok(),
        _ => None,
    })?;
    if !seconds.is_finite() || seconds <= 0.0 {
        return None;
    }
    Some((seconds * 1000.0).round() as u64)
}

// 读取字符串键值对象字段，用于 header 和 env 配置。
fn string_map_field(
    value: Option<&Value>,
    server_name: &str,
    key: &str,
) -> Result<BTreeMap<String, String>, String> {
    let Some(value) = value else {
        return Ok(BTreeMap::new());
    };
    let Some(object) = value.as_object() else {
        return Err(format!(
            "外部 MCP server `{server_name}` 的 `{key}` 必须是字符串对象。"
        ));
    };
    let mut output = BTreeMap::new();
    for (item_key, item_value) in object {
        let Some(text) = item_value.as_str() else {
            return Err(format!(
                "外部 MCP server `{server_name}` 的 `{key}.{item_key}` 必须是字符串。"
            ));
        };
        output.insert(item_key.clone(), text.to_string());
    }
    Ok(output)
}

// 按可选 server 名称选择本次要调用的 server 集合。
fn select_servers<'a>(
    servers: &'a [ExternalMcpServerConfig],
    server: Option<&str>,
) -> Result<Vec<&'a ExternalMcpServerConfig>, String> {
    if let Some(server_name) = server {
        return Ok(vec![select_single_server(servers, server_name)?]);
    }
    Ok(servers.iter().collect())
}

// 选择单个 server，并在失败时给出当前可用列表。
fn select_single_server<'a>(
    servers: &'a [ExternalMcpServerConfig],
    server: &str,
) -> Result<&'a ExternalMcpServerConfig, String> {
    servers
        .iter()
        .find(|item| item.name == server)
        .ok_or_else(|| {
            let available = servers
                .iter()
                .map(|item| item.name.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            if available.is_empty() {
                format!("未配置外部 MCP server `{server}`。")
            } else {
                format!("未找到外部 MCP server `{server}`，当前可用外部 server：{available}。")
            }
        })
}

// 判断 server 名称是否只包含安全字符。
fn is_valid_server_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
}

/// 判断工具名是否属于外部 MCP 工具命名空间。
pub fn is_external_mcp_tool_name(name: &str) -> bool {
    name.strip_prefix("mcp__")
        .and_then(|rest| rest.split_once("__"))
        .is_some_and(|(server, tool)| !server.is_empty() && !tool.is_empty())
}

/// 生成暴露给模型的 MCP 完全限定工具名。
pub fn external_mcp_tool_name(server: &str, tool: &str) -> String {
    format!(
        "mcp__{}__{}",
        sanitize_tool_name_segment(server),
        sanitize_tool_name_segment(tool)
    )
}

// 清理工具名片段，保证能拼成稳定的 `mcp__server__tool`。
fn sanitize_tool_name_segment(value: &str) -> String {
    let mut output = String::new();
    let mut previous_was_separator = false;
    for ch in value.trim().chars() {
        let normalized = if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
            ch
        } else {
            '_'
        };
        if normalized == '_' && previous_was_separator {
            continue;
        }
        previous_was_separator = normalized == '_';
        output.push(normalized);
    }
    let output = output.trim_matches('_').to_string();
    if output.is_empty() {
        "tool".to_string()
    } else {
        output
    }
}

// 根据 server transport 分发一次完整 MCP JSON-RPC 调用。
async fn call_external_mcp_method(
    server: &ExternalMcpServerConfig,
    method: &str,
    params: Option<Value>,
) -> Result<Value, String> {
    let timeout_ms = server
        .request_timeout_ms
        .unwrap_or(DEFAULT_REQUEST_TIMEOUT_MS)
        .clamp(1_000, MAX_REQUEST_TIMEOUT_MS);
    match &server.transport {
        ExternalMcpTransport::StreamableHttp { url, headers } => {
            call_http_json_rpc(HttpJsonRpcRequest {
                server_name: &server.name,
                url,
                headers,
                method,
                params,
                timeout_ms,
            })
            .await
        }
        ExternalMcpTransport::Stdio {
            command,
            args,
            env,
            cwd,
        } => {
            call_stdio_json_rpc(StdioJsonRpcRequest {
                server_name: &server.name,
                command,
                args,
                env,
                cwd: cwd.as_deref(),
                method,
                params,
                timeout_ms,
            })
            .await
        }
    }
}

struct HttpJsonRpcRequest<'a> {
    server_name: &'a str,
    url: &'a str,
    headers: &'a BTreeMap<String, String>,
    method: &'a str,
    params: Option<Value>,
    timeout_ms: u64,
}

// 通过 streamable HTTP 完成 initialize、initialized 和目标方法调用。
async fn call_http_json_rpc(request: HttpJsonRpcRequest<'_>) -> Result<Value, String> {
    let HttpJsonRpcRequest {
        server_name,
        url,
        headers,
        method,
        params,
        timeout_ms,
    } = request;
    let client = reqwest::Client::new();
    let header_map = build_http_header_map(server_name, headers)?;
    let initialize_response = post_http_json_rpc(
        &client,
        server_name,
        url,
        header_map.clone(),
        None,
        json_rpc_request(1, "initialize", Some(mcp_initialize_params())),
        timeout_ms,
    )
    .await?;
    parse_json_rpc_result(&initialize_response.text, 1)?;

    let session_id = initialize_response.session_id;
    post_http_json_rpc(
        &client,
        server_name,
        url,
        header_map.clone(),
        session_id.as_deref(),
        json_rpc_notification("notifications/initialized", Some(serde_json::json!({}))),
        timeout_ms,
    )
    .await?;
    let response = post_http_json_rpc(
        &client,
        server_name,
        url,
        header_map,
        session_id.as_deref(),
        json_rpc_request(2, method, params),
        timeout_ms,
    )
    .await?;

    parse_json_rpc_result(&response.text, 2)
}

// 构造 HTTP transport 所需请求头。秘密值已由不可变配置快照合并进来。
fn build_http_header_map(
    server_name: &str,
    headers: &BTreeMap<String, String>,
) -> Result<HeaderMap, String> {
    let mut header_map = HeaderMap::new();
    header_map.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    header_map.insert(
        ACCEPT,
        HeaderValue::from_static("application/json, text/event-stream"),
    );
    for (key, value) in headers {
        let name = HeaderName::from_bytes(key.as_bytes())
            .map_err(|err| format!("外部 MCP server `{server_name}` 的 header 名称无效：{err}"))?;
        let value = HeaderValue::from_str(value).map_err(|err| {
            format!("外部 MCP server `{server_name}` 的 header `{key}` 值无效：{err}")
        })?;
        header_map.insert(name, value);
    }
    header_map.insert(
        HeaderName::from_static(MCP_PROTOCOL_VERSION_HEADER),
        HeaderValue::from_static(MCP_PROTOCOL_VERSION),
    );
    Ok(header_map)
}

// 发送单次 HTTP JSON-RPC 请求并保留响应 session id。
async fn post_http_json_rpc(
    client: &reqwest::Client,
    server_name: &str,
    url: &str,
    mut header_map: HeaderMap,
    session_id: Option<&str>,
    payload: Value,
    timeout_ms: u64,
) -> Result<HttpJsonRpcResponse, String> {
    if let Some(session_id) = session_id.filter(|value| !value.trim().is_empty()) {
        let value = HeaderValue::from_str(session_id).map_err(|err| {
            format!("外部 MCP server `{server_name}` 的 session id 无法写入 header：{err}")
        })?;
        header_map.insert(HeaderName::from_static(MCP_SESSION_ID_HEADER), value);
    }

    let response = client
        .post(url)
        .headers(header_map)
        .json(&payload)
        .timeout(Duration::from_millis(timeout_ms))
        .send()
        .await
        .map_err(|err| format!("请求外部 MCP server `{server_name}` 失败：{err}"))?;

    let status = response.status();
    let session_id = response
        .headers()
        .get(HeaderName::from_static(MCP_SESSION_ID_HEADER))
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string);
    let text = response
        .text()
        .await
        .map_err(|err| format!("读取外部 MCP server `{server_name}` 响应失败：{err}"))?;
    if !status.is_success() {
        return Err(format!(
            "外部 MCP server `{server_name}` 返回 HTTP {status}：{}",
            truncate_for_error(&text)
        ));
    }

    Ok(HttpJsonRpcResponse { text, session_id })
}

struct StdioJsonRpcRequest<'a> {
    server_name: &'a str,
    command: &'a str,
    args: &'a [String],
    env: &'a BTreeMap<String, String>,
    cwd: Option<&'a str>,
    method: &'a str,
    params: Option<Value>,
    timeout_ms: u64,
}

// 通过 stdio 子进程完成 initialize、initialized 和目标方法调用。
async fn call_stdio_json_rpc(request: StdioJsonRpcRequest<'_>) -> Result<Value, String> {
    let StdioJsonRpcRequest {
        server_name,
        command,
        args,
        env,
        cwd,
        method,
        params,
        timeout_ms,
    } = request;
    let mut cmd = Command::new(command);
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000);
    cmd.args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in env {
        cmd.env(key, value);
    }
    if let Some(cwd) = cwd {
        cmd.current_dir(cwd);
    }

    let mut child = cmd
        .spawn()
        .map_err(|err| format!("启动外部 MCP server `{server_name}` 失败：{err}"))?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| format!("外部 MCP server `{server_name}` 无法打开 stdin。"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| format!("外部 MCP server `{server_name}` 无法打开 stdout。"))?;
    let mut stderr = child.stderr.take();

    let mut lines = BufReader::new(stdout).lines();
    let read_result = async {
        write_json_rpc_line(
            &mut stdin,
            json_rpc_request(1, "initialize", Some(mcp_initialize_params())),
        )
        .await?;
        read_stdio_json_rpc_result(&mut lines, server_name, "initialize", 1, timeout_ms).await?;
        write_json_rpc_line(
            &mut stdin,
            json_rpc_notification("notifications/initialized", Some(serde_json::json!({}))),
        )
        .await?;
        write_json_rpc_line(&mut stdin, json_rpc_request(2, method, params)).await?;
        read_stdio_json_rpc_result(&mut lines, server_name, method, 2, timeout_ms).await
    }
    .await;
    drop(stdin);

    let _ = child.kill().await;
    let _ = child.wait().await;

    match read_result {
        Ok(value) => Ok(value),
        Err(err) => {
            let stderr_text = read_stderr_snapshot(stderr.take()).await;
            if stderr_text.trim().is_empty() {
                Err(err)
            } else {
                Err(format!(
                    "{err}\nstderr：{}",
                    truncate_for_error(&stderr_text)
                ))
            }
        }
    }
}

// 从 stdio stdout 中读取指定 JSON-RPC id 的结果。
async fn read_stdio_json_rpc_result(
    lines: &mut tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
    server_name: &str,
    method: &str,
    expected_id: u64,
    timeout_ms: u64,
) -> Result<Value, String> {
    tokio::time::timeout(Duration::from_millis(timeout_ms), async {
        while let Some(line) = lines
            .next_line()
            .await
            .map_err(|err| format!("读取外部 MCP server `{server_name}` stdout 失败：{err}"))?
        {
            if line.trim().is_empty() {
                continue;
            }
            if let Some(result) = parse_json_rpc_result_value(&line, expected_id)? {
                return Ok(result);
            }
        }
        Err(format!(
            "外部 MCP server `{server_name}` 未返回请求 `{method}` 的 JSON-RPC 结果。"
        ))
    })
    .await
    .map_err(|_| format!("外部 MCP server `{server_name}` 请求 `{method}` 超时。"))?
}

// 向 stdio server 写入一行 JSON-RPC 消息。
async fn write_json_rpc_line(
    stdin: &mut tokio::process::ChildStdin,
    value: Value,
) -> Result<(), String> {
    let line =
        serde_json::to_string(&value).map_err(|err| format!("序列化 JSON-RPC 请求失败：{err}"))?;
    stdin
        .write_all(line.as_bytes())
        .await
        .map_err(|err| format!("写入外部 MCP stdin 失败：{err}"))?;
    stdin
        .write_all(b"\n")
        .await
        .map_err(|err| format!("写入外部 MCP stdin 失败：{err}"))
}

// 读取一小段 stderr 作为失败诊断，避免等待子进程长时间阻塞。
async fn read_stderr_snapshot(stderr: Option<tokio::process::ChildStderr>) -> String {
    let Some(stderr) = stderr else {
        return String::new();
    };
    let mut reader = BufReader::new(stderr);
    let mut output = String::new();
    let _ = tokio::time::timeout(Duration::from_millis(250), reader.read_line(&mut output)).await;
    output
}

// 构造带 id 的 JSON-RPC 请求对象。
fn json_rpc_request(id: u64, method: &str, params: Option<Value>) -> Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method,
        "params": params.unwrap_or_else(|| serde_json::json!({}))
    })
}

// 构造无需响应的 JSON-RPC 通知对象。
fn json_rpc_notification(method: &str, params: Option<Value>) -> Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "method": method,
        "params": params.unwrap_or_else(|| serde_json::json!({}))
    })
}

// 构造 MCP initialize 生命周期参数。
fn mcp_initialize_params() -> Value {
    serde_json::json!({
        "protocolVersion": MCP_PROTOCOL_VERSION,
        "capabilities": { "resources": {}, "tools": {} },
        "clientInfo": {
            "name": "muse-mcp-client",
            "version": env!("CARGO_PKG_VERSION"),
            "title": "muse"
        }
    })
}

// 解析普通 JSON 或 SSE 包裹的 JSON-RPC result。
fn parse_json_rpc_result(text: &str, expected_id: u64) -> Result<Value, String> {
    if let Some(result) = parse_sse_json_rpc_result(text, expected_id)? {
        return Ok(result);
    }
    parse_json_rpc_result_value(text, expected_id)?
        .ok_or_else(|| "JSON-RPC 响应中没有匹配请求 id 的 result。".to_string())
}

// 从 text/event-stream 响应中提取匹配 id 的 JSON-RPC result。
fn parse_sse_json_rpc_result(text: &str, expected_id: u64) -> Result<Option<Value>, String> {
    let mut saw_sse = false;
    for line in text.lines() {
        let trimmed = line.trim_start();
        let Some(data) = trimmed.strip_prefix("data:") else {
            continue;
        };
        saw_sse = true;
        let payload = data.trim();
        if payload.is_empty() || payload == "[DONE]" {
            continue;
        }
        if let Some(result) = parse_json_rpc_result_value(payload, expected_id)? {
            return Ok(Some(result));
        }
    }
    if saw_sse {
        return Err("SSE 响应中没有匹配请求 id 的 JSON-RPC result。".to_string());
    }
    Ok(None)
}

// 从 JSON 文本中解析单个或批量 JSON-RPC 响应。
fn parse_json_rpc_result_value(text: &str, expected_id: u64) -> Result<Option<Value>, String> {
    let value: Value = serde_json::from_str(text)
        .map_err(|err| format!("解析外部 MCP JSON-RPC 响应失败：{err}"))?;
    if let Some(items) = value.as_array() {
        for item in items {
            if let Some(result) = extract_json_rpc_result(item.clone(), expected_id)? {
                return Ok(Some(result));
            }
        }
        return Ok(None);
    }
    extract_json_rpc_result(value, expected_id)
}

// 从已解析 JSON 中提取匹配 id 的 result 或错误。
fn extract_json_rpc_result(value: Value, expected_id: u64) -> Result<Option<Value>, String> {
    let response: JsonRpcResponse = serde_json::from_value(value)
        .map_err(|err| format!("解析外部 MCP JSON-RPC 响应结构失败：{err}"))?;
    if !json_rpc_id_matches(response.id.as_ref(), expected_id) {
        return Ok(None);
    }
    if let Some(error) = response.error {
        return Err(format!("JSON-RPC 错误 {}：{}", error.code, error.message));
    }
    Ok(response.result)
}

// 判断 JSON-RPC 响应 id 是否匹配请求 id。
fn json_rpc_id_matches(id: Option<&Value>, expected_id: u64) -> bool {
    match id {
        Some(Value::Number(number)) => number.as_u64() == Some(expected_id),
        Some(Value::String(value)) => value == &expected_id.to_string(),
        _ => false,
    }
}

// 标准化 resources/list 返回的资源数组。
fn normalize_resource_entries(server: &str, result: &Value) -> Vec<Value> {
    result
        .get("resources")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .map(|item| normalize_resource_entry(server, item))
                .collect()
        })
        .unwrap_or_default()
}

// 标准化单个 MCP resource 条目。
fn normalize_resource_entry(server: &str, item: &Value) -> Value {
    serde_json::json!({
        "server": server,
        "uri": item.get("uri").and_then(Value::as_str).unwrap_or_default(),
        "name": item.get("name").and_then(Value::as_str).unwrap_or_default(),
        "description": item.get("description").and_then(Value::as_str),
        "mime_type": item.get("mimeType").or_else(|| item.get("mime_type")).and_then(Value::as_str),
        "read_only": true,
        "external": true,
        "source": "external_mcp"
    })
}

// 标准化 resources/templates/list 返回的模板数组。
fn normalize_resource_template_entries(server: &str, result: &Value) -> Vec<Value> {
    result
        .get("resourceTemplates")
        .or_else(|| result.get("resource_templates"))
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .map(|item| normalize_resource_template_entry(server, item))
                .collect()
        })
        .unwrap_or_default()
}

// 标准化单个 MCP resource template 条目。
fn normalize_resource_template_entry(server: &str, item: &Value) -> Value {
    serde_json::json!({
        "server": server,
        "uri_template": item
            .get("uriTemplate")
            .or_else(|| item.get("uri_template"))
            .and_then(Value::as_str)
            .unwrap_or_default(),
        "name": item.get("name").and_then(Value::as_str).unwrap_or_default(),
        "description": item.get("description").and_then(Value::as_str),
        "mime_type": item.get("mimeType").or_else(|| item.get("mime_type")).and_then(Value::as_str),
        "read_only": true,
        "external": true,
        "source": "external_mcp"
    })
}

// 标准化 tools/list 返回的工具定义并收集跳过原因。
fn normalize_tool_entries(server: &str, result: &Value) -> (Vec<ExternalMcpToolDef>, Vec<Value>) {
    let mut tools = Vec::new();
    let mut errors = Vec::new();
    let mut seen_names = BTreeMap::<String, ()>::new();
    let Some(items) = result.get("tools").and_then(Value::as_array) else {
        return (tools, errors);
    };

    for item in items {
        let Some(original_name) = item
            .get("name")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            errors.push(external_error_json(
                server,
                "tools/list 返回了缺少 name 的工具定义，已跳过。".to_string(),
            ));
            continue;
        };
        let name = external_mcp_tool_name(server, original_name);
        if seen_names.insert(name.clone(), ()).is_some() {
            errors.push(external_error_json(
                server,
                format!("tools/list 中工具 `{original_name}` 规范化后与已有工具重名，已跳过。"),
            ));
            continue;
        }
        let description = item
            .get("description")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| {
                format!("调用外部 MCP server `{server}` 的工具 `{original_name}`。")
            });
        let parameters = item
            .get("inputSchema")
            .or_else(|| item.get("input_schema"))
            .cloned()
            .unwrap_or_else(|| serde_json::json!({ "type": "object", "properties": {} }));
        let annotations = item
            .get("annotations")
            .cloned()
            .unwrap_or_else(|| serde_json::json!({}));
        let read_only = annotations
            .get("readOnlyHint")
            .or_else(|| annotations.get("read_only_hint"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        tools.push(ExternalMcpToolDef {
            name,
            server_name: server.to_string(),
            original_tool_name: original_name.to_string(),
            description,
            parameters,
            read_only,
            annotations,
        });
    }
    (tools, errors)
}

// 标准化 tools/call 结果，文本给模型读，结构化块给前端和归档使用。
async fn normalize_tool_call_result(
    tool: &ExternalMcpToolDef,
    result: Value,
) -> Result<ExternalMcpToolCallResult, String> {
    let is_error = result
        .get("isError")
        .or_else(|| result.get("is_error"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let structured_content = result
        .get("structuredContent")
        .or_else(|| result.get("structured_content"))
        .cloned();
    let meta = result.get("_meta").cloned();
    let mut content_parts = Vec::new();
    let mut normalized_blocks = Vec::new();

    if let Some(blocks) = result.get("content").and_then(Value::as_array) {
        for (index, block) in blocks.iter().enumerate() {
            let block_type = block
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            match block_type {
                "text" => {
                    let text = block
                        .get("text")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let (preview, truncated) = truncate_tool_text(text);
                    if !preview.is_empty() {
                        content_parts.push(preview.clone());
                    }
                    normalized_blocks.push(serde_json::json!({
                        "type": "text",
                        "text": preview,
                        "truncated": truncated
                    }));
                }
                "image" | "audio" => {
                    let mime_type = block
                        .get("mimeType")
                        .or_else(|| block.get("mime_type"))
                        .and_then(Value::as_str);
                    let Some(data) = block.get("data").and_then(Value::as_str) else {
                        normalized_blocks.push(serde_json::json!({
                            "type": block_type,
                            "mime_type": mime_type,
                            "error": "缺少 data 字段"
                        }));
                        continue;
                    };
                    let bytes = BASE64_STANDARD.decode(data).map_err(|err| {
                        format!(
                            "外部 MCP 工具 `{}` 返回的 {block_type} 不是有效 base64：{err}",
                            tool.name
                        )
                    })?;
                    let path =
                        persist_mcp_blob(&tool.server_name, index, mime_type, &bytes).await?;
                    let message = format!(
                        "MCP 工具 `{}` 返回的 {block_type} 已保存到 {}，mime_type：{}，大小：{} 字节。",
                        tool.name,
                        path.display(),
                        mime_type.unwrap_or("application/octet-stream"),
                        bytes.len()
                    );
                    content_parts.push(message.clone());
                    normalized_blocks.push(serde_json::json!({
                        "type": block_type,
                        "mime_type": mime_type,
                        "blob_saved_to": path.display().to_string(),
                        "text": message
                    }));
                }
                "resource" => {
                    let resource = block
                        .get("resource")
                        .cloned()
                        .unwrap_or_else(|| serde_json::json!({}));
                    let text = resource.get("text").and_then(Value::as_str);
                    let blob = resource.get("blob").and_then(Value::as_str);
                    let mime_type = resource
                        .get("mimeType")
                        .or_else(|| resource.get("mime_type"))
                        .and_then(Value::as_str);
                    if let Some(text) = text {
                        let (preview, truncated) = truncate_tool_text(text);
                        if !preview.is_empty() {
                            content_parts.push(preview.clone());
                        }
                        normalized_blocks.push(serde_json::json!({
                            "type": "resource",
                            "uri": resource.get("uri").and_then(Value::as_str),
                            "mime_type": mime_type,
                            "text": preview,
                            "truncated": truncated
                        }));
                    } else if let Some(blob) = blob {
                        let bytes = BASE64_STANDARD.decode(blob).map_err(|err| {
                            format!(
                                "外部 MCP 工具 `{}` 返回的 resource blob 不是有效 base64：{err}",
                                tool.name
                            )
                        })?;
                        let path =
                            persist_mcp_blob(&tool.server_name, index, mime_type, &bytes).await?;
                        let message = format!(
                            "MCP 工具 `{}` 返回的 resource blob 已保存到 {}，mime_type：{}，大小：{} 字节。",
                            tool.name,
                            path.display(),
                            mime_type.unwrap_or("application/octet-stream"),
                            bytes.len()
                        );
                        content_parts.push(message.clone());
                        normalized_blocks.push(serde_json::json!({
                            "type": "resource",
                            "uri": resource.get("uri").and_then(Value::as_str),
                            "mime_type": mime_type,
                            "blob_saved_to": path.display().to_string(),
                            "text": message
                        }));
                    } else {
                        normalized_blocks.push(serde_json::json!({
                            "type": "resource",
                            "resource": resource
                        }));
                    }
                }
                _ => {
                    let text = serde_json::to_string(block).unwrap_or_else(|_| block.to_string());
                    let (preview, truncated) = truncate_tool_text(&text);
                    content_parts.push(preview.clone());
                    normalized_blocks.push(serde_json::json!({
                        "type": block_type,
                        "json": preview,
                        "truncated": truncated
                    }));
                }
            }
        }
    }

    if content_parts.is_empty()
        && let Some(value) = structured_content.as_ref()
    {
        let text = serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string());
        let (preview, _) = truncate_tool_text(&text);
        if !preview.is_empty() {
            content_parts.push(format!("MCP 工具返回 structuredContent：\n{preview}"));
        }
    }

    let content = if content_parts.is_empty() {
        if is_error {
            format!(
                "外部 MCP 工具 `{}` 返回错误，但没有提供可读文本。",
                tool.name
            )
        } else {
            format!(
                "外部 MCP 工具 `{}` 已执行完成，但没有提供可读文本。",
                tool.name
            )
        }
    } else {
        content_parts.join("\n\n")
    };

    Ok(ExternalMcpToolCallResult {
        content,
        structured: serde_json::json!({
            "server": tool.server_name,
            "tool": tool.name,
            "original_tool_name": tool.original_tool_name,
            "source": "external_mcp",
            "is_error": is_error,
            "structuredContent": structured_content,
            "_meta": meta,
            "content_blocks": normalized_blocks
        }),
        is_error,
    })
}

// 标准化 resources/read 结果，并将二进制内容落盘。
async fn normalize_read_resource(
    server: &str,
    uri: &str,
    result: Value,
) -> Result<ExternalMcpReadResource, String> {
    let mut content_parts = Vec::new();
    let mut normalized_contents = Vec::new();
    let Some(contents) = result.get("contents").and_then(Value::as_array) else {
        return Err("外部 MCP resource 响应缺少 contents 数组。".to_string());
    };

    for (index, item) in contents.iter().enumerate() {
        let item_uri = item.get("uri").and_then(Value::as_str).unwrap_or(uri);
        let mime_type = item
            .get("mimeType")
            .or_else(|| item.get("mime_type"))
            .and_then(Value::as_str);
        if let Some(text) = item.get("text").and_then(Value::as_str) {
            let (preview, truncated) = truncate_resource_text(text);
            content_parts.push(preview.clone());
            normalized_contents.push(serde_json::json!({
                "uri": item_uri,
                "mime_type": mime_type,
                "text": preview,
                "truncated": truncated
            }));
            continue;
        }
        if let Some(blob) = item.get("blob").and_then(Value::as_str) {
            let bytes = BASE64_STANDARD
                .decode(blob)
                .map_err(|err| format!("外部 MCP resource 二进制内容不是有效 base64：{err}"))?;
            let path = persist_mcp_blob(server, index, mime_type, &bytes).await?;
            let message = format!(
                "二进制 MCP resource 已保存到 {}，mime_type：{}，大小：{} 字节。",
                path.display(),
                mime_type.unwrap_or("application/octet-stream"),
                bytes.len()
            );
            content_parts.push(message.clone());
            normalized_contents.push(serde_json::json!({
                "uri": item_uri,
                "mime_type": mime_type,
                "blob_saved_to": path.display().to_string(),
                "text": message
            }));
            continue;
        }
        normalized_contents.push(serde_json::json!({
            "uri": item_uri,
            "mime_type": mime_type
        }));
    }

    let content = if content_parts.is_empty() {
        "外部 MCP resource 没有返回可读文本内容。".to_string()
    } else {
        content_parts.join("\n\n")
    };
    Ok(ExternalMcpReadResource {
        content,
        structured: serde_json::json!({
            "server": server,
            "uri": uri,
            "external": true,
            "source": "external_mcp",
            "contents": normalized_contents
        }),
    })
}

// 将 MCP 返回的图片、音频或二进制 resource 保存到本地 blob 目录。
async fn persist_mcp_blob(
    server: &str,
    index: usize,
    mime_type: Option<&str>,
    bytes: &[u8],
) -> Result<PathBuf, String> {
    let dir = crate::config::Config::config_dir()
        .join("harness")
        .join("mcp-blobs");
    fs::create_dir_all(&dir)
        .await
        .map_err(|err| format!("创建 MCP 二进制资源目录失败：{err}"))?;
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_millis())
        .unwrap_or_default();
    let filename = format!(
        "{}-{timestamp}-{index}.{}",
        sanitize_file_component(server),
        mime_extension(mime_type)
    );
    let path = dir.join(filename);
    fs::write(&path, bytes)
        .await
        .map_err(|err| format!("保存 MCP 二进制资源失败：{err}"))?;
    Ok(path)
}

// 清理 blob 文件名片段，避免外部 server 名直接进入文件系统路径。
fn sanitize_file_component(value: &str) -> String {
    let sanitized = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>();
    if sanitized.is_empty() {
        "mcp".to_string()
    } else {
        sanitized
    }
}

// 根据 MIME 类型选择本地 blob 文件扩展名。
fn mime_extension(mime_type: Option<&str>) -> &'static str {
    match mime_type.unwrap_or_default() {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/webp" => "webp",
        "application/pdf" => "pdf",
        "application/json" => "json",
        "text/plain" => "txt",
        _ => "bin",
    }
}

// 构造外部 MCP 错误的结构化 JSON。
fn external_error_json(server: &str, message: String) -> Value {
    serde_json::json!({
        "server": server,
        "message": message
    })
}

// 返回当前 UNIX 毫秒时间，用于缓存刷新和文件命名。
fn current_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_millis())
        .unwrap_or_default()
}

// 截断超长 resource 文本，避免一次性塞爆模型上下文。
fn truncate_resource_text(text: &str) -> (String, bool) {
    let truncated = text.chars().count() > MCP_RESOURCE_TEXT_PREVIEW_CHARS;
    if !truncated {
        return (text.to_string(), false);
    }
    let preview = text
        .chars()
        .take(MCP_RESOURCE_TEXT_PREVIEW_CHARS)
        .collect::<String>();
    (format!("{preview}\n...（MCP resource 内容已截断）"), true)
}

// 截断超长工具文本结果，保留模型可读预览。
fn truncate_tool_text(text: &str) -> (String, bool) {
    let truncated = text.chars().count() > MCP_TOOL_TEXT_PREVIEW_CHARS;
    if !truncated {
        return (text.to_string(), false);
    }
    let preview = text
        .chars()
        .take(MCP_TOOL_TEXT_PREVIEW_CHARS)
        .collect::<String>();
    (format!("{preview}\n...（MCP 工具结果内容已截断）"), true)
}

// 截断错误消息中的外部响应体，避免日志和前端被大块内容淹没。
fn truncate_for_error(text: &str) -> String {
    const MAX: usize = 2_000;
    if text.chars().count() <= MAX {
        return text.to_string();
    }
    let preview = text.chars().take(MAX).collect::<String>();
    format!("{preview}\n...（已截断）")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_test_servers(entries: Value) -> Result<Vec<ExternalMcpServerConfig>, String> {
        let entries = serde_json::from_value(entries)
            .map_err(|error| format!("构造 MCP 测试配置失败：{error}"))?;
        parse_mcp_servers_map(entries)
    }

    // 验证普通 JSON-RPC 响应能够解析出 result。
    #[test]
    fn parses_plain_json_rpc_result() {
        let result = parse_json_rpc_result(
            r#"{"jsonrpc":"2.0","id":1,"result":{"resources":[{"uri":"demo://a","name":"A"}]}}"#,
            1,
        )
        .expect("应能解析普通 JSON-RPC 响应");

        assert_eq!(result["resources"][0]["uri"], "demo://a");
    }

    // 验证 SSE data 行中的 JSON-RPC 响应能够解析出 result。
    #[test]
    fn parses_sse_json_rpc_result() {
        let result = parse_json_rpc_result(
            "event: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"contents\":[{\"uri\":\"demo://a\",\"text\":\"ok\"}]}}\n\n",
            1,
        )
        .expect("应能解析 SSE data 中的 JSON-RPC 响应");

        assert_eq!(result["contents"][0]["text"], "ok");
    }

    // 验证 MCP resource template 的驼峰字段会归一化成底座字段。
    #[test]
    fn normalizes_resource_templates_from_mcp_shape() {
        let items = normalize_resource_template_entries(
            "docs",
            &serde_json::json!({
                "resourceTemplates": [
                    {
                        "uriTemplate": "docs://{id}",
                        "name": "Docs",
                        "mimeType": "text/markdown"
                    }
                ]
            }),
        );

        assert_eq!(items[0]["server"], "docs");
        assert_eq!(items[0]["uri_template"], "docs://{id}");
        assert_eq!(items[0]["mime_type"], "text/markdown");
    }

    // 验证 config.toml 快照中的 stdio 和 HTTP MCP server 都能解析。
    #[test]
    fn parses_config_snapshot_mcp_servers() {
        let servers = parse_test_servers(serde_json::json!({
            "vision": {
                "type": "stdio",
                "command": "npx",
                "args": ["-y", "vision-mcp-server"],
                "request_timeout_ms": 90000,
                "enabled_tools": ["analyze_image"],
                "disabled_tools": ["delete_image"],
                "env": {
                    "MODELSCOPE_API_KEY": "plain-api-key",
                    "MODELSCOPE_MODEL": "Qwen/Qwen3-VL-30B-A3B-Instruct"
                }
            },
            "modelscope": {
                "type": "streamable_http",
                "url": "https://example.modelscope-mcp/mcp",
                "headers": {
                    "X-Agent": "muse",
                    "Authorization": "Bearer plain-api-key"
                }
            }
        }))
        .expect("应能解析 config.toml 冻结的 MCP 快照");

        assert_eq!(servers.len(), 2);

        let vision = servers
            .iter()
            .find(|server| server.name == "vision")
            .expect("应包含 stdio server");
        assert_eq!(vision.request_timeout_ms, Some(90_000));
        assert!(vision.allows_tool("analyze_image"));
        assert!(!vision.allows_tool("delete_image"));
        match &vision.transport {
            ExternalMcpTransport::Stdio {
                command, args, env, ..
            } => {
                assert_eq!(command, "npx");
                assert_eq!(
                    args,
                    &vec!["-y".to_string(), "vision-mcp-server".to_string()]
                );
                assert_eq!(
                    env.get("MODELSCOPE_API_KEY").map(String::as_str),
                    Some("plain-api-key")
                );
            }
            _ => panic!("vision 应解析为 stdio transport"),
        }

        let modelscope = servers
            .iter()
            .find(|server| server.name == "modelscope")
            .expect("应包含 HTTP server");
        match &modelscope.transport {
            ExternalMcpTransport::StreamableHttp { url, headers } => {
                assert_eq!(url, "https://example.modelscope-mcp/mcp");
                assert_eq!(headers.get("X-Agent").map(String::as_str), Some("muse"));
                assert_eq!(
                    headers.get("Authorization").map(String::as_str),
                    Some("Bearer plain-api-key")
                );
            }
            _ => panic!("modelscope 应解析为 streamable_http transport"),
        }
    }

    #[test]
    fn catalog_keeps_the_discovered_server_target_after_config_changes() {
        let first = parse_test_servers(serde_json::json!({
            "docs": {
                "type": "streamable_http",
                "url": "https://first.example.test/mcp"
            }
        }))
        .expect("首次 MCP 配置应可解析");
        let catalog = super::McpToolCatalog {
            tools: Vec::new(),
            errors: Vec::new(),
            refreshed_at: "now".to_string(),
            refreshed_at_millis: 0,
            config_path: std::path::PathBuf::from("config.toml"),
            config_hash: "first".to_string(),
            server_configs: first,
        };
        let changed = parse_test_servers(serde_json::json!({
            "docs": {
                "type": "streamable_http",
                "url": "https://second.example.test/mcp"
            }
        }))
        .expect("变更后的 MCP 配置应可解析");

        let frozen_url = match &catalog.server_configs[0].transport {
            super::ExternalMcpTransport::StreamableHttp { url, .. } => url,
            _ => panic!("测试 server 应为 HTTP transport"),
        };
        let changed_url = match &changed[0].transport {
            super::ExternalMcpTransport::StreamableHttp { url, .. } => url,
            _ => panic!("测试 server 应为 HTTP transport"),
        };
        assert_eq!(frozen_url, "https://first.example.test/mcp");
        assert_eq!(changed_url, "https://second.example.test/mcp");
    }

    // 验证运行时快照拒绝旧 bearer 字段，Bearer 应先合并进 Authorization header。
    #[test]
    fn rejects_legacy_bearer_field_in_runtime_snapshot() {
        let err = parse_test_servers(serde_json::json!({
            "docs": {
                "type": "streamable_http",
                "url": "https://example.test/mcp",
                "bearer_token_env_var": "DOCS_TOKEN"
            }
        }))
        .expect_err("运行时快照不应保留旧 bearer 字段");

        assert!(err.contains("bearer_token_env_var"));
        assert!(err.contains("不适用于 http/streamable_http transport"));
    }

    // 验证不同 transport 的专属字段不能混用。
    #[test]
    fn rejects_transport_specific_fields_in_models_mcp_server() {
        let err = parse_test_servers(serde_json::json!({
            "docs": {
                "type": "streamable_http",
                "url": "https://example.test/mcp",
                "args": ["unexpected"]
            }
        }))
        .expect_err("HTTP MCP server 不应接受 stdio 字段");

        assert!(err.contains("args"));
        assert!(err.contains("不适用于 http/streamable_http transport"));
    }

    // 验证 v1 未实现的 transport 会给出中文边界错误。
    #[test]
    fn rejects_unsupported_transport_with_chinese_message() {
        let err = parse_test_servers(serde_json::json!({
            "demo": {
                "type": "sse",
                "url": "http://127.0.0.1:3000/sse"
            }
        }))
        .expect_err("v1 应拒绝未实现的 SSE transport");

        assert!(err.contains("Muse v1 仅支持 stdio 和 http/streamable_http"));
    }

    // 验证远端只读注解不能绕过本地审批。
    #[test]
    fn normalizes_mcp_tool_name_but_requires_local_approval() {
        let (tools, errors) = normalize_tool_entries(
            "modelscope",
            &serde_json::json!({
                "tools": [
                    {
                        "name": "search.models",
                        "description": "搜索模型。",
                        "inputSchema": {
                            "type": "object",
                            "properties": {
                                "query": { "type": "string" }
                            }
                        },
                        "annotations": {
                            "readOnlyHint": true
                        }
                    }
                ]
            }),
        );

        assert!(errors.is_empty());
        assert_eq!(tools[0].name, "mcp__modelscope__search_models");
        assert_eq!(tools[0].original_tool_name, "search.models");
        let tool_def = tools[0].to_tool_def();
        assert_eq!(tool_def.risk, ToolRisk::ExternalSideEffect);
        assert!(tool_def.requires_approval);
    }
}

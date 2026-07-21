//! MCP 客户端与动态工具目录模块，负责外部 MCP 服务发现、资源读取和工具调用。

mod client;
pub mod config;
pub mod migration;
mod store;

pub use client::{McpClientManager, McpConnectionDiagnostic, McpNegotiatedSession};
pub use config::{McpApprovalPolicy, McpProfileConfig, McpRuntimeSnapshot, McpServerProfile};

use crate::domain::persona::{McpPolicy, ResourcePolicyMode};
use crate::domain::tool::{ToolDef, ToolExecutionOwner, ToolRisk};
use crate::domain::turn::RuntimeMcpToolPolicyEntry;
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::hash_map::DefaultHasher;
use std::collections::{BTreeMap, BTreeSet};
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::fs;

const DEFAULT_REQUEST_TIMEOUT_MS: u64 = 30_000;
const MAX_REQUEST_TIMEOUT_MS: u64 = 120_000;
const MCP_PROTOCOL_VERSION: &str = "2025-11-25";
const MCP_COMPATIBLE_PROTOCOL_VERSION: &str = "2025-06-18";
const MCP_MAX_CATALOG_PAGES: usize = 64;
const MCP_MAX_CATALOG_TOOLS: usize = 1_024;
const MCP_RESOURCE_TEXT_PREVIEW_CHARS: usize = 40_000;
const MCP_TOOL_TEXT_PREVIEW_CHARS: usize = 40_000;

/// 单个 Turn 冻结的 MCP 服务范围。该范围必须在任何 transport 建立前应用。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectiveMcpScope {
    allowed_servers: Option<BTreeSet<String>>,
}

impl EffectiveMcpScope {
    /// 管理端显式测试使用的全局范围，不受 Persona 策略限制。
    pub fn unrestricted() -> Self {
        Self {
            allowed_servers: None,
        }
    }

    /// 管理端测试单个 Server 时使用的显式范围。
    pub fn only(server: impl Into<String>) -> Self {
        Self {
            allowed_servers: Some(BTreeSet::from([server.into()])),
        }
    }

    /// 从当前 Persona 策略生成不可变范围。
    pub fn from_policy(policy: Option<&McpPolicy>) -> Self {
        match policy.map(|policy| &policy.mode) {
            None | Some(ResourcePolicyMode::Inherit) => Self::unrestricted(),
            Some(ResourcePolicyMode::Disabled) => Self {
                allowed_servers: Some(BTreeSet::new()),
            },
            Some(ResourcePolicyMode::AllowList) => Self {
                allowed_servers: Some(
                    policy
                        .into_iter()
                        .flat_map(|policy| policy.allowed_servers.iter())
                        .cloned()
                        .collect(),
                ),
            },
        }
    }

    fn allows(&self, server: &str) -> bool {
        self.allowed_servers
            .as_ref()
            .is_none_or(|allowed| allowed.contains(server))
    }

    /// 参与 MCP 目录缓存键，防止不同 Persona 复用越权目录。
    pub fn cache_key(&self) -> String {
        match &self.allowed_servers {
            None => "all".to_string(),
            Some(allowed) if allowed.is_empty() => "none".to_string(),
            Some(allowed) => format!(
                "allow:{}",
                allowed.iter().cloned().collect::<Vec<_>>().join(",")
            ),
        }
    }
}

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
    // 与工具定义同时冻结的连接 lease。当前回合只能使用这里的配置 revision；
    // 配置变更只影响新 lease，最后一个旧 Turn 释放后再关闭旧 session。
    server_leases: Vec<client::McpClientLease>,
    lease_catalog_epochs: BTreeMap<String, u64>,
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
            server_leases: Vec::new(),
            lease_catalog_epochs: BTreeMap::new(),
        }
    }

    /// 判断缓存目录是否仍匹配当前配置哈希且未超过有效期。
    pub fn is_fresh(&self, config_hash: &str, ttl: Duration) -> bool {
        if self.config_hash != config_hash {
            return false;
        }
        current_millis().saturating_sub(self.refreshed_at_millis) <= ttl.as_millis()
            && self.server_leases.iter().all(|lease| {
                lease.is_healthy()
                    && self
                        .lease_catalog_epochs
                        .get(lease.server_name())
                        .is_some_and(|epoch| *epoch == lease.catalog_epoch())
            })
    }

    /// 将外部 MCP 工具目录转换为本轮可暴露给模型的工具定义。
    pub fn tool_defs(&self) -> Vec<ToolDef> {
        self.tools
            .iter()
            .map(ExternalMcpToolDef::to_tool_def)
            .collect()
    }

    /// 为单轮审计冻结模型实际可见的 MCP 审批事实。
    pub fn runtime_policy_entries(
        &self,
        visible_tool_names: &BTreeSet<String>,
    ) -> Vec<RuntimeMcpToolPolicyEntry> {
        self.tools
            .iter()
            .filter(|tool| visible_tool_names.contains(&tool.name))
            .map(|tool| RuntimeMcpToolPolicyEntry {
                name: tool.name.clone(),
                server: tool.server_name.clone(),
                server_revision: tool.server_revision.clone(),
                annotations_hash: tool.annotations_hash.clone(),
                approval_policy: tool.approval_policy.as_str().to_string(),
                approval_source: tool.approval_source.as_str().to_string(),
                final_risk: tool.final_risk.as_str().to_string(),
                requires_approval: tool.requires_approval,
            })
            .collect()
    }

    /// 按完全限定工具名查找外部 MCP 工具定义。
    pub fn find_tool(&self, name: &str) -> Option<ExternalMcpToolDef> {
        self.tools.iter().find(|tool| tool.name == name).cloned()
    }

    /// 返回指定 Server 当前 lease 的脱敏连接诊断。
    pub async fn connection_diagnostic(&self, server: &str) -> Option<McpConnectionDiagnostic> {
        let lease = self
            .server_leases
            .iter()
            .find(|lease| lease.server_name() == server)?;
        Some(lease.diagnostic().await)
    }

    /// 使用本目录冻结的 server 配置调用工具。
    pub async fn call_tool(
        &self,
        tool: &ExternalMcpToolDef,
        arguments: Value,
    ) -> Result<ExternalMcpToolCallResult, String> {
        let lease = select_single_lease(&self.server_leases, &tool.server_name)?;
        call_external_mcp_tool_with_lease(lease, tool, arguments).await
    }

    /// 使用本目录冻结的 server 配置列出资源。
    pub async fn list_resources(
        &self,
        server: Option<String>,
        cursor: Option<String>,
    ) -> Result<ExternalMcpResources, String> {
        list_external_mcp_resources_with_leases(
            &self.server_leases,
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
        list_external_mcp_resource_templates_with_leases(
            &self.server_leases,
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
        let lease = select_single_lease(&self.server_leases, &server)?;
        read_external_mcp_resource_with_lease(lease, uri).await
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
    pub server_revision: String,
    pub annotations_hash: String,
    pub approval_policy: McpApprovalPolicy,
    pub approval_source: McpApprovalSource,
    pub final_risk: ToolRisk,
    pub requires_approval: bool,
}

/// MCP 工具最终审批策略的本地来源。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum McpApprovalSource {
    ServerPolicy,
    ToolOverride,
}

impl McpApprovalSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ServerPolicy => "server_policy",
            Self::ToolOverride => "tool_override",
        }
    }
}

impl ExternalMcpToolDef {
    /// 转换为运行底座统一工具定义，保留 MCP 工具风险和参数结构。
    pub fn to_tool_def(&self) -> ToolDef {
        ToolDef {
            name: self.name.clone(),
            description: self.description.clone(),
            parameters: self.parameters.clone(),
            category: format!("mcp:{}", self.server_name),
            requires_approval: self.requires_approval,
            execution_owner: ToolExecutionOwner::ExternalProvider,
            available: true,
            disabled_reason: None,
            risk: self.final_risk.clone(),
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

/// MCP 失败的稳定、脱敏诊断。
#[derive(Debug, Clone, serde::Serialize)]
pub struct McpStructuredError {
    pub code: String,
    pub kind: String,
    pub message: String,
    pub retryable: bool,
    pub alternatives: Vec<String>,
}

/// 单个已保存 Server、且只对应一个配置 revision 的最近检查状态。
#[derive(Debug, Clone, serde::Serialize)]
pub struct McpServerCheckStatus {
    pub revision: String,
    pub status: String,
    pub tool_count: usize,
    pub resource_count: usize,
    pub checked_at: String,
    pub last_error: Option<McpStructuredError>,
    pub connection: Option<McpConnectionDiagnostic>,
}

/// 将内部错误收口成有界、可供管理页和模型解释的稳定结构。
pub fn structured_mcp_error(message: impl Into<String>) -> McpStructuredError {
    let message = message.into();
    let (code, kind, retryable, alternatives): (&str, &str, bool, &[&str]) = if message
        .contains("重定向")
    {
        (
            "mcp_redirect_blocked",
            "policy_blocked",
            false,
            &["在管理页填写 Server 的最终 URL"],
        )
    } else if message.contains("公网连接必须使用 HTTPS") || message.contains("未授权的特殊网络地址")
    {
        (
            "mcp_url_policy_blocked",
            "policy_blocked",
            false,
            &["检查 URL 是否符合本地或 HTTPS 网络策略"],
        )
    } else if message.contains("DNS") {
        (
            "mcp_dns_failed",
            "connection",
            true,
            &["检查主机名和本机 DNS 配置"],
        )
    } else if message.contains("超时") {
        (
            "mcp_timeout",
            "timeout",
            true,
            &["检查 Server 是否阻塞", "适当提高请求超时"],
        )
    } else if message.contains("协议版本") || message.contains("initialize") {
        (
            "mcp_protocol_incompatible",
            "protocol",
            false,
            &["检查 Server 支持的 MCP 协议版本"],
        )
    } else if message.contains("capability") {
        (
            "mcp_capability_missing",
            "protocol",
            false,
            &["检查 Server 声明的 capability"],
        )
    } else if message.contains("HTTP 401") || message.contains("HTTP 403") {
        (
            "mcp_auth_failed",
            "authentication",
            false,
            &["更新秘密 Header 或 Bearer 凭据"],
        )
    } else if message.contains("已退出") || message.contains("stdout") || message.contains("stdin")
    {
        (
            "mcp_server_exited",
            "server_process",
            true,
            &["检查 Server 命令和本地诊断"],
        )
    } else if message.contains("配置") || message.contains("缺少") || message.contains("无效")
    {
        (
            "mcp_config_invalid",
            "configuration",
            false,
            &["修正管理页中的 Server 配置"],
        )
    } else {
        (
            "mcp_connection_failed",
            "connection",
            true,
            &["检查 Server 是否已启动", "重新测试连接"],
        )
    };
    McpStructuredError {
        code: code.to_string(),
        kind: kind.to_string(),
        message,
        retryable,
        alternatives: alternatives
            .iter()
            .take(3)
            .map(|value| (*value).to_string())
            .collect(),
    }
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
#[derive(Clone)]
pub(super) struct ExternalMcpServerConfig {
    name: String,
    config_revision: String,
    enabled: bool,
    request_timeout_ms: Option<u64>,
    enabled_tools: Option<Vec<String>>,
    disabled_tools: Vec<String>,
    approval_policy: McpApprovalPolicy,
    tool_approval_overrides: BTreeMap<String, McpApprovalPolicy>,
    transport: ExternalMcpTransport,
}

impl std::fmt::Debug for ExternalMcpServerConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ExternalMcpServerConfig")
            .field("name", &self.name)
            .field("config_revision", &self.config_revision)
            .field("enabled", &self.enabled)
            .field("transport", &self.transport.kind())
            .finish_non_exhaustive()
    }
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

impl ExternalMcpTransport {
    fn kind(&self) -> &'static str {
        match self {
            Self::StreamableHttp { .. } => "streamable_http",
            Self::Stdio { .. } => "stdio",
        }
    }
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

    fn timeout_ms(&self) -> u64 {
        self.request_timeout_ms
            .unwrap_or(DEFAULT_REQUEST_TIMEOUT_MS)
            .clamp(1_000, MAX_REQUEST_TIMEOUT_MS)
    }

    fn approval_for_tool(&self, tool_name: &str) -> (McpApprovalPolicy, McpApprovalSource) {
        self.tool_approval_overrides
            .get(tool_name)
            .copied()
            .map(|policy| (policy, McpApprovalSource::ToolOverride))
            .unwrap_or((self.approval_policy, McpApprovalSource::ServerPolicy))
    }

    fn diagnostic_redactions(&self) -> Vec<String> {
        match &self.transport {
            ExternalMcpTransport::Stdio { env, .. } => env.values().cloned().collect(),
            ExternalMcpTransport::StreamableHttp { headers, .. } => {
                headers.values().cloned().collect()
            }
        }
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
    let manager = McpClientManager::default();
    let leases = lease_servers(&manager, &loaded.servers).await?;
    list_external_mcp_resources_with_leases(&leases, loaded.config_path, server, cursor).await
}

async fn list_external_mcp_resources_with_leases(
    leases: &[client::McpClientLease],
    config_path: PathBuf,
    server: Option<String>,
    cursor: Option<String>,
) -> Result<ExternalMcpResources, String> {
    let refreshed_at = chrono::Local::now().to_rfc3339();
    let mut resources = Vec::new();
    let mut errors = Vec::new();
    let mut next_cursor = None;

    let targets = select_leases(leases, server.as_deref())?;
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
    for lease in targets {
        let params = match cursor.as_ref() {
            Some(value) => serde_json::json!({ "cursor": value }),
            None => serde_json::json!({}),
        };
        match lease.request("resources/list", Some(params)).await {
            Ok(result) => {
                if target_count == 1 {
                    next_cursor = result
                        .get("nextCursor")
                        .or_else(|| result.get("next_cursor"))
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned);
                }
                resources.extend(normalize_resource_entries(lease.server_name(), &result));
            }
            Err(err) => errors.push(external_error_json(lease.server_name(), err)),
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
    let manager = McpClientManager::default();
    let leases = lease_servers(&manager, &loaded.servers).await?;
    list_external_mcp_resource_templates_with_leases(&leases, loaded.config_path, server, cursor)
        .await
}

async fn list_external_mcp_resource_templates_with_leases(
    leases: &[client::McpClientLease],
    config_path: PathBuf,
    server: Option<String>,
    cursor: Option<String>,
) -> Result<ExternalMcpResourceTemplates, String> {
    let refreshed_at = chrono::Local::now().to_rfc3339();
    let mut resource_templates = Vec::new();
    let mut errors = Vec::new();
    let mut next_cursor = None;

    let targets = select_leases(leases, server.as_deref())?;
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
    for lease in targets {
        let params = match cursor.as_ref() {
            Some(value) => serde_json::json!({ "cursor": value }),
            None => serde_json::json!({}),
        };
        match lease
            .request("resources/templates/list", Some(params))
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
                    lease.server_name(),
                    &result,
                ));
            }
            Err(err) => errors.push(external_error_json(lease.server_name(), err)),
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
    let manager = McpClientManager::default();
    let leases = lease_servers(&manager, &loaded.servers).await?;
    let lease = select_single_lease(&leases, &server)?;
    read_external_mcp_resource_with_lease(lease, uri).await
}

async fn read_external_mcp_resource_with_lease(
    lease: &client::McpClientLease,
    uri: String,
) -> Result<ExternalMcpReadResource, String> {
    let result = lease
        .request("resources/read", Some(serde_json::json!({ "uri": uri })))
        .await?;
    normalize_read_resource(lease.server_name(), &uri, result).await
}

/// 发现所有启用 MCP 服务暴露的工具，并隔离单个服务失败。
pub async fn discover_external_mcp_tools(snapshot: &McpRuntimeSnapshot) -> McpToolCatalog {
    let manager = McpClientManager::default();
    discover_external_mcp_tools_for_scope_with_manager(
        snapshot,
        &EffectiveMcpScope::unrestricted(),
        &manager,
    )
    .await
}

/// 仅发现当前 Turn 允许的 MCP 服务；过滤发生在任何网络请求或进程启动之前。
pub async fn discover_external_mcp_tools_for_scope(
    snapshot: &McpRuntimeSnapshot,
    scope: &EffectiveMcpScope,
) -> McpToolCatalog {
    let manager = McpClientManager::default();
    discover_external_mcp_tools_for_scope_with_manager(snapshot, scope, &manager).await
}

/// 使用运行时拥有的 manager 发现工具；返回目录持有对应配置 revision 的 lease。
pub async fn discover_external_mcp_tools_for_scope_with_manager(
    snapshot: &McpRuntimeSnapshot,
    scope: &EffectiveMcpScope,
    manager: &McpClientManager,
) -> McpToolCatalog {
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
                server_leases: Vec::new(),
                lease_catalog_epochs: BTreeMap::new(),
            };
            catalog.tools.sort_by(|a, b| a.name.cmp(&b.name));
            return catalog;
        }
    };
    let config_hash = format!("{}:{}", loaded.config_hash(), scope.cache_key());
    let scoped_servers = loaded
        .servers
        .into_iter()
        .filter(|server| scope.allows(&server.name))
        .collect::<Vec<_>>();
    let mut catalog = McpToolCatalog {
        tools: Vec::new(),
        errors: Vec::new(),
        refreshed_at,
        refreshed_at_millis,
        config_path: loaded.config_path.clone(),
        config_hash,
        server_leases: Vec::new(),
        lease_catalog_epochs: BTreeMap::new(),
    };

    for server_config in &scoped_servers {
        match manager.lease(server_config).await {
            Ok(lease) => {
                // 目录拉取开始前冻结 epoch。若 listChanged 在分页期间或完成后到达，
                // 实际 epoch 会推进，当前目录会立即判定为过期，而不是吞掉通知。
                let catalog_epoch = lease.catalog_epoch();
                match list_all_external_mcp_tools(&lease).await {
                    Ok(result) => {
                        let (mut tools, mut errors) =
                            normalize_tool_entries(server_config, &result);
                        tools.retain(|tool| server_config.allows_tool(&tool.original_tool_name));
                        catalog.tools.append(&mut tools);
                        catalog.errors.append(&mut errors);
                    }
                    Err(error) => catalog
                        .errors
                        .push(external_error_json(&server_config.name, error)),
                }
                catalog
                    .lease_catalog_epochs
                    .insert(server_config.name.clone(), catalog_epoch);
                catalog.server_leases.push(lease);
            }
            Err(error) => catalog
                .errors
                .push(external_error_json(&server_config.name, error)),
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
    let manager = McpClientManager::default();
    let leases = lease_servers(&manager, &loaded.servers).await?;
    let lease = select_single_lease(&leases, &tool.server_name)?;
    call_external_mcp_tool_with_lease(lease, tool, arguments).await
}

async fn call_external_mcp_tool_with_lease(
    lease: &client::McpClientLease,
    tool: &ExternalMcpToolDef,
    arguments: Value,
) -> Result<ExternalMcpToolCallResult, String> {
    let result = lease
        .request(
            "tools/call",
            Some(serde_json::json!({
            "name": tool.original_tool_name,
            "arguments": arguments
            })),
        )
        .await?;
    normalize_tool_call_result(tool, result).await
}

async fn list_all_external_mcp_tools(lease: &client::McpClientLease) -> Result<Value, String> {
    let mut cursor = None;
    let mut seen_cursors = BTreeSet::new();
    let mut tools = Vec::new();
    for _ in 0..MCP_MAX_CATALOG_PAGES {
        let params = cursor
            .as_ref()
            .map(|cursor| serde_json::json!({ "cursor": cursor }))
            .unwrap_or_else(|| serde_json::json!({}));
        let result = lease.request("tools/list", Some(params)).await?;
        let page = result
            .get("tools")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                format!(
                    "外部 MCP server `{}` 的 tools/list 响应缺少 tools 数组。",
                    lease.server_name()
                )
            })?;
        if tools.len().saturating_add(page.len()) > MCP_MAX_CATALOG_TOOLS {
            return Err(format!(
                "外部 MCP server `{}` 的工具目录超过 {} 项安全上限。",
                lease.server_name(),
                MCP_MAX_CATALOG_TOOLS
            ));
        }
        tools.extend(page.iter().cloned());
        let next_cursor = result
            .get("nextCursor")
            .or_else(|| result.get("next_cursor"))
            .and_then(Value::as_str)
            .map(ToString::to_string);
        let Some(next_cursor) = next_cursor else {
            return Ok(serde_json::json!({ "tools": tools }));
        };
        if !seen_cursors.insert(next_cursor.clone()) {
            return Err(format!(
                "外部 MCP server `{}` 的 tools/list 出现 cursor 循环。",
                lease.server_name()
            ));
        }
        cursor = Some(next_cursor);
    }
    Err(format!(
        "外部 MCP server `{}` 的 tools/list 超过 {} 页安全上限。",
        lease.server_name(),
        MCP_MAX_CATALOG_PAGES
    ))
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
    let config_revision = external_server_config_revision(&name, &raw);
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
    let approval_policy = raw_object
        .get("approval_policy")
        .cloned()
        .map(serde_json::from_value::<McpApprovalPolicy>)
        .transpose()
        .map_err(|_| {
            format!(
                "外部 MCP server `{name}` 的 approval_policy 仅支持 always_ask 或 trusted_read_only。"
            )
        })?
        .unwrap_or_default();
    let tool_approval_overrides = raw_object
        .get("tool_approval_overrides")
        .cloned()
        .map(serde_json::from_value::<BTreeMap<String, McpApprovalPolicy>>)
        .transpose()
        .map_err(|_| {
            format!(
                "外部 MCP server `{name}` 的 tool_approval_overrides 必须是合法的工具审批策略映射。"
            )
        })?
        .unwrap_or_default();
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
        config_revision,
        enabled,
        request_timeout_ms,
        enabled_tools,
        disabled_tools,
        approval_policy,
        tool_approval_overrides,
        transport,
    })
}

fn external_server_config_revision(name: &str, raw: &Value) -> String {
    let mut hasher = Sha256::new();
    hasher.update(name.as_bytes());
    hasher.update([0]);
    hasher.update(serde_json::to_vec(raw).unwrap_or_default());
    format!("{:x}", hasher.finalize())
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

async fn lease_servers(
    manager: &McpClientManager,
    servers: &[ExternalMcpServerConfig],
) -> Result<Vec<client::McpClientLease>, String> {
    let mut leases = Vec::with_capacity(servers.len());
    for server in servers {
        leases.push(manager.lease(server).await?);
    }
    Ok(leases)
}

// 按可选 server 名称选择本次要调用的连接 lease 集合。
fn select_leases<'a>(
    leases: &'a [client::McpClientLease],
    server: Option<&str>,
) -> Result<Vec<&'a client::McpClientLease>, String> {
    if let Some(server_name) = server {
        return Ok(vec![select_single_lease(leases, server_name)?]);
    }
    Ok(leases.iter().collect())
}

// 选择单个 lease，并在失败时给出当前可用列表。
fn select_single_lease<'a>(
    leases: &'a [client::McpClientLease],
    server: &str,
) -> Result<&'a client::McpClientLease, String> {
    leases
        .iter()
        .find(|item| item.server_name() == server)
        .ok_or_else(|| {
            let available = leases
                .iter()
                .map(client::McpClientLease::server_name)
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
fn normalize_tool_entries(
    server: &ExternalMcpServerConfig,
    result: &Value,
) -> (Vec<ExternalMcpToolDef>, Vec<Value>) {
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
                &server.name,
                "tools/list 返回了缺少 name 的工具定义，已跳过。".to_string(),
            ));
            continue;
        };
        let name = external_mcp_tool_name(&server.name, original_name);
        if seen_names.insert(name.clone(), ()).is_some() {
            errors.push(external_error_json(
                &server.name,
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
                format!(
                    "调用外部 MCP server `{}` 的工具 `{original_name}`。",
                    server.name
                )
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
        let (approval_policy, approval_source) = server.approval_for_tool(original_name);
        let trusted_read_only = approval_policy == McpApprovalPolicy::TrustedReadOnly && read_only;
        let final_risk = if trusted_read_only {
            ToolRisk::ReadOnly
        } else {
            ToolRisk::ExternalSideEffect
        };
        let annotations_hash = format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&annotations).unwrap_or_default())
        );
        tools.push(ExternalMcpToolDef {
            name,
            server_name: server.name.clone(),
            original_tool_name: original_name.to_string(),
            description,
            parameters,
            read_only,
            annotations,
            server_revision: server.config_revision.clone(),
            annotations_hash,
            approval_policy,
            approval_source,
            final_risk,
            requires_approval: !trusted_read_only,
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
    let diagnostic = structured_mcp_error(message);
    let mut value = serde_json::to_value(diagnostic).unwrap_or_else(|_| serde_json::json!({}));
    if let Some(object) = value.as_object_mut() {
        object.insert("server".to_string(), Value::String(server.to_string()));
    }
    value
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

#[cfg(test)]
mod tests {
    use super::*;

    fn test_profile(command: &str, args: Vec<String>) -> McpServerProfile {
        McpServerProfile {
            transport: "stdio".to_string(),
            enabled: true,
            request_timeout_ms: Some(1_000),
            enabled_tools: None,
            disabled_tools: Vec::new(),
            approval_policy: Default::default(),
            tool_approval_overrides: BTreeMap::new(),
            command: Some(command.to_string()),
            args: Some(args),
            cwd: None,
            url: None,
            env: BTreeMap::new(),
            secret_env: BTreeMap::new(),
            headers: BTreeMap::new(),
            secret_headers: BTreeMap::new(),
        }
    }

    fn parse_test_servers(entries: Value) -> Result<Vec<ExternalMcpServerConfig>, String> {
        let entries = serde_json::from_value(entries)
            .map_err(|error| format!("构造 MCP 测试配置失败：{error}"))?;
        parse_mcp_servers_map(entries)
    }

    /// 返回创建标记文件后以指定码退出的平台命令，用于验证 stdio Server 是否被真实启动。
    fn touch_marker_command(
        marker: &std::path::Path,
        exit_code: i32,
    ) -> (&'static str, Vec<String>) {
        #[cfg(unix)]
        {
            (
                "/bin/sh",
                vec![
                    "-c".to_string(),
                    format!("touch '{}'; exit {exit_code}", marker.display()),
                ],
            )
        }
        #[cfg(windows)]
        {
            (
                "powershell.exe",
                vec![
                    "-NoLogo".to_string(),
                    "-NoProfile".to_string(),
                    "-NonInteractive".to_string(),
                    "-Command".to_string(),
                    format!(
                        "New-Item -Path '{}' -ItemType File -Force | Out-Null; exit {exit_code}",
                        marker.display()
                    ),
                ],
            )
        }
    }

    #[tokio::test]
    async fn disabled_scope_does_not_spawn_configured_stdio_server() {
        let marker = std::env::temp_dir().join(format!(
            "muse-mcp-denied-{}-{}",
            std::process::id(),
            current_millis()
        ));
        let mut profiles = BTreeMap::new();
        let (command, args) = touch_marker_command(&marker, 0);
        profiles.insert("denied".to_string(), test_profile(command, args));
        let snapshot = McpProfileConfig {
            mcp_servers: profiles,
        }
        .runtime_snapshot(std::env::temp_dir().join("muse-mcp-scope.toml"));
        let policy = McpPolicy {
            mode: ResourcePolicyMode::Disabled,
            allowed_servers: Vec::new(),
        };

        let catalog = discover_external_mcp_tools_for_scope(
            &snapshot,
            &EffectiveMcpScope::from_policy(Some(&policy)),
        )
        .await;

        assert!(catalog.tools.is_empty());
        assert!(catalog.server_leases.is_empty());
        assert!(!marker.exists(), "禁用范围不能启动 stdio MCP 服务");
    }

    #[tokio::test]
    async fn single_server_scope_never_spawns_other_configured_servers() {
        let allowed_marker = std::env::temp_dir().join(format!(
            "muse-mcp-single-allowed-{}-{}",
            std::process::id(),
            current_millis()
        ));
        let denied_marker = std::env::temp_dir().join(format!(
            "muse-mcp-single-denied-{}-{}",
            std::process::id(),
            current_millis()
        ));
        let (allowed_command, allowed_args) = touch_marker_command(&allowed_marker, 1);
        let (denied_command, denied_args) = touch_marker_command(&denied_marker, 1);
        let profiles = BTreeMap::from([
            (
                "allowed".to_string(),
                test_profile(allowed_command, allowed_args),
            ),
            (
                "denied".to_string(),
                test_profile(denied_command, denied_args),
            ),
        ]);
        let snapshot = McpProfileConfig {
            mcp_servers: profiles,
        }
        .runtime_snapshot(std::env::temp_dir().join("muse-mcp-single.toml"));

        let catalog =
            discover_external_mcp_tools_for_scope(&snapshot, &EffectiveMcpScope::only("allowed"))
                .await;

        assert!(
            allowed_marker.exists(),
            "目标 Server 应被实际测试；catalog 诊断：{:?}",
            catalog.errors
        );
        assert!(!denied_marker.exists(), "单 Server 测试不得启动其他 Server");
        let _ = std::fs::remove_file(allowed_marker);
    }

    #[test]
    fn allow_list_scope_has_stable_order_independent_cache_key() {
        let first = McpPolicy {
            mode: ResourcePolicyMode::AllowList,
            allowed_servers: vec!["zeta".to_string(), "alpha".to_string()],
        };
        let second = McpPolicy {
            mode: ResourcePolicyMode::AllowList,
            allowed_servers: vec!["alpha".to_string(), "zeta".to_string()],
        };
        assert_eq!(
            EffectiveMcpScope::from_policy(Some(&first)).cache_key(),
            EffectiveMcpScope::from_policy(Some(&second)).cache_key()
        );
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
    fn config_revision_changes_with_transport_or_secret_content() {
        let first = parse_test_servers(serde_json::json!({
            "docs": {
                "type": "streamable_http",
                "url": "https://first.example.test/mcp"
            }
        }))
        .expect("首次 MCP 配置应可解析");
        let changed = parse_test_servers(serde_json::json!({
            "docs": {
                "type": "streamable_http",
                "url": "https://second.example.test/mcp",
                "headers": { "Authorization": "Bearer changed-secret" }
            }
        }))
        .expect("变更后的 MCP 配置应可解析");
        assert_ne!(first[0].config_revision, changed[0].config_revision);
        assert!(!first[0].config_revision.contains("first.example"));
        assert!(!changed[0].config_revision.contains("changed-secret"));
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
        let server = parse_mcp_servers_entry(
            "modelscope".to_string(),
            test_profile("demo", Vec::new()).to_runtime_value(),
        )
        .expect("测试 Server 配置应有效");
        let (tools, errors) = normalize_tool_entries(
            &server,
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

    #[test]
    fn trusted_read_only_requires_both_local_policy_and_remote_hint() {
        let mut profile = test_profile("demo", Vec::new());
        profile.approval_policy = McpApprovalPolicy::TrustedReadOnly;
        let server = parse_mcp_servers_entry("trusted".to_string(), profile.to_runtime_value())
            .expect("可信只读 Server 配置应有效");
        let (tools, errors) = normalize_tool_entries(
            &server,
            &serde_json::json!({
                "tools": [
                    { "name": "read", "annotations": { "readOnlyHint": true } },
                    { "name": "write", "annotations": { "readOnlyHint": false } }
                ]
            }),
        );

        assert!(errors.is_empty());
        assert_eq!(tools[0].to_tool_def().risk, ToolRisk::ReadOnly);
        assert!(!tools[0].to_tool_def().requires_approval);
        assert_eq!(tools[1].to_tool_def().risk, ToolRisk::ExternalSideEffect);
        assert!(tools[1].to_tool_def().requires_approval);
    }
}

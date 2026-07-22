//! MCP 管理接口的请求与响应 DTO。

use super::preferences::SecretUpdate;
use muse_core::domain::mcp;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

fn default_true() -> bool {
    true
}

/// MCP 凭据字段的变更请求。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpSecretFieldUpdate {
    pub target: String,
    #[serde(default)]
    pub secret: SecretUpdate,
}

/// MCP stdio 或 Streamable HTTP 传输配置。
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum McpTransportUpdate {
    Stdio {
        command: String,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default)]
        cwd: Option<String>,
        #[serde(default)]
        env: BTreeMap<String, String>,
        #[serde(default)]
        secrets: Vec<McpSecretFieldUpdate>,
    },
    StreamableHttp {
        url: String,
        #[serde(default)]
        headers: BTreeMap<String, String>,
        #[serde(default)]
        header_secrets: Vec<McpSecretFieldUpdate>,
        #[serde(default)]
        bearer_token: Option<McpSecretFieldUpdate>,
    },
}

/// MCP server 创建请求。
#[derive(Debug, Deserialize)]
pub struct McpServerCreateRequest {
    pub name: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub request_timeout_ms: Option<u64>,
    #[serde(default)]
    pub enabled_tools: Option<Vec<String>>,
    #[serde(default)]
    pub disabled_tools: Vec<String>,
    #[serde(default)]
    pub approval_policy: mcp::McpApprovalPolicy,
    #[serde(default)]
    pub tool_approval_overrides: BTreeMap<String, mcp::McpApprovalPolicy>,
    pub transport: McpTransportUpdate,
}

/// 不落盘的严格单 Server 草稿测试请求。
#[derive(Debug, Deserialize)]
pub struct McpDraftTestRequest {
    #[serde(default)]
    pub source_name: Option<String>,
    #[serde(default)]
    pub source_revision: Option<String>,
    #[serde(default)]
    pub include_resources: bool,
    pub server: McpServerCreateRequest,
}

/// MCP server 更新请求。
#[derive(Debug, Deserialize)]
pub struct McpServerUpdateRequest {
    pub revision: String,
    #[serde(flatten)]
    pub server: McpServerCreateRequest,
}

/// MCP 凭据的非敏感状态。
#[derive(Debug, Serialize)]
pub struct McpSecretFieldState {
    pub target: String,
    pub configured: bool,
}

/// MCP 传输配置响应。
#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum McpTransportResponse {
    Stdio {
        command: String,
        args: Vec<String>,
        cwd: Option<String>,
        env: BTreeMap<String, String>,
        secrets: Vec<McpSecretFieldState>,
    },
    StreamableHttp {
        url: String,
        headers: BTreeMap<String, String>,
        header_secrets: Vec<McpSecretFieldState>,
        bearer_token: Option<McpSecretFieldState>,
    },
}

/// MCP server 列表项。
#[derive(Debug, Serialize)]
pub struct McpServerSummaryResponse {
    pub name: String,
    pub enabled: bool,
    pub transport: String,
    pub revision: String,
    pub status: String,
    pub tested_revision: Option<String>,
    pub tool_count: usize,
    pub resource_count: usize,
    pub last_checked_at: Option<String>,
    pub last_error: Option<mcp::McpStructuredError>,
    pub policy_reason: Option<String>,
}

/// MCP server 详情。
#[derive(Debug, Serialize)]
pub struct McpServerDetailResponse {
    pub name: String,
    pub enabled: bool,
    pub request_timeout_ms: Option<u64>,
    pub enabled_tools: Option<Vec<String>>,
    pub disabled_tools: Vec<String>,
    pub approval_policy: mcp::McpApprovalPolicy,
    pub tool_approval_overrides: BTreeMap<String, mcp::McpApprovalPolicy>,
    pub transport: McpTransportResponse,
    pub revision: String,
}

/// MCP 连接测试或目录刷新结果。
#[derive(Debug, Serialize)]
pub struct McpCatalogResponse {
    pub server: String,
    pub revision: String,
    pub status: String,
    pub tools: Vec<Value>,
    pub resources: Vec<Value>,
    pub errors: Vec<Value>,
    pub refreshed_at: String,
    pub diagnostic: McpCatalogDiagnosticResponse,
}

/// MCP 测试返回的本地诊断与固定网络策略。
#[derive(Debug, Serialize)]
pub struct McpCatalogDiagnosticResponse {
    pub connection: Option<mcp::McpConnectionDiagnostic>,
    pub policy_reason: Option<String>,
    pub redirect_policy: &'static str,
    pub proxy_policy: &'static str,
    pub sensitive_headers: &'static str,
}

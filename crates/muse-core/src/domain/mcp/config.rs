//! `config.toml` 中的 MCP Server Profile 与运行时不可变快照。

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

const MAX_REQUEST_TIMEOUT_MS: u64 = 120_000;

fn default_enabled() -> bool {
    true
}

/// MCP 工具的本地审批策略。远端 annotations 只能参与只读证明，不能自行授权。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpApprovalPolicy {
    /// 所有调用都需要逐次审批。
    #[default]
    AlwaysAsk,
    /// 仅当远端同时声明 readOnlyHint 时，允许免审批执行。
    TrustedReadOnly,
}

impl McpApprovalPolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AlwaysAsk => "always_ask",
            Self::TrustedReadOnly => "trusted_read_only",
        }
    }
}

/// 单个 MCP Server 的完整声明式配置。
///
/// API Key 明文与普通传输参数保存在同一个 Profile 中；自定义 `Debug` 严禁输出秘密。
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct McpServerProfile {
    #[serde(rename = "type")]
    pub transport: String,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub request_timeout_ms: Option<u64>,
    #[serde(default)]
    pub enabled_tools: Option<Vec<String>>,
    #[serde(default)]
    pub disabled_tools: Vec<String>,
    #[serde(default)]
    pub approval_policy: McpApprovalPolicy,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tool_approval_overrides: BTreeMap<String, McpApprovalPolicy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub secret_env: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub secret_headers: BTreeMap<String, String>,
}

impl std::fmt::Debug for McpServerProfile {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("McpServerProfile")
            .field("transport", &self.transport)
            .field("enabled", &self.enabled)
            .field("request_timeout_ms", &self.request_timeout_ms)
            .field("approval_policy", &self.approval_policy)
            .field(
                "tool_approval_override_names",
                &self.tool_approval_overrides.keys(),
            )
            .field("command", &self.command)
            .field("url", &self.url)
            .field("secret_env_fields", &self.secret_env.keys())
            .field("secret_header_fields", &self.secret_headers.keys())
            .finish_non_exhaustive()
    }
}

impl McpServerProfile {
    /// 校验配置结构和 transport 边界；错误不得包含任何配置值。
    pub fn validate(&self, name: &str) -> Result<(), String> {
        validate_server_name(name)?;
        if let Some(timeout) = self.request_timeout_ms
            && !(1_000..=MAX_REQUEST_TIMEOUT_MS).contains(&timeout)
        {
            return Err(format!(
                "外部 MCP server `{name}` 的 request_timeout_ms 必须在 1000 到 120000 之间。"
            ));
        }
        validate_secret_fields(name, "secret_env", &self.secret_env)?;
        validate_secret_fields(name, "secret_headers", &self.secret_headers)?;
        for tool_name in self.tool_approval_overrides.keys() {
            if tool_name.trim().is_empty()
                || tool_name.len() > 256
                || tool_name.chars().any(char::is_control)
            {
                return Err(format!(
                    "外部 MCP server `{name}` 包含无效的逐工具审批覆盖名称。"
                ));
            }
        }
        if self.secret_env.keys().any(|key| self.env.contains_key(key)) {
            return Err(format!(
                "外部 MCP server `{name}` 的同一环境变量不能同时出现在 env 和 secret_env。"
            ));
        }
        if maps_have_case_insensitive_overlap(&self.headers, &self.secret_headers) {
            return Err(format!(
                "外部 MCP server `{name}` 的同一 Header 不能同时出现在 headers 和 secret_headers。"
            ));
        }

        match self.transport.as_str() {
            "stdio" => {
                if self
                    .command
                    .as_deref()
                    .is_none_or(|value| value.trim().is_empty())
                {
                    return Err(format!("外部 MCP server `{name}` 缺少 command。"));
                }
                if self.url.is_some() || !self.headers.is_empty() || !self.secret_headers.is_empty()
                {
                    return Err(format!(
                        "外部 MCP server `{name}` 的 stdio transport 不能包含 url 或 Header 配置。"
                    ));
                }
                for key in self.env.keys().chain(self.secret_env.keys()) {
                    validate_environment_name(name, key)?;
                }
                if self.env.keys().any(|key| looks_sensitive_env_name(key)) {
                    return Err(format!(
                        "外部 MCP server `{name}` 的敏感环境变量必须放入 secret_env。"
                    ));
                }
            }
            "streamable_http" => {
                if self
                    .url
                    .as_deref()
                    .is_none_or(|value| value.trim().is_empty())
                {
                    return Err(format!("外部 MCP server `{name}` 缺少 url。"));
                }
                let url = self.url.as_deref().unwrap_or_default();
                let parsed = reqwest::Url::parse(url).ok();
                if parsed
                    .as_ref()
                    .is_none_or(|url| !matches!(url.scheme(), "http" | "https"))
                {
                    return Err(format!(
                        "外部 MCP server `{name}` 的 url 必须是有效的 HTTP 或 HTTPS 地址。"
                    ));
                }
                let parsed = parsed.expect("上方已经确认 URL 可解析");
                if parsed.host_str().is_none()
                    || !parsed.username().is_empty()
                    || parsed.password().is_some()
                    || parsed.fragment().is_some()
                {
                    return Err(format!(
                        "外部 MCP server `{name}` 的 url 必须包含主机，且不能携带用户凭据或 fragment。"
                    ));
                }
                if self.command.is_some()
                    || self.args.is_some()
                    || self.cwd.is_some()
                    || !self.env.is_empty()
                    || !self.secret_env.is_empty()
                {
                    return Err(format!(
                        "外部 MCP server `{name}` 的 streamable_http transport 不能包含 stdio 配置。"
                    ));
                }
                let mut normalized_headers = std::collections::BTreeSet::new();
                for key in self.headers.keys().chain(self.secret_headers.keys()) {
                    if reqwest::header::HeaderName::from_bytes(key.as_bytes()).is_err() {
                        return Err(format!("外部 MCP server `{name}` 包含无效的 Header 名称。"));
                    }
                    if !normalized_headers.insert(key.to_ascii_lowercase()) {
                        return Err(format!("外部 MCP server `{name}` 包含重复的 Header 名称。"));
                    }
                }
                if self
                    .headers
                    .keys()
                    .any(|key| looks_sensitive_header_name(key))
                {
                    return Err(format!(
                        "外部 MCP server `{name}` 的敏感 Header 必须放入 secret_headers。"
                    ));
                }
            }
            other => {
                return Err(format!(
                    "外部 MCP server `{name}` 使用未知 transport `{other}`，仅支持 stdio 和 streamable_http。"
                ));
            }
        }
        Ok(())
    }

    /// 转为已有 MCP 运行时解析器使用的内存对象；秘密只存在于进程内副本。
    pub(crate) fn to_runtime_value(&self) -> Value {
        let mut object = serde_json::Map::new();
        object.insert("type".to_string(), Value::String(self.transport.clone()));
        object.insert("enabled".to_string(), Value::Bool(self.enabled));
        if let Some(timeout) = self.request_timeout_ms {
            object.insert("request_timeout_ms".to_string(), Value::from(timeout));
        }
        if let Some(enabled_tools) = &self.enabled_tools {
            object.insert(
                "enabled_tools".to_string(),
                serde_json::json!(enabled_tools),
            );
        }
        if !self.disabled_tools.is_empty() {
            object.insert(
                "disabled_tools".to_string(),
                serde_json::json!(self.disabled_tools),
            );
        }
        object.insert(
            "approval_policy".to_string(),
            Value::String(self.approval_policy.as_str().to_string()),
        );
        if !self.tool_approval_overrides.is_empty() {
            object.insert(
                "tool_approval_overrides".to_string(),
                serde_json::json!(self.tool_approval_overrides),
            );
        }
        match self.transport.as_str() {
            "stdio" => {
                if let Some(command) = &self.command {
                    object.insert("command".to_string(), Value::String(command.clone()));
                }
                object.insert(
                    "args".to_string(),
                    serde_json::json!(self.args.clone().unwrap_or_default()),
                );
                if let Some(cwd) = &self.cwd {
                    object.insert("cwd".to_string(), Value::String(cwd.clone()));
                }
                let mut env = self.env.clone();
                env.extend(self.secret_env.clone());
                object.insert("env".to_string(), serde_json::json!(env));
            }
            "streamable_http" => {
                if let Some(url) = &self.url {
                    object.insert("url".to_string(), Value::String(url.clone()));
                }
                let mut headers = self.headers.clone();
                headers.extend(self.secret_headers.clone());
                object.insert("headers".to_string(), serde_json::json!(headers));
            }
            _ => {}
        }
        Value::Object(object)
    }
}

/// `config.toml` 的 MCP 顶层配置段。
#[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct McpProfileConfig {
    #[serde(default)]
    pub mcp_servers: BTreeMap<String, McpServerProfile>,
}

impl std::fmt::Debug for McpProfileConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("McpProfileConfig")
            .field("server_names", &self.mcp_servers.keys())
            .finish()
    }
}

impl McpProfileConfig {
    pub fn validate(&self) -> Result<(), String> {
        for (name, profile) in &self.mcp_servers {
            profile.validate(name)?;
        }
        Ok(())
    }

    pub fn revision(&self) -> u64 {
        let digest = self.signature_bytes();
        u64::from_be_bytes(digest[..8].try_into().unwrap_or_default())
    }

    fn signature_bytes(&self) -> [u8; 32] {
        let bytes = serde_json::to_vec(&self.mcp_servers).unwrap_or_default();
        Sha256::digest(bytes).into()
    }

    pub fn runtime_snapshot(&self, config_path: PathBuf) -> McpRuntimeSnapshot {
        let mut hasher = Sha256::new();
        hasher.update(self.signature_bytes());
        hasher.update(config_path.to_string_lossy().as_bytes());
        McpRuntimeSnapshot {
            profiles: self.clone(),
            config_path,
            config_hash: format!("{:x}", hasher.finalize()),
        }
    }
}

/// 从共享配置 Store 冻结出的 MCP 运行时输入。
#[derive(Clone)]
pub struct McpRuntimeSnapshot {
    profiles: McpProfileConfig,
    config_path: PathBuf,
    config_hash: String,
}

impl McpRuntimeSnapshot {
    pub fn config_path(&self) -> &PathBuf {
        &self.config_path
    }

    pub fn config_hash(&self) -> &str {
        &self.config_hash
    }

    pub(crate) fn runtime_entries(&self) -> BTreeMap<String, Value> {
        self.profiles
            .mcp_servers
            .iter()
            .map(|(name, profile)| (name.clone(), profile.to_runtime_value()))
            .collect()
    }
}

fn validate_server_name(name: &str) -> Result<(), String> {
    if !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-'))
    {
        return Ok(());
    }
    Err(format!(
        "外部 MCP server 名称 `{name}` 无效，只允许 1-64 个字母、数字、下划线和短横线。"
    ))
}

fn maps_have_case_insensitive_overlap(
    ordinary: &BTreeMap<String, String>,
    sensitive: &BTreeMap<String, String>,
) -> bool {
    ordinary.keys().any(|ordinary_key| {
        sensitive
            .keys()
            .any(|key| key.eq_ignore_ascii_case(ordinary_key))
    })
}

fn validate_environment_name(server_name: &str, name: &str) -> Result<(), String> {
    if !name.is_empty()
        && name.len() <= 128
        && name
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '_')
    {
        return Ok(());
    }
    Err(format!(
        "外部 MCP server `{server_name}` 包含无效的环境变量名称。"
    ))
}

fn looks_sensitive_env_name(name: &str) -> bool {
    let normalized = name.to_ascii_uppercase();
    [
        "TOKEN",
        "SECRET",
        "PASSWORD",
        "PASSWD",
        "API_KEY",
        "PRIVATE_KEY",
    ]
    .iter()
    .any(|marker| normalized == *marker || normalized.ends_with(&format!("_{marker}")))
}

fn looks_sensitive_header_name(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "authorization"
            | "proxy-authorization"
            | "cookie"
            | "set-cookie"
            | "x-api-key"
            | "api-key"
            | "x-auth-token"
    )
}

fn validate_secret_fields(
    server_name: &str,
    field: &str,
    values: &BTreeMap<String, String>,
) -> Result<(), String> {
    if values.values().any(|value| value.trim().is_empty()) {
        return Err(format!(
            "外部 MCP server `{server_name}` 的 `{field}` 不能包含空秘密值。"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{McpProfileConfig, McpServerProfile};
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    fn stdio_profile() -> McpServerProfile {
        McpServerProfile {
            transport: "stdio".to_string(),
            enabled: true,
            request_timeout_ms: Some(30_000),
            enabled_tools: None,
            disabled_tools: Vec::new(),
            approval_policy: Default::default(),
            tool_approval_overrides: BTreeMap::new(),
            command: Some("demo".to_string()),
            args: Some(vec!["--serve".to_string()]),
            cwd: None,
            url: None,
            env: BTreeMap::from([("LOG_LEVEL".to_string(), "info".to_string())]),
            secret_env: BTreeMap::from([("TOKEN".to_string(), "top-secret".to_string())]),
            headers: BTreeMap::new(),
            secret_headers: BTreeMap::new(),
        }
    }

    #[test]
    fn validates_transport_boundaries_and_builds_runtime_snapshot() {
        let mut profiles = McpProfileConfig::default();
        profiles
            .mcp_servers
            .insert("demo".to_string(), stdio_profile());
        profiles.validate().expect("有效 stdio Profile 应通过校验");
        let snapshot = profiles.runtime_snapshot(PathBuf::from("config.toml"));
        let raw = snapshot.runtime_entries();
        assert_eq!(raw["demo"]["env"]["TOKEN"], "top-secret");
        assert!(!snapshot.config_hash().contains("top-secret"));
    }

    #[test]
    fn debug_never_contains_secret_values() {
        let profile = stdio_profile();
        assert!(!format!("{profile:?}").contains("top-secret"));
    }

    #[test]
    fn validates_http_url_and_case_insensitive_header_collisions() {
        let mut profile = McpServerProfile {
            transport: "streamable_http".to_string(),
            enabled: true,
            request_timeout_ms: None,
            enabled_tools: None,
            disabled_tools: Vec::new(),
            approval_policy: Default::default(),
            tool_approval_overrides: BTreeMap::new(),
            command: None,
            args: None,
            cwd: None,
            url: Some("not-a-url".to_string()),
            env: BTreeMap::new(),
            secret_env: BTreeMap::new(),
            headers: BTreeMap::new(),
            secret_headers: BTreeMap::new(),
        };
        assert!(
            profile
                .validate("remote")
                .expect_err("无效 URL 必须拒绝")
                .contains("HTTP 或 HTTPS")
        );

        profile.url = Some("https://example.test/mcp".to_string());
        profile
            .headers
            .insert("authorization".to_string(), "ordinary".to_string());
        profile
            .secret_headers
            .insert("Authorization".to_string(), "secret".to_string());
        assert!(
            profile
                .validate("remote")
                .expect_err("Header 名称大小写冲突必须拒绝")
                .contains("同一 Header")
        );
    }

    #[test]
    fn persists_local_approval_policy_and_rejects_secrets_in_ordinary_fields() {
        let mut profile = stdio_profile();
        profile.approval_policy = super::McpApprovalPolicy::TrustedReadOnly;
        profile
            .tool_approval_overrides
            .insert("dangerous".to_string(), super::McpApprovalPolicy::AlwaysAsk);
        let value = profile.to_runtime_value();
        assert_eq!(value["approval_policy"], "trusted_read_only");
        assert_eq!(value["tool_approval_overrides"]["dangerous"], "always_ask");

        profile
            .env
            .insert("PLAINTEXT_TOKEN".to_string(), "unsafe".to_string());
        assert!(
            profile
                .validate("local")
                .expect_err("敏感环境变量不得放入普通字段")
                .contains("secret_env")
        );

        let mut http = McpServerProfile {
            transport: "streamable_http".to_string(),
            enabled: true,
            request_timeout_ms: None,
            enabled_tools: None,
            disabled_tools: Vec::new(),
            approval_policy: Default::default(),
            tool_approval_overrides: BTreeMap::new(),
            command: None,
            args: None,
            cwd: None,
            url: Some("https://example.test/mcp".to_string()),
            env: BTreeMap::new(),
            secret_env: BTreeMap::new(),
            headers: BTreeMap::from([("Authorization".to_string(), "unsafe".to_string())]),
            secret_headers: BTreeMap::new(),
        };
        assert!(
            http.validate("remote")
                .expect_err("敏感 Header 不得放入普通字段")
                .contains("secret_headers")
        );
        http.headers.clear();
        http.url = Some("https://user:password@example.test/mcp".to_string());
        assert!(
            http.validate("remote")
                .expect_err("URL 不得携带凭据")
                .contains("用户凭据")
        );
    }
}

//! 供应商专属能力适配层。
//!
//! `profile` 只描述通用能力和默认值；余额检测、专属健康检查、特殊资源接口等
//! 放在这里按供应商拆分。新增供应商时优先增加对应子模块和调度分支。

mod deepseek;

use serde::{Deserialize, Serialize};

use crate::model::profile::provider_profile_for_identity;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ProviderSupportCapabilities {
    pub provider_id: String,
    pub supports_balance_check: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderBalanceInfo {
    pub currency: String,
    pub total_balance: String,
    pub granted_balance: String,
    pub topped_up_balance: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderBalance {
    pub provider_id: String,
    pub is_available: bool,
    pub balance_infos: Vec<ProviderBalanceInfo>,
}

#[derive(Debug)]
pub enum ProviderSupportError {
    Unsupported(String),
    Config(String),
    Network(String),
    Api { status: u16, message: String },
    Parse(String),
}

impl std::fmt::Display for ProviderSupportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProviderSupportError::Unsupported(message) => {
                write!(f, "不支持的供应商能力：{message}")
            }
            ProviderSupportError::Config(message) => write!(f, "供应商配置错误：{message}"),
            ProviderSupportError::Network(message) => write!(f, "供应商网络错误：{message}"),
            ProviderSupportError::Api { message, .. } => write!(f, "供应商接口错误：{message}"),
            ProviderSupportError::Parse(message) => write!(f, "供应商响应解析失败：{message}"),
        }
    }
}

impl std::error::Error for ProviderSupportError {}

pub fn provider_support_capabilities(
    provider: &str,
    api_base: &str,
) -> ProviderSupportCapabilities {
    let profile = provider_profile_for_identity(provider, api_base);
    let provider_id = profile
        .map(|profile| profile.id.to_string())
        .unwrap_or_else(|| provider.trim().to_string());
    ProviderSupportCapabilities {
        provider_id,
        supports_balance_check: profile.is_some_and(|profile| profile.id == "deepseek"),
    }
}

pub async fn fetch_provider_balance(
    provider: &str,
    api_base: &str,
    api_key: &str,
) -> Result<ProviderBalance, ProviderSupportError> {
    match provider_profile_for_identity(provider, api_base).map(|profile| profile.id) {
        Some("deepseek") => deepseek::fetch_balance(api_base, api_key).await,
        Some(provider_id) => Err(ProviderSupportError::Unsupported(format!(
            "`{provider_id}` 暂未实现余额检测。"
        ))),
        None => Err(ProviderSupportError::Unsupported(format!(
            "`{}` 暂未实现余额检测。",
            provider.trim()
        ))),
    }
}

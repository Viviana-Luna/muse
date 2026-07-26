//! 模型与提供器接口的请求与响应 DTO。

use muse_core::model::catalog::{ModelCatalogItem, ModelCatalogModelDraft};
use muse_core::model::vendor::ProviderBalanceInfo;
use serde::{Deserialize, Serialize};

/// 当前模型概要响应体。
#[derive(Serialize)]
pub struct ModelsResponse {
    /// 兼容旧前端的供应商展示名。
    pub provider: String,
    /// 兼容旧前端的上游模型 ID。
    pub model: String,
    pub provider_id: String,
    pub provider_name: String,
    pub model_name: String,
}

/// 拉取提供器模型列表请求体。
#[derive(Deserialize)]
pub struct FetchModelCatalogRequest {
    pub provider: String,
    pub purpose: String,
    pub api_base: String,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub api_key: Option<String>,
}

/// 拉取提供器模型列表响应体。
#[derive(Serialize)]
pub struct FetchModelCatalogResponse {
    pub provider_id: String,
    pub models: Vec<ModelCatalogItem>,
}

/// 模型目录创建与编辑请求。模型 ID 在编辑时保持不变。
pub type ModelCatalogMutationRequest = ModelCatalogModelDraft;

/// 删除模型目录项请求。
#[derive(Deserialize)]
pub struct ModelCatalogDeleteRequest {
    pub provider_id: String,
    pub model: String,
}

/// 切换当前聊天活动模型请求。
#[derive(Deserialize)]
pub struct ActiveChatModelUpdateRequest {
    pub provider_id: String,
    pub model: String,
}

/// 当前聊天活动模型响应，供应商与模型名称分别返回。
#[derive(Debug, Serialize)]
pub struct ActiveChatModelResponse {
    pub provider_id: String,
    pub provider_name: String,
    pub model: String,
    pub model_name: String,
}

/// 供应商凭据配置状态；响应不包含密钥、掩码或凭据引用。
#[derive(Serialize)]
pub struct ProviderCredentialResponse {
    pub provider_id: String,
    pub api_key_configured: bool,
    pub status: String,
}

/// 供应商启停请求。缺少字段会由 JSON 反序列化拒绝，避免把不完整请求误当作开启。
#[derive(Deserialize)]
pub struct ProviderStateUpdateRequest {
    pub enabled: bool,
}

/// 供应商专属余额检测请求体。
#[derive(Deserialize)]
pub struct ProviderBalanceRequest {
    pub provider: String,
    pub api_base: String,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub purpose: Option<String>,
}

/// 供应商专属余额检测响应体。
#[derive(Serialize)]
pub struct ProviderBalanceResponse {
    pub provider_id: String,
    pub is_available: bool,
    pub balance_infos: Vec<ProviderBalanceInfo>,
    pub status: String,
}

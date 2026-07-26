/// 模型相关能力分层：
/// - `catalog`：模型目录 DTO 与校验。
/// - `config`：由当前 Provider Profile 解析出的运行时兼容结构。
/// - `profile`：供应商能力和 OpenAI 兼容请求差异的集中描述。
/// - `profile_config`：`config.toml` 中的完整 Provider Profile 与活动选择。
/// - `provider`：聊天模型 provider 协议适配，把内部会话和工具定义转换成上游 API。
/// - `vendor`：供应商专属能力，如余额检测和特殊健康检查。
pub mod catalog;
pub mod config;
pub mod profile;
pub mod profile_config;
pub mod provider;
pub mod vendor;

pub use catalog::{ModelCatalog, ModelCatalogError, ModelCatalogItem, ModelProviderCatalog};
pub use config::{LlmConfig, ModelsConfig, SpeechRecognitionConfig, TtsConfig, VoiceInputConfig};
pub use profile::{
    ChatCompletionsRequestOptions, ModelCapabilityDefaults, ModelProviderProfile,
    chat_completions_request_options, model_capability_defaults,
};
pub use profile_config::{
    ActiveModelSelection, ActiveModelsConfig, ChatModelReferenceError, ModelProfileConfig,
    ProviderModelConfig, ProviderProfileConfig, ResolvedChatModel,
};
pub use vendor::{
    ProviderBalance, ProviderBalanceInfo, ProviderSupportCapabilities, ProviderSupportError,
    fetch_provider_balance, provider_support_capabilities,
};

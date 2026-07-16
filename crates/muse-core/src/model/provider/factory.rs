//! 聊天模型 provider 工厂，根据运行时配置选择真实模型服务。

use super::openai::OpenAiProvider;
use super::{ChatModelError, ChatModelProvider};
use crate::model::config::LlmConfig;

/// 根据配置创建聊天模型 provider。
pub fn create_provider(config: &LlmConfig) -> Result<Box<dyn ChatModelProvider>, ChatModelError> {
    match config.provider.trim().to_ascii_lowercase().as_str() {
        "deepseek" | "volcengine_agent_plan" => Ok(Box::new(OpenAiProvider::new(config.clone()))),
        other => Err(ChatModelError::ConfigError(format!(
            "不支持的聊天模型 provider：'{other}'。当前仅支持 DeepSeek 和火山方舟 Agent Plan。"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::create_provider;
    use crate::model::config::LlmConfig;

    #[test]
    fn only_two_chat_providers_are_supported() {
        for provider in ["deepseek", "volcengine_agent_plan"] {
            let config = LlmConfig {
                provider: provider.to_string(),
                ..Default::default()
            };
            assert!(create_provider(&config).is_ok(), "应支持 {provider}");
        }

        for provider in [
            "openai",
            "azure",
            "ollama",
            "minimax",
            "kimi",
            "custom",
            "openrouter",
            "volcengine_ark",
        ] {
            let config = LlmConfig {
                provider: provider.to_string(),
                ..Default::default()
            };
            assert!(create_provider(&config).is_err(), "不应支持 {provider}");
        }
    }
}

//! 模型供应商兼容 profile。
//!
//! 这里集中描述 OpenAI 兼容供应商的能力默认值和请求差异。当前只落 DeepSeek，
//! 后续增加其他供应商时优先扩展本模块，避免在 web handler 或 provider 里散落分支。

pub const DEFAULT_CONTEXT_WINDOW_TOKENS: u64 = 200_000;
pub const DEFAULT_MAX_OUTPUT_TOKENS: u32 = 2048;
pub const DEFAULT_TOKENIZER_FAMILY: &str = "rough_estimate";

pub const DEEPSEEK_CONTEXT_WINDOW_TOKENS: u64 = 1_000_000;
pub const DEEPSEEK_MAX_OUTPUT_TOKENS: u32 = 384_000;
pub const VOLCENGINE_AGENT_PLAN_DEFAULT_MODEL: &str = "doubao-seed-2.0-pro";
pub const VOLCENGINE_AGENT_PLAN_CONTEXT_WINDOW_TOKENS: u64 = 262_144;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelCapabilityDefaults {
    pub context_window: u64,
    pub default_max_output_tokens: u32,
    pub supports_usage: bool,
    pub supports_cached_tokens: bool,
    pub supports_reasoning_tokens: bool,
    pub tokenizer_family: &'static str,
}

impl ModelCapabilityDefaults {
    pub const fn generic() -> Self {
        Self {
            context_window: DEFAULT_CONTEXT_WINDOW_TOKENS,
            default_max_output_tokens: DEFAULT_MAX_OUTPUT_TOKENS,
            supports_usage: true,
            supports_cached_tokens: true,
            supports_reasoning_tokens: true,
            tokenizer_family: DEFAULT_TOKENIZER_FAMILY,
        }
    }
}

pub const DEFAULT_MODEL_CAPABILITY_DEFAULTS: ModelCapabilityDefaults =
    ModelCapabilityDefaults::generic();

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChatCompletionsRequestOptions {
    pub include_stream_usage: bool,
}

impl ChatCompletionsRequestOptions {
    pub const fn generic() -> Self {
        Self {
            include_stream_usage: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelProviderProfile {
    pub id: &'static str,
    pub name: &'static str,
    pub default_api_base: &'static str,
    pub chat_model_list_url: &'static str,
    pub notes: &'static str,
    pub model_defaults: ModelCapabilityDefaults,
    pub chat_completions_options: ChatCompletionsRequestOptions,
    provider_aliases: &'static [&'static str],
    api_base_markers: &'static [&'static str],
    model_prefixes: &'static [&'static str],
}

impl ModelProviderProfile {
    fn matches_identity(&self, provider: &str, api_base: &str) -> bool {
        let provider = provider.trim().to_ascii_lowercase();
        if !provider.is_empty()
            && (provider == self.id
                || self
                    .provider_aliases
                    .iter()
                    .any(|alias| provider == alias.trim().to_ascii_lowercase()))
        {
            return true;
        }

        let api_base = api_base.trim().to_ascii_lowercase();
        !api_base.is_empty()
            && self
                .api_base_markers
                .iter()
                .any(|marker| api_base.contains(&marker.trim().to_ascii_lowercase()))
    }

    fn matches_model(&self, model: &str) -> bool {
        let model = model.trim().to_ascii_lowercase();
        !model.is_empty()
            && self
                .model_prefixes
                .iter()
                .any(|prefix| model.starts_with(&prefix.trim().to_ascii_lowercase()))
    }
}

pub const DEEPSEEK_PROVIDER_PROFILE: ModelProviderProfile = ModelProviderProfile {
    id: "deepseek",
    name: "DeepSeek",
    default_api_base: "https://api.deepseek.com",
    chat_model_list_url: "https://api.deepseek.com/models",
    notes: "DeepSeek OpenAI 兼容接口，默认按 1M 上下文模型能力处理。",
    model_defaults: ModelCapabilityDefaults {
        context_window: DEEPSEEK_CONTEXT_WINDOW_TOKENS,
        default_max_output_tokens: DEEPSEEK_MAX_OUTPUT_TOKENS,
        supports_usage: true,
        supports_cached_tokens: true,
        supports_reasoning_tokens: true,
        tokenizer_family: DEFAULT_TOKENIZER_FAMILY,
    },
    chat_completions_options: ChatCompletionsRequestOptions {
        include_stream_usage: true,
    },
    provider_aliases: &["deepseek"],
    api_base_markers: &["api.deepseek.com"],
    model_prefixes: &["deepseek-"],
};

pub const VOLCENGINE_AGENT_PLAN_PROVIDER_PROFILE: ModelProviderProfile = ModelProviderProfile {
    id: "volcengine_agent_plan",
    name: "火山方舟 Agent Plan",
    default_api_base: "https://ark.cn-beijing.volces.com/api/plan/v3",
    chat_model_list_url: "",
    notes: "火山方舟 Agent Plan 专属 Chat Completions 接口；使用套餐 API Key 和套餐模型名。",
    model_defaults: ModelCapabilityDefaults {
        context_window: VOLCENGINE_AGENT_PLAN_CONTEXT_WINDOW_TOKENS,
        default_max_output_tokens: DEFAULT_MAX_OUTPUT_TOKENS,
        supports_usage: true,
        supports_cached_tokens: false,
        supports_reasoning_tokens: true,
        tokenizer_family: DEFAULT_TOKENIZER_FAMILY,
    },
    chat_completions_options: ChatCompletionsRequestOptions {
        include_stream_usage: true,
    },
    provider_aliases: &["volcengine_agent_plan"],
    api_base_markers: &["ark.cn-beijing.volces.com/api/plan/v3"],
    model_prefixes: &["doubao-seed-2.0-"],
};

const KNOWN_PROVIDER_PROFILES: &[ModelProviderProfile] = &[
    DEEPSEEK_PROVIDER_PROFILE,
    VOLCENGINE_AGENT_PLAN_PROVIDER_PROFILE,
];

pub fn known_provider_profiles() -> &'static [ModelProviderProfile] {
    KNOWN_PROVIDER_PROFILES
}

pub fn provider_profile_for_identity(
    provider: &str,
    api_base: &str,
) -> Option<&'static ModelProviderProfile> {
    known_provider_profiles()
        .iter()
        .find(|profile| profile.matches_identity(provider, api_base))
}

pub fn provider_profile_for_model(
    provider: &str,
    api_base: &str,
    model: &str,
) -> Option<&'static ModelProviderProfile> {
    provider_profile_for_identity(provider, api_base).or_else(|| {
        known_provider_profiles()
            .iter()
            .find(|profile| profile.matches_model(model))
    })
}

pub fn model_capability_defaults(
    provider: &str,
    api_base: &str,
    model: &str,
) -> ModelCapabilityDefaults {
    provider_profile_for_model(provider, api_base, model)
        .map(|profile| profile.model_defaults)
        .unwrap_or(DEFAULT_MODEL_CAPABILITY_DEFAULTS)
}

pub fn chat_completions_request_options(
    provider: &str,
    api_base: &str,
) -> ChatCompletionsRequestOptions {
    provider_profile_for_identity(provider, api_base)
        .map(|profile| profile.chat_completions_options)
        .unwrap_or_else(ChatCompletionsRequestOptions::generic)
}

#[cfg(test)]
mod tests {
    use super::{
        DEEPSEEK_CONTEXT_WINDOW_TOKENS, DEEPSEEK_MAX_OUTPUT_TOKENS,
        VOLCENGINE_AGENT_PLAN_CONTEXT_WINDOW_TOKENS, VOLCENGINE_AGENT_PLAN_DEFAULT_MODEL,
        chat_completions_request_options, model_capability_defaults, provider_profile_for_identity,
        provider_profile_for_model,
    };

    fn deepseek_profile_matches_provider_api_base_and_model_prefix() {
        assert!(provider_profile_for_identity("deepseek", "").is_some());
        assert!(provider_profile_for_identity("custom", "https://api.deepseek.com/v1").is_some());
        assert!(provider_profile_for_model("custom", "", "deepseek-v4-pro").is_some());
    }

    fn deepseek_profile_exposes_model_defaults() {
        let defaults = model_capability_defaults("deepseek", "", "deepseek-v4-pro");

        assert_eq!(defaults.context_window, DEEPSEEK_CONTEXT_WINDOW_TOKENS);
        assert_eq!(
            defaults.default_max_output_tokens,
            DEEPSEEK_MAX_OUTPUT_TOKENS
        );
        assert!(defaults.supports_cached_tokens);
        assert!(defaults.supports_reasoning_tokens);
    }

    fn request_options_use_provider_identity_not_model_guess() {
        assert!(chat_completions_request_options("deepseek", "").include_stream_usage);
        assert!(
            chat_completions_request_options("custom", "https://api.deepseek.com")
                .include_stream_usage
        );
        assert!(!chat_completions_request_options("custom", "").include_stream_usage);
    }

    fn volcengine_agent_plan_has_independent_identity_and_defaults() {
        let profile = provider_profile_for_identity("volcengine_agent_plan", "")
            .expect("应识别火山方舟 Agent Plan provider");
        assert_eq!(
            profile.default_api_base,
            "https://ark.cn-beijing.volces.com/api/plan/v3"
        );
        assert_eq!(profile.chat_model_list_url, "");
        assert_eq!(
            profile.model_defaults.context_window,
            VOLCENGINE_AGENT_PLAN_CONTEXT_WINDOW_TOKENS
        );
        assert!(chat_completions_request_options("volcengine_agent_plan", "").include_stream_usage);
        assert!(
            provider_profile_for_model("custom", "", VOLCENGINE_AGENT_PLAN_DEFAULT_MODEL).is_some()
        );
        assert!(
            provider_profile_for_identity(
                "volcengine_ark",
                "https://ark.cn-beijing.volces.com/api/v3"
            )
            .is_none()
        );
    }

    #[test]
    fn provider_profile_matrix() {
        let cases: [(&str, fn()); 4] = [
            (
                "deepseek_profile_matches_provider_api_base_and_model_prefix",
                deepseek_profile_matches_provider_api_base_and_model_prefix,
            ),
            (
                "deepseek_profile_exposes_model_defaults",
                deepseek_profile_exposes_model_defaults,
            ),
            (
                "request_options_use_provider_identity_not_model_guess",
                request_options_use_provider_identity_not_model_guess,
            ),
            (
                "volcengine_agent_plan_has_independent_identity_and_defaults",
                volcengine_agent_plan_has_independent_identity_and_defaults,
            ),
        ];
        let failures = cases
            .into_iter()
            .filter_map(|(name, case)| {
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(case))
                    .is_err()
                    .then_some(name)
            })
            .collect::<Vec<_>>();
        assert!(failures.is_empty(), "失败场景：{}", failures.join("、"));
    }
}

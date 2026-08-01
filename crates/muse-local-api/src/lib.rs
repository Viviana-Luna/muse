//! Muse 本地 API 适配模块，暴露路由、安全上下文、运行时事件与共享状态。

mod api;
mod assets;
pub mod dto;
mod middleware;
mod platform;
pub mod router;
mod runtime_support;
pub mod security;
mod startup;
pub mod state;

/// 保留原有 HTTP 错误适配公共路径。
pub mod error {
    pub use crate::api::error::{
        bad_request, internal_error, model_catalog_error_response, persona_card_error_response,
        persona_store_error_response, visual_pack_store_error_response, voice_error_response,
    };
}

/// 保留原有 SSE 运行时事件公共路径。
pub mod runtime {
    pub use crate::runtime_support::events::{
        RuntimeEventEmitter, RuntimeSseSender, RuntimeTurnOutcome, runtime_event_payload,
    };
}

pub use router::build_router_with_security;
// 记忆服务接缝对接线方（临时集成分支启动流程）公开。
pub use runtime_support::{MemoryManagementAudit, MemoryServices, install_memory_services};
pub use security::{LocalApiBootstrap, LocalApiSecurity, LocalApiSecurityOptions};
pub use startup::migration::{
    LegacyDefaultPersonaMigrationOutcome, migrate_pristine_legacy_default_persona,
};

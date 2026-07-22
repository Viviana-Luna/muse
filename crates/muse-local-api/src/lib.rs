//! Muse 本地 API 适配模块，暴露 HTTP 路由、处理器、运行时事件和共享状态。

mod api;
mod asset_support;
mod command_environment;
pub mod dto;
pub mod error;
mod middleware;
mod platform_path;
pub mod router;
pub mod runtime;
mod runtime_support;
pub(crate) mod runtime_tools;
pub mod security;
mod startup_migration;
pub mod state;
mod tool_result_archive;
pub use router::build_router_with_security;
pub use security::{LocalApiBootstrap, LocalApiSecurity, LocalApiSecurityOptions};
pub use startup_migration::{
    LegacyDefaultPersonaMigrationOutcome, migrate_pristine_legacy_default_persona,
};

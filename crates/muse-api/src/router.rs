//! 路由组装模块，负责挂载本地 API、中间件和运行时状态。

use axum::{Extension, Router};
use std::sync::Arc;

use crate::api;
use crate::middleware::access_log::log_api_request;
use crate::runtime_support;
use crate::security::{LocalApiSecurity, enforce_local_api_security};
use crate::state::build_app_state;
use muse_core::config::Config;

/// 使用调用方已经绑定的实际地址和安全上下文构建路由。
///
/// 桌面宿主先绑定随机端口，再通过此入口确保 Host 校验、Bootstrap 与监听地址一致。
pub async fn build_router_with_security(
    config: Config,
    security: Arc<LocalApiSecurity>,
) -> Result<Router, Box<dyn std::error::Error>> {
    let state = build_app_state(config)?;
    runtime_support::initialize_active_persona_session(&state)
        .await
        .map_err(std::io::Error::other)?;
    let protected_api = api::routes()
        .layer(Extension(security.clone()))
        .layer(axum::middleware::from_fn_with_state(
            security,
            enforce_local_api_security,
        ))
        .layer(axum::middleware::from_fn(log_api_request));

    Ok(Router::new().nest("/api", protected_api).with_state(state))
}

#[cfg(test)]
fn api_routes() -> api::ApiRouter {
    api::routes()
}

#[cfg(test)]
mod tests;

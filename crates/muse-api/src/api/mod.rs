//! 本地 HTTP API 的业务域路由入口。

mod assets;
mod chat;
mod diagnostics;
pub(crate) mod error;
mod mcp;
mod memories;
mod models;
mod personas;
pub(crate) mod preferences;
mod runtime;
mod sessions;
mod skills;
mod tools;
mod voice;

use axum::Router;
use std::sync::Arc;

use crate::state::AppState;

pub(crate) type ApiRouter = Router<Arc<AppState>>;

/// 汇总各业务域 Router，不在此处注册具体接口。
pub(crate) fn routes() -> ApiRouter {
    Router::new()
        .merge(chat::routes())
        .merge(runtime::routes())
        .merge(preferences::routes())
        .merge(diagnostics::routes())
        .merge(assets::routes())
        .merge(sessions::routes())
        .merge(personas::routes())
        .merge(tools::routes())
        .merge(skills::routes())
        .merge(mcp::routes())
        .merge(memories::routes())
        .merge(models::routes())
        .merge(voice::routes())
}

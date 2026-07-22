//! 工具目录接口，包含路由和 HTTP 适配实现。

use std::sync::Arc;

use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};

use super::ApiRouter;
use crate::runtime_support;
use crate::state::AppState;

pub(super) fn routes() -> ApiRouter {
    Router::new().route("/tools", get(handle_tools))
}

/// 查询当前角色可暴露给模型的工具定义。
async fn handle_tools(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let active_persona = runtime_support::current_active_persona(&state).await;
    let definitions =
        runtime_support::runtime_tool_defs_for_policy(&state, active_persona.as_ref()).await;
    Json(serde_json::json!({ "tools": definitions }))
}

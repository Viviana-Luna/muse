//! 运行时 HTTP handler 拆分入口。

use axum::{Json, extract::State};
use std::sync::Arc;

use crate::dto::{RuntimeModeResponse, RuntimeTodosResponse};
use crate::state::AppState;

/// GET /api/runtime/mode — 查询当前运行模式。
pub(crate) async fn handle_runtime_mode(
    State(state): State<Arc<AppState>>,
) -> Json<RuntimeModeResponse> {
    Json(super::runtime_mode_response(
        super::current_runtime_mode_state(&state),
    ))
}

/// GET /api/runtime/todos — 查询当前运行时任务清单。
pub(crate) async fn handle_runtime_todos(
    State(state): State<Arc<AppState>>,
) -> Json<RuntimeTodosResponse> {
    let todos = super::current_runtime_todos(&state).await;
    let status = if todos.is_empty() {
        "当前没有活跃任务。".to_string()
    } else {
        format!("当前有 {} 个活跃任务。", todos.len())
    };
    Json(RuntimeTodosResponse { todos, status })
}

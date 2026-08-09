//! 运行时状态、交互和策略接口，集中展示路径与 HTTP 适配入口。

use std::sync::Arc;

use axum::extract::State;
use axum::routing::{get, post};
use axum::{Json, Router};

use super::ApiRouter;
use crate::dto::{RuntimeModeResponse, RuntimeTodosResponse};
use crate::runtime_support;
use crate::state::AppState;

pub(super) fn routes() -> ApiRouter {
    Router::new()
        .route(
            "/runtime/health",
            get(runtime_support::handle_runtime_health),
        )
        .route("/runtime/state", get(runtime_support::handle_runtime_state))
        .route(
            "/runtime/skills",
            get(runtime_support::handle_runtime_skills),
        )
        .route(
            "/runtime/ws-ticket",
            post(runtime_support::handle_runtime_ws_ticket),
        )
        .route(
            "/runtime/approvals/{id}/approve",
            post(runtime_support::handle_runtime_approval_approve),
        )
        .route(
            "/runtime/approvals/{id}/reject",
            post(runtime_support::handle_runtime_approval_reject),
        )
        .route(
            "/runtime/approvals/{id}/cancel",
            post(runtime_support::handle_runtime_approval_cancel),
        )
        .route(
            "/runtime/user-questions/{id}/answer",
            post(runtime_support::handle_runtime_user_question_answer),
        )
        .route(
            "/runtime/user-questions/{id}/cancel",
            post(runtime_support::handle_runtime_user_question_cancel),
        )
        .route(
            "/runtime/turns/{id}/cancel",
            post(runtime_support::handle_runtime_turn_cancel),
        )
        .route(
            "/runtime/mode",
            get(handle_runtime_mode).put(runtime_support::handle_runtime_mode_update),
        )
        .route(
            "/runtime/approval-mode",
            get(runtime_support::handle_runtime_approval_mode)
                .put(runtime_support::handle_runtime_approval_mode_update),
        )
        .route("/runtime/todos", get(handle_runtime_todos))
        .route(
            "/runtime/token-usage",
            get(runtime_support::handle_runtime_token_usage),
        )
        .route(
            "/runtime/context-snapshot",
            get(runtime_support::handle_runtime_context_snapshot),
        )
        .route(
            "/runtime/workspaces",
            get(runtime_support::handle_runtime_workspaces)
                .put(runtime_support::handle_runtime_workspace_policy_update),
        )
        .route("/ws", get(runtime_support::handle_ws))
}

/// 查询当前运行模式。
async fn handle_runtime_mode(State(state): State<Arc<AppState>>) -> Json<RuntimeModeResponse> {
    Json(runtime_support::runtime_mode_response(
        runtime_support::current_runtime_mode_state(&state),
    ))
}

/// 查询当前运行时任务清单。
async fn handle_runtime_todos(State(state): State<Arc<AppState>>) -> Json<RuntimeTodosResponse> {
    let todos = runtime_support::current_runtime_todos(&state).await;
    let status = if todos.is_empty() {
        "当前没有活跃任务。".to_string()
    } else {
        format!("当前有 {} 个活跃任务。", todos.len())
    };
    Json(RuntimeTodosResponse { todos, status })
}

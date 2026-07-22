//! 运行时状态、交互和策略接口。

use axum::Router;
use axum::routing::{get, post};

use super::ApiRouter;
use crate::handlers;

pub(super) fn routes() -> ApiRouter {
    Router::new()
        .route("/runtime/health", get(handlers::handle_runtime_health))
        .route("/runtime/state", get(handlers::handle_runtime_state))
        .route("/runtime/skills", get(handlers::handle_runtime_skills))
        .route(
            "/runtime/ws-ticket",
            post(handlers::handle_runtime_ws_ticket),
        )
        .route(
            "/runtime/approvals/{id}/approve",
            post(handlers::handle_runtime_approval_approve),
        )
        .route(
            "/runtime/approvals/{id}/reject",
            post(handlers::handle_runtime_approval_reject),
        )
        .route(
            "/runtime/approvals/{id}/cancel",
            post(handlers::handle_runtime_approval_cancel),
        )
        .route(
            "/runtime/user-questions/{id}/answer",
            post(handlers::handle_runtime_user_question_answer),
        )
        .route(
            "/runtime/user-questions/{id}/cancel",
            post(handlers::handle_runtime_user_question_cancel),
        )
        .route(
            "/runtime/turns/{id}/cancel",
            post(handlers::handle_runtime_turn_cancel),
        )
        .route(
            "/runtime/mode",
            get(handlers::handle_runtime_mode).put(handlers::handle_runtime_mode_update),
        )
        .route(
            "/runtime/approval-mode",
            get(handlers::handle_runtime_approval_mode)
                .put(handlers::handle_runtime_approval_mode_update),
        )
        .route("/runtime/todos", get(handlers::handle_runtime_todos))
        .route(
            "/runtime/token-usage",
            get(handlers::handle_runtime_token_usage),
        )
        .route(
            "/runtime/context-snapshot",
            get(handlers::handle_runtime_context_snapshot),
        )
        .route(
            "/runtime/workspaces",
            get(handlers::handle_runtime_workspaces)
                .put(handlers::handle_runtime_workspace_policy_update),
        )
        .route("/ws", get(handlers::handle_ws))
}

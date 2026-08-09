//! 运行时会话管理接口。

use axum::Router;
use axum::routing::{get, patch, post};

use super::ApiRouter;
use crate::runtime_support;

pub(super) fn routes() -> ApiRouter {
    Router::new()
        .route(
            "/runtime/sessions",
            get(runtime_support::handle_runtime_sessions),
        )
        .route(
            "/runtime/sessions/{id}",
            patch(runtime_support::handle_runtime_session_metadata_patch)
                .delete(runtime_support::handle_runtime_session_delete),
        )
        .route(
            "/runtime/sessions/{id}/export",
            get(runtime_support::handle_runtime_session_export),
        )
        .route(
            "/runtime/sessions/{id}/context",
            get(runtime_support::handle_runtime_session_context),
        )
        .route(
            "/runtime/sessions/{id}/runtime-profile",
            get(runtime_support::handle_runtime_session_runtime_profile),
        )
        .route(
            "/runtime/sessions/{id}/resume",
            post(runtime_support::handle_runtime_session_resume),
        )
        .route(
            "/runtime/sessions/{id}/fork",
            post(runtime_support::handle_runtime_session_fork),
        )
}

//! Persona 长期记忆管理接口。

use axum::Router;
use axum::routing::{get, post};

use super::ApiRouter;
use crate::runtime_support;

pub(super) fn routes() -> ApiRouter {
    Router::new()
        .route(
            "/personas/{id}/memories",
            get(runtime_support::handle_persona_memories)
                .post(runtime_support::handle_create_persona_memory)
                .delete(runtime_support::handle_clear_persona_memories),
        )
        .route(
            "/personas/{id}/memories/{memory_id}",
            get(runtime_support::handle_get_persona_memory)
                .delete(runtime_support::handle_delete_persona_memory),
        )
        .route(
            "/personas/{id}/memories/{memory_id}/history",
            get(runtime_support::handle_persona_memory_history),
        )
        .route(
            "/personas/{id}/memories/{memory_id}/correct",
            post(runtime_support::handle_correct_persona_memory),
        )
        .route(
            "/personas/{id}/memories/{memory_id}/importance",
            post(runtime_support::handle_adjust_persona_memory_importance),
        )
}

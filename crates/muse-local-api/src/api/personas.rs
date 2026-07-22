//! Persona 管理接口。

use axum::Router;
use axum::routing::{get, post};

use super::ApiRouter;
use crate::runtime_support;

pub(super) fn routes() -> ApiRouter {
    Router::new()
        .route(
            "/personas",
            get(runtime_support::handle_personas).post(runtime_support::handle_create_persona),
        )
        .route(
            "/personas/import",
            post(runtime_support::handle_import_persona_card),
        )
        .route(
            "/personas/active",
            get(runtime_support::handle_active_persona),
        )
        .route(
            "/personas/{id}",
            get(runtime_support::handle_get_persona)
                .put(runtime_support::handle_update_persona)
                .delete(runtime_support::handle_delete_persona),
        )
        .route(
            "/personas/{id}/card",
            get(runtime_support::handle_export_persona_card),
        )
        .route(
            "/personas/{id}/deletion-impact",
            get(runtime_support::handle_persona_deletion_impact),
        )
        .route(
            "/personas/{id}/activate",
            post(runtime_support::handle_activate_persona),
        )
}

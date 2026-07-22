//! Persona 管理接口。

use axum::Router;
use axum::routing::{get, post};

use super::ApiRouter;
use crate::handlers;

pub(super) fn routes() -> ApiRouter {
    Router::new()
        .route(
            "/personas",
            get(handlers::handle_personas).post(handlers::handle_create_persona),
        )
        .route(
            "/personas/import",
            post(handlers::handle_import_persona_card),
        )
        .route("/personas/active", get(handlers::handle_active_persona))
        .route(
            "/personas/{id}",
            get(handlers::handle_get_persona)
                .put(handlers::handle_update_persona)
                .delete(handlers::handle_delete_persona),
        )
        .route(
            "/personas/{id}/card",
            get(handlers::handle_export_persona_card),
        )
        .route(
            "/personas/{id}/deletion-impact",
            get(handlers::handle_persona_deletion_impact),
        )
        .route(
            "/personas/{id}/activate",
            post(handlers::handle_activate_persona),
        )
}

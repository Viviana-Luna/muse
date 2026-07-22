//! Skill 管理接口。

use axum::Router;
use axum::routing::get;

use super::ApiRouter;
use crate::handlers;

pub(super) fn routes() -> ApiRouter {
    Router::new()
        .route(
            "/skills",
            get(handlers::handle_skills).post(handlers::handle_create_skill),
        )
        .route(
            "/skills/{name}",
            get(handlers::handle_get_skill)
                .put(handlers::handle_update_skill)
                .delete(handlers::handle_delete_skill),
        )
}

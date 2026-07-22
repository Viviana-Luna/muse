//! 应用偏好接口。

use axum::Router;
use axum::routing::get;

use super::ApiRouter;
use crate::handlers;

pub(super) fn routes() -> ApiRouter {
    Router::new().route(
        "/preferences/appearance",
        get(handlers::handle_get_appearance_preferences)
            .put(handlers::handle_put_appearance_preferences),
    )
}

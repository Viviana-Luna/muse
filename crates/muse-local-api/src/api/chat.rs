//! 对话与历史记录接口。

use axum::Router;
use axum::routing::{get, post};

use super::ApiRouter;
use crate::handlers;

pub(super) fn routes() -> ApiRouter {
    Router::new()
        .route("/chat", post(handlers::handle_chat))
        .route(
            "/chat/stream",
            post(handlers::handle_chat_stream).get(handlers::handle_chat_stream_get_not_allowed),
        )
        .route("/history", get(handlers::handle_history))
        .route("/reset", post(handlers::handle_reset))
}

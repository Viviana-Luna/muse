//! 对话与历史记录接口。

use axum::Router;
use axum::routing::{get, post};

use super::ApiRouter;
use crate::runtime_support;

pub(super) fn routes() -> ApiRouter {
    Router::new()
        .route("/chat", post(runtime_support::handle_chat))
        .route(
            "/chat/stream",
            post(runtime_support::handle_chat_stream)
                .get(runtime_support::handle_chat_stream_get_not_allowed),
        )
        .route("/history", get(runtime_support::handle_history))
        .route("/reset", post(runtime_support::handle_reset))
}

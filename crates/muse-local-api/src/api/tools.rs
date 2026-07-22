//! 工具目录接口。

use axum::Router;
use axum::routing::get;

use super::ApiRouter;
use crate::handlers;

pub(super) fn routes() -> ApiRouter {
    Router::new().route("/tools", get(handlers::handle_tools))
}

//! 本地诊断接口。

use axum::Router;
use axum::routing::get;

use super::ApiRouter;
use crate::handlers;

pub(super) fn routes() -> ApiRouter {
    Router::new().route(
        "/diagnostics/connectivity",
        get(handlers::handle_diagnostics_connectivity),
    )
}

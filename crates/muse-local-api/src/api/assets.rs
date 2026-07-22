//! Persona 资源上传与读取接口。

use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::routing::{get, post};

use super::ApiRouter;
use crate::handlers;

const PERSONA_ASSET_UPLOAD_BODY_LIMIT_BYTES: usize = 6 * 1024 * 1024;

pub(super) fn routes() -> ApiRouter {
    Router::new()
        .route(
            "/assets/upload",
            post(handlers::handle_upload_asset)
                .layer(DefaultBodyLimit::max(PERSONA_ASSET_UPLOAD_BODY_LIMIT_BYTES)),
        )
        .route(
            "/assets/uploaded/{filename}",
            get(handlers::handle_uploaded_asset).delete(handlers::handle_discard_uploaded_asset),
        )
}

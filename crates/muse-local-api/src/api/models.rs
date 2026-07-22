//! 模型目录、供应商状态与模型相关配置接口。

use axum::Router;
use axum::routing::{get, post, put};

use super::ApiRouter;
use crate::handlers;

pub(super) fn routes() -> ApiRouter {
    Router::new()
        .route("/models", get(handlers::handle_models))
        .route("/models/catalog", get(handlers::handle_models_catalog))
        .route(
            "/models/catalog/models",
            post(handlers::handle_create_catalog_model)
                .put(handlers::handle_update_catalog_model)
                .delete(handlers::handle_delete_catalog_model),
        )
        .route(
            "/models/catalog/fetch",
            post(handlers::handle_fetch_model_catalog),
        )
        .route(
            "/models/providers/{id}/credential",
            put(handlers::handle_put_provider_credential),
        )
        .route(
            "/models/providers/{id}/state",
            put(handlers::handle_put_provider_state),
        )
        .route(
            "/models/active",
            put(handlers::handle_put_active_chat_model),
        )
        .route(
            "/models/provider-balance",
            post(handlers::handle_provider_balance),
        )
        .route(
            "/models/config",
            get(handlers::handle_get_models_config).put(handlers::handle_put_models_config),
        )
        .route(
            "/web-search/config",
            get(handlers::handle_get_web_search_config).put(handlers::handle_put_web_search_config),
        )
}

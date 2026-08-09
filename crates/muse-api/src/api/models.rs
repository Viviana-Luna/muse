//! 模型目录、供应商状态与模型相关配置接口。

use axum::Router;
use axum::routing::{get, post, put};

use super::ApiRouter;
use crate::runtime_support;

pub(super) fn routes() -> ApiRouter {
    Router::new()
        .route("/models", get(runtime_support::handle_models))
        .route(
            "/models/catalog",
            get(runtime_support::handle_models_catalog),
        )
        .route(
            "/models/catalog/models",
            post(runtime_support::handle_create_catalog_model)
                .put(runtime_support::handle_update_catalog_model)
                .delete(runtime_support::handle_delete_catalog_model),
        )
        .route(
            "/models/catalog/fetch",
            post(runtime_support::handle_fetch_model_catalog),
        )
        .route(
            "/models/providers/{id}/credential",
            put(runtime_support::handle_put_provider_credential),
        )
        .route(
            "/models/providers/{id}/state",
            put(runtime_support::handle_put_provider_state),
        )
        .route(
            "/models/active",
            put(runtime_support::handle_put_active_chat_model),
        )
        .route(
            "/models/provider-balance",
            post(runtime_support::handle_provider_balance),
        )
        .route(
            "/models/config",
            get(runtime_support::handle_get_models_config)
                .put(runtime_support::handle_put_models_config),
        )
        .route(
            "/web-search/config",
            get(runtime_support::handle_get_web_search_config)
                .put(runtime_support::handle_put_web_search_config),
        )
}

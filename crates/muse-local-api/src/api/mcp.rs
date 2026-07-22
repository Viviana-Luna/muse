//! MCP Server 管理接口。

use axum::Router;
use axum::routing::{get, post};

use super::ApiRouter;
use crate::handlers;

pub(super) fn routes() -> ApiRouter {
    Router::new()
        .route(
            "/mcp/servers",
            get(handlers::handle_mcp_servers).post(handlers::handle_create_mcp_server),
        )
        .route(
            "/mcp/servers/test-draft",
            post(handlers::handle_test_mcp_draft),
        )
        .route(
            "/mcp/servers/{name}",
            get(handlers::handle_get_mcp_server)
                .put(handlers::handle_update_mcp_server)
                .delete(handlers::handle_delete_mcp_server),
        )
        .route(
            "/mcp/servers/{name}/test",
            post(handlers::handle_test_mcp_server),
        )
        .route(
            "/mcp/servers/{name}/refresh",
            post(handlers::handle_refresh_mcp_server),
        )
        .route(
            "/mcp/servers/{name}/tools",
            get(handlers::handle_mcp_server_tools),
        )
        .route(
            "/mcp/servers/{name}/resources",
            get(handlers::handle_mcp_server_resources),
        )
}

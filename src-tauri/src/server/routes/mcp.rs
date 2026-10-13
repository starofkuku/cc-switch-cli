//! MCP server registry endpoints. Mutation routes through `McpService`,
//! which also syncs the affected live config files.

use axum::extract::{Path as AxPath, State};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use utoipa::ToSchema;

use crate::app_config::{McpApps, McpServer};
use crate::services::mcp::McpService;

use super::super::error::{ApiError, ApiResult};
use super::super::ServerState;
use super::docs::ErrorBody;
use super::mask_secrets;

#[utoipa::path(
    get,
    path = "/api/mcp",
    security(("bearer_token" = [])),
    responses(
        (status = 200, description = "All registered MCP servers with per-app enablement"),
        (status = 401, description = "Missing or invalid bearer token", body = ErrorBody),
    )
)]
pub(crate) async fn list_servers(State(state): State<ServerState>) -> ApiResult<Json<Value>> {
    let servers = McpService::get_all_servers(&state.app)?;
    let items: Vec<Value> = servers
        .values()
        .map(|server| {
            let mut value = serde_json::to_value(server).unwrap_or_else(|_| json!({}));
            mask_secrets(&mut value);
            value
        })
        .collect();
    Ok(Json(json!({ "servers": items })))
}

#[utoipa::path(
    get,
    path = "/api/mcp/{id}",
    security(("bearer_token" = [])),
    responses(
        (status = 200, description = "MCP server detail"),
        (status = 401, description = "Missing or invalid bearer token", body = ErrorBody),
        (status = 404, description = "Server not found", body = ErrorBody),
    )
)]
pub(crate) async fn get_server(
    AxPath(id): AxPath<String>,
    State(state): State<ServerState>,
) -> ApiResult<Json<Value>> {
    let servers = McpService::get_all_servers(&state.app)?;
    let server = servers
        .get(&id)
        .ok_or_else(|| ApiError::not_found(format!("mcp server '{id}' not found")))?;
    let mut value = serde_json::to_value(server).map_err(ApiError::invalid_json)?;
    mask_secrets(&mut value);
    Ok(Json(value))
}

/// Upsert an MCP server. The body is an `McpServer` JSON document
/// (id/name/server/apps/…); enabling an app triggers its live sync.
#[utoipa::path(
    post,
    path = "/api/mcp",
    request_body = Value,
    security(("bearer_token" = [])),
    responses(
        (status = 200, description = "Upserted; body is an `McpServer` JSON document (id/name/server/apps/...); enabling an app syncs its live config"),
        (status = 401, description = "Missing or invalid bearer token", body = ErrorBody),
        (status = 400, description = "Missing id", body = ErrorBody),
    )
)]
pub(crate) async fn upsert_server(
    State(state): State<ServerState>,
    Json(server): Json<McpServer>,
) -> ApiResult<Json<Value>> {
    if server.id.trim().is_empty() {
        return Err(ApiError::bad_request("mcp server id must not be empty"));
    }
    let id = server.id.clone();
    McpService::upsert_server(&state.app, server)?;
    state.events.emit("mcp_upserted", None, json!({ "id": id }));
    Ok(Json(json!({ "id": id })))
}

#[derive(Deserialize, ToSchema)]
pub(crate) struct SetMcpAppsRequest {
    /// `McpApps` JSON object, keys: `claude`, `codex`, `gemini`, `grokbuild`,
    /// `opencode`, `hermes` (booleans).
    pub apps: Value,
}

#[utoipa::path(
    patch,
    path = "/api/mcp/{id}",
    request_body = SetMcpAppsRequest,
    security(("bearer_token" = [])),
    responses(
        (status = 200, description = "App enablement updated and synced to live configs"),
        (status = 401, description = "Missing or invalid bearer token", body = ErrorBody),
        (status = 404, description = "Server not found", body = ErrorBody),
    )
)]
pub(crate) async fn set_server_apps(
    AxPath(id): AxPath<String>,
    State(state): State<ServerState>,
    Json(body): Json<SetMcpAppsRequest>,
) -> ApiResult<Json<Value>> {
    let apps: McpApps = serde_json::from_value(body.apps).map_err(ApiError::invalid_json)?;
    let changed = McpService::set_apps(&state.app, &id, apps)?;
    if !changed {
        return Err(ApiError::not_found(format!("mcp server '{id}' not found")));
    }
    state
        .events
        .emit("mcp_apps_updated", None, json!({ "id": id }));
    Ok(Json(json!({ "id": id })))
}

#[utoipa::path(
    delete,
    path = "/api/mcp/{id}",
    security(("bearer_token" = [])),
    responses(
        (status = 200, description = "Deleted and removed from live configs"),
        (status = 401, description = "Missing or invalid bearer token", body = ErrorBody),
        (status = 404, description = "Server not found", body = ErrorBody),
    )
)]
pub(crate) async fn delete_server(
    AxPath(id): AxPath<String>,
    State(state): State<ServerState>,
) -> ApiResult<Json<Value>> {
    let deleted = McpService::delete_server(&state.app, &id)?;
    if !deleted {
        return Err(ApiError::not_found(format!("mcp server '{id}' not found")));
    }
    state.events.emit("mcp_deleted", None, json!({ "id": id }));
    Ok(Json(json!({ "deleted": id })))
}

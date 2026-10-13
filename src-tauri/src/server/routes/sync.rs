//! WebDAV sync triggers and read-only proxy status.

use axum::extract::State;
use axum::Json;
use serde_json::{json, Value};

use crate::app_config::AppType;
use crate::services::webdav_sync::WebDavSyncService;

use super::super::error::ApiResult;
use super::super::ServerState;
use super::docs::ErrorBody;

#[utoipa::path(
    post,
    path = "/api/sync/webdav/upload",
    security(("bearer_token" = [])),
    responses(
        (status = 200, description = "Snapshot (db.sql + skills.zip) uploaded to the configured WebDAV remote"),
        (status = 401, description = "Missing or invalid bearer token", body = ErrorBody),
        (status = 409, description = "WebDAV not configured or upload failed", body = ErrorBody),
    )
)]
pub(crate) async fn webdav_upload(State(state): State<ServerState>) -> ApiResult<Json<Value>> {
    let summary = WebDavSyncService::upload()?;
    state.events.emit("webdav_upload", None, json!({}));
    Ok(Json(
        json!({ "decision": format!("{:?}", summary.decision), "message": summary.message }),
    ))
}

#[utoipa::path(
    post,
    path = "/api/sync/webdav/download",
    security(("bearer_token" = [])),
    responses(
        (status = 200, description = "Remote snapshot applied; providers replayed into live configs (same as the CLI download flow)"),
        (status = 401, description = "Missing or invalid bearer token", body = ErrorBody),
        (status = 409, description = "WebDAV not configured, download or apply failed", body = ErrorBody),
    )
)]
pub(crate) async fn webdav_download(State(state): State<ServerState>) -> ApiResult<Json<Value>> {
    let summary = WebDavSyncService::download()?;
    // The database was replaced underneath the in-memory snapshot — reload it
    // and replay providers into live configs, mirroring the CLI download flow.
    state.app.refresh_config_from_db()?;
    crate::services::ProviderService::sync_current_to_live(&state.app)?;
    state.events.emit("webdav_download", None, json!({}));
    Ok(Json(
        json!({ "decision": format!("{:?}", summary.decision), "message": summary.message }),
    ))
}

#[utoipa::path(
    get,
    path = "/api/proxy",
    security(("bearer_token" = [])),
    responses(
        (status = 200, description = "Local proxy running state and per-app takeover flags (claude/codex/gemini)"),
        (status = 401, description = "Missing or invalid bearer token", body = ErrorBody),
    )
)]
pub(crate) async fn proxy_status(State(state): State<ServerState>) -> ApiResult<Json<Value>> {
    let running = state
        .app
        .proxy_service
        .is_running_blocking()
        .unwrap_or(false);
    let mut takeovers = serde_json::Map::new();
    for app in [AppType::Claude, AppType::Codex, AppType::Gemini] {
        let active = state
            .app
            .proxy_service
            .is_app_takeover_active_blocking(&app)
            .unwrap_or(false);
        takeovers.insert(app.as_str().to_string(), Value::Bool(active));
    }
    Ok(Json(json!({ "running": running, "takeover": takeovers })))
}

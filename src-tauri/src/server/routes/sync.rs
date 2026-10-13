//! WebDAV sync triggers and read-only proxy status.

use axum::extract::State;
use axum::Json;
use serde_json::{json, Value};

use crate::app_config::AppType;
use crate::services::webdav_sync::{WebDavSyncService, WebDavSyncSummary};

use super::super::error::{ApiError, ApiResult};
use super::super::ServerState;
use super::docs::ErrorBody;

/// The sync service internally builds its own tokio runtime and blocks on it,
/// which panics ("cannot start a runtime from within a runtime") when called
/// from an async worker thread. Every blocking entry point below therefore
/// runs on the dedicated blocking pool via `spawn_blocking`.
fn join_error(where_: &str) -> ApiError {
    ApiError::bad_request(format!("sync task panicked ({where_})"))
}

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
    let summary: WebDavSyncSummary = tokio::task::spawn_blocking(WebDavSyncService::upload)
        .await
        .map_err(|_| join_error("upload"))?
        .map_err(ApiError::from)?;
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
    let app_state = state.app.clone();
    let summary: WebDavSyncSummary = tokio::task::spawn_blocking(
        move || -> Result<WebDavSyncSummary, crate::error::AppError> {
            let summary = WebDavSyncService::download()?;
            // The database was replaced underneath the in-memory snapshot — reload
            // it and replay providers into live configs, mirroring the CLI flow.
            app_state.refresh_config_from_db()?;
            crate::services::ProviderService::sync_current_to_live(&app_state)?;
            Ok(summary)
        },
    )
    .await
    .map_err(|_| join_error("download"))?
    .map_err(ApiError::from)?;
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

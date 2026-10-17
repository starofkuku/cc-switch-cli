//! Codex model catalog file endpoints.
//!
//! The catalog file is user-owned territory when no provider carries
//! `modelCatalog` material: cc-switch transports it (sync) but never
//! regenerates it, and provider switches keep the config.toml reference key
//! alive as long as the file exists. These endpoints expose read/write access
//! so panels can edit it directly.

use axum::extract::State;
use axum::Json;
use serde_json::{json, Value};

use super::super::error::{ApiError, ApiResult};
use super::super::ServerState;
use super::docs::ErrorBody;

#[utoipa::path(
    get,
    path = "/api/apps/codex/model-catalog",
    security(("bearer_token" = [])),
    responses(
        (status = 200, description = "Catalog file content plus whether config.toml currently references it (`referenced: false` means Codex is NOT loading it)"),
        (status = 401, description = "Missing or invalid bearer token", body = ErrorBody),
        (status = 404, description = "The catalog file does not exist", body = ErrorBody),
    )
)]
pub(crate) async fn get_catalog(State(_state): State<ServerState>) -> ApiResult<Json<Value>> {
    let Some(content) = crate::codex_config::read_codex_model_catalog_file()? else {
        return Err(ApiError::not_found(
            "cc-switch-model-catalog.json does not exist",
        ));
    };
    Ok(Json(json!({
        "content": content,
        "referenced": crate::codex_config::codex_catalog_referenced_in_config(),
    })))
}

#[utoipa::path(
    put,
    path = "/api/apps/codex/model-catalog",
    request_body = Value,
    security(("bearer_token" = [])),
    responses(
        (status = 200, description = "File written atomically; the config.toml reference key was ensured (Codex loads the file on next start). `keyAdded` tells whether the key was newly added"),
        (status = 400, description = "Body is not a JSON object", body = ErrorBody),
        (status = 401, description = "Missing or invalid bearer token", body = ErrorBody),
        (status = 409, description = "The ~/.codex directory does not exist (refusing to create it)", body = ErrorBody),
    )
)]
pub(crate) async fn put_catalog(
    State(state): State<ServerState>,
    Json(content): Json<Value>,
) -> ApiResult<Json<Value>> {
    if !content.is_object() {
        return Err(ApiError::bad_request(
            "catalog content must be a JSON object",
        ));
    }
    crate::codex_config::write_codex_model_catalog_file(&content)?;
    // A written catalog nothing references is a silent no-op for Codex —
    // make sure the config.toml key points at it.
    let key_added = crate::codex_config::ensure_codex_catalog_reference_in_config()?;
    state.events.emit(
        "codex_model_catalog_updated",
        Some("codex"),
        json!({ "keyAdded": key_added }),
    );
    Ok(Json(json!({
        "written": true,
        "keyAdded": key_added,
        "referenced": crate::codex_config::codex_catalog_referenced_in_config(),
    })))
}

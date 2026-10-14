//! Global common prompt preamble endpoints: a preset library with a single
//! active entry whose content is rendered into every live prompt file.

use axum::extract::{Path as AxPath, State};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use utoipa::ToSchema;

use crate::services::prompt_common::{self, CommonPreset};

use super::super::error::{ApiError, ApiResult};
use super::super::ServerState;
use super::docs::ErrorBody;

#[derive(Deserialize, ToSchema)]
pub(crate) struct CreateCommonPresetRequest {
    /// Preset id (unique).
    pub id: String,
    /// Display name (defaults to the id).
    #[schema(nullable = true)]
    pub name: Option<String>,
    /// Preamble content (markdown). Activating renders it at the top of every
    /// live prompt file inside `cc-switch common` markers.
    pub content: String,
}

#[derive(Deserialize, ToSchema)]
pub(crate) struct UpdateCommonPresetRequest {
    #[schema(nullable = true)]
    pub name: Option<String>,
    #[schema(nullable = true)]
    pub content: Option<String>,
}

#[utoipa::path(
    get,
    path = "/api/prompts/common",
    security(("bearer_token" = [])),
    responses(
        (status = 200, description = "Common preset library and the active id (at most one active; null = preamble not rendered)"),
        (status = 401, description = "Missing or invalid bearer token", body = ErrorBody),
    )
)]
pub(crate) async fn list_common(State(state): State<ServerState>) -> ApiResult<Json<Value>> {
    let current = prompt_common::list(&state.app)?;
    Ok(Json(
        serde_json::to_value(current).map_err(ApiError::invalid_json)?,
    ))
}

#[utoipa::path(
    post,
    path = "/api/prompts/common",
    request_body = CreateCommonPresetRequest,
    security(("bearer_token" = [])),
    responses(
        (status = 200, description = "Created/updated. Repaints every live file when the preset is the active one"),
        (status = 400, description = "Empty id", body = ErrorBody),
        (status = 401, description = "Missing or invalid bearer token", body = ErrorBody),
    )
)]
pub(crate) async fn create_common(
    State(state): State<ServerState>,
    Json(body): Json<CreateCommonPresetRequest>,
) -> ApiResult<Json<Value>> {
    let preset = CommonPreset {
        id: body.id.clone(),
        name: body.name.unwrap_or_else(|| body.id.clone()),
        content: body.content,
    };
    let updated = prompt_common::upsert(&state.app, preset)?;
    state.events.emit(
        "prompts_common_upserted",
        None,
        json!({ "id": body.id, "updated": updated }),
    );
    Ok(Json(json!({ "id": body.id, "updated": updated })))
}

#[utoipa::path(
    patch,
    path = "/api/prompts/common/{id}",
    request_body = UpdateCommonPresetRequest,
    security(("bearer_token" = [])),
    responses(
        (status = 200, description = "Updated; live files repainted when this preset is active"),
        (status = 404, description = "Preset not found", body = ErrorBody),
        (status = 401, description = "Missing or invalid bearer token", body = ErrorBody),
    )
)]
pub(crate) async fn update_common(
    AxPath(id): AxPath<String>,
    State(state): State<ServerState>,
    Json(body): Json<UpdateCommonPresetRequest>,
) -> ApiResult<Json<Value>> {
    let mut current = prompt_common::list(&state.app)?;
    let Some(preset) = current.presets.iter_mut().find(|p| p.id == id) else {
        return Err(ApiError::not_found(format!(
            "common preset '{id}' not found"
        )));
    };
    if let Some(name) = body.name {
        preset.name = name;
    }
    if let Some(content) = body.content {
        preset.content = content;
    }
    let patched = preset.clone();
    let is_active = current.active_id.as_deref() == Some(id.as_str());
    prompt_common::upsert(&state.app, patched)?;
    state.events.emit(
        "prompts_common_updated",
        None,
        json!({ "id": id, "active": is_active }),
    );
    Ok(Json(json!({ "id": id, "active": is_active })))
}

#[utoipa::path(
    delete,
    path = "/api/prompts/common/{id}",
    security(("bearer_token" = [])),
    responses(
        (status = 200, description = "Deleted; deactivated first (live files repainted) when it was active"),
        (status = 404, description = "Preset not found", body = ErrorBody),
        (status = 401, description = "Missing or invalid bearer token", body = ErrorBody),
    )
)]
pub(crate) async fn delete_common(
    AxPath(id): AxPath<String>,
    State(state): State<ServerState>,
) -> ApiResult<Json<Value>> {
    match prompt_common::delete(&state.app, &id)? {
        true => {
            state
                .events
                .emit("prompts_common_deleted", None, json!({ "id": id }));
            Ok(Json(json!({ "deleted": id })))
        }
        false => Err(ApiError::not_found(format!(
            "common preset '{id}' not found"
        ))),
    }
}

#[utoipa::path(
    post,
    path = "/api/prompts/common/{id}/use",
    security(("bearer_token" = [])),
    responses(
        (status = 200, description = "Activated (single-active) and repainted into every existing live prompt file; `repainted` counts the files changed"),
        (status = 404, description = "Preset not found", body = ErrorBody),
        (status = 401, description = "Missing or invalid bearer token", body = ErrorBody),
    )
)]
pub(crate) async fn use_common(
    AxPath(id): AxPath<String>,
    State(state): State<ServerState>,
) -> ApiResult<Json<Value>> {
    let repainted = prompt_common::use_preset(&state.app, &id).map_err(|e| match e {
        crate::error::AppError::InvalidInput(_) => ApiError::not_found(e.to_string()),
        other => ApiError::from(other),
    })?;
    state.events.emit(
        "prompts_common_switched",
        None,
        json!({ "id": id, "repainted": repainted }),
    );
    Ok(Json(json!({ "active": id, "repainted": repainted })))
}

#[utoipa::path(
    post,
    path = "/api/prompts/common/off",
    security(("bearer_token" = [])),
    responses(
        (status = 200, description = "Deactivated; marker sections removed from live files"),
        (status = 401, description = "Missing or invalid bearer token", body = ErrorBody),
    )
)]
pub(crate) async fn off_common(State(state): State<ServerState>) -> ApiResult<Json<Value>> {
    let repainted = prompt_common::deactivate(&state.app)?;
    state.events.emit(
        "prompts_common_switched",
        None,
        json!({ "id": null, "repainted": repainted }),
    );
    Ok(Json(json!({ "active": null, "repainted": repainted })))
}

#[utoipa::path(
    post,
    path = "/api/prompts/common/reapply",
    security(("bearer_token" = [])),
    responses(
        (status = 200, description = "Re-rendered the active preamble into every existing live prompt file"),
        (status = 401, description = "Missing or invalid bearer token", body = ErrorBody),
    )
)]
pub(crate) async fn reapply_common(State(state): State<ServerState>) -> ApiResult<Json<Value>> {
    let repainted = prompt_common::reapply_common_to_all_apps(&state.app)?;
    state.events.emit(
        "prompts_common_reapplied",
        None,
        json!({ "repainted": repainted }),
    );
    Ok(Json(json!({ "repainted": repainted })))
}

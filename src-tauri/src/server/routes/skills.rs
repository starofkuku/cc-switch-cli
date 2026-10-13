//! Skill registry endpoints: list the SSOT index and change the per-app
//! enablement matrix (including the ZCode sidecar state).

use axum::extract::{Path as AxPath, State};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use utoipa::ToSchema;

use crate::services::skill::SkillService;

use super::super::error::{ApiError, ApiResult};
use super::super::ServerState;
use super::docs::ErrorBody;

#[utoipa::path(
    get,
    path = "/api/skills",
    security(("bearer_token" = [])),
    responses(
        (status = 200, description = "Installed skills with the per-app enablement matrix (incl. the zcode sidecar state)"),
        (status = 401, description = "Missing or invalid bearer token", body = ErrorBody),
    )
)]
pub(crate) async fn list_skills(State(_state): State<ServerState>) -> ApiResult<Json<Value>> {
    let index = SkillService::load_index()?;
    let skills: Vec<Value> = index
        .skills
        .values()
        .map(|skill| serde_json::to_value(skill).unwrap_or_else(|_| json!({})))
        .collect();
    Ok(Json(
        json!({ "syncMethod": format!("{:?}", index.sync_method), "skills": skills }),
    ))
}

#[derive(Deserialize, ToSchema)]
pub(crate) struct SetSkillAppsRequest {
    /// `SkillApps` JSON object, keys: `claude`, `codex`, `gemini`, `grokbuild`,
    /// `opencode`, `hermes`, `zcode` (booleans).
    pub apps: Value,
}

#[utoipa::path(
    patch,
    path = "/api/skills/{directory}/apps",
    request_body = SetSkillAppsRequest,
    security(("bearer_token" = [])),
    responses(
        (status = 200, description = "Enablement updated; enabled apps receive the skill dir under their live skills path"),
        (status = 401, description = "Missing or invalid bearer token", body = ErrorBody),
        (status = 404, description = "Skill not found", body = ErrorBody),
    )
)]
pub(crate) async fn set_skill_apps(
    AxPath(directory): AxPath<String>,
    State(state): State<ServerState>,
    Json(body): Json<SetSkillAppsRequest>,
) -> ApiResult<Json<Value>> {
    let apps: crate::app_config::SkillApps =
        serde_json::from_value(body.apps).map_err(ApiError::invalid_json)?;
    let changed = SkillService::set_apps(&directory, apps)?;
    if !changed {
        return Err(ApiError::not_found(format!(
            "skill '{directory}' not found"
        )));
    }
    state
        .events
        .emit("skill_apps_updated", None, json!({ "skill": directory }));
    Ok(Json(json!({ "skill": directory })))
}

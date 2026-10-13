//! Prompt preset endpoints per app.

use axum::extract::{Path as AxPath, State};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use utoipa::ToSchema;

use crate::prompt::Prompt;
use crate::services::prompt::PromptService;

use super::super::error::{ApiError, ApiResult};
use super::super::ServerState;
use super::docs::ErrorBody;

#[utoipa::path(
    get,
    path = "/api/apps/{app}/prompts",
    params(("app" = String, Path, description = "App id (see GET /api/apps)")),
    security(("bearer_token" = [])),
    responses(
        (status = 200, description = "Prompt presets; `active` lists enabled ids"),
        (status = 401, description = "Missing or invalid bearer token", body = ErrorBody),
    )
)]
pub(crate) async fn list_prompts(
    AxPath(app_raw): AxPath<String>,
    State(state): State<ServerState>,
) -> ApiResult<Json<Value>> {
    let app = super::super::parse_app(&app_raw)?;
    let prompts = PromptService::get_prompts(&state.app, app.clone())?;
    let items: Vec<Value> = prompts
        .values()
        .map(|prompt| serde_json::to_value(prompt).unwrap_or_else(|_| json!({})))
        .collect();
    let active: Vec<&str> = prompts
        .values()
        .filter(|prompt| prompt.enabled)
        .map(|prompt| prompt.id.as_str())
        .collect();
    Ok(Json(json!({ "active": active, "prompts": items })))
}

#[utoipa::path(
    get,
    path = "/api/apps/{app}/prompts/{id}",
    params(("app" = String, Path, description = "App id (see GET /api/apps)")),
    security(("bearer_token" = [])),
    responses(
        (status = 200, description = "Prompt detail"),
        (status = 401, description = "Missing or invalid bearer token", body = ErrorBody),
        (status = 404, description = "Prompt not found", body = ErrorBody),
    )
)]
pub(crate) async fn get_prompt(
    AxPath((app_raw, id)): AxPath<(String, String)>,
    State(state): State<ServerState>,
) -> ApiResult<Json<Value>> {
    let app = super::super::parse_app(&app_raw)?;
    let prompts = PromptService::get_prompts(&state.app, app)?;
    let prompt = prompts
        .get(&id)
        .ok_or_else(|| ApiError::not_found(format!("prompt '{id}' not found")))?;
    Ok(Json(
        serde_json::to_value(prompt).map_err(ApiError::invalid_json)?,
    ))
}

#[derive(Deserialize, ToSchema)]
pub(crate) struct CreatePromptRequest {
    /// Optional preset id; generated from the name when omitted.
    #[serde(rename = "id", default)]
    #[schema(nullable = true)]
    pub id: Option<String>,
    /// Preset display name.
    pub name: String,
    /// Full prompt content (markdown).
    pub content: String,
    /// Optional one-line description.
    #[schema(nullable = true)]
    pub description: Option<String>,
}

#[utoipa::path(
    post,
    path = "/api/apps/{app}/prompts",
    request_body = CreatePromptRequest,
    params(("app" = String, Path, description = "App id (see GET /api/apps)")),
    security(("bearer_token" = [])),
    responses(
        (status = 200, description = "Created prompt (full record)"),
        (status = 401, description = "Missing or invalid bearer token", body = ErrorBody),
        (status = 400, description = "Invalid prompt payload", body = ErrorBody),
        (status = 409, description = "Id already exists", body = ErrorBody),
    )
)]
pub(crate) async fn create_prompt(
    AxPath(app_raw): AxPath<String>,
    State(state): State<ServerState>,
    Json(body): Json<CreatePromptRequest>,
) -> ApiResult<Json<Value>> {
    let app = super::super::parse_app(&app_raw)?;
    let prompt = PromptService::create_prompt_with_id(
        &state.app,
        app.clone(),
        body.id.as_deref(),
        &body.name,
        body.description.as_deref(),
        &body.content,
    )?;
    state.events.emit(
        "prompt_created",
        Some(app.as_str()),
        json!({ "id": prompt.id }),
    );
    Ok(Json(
        serde_json::to_value(prompt).map_err(ApiError::invalid_json)?,
    ))
}

#[derive(Deserialize, ToSchema)]
pub(crate) struct UpdatePromptRequest {
    #[schema(nullable = true)]
    pub name: Option<String>,
    #[schema(nullable = true)]
    pub content: Option<String>,
    #[schema(nullable = true)]
    pub description: Option<String>,
}

#[utoipa::path(
    patch,
    path = "/api/apps/{app}/prompts/{id}",
    request_body = UpdatePromptRequest,
    params(("app" = String, Path, description = "App id (see GET /api/apps)")),
    security(("bearer_token" = [])),
    responses(
        (status = 200, description = "Updated"),
        (status = 401, description = "Missing or invalid bearer token", body = ErrorBody),
        (status = 404, description = "Prompt not found", body = ErrorBody),
    )
)]
pub(crate) async fn update_prompt(
    AxPath((app_raw, id)): AxPath<(String, String)>,
    State(state): State<ServerState>,
    Json(body): Json<UpdatePromptRequest>,
) -> ApiResult<Json<Value>> {
    let app = super::super::parse_app(&app_raw)?;
    let prompts = PromptService::get_prompts(&state.app, app.clone())?;
    let mut prompt: Prompt = prompts
        .get(&id)
        .ok_or_else(|| ApiError::not_found(format!("prompt '{id}' not found")))?
        .clone();
    if let Some(name) = body.name {
        prompt.name = name;
    }
    if let Some(content) = body.content {
        prompt.content = content;
    }
    if let Some(description) = body.description {
        prompt.description = Some(description);
    }
    PromptService::upsert_prompt(&state.app, app.clone(), &id, prompt)?;
    state
        .events
        .emit("prompt_updated", Some(app.as_str()), json!({ "id": id }));
    Ok(Json(json!({ "id": id })))
}

#[utoipa::path(
    delete,
    path = "/api/apps/{app}/prompts/{id}",
    params(("app" = String, Path, description = "App id (see GET /api/apps)")),
    security(("bearer_token" = [])),
    responses(
        (status = 200, description = "Deleted"),
        (status = 401, description = "Missing or invalid bearer token", body = ErrorBody),
        (status = 404, description = "Prompt not found", body = ErrorBody),
    )
)]
pub(crate) async fn delete_prompt(
    AxPath((app_raw, id)): AxPath<(String, String)>,
    State(state): State<ServerState>,
) -> ApiResult<Json<Value>> {
    let app = super::super::parse_app(&app_raw)?;
    PromptService::delete_prompt(&state.app, app.clone(), &id)?;
    state
        .events
        .emit("prompt_deleted", Some(app.as_str()), json!({ "id": id }));
    Ok(Json(json!({ "deleted": id })))
}

#[utoipa::path(
    post,
    path = "/api/apps/{app}/prompts/{id}/activate",
    params(("app" = String, Path, description = "App id (see GET /api/apps)")),
    security(("bearer_token" = [])),
    responses(
        (status = 200, description = "Activated; content written to the live prompt file (e.g. ~/.codex/AGENTS.md)"),
        (status = 401, description = "Missing or invalid bearer token", body = ErrorBody),
        (status = 404, description = "Prompt not found", body = ErrorBody),
    )
)]
pub(crate) async fn activate_prompt(
    AxPath((app_raw, id)): AxPath<(String, String)>,
    State(state): State<ServerState>,
) -> ApiResult<Json<Value>> {
    let app = super::super::parse_app(&app_raw)?;
    PromptService::enable_prompt(&state.app, app.clone(), &id)?;
    state
        .events
        .emit("prompt_activated", Some(app.as_str()), json!({ "id": id }));
    Ok(Json(json!({ "activated": id, "app": app.as_str() })))
}

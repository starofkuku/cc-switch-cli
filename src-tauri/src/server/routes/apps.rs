//! Provider endpoints: list / detail / create / update / delete / switch /
//! model discovery — one thin wrapper per `ProviderService` call.

use axum::extract::{Path as AxPath, State};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use utoipa::ToSchema;

use crate::provider::Provider;
use crate::services::provider::ProviderService;

use super::super::error::{ApiError, ApiResult};
use super::super::ServerState;
use super::docs::ErrorBody;
use super::mask_secrets;

#[derive(Serialize, ToSchema)]
pub(crate) struct ProviderSummary {
    pub id: String,
    pub name: String,
    #[schema(nullable = true)]
    pub current: Option<bool>,
    #[schema(nullable = true)]
    pub notes: Option<String>,
    #[schema(nullable = true)]
    pub website_url: Option<String>,
    #[schema(nullable = true)]
    #[serde(rename = "sortIndex", default)]
    pub sort_index: Option<usize>,
}

fn summary(id: &str, provider: &Provider, current: &str) -> ProviderSummary {
    ProviderSummary {
        id: id.to_string(),
        name: provider.name.clone(),
        current: Some(id == current),
        notes: provider.notes.clone(),
        website_url: provider.website_url.clone(),
        sort_index: provider.sort_index,
    }
}

/// Resolve the active provider exactly like `ProviderService::current` does for
/// the CLI: `settings.json` wins when it names a provider that still exists,
/// falling back to the database's `is_current` column. Reading the in-memory
/// snapshot here would report the last synced value instead of the locally
/// effective one, which is what actually gets written to the live config.
fn current_of(state: &ServerState, app: &crate::app_config::AppType) -> String {
    crate::services::ProviderService::current(&state.app, app.clone()).unwrap_or_default()
}

#[utoipa::path(
    get,
    path = "/api/apps/{app}/providers",
    params(("app" = String, Path, description = "App id: claude | codex | gemini | opencode | hermes | openclaw | pi | grokbuild (alias grok) | zcode")),
    security(("bearer_token" = [])),
    responses(
        (status = 200, description = "Provider list; `current` marks the active provider"),
        (status = 401, description = "Missing or invalid bearer token", body = ErrorBody),
    )
)]
pub(crate) async fn list_providers(
    AxPath(app_raw): AxPath<String>,
    State(state): State<ServerState>,
) -> ApiResult<Json<Value>> {
    let app = super::super::parse_app(&app_raw)?;
    let providers = ProviderService::list(&state.app, app.clone())?;
    let current = current_of(&state, &app);
    let items: Vec<ProviderSummary> = providers
        .iter()
        .map(|(id, provider)| summary(id, provider, &current))
        .collect();
    Ok(Json(json!({ "current": current, "providers": items })))
}

#[utoipa::path(
    get,
    path = "/api/apps/{app}/providers/{id}",
    params(("app" = String, Path, description = "App id: claude | codex | gemini | opencode | hermes | openclaw | pi | grokbuild (alias grok) | zcode")),
    security(("bearer_token" = [])),
    responses(
        (status = 200, description = "Provider detail; secret fields are masked as `***`"),
        (status = 401, description = "Missing or invalid bearer token", body = ErrorBody),
        (status = 404, description = "Provider not found", body = ErrorBody),
    )
)]
pub(crate) async fn get_provider(
    AxPath((app_raw, id)): AxPath<(String, String)>,
    State(state): State<ServerState>,
) -> ApiResult<Json<Value>> {
    let app = super::super::parse_app(&app_raw)?;
    let providers = ProviderService::list(&state.app, app.clone())?;
    let provider = providers
        .get(&id)
        .ok_or_else(|| ApiError::not_found(format!("provider '{id}' not found")))?;
    let current = current_of(&state, &app);
    let mut settings = provider.settings_config.clone();
    mask_secrets(&mut settings);
    let mut body =
        serde_json::to_value(summary(&id, provider, &current)).map_err(ApiError::invalid_json)?;
    if let Some(obj) = body.as_object_mut() {
        obj.insert("settingsConfig".to_string(), settings);
    }
    Ok(Json(body))
}

#[derive(Deserialize, ToSchema)]
pub(crate) struct CreateProviderRequest {
    /// Provider id. Required for additive apps (opencode / hermes / openclaw /
    /// pi / grokbuild / zcode); generated as a UUID otherwise.
    #[schema(nullable = true)]
    pub id: Option<String>,
    /// Display name.
    pub name: String,
    /// App-specific provider settings, same JSON shape the CLI stores.
    /// Example (zcode bare-config form): `{ "access": { "type": "api-key",
    /// "apiKey": "sk-..." }, "api": { "type": "openai-chat-completions",
    /// "baseUrl": "https://.../v1" }, "modelOrder": ["m1"] }`.
    /// Other apps mirror their CLI `settings_config` shape.
    #[serde(rename = "settingsConfig")]
    pub settings_config: Value,
    /// Optional homepage URL.
    #[serde(rename = "websiteUrl", default)]
    #[schema(nullable = true)]
    pub website_url: Option<String>,
    #[schema(nullable = true)]
    pub notes: Option<String>,
    #[schema(nullable = true)]
    #[serde(rename = "sortIndex", default)]
    pub sort_index: Option<usize>,
}

#[utoipa::path(
    post,
    path = "/api/apps/{app}/providers",
    request_body = CreateProviderRequest,
    params(("app" = String, Path, description = "App id: claude | codex | gemini | opencode | hermes | openclaw | pi | grokbuild (alias grok) | zcode")),
    security(("bearer_token" = [])),
    responses(
        (status = 200, description = "Created provider id/name"),
        (status = 401, description = "Missing or invalid bearer token", body = ErrorBody),
        (status = 400, description = "Invalid settings or missing id for additive apps", body = ErrorBody),
        (status = 409, description = "Provider id already exists or live write rejected", body = ErrorBody),
    )
)]
pub(crate) async fn create_provider(
    AxPath(app_raw): AxPath<String>,
    State(state): State<ServerState>,
    Json(body): Json<CreateProviderRequest>,
) -> ApiResult<Json<Value>> {
    let app = super::super::parse_app(&app_raw)?;
    let id = match body.id {
        Some(id) => id,
        None => {
            if ProviderService::is_provider_key_app(&app) {
                return Err(ApiError::bad_request(
                    "this app requires an explicit provider id",
                ));
            }
            uuid::Uuid::new_v4().simple().to_string()
        }
    };
    if id.trim().is_empty() {
        return Err(ApiError::bad_request("provider id must not be empty"));
    }

    let provider = Provider::with_id(
        id.clone(),
        body.name.clone(),
        body.settings_config.clone(),
        body.website_url.clone(),
    );
    let mut provider = provider;
    provider.notes = body.notes.clone();
    provider.sort_index = body.sort_index;

    ProviderService::add(&state.app, app.clone(), provider)?;
    state
        .events
        .emit("provider_added", Some(app.as_str()), json!({ "id": id }));
    Ok(Json(json!({ "id": id, "name": body.name })))
}

#[derive(Deserialize, ToSchema)]
pub(crate) struct UpdateProviderRequest {
    /// New display name (omit to keep current).
    #[schema(nullable = true)]
    pub name: Option<String>,
    #[serde(rename = "settingsConfig", default)]
    #[schema(nullable = true)]
    pub settings_config: Option<Value>,
    #[serde(rename = "websiteUrl", default)]
    #[schema(nullable = true)]
    pub website_url: Option<String>,
    #[schema(nullable = true)]
    pub notes: Option<String>,
    #[schema(nullable = true)]
    #[serde(rename = "sortIndex", default)]
    pub sort_index: Option<usize>,
}

#[utoipa::path(
    patch,
    path = "/api/apps/{app}/providers/{id}",
    request_body = UpdateProviderRequest,
    params(("app" = String, Path, description = "App id: claude | codex | gemini | opencode | hermes | openclaw | pi | grokbuild (alias grok) | zcode")),
    security(("bearer_token" = [])),
    responses(
        (status = 200, description = "Updated"),
        (status = 401, description = "Missing or invalid bearer token", body = ErrorBody),
        (status = 404, description = "Provider not found", body = ErrorBody),
        (status = 409, description = "Validation or live write error", body = ErrorBody),
    )
)]
pub(crate) async fn update_provider(
    AxPath((app_raw, id)): AxPath<(String, String)>,
    State(state): State<ServerState>,
    Json(body): Json<UpdateProviderRequest>,
) -> ApiResult<Json<Value>> {
    let app = super::super::parse_app(&app_raw)?;
    let providers = ProviderService::list(&state.app, app.clone())?;
    let existing = providers
        .get(&id)
        .ok_or_else(|| ApiError::not_found(format!("provider '{id}' not found")))?
        .clone();

    let mut provider = existing;
    if let Some(name) = body.name {
        provider.name = name;
    }
    if let Some(mut settings) = body.settings_config {
        // GET responses mask secrets as `***`; a client doing read-modify-write
        // would otherwise overwrite the stored credentials with the mask.
        unmask_settings(&mut settings, &provider.settings_config);
        provider.settings_config = settings;
    }
    if let Some(website_url) = body.website_url {
        provider.website_url = Some(website_url);
    }
    if let Some(notes) = body.notes {
        provider.notes = Some(notes);
    }
    if let Some(sort_index) = body.sort_index {
        provider.sort_index = Some(sort_index);
    }

    ProviderService::update(&state.app, app.clone(), provider)?;
    state
        .events
        .emit("provider_updated", Some(app.as_str()), json!({ "id": id }));
    Ok(Json(json!({ "id": id })))
}

#[utoipa::path(
    delete,
    path = "/api/apps/{app}/providers/{id}",
    params(("app" = String, Path, description = "App id: claude | codex | gemini | opencode | hermes | openclaw | pi | grokbuild (alias grok) | zcode")),
    security(("bearer_token" = [])),
    responses(
        (status = 200, description = "Deleted"),
        (status = 401, description = "Missing or invalid bearer token", body = ErrorBody),
        (status = 404, description = "Provider not found", body = ErrorBody),
        (status = 409, description = "Provider is currently in use; switch first", body = ErrorBody),
    )
)]
pub(crate) async fn delete_provider(
    AxPath((app_raw, id)): AxPath<(String, String)>,
    State(state): State<ServerState>,
) -> ApiResult<Json<Value>> {
    let app = super::super::parse_app(&app_raw)?;
    ProviderService::delete(&state.app, app.clone(), &id)?;
    state
        .events
        .emit("provider_deleted", Some(app.as_str()), json!({ "id": id }));
    Ok(Json(json!({ "deleted": id })))
}

#[utoipa::path(
    post,
    path = "/api/apps/{app}/providers/{id}/switch",
    params(("app" = String, Path, description = "App id: claude | codex | gemini | opencode | hermes | openclaw | pi | grokbuild (alias grok) | zcode")),
    security(("bearer_token" = [])),
    responses(
        (status = 200, description = "Switched; live config updated"),
        (status = 401, description = "Missing or invalid bearer token", body = ErrorBody),
        (status = 404, description = "Provider not found", body = ErrorBody),
        (status = 409, description = "Switch rejected (validation or live write error)", body = ErrorBody),
    )
)]
pub(crate) async fn switch_provider(
    AxPath((app_raw, id)): AxPath<(String, String)>,
    State(state): State<ServerState>,
) -> ApiResult<Json<Value>> {
    let app = super::super::parse_app(&app_raw)?;
    ProviderService::switch(&state.app, app.clone(), &id)?;
    state
        .events
        .emit("provider_switched", Some(app.as_str()), json!({ "id": id }));
    Ok(Json(json!({ "switched": id, "app": app.as_str() })))
}

#[utoipa::path(
    get,
    path = "/api/apps/{app}/providers/{id}/fetch-models",
    params(("app" = String, Path, description = "App id: claude | codex | gemini | opencode | hermes | openclaw | pi | grokbuild (alias grok) | zcode")),
    security(("bearer_token" = [])),
    responses(
        (status = 200, description = "Model ids advertised by the provider endpoint (`/v1/models` or native)"),
        (status = 401, description = "Missing or invalid bearer token", body = ErrorBody),
        (status = 404, description = "Provider not found", body = ErrorBody),
        (status = 409, description = "Endpoint unreachable or credentials missing", body = ErrorBody),
    )
)]
pub(crate) async fn fetch_models(
    AxPath((app_raw, id)): AxPath<(String, String)>,
    State(state): State<ServerState>,
) -> ApiResult<Json<Value>> {
    let app = super::super::parse_app(&app_raw)?;
    let providers = ProviderService::list(&state.app, app.clone())?;
    let provider = providers
        .get(&id)
        .ok_or_else(|| ApiError::not_found(format!("provider '{id}' not found")))?
        .clone();
    // `fetch_live_model_ids` builds its own tokio runtime internally, which
    // would panic on an async worker thread — run it on the blocking pool.

    let models = tokio::task::spawn_blocking(move || {
        crate::cli::commands::provider_inspect::fetch_live_model_ids(&app, &provider)
    })
    .await
    .map_err(|_| ApiError::bad_request("model fetch task panicked"))?
    .map_err(ApiError::from)?;
    Ok(Json(json!({ "models": models })))
}

/// Replace `"***"` leaves in `incoming` with the value at the same path in
/// `stored`, so masked read-modify-write PATCHes never erase credentials.
fn unmask_settings(incoming: &mut Value, stored: &Value) {
    if let (Value::Object(incoming_map), Value::Object(stored_map)) = (incoming, stored) {
        for (key, stored_value) in stored_map {
            match incoming_map.get_mut(key) {
                Some(Value::String(masked)) if masked == "***" => {
                    incoming_map.insert(key.clone(), stored_value.clone());
                }
                Some(incoming_value) => unmask_settings(incoming_value, stored_value),
                None => {}
            }
        }
    }
}

#[derive(Deserialize, Default, ToSchema)]
pub(crate) struct ImportLiveRequest {
    /// Overwrite existing providers with the live content instead of skipping
    /// them. Currently supported for Pi only; other apps reject the flag.
    #[serde(default)]
    pub update: bool,
    /// Additionally delete managed providers that no longer exist in the live
    /// config (rows created purely inside cc-switch are never destroyed).
    /// Currently supported for Pi only.
    #[serde(default)]
    pub prune: bool,
}

#[utoipa::path(
    post,
    path = "/api/apps/{app}/providers/import-live",
    request_body = ImportLiveRequest,
    params(("app" = String, Path, description = "App id: claude | codex | gemini | opencode | hermes | openclaw | pi | grokbuild (alias grok) | zcode")),
    security(("bearer_token" = [])),
    responses(
        (status = 200, description = "Import summary: providers imported from the live config into the database. `{}` = add-only, `{\"update\": true}` overwrites existing entries, `{\"prune\": true}` also removes managed entries missing from live (Pi only for both flags)"),
        (status = 401, description = "Missing or invalid bearer token", body = ErrorBody),
        (status = 409, description = "Live config missing/unreadable, or --update/--prune requested for an app other than pi", body = ErrorBody),
    )
)]
pub(crate) async fn import_live(
    AxPath(app_raw): AxPath<String>,
    State(state): State<ServerState>,
    body: Option<Json<ImportLiveRequest>>,
) -> ApiResult<Json<Value>> {
    let app = super::super::parse_app(&app_raw)?;
    let request = body.map(|Json(body)| body).unwrap_or_default();
    let summary = crate::services::provider::ProviderService::import_live_config_with_options(
        &state.app,
        app.clone(),
        crate::services::provider::LiveImportOptions {
            update: request.update,
            prune: request.prune,
        },
    )?;
    state.events.emit(
        "provider_imported_live",
        Some(app.as_str()),
        json!({
            "added": summary.added,
            "updated": summary.updated,
            "pruned": summary.pruned,
        }),
    );
    Ok(Json(json!({
        "added": summary.added,
        "updated": summary.updated,
        "pruned": summary.pruned,
    })))
}

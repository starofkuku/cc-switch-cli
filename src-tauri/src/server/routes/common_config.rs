//! Per-app common config snippet endpoints — thin wrappers over the same
//! canonicalization + transactional setter the CLI `config common set` uses.
//!
//! The snippet is raw text (JSON for most apps, TOML for Codex), never a
//! parsed object: PUT canonicalizes exactly like the CLI (pretty-printed JSON
//! / normalized TOML) and an empty string clears the snippet.

use axum::extract::{Path as AxPath, State};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use utoipa::ToSchema;

use crate::app_config::AppType;
use crate::services::provider::ProviderService;

use super::super::error::{ApiError, ApiResult};
use super::super::ServerState;
use super::docs::ErrorBody;

fn snippet_format(app: &AppType) -> &'static str {
    if matches!(app, AppType::Codex) {
        "toml"
    } else {
        "json"
    }
}

fn current_snippet(state: &ServerState, app: &AppType) -> Option<String> {
    state
        .app
        .config
        .read()
        .ok()?
        .common_config_snippets
        .get(app)
        .cloned()
        .filter(|value| !value.trim().is_empty())
}

#[utoipa::path(
    get,
    path = "/api/apps/{app}/common-config",
    params(("app" = String, Path, description = "App id: claude | codex | gemini | opencode | hermes | openclaw | pi | grokbuild (alias grok) | zcode")),
    security(("bearer_token" = [])),
    responses(
        (status = 200, description = "Raw snippet text (`content: null` when unset). `format` tells the expected shape: json for every app except codex (toml). Effective merging only happens for claude/codex/gemini"),
        (status = 401, description = "Missing or invalid bearer token", body = ErrorBody),
    )
)]
pub(crate) async fn get_common_config(
    AxPath(app_raw): AxPath<String>,
    State(state): State<ServerState>,
) -> ApiResult<Json<Value>> {
    let app = super::super::parse_app(&app_raw)?;
    Ok(Json(json!({
        "content": current_snippet(&state, &app),
        "format": snippet_format(&app),
    })))
}

#[derive(Deserialize, ToSchema)]
pub(crate) struct CommonConfigRequest {
    /// Raw snippet text. JSON object for every app except codex (TOML
    /// document). Canonicalized like the CLI (`config common set`); an empty
    /// string clears the snippet for the app.
    pub content: String,
}

#[utoipa::path(
    put,
    path = "/api/apps/{app}/common-config",
    request_body = CommonConfigRequest,
    params(("app" = String, Path, description = "App id: claude | codex | gemini | opencode | hermes | openclaw | pi | grokbuild (alias grok) | zcode")),
    security(("bearer_token" = [])),
    responses(
        (status = 200, description = "Stored (canonicalized) or cleared. Same transactional behavior as the CLI set/clear, including the live-config rewrite when the current provider uses the snippet"),
        (status = 400, description = "Invalid snippet (not a JSON object / invalid TOML)", body = ErrorBody),
        (status = 401, description = "Missing or invalid bearer token", body = ErrorBody),
        (status = 409, description = "Snippet validation against the current provider failed", body = ErrorBody),
    )
)]
pub(crate) async fn put_common_config(
    AxPath(app_raw): AxPath<String>,
    State(state): State<ServerState>,
    Json(body): Json<CommonConfigRequest>,
) -> ApiResult<Json<Value>> {
    let app = super::super::parse_app(&app_raw)?;
    let canonical =
        crate::cli::commands::config_common::canonical_common_snippet(app.clone(), &body.content)
            .map_err(|e| match e {
            // Parse errors are caller input problems, not conflicts.
            crate::error::AppError::InvalidInput(_) => ApiError::bad_request(e.to_string()),
            other => ApiError::from(other),
        })?;
    let cleared = canonical.is_none();
    ProviderService::set_common_config_snippet(&state.app, app.clone(), canonical)?;
    state.events.emit(
        "common_config_updated",
        Some(app.as_str()),
        json!({ "cleared": cleared }),
    );
    Ok(Json(json!({ "written": true, "cleared": cleared })))
}

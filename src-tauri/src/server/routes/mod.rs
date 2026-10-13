//! Route handlers. Each module maps one resource to services-layer calls.

pub(crate) mod apps;
pub(crate) mod docs;
pub(crate) mod mcp;
pub(crate) mod prompts;
pub(crate) mod settings;
pub(crate) mod skills;
pub(crate) mod sync;

use axum::extract::State;
use axum::Json;
use serde_json::{json, Value};

use super::error::ApiResult;
use super::ServerState;
use crate::app_config::AppType;

/// Liveness + build info. Unauthenticated so probes can discover the API.
#[utoipa::path(
    get,
    path = "/api/health",
    security(()),
    responses((status = 200, description = "Server liveness and build info (unauthenticated)"))
)]
pub(crate) async fn health(State(state): State<ServerState>) -> ApiResult<Json<Value>> {
    Ok(Json(json!({
        "name": "cc-switch-server",
        "version": env!("CARGO_PKG_VERSION"),
        "uptimeSeconds": state.started_at.elapsed().as_secs(),
        "apps": AppType::all().map(|app| app.as_str()).collect::<Vec<_>>(),
    })))
}

/// Supported apps with live-config initialization state.
pub(crate) async fn list_apps(State(state): State<ServerState>) -> ApiResult<Json<Value>> {
    let _ = &state;
    let apps: Vec<Value> = AppType::all()
        .map(|app| {
            json!({
                "id": app.as_str(),
                "label": app_display_label(&app),
                "additive": app.is_additive_mode(),
                "supportsFailover": app.supports_failover(),
                "initialized": crate::sync_policy::should_sync_live(&app),
            })
        })
        .collect();
    Ok(Json(json!({ "apps": apps })))
}

fn app_display_label(app: &AppType) -> &'static str {
    match app {
        AppType::Claude => "Claude",
        AppType::Codex => "Codex",
        AppType::Gemini => "Gemini",
        AppType::OpenCode => "OpenCode",
        AppType::Hermes => "Hermes",
        AppType::OpenClaw => "OpenClaw",
        AppType::Pi => "Pi",
        AppType::GrokBuild => "GrokBuild",
        AppType::Zcode => "ZCode",
    }
}

/// Recursively replace secret-looking string fields with a mask so API
/// responses never echo stored credentials.
pub(crate) fn mask_secrets(value: &mut Value) {
    const SECRET_HINTS: [&str; 6] = ["apikey", "api_key", "token", "secret", "password", "auth"];

    fn looks_secret(key: &str) -> bool {
        let lower = key.to_ascii_lowercase();
        // Keep non-credential keys that merely contain "auth"/"token" as a
        // substring of a longer word (e.g. "author") intact by requiring a
        // word-boundary style match on the whole key.
        SECRET_HINTS.iter().any(|hint| {
            lower == *hint
                || lower.starts_with(&format!("{hint}_"))
                || lower.ends_with(&format!("_{hint}"))
                || lower.contains(&format!("_{hint}_"))
        })
    }

    match value {
        Value::Object(map) => {
            for (key, entry) in map.iter_mut() {
                if looks_secret(key) && entry.is_string() {
                    *entry = Value::String("***".to_string());
                } else {
                    mask_secrets(entry);
                }
            }
        }
        Value::Array(items) => {
            for item in items.iter_mut() {
                mask_secrets(item);
            }
        }
        _ => {}
    }
}

pub(super) use super::events::events_route;

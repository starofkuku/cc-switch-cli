//! Bearer-token authentication for the management API.
//!
//! The token is generated on first start (two UUIDv4s, 64 hex chars) and
//! stored at `~/.cc-switch/server-token` with 0600 permissions. Every route
//! except `/api/health` and `/api/openapi.json` requires it.

use axum::body::Body;
use axum::extract::State;
use axum::http::{header, Request, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use serde_json::json;

use super::ServerState;
use crate::error::AppError;

fn token_path() -> std::path::PathBuf {
    crate::config::get_app_config_dir().join("server-token")
}

/// Load the existing token, or generate and persist a new one.
pub(crate) fn load_or_create_token() -> Result<String, AppError> {
    let path = token_path();
    if path.exists() {
        let content = std::fs::read_to_string(&path).map_err(|e| AppError::io(&path, e))?;
        let token = content.trim().to_string();
        if token.len() >= 32 {
            return Ok(token);
        }
    }

    let token = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| AppError::io(parent, e))?;
    }
    crate::config::atomic_write(&path, token.as_bytes())?;
    restrict_permissions(&path);
    Ok(token)
}

#[cfg(unix)]
fn restrict_permissions(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(metadata) = std::fs::metadata(path) {
        let mut permissions = metadata.permissions();
        permissions.set_mode(0o600);
        let _ = std::fs::set_permissions(path, permissions);
    }
}

#[cfg(not(unix))]
fn restrict_permissions(_path: &std::path::Path) {}

pub(crate) async fn auth_middleware(
    State(state): State<ServerState>,
    request: Request<Body>,
    next: Next,
) -> Response {
    let provided = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::trim);

    match provided {
        Some(candidate) if constant_time_eq(candidate, &state.token) => next.run(request).await,
        _ => (
            StatusCode::UNAUTHORIZED,
            [(header::WWW_AUTHENTICATE, "Bearer")],
            axum::Json(json!({ "error": "missing or invalid bearer token" })),
        )
            .into_response(),
    }
}

fn constant_time_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

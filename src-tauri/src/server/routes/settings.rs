//! Settings read/write. Secrets are masked on read; a masked `"***"` value
//! sent back on PATCH keeps the stored value so round-trips never wipe
//! credentials.

use axum::extract::State;
use axum::Json;
use serde_json::{json, Value};

use crate::settings;

use super::super::error::ApiResult;
use super::super::ServerState;
use super::docs::ErrorBody;
use super::mask_secrets;

#[utoipa::path(
    get,
    path = "/api/settings",
    security(("bearer_token" = [])),
    responses(
        (status = 200, description = "App settings (same shape as ~/.cc-switch/settings.json) with secrets masked as `***`"),
        (status = 401, description = "Missing or invalid bearer token", body = ErrorBody),
    )
)]
pub(crate) async fn get_settings(State(_state): State<ServerState>) -> ApiResult<Json<Value>> {
    let settings = settings::get_settings();
    let mut value = serde_json::to_value(settings).unwrap_or_else(|_| json!({}));
    mask_secrets(&mut value);
    Ok(Json(value))
}

#[utoipa::path(
    patch,
    path = "/api/settings",
    request_body = Value,
    security(("bearer_token" = [])),
    responses(
        (status = 200, description = "Updated settings (masked in the response). Fields sent back as `***` keep their stored values, so a read-modify-write never wipes credentials"),
        (status = 401, description = "Missing or invalid bearer token", body = ErrorBody),
        (status = 400, description = "Invalid settings payload", body = ErrorBody),
    )
)]
pub(crate) async fn update_settings(
    State(_state): State<ServerState>,
    Json(mut body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let stored = serde_json::to_value(settings::get_settings()).unwrap_or_else(|_| json!({}));
    unmask_merge(&mut body, &stored);
    let new_settings: crate::settings::AppSettings =
        serde_json::from_value(body).map_err(super::super::error::ApiError::invalid_json)?;
    settings::update_settings(new_settings)?;
    let mut value = serde_json::to_value(settings::get_settings()).unwrap_or_else(|_| json!({}));
    mask_secrets(&mut value);
    Ok(Json(value))
}

/// Replace masked `"***"` leaves with the stored values so a client that
/// reads settings and posts them back unchanged does not erase credentials.
fn unmask_merge(incoming: &mut Value, stored: &Value) {
    if let (Value::Object(incoming_map), Value::Object(stored_map)) = (incoming, stored) {
        for (key, stored_value) in stored_map {
            match incoming_map.get_mut(key) {
                Some(Value::String(masked)) if masked == "***" => {
                    incoming_map.insert(key.clone(), stored_value.clone());
                }
                Some(incoming_value) => unmask_merge(incoming_value, stored_value),
                None => {}
            }
        }
    }
}

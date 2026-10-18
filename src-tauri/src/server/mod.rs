//! Local HTTP management API.
//!
//! `cc-switch server` exposes the same services layer the CLI/TUI use as a
//! token-guarded REST API bound to localhost by default. Every handler is a
//! thin wrapper around `ProviderService` / `McpService` / `PromptService` /
//! `SkillStore` — the server never touches the database or live config files
//! directly, so consistency guarantees are identical to the CLI.

pub(crate) mod auth;
pub(crate) mod error;
pub(crate) mod events;
pub(crate) mod routes;

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::routing::{get, patch, post};
use axum::Router;
use tokio::net::TcpListener;
use tower_http::cors::{Any, CorsLayer};

use crate::AppState;
use error::{ApiError, ApiResult};
use events::EventBus;

/// Shared per-process server state: the long-lived `AppState` (started once,
/// refreshed in place after writes) plus the auth token and event bus.
#[derive(Clone)]
pub struct ServerState {
    pub app: Arc<AppState>,
    pub token: Arc<String>,
    pub events: EventBus,
    pub started_at: std::time::Instant,
}

/// Bindings for one `cc-switch server` run.
pub struct ServerConfig {
    pub host: String,
    pub port: u16,
    pub token: Option<String>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".to_string(),
            port: 7861,
            token: None,
        }
    }
}

pub fn build_state(config: &ServerConfig) -> Result<ServerState, crate::error::AppError> {
    let app = Arc::new(AppState::try_new_with_startup_recovery()?);
    let token = match &config.token {
        Some(explicit) => explicit.clone(),
        None => auth::load_or_create_token()?,
    };
    Ok(ServerState {
        app,
        token: Arc::new(token),
        events: EventBus::new(),
        started_at: std::time::Instant::now(),
    })
}

pub fn router(state: ServerState) -> Router {
    let protected = Router::new()
        .route("/api/apps", get(routes::list_apps))
        .route(
            "/api/apps/:app/providers",
            get(routes::apps::list_providers).post(routes::apps::create_provider),
        )
        .route(
            "/api/apps/:app/providers/:id",
            get(routes::apps::get_provider)
                .patch(routes::apps::update_provider)
                .delete(routes::apps::delete_provider),
        )
        .route(
            "/api/apps/:app/providers/:id/switch",
            post(routes::apps::switch_provider),
        )
        .route(
            "/api/apps/:app/providers/:id/fetch-models",
            get(routes::apps::fetch_models),
        )
        .route(
            "/api/apps/:app/providers/import-live",
            post(routes::apps::import_live),
        )
        .route(
            "/api/apps/:app/prompts",
            get(routes::prompts::list_prompts).post(routes::prompts::create_prompt),
        )
        .route(
            "/api/apps/:app/prompts/:id",
            get(routes::prompts::get_prompt)
                .patch(routes::prompts::update_prompt)
                .delete(routes::prompts::delete_prompt),
        )
        .route(
            "/api/apps/:app/prompts/:id/activate",
            post(routes::prompts::activate_prompt),
        )
        .route(
            "/api/apps/:app/common-config",
            get(routes::common_config::get_common_config)
                .put(routes::common_config::put_common_config),
        )
        .route(
            "/api/apps/codex/model-catalog",
            get(routes::codex_catalog::get_catalog).put(routes::codex_catalog::put_catalog),
        )
        .route(
            "/api/prompts/common",
            get(routes::prompts_common::list_common).post(routes::prompts_common::create_common),
        )
        .route(
            "/api/prompts/common/off",
            post(routes::prompts_common::off_common),
        )
        .route(
            "/api/prompts/common/reapply",
            post(routes::prompts_common::reapply_common),
        )
        .route(
            "/api/prompts/common/:id",
            patch(routes::prompts_common::update_common)
                .delete(routes::prompts_common::delete_common),
        )
        .route(
            "/api/prompts/common/:id/use",
            post(routes::prompts_common::use_common),
        )
        .route(
            "/api/mcp",
            get(routes::mcp::list_servers).post(routes::mcp::upsert_server),
        )
        .route(
            "/api/mcp/:id",
            get(routes::mcp::get_server)
                .patch(routes::mcp::set_server_apps)
                .delete(routes::mcp::delete_server),
        )
        .route("/api/skills", get(routes::skills::list_skills))
        .route(
            "/api/skills/:directory/apps",
            patch(routes::skills::set_skill_apps),
        )
        .route("/api/sync/webdav/upload", post(routes::sync::webdav_upload))
        .route(
            "/api/sync/webdav/download",
            post(routes::sync::webdav_download),
        )
        .route("/api/proxy", get(routes::sync::proxy_status))
        .route(
            "/api/settings",
            get(routes::settings::get_settings).patch(routes::settings::update_settings),
        )
        .route("/api/events", get(routes::events_route))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            auth::auth_middleware,
        ));

    // CORS is deliberately open (`*`): the security boundary is the bearer
    // token, not the origin — no cookies or ambient credentials are used, so
    // cross-origin pages can only ever receive 401 without the token. The
    // layer must sit OUTSIDE the auth middleware: browser preflights carry no
    // Authorization header and would otherwise be rejected before CORS can
    // answer them. `max_age` lets the browser cache the preflight for 10
    // minutes instead of re-asking before every request.
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any)
        .max_age(Duration::from_secs(600));

    Router::new()
        // Health and the OpenAPI document stay reachable without a token so
        // that probes and tooling can discover the API before authenticating.
        .route("/api/health", get(routes::health))
        .route("/api/openapi.json", get(routes::docs::openapi_json))
        .merge(protected)
        .layer(cors)
        .with_state(state)
}

/// Serve on an already-bound listener. Returns the bound address so callers
/// (and tests binding port 0) can discover the effective port.
pub async fn serve(
    listener: TcpListener,
    state: ServerState,
) -> Result<(), crate::error::AppError> {
    let app = router(state);
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
        .map_err(|e| crate::error::AppError::Message(format!("server stopped: {e}")))
}

/// Blocking entry point used by the CLI command.
pub fn run(config: ServerConfig) -> Result<(), crate::error::AppError> {
    let state = build_state(&config)?;
    let addr: SocketAddr = format!("{}:{}", config.host, config.port)
        .parse()
        .map_err(|e| crate::error::AppError::Message(format!("invalid bind address: {e}")))?;

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| {
            crate::error::AppError::Message(format!("failed to start async runtime: {e}"))
        })?;

    runtime.block_on(async move {
        let listener = TcpListener::bind(addr)
            .await
            .map_err(|e| crate::error::AppError::Message(format!("failed to bind {addr}: {e}")))?;
        serve(listener, state).await
    })
}

/// Resolve an `AppType` path segment (`:app`), accepting every CLI alias.
pub(crate) fn parse_app(raw: &str) -> ApiResult<crate::app_config::AppType> {
    use std::str::FromStr;
    crate::app_config::AppType::from_str(raw)
        .map_err(|_| ApiError::not_found(format!("unknown app '{raw}'")))
}

#[cfg(test)]
mod tests;

//! Clone an existing provider from another app into the current app's format.
//!
//! Used by CLI `provider add` interactive flow:
//! pick source app → pick provider → convert settings → optional models.dev enrich.

use std::collections::HashSet;

use inquire::{Confirm, Select, Text};
use serde_json::{json, Map, Value};

use crate::app_config::AppType;
use crate::cli::commands::provider_input::{
    build_claude_settings_config_from_prompt, build_codex_settings_config_from_prompt,
    build_gemini_api_key_settings_config, current_timestamp, display_provider_summary,
    generate_provider_id_for_app,
};
use crate::cli::i18n::texts;
use crate::cli::ui::{info, warning};
use crate::error::AppError;
use crate::provider::{ClaudeApiKeyField, Provider, ProviderMeta};
use crate::services::models_dev::{openclaw_model_entry_from_upstream, ModelsDevCatalog};
use crate::services::stream_check::StreamCheckService;
use crate::store::AppState;

#[derive(Debug, Clone)]
pub(crate) struct SharedChannel {
    pub name: String,
    pub website_url: Option<String>,
    pub notes: Option<String>,
    pub base_url: String,
    pub api_key: String,
    /// Ordered model ids discovered on the source provider.
    pub model_ids: Vec<String>,
    pub primary_model: Option<String>,
    pub source_app: AppType,
    pub source_id: String,
}

struct AppPick {
    app: AppType,
    label: String,
}

impl std::fmt::Display for AppPick {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.label)
    }
}

struct ProviderPick {
    id: String,
    label: String,
}

impl std::fmt::Display for ProviderPick {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.label)
    }
}

fn inquire_err(e: impl std::fmt::Display) -> AppError {
    AppError::Message(texts::input_failed_error(&e.to_string()))
}

/// Whether the interactive add flow should offer clone (TTY, no flags).
pub(crate) fn should_offer_clone(has_noninteractive_input: bool) -> bool {
    std::io::stdin().is_terminal() && std::io::stdout().is_terminal() && !has_noninteractive_input
}

/// Ask the user if they want to clone; if yes, run clone and return the new provider.
/// Returns `None` when the user declines clone.
pub(crate) fn maybe_clone_provider(
    target_app: &AppType,
    existing_ids: &[String],
) -> Result<Option<Provider>, AppError> {
    crate::cli::terminal::disable_bracketed_paste_mode_best_effort();

    let clone = Confirm::new("Clone from an existing provider on another app?")
        .with_default(false)
        .with_help_message(
            "Copy base URL, API key, and models into this app's format (models.dev may enrich Pi/OpenClaw/Hermes)",
        )
        .prompt()
        .map_err(inquire_err)?;

    if !clone {
        return Ok(None);
    }

    let provider = run_clone_wizard(target_app, existing_ids)?;
    Ok(Some(provider))
}

pub(crate) fn run_clone_wizard(
    target_app: &AppType,
    existing_ids: &[String],
) -> Result<Provider, AppError> {
    let state = AppState::try_new()?;
    let config = state.config.read().map_err(AppError::from)?;

    let mut app_picks = Vec::new();
    for app in AppType::all() {
        if app.as_str() == target_app.as_str() {
            continue;
        }
        let count = config
            .get_manager(&app)
            .map(|m| m.providers.len())
            .unwrap_or(0);
        if count == 0 {
            continue;
        }
        app_picks.push(AppPick {
            label: format!("{} ({} provider(s))", app.as_str(), count),
            app,
        });
    }
    drop(config);

    if app_picks.is_empty() {
        return Err(AppError::Message(
            "No providers found on other apps to clone from.".into(),
        ));
    }

    let source_app = Select::new("Source app (type to filter)", app_picks)
        .with_page_size(15)
        .prompt()
        .map_err(inquire_err)?
        .app;

    let config = state.config.read().map_err(AppError::from)?;
    let manager = config
        .get_manager(&source_app)
        .ok_or_else(|| AppError::Message(texts::app_config_not_found(source_app.as_str())))?;

    let mut provider_picks = Vec::new();
    for (id, provider) in &manager.providers {
        let url = StreamCheckService::extract_base_url(provider, &source_app)
            .unwrap_or_else(|_| "—".into());
        provider_picks.push(ProviderPick {
            label: format!("{}  [{}]  {}", provider.name, id, url),
            id: id.clone(),
        });
    }
    provider_picks.sort_by(|a, b| {
        a.label
            .to_ascii_lowercase()
            .cmp(&b.label.to_ascii_lowercase())
    });

    if provider_picks.is_empty() {
        return Err(AppError::Message("Selected app has no providers.".into()));
    }

    let picked_id = Select::new("Source provider (type to filter)", provider_picks)
        .with_page_size(15)
        .prompt()
        .map_err(inquire_err)?
        .id;

    let source = manager.providers.get(&picked_id).cloned().ok_or_else(|| {
        let msg = texts::entity_not_found(texts::entity_provider(), &picked_id);
        AppError::localized("provider.not_found", msg.clone(), msg)
    })?;
    drop(config);

    let channel = extract_shared_channel(&source_app, &source)?;
    println!(
        "{}",
        info(&format!(
            "Cloning '{}' from {} → {} (endpoint: {})",
            channel.name,
            channel.source_app.as_str(),
            target_app.as_str(),
            channel.base_url
        ))
    );

    let default_name = channel.name.clone();
    let name = Text::new("Provider name on this app")
        .with_initial_value(&default_name)
        .prompt()
        .map_err(inquire_err)?;
    let name = {
        let t = name.trim();
        if t.is_empty() {
            default_name
        } else {
            t.to_string()
        }
    };

    let id = generate_provider_id_for_app(target_app, &name, existing_ids);
    let provider = build_provider_from_channel(target_app, &channel, &name, &id)?;

    display_provider_summary(&provider, target_app);
    if !Confirm::new("Save this cloned provider?")
        .with_default(true)
        .prompt()
        .map_err(inquire_err)?
    {
        return Err(AppError::Message("Clone cancelled.".into()));
    }

    // Caller (provider add) persists via ProviderService::add.
    let _ = state;
    Ok(provider)
}

pub(crate) fn extract_shared_channel(
    app_type: &AppType,
    provider: &Provider,
) -> Result<SharedChannel, AppError> {
    let base_url = StreamCheckService::extract_base_url(provider, app_type).map_err(|err| {
        AppError::Message(format!(
            "Cannot clone '{}': missing base URL ({err})",
            provider.name
        ))
    })?;

    let api_key = extract_api_key(app_type, provider).unwrap_or_default();
    if api_key.trim().is_empty() {
        println!(
            "{}",
            warning("Source provider has no API key; you may need to set it after clone.")
        );
    }

    let model_ids = extract_model_ids(app_type, provider);
    let primary_model = model_ids.first().cloned();

    Ok(SharedChannel {
        name: provider.name.clone(),
        website_url: provider.website_url.clone(),
        notes: provider.notes.clone(),
        base_url,
        api_key,
        model_ids,
        primary_model,
        source_app: app_type.clone(),
        source_id: provider.id.clone(),
    })
}

fn extract_api_key(app_type: &AppType, provider: &Provider) -> Option<String> {
    match app_type {
        AppType::Claude => StreamCheckService::extract_claude_key(provider),
        AppType::Codex => StreamCheckService::extract_codex_key(provider),
        AppType::Gemini => provider
            .settings_config
            .get("env")
            .and_then(|env| {
                env.get("GEMINI_API_KEY")
                    .or_else(|| env.get("GOOGLE_API_KEY"))
            })
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string),
        AppType::OpenCode => provider
            .settings_config
            .get("options")
            .and_then(|o| o.get("apiKey"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .or_else(|| {
                provider
                    .settings_config
                    .get("apiKey")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
            }),
        AppType::Hermes => provider
            .settings_config
            .get("api_key")
            .or_else(|| provider.settings_config.get("apiKey"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string),
        AppType::OpenClaw | AppType::Pi | AppType::Grok => provider
            .settings_config
            .get("apiKey")
            .or_else(|| provider.settings_config.get("api_key"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string),
    }
}

fn extract_model_ids(app_type: &AppType, provider: &Provider) -> Vec<String> {
    let mut ids = Vec::new();
    let mut seen = HashSet::new();

    let mut push = |id: &str| {
        let id = id.trim();
        if !id.is_empty() && seen.insert(id.to_string()) {
            ids.push(id.to_string());
        }
    };

    match app_type {
        AppType::Claude => {
            if let Some(env) = provider.settings_config.get("env") {
                for key in [
                    "ANTHROPIC_MODEL",
                    "ANTHROPIC_DEFAULT_SONNET_MODEL",
                    "ANTHROPIC_DEFAULT_OPUS_MODEL",
                    "ANTHROPIC_DEFAULT_HAIKU_MODEL",
                ] {
                    if let Some(m) = env.get(key).and_then(Value::as_str) {
                        push(m);
                    }
                }
            }
        }
        AppType::Codex => {
            if let Some(cfg) = provider
                .settings_config
                .get("config")
                .and_then(Value::as_str)
            {
                if let Ok(table) = toml::from_str::<toml::Table>(cfg) {
                    if let Some(m) = table.get("model").and_then(|v| v.as_str()) {
                        push(m);
                    }
                }
            }
            if let Some(catalog) = provider
                .settings_config
                .get("modelCatalog")
                .and_then(|v| v.get("models"))
                .and_then(Value::as_array)
            {
                for entry in catalog {
                    if let Some(m) = entry.get("model").and_then(Value::as_str) {
                        push(m);
                    }
                }
            }
        }
        AppType::Gemini => {
            if let Some(m) = provider
                .settings_config
                .get("env")
                .and_then(|e| e.get("GEMINI_MODEL"))
                .and_then(Value::as_str)
            {
                push(m);
            }
        }
        AppType::OpenCode => {
            if let Some(models) = provider
                .settings_config
                .get("models")
                .and_then(Value::as_object)
            {
                for id in models.keys() {
                    push(id);
                }
            }
        }
        AppType::Hermes | AppType::OpenClaw | AppType::Pi | AppType::Grok => {
            if let Some(models) = provider
                .settings_config
                .get("models")
                .and_then(Value::as_array)
            {
                for entry in models {
                    if let Some(id) = entry.get("id").and_then(Value::as_str) {
                        push(id);
                    }
                }
            }
        }
    }

    ids
}

pub(crate) fn build_provider_from_channel(
    target_app: &AppType,
    channel: &SharedChannel,
    name: &str,
    id: &str,
) -> Result<Provider, AppError> {
    let primary = channel
        .primary_model
        .clone()
        .or_else(|| channel.model_ids.first().cloned())
        .unwrap_or_default();

    let settings_config = match target_app {
        AppType::Claude => {
            let field = ClaudeApiKeyField::AuthToken;
            let model_fields = vec![("ANTHROPIC_MODEL", {
                if primary.is_empty() {
                    None
                } else {
                    Some(primary.clone())
                }
            })];
            build_claude_settings_config_from_prompt(
                None,
                field,
                &channel.api_key,
                &channel.base_url,
                model_fields,
                false,
            )
        }
        AppType::Codex => build_codex_settings_config_from_prompt(
            None,
            &channel.api_key,
            &channel.base_url,
            &primary,
            name,
        ),
        AppType::Gemini => build_gemini_api_key_settings_config(
            None,
            &channel.api_key,
            &channel.base_url,
            &primary,
        ),
        AppType::OpenCode => build_opencode_settings_from_channel(channel, &primary),
        AppType::Hermes => build_hermes_settings_from_channel(channel)?,
        AppType::OpenClaw | AppType::Pi | AppType::Grok => {
            build_openclaw_settings_from_channel(channel)?
        }
    };

    let notes = match (
        &channel.notes,
        channel.source_app.as_str(),
        &channel.source_id,
    ) {
        (Some(n), app, sid) if !n.trim().is_empty() => {
            Some(format!("{n} [cloned from {app}:{sid}]"))
        }
        (_, app, sid) => Some(format!("cloned from {app}:{sid}")),
    };

    let mut meta = ProviderMeta::default();
    if matches!(target_app, AppType::Codex) {
        // Prefer responses; user can change later. Chat sources may need proxy.
        meta.api_format = Some("openai_responses".into());
    }

    Ok(Provider {
        id: id.to_string(),
        name: name.to_string(),
        settings_config,
        website_url: channel.website_url.clone(),
        category: Some("cloned".into()),
        created_at: Some(current_timestamp()),
        sort_index: None,
        notes,
        icon: None,
        icon_color: None,
        meta: Some(meta),
        in_failover_queue: false,
    })
}

fn build_opencode_settings_from_channel(channel: &SharedChannel, primary: &str) -> Value {
    let mut models = Map::new();
    let ids = if channel.model_ids.is_empty() && !primary.is_empty() {
        vec![primary.to_string()]
    } else {
        channel.model_ids.clone()
    };
    for id in &ids {
        models.insert(
            id.clone(),
            json!({
                "name": id,
            }),
        );
    }
    if models.is_empty() {
        models.insert("default".into(), json!({ "name": "default" }));
    }

    json!({
        "npm": "@ai-sdk/openai-compatible",
        "name": channel.name,
        "options": {
            "baseURL": channel.base_url,
            "apiKey": channel.api_key,
        },
        "models": models,
    })
}

fn build_hermes_settings_from_channel(channel: &SharedChannel) -> Result<Value, AppError> {
    let catalog = ModelsDevCatalog::load_or_fetch().ok();
    let mut models = Vec::new();
    let mut enriched = 0usize;
    let mut bare = 0usize;

    let ids = if channel.model_ids.is_empty() {
        if let Some(p) = &channel.primary_model {
            vec![p.clone()]
        } else {
            Vec::new()
        }
    } else {
        channel.model_ids.clone()
    };

    for id in ids {
        let (entry, ok) = openclaw_model_entry_from_upstream(&id, catalog.as_ref());
        // Hermes uses a looser models array; map OpenClaw-shaped entry.
        let mut obj = entry.as_object().cloned().unwrap_or_default();
        // Hermes often uses id/name; contextWindow is fine in extra.
        if ok {
            enriched += 1;
        } else {
            bare += 1;
        }
        // Ensure id present
        obj.entry("id".to_string()).or_insert_with(|| json!(id));
        models.push(Value::Object(obj));
    }

    if models.is_empty() {
        models.push(json!({ "id": "default", "name": "default" }));
        bare += 1;
    }

    println!(
        "{}",
        info(&format!(
            "Hermes models: {enriched} enriched from models.dev, {bare} id-only"
        ))
    );

    Ok(json!({
        "api_mode": "chat_completions",
        "base_url": channel.base_url,
        "api_key": channel.api_key,
        "models": models,
    }))
}

fn build_openclaw_settings_from_channel(channel: &SharedChannel) -> Result<Value, AppError> {
    let catalog = ModelsDevCatalog::load_or_fetch().ok();
    let mut models = Vec::new();
    let mut enriched = 0usize;
    let mut bare = 0usize;

    let ids = if channel.model_ids.is_empty() {
        if let Some(p) = &channel.primary_model {
            vec![p.clone()]
        } else {
            Vec::new()
        }
    } else {
        channel.model_ids.clone()
    };

    for id in ids {
        let (entry, ok) = openclaw_model_entry_from_upstream(&id, catalog.as_ref());
        if ok {
            enriched += 1;
        } else {
            bare += 1;
        }
        models.push(entry);
    }

    if models.is_empty() {
        // OpenClaw/Pi require non-empty models array
        models.push(json!({ "id": "default" }));
        bare += 1;
        println!(
            "{}",
            warning("No models on source provider; wrote placeholder {\"id\":\"default\"}. Edit after save.")
        );
    } else {
        println!(
            "{}",
            info(&format!(
                "Pi/OpenClaw models: {enriched} enriched from models.dev, {bare} id-only (edit JSON if needed)"
            ))
        );
    }

    Ok(json!({
        "api": "openai-responses",
        "apiKey": channel.api_key,
        "baseUrl": channel.base_url,
        "models": models,
    }))
}

use std::io::IsTerminal;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn extract_claude_models_and_key() {
        let provider = Provider::with_id(
            "p1".into(),
            "P1".into(),
            json!({
                "env": {
                    "ANTHROPIC_AUTH_TOKEN": "sk-test",
                    "ANTHROPIC_BASE_URL": "https://api.example.com",
                    "ANTHROPIC_MODEL": "claude-sonnet-4",
                    "ANTHROPIC_DEFAULT_HAIKU_MODEL": "claude-haiku"
                }
            }),
            None,
        );
        let ch = extract_shared_channel(&AppType::Claude, &provider).expect("extract");
        assert_eq!(ch.base_url, "https://api.example.com");
        assert_eq!(ch.api_key, "sk-test");
        assert!(ch.model_ids.contains(&"claude-sonnet-4".to_string()));
        assert!(ch.model_ids.contains(&"claude-haiku".to_string()));
    }

    #[test]
    fn clone_claude_to_pi_builds_openclaw_shape() {
        let channel = SharedChannel {
            name: "Relay".into(),
            website_url: None,
            notes: None,
            base_url: "https://relay.example/v1".into(),
            api_key: "sk-abc".into(),
            model_ids: vec!["gpt-5.4".into()],
            primary_model: Some("gpt-5.4".into()),
            source_app: AppType::Codex,
            source_id: "relay".into(),
        };
        let provider =
            build_provider_from_channel(&AppType::Pi, &channel, "Relay", "relay").expect("build");
        assert_eq!(
            provider.settings_config["baseUrl"],
            "https://relay.example/v1"
        );
        assert_eq!(provider.settings_config["apiKey"], "sk-abc");
        assert!(provider.settings_config["models"].as_array().is_some());
        assert_eq!(provider.settings_config["models"][0]["id"], "gpt-5.4");
    }

    #[test]
    fn clone_to_codex_writes_config_toml() {
        let channel = SharedChannel {
            name: "X".into(),
            website_url: None,
            notes: None,
            base_url: "https://api.example.com/v1".into(),
            api_key: "sk-x".into(),
            model_ids: vec!["gpt-4o".into()],
            primary_model: Some("gpt-4o".into()),
            source_app: AppType::Claude,
            source_id: "x".into(),
        };
        let provider =
            build_provider_from_channel(&AppType::Codex, &channel, "X", "x").expect("build");
        let cfg = provider.settings_config["config"].as_str().unwrap_or("");
        assert!(cfg.contains("gpt-4o"));
        assert!(cfg.contains("https://api.example.com/v1"));
        assert_eq!(provider.settings_config["auth"]["OPENAI_API_KEY"], "sk-x");
    }
}

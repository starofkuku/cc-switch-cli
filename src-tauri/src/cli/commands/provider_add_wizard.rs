//! Interactive CLI wizards for `provider add`.
//!
//! - Claude/Codex: Custom or models.dev catalog (filterable).
//! - All other apps: guided field prompts via `prompt_settings_config`
//!   (same path as `provider edit`, no TUI required).

use std::io::IsTerminal;

use inquire::{Confirm, Select, Text};
use serde_json::Value;

use crate::app_config::AppType;
use crate::cli::commands::provider_input::{
    build_claude_settings_config_from_prompt, build_codex_settings_config_from_prompt,
    current_timestamp, display_provider_summary, generate_provider_id_for_app,
    prompt_basic_fields, prompt_model_with_optional_v1_fetch, prompt_optional_fields,
    prompt_settings_config, SettingsConfigPromptResult,
};
use crate::cli::i18n::texts;
use crate::cli::ui::{info, success, warning};
use crate::error::AppError;
use crate::provider::{ClaudeApiKeyField, Provider, ProviderMeta};
use crate::services::models_dev::{
    model_context_limit, model_list, ModelsDevCatalog, ModelsDevModel, ModelsDevProtocolPool,
    ModelsDevProvider,
};
use crate::services::ProviderService;

const CLAUDE_API_FORMAT_ANTHROPIC: &str = "anthropic";
const CODEX_API_FORMAT_CHAT: &str = "openai_chat";
const CODEX_API_FORMAT_RESPONSES: &str = "openai_responses";

#[derive(Debug, Clone)]
enum SourceChoice {
    Custom,
    Vendor {
        provider_id: String,
        protocol: ModelsDevProtocolPool,
    },
}

struct SourceItem {
    label: String,
    choice: SourceChoice,
}

impl std::fmt::Display for SourceItem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.label)
    }
}

struct ModelItem {
    label: String,
    model: ModelsDevModel,
}

impl std::fmt::Display for ModelItem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.label)
    }
}

pub(crate) fn supports_catalog_wizard(app_type: &AppType) -> bool {
    matches!(app_type, AppType::Claude | AppType::Codex)
}

/// True when `provider add` should run a full CLI guided flow (no flags, TTY).
pub(crate) fn should_run_interactive_add(has_noninteractive_input: bool) -> bool {
    std::io::stdin().is_terminal()
        && std::io::stdout().is_terminal()
        && !has_noninteractive_input
}

pub(crate) fn should_run_catalog_wizard(
    app_type: &AppType,
    has_noninteractive_input: bool,
) -> bool {
    supports_catalog_wizard(app_type) && should_run_interactive_add(has_noninteractive_input)
}

/// Guided CLI add for apps without the models.dev catalog wizard
/// (Gemini, OpenCode, Hermes, OpenClaw, Pi, Grok, …).
///
/// Prompts: name → settings (app-specific) → optional fields → confirm.
pub(crate) fn run_interactive_manual_add(
    app_type: &AppType,
    existing_ids: &[String],
) -> Result<(Provider, Option<SettingsConfigPromptResult>), AppError> {
    crate::cli::terminal::disable_bracketed_paste_mode_best_effort();

    println!(
        "{}",
        info(&format!(
            "Interactive provider add for {} (guided prompts)",
            app_type.as_str()
        ))
    );

    let (name, website_url) = prompt_basic_fields(None)?;
    let id = generate_provider_id_for_app(app_type, &name, existing_ids);

    let prompt_result = prompt_settings_config(app_type, None, None, false, &name)?;

    let optional = if Confirm::new(texts::modify_optional_fields_prompt())
        .with_default(false)
        .prompt()
        .map_err(inquire_err)?
    {
        prompt_optional_fields(None)?
    } else {
        crate::cli::commands::provider_input::OptionalFields {
            notes: None,
            icon: None,
            icon_color: None,
            sort_index: None,
        }
    };

    let mut provider = Provider {
        id,
        name,
        settings_config: prompt_result.settings_config.clone(),
        website_url,
        category: None,
        created_at: Some(current_timestamp()),
        sort_index: optional.sort_index,
        notes: optional.notes,
        icon: optional.icon,
        icon_color: optional.icon_color,
        meta: None,
        in_failover_queue: false,
    };
    apply_wizard_meta(app_type, &mut provider, Some(&prompt_result));

    Ok((provider, Some(prompt_result)))
}

fn protocol_pools(app_type: &AppType) -> Result<Vec<ModelsDevProtocolPool>, AppError> {
    match app_type {
        AppType::Claude => Ok(vec![ModelsDevProtocolPool::Anthropic]),
        AppType::Codex => Ok(vec![
            ModelsDevProtocolPool::OpenAiResponses,
            ModelsDevProtocolPool::OpenAiChat,
        ]),
        _ => Err(AppError::Message(
            "models.dev catalog add is only available for Claude and Codex".into(),
        )),
    }
}

fn protocol_label(protocol: ModelsDevProtocolPool) -> &'static str {
    match protocol {
        ModelsDevProtocolPool::Anthropic => "Anthropic",
        ModelsDevProtocolPool::OpenAiResponses => "Responses / direct",
        ModelsDevProtocolPool::OpenAiChat => "Chat / proxy required",
    }
}

fn default_protocol(app_type: &AppType) -> ModelsDevProtocolPool {
    match app_type {
        AppType::Codex => ModelsDevProtocolPool::OpenAiResponses,
        _ => ModelsDevProtocolPool::Anthropic,
    }
}

fn inquire_err(e: impl std::fmt::Display) -> AppError {
    AppError::Message(texts::input_failed_error(&e.to_string()))
}

fn select_filterable<T: std::fmt::Display>(prompt: &str, options: Vec<T>) -> Result<T, AppError> {
    if options.is_empty() {
        return Err(AppError::Message(
            "No options available for selection.".into(),
        ));
    }
    // inquire Select supports typing to filter options when the fuzzy feature is enabled.
    Select::new(prompt, options)
        .with_page_size(15)
        .prompt()
        .map_err(inquire_err)
}

fn prompt_required_text(prompt: &str) -> Result<String, AppError> {
    loop {
        let value = Text::new(prompt).prompt().map_err(inquire_err)?;
        let trimmed = value.trim();
        if !trimmed.is_empty() {
            return Ok(trimmed.to_string());
        }
        println!("{}", warning("Value cannot be empty."));
    }
}

fn prompt_optional_text(prompt: &str, default: &str) -> Result<String, AppError> {
    let value = Text::new(prompt)
        .with_initial_value(default)
        .prompt()
        .map_err(inquire_err)?;
    let trimmed = value.trim();
    if trimmed.is_empty() {
        Ok(default.trim().to_string())
    } else {
        Ok(trimmed.to_string())
    }
}

/// Run interactive catalog/custom add for Claude or Codex. Returns the built provider.
pub(crate) fn run_catalog_add_wizard(
    app_type: &AppType,
    existing_ids: &[String],
) -> Result<(Provider, Option<SettingsConfigPromptResult>), AppError> {
    crate::cli::terminal::disable_bracketed_paste_mode_best_effort();

    println!(
        "{}",
        info("Interactive provider add (Custom or models.dev catalog)")
    );

    let pools = protocol_pools(app_type)?;
    println!("{}", info("Loading models.dev catalog…"));
    let catalog = ModelsDevCatalog::load_or_fetch()?;
    let vendors: Vec<(ModelsDevProtocolPool, &ModelsDevProvider)> = pools
        .into_iter()
        .flat_map(|protocol| {
            catalog
                .providers_for_pool(protocol)
                .into_iter()
                .map(move |vendor| (protocol, vendor))
        })
        .collect();

    let mut items = Vec::with_capacity(vendors.len() + 1);
    items.push(SourceItem {
        label: "Custom provider (manual endpoint)".to_string(),
        choice: SourceChoice::Custom,
    });
    for (protocol, vendor) in &vendors {
        let name = vendor.name.as_deref().unwrap_or(vendor.id.as_str());
        let api = vendor.api.as_deref().unwrap_or("");
        items.push(SourceItem {
            label: format!(
                "{name}  [{id}]  [{protocol}]  {api}",
                id = vendor.id,
                protocol = protocol_label(*protocol)
            ),
            choice: SourceChoice::Vendor {
                provider_id: vendor.id.clone(),
                protocol: *protocol,
            },
        });
    }

    let selected = select_filterable("Select provider source (type to filter)", items)?;
    match selected.choice {
        SourceChoice::Custom => run_custom_path(app_type, existing_ids),
        SourceChoice::Vendor {
            provider_id,
            protocol,
        } => {
            let vendor = catalog
                .providers
                .get(&provider_id)
                .ok_or_else(|| AppError::Message(format!("Vendor '{provider_id}' not found")))?;
            run_vendor_path(app_type, existing_ids, vendor, protocol)
        }
    }
}

fn run_custom_path(
    app_type: &AppType,
    existing_ids: &[String],
) -> Result<(Provider, Option<SettingsConfigPromptResult>), AppError> {
    let name = prompt_required_text("Provider name")?;
    let id = generate_provider_id_for_app(app_type, &name, existing_ids);
    let base_url = prompt_required_text("Base URL")?;
    let api_key = prompt_required_text("API key")?;

    let model = resolve_custom_model(app_type, &base_url, &api_key)?;

    let (settings_config, prompt_result) =
        build_settings_for_fields(app_type, &name, &base_url, &api_key, model.as_deref())?;

    let mut provider = Provider {
        id,
        name,
        settings_config,
        website_url: None,
        category: None,
        created_at: Some(current_timestamp()),
        sort_index: None,
        notes: None,
        icon: None,
        icon_color: None,
        meta: None,
        in_failover_queue: false,
    };
    apply_wizard_meta(app_type, &mut provider, prompt_result.as_ref());
    apply_wizard_api_format(app_type, &mut provider, default_protocol(app_type))?;

    Ok((provider, prompt_result))
}

fn resolve_custom_model(
    app_type: &AppType,
    base_url: &str,
    api_key: &str,
) -> Result<Option<String>, AppError> {
    let _ = app_type;
    let mut remote_models: Option<Vec<String>> = None;
    prompt_model_with_optional_v1_fetch(
        "Default model",
        "gpt-5.4",
        None,
        base_url,
        api_key,
        &mut remote_models,
    )
}

fn run_vendor_path(
    app_type: &AppType,
    existing_ids: &[String],
    vendor: &ModelsDevProvider,
    protocol: ModelsDevProtocolPool,
) -> Result<(Provider, Option<SettingsConfigPromptResult>), AppError> {
    let default_name = vendor.name.clone().unwrap_or_else(|| vendor.id.clone());
    let name = prompt_optional_text("Provider name", &default_name)?;
    let id = generate_provider_id_for_app(app_type, &name, existing_ids);
    let base_url = vendor
        .api
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| AppError::Message("Selected vendor has no API endpoint".into()))?
        .to_string();

    println!("{}", info(&format!("Endpoint: {base_url}")));
    if matches!(
        (app_type, protocol),
        (AppType::Codex, ModelsDevProtocolPool::OpenAiChat)
    ) {
        println!(
            "{}",
            warning(
                "This provider uses Chat Completions. Enable the CC-Switch proxy when using it with Codex."
            )
        );
    }
    let api_key = prompt_required_text("API key")?;

    let models = model_list(vendor);
    if models.is_empty() {
        return Err(AppError::Message(format!(
            "Vendor '{}' has no models in the local catalog. Run `cc-switch provider catalog refresh`.",
            vendor.id
        )));
    }

    let items: Vec<ModelItem> = models
        .into_iter()
        .map(|model| {
            let display = model
                .name
                .as_deref()
                .filter(|n| !n.is_empty())
                .unwrap_or(model.id.as_str());
            let context = model_context_limit(&model)
                .map(|n| format!("  (ctx {n})"))
                .unwrap_or_default();
            ModelItem {
                label: format!("{display}  [{}]{context}", model.id),
                model,
            }
        })
        .collect();

    let picked = select_filterable("Select model from catalog (type to filter)", items)?;
    let model_id = picked.model.id.clone();
    let context = model_context_limit(&picked.model);

    // context is recorded for apps that store it; Claude/Codex use model id only for now.
    if let Some(ctx) = context {
        println!("{}", info(&format!("Catalog context window: {ctx}")));
    }

    let (settings_config, prompt_result) = build_settings_for_fields(
        app_type,
        &name,
        &base_url,
        &api_key,
        Some(model_id.as_str()),
    )?;

    // Attach context metadata for Hermes/Pi-style consumers (non-breaking extra key).
    let settings_config = attach_catalog_model_meta(settings_config, &picked.model);

    let mut provider = Provider {
        id,
        name,
        settings_config,
        website_url: vendor.doc.clone(),
        category: Some("models.dev".into()),
        created_at: Some(current_timestamp()),
        sort_index: None,
        notes: Some(format!("models.dev:{}", vendor.id)),
        icon: None,
        icon_color: None,
        meta: None,
        in_failover_queue: false,
    };
    apply_wizard_meta(app_type, &mut provider, prompt_result.as_ref());
    apply_wizard_api_format(app_type, &mut provider, protocol)?;

    Ok((provider, prompt_result))
}

fn attach_catalog_model_meta(mut settings: Value, model: &ModelsDevModel) -> Value {
    let Some(obj) = settings.as_object_mut() else {
        return settings;
    };
    let mut meta = serde_json::Map::new();
    meta.insert("id".into(), Value::String(model.id.clone()));
    if let Some(name) = &model.name {
        meta.insert("name".into(), Value::String(name.clone()));
    }
    if let Some(ctx) = model_context_limit(model) {
        meta.insert("context".into(), Value::Number(ctx.into()));
    }
    obj.insert("modelsDevModel".into(), Value::Object(meta));
    settings
}

fn build_settings_for_fields(
    app_type: &AppType,
    name: &str,
    base_url: &str,
    api_key: &str,
    model: Option<&str>,
) -> Result<(Value, Option<SettingsConfigPromptResult>), AppError> {
    match app_type {
        AppType::Claude => {
            let field = ClaudeApiKeyField::AuthToken;
            let model_fields = vec![("ANTHROPIC_MODEL", model.map(str::to_string))];
            let settings = build_claude_settings_config_from_prompt(
                None,
                field,
                api_key,
                base_url,
                model_fields,
                false,
            );
            let prompt_result = SettingsConfigPromptResult {
                settings_config: settings.clone(),
                claude_api_key_field: Some(field),
            };
            Ok((settings, Some(prompt_result)))
        }
        AppType::Codex => {
            let settings = build_codex_settings_config_from_prompt(
                None,
                api_key,
                base_url,
                model.unwrap_or(""),
                name,
            );
            Ok((settings, None))
        }
        _ => Err(AppError::Message(
            "Catalog wizard only supports Claude and Codex".into(),
        )),
    }
}

fn apply_wizard_meta(
    app_type: &AppType,
    provider: &mut Provider,
    prompt_result: Option<&SettingsConfigPromptResult>,
) {
    if !matches!(app_type, AppType::Claude) {
        return;
    }
    let Some(field) = prompt_result.and_then(|r| r.claude_api_key_field) else {
        return;
    };
    match field {
        ClaudeApiKeyField::AuthToken => {
            if let Some(meta) = provider.meta.as_mut() {
                meta.api_key_field = None;
            }
        }
        ClaudeApiKeyField::ApiKey => {
            provider
                .meta
                .get_or_insert_with(ProviderMeta::default)
                .api_key_field = Some(field.as_env_key().to_string());
        }
    }
}

fn apply_wizard_api_format(
    app_type: &AppType,
    provider: &mut Provider,
    protocol: ModelsDevProtocolPool,
) -> Result<(), AppError> {
    match app_type {
        AppType::Claude => {
            // Native Anthropic: leave api_format unset (same as apply_claude_api_format).
            if let Some(meta) = provider.meta.as_mut() {
                meta.api_format = None;
            }
            if let Some(obj) = provider.settings_config.as_object_mut() {
                obj.remove("api_format");
                obj.remove("apiFormat");
                obj.remove("openrouter_compat_mode");
            }
        }
        AppType::Codex => {
            // Codex TOML stays on Responses; CC-Switch proxy routing uses this metadata.
            let api_format = match protocol {
                ModelsDevProtocolPool::OpenAiChat => CODEX_API_FORMAT_CHAT,
                _ => CODEX_API_FORMAT_RESPONSES,
            };
            provider
                .meta
                .get_or_insert_with(ProviderMeta::default)
                .api_format = Some(api_format.to_string());
            if let Some(obj) = provider.settings_config.as_object_mut() {
                obj.remove("api_format");
                obj.remove("apiFormat");
            }
        }
        _ => {}
    }
    let _ = CLAUDE_API_FORMAT_ANTHROPIC;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn interactive_add_supported_for_all_apps_via_catalog_or_manual() {
        for app in AppType::all() {
            if supports_catalog_wizard(&app) {
                assert!(matches!(app, AppType::Claude | AppType::Codex));
            } else {
                // Non-catalog apps use run_interactive_manual_add in the TTY path.
                assert!(matches!(
                    app,
                    AppType::Gemini
                        | AppType::OpenCode
                        | AppType::Hermes
                        | AppType::OpenClaw
                        | AppType::Pi
                        | AppType::Grok
                ));
            }
        }
    }

    fn codex_provider() -> Provider {
        Provider::with_id(
            "catalog".to_string(),
            "Catalog".to_string(),
            json!({
                "config": "model_provider = \"custom\"\n\n[model_providers.custom]\nbase_url = \"https://example.com/v1\"\nwire_api = \"responses\"\n"
            }),
            None,
        )
    }

    #[test]
    fn codex_chat_catalog_provider_keeps_responses_wire_api() {
        let mut provider = codex_provider();

        apply_wizard_api_format(
            &AppType::Codex,
            &mut provider,
            ModelsDevProtocolPool::OpenAiChat,
        )
        .expect("apply chat format");

        assert_eq!(
            provider.meta.and_then(|meta| meta.api_format),
            Some(CODEX_API_FORMAT_CHAT.to_string())
        );
        assert!(provider.settings_config["config"]
            .as_str()
            .is_some_and(|config| config.contains("wire_api = \"responses\"")));
    }

    #[test]
    fn codex_responses_catalog_provider_marks_responses_format() {
        let mut provider = codex_provider();

        apply_wizard_api_format(
            &AppType::Codex,
            &mut provider,
            ModelsDevProtocolPool::OpenAiResponses,
        )
        .expect("apply responses format");

        assert_eq!(
            provider.meta.and_then(|meta| meta.api_format),
            Some(CODEX_API_FORMAT_RESPONSES.to_string())
        );
    }
}

pub(crate) fn finish_wizard_add(
    app_type: AppType,
    provider: Provider,
    prompt_result: Option<SettingsConfigPromptResult>,
) -> Result<(), AppError> {
    finish_wizard_add_with_confirm(app_type, provider, prompt_result, false)
}

/// Persist a wizard-built provider. When `confirm` is true, show summary and ask first.
pub(crate) fn finish_wizard_add_with_confirm(
    app_type: AppType,
    provider: Provider,
    prompt_result: Option<SettingsConfigPromptResult>,
    confirm: bool,
) -> Result<(), AppError> {
    let state = crate::store::AppState::try_new()?;
    let mut provider = provider;
    apply_wizard_meta(&app_type, &mut provider, prompt_result.as_ref());

    display_provider_summary(&provider, &app_type);
    if confirm {
        let ok = Confirm::new(&texts::confirm_create_entity(texts::entity_provider()))
            .with_default(true)
            .prompt()
            .map_err(inquire_err)?;
        if !ok {
            println!("{}", info(texts::cancelled()));
            return Ok(());
        }
    }

    let provider_id = provider.id.clone();
    ProviderService::add(&state, app_type, provider)?;
    println!(
        "\n{}",
        success(&texts::entity_added_success(
            texts::entity_provider(),
            &provider_id
        ))
    );
    Ok(())
}

pub(crate) fn refresh_catalog_command() -> Result<(), AppError> {
    println!("{}", info("Refreshing models.dev catalog…"));
    let path = ModelsDevCatalog::refresh()?;
    let catalog = ModelsDevCatalog::load_from_path(&path)?;
    println!(
        "{}",
        success(&format!(
            "✓ Catalog updated ({} providers) → {}",
            catalog.providers.len(),
            path.display()
        ))
    );
    Ok(())
}

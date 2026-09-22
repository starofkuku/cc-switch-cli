use std::collections::HashMap;

use indexmap::IndexMap;
use serde_json::{Map, Value};

use crate::app_config::AppType;
use crate::codex_config::{get_codex_auth_path, get_codex_config_path};
use crate::config::{delete_file, get_claude_settings_path, read_json_file, write_json_file};
use crate::error::AppError;
use crate::provider::{Provider, ProviderMeta};
use crate::store::AppState;

#[derive(Clone)]
pub(super) enum LiveSnapshot {
    Claude {
        settings: Option<Value>,
    },
    Codex {
        auth: Option<Value>,
        config: Option<String>,
    },
    Gemini {
        env: Option<HashMap<String, String>>,
        config: Option<Value>,
    },
    OpenCode {
        config: Option<Value>,
    },
    Hermes {
        config_source: Option<String>,
    },
    OpenClaw {
        config_source: Option<String>,
    },
    Pi {
        models: Option<Value>,
    },
    Grok {
        config_source: Option<String>,
    },
}

impl LiveSnapshot {
    pub(super) fn restore(&self) -> Result<(), AppError> {
        match self {
            LiveSnapshot::Claude { settings } => {
                let path = get_claude_settings_path();
                if let Some(value) = settings {
                    write_json_file(&path, value)?;
                } else if path.exists() {
                    delete_file(&path)?;
                }
            }
            LiveSnapshot::Codex { auth, config } => {
                let auth_path = get_codex_auth_path();
                let config_path = get_codex_config_path();
                if let Some(value) = auth {
                    write_json_file(&auth_path, value)?;
                } else if auth_path.exists() {
                    delete_file(&auth_path)?;
                }

                if let Some(text) = config {
                    crate::config::write_text_file(&config_path, text)?;
                } else if config_path.exists() {
                    delete_file(&config_path)?;
                }
            }
            LiveSnapshot::Gemini { env, config } => {
                use crate::gemini_config::{
                    get_gemini_env_path, get_gemini_settings_path, write_gemini_env_atomic,
                };

                let path = get_gemini_env_path();
                if let Some(env_map) = env {
                    write_gemini_env_atomic(env_map)?;
                } else if path.exists() {
                    delete_file(&path)?;
                }

                let settings_path = get_gemini_settings_path();
                match config {
                    Some(cfg) => {
                        write_json_file(&settings_path, cfg)?;
                    }
                    None if settings_path.exists() => {
                        delete_file(&settings_path)?;
                    }
                    _ => {}
                }
            }
            LiveSnapshot::OpenCode { config } => {
                let path = crate::opencode_config::get_opencode_config_path();
                if let Some(value) = config {
                    write_json_file(&path, value)?;
                } else if path.exists() {
                    delete_file(&path)?;
                }
            }
            LiveSnapshot::Hermes { config_source } => {
                let path = crate::hermes_config::get_hermes_config_path();
                if let Some(source) = config_source {
                    crate::hermes_config::write_hermes_config_source(source)?;
                } else if path.exists() {
                    delete_file(&path)?;
                }
            }
            LiveSnapshot::OpenClaw { config_source } => {
                let path = crate::openclaw_config::get_openclaw_config_path();
                if let Some(source) = config_source {
                    crate::openclaw_config::write_openclaw_config_source(source)?;
                } else if path.exists() {
                    delete_file(&path)?;
                }
            }
            LiveSnapshot::Pi { models } => {
                let path = crate::pi_config::get_pi_models_path();
                if let Some(value) = models {
                    crate::pi_config::write_pi_models(value)?;
                } else if path.exists() {
                    delete_file(&path)?;
                }
            }
            LiveSnapshot::Grok { config_source } => {
                let path = crate::grok_config::get_grok_config_path();
                if let Some(source) = config_source {
                    crate::grok_config::write_grok_config_source(source)?;
                } else if path.exists() {
                    delete_file(&path)?;
                }
            }
        }
        Ok(())
    }
}

pub(super) fn capture_live_snapshot(app_type: &AppType) -> Result<LiveSnapshot, AppError> {
    match app_type {
        AppType::Claude => {
            let path = get_claude_settings_path();
            let settings = if path.exists() {
                Some(read_json_file(&path)?)
            } else {
                None
            };
            Ok(LiveSnapshot::Claude { settings })
        }
        AppType::Codex => {
            let auth_path = get_codex_auth_path();
            let config_path = get_codex_config_path();
            let auth = if auth_path.exists() {
                Some(read_json_file(&auth_path)?)
            } else {
                None
            };
            let config = if config_path.exists() {
                Some(crate::codex_config::read_and_validate_codex_config_text()?)
            } else {
                None
            };
            Ok(LiveSnapshot::Codex { auth, config })
        }
        AppType::Gemini => {
            use crate::gemini_config::{
                get_gemini_env_path, get_gemini_settings_path, read_gemini_env,
            };

            let env_path = get_gemini_env_path();
            let env = if env_path.exists() {
                Some(read_gemini_env()?)
            } else {
                None
            };
            let settings_path = get_gemini_settings_path();
            let config = if settings_path.exists() {
                Some(read_json_file(&settings_path)?)
            } else {
                None
            };
            Ok(LiveSnapshot::Gemini { env, config })
        }
        AppType::OpenCode => {
            let path = crate::opencode_config::get_opencode_config_path();
            let config = if path.exists() {
                Some(crate::opencode_config::read_opencode_config()?)
            } else {
                None
            };
            Ok(LiveSnapshot::OpenCode { config })
        }
        AppType::Hermes => {
            let config_source = crate::hermes_config::read_hermes_config_source()?;
            Ok(LiveSnapshot::Hermes { config_source })
        }
        AppType::OpenClaw => {
            let config_source = crate::openclaw_config::read_openclaw_config_source()?;
            Ok(LiveSnapshot::OpenClaw { config_source })
        }
        AppType::Pi => {
            let path = crate::pi_config::get_pi_models_path();
            let models = if path.exists() {
                Some(crate::pi_config::read_pi_models()?)
            } else {
                None
            };
            Ok(LiveSnapshot::Pi { models })
        }
        AppType::Grok => {
            let config_source = crate::grok_config::read_grok_config_source()?;
            Ok(LiveSnapshot::Grok { config_source })
        }
    }
}

pub fn import_hermes_providers_from_live(state: &AppState) -> Result<usize, AppError> {
    let providers = crate::hermes_config::get_providers()?;
    if providers.is_empty() {
        return Ok(0);
    }

    let mut imported = 0usize;
    let existing_ids = state.db.get_provider_ids("hermes")?;

    for (id, settings_config) in providers {
        if id.trim().is_empty() {
            log::warn!("Skipping Hermes provider with empty id");
            continue;
        }
        if existing_ids.contains(&id) {
            log::debug!("Hermes provider '{id}' already exists in database, skipping");
            continue;
        }
        if !settings_config.is_object() {
            log::warn!("Skipping Hermes provider '{id}': config is not an object");
            continue;
        }

        let display_name = settings_config
            .get("name")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(&id)
            .to_string();
        let mut provider = Provider::with_id(id.clone(), display_name, settings_config, None);
        provider.meta = Some(ProviderMeta {
            live_config_managed: Some(true),
            ..Default::default()
        });

        if let Err(err) = state.db.save_provider("hermes", &provider) {
            log::warn!("Failed to import Hermes provider '{id}': {err}");
            continue;
        }

        imported += 1;
        log::info!("Imported Hermes provider '{id}' from live config");
    }

    Ok(imported)
}

pub fn sync_openclaw_providers_from_live(state: &AppState) -> Result<usize, AppError> {
    if !crate::openclaw_config::get_openclaw_config_path().exists() {
        return Ok(0);
    }

    let providers = crate::openclaw_config::get_providers()?;
    let mut live_providers = IndexMap::new();
    for (id, live_provider) in providers {
        if id.trim().is_empty() {
            log::warn!("Skipping OpenClaw live provider with blank id during local mirror");
            continue;
        }

        let config = match super::ProviderService::parse_openclaw_provider_settings(&live_provider)
        {
            Ok(config) => config,
            Err(err) => {
                log::warn!(
                    "Skipping malformed OpenClaw live provider '{id}' during local mirror: {err}"
                );
                continue;
            }
        };

        if let Err(err) = super::ProviderService::validate_openclaw_provider_models(&id, &config) {
            log::warn!(
                "Skipping model-less OpenClaw live provider '{id}' during local mirror: {err}"
            );
            continue;
        }

        if config.models.iter().any(|model| model.id.trim().is_empty()) {
            log::warn!(
                "Skipping OpenClaw live provider '{id}' during local mirror because a model id is blank"
            );
            continue;
        }

        let canonical =
            serde_json::to_value(config).map_err(|source| AppError::JsonSerialize { source })?;
        live_providers.insert(id, canonical);
    }

    let mut changed = 0;
    {
        let mut config = state.config.write().map_err(AppError::from)?;
        config.ensure_app(&AppType::OpenClaw);
        let manager = config
            .get_manager_mut(&AppType::OpenClaw)
            .ok_or_else(|| AppError::Config("OpenClaw manager missing".to_string()))?;

        for (id, live_provider) in live_providers {
            if let Some(existing) = manager.providers.get_mut(&id) {
                let mut provider_changed = false;
                if existing.id != id {
                    existing.id = id.clone();
                    provider_changed = true;
                }
                if is_auto_mirrored_openclaw_snapshot(existing) && existing.name != id {
                    existing.name = id.clone();
                    provider_changed = true;
                }
                if existing.settings_config != live_provider {
                    existing.settings_config = live_provider;
                    provider_changed = true;
                }
                if provider_changed {
                    changed += 1;
                }
                continue;
            }

            manager.providers.insert(
                id.clone(),
                Provider::with_id(id.clone(), id.clone(), live_provider, None),
            );
            changed += 1;
        }
    }

    if changed > 0 {
        state.save()?;
    }

    Ok(changed)
}

pub(super) fn is_auto_mirrored_openclaw_snapshot(provider: &Provider) -> bool {
    provider.website_url.is_none()
        && provider.category.is_none()
        && provider.created_at.is_none()
        && provider.sort_index.is_none()
        && provider.notes.is_none()
        && provider
            .meta
            .as_ref()
            .is_none_or(is_default_openclaw_common_config_marker)
        && provider.icon.is_none()
        && provider.icon_color.is_none()
        && !provider.in_failover_queue
}

fn is_default_openclaw_common_config_marker(meta: &ProviderMeta) -> bool {
    meta.apply_common_config == Some(false)
        && meta.codex_official.is_none()
        && meta.custom_endpoints.is_empty()
        && meta.usage_script.is_none()
        && meta.endpoint_auto_select.is_none()
        && meta.is_partner.is_none()
        && meta.partner_promotion_key.is_none()
        && meta.cost_multiplier.is_none()
        && meta.pricing_model_source.is_none()
        && meta.limit_daily_usd.is_none()
        && meta.limit_monthly_usd.is_none()
        && meta.test_config.is_none()
        && meta.proxy_config.is_none()
        && meta.api_format.is_none()
        && meta.prompt_cache_key.is_none()
        && meta.live_config_managed.is_none()
}

pub fn import_openclaw_providers_from_live(state: &AppState) -> Result<usize, AppError> {
    let providers = crate::openclaw_config::get_typed_providers()?;
    if providers.is_empty() {
        return Ok(0);
    }

    let mut imported = 0usize;
    let existing_ids = state.db.get_provider_ids("openclaw")?;

    for (id, config) in providers {
        if id.trim().is_empty() {
            log::warn!("Skipping OpenClaw provider with empty id");
            continue;
        }
        if config.models.is_empty() {
            log::warn!("Skipping OpenClaw provider '{id}': no models defined");
            continue;
        }
        if existing_ids.contains(&id) {
            log::debug!("OpenClaw provider '{id}' already exists in database, skipping");
            continue;
        }

        let settings_config = match serde_json::to_value(&config) {
            Ok(value) => value,
            Err(err) => {
                log::warn!("Failed to serialize OpenClaw provider '{id}': {err}");
                continue;
            }
        };
        let display_name = config
            .models
            .first()
            .and_then(|model| model.name.clone())
            .unwrap_or_else(|| id.clone());
        let mut provider = Provider::with_id(id.clone(), display_name, settings_config, None);
        provider.meta = Some(ProviderMeta {
            live_config_managed: Some(true),
            ..Default::default()
        });

        if let Err(err) = state.db.save_provider("openclaw", &provider) {
            log::warn!("Failed to import OpenClaw provider '{id}': {err}");
            continue;
        }

        {
            let mut config = state.config.write().map_err(AppError::from)?;
            config.ensure_app(&AppType::OpenClaw);
            if let Some(manager) = config.get_manager_mut(&AppType::OpenClaw) {
                manager.providers.insert(id.clone(), provider);
            }
        }

        imported += 1;
        log::info!("Imported OpenClaw provider '{id}' from live config");
    }

    Ok(imported)
}

/// Reports what a live import changed, so callers can distinguish "nothing new"
/// from "nothing at all".
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LiveImportSummary {
    /// Providers newly created from live config.
    pub added: usize,
    /// Existing providers whose stored config was overwritten with live content.
    pub updated: usize,
    /// Providers deleted because they no longer exist in live config.
    pub pruned: usize,
}

impl LiveImportSummary {
    pub fn is_empty(&self) -> bool {
        self.added == 0 && self.updated == 0 && self.pruned == 0
    }
}

/// How a live import should treat providers that the database already knows about.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LiveImportOptions {
    /// Overwrite existing providers with the live content instead of skipping them.
    pub update: bool,
    /// Delete providers that no longer exist in live config. Intentionally limited
    /// to providers marked `live_config_managed`, so rows that were only ever
    /// created inside cc-switch are never destroyed by a prune.
    pub prune: bool,
}

fn import_pi_providers_from_live_with(
    state: &AppState,
    options: LiveImportOptions,
) -> Result<LiveImportSummary, AppError> {
    let providers = crate::pi_config::get_providers()?;
    let mut summary = LiveImportSummary::default();
    let existing_ids = state.db.get_provider_ids("pi")?;

    for (id, settings_config) in providers.iter() {
        if id.trim().is_empty() || !settings_config.is_object() {
            continue;
        }
        let replace_existing = existing_ids.contains(id);
        if replace_existing && !options.update {
            continue;
        }

        let display_name = settings_config
            .get("name")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .unwrap_or(id)
            .to_string();

        // Live owns the user-visible fields (name + provider config). When the row
        // already exists, keep everything cc-switch authored around it — usage
        // scripts, custom endpoints, proxy overrides, icons, notes, ordering — so
        // `--update` refreshes live content without wiping local metadata.
        //
        // Read the existing row straight from SQLite: the in-memory MultiAppConfig
        // snapshot does not carry every `ProviderMeta` field, so merging from it
        // would silently drop things like `usageScript` or `customEndpoints`.
        let existing = if replace_existing {
            state.db.get_all_providers("pi")?.shift_remove(id)
        } else {
            None
        };

        let provider = match existing {
            Some(existing) => {
                let mut merged = existing;
                merged.name = display_name;
                merged.settings_config = settings_config.clone();
                merged
                    .meta
                    .get_or_insert_with(ProviderMeta::default)
                    .live_config_managed = Some(true);
                merged
            }
            None => {
                let mut created =
                    Provider::with_id(id.clone(), display_name, settings_config.clone(), None);
                created.meta = Some(ProviderMeta {
                    live_config_managed: Some(true),
                    ..Default::default()
                });
                created
            }
        };
        state.db.save_provider("pi", &provider)?;
        {
            let mut config = state.config.write().map_err(AppError::from)?;
            config.ensure_app(&AppType::Pi);
            if let Some(manager) = config.get_manager_mut(&AppType::Pi) {
                manager.providers.insert(id.clone(), provider);
            }
        }
        if replace_existing {
            summary.updated += 1;
        } else {
            summary.added += 1;
        }
    }

    if options.prune {
        summary.pruned = prune_pi_providers_missing_from_live(state, &providers, &existing_ids)?;
    }

    if !summary.is_empty() {
        state.save()?;
    }
    Ok(summary)
}

/// Deletes Pi providers that live no longer defines, but only when they are
/// flagged `live_config_managed` so locally-authored rows survive a prune.
fn prune_pi_providers_missing_from_live(
    state: &AppState,
    live_providers: &Map<String, Value>,
    existing_ids: &std::collections::HashSet<String>,
) -> Result<usize, AppError> {
    let stale = {
        let config = state.config.read().map_err(AppError::from)?;
        existing_ids
            .iter()
            .filter(|id| !live_providers.contains_key(*id))
            .filter(|id| {
                config
                    .get_manager(&AppType::Pi)
                    .and_then(|manager| manager.providers.get(*id))
                    .and_then(|provider| provider.meta.as_ref())
                    .and_then(|meta| meta.live_config_managed)
                    == Some(true)
            })
            .cloned()
            .collect::<Vec<_>>()
    };

    let mut pruned = 0usize;
    for id in stale {
        state.db.delete_provider("pi", &id)?;
        let mut config = state.config.write().map_err(AppError::from)?;
        if let Some(manager) = config.get_manager_mut(&AppType::Pi) {
            manager.providers.shift_remove(&id);
        }
        pruned += 1;
    }
    Ok(pruned)
}

pub fn import_pi_providers_from_live(state: &AppState) -> Result<usize, AppError> {
    let summary = import_pi_providers_from_live_with(state, LiveImportOptions::default())?;
    Ok(summary.added)
}

pub(crate) fn import_pi_providers_from_live_with_options(
    state: &AppState,
    options: LiveImportOptions,
) -> Result<LiveImportSummary, AppError> {
    import_pi_providers_from_live_with(state, options)
}

/// Reports what a live export changed, so callers can tell "already in sync"
/// apart from "wrote something".
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LiveExportSummary {
    /// Providers written to live config (created or overwritten).
    pub written: usize,
    /// Entries removed from live config because the database no longer has them.
    pub pruned: usize,
}

impl LiveExportSummary {
    pub fn is_empty(&self) -> bool {
        self.written == 0 && self.pruned == 0
    }
}

/// How a live export should treat entries that already exist in live config.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LiveExportOptions {
    /// Delete live entries that the database does not define. Without this flag
    /// the live file keeps entries cc-switch does not manage.
    pub prune: bool,
}

/// Push every Pi provider in the database into `models.json`.
///
/// This is the mirror of `import_pi_providers_from_live_with`: the database is
/// authoritative and the live file is rewritten to match it. Entries the
/// database does not manage are left untouched unless `prune` is requested.
pub(crate) fn export_pi_providers_to_live_with(
    state: &AppState,
    options: LiveExportOptions,
) -> Result<LiveExportSummary, AppError> {
    if !crate::sync_policy::should_sync_live(&AppType::Pi) {
        return Err(AppError::localized(
            "provider.export_live.not_initialized",
            "目标应用 pi 尚未初始化，拒绝写入其配置目录",
            "Destination app pi is not initialized; refusing to write its config directory",
        ));
    }

    // Snapshot the database under the read lock, then write files without holding it.
    let managed: Vec<(String, Value)> = {
        let config = state.config.read().map_err(AppError::from)?;
        config
            .get_manager(&AppType::Pi)
            .map(|manager| {
                manager
                    .providers
                    .values()
                    .filter(|provider| {
                        provider.meta.as_ref().and_then(|m| m.live_config_managed) != Some(false)
                    })
                    .map(|provider| (provider.id.clone(), provider.settings_config.clone()))
                    .collect()
            })
            .unwrap_or_default()
    };

    // Start from the live document so unmanaged entries survive a plain export.
    let mut providers = crate::pi_config::get_providers()?;
    let mut written = 0usize;
    for (id, settings_config) in &managed {
        if id.trim().is_empty() || !settings_config.is_object() {
            continue;
        }
        // Merge against the existing live entry so fields cc-switch does not model
        // (and that a user may have added by hand) are preserved.
        let base = providers.shift_remove(id);
        let built =
            crate::pi_config::prepare_provider_with_base(id, base, settings_config.clone())?;
        let entry = built
            .get("providers")
            .and_then(Value::as_object)
            .and_then(|map| map.get(id))
            .cloned()
            .ok_or_else(|| AppError::Config(format!("Pi provider preparation dropped '{id}'")))?;
        providers.insert(id.clone(), entry);
        written += 1;
    }

    let mut pruned = 0usize;
    if options.prune {
        let managed_ids: std::collections::HashSet<&str> =
            managed.iter().map(|(id, _)| id.as_str()).collect();
        providers.retain(|id, _| {
            if managed_ids.contains(id.as_str()) {
                true
            } else {
                pruned += 1;
                false
            }
        });
    }

    crate::pi_config::replace_providers(providers)?;

    Ok(LiveExportSummary { written, pruned })
}

/// Outcome of `provider add-model`.
#[derive(Debug, Clone, Default)]
pub struct AddModelsSummary {
    /// Models appended, with how each id was resolved.
    pub added: Vec<(String, crate::services::model_enrichment::ModelResolution)>,
    /// Requested ids that were already present in the provider.
    pub skipped_existing: Vec<String>,
}

/// Append models to a single Pi provider, changing **only** its `models[]`.
///
/// Existing ids are skipped. Catalog parameters are filled by the shared fuzzy
/// enrichment flow, so an unmatched id is only recorded as `{"id": ...}` when
/// the user is not at a TTY (scripts/CI) or explicitly declines the picker.
/// Nothing else on the provider, and no other provider, is touched.
pub(crate) fn add_models_to_pi_provider(
    state: &AppState,
    provider_id: &str,
    requested: &[String],
    fetch: bool,
) -> Result<AddModelsSummary, AppError> {
    if !crate::sync_policy::should_sync_live(&AppType::Pi) {
        return Err(AppError::localized(
            "provider.add_model.not_initialized",
            "目标应用 pi 尚未初始化，拒绝写入其配置目录",
            "Destination app pi is not initialized; refusing to write its config directory",
        ));
    }

    let mut provider = state
        .db
        .get_all_providers("pi")?
        .shift_remove(provider_id)
        .ok_or_else(|| {
            AppError::localized(
                "provider.add_model.missing_provider",
                format!("Pi 供应商不存在: {provider_id}"),
                format!("Pi provider not found: {provider_id}"),
            )
        })?;

    // Collect the ids to consider: explicit --model values first, then anything
    // discovered from the provider's own endpoint. Deduplicate in order so the
    // same id passed twice is only added once.
    let mut wanted: Vec<String> = Vec::new();
    let push_unique = |wanted: &mut Vec<String>, id: &str| {
        let id = id.trim().to_string();
        if !id.is_empty() && !wanted.contains(&id) {
            wanted.push(id);
        }
    };
    for id in requested {
        push_unique(&mut wanted, id);
    }
    if fetch {
        let discovered =
            crate::cli::commands::provider_inspect::fetch_live_model_ids(&AppType::Pi, &provider)?;
        for id in discovered {
            push_unique(&mut wanted, &id);
        }
    }
    if wanted.is_empty() {
        return Err(AppError::InvalidInput(
            "No model ids to add (empty --model list and /v1/models returned nothing)".to_string(),
        ));
    }

    let catalog = crate::services::models_dev::ModelsDevCatalog::load_or_fetch().ok();
    let interactive = crate::services::model_enrichment::can_prompt_interactively();

    let existing_models: Vec<Value> = provider
        .settings_config
        .get("models")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let existing_ids: std::collections::HashSet<String> = existing_models
        .iter()
        .filter_map(|entry| entry.get("id").and_then(Value::as_str))
        .map(str::to_string)
        .collect();

    let mut summary = AddModelsSummary::default();
    let mut merged = existing_models;

    for id in wanted {
        if existing_ids.contains(&id) {
            summary.skipped_existing.push(id);
            continue;
        }
        let (entry, resolution) = crate::services::model_enrichment::resolve_model_entry(
            &id,
            catalog.as_ref(),
            None,
            interactive,
        )?;
        // `--fetch` is a discovery convenience; when the user is not selecting a
        // subset, drop ids that carry no catalog data so the config is not
        // polluted with parameters-less placeholders.
        if resolution == crate::services::model_enrichment::ModelResolution::Unmatched && fetch {
            continue;
        }
        merged.push(entry);
        summary.added.push((id, resolution));
    }

    if !summary.added.is_empty() {
        // Replace only the provider's own `models` array.
        if let Some(obj) = provider.settings_config.as_object_mut() {
            obj.insert("models".into(), Value::Array(merged));
        }
        state.db.save_provider("pi", &provider)?;

        // Keep the live file in step so the next sync does not revert this.
        let live = crate::pi_config::prepare_provider_with_base(
            &provider.id,
            None,
            provider.settings_config.clone(),
        )?;
        crate::pi_config::write_pi_models(&live)?;

        let mut config = state.config.write().map_err(AppError::from)?;
        config.ensure_app(&AppType::Pi);
        if let Some(manager) = config.get_manager_mut(&AppType::Pi) {
            manager.providers.insert(provider.id.clone(), provider);
        }
    }

    Ok(summary)
}

pub fn import_grok_providers_from_live(state: &AppState) -> Result<usize, AppError> {
    let providers = crate::grok_config::get_providers()?;
    if providers.is_empty() {
        return Ok(0);
    }

    let mut imported = 0usize;
    let existing_ids = state.db.get_provider_ids("grok")?;
    let default_id = crate::grok_config::get_default_model()?;

    for (id, settings_config) in providers {
        if id.trim().is_empty() || existing_ids.contains(&id) || !settings_config.is_object() {
            continue;
        }
        // Skip incomplete sections that cannot be managed as custom endpoints.
        if settings_config
            .get("model")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .is_none()
            || settings_config
                .get("base_url")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .is_none()
        {
            log::debug!("Skipping Grok model '{id}' during import: missing model/base_url");
            continue;
        }

        let display_name = settings_config
            .get("name")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .unwrap_or(&id)
            .to_string();
        let mut provider = Provider::with_id(id.clone(), display_name, settings_config, None);
        provider.meta = Some(ProviderMeta {
            live_config_managed: Some(true),
            ..Default::default()
        });
        if let Err(err) = state.db.save_provider("grok", &provider) {
            log::warn!("Failed to import Grok provider '{id}': {err}");
            continue;
        }
        {
            let mut config = state.config.write().map_err(AppError::from)?;
            config.ensure_app(&AppType::Grok);
            if let Some(manager) = config.get_manager_mut(&AppType::Grok) {
                manager.providers.insert(id.clone(), provider);
                if default_id.as_deref() == Some(id.as_str()) {
                    manager.current = id;
                }
            }
        }
        imported += 1;
        log::info!("Imported Grok provider from live config");
    }

    if imported > 0 {
        state.save()?;
    }
    Ok(imported)
}

pub fn import_opencode_providers_from_live(state: &AppState) -> Result<usize, AppError> {
    let providers = crate::opencode_config::get_typed_providers()?;
    if providers.is_empty() {
        return Ok(0);
    }

    let mut imported = 0usize;
    let existing_ids = state.db.get_provider_ids("opencode")?;

    for (id, config) in providers {
        if existing_ids.contains(&id) {
            log::debug!("OpenCode provider '{id}' already exists in database, skipping");
            continue;
        }

        let settings_config = match serde_json::to_value(&config) {
            Ok(value) => value,
            Err(err) => {
                log::warn!("Failed to serialize OpenCode provider '{id}': {err}");
                continue;
            }
        };
        let mut provider = Provider::with_id(
            id.clone(),
            config.name.clone().unwrap_or_else(|| id.clone()),
            settings_config,
            None,
        );
        provider.meta = Some(ProviderMeta {
            live_config_managed: Some(true),
            ..Default::default()
        });

        if let Err(err) = state.db.save_provider("opencode", &provider) {
            log::warn!("Failed to import OpenCode provider '{id}': {err}");
            continue;
        }

        imported += 1;
        log::info!("Imported OpenCode provider '{id}' from live config");
    }

    Ok(imported)
}

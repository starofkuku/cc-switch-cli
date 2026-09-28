//! ZCode (https://github.com/zcode) live config adapter.
//!
//! ZCode stores its provider registry at `~/.zcode/v2/provider_config.json`
//! (schemaVersion 1, strict-decoded by ZCode with `zod`, polled every second)
//! and its per-agent model selection overrides at
//! `~/.zcode/v2/agents-state.json`. `ZCODE_DATA_BASE_DIR` redirects the whole
//! data root, which keeps tests hermetic.
//!
//! cc-switch only manages **third-party providers**: rules without a
//! `templateId` whose `access.type` is `api-key`. Built-in providers (BigModel
//! OAuth etc.) are passed through untouched and never surfaced.

use crate::config::write_json_file;
use crate::error::AppError;
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};

/// ZCode data root: `$ZCODE_DATA_BASE_DIR/.zcode` or `~/.zcode`.
/// (ZCode resolves `ZCODE_DATA_BASE_DIR` as the *base* dir and appends
/// `.zcode` itself — see packages/services/src/paths.ts.)
pub fn get_zcode_dir() -> PathBuf {
    let base = std::env::var_os("ZCODE_DATA_BASE_DIR")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(crate::config::home_dir)
        .unwrap_or_else(|| PathBuf::from("."));

    base.join(".zcode")
}

/// ZCode app config dir (`<root>/v2`) holding provider + agent state files.
pub fn get_zcode_v2_dir() -> PathBuf {
    get_zcode_dir().join("v2")
}

pub fn get_provider_config_path() -> PathBuf {
    get_zcode_v2_dir().join("provider_config.json")
}

pub fn get_agents_state_path() -> PathBuf {
    get_zcode_v2_dir().join("agents-state.json")
}

fn read_json_object(path: &Path, missing: Value) -> Result<Value, AppError> {
    if !path.exists() {
        return Ok(missing);
    }
    let content = std::fs::read_to_string(path).map_err(|e| AppError::io(path, e))?;
    let value: Value = serde_json::from_str(&content).map_err(|e| AppError::json(path, e))?;
    if !value.is_object() {
        return Err(AppError::Config(format!(
            "{} root must be a JSON object",
            path.display()
        )));
    }
    Ok(value)
}

/// Empty but schema-valid provider_config.json skeleton.
fn empty_provider_config() -> Value {
    json!({
        "schemaVersion": 1,
        "config": {
            "providerOrder": [],
            "providerConfigRules": { "providerRules": [] },
            "modelConfigRules": {
                "providerModelRules": [],
                "manualProviderModelRules": []
            }
        }
    })
}

pub fn read_provider_config() -> Result<Value, AppError> {
    read_json_object(&get_provider_config_path(), empty_provider_config())
}

/// Atomic write; ZCode keeps this file 0600 (it embeds api keys).
pub fn write_provider_config(config: &Value) -> Result<(), AppError> {
    let path = get_provider_config_path();
    write_json_file(&path, config)?;
    restrict_permissions(&path);
    Ok(())
}

pub(crate) fn read_agents_state() -> Result<Value, AppError> {
    read_json_object(&get_agents_state_path(), json!({}))
}

pub(crate) fn write_agents_state(state: &Value) -> Result<(), AppError> {
    write_json_file(&get_agents_state_path(), state)
}

#[cfg(unix)]
fn restrict_permissions(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(metadata) = std::fs::metadata(path) {
        let mut permissions = metadata.permissions();
        permissions.set_mode(0o600);
        let _ = std::fs::set_permissions(path, permissions);
    }
}

#[cfg(not(unix))]
fn restrict_permissions(_path: &Path) {}

fn provider_rules_mut(config: &mut Value) -> Result<&mut Vec<Value>, AppError> {
    config
        .get_mut("config")
        .and_then(|c| c.get_mut("providerConfigRules"))
        .and_then(|r| r.get_mut("providerRules"))
        .and_then(Value::as_array_mut)
        .ok_or_else(|| {
            AppError::Config(
                "ZCode provider_config.json is missing config.providerConfigRules.providerRules"
                    .to_string(),
            )
        })
}

/// All provider rules from the live file (unfiltered).
pub fn get_all_provider_rules() -> Result<Vec<Value>, AppError> {
    let config = read_provider_config()?;
    Ok(config
        .get("config")
        .and_then(|c| c.get("providerConfigRules"))
        .and_then(|r| r.get("providerRules"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default())
}

/// A rule is third-party when it has no `templateId` and uses a plain api key.
/// Built-in template providers (BigModel OAuth etc.) stay invisible to
/// cc-switch but are preserved verbatim on disk.
pub fn is_third_party_rule(rule: &Value) -> bool {
    let has_template = rule
        .get("templateId")
        .map(|v| !v.is_null() && v.as_str().is_some_and(|s| !s.trim().is_empty()))
        .unwrap_or(false);
    if has_template {
        return false;
    }
    rule.get("config")
        .and_then(|c| c.get("access"))
        .and_then(|a| a.get("type"))
        .and_then(Value::as_str)
        .map(|t| t == "api-key")
        .unwrap_or(false)
}

/// Third-party rules only — the set cc-switch lists, switches and edits.
pub fn get_third_party_rules() -> Result<Vec<Value>, AppError> {
    Ok(get_all_provider_rules()?
        .into_iter()
        .filter(is_third_party_rule)
        .collect())
}

pub fn rule_id(rule: &Value) -> Option<&str> {
    rule.get("providerId").and_then(Value::as_str)
}

/// Preferred model for a rule: `modelOrder[0]`, else `personalModelIds[0]`.
pub fn preferred_model_id(rule: &Value) -> Option<String> {
    let config = rule.get("config")?;
    for key in ["modelOrder", "personalModelIds"] {
        if let Some(model) = config
            .get(key)
            .and_then(Value::as_array)
            .and_then(|arr| arr.first())
            .and_then(Value::as_str)
        {
            return Some(model.to_string());
        }
    }
    None
}

/// ZCode gates personal models through `modelConfigRules.providerModelRules`;
/// a model without an explicit rule may stay disabled in the picker. Enable
/// every model the rule advertises so a freshly added provider is usable.
fn ensure_model_rules_enabled(config: &mut Value, provider_id: &str) {
    let Some(models) = config
        .get("config")
        .and_then(|c| c.get("providerConfigRules"))
        .and_then(|r| r.get("providerRules"))
        .and_then(Value::as_array)
        .and_then(|rules| {
            rules
                .iter()
                .find(|r| rule_id(r) == Some(provider_id))
                .cloned()
        })
        .and_then(|rule| {
            let config = rule.get("config")?;
            let order = config
                .get("modelOrder")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let personal = config
                .get("personalModelIds")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            Some([order, personal].concat())
        })
    else {
        return;
    };

    let Some(model_rules) = config
        .get_mut("config")
        .and_then(|c| c.get_mut("modelConfigRules"))
        .and_then(|r| r.get_mut("providerModelRules"))
        .and_then(Value::as_array_mut)
    else {
        return;
    };

    for model in models {
        let Some(model_id) = model.as_str() else {
            continue;
        };
        let exists = model_rules.iter().any(|r| {
            r.get("providerId").and_then(Value::as_str) == Some(provider_id)
                && r.get("modelId").and_then(Value::as_str) == Some(model_id)
        });
        if !exists {
            model_rules.push(json!({
                "modelId": model_id,
                "config": { "enabled": true },
                "providerId": provider_id,
            }));
        }
    }
}

/// Remove a third-party provider rule. Selection pointers (default model
/// selection / agent overrides) that referenced it are dropped as well;
/// built-in providers are never touched.
pub fn remove_provider_rule(id: &str) -> Result<(), AppError> {
    let mut config = read_provider_config()?;
    let rules = provider_rules_mut(&mut config)?;
    let target = rules
        .iter()
        .position(|r| rule_id(r) == Some(id) && is_third_party_rule(r));
    let Some(index) = target else {
        return Ok(());
    };
    rules.remove(index);

    if let Some(order) = config
        .get_mut("config")
        .and_then(|c| c.get_mut("providerOrder"))
        .and_then(Value::as_array_mut)
    {
        order.retain(|v| v.as_str() != Some(id));
    }
    if let Some(model_rules) = config
        .get_mut("config")
        .and_then(|c| c.get_mut("modelConfigRules"))
        .and_then(|r| r.get_mut("providerModelRules"))
        .and_then(Value::as_array_mut)
    {
        model_rules.retain(|r| r.get("providerId").and_then(Value::as_str) != Some(id));
    }
    drop_selection_pointing_to(&mut config, id);
    write_provider_config(&config)?;

    let mut state = read_agents_state()?;
    remove_agent_overrides_for(&mut state, id);
    write_agents_state(&state)
}

fn drop_selection_pointing_to(config: &mut Value, id: &str) {
    let Some(selection) = config
        .get_mut("config")
        .and_then(|c| c.get_mut("defaultModelSelection"))
    else {
        return;
    };
    if selection.get("providerId").and_then(Value::as_str) == Some(id) {
        if let Some(config_obj) = config.get_mut("config").and_then(Value::as_object_mut) {
            config_obj.remove("defaultModelSelection");
        }
    }
}

fn remove_agent_overrides_for(state: &mut Value, id: &str) {
    let Some(overrides) = state
        .get_mut("builtInModelSelectionOverrides")
        .and_then(Value::as_object_mut)
    else {
        return;
    };
    let stale: Vec<String> = overrides
        .iter()
        .filter(|(_, selection)| selection.get("providerId").and_then(Value::as_str) == Some(id))
        .map(|(agent, _)| agent.clone())
        .collect();
    for agent in stale {
        overrides.shift_remove(&agent);
    }
}

/// Re-point every agent override that currently selects a third-party
/// provider (including the one being switched away from).
fn retarget_agent_overrides(
    state: &mut Value,
    id: &str,
    model_id: &str,
    rules: &[Value],
) -> Result<(), AppError> {
    let Some(overrides) = state
        .get_mut("builtInModelSelectionOverrides")
        .and_then(Value::as_object_mut)
    else {
        return Ok(());
    };

    let third_party_ids: Vec<&str> = rules
        .iter()
        .filter(|r| is_third_party_rule(r))
        .filter_map(|r| rule_id(r))
        .collect();

    for (_, selection) in overrides.iter_mut() {
        let current = selection.get("providerId").and_then(Value::as_str);
        if current.is_some_and(|current| third_party_ids.contains(&current)) {
            if let Some(obj) = selection.as_object_mut() {
                obj.insert("providerId".to_string(), Value::String(id.to_string()));
                obj.insert("modelId".to_string(), Value::String(model_id.to_string()));
            }
        }
    }
    Ok(())
}

/// The provider ZCode would currently use: `defaultModelSelection` first,
/// then the `general-purpose` agent override.
pub fn current_selection() -> Result<Option<(String, String)>, AppError> {
    let config = read_provider_config()?;
    if let Some(selection) = config
        .get("config")
        .and_then(|c| c.get("defaultModelSelection"))
    {
        if let (Some(provider), Some(model)) = (
            selection.get("providerId").and_then(Value::as_str),
            selection.get("modelId").and_then(Value::as_str),
        ) {
            return Ok(Some((provider.to_string(), model.to_string())));
        }
    }

    let state = read_agents_state()?;
    if let Some(selection) = state
        .get("builtInModelSelectionOverrides")
        .and_then(|o| o.get("general-purpose"))
    {
        if let (Some(provider), Some(model)) = (
            selection.get("providerId").and_then(Value::as_str),
            selection.get("modelId").and_then(Value::as_str),
        ) {
            return Ok(Some((provider.to_string(), model.to_string())));
        }
    }
    Ok(None)
}

/// Validate provider settings without touching the live files.
///
/// Mirrors the checks [`prepare_write`] performs, for `provider add` input
/// validation. Accepts either a full rule or a bare rule `config` object.
pub fn validate_settings(id: &str, name: &str, settings: &Value) -> Result<(), AppError> {
    prepare_write(id, name, settings).map(|_| ())
}

/// A prepared two-file live write (provider registry + agent state).
///
/// Mirrors `grok_config::prepare_switch_document`: everything is computed and
/// validated up front; `write_prepared` only persists. Both add and switch go
/// through this path — like Grok, adding a provider also points the default
/// selection at it.
#[derive(Clone, Debug)]
pub struct ZcodePreparedWrite {
    pub provider_config: Value,
    pub agents_state: Value,
}

/// Validate and stage a live write for provider `id` / `name`.
///
/// `settings` may be either a full provider rule (object carrying `config`)
/// — the shape `import` stores — or a bare rule `config` object; a bare
/// config is wrapped with `providerId` / `providerName`.
pub fn prepare_write(
    id: &str,
    name: &str,
    settings: &Value,
) -> Result<ZcodePreparedWrite, AppError> {
    let rule = normalize_rule(id, name, settings)?;
    let config = rule.get("config").cloned().ok_or_else(|| {
        AppError::localized(
            "provider.zcode.settings.missing_config",
            "ZCode 供应商配置缺少 config 字段",
            "ZCode provider settings are missing the config field",
        )
    })?;
    let api_key = config
        .get("access")
        .and_then(|a| a.get("apiKey"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty());
    if api_key.is_none() {
        return Err(AppError::localized(
            "provider.zcode.settings.api_key_required",
            "ZCode 供应商必须设置 config.access.apiKey 字段",
            "ZCode provider requires the config.access.apiKey field",
        ));
    }
    let base_url = config
        .get("api")
        .and_then(|a| a.get("baseUrl"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty());
    if base_url.is_none() {
        return Err(AppError::localized(
            "provider.zcode.settings.base_url_required",
            "ZCode 供应商必须设置 config.api.baseUrl 字段",
            "ZCode provider requires the config.api.baseUrl field",
        ));
    }

    let mut provider_config = read_provider_config()?;
    upsert_rule_in_document(&mut provider_config, rule)?;
    ensure_model_rules_enabled(&mut provider_config, id);

    let model_id = provider_config
        .get("config")
        .and_then(|c| c.get("providerConfigRules"))
        .and_then(|r| r.get("providerRules"))
        .and_then(Value::as_array)
        .and_then(|rules| {
            rules
                .iter()
                .find(|r| rule_id(r) == Some(id))
                .and_then(preferred_model_id)
        })
        .ok_or_else(|| {
            AppError::localized(
                "provider.zcode.settings.model_required",
                "ZCode 供应商至少需要一个模型（modelOrder 或 personalModelIds）",
                "ZCode provider needs at least one model (modelOrder or personalModelIds)",
            )
        })?;

    let existing_options = provider_config
        .get("config")
        .and_then(|c| c.get("defaultModelSelection"))
        .and_then(|s| s.get("options"))
        .cloned();
    let mut selection = json!({ "providerId": id, "modelId": model_id });
    if let Some(options) = existing_options {
        selection["options"] = options;
    }
    if let Some(config_obj) = provider_config
        .get_mut("config")
        .and_then(Value::as_object_mut)
    {
        config_obj.insert("defaultModelSelection".to_string(), selection);
    }

    let all_rules = provider_config
        .get("config")
        .and_then(|c| c.get("providerConfigRules"))
        .and_then(|r| r.get("providerRules"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut agents_state = read_agents_state()?;
    retarget_agent_overrides(&mut agents_state, id, &model_id, &all_rules)?;

    Ok(ZcodePreparedWrite {
        provider_config,
        agents_state,
    })
}

/// Persist a prepared write (atomic per file).
pub fn write_prepared(prepared: &ZcodePreparedWrite) -> Result<(), AppError> {
    write_provider_config(&prepared.provider_config)?;
    write_agents_state(&prepared.agents_state)
}

/// Accept either a full rule or a bare rule `config`; always return a full
/// rule with `providerId` forced to `id` and a `providerName`.
fn normalize_rule(id: &str, name: &str, settings: &Value) -> Result<Value, AppError> {
    // The shared TUI add/edit form produces an OpenClaw-shaped document
    // ({apiKey, baseUrl, models} at the top level); translate it into a bare
    // ZCode rule config so the same form works for ZCode providers.
    let settings = if settings.get("config").is_none()
        && settings.get("baseUrl").is_some()
        && settings.get("apiKey").is_some()
    {
        translate_openclaw_style_settings(settings)
    } else {
        settings.clone()
    };
    let mut rule = if settings.get("config").is_some() {
        settings
    } else {
        json!({ "config": settings })
    };
    if !rule.is_object() {
        return Err(AppError::localized(
            "provider.zcode.settings.not_object",
            "ZCode 供应商配置必须是 JSON 对象",
            "ZCode provider settings must be a JSON object",
        ));
    }
    let provider_name = rule
        .get("providerName")
        .and_then(Value::as_str)
        .unwrap_or(name)
        .to_string();
    let obj = rule.as_object_mut().expect("checked above");
    obj.insert("providerId".to_string(), Value::String(id.to_string()));
    obj.insert("providerName".to_string(), Value::String(provider_name));
    Ok(rule)
}

/// `{apiKey, baseUrl, models}` (shared TUI form shape) -> bare ZCode rule config.
fn translate_openclaw_style_settings(settings: &Value) -> Value {
    let models: Vec<String> = settings
        .get("models")
        .and_then(Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| {
                    entry
                        .get("id")
                        .and_then(Value::as_str)
                        .or_else(|| entry.as_str())
                        .map(str::to_string)
                })
                .collect()
        })
        .unwrap_or_default();
    json!({
        "group": "standard-personal",
        "access": {
            "type": "api-key",
            "apiKey": settings.get("apiKey").cloned().unwrap_or(Value::Null),
        },
        "api": {
            "type": "openai-chat-completions",
            "baseUrl": settings.get("baseUrl").cloned().unwrap_or(Value::Null),
        },
        "personalModelIds": models,
        "modelOrder": models,
    })
}

/// In-place variant of [`upsert_provider_rule`] used by `prepare_write`.
fn upsert_rule_in_document(config: &mut Value, rule: Value) -> Result<(), AppError> {
    let id = rule_id(&rule).map(str::to_string).ok_or_else(|| {
        AppError::localized(
            "provider.zcode.rule_missing_id",
            "ZCode 供应商规则缺少 providerId",
            "ZCode provider rule is missing providerId",
        )
    })?;
    let rules = provider_rules_mut(config)?;
    match rules.iter().position(|r| rule_id(r) == Some(id.as_str())) {
        Some(index) => rules[index] = rule,
        None => rules.push(rule),
    }
    if let Some(order) = config
        .get_mut("config")
        .and_then(|c| c.get_mut("providerOrder"))
        .and_then(Value::as_array_mut)
    {
        if !order.iter().any(|v| v.as_str() == Some(id.as_str())) {
            order.push(Value::String(id));
        }
    }
    Ok(())
}

/// Map form: id -> rule for third-party providers.
pub fn third_party_rule_map() -> Result<Map<String, Value>, AppError> {
    let mut map = Map::new();
    for rule in get_third_party_rules()? {
        if let Some(id) = rule_id(&rule) {
            map.insert(id.to_string(), rule);
        }
    }
    Ok(map)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestEnvGuard;

    fn seed_provider_config() {
        write_provider_config(&json!({
            "schemaVersion": 1,
            "config": {
                "providerOrder": ["bigmodel-api", "new-provider"],
                "providerConfigRules": { "providerRules": [
                    {
                        "providerId": "bigmodel-api",
                        "templateId": "bigmodel-api",
                        "providerName": "BigModel Coding Plan",
                        "config": {
                            "group": "standard-personal",
                            "access": { "type": "zhipu-coding-plan-api-key", "apiKey": "bm-key" }
                        }
                    },
                    {
                        "providerId": "new-provider",
                        "providerName": "本地",
                        "config": {
                            "group": "standard-personal",
                            "access": { "type": "api-key", "apiKey": "local-key" },
                            "api": { "type": "openai-chat-completions", "baseUrl": "http://127.0.0.1:3001/v1" },
                            "personalModelIds": ["deepseek-flash-cline"],
                            "modelOrder": ["deepseek-flash-cline"]
                        }
                    }
                ]},
                "modelConfigRules": {
                    "providerModelRules": [
                        { "modelId": "deepseek-flash-cline", "config": { "enabled": true }, "providerId": "new-provider" }
                    ],
                    "manualProviderModelRules": []
                },
                "unknownFutureField": { "keep": true }
            }
        }))
        .expect("seed provider_config");
    }

    fn seed_agents_state() {
        write_agents_state(&json!({
            "builtInModelSelectionOverrides": {
                "general-purpose": {
                    "providerId": "new-provider",
                    "modelId": "deepseek-flash-cline",
                    "options": { "reasoningLevel": "max" }
                },
                "Explore": {
                    "providerId": "new-provider",
                    "modelId": "deepseek-flash-cline",
                    "options": { "reasoningLevel": "max" }
                }
            },
            "disabledAgentIds": []
        }))
        .expect("seed agents-state");
    }

    #[test]
    fn zcode_third_party_filter_excludes_template_providers() {
        let temp = tempfile::tempdir().expect("tempdir");
        let _env = TestEnvGuard::isolated(temp.path());
        seed_provider_config();

        let rules = get_third_party_rules().expect("filter rules");
        assert_eq!(rules.len(), 1);
        assert_eq!(rule_id(&rules[0]), Some("new-provider"));
    }

    #[test]
    fn zcode_upsert_preserves_unknown_fields_and_enables_models() {
        let temp = tempfile::tempdir().expect("tempdir");
        let _env = TestEnvGuard::isolated(temp.path());
        seed_provider_config();

        let prepared = prepare_write(
            "relay",
            "中转",
            &json!({
                "access": { "type": "api-key", "apiKey": "sk-relay" },
                "api": { "type": "openai-chat-completions", "baseUrl": "https://relay.example/v1" },
                "personalModelIds": ["gpt-x"],
                "modelOrder": ["gpt-x"],
            }),
        )
        .expect("prepare");
        write_prepared(&prepared).expect("write");

        let live = read_provider_config().expect("read");
        assert_eq!(
            live["config"]["unknownFutureField"]["keep"], true,
            "unknown top-level config fields must survive"
        );
        assert_eq!(
            live["config"]["providerOrder"],
            json!(["bigmodel-api", "new-provider", "relay"])
        );

        let enabled: Vec<&str> = live["config"]["modelConfigRules"]["providerModelRules"]
            .as_array()
            .expect("model rules")
            .iter()
            .filter(|r| r["providerId"] == "relay")
            .map(|r| r["modelId"].as_str().expect("model id"))
            .collect();
        assert_eq!(enabled, vec!["gpt-x"]);
    }

    #[test]
    fn zcode_switch_updates_selection_and_third_party_overrides_only() {
        let temp = tempfile::tempdir().expect("tempdir");
        let _env = TestEnvGuard::isolated(temp.path());
        seed_provider_config();
        seed_agents_state();

        let prepared = prepare_write(
            "relay",
            "中转",
            &json!({
                "access": { "type": "api-key", "apiKey": "sk-relay" },
                "api": { "type": "openai-chat-completions", "baseUrl": "https://relay.example/v1" },
                "personalModelIds": ["gpt-x"],
                "modelOrder": ["gpt-x"],
            }),
        )
        .expect("prepare");
        write_prepared(&prepared).expect("write");

        let selection = current_selection().expect("selection");
        assert_eq!(selection, Some(("relay".to_string(), "gpt-x".to_string())));

        let live = read_provider_config().expect("read");
        assert_eq!(
            live["config"]["defaultModelSelection"]["providerId"],
            "relay"
        );

        let state = read_agents_state().expect("state");
        assert_eq!(
            state["builtInModelSelectionOverrides"]["general-purpose"]["providerId"],
            "relay"
        );
        assert_eq!(
            state["builtInModelSelectionOverrides"]["general-purpose"]["options"]["reasoningLevel"],
            "max",
            "existing override options must be preserved"
        );
    }

    #[test]
    fn zcode_remove_cleans_up_selection_pointers() {
        let temp = tempfile::tempdir().expect("tempdir");
        let _env = TestEnvGuard::isolated(temp.path());
        seed_provider_config();
        seed_agents_state();
        let prepared = prepare_write(
            "new-provider",
            "本地",
            &read_provider_config().expect("read")["config"]["providerConfigRules"]
                ["providerRules"][1]
                .get("config")
                .cloned()
                .expect("rule config"),
        )
        .expect("prepare");
        write_prepared(&prepared).expect("switch write");

        remove_provider_rule("new-provider").expect("remove");

        let live = read_provider_config().expect("read");
        let order: Vec<&str> = live["config"]["providerOrder"]
            .as_array()
            .expect("order")
            .iter()
            .map(|v| v.as_str().expect("id"))
            .collect();
        assert_eq!(order, vec!["bigmodel-api"]);
        assert!(live["config"]["defaultModelSelection"].is_null());

        let state = read_agents_state().expect("state");
        assert!(state["builtInModelSelectionOverrides"]
            .get("general-purpose")
            .is_none());
    }

    #[test]
    fn zcode_remove_never_touches_builtin_providers() {
        let temp = tempfile::tempdir().expect("tempdir");
        let _env = TestEnvGuard::isolated(temp.path());
        seed_provider_config();

        remove_provider_rule("bigmodel-api").expect("remove");

        let live = read_provider_config().expect("read");
        let ids: Vec<&str> = live["config"]["providerConfigRules"]["providerRules"]
            .as_array()
            .expect("rules")
            .iter()
            .map(|r| r["providerId"].as_str().expect("id"))
            .collect();
        assert_eq!(ids, vec!["bigmodel-api", "new-provider"]);
    }

    #[test]
    fn zcode_current_selection_falls_back_to_general_purpose() {
        let temp = tempfile::tempdir().expect("tempdir");
        let _env = TestEnvGuard::isolated(temp.path());
        seed_provider_config();
        seed_agents_state();

        assert_eq!(
            current_selection().expect("selection"),
            Some((
                "new-provider".to_string(),
                "deepseek-flash-cline".to_string()
            ))
        );
    }
}

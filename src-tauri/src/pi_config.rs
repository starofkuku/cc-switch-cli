use crate::config::write_json_file;
use crate::error::AppError;
use crate::services::provider::live_merge;
use serde_json::{json, Map, Value};
use std::path::PathBuf;

/// Return Pi's global agent configuration directory.
///
/// Pi itself supports `PI_CODING_AGENT_DIR`; keeping the same override makes
/// cc-switch safe to use in sandboxes and with non-default Pi installations.
pub fn get_pi_dir() -> PathBuf {
    if let Some(path) = std::env::var_os("PI_CODING_AGENT_DIR").filter(|value| !value.is_empty()) {
        return PathBuf::from(path);
    }

    crate::config::home_dir()
        .map(|home| home.join(".pi").join("agent"))
        .unwrap_or_else(|| PathBuf::from(".pi").join("agent"))
}

pub fn get_pi_models_path() -> PathBuf {
    get_pi_dir().join("models.json")
}

pub fn get_pi_settings_path() -> PathBuf {
    get_pi_dir().join("settings.json")
}

pub fn read_pi_models() -> Result<Value, AppError> {
    let path = get_pi_models_path();
    if !path.exists() {
        return Ok(json!({ "providers": {} }));
    }

    let content = std::fs::read_to_string(&path).map_err(|e| AppError::io(&path, e))?;
    let value: Value = serde_json::from_str(&content).map_err(|e| AppError::json(&path, e))?;
    if !value.is_object() {
        return Err(AppError::Config(
            "Pi models.json root must be a JSON object".to_string(),
        ));
    }
    Ok(value)
}

pub fn write_pi_models(models: &Value) -> Result<(), AppError> {
    write_json_file(&get_pi_models_path(), models)
}

pub fn get_providers() -> Result<Map<String, Value>, AppError> {
    Ok(read_pi_models()?
        .get("providers")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default())
}

pub fn prepare_provider_with_base(
    id: &str,
    base_provider: Option<Value>,
    provider: Value,
) -> Result<Value, AppError> {
    if !provider.is_object() {
        return Err(AppError::localized(
            "provider.pi.settings.not_object",
            "Pi 供应商配置必须是 JSON 对象",
            "Pi provider configuration must be a JSON object",
        ));
    }

    let mut models = read_pi_models()?;
    if models.get("providers").is_none() {
        models["providers"] = json!({});
    }
    let providers = models
        .get_mut("providers")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| AppError::Config("Pi models.json providers must be an object".into()))?;

    let merged = match providers.get(id) {
        Some(existing) => match base_provider {
            Some(base) => live_merge::merge_json_with_base_live(
                &crate::app_config::AppType::Pi,
                format!("models.json providers.{id}"),
                existing.clone(),
                &base,
                &provider,
            )?,
            None => live_merge::merge_json_live(
                &crate::app_config::AppType::Pi,
                format!("models.json providers.{id}"),
                existing.clone(),
                &provider,
            )?,
        },
        None => provider,
    };
    providers.insert(id.to_string(), merged);
    Ok(models)
}

pub fn remove_provider(id: &str) -> Result<(), AppError> {
    let mut models = read_pi_models()?;
    if let Some(providers) = models.get_mut("providers").and_then(Value::as_object_mut) {
        providers.remove(id);
    }
    write_pi_models(&models)
}

pub fn read_pi_settings() -> Result<Value, AppError> {
    let path = get_pi_settings_path();
    if !path.exists() {
        return Ok(json!({}));
    }
    let content = std::fs::read_to_string(&path).map_err(|e| AppError::io(&path, e))?;
    let value: Value = serde_json::from_str(&content).map_err(|e| AppError::json(&path, e))?;
    if !value.is_object() {
        return Err(AppError::Config(
            "Pi settings.json root must be a JSON object".to_string(),
        ));
    }
    Ok(value)
}

pub fn set_default_model(provider_id: &str, model_id: &str) -> Result<String, AppError> {
    let providers = get_providers()?;
    let provider = providers.get(provider_id).ok_or_else(|| {
        AppError::localized(
            "provider.set_default_model.pi_provider_missing",
            format!("请先将该 Pi 供应商加入当前配置: {provider_id}"),
            format!("Add this Pi provider to models.json first: {provider_id}"),
        )
    })?;
    if let Some(models) = provider.get("models").and_then(Value::as_array) {
        let model_exists = models
            .iter()
            .any(|model| model.get("id").and_then(Value::as_str) == Some(model_id));
        if !model_exists {
            return Err(AppError::localized(
                "provider.set_default_model.pi_model_missing",
                format!("Pi 供应商 {provider_id} 中不存在模型: {model_id}"),
                format!("Pi provider {provider_id} does not define model: {model_id}"),
            ));
        }
    }

    let mut settings = read_pi_settings()?;
    let object = settings
        .as_object_mut()
        .expect("Pi settings normalized to an object");
    object.insert(
        "defaultProvider".to_string(),
        Value::String(provider_id.to_string()),
    );
    object.insert(
        "defaultModel".to_string(),
        Value::String(model_id.to_string()),
    );
    write_json_file(&get_pi_settings_path(), &settings)?;
    Ok(format!("{provider_id}/{model_id}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestEnvGuard;

    #[test]
    fn pi_models_preserve_unmanaged_top_level_fields() {
        let temp = tempfile::tempdir().expect("tempdir");
        let _env = TestEnvGuard::isolated(temp.path());
        write_pi_models(&json!({
            "custom": true,
            "providers": { "old": { "baseUrl": "https://old.example" } }
        }))
        .expect("seed models");

        let prepared = prepare_provider_with_base(
            "new",
            None,
            json!({
                "baseUrl": "https://new.example/v1",
                "api": "openai-completions",
                "models": [{ "id": "demo" }]
            }),
        )
        .expect("prepare provider");
        write_pi_models(&prepared).expect("write models");

        let live = read_pi_models().expect("read models");
        assert_eq!(live["custom"], true);
        assert!(live["providers"]["old"].is_object());
        assert_eq!(live["providers"]["new"]["models"][0]["id"], "demo");
    }

    #[test]
    fn pi_default_model_updates_settings_without_dropping_other_settings() {
        let temp = tempfile::tempdir().expect("tempdir");
        let _env = TestEnvGuard::isolated(temp.path());
        write_pi_models(&json!({
            "providers": {
                "demo": {
                    "baseUrl": "https://example.com/v1",
                    "api": "openai-completions",
                    "models": [{ "id": "model-a" }]
                }
            }
        }))
        .expect("seed models");
        write_json_file(&get_pi_settings_path(), &json!({ "theme": "light" }))
            .expect("seed settings");

        assert_eq!(
            set_default_model("demo", "model-a").expect("set default"),
            "demo/model-a"
        );
        let settings = read_pi_settings().expect("read settings");
        assert_eq!(settings["theme"], "light");
        assert_eq!(settings["defaultProvider"], "demo");
        assert_eq!(settings["defaultModel"], "model-a");
    }
}

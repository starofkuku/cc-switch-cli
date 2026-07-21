//! Grok Build live configuration adapter.
//!
//! Reads/writes `$GROK_HOME/config.toml` (default `~/.grok/config.toml`) using
//! `toml_edit` so unrelated sections and comments are preserved.
//!
//! A CC-Switch provider maps to one `[model.<id>]` nested table. Switching sets
//! `[models].default = <id>` and upserts that model section.

use crate::config::{atomic_write, home_dir};
use crate::error::AppError;
use serde_json::{json, Map, Value};
use std::fs;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use toml_edit::{Array, DocumentMut, InlineTable, Item, Table, Value as TomlValue};

/// Managed keys written into `[model.<id>]` from provider settings_config.
const MANAGED_MODEL_KEYS: &[&str] = &[
    "model",
    "base_url",
    "name",
    "description",
    "api_key",
    "env_key",
    "api_backend",
    "temperature",
    "top_p",
    "max_completion_tokens",
    "context_window",
    "extra_headers",
    "supports_reasoning_effort",
    "reasoning_efforts",
];

const VALID_API_BACKENDS: &[&str] = &["chat_completions", "responses", "messages"];

fn write_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

/// Grok config directory: `$GROK_HOME` or `~/.grok`.
pub fn get_grok_dir() -> PathBuf {
    if let Some(path) = std::env::var_os("GROK_HOME").filter(|value| !value.is_empty()) {
        return PathBuf::from(path);
    }

    home_dir()
        .map(|home| home.join(".grok"))
        .unwrap_or_else(|| PathBuf::from(".grok"))
}

pub fn get_grok_config_path() -> PathBuf {
    get_grok_dir().join("config.toml")
}

pub fn read_grok_config_source() -> Result<Option<String>, AppError> {
    let path = get_grok_config_path();
    if !path.exists() {
        return Ok(None);
    }
    fs::read_to_string(&path)
        .map(Some)
        .map_err(|e| AppError::io(&path, e))
}

pub fn write_grok_config_source(source: &str) -> Result<(), AppError> {
    let path = get_grok_config_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| AppError::io(parent, e))?;
    }
    atomic_write(&path, source.as_bytes())
}

fn read_document() -> Result<DocumentMut, AppError> {
    let path = get_grok_config_path();
    if !path.exists() {
        return Ok(DocumentMut::default());
    }
    let content = fs::read_to_string(&path).map_err(|e| AppError::io(&path, e))?;
    if content.trim().is_empty() {
        return Ok(DocumentMut::default());
    }
    content
        .parse::<DocumentMut>()
        .map_err(|e| AppError::Config(format!("Failed to parse Grok config.toml as TOML: {e}")))
}

fn write_document(doc: &DocumentMut) -> Result<(), AppError> {
    let _guard = write_lock()
        .lock()
        .map_err(|e| AppError::Config(format!("Failed to acquire Grok write lock: {e}")))?;
    write_grok_config_source(&doc.to_string())
}

/// List `[model.<id>]` sections as JSON objects keyed by section id.
pub fn get_providers() -> Result<Map<String, Value>, AppError> {
    let doc = read_document()?;
    let mut out = Map::new();

    let Some(model_root) = doc.get("model").and_then(Item::as_table) else {
        return Ok(out);
    };

    for (id, item) in model_root.iter() {
        if id.trim().is_empty() {
            continue;
        }
        let Some(table) = item.as_table() else {
            continue;
        };
        out.insert(id.to_string(), model_table_to_json(table));
    }

    Ok(out)
}

pub fn get_default_model() -> Result<Option<String>, AppError> {
    let doc = read_document()?;
    Ok(doc
        .get("models")
        .and_then(Item::as_table)
        .and_then(|models| models.get("default"))
        .and_then(Item::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string))
}

pub fn set_default_model(id: &str) -> Result<(), AppError> {
    let mut doc = read_document()?;
    ensure_models_table(&mut doc);
    doc["models"]["default"] = toml_edit::value(id);
    write_document(&doc)
}

fn model_settings_object(settings: &Value) -> Result<&Map<String, Value>, AppError> {
    settings.as_object().ok_or_else(|| {
        AppError::localized(
            "provider.grok.settings.not_object",
            "Grok 供应商配置必须是 JSON 对象",
            "Grok provider configuration must be a JSON object",
        )
    })
}

fn apply_settings_to_model_table(table: &mut Table, obj: &Map<String, Value>) {
    for key in MANAGED_MODEL_KEYS {
        table.remove(*key);
    }
    for (key, value) in obj {
        if !MANAGED_MODEL_KEYS.contains(&key.as_str()) || value.is_null() {
            continue;
        }
        if let Some(item) = json_to_toml_item(value, key) {
            table.insert(key, item);
        }
    }
}

fn upsert_model_in_document(
    doc: &mut DocumentMut,
    id: &str,
    obj: &Map<String, Value>,
) -> Result<(), AppError> {
    ensure_model_root(doc);
    let model_root = doc
        .get_mut("model")
        .and_then(Item::as_table_mut)
        .ok_or_else(|| AppError::Config("Grok config 'model' must be a table".into()))?;

    let mut table = model_root
        .get(id)
        .and_then(Item::as_table)
        .cloned()
        .unwrap_or_default();
    apply_settings_to_model_table(&mut table, obj);
    model_root.insert(id, Item::Table(table));
    Ok(())
}

/// Upsert `[model.<id>]` from a JSON settings object. Unknown keys in the
/// existing section are preserved; managed keys are rewritten from settings.
pub fn upsert_model(id: &str, settings: &Value) -> Result<(), AppError> {
    validate_settings(settings)?;
    let obj = model_settings_object(settings)?;
    let mut doc = read_document()?;
    upsert_model_in_document(&mut doc, id, obj)?;
    write_document(&doc)
}

pub fn remove_model(id: &str) -> Result<(), AppError> {
    let mut doc = read_document()?;
    let removed = doc
        .get_mut("model")
        .and_then(Item::as_table_mut)
        .map(|root| root.remove(id).is_some())
        .unwrap_or(false);
    if removed {
        // Drop empty `model` table to avoid leaving a bare [model] section.
        if doc
            .get("model")
            .and_then(Item::as_table)
            .is_some_and(|t| t.is_empty())
        {
            doc.as_table_mut().remove("model");
        }
        write_document(&doc)?;
    }
    Ok(())
}

/// Switch: upsert model section and set it as the default model.
pub fn apply_provider_switch(id: &str, settings: &Value) -> Result<(), AppError> {
    validate_settings(settings)?;
    let obj = model_settings_object(settings)?;
    let mut doc = read_document()?;
    upsert_model_in_document(&mut doc, id, obj)?;
    ensure_models_table(&mut doc);
    doc["models"]["default"] = toml_edit::value(id);
    write_document(&doc)
}

/// Prepare a full document string after applying switch (for PreparedLiveWrite).
pub fn prepare_switch_document(id: &str, settings: &Value) -> Result<String, AppError> {
    validate_settings(settings)?;
    let obj = model_settings_object(settings)?;
    let mut doc = read_document()?;
    upsert_model_in_document(&mut doc, id, obj)?;
    ensure_models_table(&mut doc);
    doc["models"]["default"] = toml_edit::value(id);
    Ok(doc.to_string())
}

pub fn write_prepared_document(source: &str) -> Result<(), AppError> {
    let _guard = write_lock()
        .lock()
        .map_err(|e| AppError::Config(format!("Failed to acquire Grok write lock: {e}")))?;
    write_grok_config_source(source)
}

pub fn validate_settings(settings: &Value) -> Result<(), AppError> {
    let obj = model_settings_object(settings)?;

    let model = obj
        .get("model")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty());
    if model.is_none() {
        return Err(AppError::localized(
            "provider.grok.settings.model_required",
            "Grok 供应商必须设置 model 字段",
            "Grok provider requires a model field",
        ));
    }

    let base_url = obj
        .get("base_url")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty());
    if base_url.is_none() {
        return Err(AppError::localized(
            "provider.grok.settings.base_url_required",
            "Grok 自定义供应商必须设置 base_url",
            "Grok custom providers require a base_url",
        ));
    }

    if let Some(backend) = obj
        .get("api_backend")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        if !VALID_API_BACKENDS.contains(&backend) {
            return Err(AppError::localized(
                "provider.grok.settings.api_backend_invalid",
                format!(
                    "Grok api_backend 无效: '{backend}'。可选: chat_completions, responses, messages"
                ),
                format!(
                    "Invalid Grok api_backend: '{backend}'. Allowed: chat_completions, responses, messages"
                ),
            ));
        }
    }

    Ok(())
}

fn ensure_models_table(doc: &mut DocumentMut) {
    if !doc
        .as_table()
        .get("models")
        .map(Item::is_table)
        .unwrap_or(false)
    {
        doc["models"] = Item::Table(Table::new());
    }
}

fn ensure_model_root(doc: &mut DocumentMut) {
    if !doc
        .as_table()
        .get("model")
        .map(Item::is_table)
        .unwrap_or(false)
    {
        let mut table = Table::new();
        // Keep dotted [model.id] form rather than nested [[model]] style.
        table.set_implicit(true);
        doc["model"] = Item::Table(table);
    } else if let Some(table) = doc.get_mut("model").and_then(Item::as_table_mut) {
        table.set_implicit(true);
    }
}

fn model_table_to_json(table: &Table) -> Value {
    let mut map = Map::new();
    for (key, item) in table.iter() {
        if let Some(value) = toml_item_to_json(item) {
            map.insert(key.to_string(), value);
        }
    }
    Value::Object(map)
}

fn toml_item_to_json(item: &Item) -> Option<Value> {
    match item {
        Item::Value(v) => toml_value_to_json(v),
        Item::Table(t) => {
            let mut map = Map::new();
            for (k, child) in t.iter() {
                if let Some(value) = toml_item_to_json(child) {
                    map.insert(k.to_string(), value);
                }
            }
            Some(Value::Object(map))
        }
        Item::ArrayOfTables(arr) => {
            let values: Vec<Value> = arr
                .iter()
                .map(|t| {
                    let mut map = Map::new();
                    for (k, child) in t.iter() {
                        if let Some(value) = toml_item_to_json(child) {
                            map.insert(k.to_string(), value);
                        }
                    }
                    Value::Object(map)
                })
                .collect();
            Some(Value::Array(values))
        }
        Item::None => None,
    }
}

fn toml_value_to_json(value: &TomlValue) -> Option<Value> {
    match value {
        TomlValue::String(s) => Some(Value::String(s.value().clone())),
        TomlValue::Integer(i) => Some(json!(*i.value())),
        TomlValue::Float(f) => {
            let n = *f.value();
            serde_json::Number::from_f64(n).map(Value::Number)
        }
        TomlValue::Boolean(b) => Some(Value::Bool(*b.value())),
        TomlValue::Datetime(dt) => Some(Value::String(dt.to_string())),
        TomlValue::Array(arr) => {
            let items: Vec<Value> = arr.iter().filter_map(toml_value_to_json).collect();
            Some(Value::Array(items))
        }
        TomlValue::InlineTable(table) => {
            let mut map = Map::new();
            for (k, v) in table.iter() {
                if let Some(value) = toml_value_to_json(v) {
                    map.insert(k.to_string(), value);
                }
            }
            Some(Value::Object(map))
        }
    }
}

fn json_to_toml_item(value: &Value, field_name: &str) -> Option<Item> {
    match value {
        Value::String(s) => Some(toml_edit::value(s.as_str())),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Some(toml_edit::value(i))
            } else if let Some(f) = n.as_f64() {
                Some(toml_edit::value(f))
            } else {
                None
            }
        }
        Value::Bool(b) => Some(toml_edit::value(*b)),
        Value::Array(arr) => {
            let mut toml_arr = Array::default();
            for item in arr {
                match item {
                    Value::String(s) => toml_arr.push(s.as_str()),
                    Value::Number(n) if n.is_i64() => toml_arr.push(n.as_i64().unwrap()),
                    Value::Number(n) if n.is_f64() => toml_arr.push(n.as_f64().unwrap()),
                    Value::Bool(b) => toml_arr.push(*b),
                    _ => {
                        log::warn!("Skipping Grok field '{field_name}': unsupported array item");
                        return None;
                    }
                }
            }
            Some(Item::Value(TomlValue::Array(toml_arr)))
        }
        Value::Object(obj) => {
            let mut inline = InlineTable::new();
            for (k, v) in obj {
                if let Some(s) = v.as_str() {
                    inline.insert(k, s.into());
                } else {
                    log::warn!(
                        "Skipping Grok field '{field_name}.{k}': only string map values supported"
                    );
                }
            }
            Some(Item::Value(TomlValue::InlineTable(inline)))
        }
        Value::Null => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestEnvGuard;
    use serde_json::json;

    fn seed_config(content: &str) {
        let path = get_grok_config_path();
        fs::create_dir_all(path.parent().unwrap()).expect("mkdir");
        fs::write(&path, content).expect("write config");
    }

    #[test]
    fn upsert_preserves_unrelated_sections_and_sets_default() {
        let temp = tempfile::tempdir().expect("tempdir");
        let _env = TestEnvGuard::isolated(temp.path());
        seed_config(
            r#"
# keep me
[ui]
yolo = false

[models]
default = "old"

[model.old]
model = "old-model"
base_url = "https://old.example/v1"
api_key = "old-key"
"#,
        );

        apply_provider_switch(
            "relay",
            &json!({
                "model": "grok-4.5",
                "base_url": "https://relay.example/v1",
                "api_key": "sk-relay",
                "api_backend": "responses",
                "name": "Relay",
                "context_window": 200000,
                "reasoning_efforts": ["low", "high"],
            }),
        )
        .expect("switch");

        let text = fs::read_to_string(get_grok_config_path()).expect("read");
        assert!(text.contains("# keep me"), "comment preserved: {text}");
        assert!(text.contains("yolo = false"), "ui preserved: {text}");
        assert!(
            text.contains("base_url = \"https://old.example/v1\""),
            "old model kept: {text}"
        );
        assert!(text.contains("[model.relay]"), "new section: {text}");
        assert!(text.contains("default = \"relay\""), "default: {text}");
        assert!(text.contains("api_backend = \"responses\""), "{text}");
        assert_eq!(get_default_model().unwrap().as_deref(), Some("relay"));

        let providers = get_providers().expect("providers");
        assert!(providers.contains_key("old"));
        assert!(providers.contains_key("relay"));
        assert_eq!(providers["relay"]["model"], "grok-4.5");
    }

    #[test]
    fn remove_model_keeps_other_sections() {
        let temp = tempfile::tempdir().expect("tempdir");
        let _env = TestEnvGuard::isolated(temp.path());
        seed_config(
            r#"
[models]
default = "keep"

[model.keep]
model = "a"
base_url = "https://a.example/v1"

[model.drop]
model = "b"
base_url = "https://b.example/v1"
"#,
        );
        remove_model("drop").expect("remove");
        let providers = get_providers().expect("providers");
        assert!(providers.contains_key("keep"));
        assert!(!providers.contains_key("drop"));
    }

    #[test]
    fn validate_requires_model_and_base_url() {
        let err = validate_settings(&json!({ "base_url": "https://x" })).unwrap_err();
        assert!(err.to_string().contains("model") || err.to_string().contains("Model"));

        let err = validate_settings(&json!({ "model": "m" })).unwrap_err();
        assert!(err.to_string().to_lowercase().contains("base_url"));
    }

    #[test]
    fn unknown_keys_are_preserved_on_upsert() {
        let temp = tempfile::tempdir().expect("tempdir");
        let _env = TestEnvGuard::isolated(temp.path());
        seed_config(
            r#"
[model.relay]
model = "old"
base_url = "https://old.example/v1"
future_flag = true
"#,
        );
        upsert_model(
            "relay",
            &json!({
                "model": "new",
                "base_url": "https://new.example/v1",
                "api_key": "k",
            }),
        )
        .expect("upsert");
        let text = fs::read_to_string(get_grok_config_path()).expect("read");
        assert!(text.contains("future_flag = true"), "{text}");
        assert!(
            text.contains("base_url = \"https://new.example/v1\""),
            "{text}"
        );
    }
}

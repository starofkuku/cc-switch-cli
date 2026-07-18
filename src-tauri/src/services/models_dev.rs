//! Local cache and helpers for the models.dev provider catalog.
//!
//! Source: `https://models.dev/api.json`
//! Cache: `$CC_SWITCH_CONFIG_DIR/cache/models.dev.json`

use crate::config::get_app_config_dir;
use crate::error::AppError;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const MODELS_DEV_URL: &str = "https://models.dev/api.json";
const CACHE_RELATIVE: &str = "cache/models.dev.json";
const HTTP_TIMEOUT_SECS: u64 = 30;

/// Which protocol pool a vendor belongs to for CLI catalog add.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelsDevProtocolPool {
    /// Claude: Anthropic Messages API vendors.
    Anthropic,
    /// Codex: native OpenAI / Responses-oriented vendors (not openai-compatible chat).
    OpenAiResponses,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelsDevModelLimit {
    #[serde(default)]
    pub context: Option<u64>,
    #[serde(default)]
    pub input: Option<u64>,
    #[serde(default)]
    pub output: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelsDevModel {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub limit: Option<ModelsDevModelLimit>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelsDevProvider {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub api: Option<String>,
    #[serde(default)]
    pub npm: Option<String>,
    #[serde(default)]
    pub env: Vec<String>,
    #[serde(default)]
    pub doc: Option<String>,
    #[serde(default)]
    pub models: BTreeMap<String, ModelsDevModelEntry>,
}

/// Catalog entries may store models as objects with or without nested `id`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ModelsDevModelEntry {
    Full(ModelsDevModel),
    Sparse(Value),
}

impl ModelsDevModelEntry {
    pub fn into_model(self, key: &str) -> ModelsDevModel {
        match self {
            Self::Full(mut model) => {
                if model.id.trim().is_empty() {
                    model.id = key.to_string();
                }
                model
            }
            Self::Sparse(value) => {
                let id = value
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or(key)
                    .to_string();
                let name = value
                    .get("name")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                let limit = value.get("limit").and_then(|limit| {
                    serde_json::from_value::<ModelsDevModelLimit>(limit.clone()).ok()
                });
                ModelsDevModel { id, name, limit }
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct ModelsDevCatalog {
    pub providers: BTreeMap<String, ModelsDevProvider>,
    #[allow(dead_code)]
    pub path: PathBuf,
}

impl ModelsDevCatalog {
    pub fn cache_path() -> PathBuf {
        get_app_config_dir().join(CACHE_RELATIVE)
    }

    /// Load catalog from disk, downloading on first use when missing.
    pub fn load_or_fetch() -> Result<Self, AppError> {
        let path = Self::cache_path();
        if path.is_file() {
            Self::load_from_path(&path)
        } else {
            Self::refresh()?;
            Self::load_from_path(&path)
        }
    }

    pub fn load_from_path(path: &Path) -> Result<Self, AppError> {
        let raw = fs::read_to_string(path).map_err(|e| AppError::io(path, e))?;
        let root: BTreeMap<String, Value> = serde_json::from_str(&raw)
            .map_err(|e| AppError::Message(format!("Invalid models.dev cache JSON: {e}")))?;

        let mut providers = BTreeMap::new();
        for (key, value) in root {
            let mut provider: ModelsDevProvider = serde_json::from_value(value).map_err(|e| {
                AppError::Message(format!("Invalid models.dev provider '{key}': {e}"))
            })?;
            if provider.id.trim().is_empty() {
                provider.id = key.clone();
            }
            // Normalize nested model ids from map keys when missing.
            let mut models = BTreeMap::new();
            for (model_key, entry) in provider.models {
                let model = entry.into_model(&model_key);
                models.insert(model_key, ModelsDevModelEntry::Full(model));
            }
            provider.models = models;
            providers.insert(key, provider);
        }

        Ok(Self {
            providers,
            path: path.to_path_buf(),
        })
    }

    /// Download and replace the local cache.
    pub fn refresh() -> Result<PathBuf, AppError> {
        let path = Self::cache_path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| AppError::io(parent, e))?;
        }

        let runtime = tokio::runtime::Runtime::new()
            .map_err(|e| AppError::Message(format!("Failed to create async runtime: {e}")))?;
        let body = runtime.block_on(async {
            let client = reqwest::Client::builder()
                .timeout(Duration::from_secs(HTTP_TIMEOUT_SECS))
                .user_agent(concat!(
                    env!("CARGO_PKG_NAME"),
                    "/",
                    env!("CARGO_PKG_VERSION"),
                    " models-dev-catalog"
                ))
                .build()
                .map_err(|e| AppError::Message(format!("Failed to build HTTP client: {e}")))?;

            let response = client
                .get(MODELS_DEV_URL)
                .send()
                .await
                .map_err(|e| {
                    AppError::Message(format!("Failed to download models.dev catalog: {e}"))
                })?
                .error_for_status()
                .map_err(|e| AppError::Message(format!("models.dev catalog HTTP error: {e}")))?;

            response
                .text()
                .await
                .map_err(|e| AppError::Message(format!("Failed to read models.dev response: {e}")))
        })?;

        // Validate JSON before writing.
        let _: BTreeMap<String, Value> = serde_json::from_str(&body).map_err(|e| {
            AppError::Message(format!("models.dev response is not valid JSON: {e}"))
        })?;

        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, body.as_bytes()).map_err(|e| AppError::io(&tmp, e))?;
        fs::rename(&tmp, &path).map_err(|e| AppError::io(&path, e))?;
        Ok(path)
    }

    pub fn providers_for_pool(&self, pool: ModelsDevProtocolPool) -> Vec<&ModelsDevProvider> {
        let mut list: Vec<&ModelsDevProvider> = self
            .providers
            .values()
            .filter(|provider| provider_matches_pool(provider, pool))
            .collect();
        list.sort_by(|a, b| {
            let an = a.name.as_deref().unwrap_or(a.id.as_str());
            let bn = b.name.as_deref().unwrap_or(b.id.as_str());
            an.to_ascii_lowercase()
                .cmp(&bn.to_ascii_lowercase())
                .then_with(|| a.id.cmp(&b.id))
        });
        list
    }
}

fn provider_has_api(provider: &ModelsDevProvider) -> bool {
    provider
        .api
        .as_deref()
        .map(str::trim)
        .is_some_and(|api| !api.is_empty())
}

fn provider_matches_pool(provider: &ModelsDevProvider, pool: ModelsDevProtocolPool) -> bool {
    if !provider_has_api(provider) {
        return false;
    }
    let npm = provider.npm.as_deref().unwrap_or("").to_ascii_lowercase();
    match pool {
        ModelsDevProtocolPool::Anthropic => {
            npm == "@ai-sdk/anthropic" || npm.ends_with("/anthropic")
        }
        ModelsDevProtocolPool::OpenAiResponses => {
            // Explicitly exclude Chat Completions compatible shims.
            if npm.contains("openai-compatible") {
                return false;
            }
            npm == "@ai-sdk/openai" || npm.ends_with("/openai")
        }
    }
}

pub fn model_list(provider: &ModelsDevProvider) -> Vec<ModelsDevModel> {
    let mut models: Vec<ModelsDevModel> = provider
        .models
        .iter()
        .map(|(key, entry)| match entry {
            ModelsDevModelEntry::Full(model) => {
                let mut model = model.clone();
                if model.id.trim().is_empty() {
                    model.id = key.clone();
                }
                model
            }
            ModelsDevModelEntry::Sparse(value) => {
                ModelsDevModelEntry::Sparse(value.clone()).into_model(key)
            }
        })
        .filter(|model| !model.id.trim().is_empty())
        .collect();
    models.sort_by(|a, b| {
        let an = a.name.as_deref().unwrap_or(a.id.as_str());
        let bn = b.name.as_deref().unwrap_or(b.id.as_str());
        an.to_ascii_lowercase()
            .cmp(&bn.to_ascii_lowercase())
            .then_with(|| a.id.cmp(&b.id))
    });
    models
}

/// Preferred context window size for apps that store context limits (Hermes/Pi/…).
pub fn model_context_limit(model: &ModelsDevModel) -> Option<u64> {
    model
        .limit
        .as_ref()
        .and_then(|limit| limit.context.or(limit.input))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn pool_filter_hides_null_api_and_openai_compatible() {
        let anthropic = ModelsDevProvider {
            id: "kimi".into(),
            name: Some("Kimi".into()),
            api: Some("https://api.kimi.com/v1".into()),
            npm: Some("@ai-sdk/anthropic".into()),
            env: vec![],
            doc: None,
            models: BTreeMap::new(),
        };
        let compatible = ModelsDevProvider {
            id: "relay".into(),
            name: Some("Relay".into()),
            api: Some("https://relay.example/v1".into()),
            npm: Some("@ai-sdk/openai-compatible".into()),
            env: vec![],
            doc: None,
            models: BTreeMap::new(),
        };
        let openai = ModelsDevProvider {
            id: "oa".into(),
            name: Some("OpenAI".into()),
            api: Some("https://api.openai.com/v1".into()),
            npm: Some("@ai-sdk/openai".into()),
            env: vec![],
            doc: None,
            models: BTreeMap::new(),
        };
        let no_api = ModelsDevProvider {
            id: "anthropic".into(),
            name: Some("Anthropic".into()),
            api: None,
            npm: Some("@ai-sdk/anthropic".into()),
            env: vec![],
            doc: None,
            models: BTreeMap::new(),
        };

        assert!(provider_matches_pool(
            &anthropic,
            ModelsDevProtocolPool::Anthropic
        ));
        assert!(!provider_matches_pool(
            &no_api,
            ModelsDevProtocolPool::Anthropic
        ));
        assert!(!provider_matches_pool(
            &compatible,
            ModelsDevProtocolPool::OpenAiResponses
        ));
        assert!(provider_matches_pool(
            &openai,
            ModelsDevProtocolPool::OpenAiResponses
        ));
    }

    #[test]
    fn model_entry_sparse_reads_limit_context() {
        let entry = ModelsDevModelEntry::Sparse(json!({
            "name": "Demo",
            "limit": { "context": 128000, "output": 8192 }
        }));
        let model = entry.into_model("demo-id");
        assert_eq!(model.id, "demo-id");
        assert_eq!(model_context_limit(&model), Some(128000));
    }
}

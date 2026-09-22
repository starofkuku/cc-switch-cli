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
    /// Codex: native OpenAI / Responses-oriented vendors.
    OpenAiResponses,
    /// Codex through the CC-Switch proxy: OpenAI-compatible Chat Completions vendors.
    OpenAiChat,
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
pub struct ModelsDevModalities {
    #[serde(default)]
    pub input: Vec<String>,
    #[serde(default)]
    pub output: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelsDevModel {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub reasoning: Option<bool>,
    #[serde(default)]
    pub tool_call: Option<bool>,
    #[serde(default)]
    pub attachment: Option<bool>,
    #[serde(default)]
    pub modalities: Option<ModelsDevModalities>,
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
                let reasoning = value.get("reasoning").and_then(Value::as_bool);
                let tool_call = value.get("tool_call").and_then(Value::as_bool);
                let attachment = value.get("attachment").and_then(Value::as_bool);
                let modalities = value.get("modalities").and_then(|modalities| {
                    serde_json::from_value::<ModelsDevModalities>(modalities.clone()).ok()
                });
                let limit = value.get("limit").and_then(|limit| {
                    serde_json::from_value::<ModelsDevModelLimit>(limit.clone()).ok()
                });
                ModelsDevModel {
                    id,
                    name,
                    reasoning,
                    tool_call,
                    attachment,
                    modalities,
                    limit,
                }
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
            npm == "@ai-sdk/openai" || npm.ends_with("/openai")
        }
        ModelsDevProtocolPool::OpenAiChat => {
            npm == "@ai-sdk/openai-compatible" || npm.ends_with("/openai-compatible")
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

/// Maximum output tokens, mapped from models.dev `limit.output`.
pub fn model_max_tokens(model: &ModelsDevModel) -> Option<u64> {
    model.limit.as_ref().and_then(|limit| limit.output)
}

/// Normalize model ids for fuzzy catalog matching.
pub fn normalize_model_id(id: &str) -> String {
    let mut s = id.trim().to_ascii_lowercase();
    for prefix in [
        "openai/",
        "anthropic/",
        "google/",
        "meta/",
        "mistral/",
        "deepseek/",
        "xai/",
        "amazon-bedrock/",
        "bedrock/",
    ] {
        if let Some(rest) = s.strip_prefix(prefix) {
            s = rest.to_string();
            break;
        }
    }
    // Also strip trailing provider-style prefixes like "openai."
    for prefix in ["openai.", "anthropic.", "google."] {
        if let Some(rest) = s.strip_prefix(prefix) {
            s = rest.to_string();
            break;
        }
    }
    s
}

/// Best-effort lookup of a catalog model by upstream `/v1/models` id.
///
/// When multiple providers share the same id, prefer the entry with the largest
/// known context window (more complete metadata).
/// Collapse a model id to a comparison key for fuzzy matching.
///
/// Strips case, surrounding whitespace, vendor prefixes and dated release
/// suffixes so `openai/GPT-5.4-2026-01-15` and `gpt-5.4` compare equal.
fn fuzzy_key(id: &str) -> String {
    let normalized = normalize_model_id(id);
    strip_release_suffix(&normalized)
}

/// Remove trailing release markers such as `-2026-01-15`, `-20260115`,
/// `@2026-01-15` or `:latest`, while keeping meaningful name parts.
///
/// The date may contain separators itself, so this scans backwards from each
/// candidate separator and trims the longest trailing date-shaped run.
fn strip_release_suffix(id: &str) -> String {
    let mut s = id.trim().to_string();
    if let Some(rest) = s.strip_suffix(":latest") {
        s = rest.to_string();
    }

    // Separator positions, left to right. Try the earliest one first so the
    // whole `-2026-01-15` run is removed in a single pass.
    let separators: Vec<usize> = s
        .char_indices()
        .filter(|(_, c)| *c == '-' || *c == '@')
        .map(|(i, _)| i)
        .collect();

    for idx in separators {
        let head = &s[..idx];
        let tail = &s[idx + 1..];
        if head.is_empty() {
            continue;
        }
        if is_release_marker(tail) {
            s = head.to_string();
            break;
        }
    }

    s
}

/// A trailing release marker: `YYYYMMDD` or `YYYY-MM-DD`.
fn is_release_marker(value: &str) -> bool {
    is_date_like(value.trim_start_matches(['-', '@']))
}

/// `YYYYMMDD` or `YYYY-MM-DD`.
fn is_date_like(value: &str) -> bool {
    match value.len() {
        8 => value.chars().all(|c| c.is_ascii_digit()),
        10 => {
            let bytes = value.as_bytes();
            bytes[4] == b'-'
                && bytes[7] == b'-'
                && bytes
                    .iter()
                    .enumerate()
                    .all(|(i, b)| *b == b'-' || (i != 4 && i != 7 && b.is_ascii_digit()))
        }
        _ => false,
    }
}

/// Similarity score in `0..=1000` between two fuzzy keys.
///
/// Exact match wins outright; otherwise a shared prefix and a long common
/// prefix ratio are combined so `gpt-5.4` beats `gpt-5.4-mini` for query
/// `gpt5.4`, while unrelated ids stay low.
fn similarity_score(query: &str, candidate: &str) -> u64 {
    if query == candidate {
        return 1000;
    }
    if query.is_empty() || candidate.is_empty() {
        return 0;
    }

    let shared_prefix = query
        .chars()
        .zip(candidate.chars())
        .take_while(|(a, b)| a == b)
        .count();
    let prefix_ratio = (shared_prefix * 2 * 1000) as u64
        / (query.chars().count() + candidate.chars().count()) as u64;

    // Containment is a strong signal but weaker than an exact match.
    let contains: u64 = if candidate.contains(query) || query.contains(candidate) {
        200
    } else {
        0
    };

    prefix_ratio + contains
}

/// Rank every catalog model against `upstream_id`, best first.
///
/// Used both to auto-match and to build the manual picker candidates.
pub fn rank_catalog_models(
    catalog: &ModelsDevCatalog,
    upstream_id: &str,
) -> Vec<(ModelsDevModel, u64)> {
    let needle = fuzzy_key(upstream_id);
    if needle.is_empty() {
        return Vec::new();
    }

    let mut scored: Vec<(ModelsDevModel, u64)> = catalog
        .providers
        .values()
        .flat_map(|provider| provider.models.iter())
        .map(|(key, entry)| {
            let model = entry.clone().into_model(key);
            let score = [
                fuzzy_key(&model.id),
                fuzzy_key(key),
                model.name.as_deref().map(fuzzy_key).unwrap_or_default(),
            ]
            .iter()
            .map(|candidate| similarity_score(&needle, candidate))
            .max()
            .unwrap_or(0);
            (model, score)
        })
        .filter(|(_, score)| *score > 0)
        .collect();

    // Prefer the higher score, then the larger context window, then a stable id.
    scored.sort_by(|a, b| {
        b.1.cmp(&a.1)
            .then_with(|| {
                model_context_limit(&b.0)
                    .unwrap_or(0)
                    .cmp(&model_context_limit(&a.0).unwrap_or(0))
            })
            .then_with(|| a.0.id.cmp(&b.0.id))
    });
    scored
}

/// Confidence at or above which a fuzzy hit is applied without asking.
const AUTO_MATCH_MIN_SCORE: u64 = 800;

/// Best automatic catalog match for an upstream model id.
///
/// Returns `None` when nothing scores high enough, leaving the decision to the
/// caller (usually a manual picker).
pub fn find_catalog_model_fuzzy(
    catalog: &ModelsDevCatalog,
    upstream_id: &str,
) -> Option<ModelsDevModel> {
    rank_catalog_models(catalog, upstream_id)
        .into_iter()
        .find(|(_, score)| *score >= AUTO_MATCH_MIN_SCORE)
        .map(|(model, _)| model)
}

/// Every catalog model, sorted by provider then id, for the manual picker.
pub fn all_catalog_models(catalog: &ModelsDevCatalog) -> Vec<ModelsDevModel> {
    let mut models: Vec<ModelsDevModel> = catalog
        .providers
        .values()
        .flat_map(|provider| provider.models.iter())
        .map(|(key, entry)| entry.clone().into_model(key))
        .collect();
    models.sort_by(|a, b| a.id.cmp(&b.id));
    models.dedup_by(|a, b| a.id == b.id);
    models
}

/// Catalog models closest to `upstream_id`, for the manual picker's shortlist.
pub fn catalog_candidates(
    catalog: &ModelsDevCatalog,
    upstream_id: &str,
    limit: usize,
) -> Vec<ModelsDevModel> {
    rank_catalog_models(catalog, upstream_id)
        .into_iter()
        .take(limit)
        .map(|(model, _)| model)
        .collect()
}

/// Build an OpenClaw/Pi `models[]` entry from an upstream model id.
///
/// - `id` always uses the upstream id (so the client can call the provider).
/// - Extra fields are filled from models.dev when a match is found.
/// - When no catalog match exists, returns `{ "id": "<upstream>" }` for manual edit.
pub fn openclaw_model_entry_from_upstream(
    upstream_id: &str,
    catalog: Option<&ModelsDevCatalog>,
) -> (Value, bool) {
    let mut obj = serde_json::Map::new();
    obj.insert("id".into(), Value::String(upstream_id.trim().to_string()));

    let Some(catalog) = catalog else {
        return (Value::Object(obj), false);
    };
    let Some(matched) = find_catalog_model_fuzzy(catalog, upstream_id) else {
        return (Value::Object(obj), false);
    };

    enrich_model_entry(&mut obj, &matched);
    (Value::Object(obj), true)
}

/// Copy the catalog-derived fields onto an existing `{"id": ...}` entry,
/// preserving anything already present that models.dev does not define.
pub fn enrich_model_entry(obj: &mut serde_json::Map<String, Value>, matched: &ModelsDevModel) {
    if let Some(name) = matched
        .name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        obj.insert("name".into(), Value::String(name.to_string()));
    }
    if let Some(ctx) = model_context_limit(matched) {
        if ctx <= u64::from(u32::MAX) {
            obj.insert("contextWindow".into(), Value::Number(ctx.into()));
        }
    }
    if let Some(max_tokens) = model_max_tokens(matched) {
        if max_tokens <= u64::from(u32::MAX) {
            obj.insert("maxTokens".into(), Value::Number(max_tokens.into()));
        }
    }
    if let Some(reasoning) = matched.reasoning {
        obj.insert("reasoning".into(), Value::Bool(reasoning));
    }
    if let Some(tool_call) = matched.tool_call {
        obj.insert("toolCall".into(), Value::Bool(tool_call));
    }
    if let Some(attachment) = matched.attachment {
        obj.insert("attachment".into(), Value::Bool(attachment));
    }
    if let Some(modalities) = &matched.modalities {
        // Pi (and OpenClaw-shaped entries shared with Pi) only accept text/image.
        // models.dev also lists pdf/audio/video which fail Pi's models.json schema.
        let mut seen = std::collections::HashSet::new();
        let input: Vec<Value> = modalities
            .input
            .iter()
            .map(String::as_str)
            .filter(|modality| matches!(*modality, "text" | "image"))
            .filter(|modality| seen.insert(*modality))
            .map(|modality| Value::String(modality.to_string()))
            .collect();
        if !input.is_empty() {
            obj.insert("input".into(), Value::Array(input));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn release_suffixes_are_stripped_for_fuzzy_matching() {
        // Dated variants must collapse onto the bare model id.
        assert_eq!(fuzzy_key("gpt-5.4-2026-01-15"), "gpt-5.4");
        assert_eq!(fuzzy_key("gpt-5.4-20260115"), "gpt-5.4");
        assert_eq!(fuzzy_key("openai/GPT-5.4-2026-01-15"), "gpt-5.4");
        assert_eq!(fuzzy_key("gpt-5.4:latest"), "gpt-5.4");

        // Meaningful suffixes must survive.
        assert_eq!(fuzzy_key("gpt-5.4"), "gpt-5.4");
        assert_eq!(fuzzy_key("gpt-5.4-mini"), "gpt-5.4-mini");
        assert_eq!(fuzzy_key("claude-sonnet-5"), "claude-sonnet-5");
    }

    #[test]
    fn similarity_ranks_exact_above_partial() {
        let exact = similarity_score("gpt-5.4", "gpt-5.4");
        let variant = similarity_score("gpt-5.4", "gpt-5.4-mini");
        let unrelated = similarity_score("gpt-5.4", "llama-3-70b");
        assert!(
            exact > variant,
            "exact {exact} should beat variant {variant}"
        );
        assert!(variant > unrelated, "variant {variant} vs {unrelated}");
        assert_eq!(exact, 1000);
    }

    #[test]
    fn pool_filter_classifies_responses_and_chat_providers() {
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
            &compatible,
            ModelsDevProtocolPool::OpenAiChat
        ));
        assert!(provider_matches_pool(
            &openai,
            ModelsDevProtocolPool::OpenAiResponses
        ));
        assert!(!provider_matches_pool(
            &openai,
            ModelsDevProtocolPool::OpenAiChat
        ));
    }

    #[test]
    fn model_entry_sparse_reads_limit_context() {
        let entry = ModelsDevModelEntry::Sparse(json!({
            "name": "Demo",
            "reasoning": true,
            "modalities": { "input": ["text", "image"] },
            "limit": { "context": 128000, "output": 8192 }
        }));
        let model = entry.into_model("demo-id");
        assert_eq!(model.id, "demo-id");
        assert_eq!(model_context_limit(&model), Some(128000));
        assert_eq!(model.reasoning, Some(true));
    }

    #[test]
    fn normalize_model_id_strips_provider_prefix() {
        assert_eq!(normalize_model_id("openai/gpt-5.4"), "gpt-5.4");
        assert_eq!(normalize_model_id("GPT-5.4"), "gpt-5.4");
    }

    #[test]
    fn openclaw_entry_enriches_when_catalog_matches() {
        let mut models = BTreeMap::new();
        models.insert(
            "gpt-5.4".into(),
            ModelsDevModelEntry::Full(ModelsDevModel {
                id: "gpt-5.4".into(),
                name: Some("GPT-5.4".into()),
                reasoning: Some(true),
                tool_call: Some(true),
                attachment: None,
                modalities: Some(ModelsDevModalities {
                    input: vec![
                        "text".into(),
                        "image".into(),
                        "pdf".into(),
                        "video".into(),
                        "audio".into(),
                    ],
                    output: vec!["text".into()],
                }),
                limit: Some(ModelsDevModelLimit {
                    context: Some(200_000),
                    input: None,
                    output: None,
                }),
            }),
        );
        let mut providers = BTreeMap::new();
        providers.insert(
            "demo".into(),
            ModelsDevProvider {
                id: "demo".into(),
                name: Some("Demo".into()),
                api: Some("https://example.com/v1".into()),
                npm: Some("@ai-sdk/openai".into()),
                env: vec![],
                doc: None,
                models,
            },
        );
        let catalog = ModelsDevCatalog {
            providers,
            path: PathBuf::from("/tmp/models.dev.json"),
        };

        let (entry, enriched) = openclaw_model_entry_from_upstream("gpt-5.4", Some(&catalog));
        assert!(enriched);
        assert_eq!(entry["id"], "gpt-5.4");
        assert_eq!(entry["name"], "GPT-5.4");
        assert_eq!(entry["contextWindow"], 200_000);
        assert_eq!(entry["reasoning"], true);
        // Pi schema only allows text/image; pdf/audio/video must be dropped.
        assert_eq!(entry["input"], json!(["text", "image"]));

        let (bare, ok) = openclaw_model_entry_from_upstream("totally-custom-id", Some(&catalog));
        assert!(!ok);
        assert_eq!(bare, json!({ "id": "totally-custom-id" }));
    }
}

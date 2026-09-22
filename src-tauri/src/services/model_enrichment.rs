//! Shared model-catalog enrichment used by `provider add`, `provider edit` and
//! `provider add-model`.
//!
//! All three entry points must resolve upstream model ids the same way:
//!
//! 1. Try a fuzzy match against the local models.dev catalog.
//! 2. On a confident hit, fill the entry from the catalog.
//! 3. On a miss, let the user pick the right catalog entry by hand (TTY only).
//! 4. With no TTY (scripts, CI), keep an id-only entry and warn instead of
//!    blocking on a prompt that could never be answered.

use serde_json::Value;

use crate::error::AppError;
use crate::services::models_dev::{
    all_catalog_models, catalog_candidates, enrich_model_entry, find_catalog_model_fuzzy,
    ModelsDevCatalog, ModelsDevModel,
};

/// How one upstream model id was resolved against the catalog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelResolution {
    /// Filled automatically from a confident fuzzy match.
    Auto,
    /// The user picked the catalog entry manually.
    Manual,
    /// Recorded as `{"id": ...}` because nothing matched.
    Unmatched,
}

/// Result of resolving a batch of upstream ids.
#[derive(Debug, Clone, Default)]
pub struct ModelEnrichmentSummary {
    pub auto: usize,
    pub manual: usize,
    pub unmatched: Vec<String>,
}

impl ModelEnrichmentSummary {
    pub fn record(&mut self, resolution: ModelResolution, id: &str) {
        match resolution {
            ModelResolution::Auto => self.auto += 1,
            ModelResolution::Manual => self.manual += 1,
            ModelResolution::Unmatched => self.unmatched.push(id.to_string()),
        }
    }

    /// Human-readable tail shared by every caller, e.g.
    /// `2 enriched from models.dev, 1 picked manually, 1 without catalog match (id-only): x`.
    pub fn describe(&self) -> String {
        let mut parts = Vec::new();
        if self.auto > 0 {
            parts.push(format!("{} auto-enriched from models.dev", self.auto));
        }
        if self.manual > 0 {
            parts.push(format!("{} picked manually", self.manual));
        }
        if !self.unmatched.is_empty() {
            parts.push(format!(
                "{} without a catalog match (id-only): {}",
                self.unmatched.len(),
                self.unmatched.join(", ")
            ));
        }
        if parts.is_empty() {
            "no models processed".to_string()
        } else {
            parts.join(", ")
        }
    }
}

/// Whether an interactive prompt can be shown on the current stdin/stdout.
pub(crate) fn can_prompt_interactively() -> bool {
    use std::io::IsTerminal;
    std::io::stdin().is_terminal() && std::io::stdout().is_terminal()
}

/// Resolve one upstream model id into a `models[]` entry.
///
/// `existing_entry` is reused when present so hand-added fields survive; only
/// catalog-known keys are overwritten.
pub(crate) fn resolve_model_entry(
    upstream_id: &str,
    catalog: Option<&ModelsDevCatalog>,
    existing_entry: Option<&Value>,
    interactive: bool,
) -> Result<(Value, ModelResolution), AppError> {
    let upstream_id = upstream_id.trim();
    let mut obj = existing_entry
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    obj.insert("id".into(), Value::String(upstream_id.to_string()));

    let Some(catalog) = catalog else {
        return Ok((Value::Object(obj), ModelResolution::Unmatched));
    };

    if let Some(matched) = find_catalog_model_fuzzy(catalog, upstream_id) {
        enrich_model_entry(&mut obj, &matched);
        return Ok((Value::Object(obj), ModelResolution::Auto));
    }

    if !interactive {
        return Ok((Value::Object(obj), ModelResolution::Unmatched));
    }

    match prompt_catalog_pick(upstream_id, catalog)? {
        Some(matched) => {
            enrich_model_entry(&mut obj, &matched);
            Ok((Value::Object(obj), ModelResolution::Manual))
        }
        None => Ok((Value::Object(obj), ModelResolution::Unmatched)),
    }
}

/// Let the user pick the catalog entry for `upstream_id`.
///
/// The shortlist is fuzzy-ranked first, with a full-catalog search as fallback.
/// Returns `None` when the user opts to keep the id-only entry.
fn prompt_catalog_pick(
    upstream_id: &str,
    catalog: &ModelsDevCatalog,
) -> Result<Option<ModelsDevModel>, AppError> {
    use inquire::Select;

    let fallback = all_catalog_models(catalog);
    let shortlist = catalog_candidates(catalog, upstream_id, 20);

    let mut options: Vec<String> = shortlist.iter().map(describe_model).collect();
    if !fallback.is_empty() {
        options.push("Search the full models.dev catalog…".to_string());
    }
    options.push("Keep id-only (no catalog parameters)".to_string());

    let prompt =
        format!("No confident models.dev match for '{upstream_id}' — pick one (type to filter)");
    let picked = Select::new(&prompt, options.clone())
        .with_page_size(20)
        .prompt()
        .map_err(|e| AppError::Message(format!("Prompt failed: {e}")))?;

    if picked == "Keep id-only (no catalog parameters)" {
        return Ok(None);
    }

    if picked == "Search the full models.dev catalog…" {
        if fallback.is_empty() {
            return Ok(None);
        }
        let all: Vec<String> = fallback.iter().map(describe_model).collect();
        let picked = Select::new(
            &format!("Pick a models.dev entry for '{upstream_id}'"),
            all.clone(),
        )
        .with_page_size(20)
        .prompt()
        .map_err(|e| AppError::Message(format!("Prompt failed: {e}")))?;
        let index = all
            .iter()
            .position(|option| option == &picked)
            .ok_or_else(|| AppError::Message("Selection index out of range".to_string()))?;
        return Ok(fallback.get(index).cloned());
    }

    let index = shortlist
        .iter()
        .map(describe_model)
        .position(|option| option == picked)
        .ok_or_else(|| AppError::Message("Selection index out of range".to_string()))?;
    Ok(shortlist.get(index).cloned())
}

fn describe_model(model: &ModelsDevModel) -> String {
    let ctx = crate::services::models_dev::model_context_limit(model)
        .map(|value| format!(", ctx={value}"))
        .unwrap_or_default();
    format!("{}{}", model.id, ctx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::models_dev::ModelsDevCatalog;
    use serde_json::json;
    use std::collections::BTreeMap;

    fn catalog() -> ModelsDevCatalog {
        let raw = json!({
            "openai": {
                "id": "openai",
                "models": {
                    "gpt-5.4": {
                        "id": "gpt-5.4",
                        "name": "GPT-5.4",
                        "limit": { "context": 1050000, "output": 128000 }
                    },
                    "gpt-5.4-mini": {
                        "id": "gpt-5.4-mini",
                        "name": "GPT-5.4 mini",
                        "limit": { "context": 400000, "output": 64000 }
                    }
                }
            }
        });
        let providers: BTreeMap<String, _> = serde_json::from_value(raw).expect("parse catalog");
        ModelsDevCatalog {
            providers,
            path: std::path::PathBuf::from("/tmp/models.dev.json"),
        }
    }

    #[test]
    fn exact_and_dated_ids_auto_match() {
        let catalog = catalog();
        let (entry, resolution) =
            resolve_model_entry("gpt-5.4", Some(&catalog), None, false).expect("resolve");
        assert_eq!(resolution, ModelResolution::Auto);
        assert_eq!(entry["contextWindow"], json!(1050000));
        assert_eq!(entry["maxTokens"], json!(128000));

        let (entry, resolution) =
            resolve_model_entry("openai/GPT-5.4-2026-01-15", Some(&catalog), None, false)
                .expect("resolve");
        assert_eq!(resolution, ModelResolution::Auto);
        assert_eq!(entry["contextWindow"], json!(1050000));
    }

    #[test]
    fn unknown_id_is_id_only_and_reported() {
        let catalog = catalog();
        let (entry, resolution) =
            resolve_model_entry("totally-made-up", Some(&catalog), None, false).expect("resolve");
        assert_eq!(resolution, ModelResolution::Unmatched);
        assert_eq!(entry, json!({ "id": "totally-made-up" }));
    }

    #[test]
    fn existing_fields_survive_enrichment() {
        let catalog = catalog();
        let existing = json!({ "id": "gpt-5.4", "custom": "keep-me" });
        let (entry, _) = resolve_model_entry("gpt-5.4", Some(&catalog), Some(&existing), false)
            .expect("resolve");
        assert_eq!(entry["custom"], json!("keep-me"));
        assert_eq!(entry["contextWindow"], json!(1050000));
    }

    #[test]
    fn summary_describes_all_buckets() {
        let mut summary = ModelEnrichmentSummary::default();
        summary.record(ModelResolution::Auto, "a");
        summary.record(ModelResolution::Manual, "b");
        summary.record(ModelResolution::Unmatched, "c");
        let text = summary.describe();
        assert!(text.contains("1 auto-enriched"));
        assert!(text.contains("1 picked manually"));
        assert!(text.contains("c"));
    }
}

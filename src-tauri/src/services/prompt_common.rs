//! Global common prompt preamble: a small preset library with a single
//! active entry whose content is rendered into every live prompt file
//! (`CLAUDE.md` / `AGENTS.md` / `GEMINI.md`) inside managed markers.
//!
//! Storage: one settings-table key (`prompt_common`) holding the presets and
//! the active id — no schema change, and it rides along with WebDAV db sync.
//! Rendering: `apply_common_preamble` rewrites only the marker section at the
//! very top of a live file; everything below the markers is app-private and
//! survives untouched.

use serde::{Deserialize, Serialize};

use crate::app_config::AppType;
use crate::config::write_text_file;
use crate::error::AppError;
use crate::prompt_files::prompt_file_path;
use crate::AppState;

const SETTINGS_KEY: &str = "prompt_common";
pub const COMMON_START_MARKER: &str = "<!-- cc-switch common start -->";
pub const COMMON_END_MARKER: &str = "<!-- cc-switch common end -->";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CommonPreset {
    pub id: String,
    pub name: String,
    pub content: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct CommonPromptState {
    #[serde(default)]
    pub presets: Vec<CommonPreset>,
    #[serde(default)]
    pub active_id: Option<String>,
}

fn load_state(db: &crate::database::Database) -> Result<CommonPromptState, AppError> {
    match db.get_setting(SETTINGS_KEY)? {
        Some(raw) => serde_json::from_str(&raw)
            .map_err(|e| AppError::Config(format!("invalid {SETTINGS_KEY} payload: {e}"))),
        None => Ok(CommonPromptState::default()),
    }
}

fn save_state(db: &crate::database::Database, state: &CommonPromptState) -> Result<(), AppError> {
    let raw = serde_json::to_string(state)
        .map_err(|e| AppError::Config(format!("serialize {SETTINGS_KEY} failed: {e}")))?;
    db.set_setting(SETTINGS_KEY, &raw)
}

// ---------------------------------------------------------------------------
// Rendering helpers (pure)
// ---------------------------------------------------------------------------

fn marker_section(content: &str) -> String {
    format!("{COMMON_START_MARKER}\n{content}\n{COMMON_END_MARKER}")
}

/// Split a live document into `(before_markers, section_content, after_markers)`.
/// `before` is always empty for well-formed files (the section lives at the top)
/// but is preserved anyway so a hand-moved section round-trips.
fn split_section(document: &str) -> Option<(String, String, String)> {
    let start = document.find(COMMON_START_MARKER)?;
    let content_start = start + COMMON_START_MARKER.len();
    // Trim exactly one leading newline after the marker.
    let content_start = document[content_start..]
        .strip_prefix('\n')
        .map(|_| content_start + 1)
        .unwrap_or(content_start);
    let end = document[content_start..].find(COMMON_END_MARKER)? + content_start;
    let content_end = document[..end]
        .strip_suffix('\n')
        .map(|rest| rest.len())
        .unwrap_or(end);

    let after_marker_end = (end + COMMON_END_MARKER.len())
        + document[end + COMMON_END_MARKER.len()..]
            .strip_prefix("\n\n")
            .map(|_| 2)
            .or_else(|| {
                document[end + COMMON_END_MARKER.len()..]
                    .strip_prefix('\n')
                    .map(|_| 1)
            })
            .unwrap_or(0);

    Some((
        document[..start].to_string(),
        document[content_start..content_end].to_string(),
        document[after_marker_end..].to_string(),
    ))
}

/// Insert / replace / remove the common marker section at the top of a live
/// document. `None` or empty common content removes the section entirely.
pub(crate) fn apply_common_preamble(document: &str, common: Option<&str>) -> String {
    let active = common.filter(|content| !content.trim().is_empty());
    match (split_section(document), active) {
        (Some((before, _, after)), Some(content)) => {
            if after.trim().is_empty() {
                format!("{before}{}\n", marker_section(content))
            } else {
                format!("{before}{}\n\n{after}", marker_section(content))
            }
        }
        (Some((before, _, after)), None) => {
            let joined = format!("{before}{after}");
            joined.trim_start_matches('\n').to_string()
        }
        (None, Some(content)) => {
            if document.trim().is_empty() {
                marker_section(content)
            } else {
                format!("{}\n\n{document}", marker_section(content))
            }
        }
        (None, None) => document.to_string(),
    }
}

/// Drop the managed section, returning only the app-private content.
pub(crate) fn strip_common_section(document: &str) -> String {
    match split_section(document) {
        Some((_, _, after)) => after.trim_start_matches('\n').to_string(),
        None => document.to_string(),
    }
}

fn active_common_content(db: &crate::database::Database) -> Result<Option<String>, AppError> {
    let state = load_state(db)?;
    Ok(state
        .active_id
        .and_then(|id| state.presets.iter().find(|p| p.id == id))
        .map(|preset| preset.content.clone()))
}

/// Render the private content plus the active common preamble (used by every
/// path that writes a live prompt file).
pub(crate) fn render_live(
    db: &crate::database::Database,
    private_content: &str,
) -> Result<String, AppError> {
    Ok(apply_common_preamble(
        private_content,
        active_common_content(db)?.as_deref(),
    ))
}

// ---------------------------------------------------------------------------
// Repaint: apply the active preamble to every initialized app's live file
// ---------------------------------------------------------------------------

/// Rewrite the marker section of every existing live prompt file. Apps whose
/// live file does not exist yet are skipped — we never create live files here,
/// activation/import create them (and render through `render_live`).
pub fn reapply_common_to_all_apps(state: &AppState) -> Result<usize, AppError> {
    let common = active_common_content(&state.db)?;
    let mut touched = 0usize;
    for app in AppType::all() {
        if !crate::sync_policy::should_sync_live(&app) {
            continue;
        }
        let path = match prompt_file_path(&app) {
            Ok(path) => path,
            Err(err) => {
                log::warn!("解析 {app} 提示词路径失败,跳过公共前缀渲染: {err}");
                continue;
            }
        };
        if !path.is_file() {
            continue;
        }
        let document = match std::fs::read_to_string(&path) {
            Ok(document) => document,
            Err(err) => {
                log::warn!("读取 {} 失败,跳过公共前缀渲染: {err}", path.display());
                continue;
            }
        };
        let rendered = apply_common_preamble(&document, common.as_deref());
        if rendered == document {
            continue;
        }
        write_text_file(&path, &rendered)?;
        touched += 1;
    }
    Ok(touched)
}

// ---------------------------------------------------------------------------
// Store operations
// ---------------------------------------------------------------------------

pub fn list(state: &AppState) -> Result<CommonPromptState, AppError> {
    load_state(&state.db)
}

pub fn upsert(state: &AppState, preset: CommonPreset) -> Result<bool, AppError> {
    if preset.id.trim().is_empty() {
        return Err(AppError::InvalidInput("公共预设 id 不能为空".to_string()));
    }
    let mut current = load_state(&state.db)?;
    let existing = current.presets.iter().position(|p| p.id == preset.id);
    let is_active = current.active_id.as_deref() == Some(preset.id.as_str());
    match existing {
        Some(index) => current.presets[index] = preset,
        None => current.presets.push(preset),
    }
    save_state(&state.db, &current)?;
    if is_active {
        reapply_common_to_all_apps(state)?;
    }
    Ok(existing.is_some())
}

pub fn delete(state: &AppState, id: &str) -> Result<bool, AppError> {
    let mut current = load_state(&state.db)?;
    let before = current.presets.len();
    current.presets.retain(|p| p.id != id);
    if current.presets.len() == before {
        return Ok(false);
    }
    let was_active = current.active_id.as_deref() == Some(id);
    if was_active {
        current.active_id = None;
    }
    save_state(&state.db, &current)?;
    if was_active {
        reapply_common_to_all_apps(state)?;
    }
    Ok(true)
}

/// Activate one preset and repaint every live file. Only one preset can be
/// active at a time.
pub fn use_preset(state: &AppState, id: &str) -> Result<usize, AppError> {
    let mut current = load_state(&state.db)?;
    if !current.presets.iter().any(|p| p.id == id) {
        return Err(AppError::InvalidInput(format!("公共预设 {id} 不存在")));
    }
    current.active_id = Some(id.to_string());
    save_state(&state.db, &current)?;
    reapply_common_to_all_apps(state)
}

/// Deactivate the preamble (marker sections are removed from live files).
pub fn deactivate(state: &AppState) -> Result<usize, AppError> {
    let mut current = load_state(&state.db)?;
    if current.active_id.take().is_none() {
        return Ok(0);
    }
    save_state(&state.db, &current)?;
    reapply_common_to_all_apps(state)
}

// ---------------------------------------------------------------------------
// Integration with the per-app prompt write paths
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_inserts_replaces_and_removes() {
        let doc = "# private\n\ncontent";
        assert_eq!(
            apply_common_preamble(doc, Some("# common")),
            format!("{}\n\n{doc}", marker_section("# common"))
        );

        let with_section = apply_common_preamble(doc, Some("# common"));
        assert_eq!(
            apply_common_preamble(&with_section, Some("# other")),
            format!("{}\n\n{doc}", marker_section("# other"))
        );

        assert_eq!(apply_common_preamble(&with_section, None), doc);
        assert_eq!(apply_common_preamble(doc, None), doc);
        assert_eq!(apply_common_preamble(doc, Some("   ")), doc);
    }

    #[test]
    fn strip_removes_marker_section_only() {
        let doc = "# private tail";
        let with_section = apply_common_preamble(doc, Some("# common"));
        assert_eq!(strip_common_section(&with_section), doc);
        assert_eq!(strip_common_section(doc), doc);
    }

    #[test]
    fn empty_document_gets_section_without_padding() {
        let rendered = apply_common_preamble("", Some("# common"));
        assert_eq!(rendered, marker_section("# common"));
        assert_eq!(apply_common_preamble(&rendered, None), "");
    }
}

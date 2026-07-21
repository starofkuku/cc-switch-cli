//! Pi coding agent session scanner.
//!
//! Layout:
//! `~/.pi/agent/sessions/<encoded-cwd>/<timestamp>_<uuid>.jsonl`
//! (or `$PI_CODING_AGENT_DIR/sessions/...`)

use std::ops::ControlFlow;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::session_manager::cache::{self, FileScanTarget};
use crate::session_manager::scan_cache_store::ScanCacheStore;
use crate::session_manager::{
    SearchSnippet, SessionMessage, SessionMessageBatch, SessionMessageBatchBuilder, SessionMeta,
    SessionSearchHit,
};

use super::utils::{
    build_snippet_cancellable, extract_text, file_modified_ms, parse_timestamp_to_ms,
    read_head_tail_lines_bounded, truncate_summary, visit_bounded_lines_cancellable,
    visit_bounded_lines_cancellable_with_status, TITLE_MAX_CHARS,
};

const PROVIDER_ID: &str = "pi";

fn sessions_root() -> PathBuf {
    crate::pi_config::get_pi_dir().join("sessions")
}

pub fn scan_sessions() -> Vec<SessionMeta> {
    let root = sessions_root();
    let mut files = Vec::new();
    collect_jsonl_files(&root, &mut files);
    super::utils::parse_sessions_parallel(files, parse_session)
}

pub(crate) fn scan_sessions_cached(store: &ScanCacheStore, force: bool) -> Vec<SessionMeta> {
    cache::scan_provider_cached(
        store,
        PROVIDER_ID,
        scan_targets(),
        force,
        parse_session,
        |_| true,
    )
}

pub(crate) fn scan_sessions_progressive(
    store: Option<&ScanCacheStore>,
    force: bool,
    on_session: &mut dyn FnMut(&SessionMeta),
) -> Vec<SessionMeta> {
    let targets = scan_targets();
    match store {
        Some(store) => cache::scan_provider_cached_progressive(
            store,
            PROVIDER_ID,
            targets,
            force,
            parse_session,
            |_| true,
            on_session,
        ),
        None => cache::scan_provider_uncached_progressive(targets, parse_session, on_session),
    }
}

pub(crate) fn scan_sessions_progressive_cancellable(
    store: Option<&ScanCacheStore>,
    force: bool,
    on_session: &mut dyn FnMut(&SessionMeta),
    is_cancelled: &(dyn Fn() -> bool + Sync),
) -> Option<Vec<SessionMeta>> {
    let targets = scan_targets_cancellable(is_cancelled)?;
    match store {
        Some(store) => cache::scan_provider_cached_progressive_cancellable(
            store,
            PROVIDER_ID,
            targets,
            force,
            parse_session,
            |_| true,
            on_session,
            is_cancelled,
        ),
        None => cache::scan_provider_uncached_progressive_cancellable(
            targets,
            parse_session,
            on_session,
            is_cancelled,
        ),
    }
}

pub(crate) fn stream_sessions_cancellable(
    store: Option<&ScanCacheStore>,
    force: bool,
    on_session: &mut dyn FnMut(SessionMeta) -> ControlFlow<()>,
    is_cancelled: &(dyn Fn() -> bool + Sync),
) -> Result<cache::StreamScanStats, cache::StreamScanStop> {
    let root = sessions_root();
    cache::stream_file_provider_cancellable(
        store,
        PROVIDER_ID,
        force,
        |path| {
            if is_cancelled() {
                return Err(cache::StreamScanStop::Cancelled);
            }
            let meta = parse_session_authoritative(path)?;
            if is_cancelled() {
                Err(cache::StreamScanStop::Cancelled)
            } else {
                Ok(meta)
            }
        },
        |_| true,
        cache::stat_target,
        move |on_target, cancel| {
            cache::visit_targets_recursive_cancellable(&root, "jsonl", on_target, cancel)
        },
        on_session,
        is_cancelled,
    )
}

fn scan_targets() -> Vec<FileScanTarget> {
    scan_targets_cancellable(&|| false).expect("non-cancellable target scan cannot stop")
}

fn scan_targets_cancellable(
    is_cancelled: &(dyn Fn() -> bool + Sync),
) -> Option<Vec<FileScanTarget>> {
    let root = sessions_root();
    let mut targets = Vec::new();
    if !cache::collect_targets_recursive_cancellable(&root, "jsonl", &mut targets, is_cancelled) {
        return None;
    }
    Some(targets)
}

pub fn load_messages(path: &Path) -> Result<SessionMessageBatch, String> {
    load_messages_cancellable(path, &|| false)
}

pub(crate) fn load_messages_cancellable(
    path: &Path,
    is_cancelled: &(dyn Fn() -> bool + Sync),
) -> Result<SessionMessageBatch, String> {
    let mut batch = SessionMessageBatchBuilder::new();
    let status = visit_bounded_lines_cancellable_with_status(path, is_cancelled, &mut |line| {
        let value: Value = match serde_json::from_str(line) {
            Ok(parsed) => parsed,
            Err(_) => return ControlFlow::Continue(()),
        };

        if value.get("type").and_then(Value::as_str) != Some("message") {
            return ControlFlow::Continue(());
        }

        let message = match value.get("message") {
            Some(msg) => msg,
            None => return ControlFlow::Continue(()),
        };

        let raw_role = message
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let role = match raw_role {
            "toolResult" => "tool".to_string(),
            other => other.to_string(),
        };

        let content = message.get("content").map(extract_text).unwrap_or_default();
        if content.trim().is_empty() {
            return ControlFlow::Continue(());
        }

        let ts = value
            .get("timestamp")
            .and_then(parse_timestamp_to_ms)
            .or_else(|| message.get("timestamp").and_then(parse_timestamp_to_ms));

        batch.push(SessionMessage { role, content, ts })
    })
    .map_err(|error| format!("Failed to read Pi session file: {error}"))?
    .ok_or_else(|| "Session message preview was cancelled".to_string())?;
    if status.oversized_record_skipped {
        batch.mark_truncated();
    }

    Ok(batch.finish())
}

#[allow(dead_code)]
pub fn search_session(meta: &SessionMeta, needle: &str) -> Option<SessionSearchHit> {
    search_session_cancellable(meta, needle, &|| false)
}

pub(crate) fn search_session_cancellable(
    meta: &SessionMeta,
    needle: &str,
    is_cancelled: &(dyn Fn() -> bool + Sync),
) -> Option<SessionSearchHit> {
    if is_cancelled() {
        return None;
    }
    let source_path = meta.source_path.as_deref()?;
    let path = Path::new(source_path);
    let mut snippets: Vec<SearchSnippet> = Vec::new();
    const MAX_SNIPPETS: usize = 5;

    let visited = visit_bounded_lines_cancellable(path, is_cancelled, &mut |line| {
        let value: Value = match serde_json::from_str(line) {
            Ok(value) => value,
            Err(_) => return ControlFlow::Continue(()),
        };
        if value.get("type").and_then(Value::as_str) != Some("message") {
            return ControlFlow::Continue(());
        }
        let message = match value.get("message") {
            Some(message) => message,
            None => return ControlFlow::Continue(()),
        };
        let raw_role = message
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let role = match raw_role {
            "toolResult" => "tool".to_string(),
            other => other.to_string(),
        };
        let content = message.get("content").map(extract_text).unwrap_or_default();
        if content.trim().is_empty() {
            return ControlFlow::Continue(());
        }
        match build_snippet_cancellable(&content, needle, is_cancelled) {
            Ok(Some(snippet)) => {
                snippets.push(SearchSnippet { role, snippet });
                if snippets.len() >= MAX_SNIPPETS {
                    return ControlFlow::Break(());
                }
            }
            Ok(None) => {}
            Err(_) => return ControlFlow::Break(()),
        }
        ControlFlow::Continue(())
    })
    .ok()??;
    let _ = visited;
    if is_cancelled() || snippets.is_empty() {
        return None;
    }
    Some(SessionSearchHit {
        provider_id: PROVIDER_ID.to_string(),
        session_id: meta.session_id.clone(),
        source_path: source_path.to_string(),
        snippets,
    })
}

pub fn delete_session(_root: &Path, path: &Path, session_id: &str) -> Result<bool, String> {
    let meta = parse_session(path)
        .ok_or_else(|| format!("Failed to parse Pi session metadata: {}", path.display()))?;
    if meta.session_id != session_id {
        return Err(format!(
            "Pi session ID mismatch: expected {session_id}, found {}",
            meta.session_id
        ));
    }
    std::fs::remove_file(path)
        .map_err(|e| format!("Failed to delete Pi session file {}: {e}", path.display()))?;
    Ok(true)
}

fn parse_session(path: &Path) -> Option<SessionMeta> {
    parse_session_authoritative(path).ok().flatten()
}

fn parse_session_authoritative(path: &Path) -> Result<Option<SessionMeta>, cache::StreamScanStop> {
    let (head, tail) = read_head_tail_lines_bounded(path, 15, 40).map_err(|error| {
        log::warn!(
            "authoritative Pi metadata read failed at {}: {error}",
            path.display()
        );
        cache::StreamScanStop::Incomplete
    })?;
    Ok(parse_session_lines(path, &head, &tail))
}

fn parse_session_lines(path: &Path, head: &[String], tail: &[String]) -> Option<SessionMeta> {
    let mut session_id: Option<String> = None;
    let mut project_dir: Option<String> = None;
    let mut created_at: Option<i64> = None;
    let mut first_user: Option<String> = None;
    let mut custom_title: Option<String> = None;

    for line in head {
        let value: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if value.get("type").and_then(Value::as_str) == Some("session") {
            if session_id.is_none() {
                session_id = value.get("id").and_then(Value::as_str).map(str::to_string);
            }
            if project_dir.is_none() {
                project_dir = value.get("cwd").and_then(Value::as_str).map(str::to_string);
            }
            if created_at.is_none() {
                created_at = value.get("timestamp").and_then(parse_timestamp_to_ms);
            }
            if custom_title.is_none() {
                custom_title = value
                    .get("title")
                    .or_else(|| value.get("name"))
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string);
            }
        }
        if first_user.is_none() && value.get("type").and_then(Value::as_str) == Some("message") {
            if let Some(message) = value.get("message") {
                if message.get("role").and_then(Value::as_str) == Some("user") {
                    let text = message.get("content").map(extract_text).unwrap_or_default();
                    let trimmed = text.trim();
                    if !trimmed.is_empty() {
                        first_user = Some(trimmed.to_string());
                    }
                }
            }
        }
    }

    let mut last_active_at: Option<i64> = None;
    for line in tail.iter().rev() {
        let value: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if last_active_at.is_none() {
            last_active_at = value.get("timestamp").and_then(parse_timestamp_to_ms);
        }
        if last_active_at.is_some() {
            break;
        }
    }

    let session_id = session_id.or_else(|| infer_session_id_from_filename(path))?;
    let fallback_time = file_modified_ms(path);
    // Prefer an explicit session name only. Export UI falls back to the last
    // user message when `title` is empty.
    let title = custom_title.map(|t| truncate_summary(&t, TITLE_MAX_CHARS));
    let summary = first_user.map(|t| truncate_summary(&t, 160));

    Some(SessionMeta {
        provider_id: PROVIDER_ID.to_string(),
        session_id: session_id.clone(),
        title,
        summary,
        project_dir,
        created_at: created_at.or(fallback_time),
        last_active_at: last_active_at.or(fallback_time).or(created_at),
        source_path: Some(path.to_string_lossy().to_string()),
        resume_command: None,
    })
}

fn infer_session_id_from_filename(path: &Path) -> Option<String> {
    let stem = path.file_stem()?.to_str()?;
    // Files look like: 2026-07-17T16-29-38-798Z_019f70e9-...
    if let Some((_, id)) = stem.rsplit_once('_') {
        if !id.is_empty() {
            return Some(id.to_string());
        }
    }
    Some(stem.to_string())
}

fn collect_jsonl_files(root: &Path, files: &mut Vec<PathBuf>) {
    if !root.exists() {
        return;
    }
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_jsonl_files(&path, files);
        } else if path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
            files.push(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn parse_session_uses_session_id_and_first_user() {
        let temp = tempdir().expect("tempdir");
        let path = temp
            .path()
            .join("2026-07-17T16-29-38-798Z_abc-session-id.jsonl");
        std::fs::write(
            &path,
            concat!(
                r#"{"type":"session","version":3,"id":"abc-session-id","timestamp":"2026-07-17T16:29:38.798Z","cwd":"/tmp/proj"}"#,
                "\n",
                r#"{"type":"message","timestamp":"2026-07-17T16:29:45Z","message":{"role":"user","content":[{"type":"text","text":"hello pi"}]}}"#,
                "\n",
                r#"{"type":"message","timestamp":"2026-07-17T16:29:46Z","message":{"role":"assistant","content":[{"type":"text","text":"hi"}]}}"#,
                "\n",
            ),
        )
        .expect("write");

        let meta = parse_session(&path).expect("parse");
        assert_eq!(meta.session_id, "abc-session-id");
        assert_eq!(meta.project_dir.as_deref(), Some("/tmp/proj"));
        assert!(meta.title.is_none(), "no explicit name on session header");
        assert_eq!(meta.summary.as_deref(), Some("hello pi"));

        let msgs = load_messages(&path).expect("load");
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].role, "user");
        assert_eq!(msgs[1].role, "assistant");
    }
}

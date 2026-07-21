//! Grok Build session scanner.
//!
//! Layout:
//! `$GROK_HOME/sessions/<url-encoded-cwd>/<session-id>/`
//!   summary.json
//!   chat_history.jsonl
//!   updates.jsonl
//!   ...

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
    truncate_summary, visit_bounded_lines_cancellable, visit_bounded_lines_cancellable_with_status,
    TITLE_MAX_CHARS,
};

const PROVIDER_ID: &str = "grok";

fn sessions_root() -> PathBuf {
    crate::grok_config::get_grok_dir().join("sessions")
}

pub fn scan_sessions() -> Vec<SessionMeta> {
    let root = sessions_root();
    let mut dirs = Vec::new();
    collect_session_dirs(&root, &mut dirs);
    super::utils::parse_sessions_parallel(dirs, parse_session_dir)
}

pub(crate) fn scan_sessions_cached(store: &ScanCacheStore, force: bool) -> Vec<SessionMeta> {
    cache::scan_provider_cached(
        store,
        PROVIDER_ID,
        scan_targets(),
        force,
        parse_session_from_summary_path,
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
            parse_session_from_summary_path,
            |_| true,
            on_session,
        ),
        None => cache::scan_provider_uncached_progressive(
            targets,
            parse_session_from_summary_path,
            on_session,
        ),
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
            parse_session_from_summary_path,
            |_| true,
            on_session,
            is_cancelled,
        ),
        None => cache::scan_provider_uncached_progressive_cancellable(
            targets,
            parse_session_from_summary_path,
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
            Ok(parse_session_from_summary_path(path))
        },
        |_| true,
        cache::stat_target,
        move |on_target, cancel| visit_summary_targets(&root, on_target, cancel),
        on_session,
        is_cancelled,
    )
}

fn visit_summary_targets(
    root: &Path,
    on_target: &mut dyn FnMut(FileScanTarget) -> Result<(), cache::StreamScanStop>,
    is_cancelled: &(dyn Fn() -> bool + Sync),
) -> Result<(), cache::StreamScanStop> {
    if is_cancelled() {
        return Err(cache::StreamScanStop::Cancelled);
    }
    if !root.exists() {
        return Ok(());
    }
    let cwd_dirs = match std::fs::read_dir(root) {
        Ok(d) => d,
        Err(_) => return Ok(()),
    };
    for cwd_entry in cwd_dirs.flatten() {
        if is_cancelled() {
            return Err(cache::StreamScanStop::Cancelled);
        }
        let cwd_path = cwd_entry.path();
        if !cwd_path.is_dir() {
            continue;
        }
        let session_dirs = match std::fs::read_dir(&cwd_path) {
            Ok(d) => d,
            Err(_) => continue,
        };
        for session_entry in session_dirs.flatten() {
            if is_cancelled() {
                return Err(cache::StreamScanStop::Cancelled);
            }
            let session_path = session_entry.path();
            if !session_path.is_dir() {
                continue;
            }
            let summary = session_path.join("summary.json");
            if !summary.is_file() {
                continue;
            }
            if let Some(target) = cache::stat_target(&summary) {
                on_target(target)?;
            }
        }
    }
    Ok(())
}

fn scan_targets() -> Vec<FileScanTarget> {
    scan_targets_cancellable(&|| false).expect("non-cancellable")
}

fn scan_targets_cancellable(
    is_cancelled: &(dyn Fn() -> bool + Sync),
) -> Option<Vec<FileScanTarget>> {
    let root = sessions_root();
    let mut targets = Vec::new();
    if is_cancelled() {
        return None;
    }
    if !root.exists() {
        return Some(targets);
    }
    let cwd_dirs = match std::fs::read_dir(&root) {
        Ok(d) => d,
        Err(_) => return Some(targets),
    };
    for cwd_entry in cwd_dirs.flatten() {
        if is_cancelled() {
            return None;
        }
        let cwd_path = cwd_entry.path();
        if !cwd_path.is_dir() {
            continue;
        }
        let session_dirs = match std::fs::read_dir(&cwd_path) {
            Ok(d) => d,
            Err(_) => continue,
        };
        for session_entry in session_dirs.flatten() {
            if is_cancelled() {
                return None;
            }
            let session_path = session_entry.path();
            if !session_path.is_dir() {
                continue;
            }
            let summary = session_path.join("summary.json");
            if summary.is_file() {
                if let Some(target) = cache::stat_target(&summary) {
                    targets.push(target);
                }
            }
        }
    }
    Some(targets)
}

fn parse_session_from_summary_path(summary_path: &Path) -> Option<SessionMeta> {
    let dir = summary_path.parent()?;
    parse_session_dir(dir)
}

fn parse_session_dir(dir: &Path) -> Option<SessionMeta> {
    let summary_path = dir.join("summary.json");
    if !summary_path.is_file() {
        return None;
    }
    let content = std::fs::read_to_string(&summary_path).ok()?;
    let value: Value = serde_json::from_str(&content).ok()?;

    let info = value.get("info");
    let session_id = info
        .and_then(|i| i.get("id"))
        .and_then(Value::as_str)
        .or_else(|| dir.file_name().and_then(|n| n.to_str()))
        .map(str::to_string)?;

    let project_dir = info
        .and_then(|i| i.get("cwd"))
        .and_then(Value::as_str)
        .map(str::to_string);

    let named_title = value
        .get("generated_title")
        .or_else(|| value.get("session_summary"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| truncate_summary(s, TITLE_MAX_CHARS));

    let created_at = value
        .get("created_at")
        .and_then(parse_timestamp_to_ms)
        .or_else(|| file_modified_ms(&summary_path));
    let last_active_at = value
        .get("last_active_at")
        .or_else(|| value.get("updated_at"))
        .and_then(parse_timestamp_to_ms)
        .or(created_at);

    // Only store agent-generated/session names here. Export UI falls back to
    // the last user message when `title` is absent.
    let title = named_title;

    Some(SessionMeta {
        provider_id: PROVIDER_ID.to_string(),
        session_id: session_id.clone(),
        title,
        summary: value
            .get("session_summary")
            .and_then(Value::as_str)
            .map(|s| truncate_summary(s, 160)),
        project_dir,
        created_at,
        last_active_at,
        // Point at session directory so load/delete operate on the folder.
        source_path: Some(dir.to_string_lossy().to_string()),
        resume_command: Some(format!("grok --resume {session_id}")),
    })
}

fn collect_session_dirs(root: &Path, out: &mut Vec<PathBuf>) {
    if !root.exists() {
        return;
    }
    let Ok(cwd_dirs) = std::fs::read_dir(root) else {
        return;
    };
    for cwd_entry in cwd_dirs.flatten() {
        let cwd_path = cwd_entry.path();
        if !cwd_path.is_dir() {
            continue;
        }
        let Ok(session_dirs) = std::fs::read_dir(&cwd_path) else {
            continue;
        };
        for session_entry in session_dirs.flatten() {
            let session_path = session_entry.path();
            if session_path.is_dir() && session_path.join("summary.json").is_file() {
                out.push(session_path);
            }
        }
    }
}

fn chat_history_path(source: &Path) -> PathBuf {
    if source.is_dir() {
        source.join("chat_history.jsonl")
    } else if source.file_name().and_then(|n| n.to_str()) == Some("summary.json") {
        source.parent().unwrap_or(source).join("chat_history.jsonl")
    } else {
        source.to_path_buf()
    }
}

pub fn load_messages(path: &Path) -> Result<SessionMessageBatch, String> {
    load_messages_cancellable(path, &|| false)
}

pub(crate) fn load_messages_cancellable(
    path: &Path,
    is_cancelled: &(dyn Fn() -> bool + Sync),
) -> Result<SessionMessageBatch, String> {
    let history = chat_history_path(path);
    if !history.is_file() {
        return Err(format!(
            "Grok chat_history.jsonl not found under {}",
            path.display()
        ));
    }

    let mut batch = SessionMessageBatchBuilder::new();
    let status = visit_bounded_lines_cancellable_with_status(&history, is_cancelled, &mut |line| {
        let value: Value = match serde_json::from_str(line) {
            Ok(parsed) => parsed,
            Err(_) => return ControlFlow::Continue(()),
        };

        let Some(role) = grok_message_role(&value) else {
            return ControlFlow::Continue(());
        };

        let content = value.get("content").map(extract_text).unwrap_or_default();
        if content.trim().is_empty() {
            return ControlFlow::Continue(());
        }

        // Skip Grok harness noise (system prompts injected as user turns).
        if role == "user" && is_grok_synthetic_user_content(&content) {
            return ControlFlow::Continue(());
        }

        let ts = value.get("timestamp").and_then(parse_timestamp_to_ms);
        batch.push(SessionMessage {
            role: role.to_string(),
            content,
            ts,
        })
    })
    .map_err(|error| format!("Failed to read Grok chat_history: {error}"))?
    .ok_or_else(|| "Session message preview was cancelled".to_string())?;
    if status.oversized_record_skipped {
        batch.mark_truncated();
    }

    Ok(batch.finish())
}

/// Grok chat_history lines use `type` (newer) or `role` (older) for the speaker.
fn grok_message_role(value: &Value) -> Option<&'static str> {
    let raw = value
        .get("role")
        .and_then(Value::as_str)
        .or_else(|| value.get("type").and_then(Value::as_str))
        .unwrap_or("");
    match raw {
        "user" => Some("user"),
        "assistant" => Some("assistant"),
        _ => None,
    }
}

fn is_grok_synthetic_user_content(content: &str) -> bool {
    let t = content.trim_start();
    // Injected context blocks, not real user prompts.
    t.starts_with("<user_info>")
        || t.starts_with("<system-reminder>")
        || t.starts_with("<git_status>")
        || t.contains("synthetic_reason")
        || (t.starts_with('<') && t.contains("</system-reminder>") && !t.contains("<user_query>"))
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
    let history = chat_history_path(Path::new(source_path));
    if !history.is_file() {
        return None;
    }
    let mut snippets: Vec<SearchSnippet> = Vec::new();
    const MAX_SNIPPETS: usize = 5;

    let visited = visit_bounded_lines_cancellable(&history, is_cancelled, &mut |line| {
        let value: Value = match serde_json::from_str(line) {
            Ok(value) => value,
            Err(_) => return ControlFlow::Continue(()),
        };
        let Some(role) = grok_message_role(&value) else {
            return ControlFlow::Continue(());
        };
        let content = value.get("content").map(extract_text).unwrap_or_default();
        if content.trim().is_empty() {
            return ControlFlow::Continue(());
        }
        if role == "user" && is_grok_synthetic_user_content(&content) {
            return ControlFlow::Continue(());
        }
        match build_snippet_cancellable(&content, needle, is_cancelled) {
            Ok(Some(snippet)) => {
                snippets.push(SearchSnippet {
                    role: role.to_string(),
                    snippet,
                });
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
    let dir = if path.is_dir() {
        path.to_path_buf()
    } else {
        path.parent()
            .ok_or_else(|| "Invalid Grok session path".to_string())?
            .to_path_buf()
    };
    let meta = parse_session_dir(&dir)
        .ok_or_else(|| format!("Failed to parse Grok session metadata: {}", dir.display()))?;
    if meta.session_id != session_id {
        return Err(format!(
            "Grok session ID mismatch: expected {session_id}, found {}",
            meta.session_id
        ));
    }
    std::fs::remove_dir_all(&dir).map_err(|e| {
        format!(
            "Failed to delete Grok session directory {}: {e}",
            dir.display()
        )
    })?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn parse_and_load_grok_session_dir() {
        let temp = tempdir().expect("tempdir");
        let session_dir = temp.path().join("%2Ftmp%2Fproj").join("sess-123");
        std::fs::create_dir_all(&session_dir).expect("mkdir");
        std::fs::write(
            session_dir.join("summary.json"),
            r#"{
              "info": {"id": "sess-123", "cwd": "/tmp/proj"},
              "generated_title": "My Named Session",
              "session_summary": "My Named Session",
              "created_at": "2026-07-20T10:00:00Z",
              "updated_at": "2026-07-21T10:00:00Z"
            }"#,
        )
        .expect("summary");
        std::fs::write(
            session_dir.join("chat_history.jsonl"),
            concat!(
                // Newer Grok format uses `type` instead of `role`.
                r#"{"type":"system","content":"sys"}"#,
                "\n",
                r#"{"type":"user","content":[{"type":"text","text":"<user_info>\nOS Version: linux\n</user_info>"}]}"#,
                "\n",
                r#"{"type":"user","content":[{"type":"text","text":"<user_query>\nhello grok\n</user_query>"}]}"#,
                "\n",
                r#"{"type":"assistant","content":[{"type":"text","text":"hi there"}]}"#,
                "\n",
                r#"{"type":"reasoning","content":"think"}"#,
                "\n",
                // Older format still supported.
                r#"{"role":"user","content":"legacy hello"}"#,
                "\n",
            ),
        )
        .expect("history");

        let meta = parse_session_dir(&session_dir).expect("parse");
        assert_eq!(meta.session_id, "sess-123");
        assert_eq!(meta.title.as_deref(), Some("My Named Session"));
        assert_eq!(meta.project_dir.as_deref(), Some("/tmp/proj"));
        assert_eq!(meta.provider_id, "grok");

        let msgs = load_messages(&session_dir).expect("load");
        assert_eq!(msgs.len(), 3);
        assert_eq!(msgs[0].role, "user");
        assert!(msgs[0].content.contains("hello grok"));
        assert_eq!(msgs[1].role, "assistant");
        assert_eq!(msgs[1].content, "hi there");
        assert_eq!(msgs[2].role, "user");
        assert_eq!(msgs[2].content, "legacy hello");
    }
}

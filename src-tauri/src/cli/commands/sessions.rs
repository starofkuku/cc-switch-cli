use chrono::{Local, TimeZone, Utc};
use clap::Subcommand;
use colored::Colorize;
use crossterm::cursor::{Hide, MoveTo, Show};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::style::{Print, ResetColor, SetForegroundColor, Color as CtColor};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::{execute, queue};
use serde::ser::SerializeSeq;
use serde::Serialize;
use serde::Serializer as _;
use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::Command;
use std::str::FromStr;
use std::time::Duration;

use crate::app_config::AppType;
use crate::cli::ui::{create_table, info, success, to_json, warning};
use crate::database::Database;
use crate::error::AppError;
use crate::services::session_usage::SessionSyncResult;
use crate::session_manager::{self, SessionMessage, SessionMessageBatch, SessionMeta};

#[derive(Subcommand, Debug, Clone)]
pub enum SessionsCommand {
    /// List saved assistant sessions
    List {
        /// Scan a specific provider instead of --app
        #[arg(long, value_parser = parse_session_provider)]
        provider: Option<AppType>,
        /// Scan every supported app instead of the selected app
        #[arg(long)]
        all: bool,
        /// Print machine-readable JSON
        #[arg(long)]
        json: bool,
    },
    /// Show session metadata and messages
    Show {
        /// Session id or unique id prefix. Also accepts provider/id.
        selector: String,
        /// Resolve against a specific provider instead of --app
        #[arg(long, value_parser = parse_session_provider)]
        provider: Option<AppType>,
        /// Search every supported app
        #[arg(long)]
        all: bool,
        /// Print machine-readable JSON
        #[arg(long)]
        json: bool,
    },
    /// Print messages for a session
    Messages {
        /// Session id or unique id prefix. Also accepts provider/id.
        selector: String,
        /// Resolve against a specific provider instead of --app
        #[arg(long, value_parser = parse_session_provider)]
        provider: Option<AppType>,
        /// Search every supported app
        #[arg(long)]
        all: bool,
        /// Print machine-readable JSON
        #[arg(long)]
        json: bool,
    },
    /// Resume a session using its saved resume command
    Resume {
        /// Session id or unique id prefix. Also accepts provider/id.
        selector: String,
        /// Resolve against a specific provider instead of --app
        #[arg(long, value_parser = parse_session_provider)]
        provider: Option<AppType>,
        /// Search every supported app
        #[arg(long)]
        all: bool,
        /// Print the saved command instead of executing it
        #[arg(long)]
        print: bool,
    },
    /// Delete a saved session
    Delete {
        /// Session id or unique id prefix. Also accepts provider/id.
        selector: String,
        /// Resolve against a specific provider instead of --app
        #[arg(long, value_parser = parse_session_provider)]
        provider: Option<AppType>,
        /// Search every supported app
        #[arg(long)]
        all: bool,
        /// Confirm deletion without prompting
        #[arg(long)]
        yes: bool,
    },
    /// Search full conversation content across all sessions
    Search {
        /// Search query (case-insensitive, matches message content)
        query: String,
        /// Print machine-readable JSON
        #[arg(long)]
        json: bool,
    },
    /// Sync local session logs into usage statistics
    SyncUsage {
        /// Sync a specific provider instead of --app
        #[arg(long, value_parser = parse_session_provider)]
        provider: Option<AppType>,
        /// Sync every supported provider
        #[arg(long)]
        all: bool,
        /// Print machine-readable JSON
        #[arg(long)]
        json: bool,
    },
    /// Export one session to a JSON file (requires --app)
    Export {
        /// Session id or unique id prefix (skips interactive picker when set)
        #[arg(long = "id")]
        id: Option<String>,
        /// Output path (default: ./ccswitch-<app>-<id>-<date>.json)
        #[arg(short = 'o', long = "output")]
        output: Option<PathBuf>,
    },
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SessionDetail {
    session: SessionMeta,
    #[serde(skip_serializing_if = "Option::is_none")]
    messages: Option<Vec<SessionMessage>>,
    messages_truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    messages_error: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SessionMessagesOutput<'a> {
    messages: &'a [SessionMessage],
    messages_truncated: bool,
}

pub fn execute(cmd: SessionsCommand, app: Option<AppType>) -> Result<(), AppError> {
    match cmd {
        SessionsCommand::List {
            provider,
            all,
            json,
        } => list_sessions(app, provider, all, json),
        SessionsCommand::Show {
            selector,
            provider,
            all,
            json,
        } => show_session(app, provider, all, &selector, json),
        SessionsCommand::Messages {
            selector,
            provider,
            all,
            json,
        } => print_messages(app, provider, all, &selector, json),
        SessionsCommand::Resume {
            selector,
            provider,
            all,
            print,
        } => resume_session(app, provider, all, &selector, print),
        SessionsCommand::Delete {
            selector,
            provider,
            all,
            yes,
        } => delete_session(app, provider, all, &selector, yes),
        SessionsCommand::Search { query, json } => search_sessions_cmd(&query, json),
        SessionsCommand::SyncUsage {
            provider,
            all,
            json,
        } => sync_usage(app, provider, all, json),
        SessionsCommand::Export { id, output } => export_session(app, id, output),
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SessionExportDocument {
    format: &'static str,
    version: u32,
    exported_at: String,
    app: String,
    session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    project_dir: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    source_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    created_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_active_at: Option<i64>,
    messages: Vec<SessionExportMessage>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SessionExportMessage {
    role: String,
    content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    ts: Option<i64>,
}

struct ExportChoice {
    session: SessionMeta,
    label: String,
    /// user/assistant messages already loaded for list labels and Ctrl+E preview.
    messages: Vec<SessionMessage>,
    messages_truncated: bool,
}

fn export_session(
    app: Option<AppType>,
    id: Option<String>,
    output: Option<PathBuf>,
) -> Result<(), AppError> {
    let app = app.ok_or_else(|| {
        AppError::InvalidInput(
            "sessions export requires --app (e.g. --app claude). There is no default app."
                .to_string(),
        )
    })?;
    let provider_id = app.as_str();

    if !supports_session_export(provider_id) {
        return Err(AppError::InvalidInput(format!(
            "Session export is not supported for app '{provider_id}'. Allowed: {}",
            export_supported_apps_hint()
        )));
    }

    println!(
        "{}",
        format!("Scanning {provider_id} sessions…").bright_cyan()
    );
    let mut sessions = session_manager::scan_sessions_for_provider(provider_id);
    if sessions.is_empty() {
        println!("{}", info(&format!("No sessions found for {provider_id}.")));
        return Ok(());
    }
    session_manager::sort_by_recent(&mut sessions);

    let selected = if let Some(selector) = id.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        // Non-interactive: resolve by exact id or unique prefix, then load once.
        let session = resolve_session_from_list(&sessions, selector)?;
        load_export_choice(provider_id, session, true)?
    } else {
        // Interactive: load messages for labels/preview, then pick one.
        let mut choices = Vec::with_capacity(sessions.len());
        for session in sessions {
            // Soft-load so one unreadable session does not block the whole picker.
            choices.push(load_export_choice(provider_id, session, false)?);
        }
        let selected_index = prompt_export_session_picker(&choices)?;
        choices.into_iter().nth(selected_index).ok_or_else(|| {
            AppError::Message("Invalid session selection.".to_string())
        })?
    };

    write_export_document(provider_id, &selected, output)
}

fn load_export_choice(
    provider_id: &str,
    session: SessionMeta,
    strict: bool,
) -> Result<ExportChoice, AppError> {
    let batch = match session.source_path.as_deref() {
        Some(path) => match session_manager::load_messages(provider_id, path) {
            Ok(batch) => Some(batch),
            Err(err) if strict => {
                return Err(AppError::Message(format!(
                    "Failed to load messages for session '{}': {err}",
                    session.session_id
                )));
            }
            Err(_) => None,
        },
        None if strict => {
            return Err(AppError::Message(format!(
                "Session '{}' has no source path; cannot export.",
                session.session_id
            )));
        }
        None => None,
    };
    let messages_truncated = batch.as_ref().map(|b| b.truncated).unwrap_or(false);
    let messages: Vec<SessionMessage> = batch
        .map(|b| b.messages)
        .unwrap_or_default()
        .into_iter()
        .filter(|m| m.role == "user" || m.role == "assistant")
        .filter(|m| !m.content.trim().is_empty())
        .collect();
    let label = format_export_choice_label(&session, &messages);
    Ok(ExportChoice {
        session,
        label,
        messages,
        messages_truncated,
    })
}

fn write_export_document(
    provider_id: &str,
    selected: &ExportChoice,
    output: Option<PathBuf>,
) -> Result<(), AppError> {
    let export_messages: Vec<SessionExportMessage> = selected
        .messages
        .iter()
        .map(|m| SessionExportMessage {
            role: m.role.clone(),
            content: m.content.clone(),
            ts: m.ts,
        })
        .collect();

    let doc = SessionExportDocument {
        format: "ccswitch-session",
        version: 1,
        exported_at: Utc::now().to_rfc3339(),
        app: provider_id.to_string(),
        session_id: selected.session.session_id.clone(),
        title: selected.session.title.clone(),
        project_dir: selected.session.project_dir.clone(),
        source_path: selected.session.source_path.clone(),
        created_at: selected.session.created_at,
        last_active_at: selected.session.last_active_at,
        messages: export_messages,
    };

    let out_path = match output {
        Some(path) => path,
        None => default_export_path(provider_id, &selected.session.session_id),
    };

    if let Some(parent) = out_path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).map_err(|source| AppError::IoContext {
                context: format!("failed to create export directory {}", parent.display()),
                source,
            })?;
        }
    }

    let json =
        serde_json::to_string_pretty(&doc).map_err(|source| AppError::JsonSerialize { source })?;
    fs::write(&out_path, format!("{json}\n")).map_err(|source| AppError::IoContext {
        context: format!("failed to write export file {}", out_path.display()),
        source,
    })?;

    println!(
        "{}",
        success(&format!(
            "Exported {} message(s) from {} to {}",
            doc.messages.len(),
            selected.session.session_id,
            out_path.display()
        ))
    );
    if selected.messages_truncated {
        println!(
            "{}",
            warning(
                "Source message stream was truncated by the session reader; export may be incomplete."
            )
        );
    }
    Ok(())
}

fn resolve_session_from_list(
    sessions: &[SessionMeta],
    selector: &str,
) -> Result<SessionMeta, AppError> {
    let mut resolver = SessionSelectorResolver::new(selector)?;
    for session in sessions {
        resolver.push(session.clone());
    }
    resolver.finish()
}

fn supports_session_export(provider_id: &str) -> bool {
    matches!(
        provider_id,
        "claude" | "codex" | "gemini" | "opencode" | "openclaw" | "hermes" | "grok" | "pi"
    )
}

fn export_supported_apps_hint() -> &'static str {
    "claude, codex, gemini, opencode, openclaw, hermes, grok, pi"
}

fn default_export_path(app: &str, session_id: &str) -> PathBuf {
    let date = Local::now().format("%Y%m%d");
    let short_id: String = session_id.chars().take(8).collect();
    PathBuf::from(format!("ccswitch-{app}-{short_id}-{date}.json"))
}

/// Picker label: prefer a real session name; otherwise last user message (10 chars).
fn format_export_choice_label(session: &SessionMeta, messages: &[SessionMessage]) -> String {
    let first_user = messages
        .iter()
        .find(|m| m.role == "user")
        .map(|m| m.content.trim());
    let last_user = messages
        .iter()
        .rev()
        .find(|m| m.role == "user")
        .map(|m| m.content.trim());

    if let Some(title) = session
        .title
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
    {
        let looks_like_first_user = first_user.is_some_and(|fu| {
            let fu = fu.trim();
            if fu.is_empty() {
                return false;
            }
            fu.starts_with(title)
                || title.starts_with(&truncate_export_label_chars(fu, title.chars().count()))
        });
        // Grok/Pi only store explicit names in `title`. Other apps may put the
        // first user message there; prefer last-user preview in that case.
        if matches!(session.provider_id.as_str(), "grok" | "pi") || !looks_like_first_user {
            return collapse_whitespace(title);
        }
    }

    if let Some(text) = last_user.filter(|t| !t.is_empty()) {
        return collapse_whitespace(&truncate_export_label_chars(text, 10));
    }

    format!("({})", short_session_id(&session.session_id))
}

fn truncate_export_label_chars(text: &str, max_chars: usize) -> String {
    let mut out = String::new();
    for (i, ch) in text.chars().enumerate() {
        if i >= max_chars {
            out.push('…');
            break;
        }
        out.push(ch);
    }
    out
}

fn collapse_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Interactive session picker with Ctrl+E conversation preview.
///
/// Keys:
/// - ↑/↓ or j/k: move selection (or scroll preview when expanded)
/// - Enter: confirm
/// - Ctrl+E: expand/collapse conversation for the highlighted session
/// - Esc: collapse if expanded, otherwise cancel
fn prompt_export_session_picker(choices: &[ExportChoice]) -> Result<usize, AppError> {
    if choices.is_empty() {
        return Err(AppError::Message("No sessions available to export.".into()));
    }

    struct RawModeGuard;
    impl Drop for RawModeGuard {
        fn drop(&mut self) {
            let mut stdout = io::stdout();
            let _ = disable_raw_mode();
            let _ = execute!(stdout, Show, LeaveAlternateScreen);
        }
    }

    enable_raw_mode().map_err(|e| AppError::Message(format!("failed to enable raw mode: {e}")))?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, Hide)
        .map_err(|e| AppError::Message(format!("failed to enter alternate screen: {e}")))?;
    let _guard = RawModeGuard;

    let mut selected: usize = 0;
    let mut list_scroll: usize = 0;
    let mut expanded = false;
    let mut preview_scroll: usize = 0;

    loop {
        let (term_cols, term_rows) = crossterm::terminal::size().unwrap_or((80, 24));
        let term_cols = term_cols as usize;
        let term_rows = term_rows.max(8) as usize;

        draw_export_picker(
            &mut stdout,
            choices,
            selected,
            list_scroll,
            expanded,
            preview_scroll,
            term_cols,
            term_rows,
        )?;

        if !event::poll(Duration::from_millis(250)).map_err(|e| {
            AppError::Message(format!("failed to poll keyboard events: {e}"))
        })? {
            continue;
        }
        let Event::Key(key) = event::read()
            .map_err(|e| AppError::Message(format!("failed to read keyboard event: {e}")))?
        else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }

        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

        match key.code {
            KeyCode::Char('c') if ctrl => {
                return Err(AppError::Message("Session selection cancelled.".into()));
            }
            KeyCode::Char('e') if ctrl => {
                expanded = !expanded;
                preview_scroll = 0;
            }
            KeyCode::Esc => {
                if expanded {
                    expanded = false;
                    preview_scroll = 0;
                } else {
                    return Err(AppError::Message("Session selection cancelled.".into()));
                }
            }
            KeyCode::Enter => {
                return Ok(selected);
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if expanded {
                    preview_scroll = preview_scroll.saturating_sub(1);
                } else if selected > 0 {
                    selected -= 1;
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if expanded {
                    preview_scroll = preview_scroll.saturating_add(1);
                } else if selected + 1 < choices.len() {
                    selected += 1;
                }
            }
            KeyCode::PageUp => {
                if expanded {
                    preview_scroll = preview_scroll.saturating_sub(10);
                } else {
                    selected = selected.saturating_sub(10);
                }
            }
            KeyCode::PageDown => {
                if expanded {
                    preview_scroll = preview_scroll.saturating_add(10);
                } else {
                    selected = (selected + 10).min(choices.len().saturating_sub(1));
                }
            }
            KeyCode::Home => {
                if expanded {
                    preview_scroll = 0;
                } else {
                    selected = 0;
                }
            }
            KeyCode::End => {
                if !expanded {
                    selected = choices.len().saturating_sub(1);
                }
            }
            _ => {}
        }

        // Keep selected row visible in the list viewport.
        let list_height = export_list_viewport_height(term_rows, expanded);
        if selected < list_scroll {
            list_scroll = selected;
        } else if selected >= list_scroll + list_height {
            list_scroll = selected + 1 - list_height;
        }
    }
}

fn export_list_viewport_height(term_rows: usize, expanded: bool) -> usize {
    // header(1) + help(1) + blank(1) + optional preview header/footer
    if expanded {
        // reserve ~half for preview
        ((term_rows.saturating_sub(6)) / 2).max(3)
    } else {
        term_rows.saturating_sub(4).max(3)
    }
}

fn draw_export_picker(
    stdout: &mut io::Stdout,
    choices: &[ExportChoice],
    selected: usize,
    list_scroll: usize,
    expanded: bool,
    preview_scroll: usize,
    term_cols: usize,
    term_rows: usize,
) -> Result<(), AppError> {
    queue!(stdout, MoveTo(0, 0), Clear(ClearType::All))
        .map_err(|e| AppError::Message(format!("terminal draw failed: {e}")))?;

    let mut row: u16 = 0;
    write_line(
        stdout,
        row,
        term_cols,
        "Select a session to export",
        CtColor::Cyan,
        true,
    )?;
    row += 1;
    write_line(
        stdout,
        row,
        term_cols,
        "↑/↓ move  Enter export  Ctrl+E preview  Esc cancel",
        CtColor::DarkGrey,
        false,
    )?;
    row += 1;

    let list_height = export_list_viewport_height(term_rows, expanded);
    let end = (list_scroll + list_height).min(choices.len());
    for (visible_i, index) in (list_scroll..end).enumerate() {
        let choice = &choices[index];
        let time =
            format_session_time(choice.session.last_active_at.or(choice.session.created_at));
        let marker = if index == selected { ">" } else { " " };
        let line = format!(
            "{marker} {:>3}. [{}] {}  ({})",
            index + 1,
            time,
            choice.label,
            short_session_id(&choice.session.session_id)
        );
        let is_sel = index == selected;
        write_line(
            stdout,
            row + visible_i as u16,
            term_cols,
            &line,
            if is_sel {
                CtColor::Yellow
            } else {
                CtColor::Reset
            },
            is_sel,
        )?;
    }
    row += list_height as u16;

    if expanded {
        let choice = &choices[selected];
        row = row.saturating_add(1);
        let header = format!(
            "── Conversation: {} ({}) ──  Ctrl+E / Esc collapse",
            choice.label,
            short_session_id(&choice.session.session_id)
        );
        write_line(stdout, row, term_cols, &header, CtColor::Green, true)?;
        row += 1;

        let preview_lines = build_conversation_preview_lines(&choice.messages, term_cols);
        let preview_height = term_rows
            .saturating_sub(row as usize)
            .saturating_sub(1)
            .max(3);
        let max_scroll = preview_lines.len().saturating_sub(preview_height);
        let scroll = preview_scroll.min(max_scroll);
        let preview_end = (scroll + preview_height).min(preview_lines.len());

        for (i, line) in preview_lines[scroll..preview_end].iter().enumerate() {
            let color = if line.starts_with("[user]") {
                CtColor::Green
            } else if line.starts_with("[assistant]") {
                CtColor::Magenta
            } else {
                CtColor::Reset
            };
            write_line(stdout, row + i as u16, term_cols, line, color, false)?;
        }
        row += preview_height as u16;

        let status = if preview_lines.is_empty() {
            "No user/assistant messages in this session.".to_string()
        } else {
            format!(
                "lines {}-{} / {}{}",
                scroll + 1,
                preview_end,
                preview_lines.len(),
                if choice.messages_truncated {
                    "  (source truncated)"
                } else {
                    ""
                }
            )
        };
        write_line(stdout, row, term_cols, &status, CtColor::DarkGrey, false)?;
    }

    stdout
        .flush()
        .map_err(|e| AppError::Message(format!("terminal flush failed: {e}")))?;
    Ok(())
}

fn write_line(
    stdout: &mut io::Stdout,
    row: u16,
    term_cols: usize,
    text: &str,
    color: CtColor,
    bold_like: bool,
) -> Result<(), AppError> {
    let clipped = clip_display_width(text, term_cols.saturating_sub(1));
    let padded = format!("{:<width$}", clipped, width = term_cols.saturating_sub(1));
    let _ = bold_like;
    queue!(
        stdout,
        MoveTo(0, row),
        SetForegroundColor(color),
        Print(padded),
        ResetColor
    )
    .map_err(|e| AppError::Message(format!("terminal write failed: {e}")))?;
    Ok(())
}

fn clip_display_width(text: &str, max_cols: usize) -> String {
    if max_cols == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut width = 0usize;
    for ch in text.chars() {
        let w = if ch.is_ascii() { 1 } else { 2 };
        if width + w > max_cols {
            break;
        }
        out.push(ch);
        width += w;
    }
    out
}

fn build_conversation_preview_lines(messages: &[SessionMessage], term_cols: usize) -> Vec<String> {
    let wrap_width = term_cols.saturating_sub(2).max(20);
    let mut lines = Vec::new();
    for msg in messages {
        let role = match msg.role.as_str() {
            "user" => "[user]",
            "assistant" => "[assistant]",
            other => other,
        };
        let body = collapse_whitespace(&msg.content);
        let header = format!("{role} ");
        let mut remaining = body.as_str();
        // First line with role prefix.
        let first_budget = wrap_width.saturating_sub(header.chars().count()).max(8);
        let (first, rest) = split_at_chars(remaining, first_budget);
        lines.push(format!("{header}{first}"));
        remaining = rest;
        while !remaining.is_empty() {
            let (chunk, rest) = split_at_chars(remaining, wrap_width);
            lines.push(format!("  {chunk}"));
            remaining = rest;
        }
        lines.push(String::new());
    }
    if lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    lines
}

fn split_at_chars(text: &str, max_chars: usize) -> (&str, &str) {
    if max_chars == 0 || text.is_empty() {
        return ("", text);
    }
    let mut end = text.len();
    for (i, (idx, _)) in text.char_indices().enumerate() {
        if i >= max_chars {
            end = idx;
            break;
        }
    }
    if end >= text.len() {
        (text, "")
    } else {
        (&text[..end], &text[end..])
    }
}

fn list_sessions(
    app: Option<AppType>,
    provider: Option<AppType>,
    all: bool,
    json: bool,
) -> Result<(), AppError> {
    let include_all = all && provider.is_none();
    let scope = session_scope(app, provider, include_all);
    let reader =
        session_manager::build_fresh_session_manifest(&scope).map_err(AppError::Message)?;
    if json {
        let stdout = std::io::stdout();
        let mut output = stdout.lock();
        return write_manifest_json(&reader, &mut output);
    }

    if reader.total_rows() == 0 {
        println!("{}", info("No sessions found."));
        return Ok(());
    }

    // `comfy_table` needs to retain every row whose widths it computes. Render
    // one immutable manifest page at a time so a million-row list never turns
    // into a million-row in-memory table. Repeating the header at page
    // boundaries also keeps long terminal output readable.
    for page_index in 0..reader.page_count() {
        let page = reader.load_page(page_index).ok_or_else(|| {
            AppError::Message(format!(
                "Session page {page_index} became unavailable while listing."
            ))
        })?;
        let mut table = create_table();
        table.set_header(vec!["Provider", "Session", "Title", "Updated", "Workdir"]);
        for session in &page.rows {
            table.add_row(vec![
                session.provider_id.clone(),
                short_session_id(&session.session_id),
                session_title(session),
                format_session_time(session.last_active_at.or(session.created_at)),
                session
                    .project_dir
                    .as_deref()
                    .filter(|value| !value.trim().is_empty())
                    .unwrap_or("-")
                    .to_string(),
            ]);
        }
        println!("{table}");
    }
    Ok(())
}

fn write_manifest_json(
    reader: &session_manager::paged_manifest::ManifestReader,
    output: &mut dyn Write,
) -> Result<(), AppError> {
    // Publication validated every page before this immutable generation became
    // current. Stream it once so first-byte latency and total I/O remain linear.
    let mut serializer = serde_json::Serializer::pretty(&mut *output);
    let mut sequence = serializer
        .serialize_seq(Some(reader.total_rows()))
        .map_err(|source| AppError::JsonSerialize { source })?;
    for page_index in 0..reader.page_count() {
        let page = reader.load_page(page_index).ok_or_else(|| {
            AppError::Message(format!(
                "Session page {page_index} became unavailable while writing JSON."
            ))
        })?;
        for session in &page.rows {
            sequence
                .serialize_element(session)
                .map_err(|source| AppError::JsonSerialize { source })?;
        }
    }
    sequence
        .end()
        .map_err(|source| AppError::JsonSerialize { source })?;
    drop(serializer);
    output
        .write_all(b"\n")
        .map_err(|source| AppError::IoContext {
            context: "failed to finish session JSON output".to_string(),
            source,
        })
}

fn show_session(
    app: Option<AppType>,
    provider: Option<AppType>,
    all: bool,
    selector: &str,
    json: bool,
) -> Result<(), AppError> {
    let (session, _) = resolve_scanned_session(app, provider, all, selector)?;
    let (message_batch, messages_error) = load_session_messages(&session);

    if json {
        let (messages, messages_truncated) = message_batch
            .map(|batch| (Some(batch.messages), batch.truncated))
            .unwrap_or((None, false));
        let detail = SessionDetail {
            session,
            messages,
            messages_truncated,
            messages_error,
        };
        println!(
            "{}",
            to_json(&detail).map_err(|source| AppError::JsonSerialize { source })?
        );
        return Ok(());
    }

    print_session_overview(&session);
    println!();
    match (message_batch, messages_error) {
        (Some(batch), None) => print_message_batch(&batch),
        (_, Some(error)) => println!("{}", warning(&error)),
        _ => println!("{}", info("Messages are not available.")),
    }

    Ok(())
}

fn print_messages(
    app: Option<AppType>,
    provider: Option<AppType>,
    all: bool,
    selector: &str,
    json: bool,
) -> Result<(), AppError> {
    let (session, _) = resolve_scanned_session(app, provider, all, selector)?;
    let source_path = required_source_path(&session)?;
    let batch = session_manager::load_messages(&session.provider_id, source_path)
        .map_err(AppError::Message)?;

    if json {
        let output = SessionMessagesOutput {
            messages: &batch.messages,
            messages_truncated: batch.truncated,
        };
        println!(
            "{}",
            to_json(&output).map_err(|source| AppError::JsonSerialize { source })?
        );
        return Ok(());
    }

    print_message_batch(&batch);
    Ok(())
}

fn resume_session(
    app: Option<AppType>,
    provider: Option<AppType>,
    all: bool,
    selector: &str,
    print: bool,
) -> Result<(), AppError> {
    let (session, _) = resolve_scanned_session(app, provider, all, selector)?;
    let command = session
        .resume_command
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| AppError::Message("Session does not have a resume command.".to_string()))?;

    if print {
        println!("{command}");
        return Ok(());
    }

    let status = shell_command(command)
        .current_dir_for_session(session.project_dir.as_deref())?
        .status()
        .map_err(|source| AppError::IoContext {
            context: format!("failed to execute resume command `{command}`"),
            source,
        })?;

    if status.success() {
        Ok(())
    } else {
        Err(AppError::Message(format!(
            "Resume command exited with status {status}"
        )))
    }
}

fn delete_session(
    app: Option<AppType>,
    provider: Option<AppType>,
    all: bool,
    selector: &str,
    yes: bool,
) -> Result<(), AppError> {
    let (session, _) = resolve_scanned_session(app, provider, all, selector)?;
    let source_path = required_source_path(&session)?.to_string();

    if !yes {
        let confirm = inquire::Confirm::new(&format!(
            "Delete session '{}' from {}?",
            session_title(&session),
            session.provider_id
        ))
        .with_default(false)
        .prompt()
        .map_err(|e| AppError::Message(format!("Prompt failed: {e}")))?;
        if !confirm {
            println!("{}", info("Cancelled."));
            return Ok(());
        }
    }

    let deleted =
        session_manager::delete_session(&session.provider_id, &session.session_id, &source_path)
            .map_err(AppError::Message)?;
    if !deleted {
        return Err(AppError::Message("Session was not deleted.".to_string()));
    }

    // 与 TUI 删除路径一致：清掉扫描 sidecar、首屏快照以及 provider/all 两份
    // 分页 manifest，避免 CLI 删除后重开 TUI 时旧首屏短暂“复活”。这些都是
    // schema-free 的本地缓存；失败只记 debug，不影响真实会话已经删除的结果。
    let _ = session_manager::purge_deleted_session_from_local_caches(
        &session.provider_id,
        &session.session_id,
        &source_path,
    );

    println!(
        "{}",
        success(&format!(
            "Deleted session '{}' from {}.",
            session_title(&session),
            session.provider_id
        ))
    );
    Ok(())
}

fn search_sessions_cmd(query: &str, json: bool) -> Result<(), AppError> {
    let reader = session_manager::build_fresh_session_manifest("all").map_err(AppError::Message)?;

    if json {
        let stdout = std::io::stdout();
        let mut output = stdout.lock();
        return write_manifest_search_json(&reader, query, &mut output);
    }

    let mut found = false;
    for page_index in 0..reader.page_count() {
        let page = reader.load_page(page_index).ok_or_else(|| {
            AppError::Message(format!(
                "Session page {page_index} became unavailable while searching."
            ))
        })?;
        let hits = session_manager::search_sessions_in(&page.rows, query);
        if hits.is_empty() {
            continue;
        }
        found = true;

        // Both the source page and this table are capped at 100 rows. Print
        // each result page before searching the next one so neither the corpus
        // nor a high-match result set accumulates in memory.
        let mut table = create_table();
        table.set_header(vec!["Provider", "Session", "Title", "Snippets"]);
        for hit in &hits {
            let session = page
                .rows
                .iter()
                .find(|session| {
                    session.provider_id == hit.provider_id
                        && session.session_id == hit.session_id
                        && session.source_path.as_deref() == Some(hit.source_path.as_str())
                })
                .map(session_title)
                .unwrap_or_else(|| short_session_id(&hit.session_id));
            let snippet_preview = hit
                .snippets
                .iter()
                .take(2)
                .map(|snippet| truncate_chars(&snippet.snippet, 80))
                .collect::<Vec<_>>()
                .join(" | ");
            table.add_row(vec![
                hit.provider_id.clone(),
                short_session_id(&hit.session_id),
                session,
                snippet_preview,
            ]);
        }
        println!("{table}");

        for hit in &hits {
            println!();
            println!(
                "{}",
                info(&format!(
                    "=== {} / {} ===",
                    hit.provider_id,
                    short_session_id(&hit.session_id)
                ))
            );
            for snippet in &hit.snippets {
                println!(
                    "  [{}] {}",
                    snippet.role,
                    truncate_chars(&snippet.snippet, 200)
                );
            }
        }
    }
    if !found {
        println!("{}", info("No matches found."));
    }
    Ok(())
}

fn write_manifest_search_json(
    reader: &session_manager::paged_manifest::ManifestReader,
    query: &str,
    output: &mut dyn Write,
) -> Result<(), AppError> {
    let mut serializer = serde_json::Serializer::pretty(&mut *output);
    let mut sequence = serializer
        .serialize_seq(None)
        .map_err(|source| AppError::JsonSerialize { source })?;
    for page_index in 0..reader.page_count() {
        let page = reader.load_page(page_index).ok_or_else(|| {
            AppError::Message(format!(
                "Session page {page_index} became unavailable while searching."
            ))
        })?;
        for hit in session_manager::search_sessions_in(&page.rows, query) {
            sequence
                .serialize_element(&hit)
                .map_err(|source| AppError::JsonSerialize { source })?;
        }
    }
    sequence
        .end()
        .map_err(|source| AppError::JsonSerialize { source })?;
    drop(serializer);
    output
        .write_all(b"\n")
        .map_err(|source| AppError::IoContext {
            context: "failed to finish session search JSON output".to_string(),
            source,
        })
}

fn sync_usage(
    app: Option<AppType>,
    provider: Option<AppType>,
    all: bool,
    json: bool,
) -> Result<(), AppError> {
    let db = Database::init()?;
    let backfill_error = match db.backfill_missing_usage_costs() {
        Ok(updated) if updated > 0 => {
            log::info!("Manual session usage sync backfilled costs for {updated} usage row(s)");
            None
        }
        Ok(_) => None,
        Err(error) => Some(format!("Usage cost backfill failed: {error}")),
    };

    let mut result = if all || (app.is_none() && provider.is_none()) {
        crate::services::session_usage::sync_all_session_usage(&db)?
    } else {
        let app_type = provider.or(app).unwrap_or(AppType::Claude);
        sync_usage_for_provider(&db, app_type)?
    };
    if let Some(message) = backfill_error {
        result.errors.push(message);
    }

    if json {
        println!(
            "{}",
            to_json(&result).map_err(|source| AppError::JsonSerialize { source })?
        );
        return Ok(());
    }

    println!(
        "{}",
        success(&format!(
            "Synced usage: imported {}, skipped {}, scanned {} file(s).",
            result.imported, result.skipped, result.files_scanned
        ))
    );
    for error in result.errors.iter().take(8) {
        println!("{}", warning(error));
    }
    if result.errors.len() > 8 {
        println!(
            "{}",
            warning(&format!(
                "... and {} more error(s)",
                result.errors.len() - 8
            ))
        );
    }

    Ok(())
}

fn sync_usage_for_provider(
    db: &Database,
    app_type: AppType,
) -> Result<SessionSyncResult, AppError> {
    // 与 sync_all_session_usage 相同的导入作用域：进度可见 + 本连接（本
    // 命令独占）在导入期间临时 synchronous=NORMAL，结束恢复 FULL。
    let _progress = crate::services::session_usage::sync_progress::begin();
    let _durability = db.bulk_import_durability_guard();
    match app_type {
        AppType::Claude => crate::services::session_usage::sync_claude_session_logs(db),
        AppType::Codex => crate::services::session_usage_codex::sync_codex_usage(db),
        AppType::Gemini => crate::services::session_usage_gemini::sync_gemini_usage(db),
        AppType::OpenCode => crate::services::session_usage_opencode::sync_opencode_usage(db),
        other => Err(AppError::InvalidInput(format!(
            "session usage sync is only supported for claude, codex, gemini, and opencode; got {}",
            other.as_str()
        ))),
    }
}

fn resolve_scanned_session(
    app: Option<AppType>,
    provider: Option<AppType>,
    all: bool,
    selector: &str,
) -> Result<(SessionMeta, String), AppError> {
    let scoped = parse_scoped_selector(selector);
    let selector = scoped
        .as_ref()
        .map(|(_, selector)| selector.as_str())
        .unwrap_or(selector);
    let forced_provider = scoped
        .as_ref()
        .and_then(|(provider, _)| app_type_from_provider_id(provider))
        .or(provider);
    let include_all = all && forced_provider.is_none();
    let scope = session_scope(app, forced_provider, include_all);
    let reader =
        session_manager::build_fresh_session_manifest(&scope).map_err(AppError::Message)?;
    let session = resolve_session_manifest(&reader, selector)?;
    Ok((session, selector.to_string()))
}

fn session_scope(app: Option<AppType>, provider: Option<AppType>, all: bool) -> String {
    if all {
        return "all".to_string();
    }

    provider
        .or(app)
        .unwrap_or(AppType::Claude)
        .as_str()
        .to_string()
}

#[derive(Default)]
struct SelectorMatches {
    count: usize,
    first: Option<SessionMeta>,
    choices: Vec<String>,
}

impl SelectorMatches {
    const MAX_CHOICES: usize = 8;

    fn push(&mut self, session: SessionMeta) {
        self.count = self.count.saturating_add(1);
        if self.first.is_none() {
            self.first = Some(session.clone());
        }
        if self.choices.len() < Self::MAX_CHOICES {
            self.choices
                .push(format!("{}:{}", session.provider_id, session.session_id));
        }
    }
}

struct SessionSelectorResolver<'a> {
    selector: &'a str,
    exact: SelectorMatches,
    prefixed: SelectorMatches,
}

impl<'a> SessionSelectorResolver<'a> {
    fn new(selector: &'a str) -> Result<Self, AppError> {
        let selector = selector.trim();
        if selector.is_empty() {
            return Err(AppError::InvalidInput(
                "session selector cannot be empty".to_string(),
            ));
        }
        Ok(Self {
            selector,
            exact: SelectorMatches::default(),
            prefixed: SelectorMatches::default(),
        })
    }

    fn push(&mut self, session: SessionMeta) {
        if session.session_id == self.selector {
            self.exact.push(session);
        } else if session.session_id.starts_with(self.selector) {
            self.prefixed.push(session);
        }
    }

    fn finish(self) -> Result<SessionMeta, AppError> {
        if self.exact.count == 1 {
            return self.exact.first.ok_or_else(|| {
                AppError::Message("Session selector state was incomplete.".to_string())
            });
        }
        if self.exact.count > 1 {
            return Err(ambiguous_selector_error(
                self.selector,
                self.exact.count,
                &self.exact.choices,
            ));
        }
        if self.prefixed.count == 1 {
            return self.prefixed.first.ok_or_else(|| {
                AppError::Message("Session selector state was incomplete.".to_string())
            });
        }
        if self.prefixed.count > 1 {
            return Err(ambiguous_selector_error(
                self.selector,
                self.prefixed.count,
                &self.prefixed.choices,
            ));
        }
        Err(AppError::Message(format!(
            "Session '{}' was not found.",
            self.selector
        )))
    }
}

fn resolve_session_manifest(
    reader: &session_manager::paged_manifest::ManifestReader,
    selector: &str,
) -> Result<SessionMeta, AppError> {
    let mut resolver = SessionSelectorResolver::new(selector)?;
    for page_index in 0..reader.page_count() {
        let page = reader.load_page(page_index).ok_or_else(|| {
            AppError::Message(format!(
                "Session page {page_index} is unavailable while resolving '{selector}'."
            ))
        })?;
        for session in page.rows {
            resolver.push(session);
        }
    }
    resolver.finish()
}

#[cfg(test)]
fn resolve_session(sessions: &[SessionMeta], selector: &str) -> Result<SessionMeta, AppError> {
    let mut resolver = SessionSelectorResolver::new(selector)?;
    for session in sessions {
        resolver.push(session.clone());
    }
    resolver.finish()
}

fn ambiguous_selector_error(selector: &str, count: usize, choices: &[String]) -> AppError {
    let selector = selector.trim();
    let choices = choices.join(", ");
    let suffix = if count > SelectorMatches::MAX_CHOICES {
        ", ..."
    } else {
        ""
    };
    AppError::Message(format!(
        "Session selector '{selector}' is ambiguous: {choices}{suffix}"
    ))
}

fn parse_scoped_selector(selector: &str) -> Option<(String, String)> {
    selector
        .split_once('/')
        .or_else(|| selector.split_once(':'))
        .and_then(|(provider, id)| {
            let provider = provider.trim();
            let id = id.trim();
            if provider.is_empty() || id.is_empty() || app_type_from_provider_id(provider).is_none()
            {
                return None;
            }
            Some((provider.to_string(), id.to_string()))
        })
}

fn parse_session_provider(value: &str) -> Result<AppType, String> {
    app_type_from_provider_id(value).ok_or_else(|| {
        format!(
            "unsupported provider '{value}'. Allowed: claude, codex, gemini, opencode, openclaw, hermes, grok, pi"
        )
    })
}

fn app_type_from_provider_id(provider_id: &str) -> Option<AppType> {
    let normalized = provider_id.trim().to_lowercase().replace('-', "");
    AppType::from_str(&normalized).ok()
}

fn load_session_messages(session: &SessionMeta) -> (Option<SessionMessageBatch>, Option<String>) {
    let Some(source_path) = session
        .source_path
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    else {
        return (
            None,
            Some("Session source path is not available; messages cannot be loaded.".to_string()),
        );
    };

    match session_manager::load_messages(&session.provider_id, source_path) {
        Ok(messages) => (Some(messages), None),
        Err(error) => (None, Some(error)),
    }
}

fn required_source_path(session: &SessionMeta) -> Result<&str, AppError> {
    session
        .source_path
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            AppError::Message(
                "Session source path is not available; this operation cannot continue.".to_string(),
            )
        })
}

fn print_session_overview(session: &SessionMeta) {
    println!("Provider: {}", session.provider_id);
    println!("Session:  {}", session.session_id);
    println!("Title:    {}", session_title(session));
    println!(
        "Summary:  {}",
        session
            .summary
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or("-")
    );
    println!(
        "Time:     {}",
        format_session_time(session.last_active_at.or(session.created_at))
    );
    println!(
        "Workdir:  {}",
        session
            .project_dir
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or("-")
    );
    println!(
        "Source:   {}",
        session
            .source_path
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or("-")
    );
    println!(
        "Resume:   {}",
        session
            .resume_command
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or("-")
    );
}

fn print_message_table(messages: &[SessionMessage]) {
    if messages.is_empty() {
        println!("{}", info("No messages found."));
        return;
    }

    let mut table = create_table();
    table.set_header(vec!["Role", "Time", "Content"]);
    for message in messages {
        table.add_row(vec![
            message.role.clone(),
            format_session_time(message.ts),
            collapse_message_preview(&message.content),
        ]);
    }
    println!("{table}");
}

fn print_message_batch(batch: &SessionMessageBatch) {
    print_message_table(&batch.messages);
    if batch.truncated {
        println!(
            "{}",
            warning("Showing a bounded message preview; additional content was truncated.")
        );
    }
}

fn session_title(session: &SessionMeta) -> String {
    session
        .title
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
        .or_else(|| {
            session
                .project_dir
                .as_deref()
                .map(path_basename)
                .filter(|value| !value.is_empty())
        })
        .unwrap_or_else(|| short_session_id(&session.session_id))
}

fn short_session_id(session_id: &str) -> String {
    session_id.chars().take(12).collect()
}

fn path_basename(path: &str) -> String {
    let trimmed = path.trim().trim_end_matches(['/', '\\']);
    if trimmed.is_empty() {
        return String::new();
    }
    std::path::Path::new(trimmed)
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or(trimmed)
        .to_string()
}

fn format_session_time(timestamp_ms: Option<i64>) -> String {
    timestamp_ms
        .and_then(|timestamp| Local.timestamp_millis_opt(timestamp).single())
        .map(|dt| dt.format("%Y/%m/%d %H:%M").to_string())
        .unwrap_or_else(|| "-".to_string())
}

fn collapse_message_preview(content: &str) -> String {
    const LIMIT: usize = 160;

    let mut preview = String::with_capacity(LIMIT + 3);
    let mut count = 0usize;
    let mut pending_space = false;
    let mut truncated = false;
    for ch in content.chars() {
        if ch.is_whitespace() {
            pending_space |= !preview.is_empty();
            continue;
        }
        if pending_space {
            if count >= LIMIT {
                truncated = true;
                break;
            }
            preview.push(' ');
            count += 1;
            pending_space = false;
        }
        if count >= LIMIT {
            truncated = true;
            break;
        }
        preview.push(ch);
        count += 1;
    }
    if truncated {
        preview.push_str("...");
    }
    preview
}

fn truncate_chars(value: &str, limit: usize) -> String {
    let mut chars = value.chars();
    let truncated = chars.by_ref().take(limit).collect::<String>();
    if chars.next().is_some() {
        format!("{truncated}...")
    } else {
        truncated
    }
}

trait SessionCommandExt {
    fn current_dir_for_session(self, cwd: Option<&str>) -> Result<Self, AppError>
    where
        Self: Sized;
}

impl SessionCommandExt for Command {
    fn current_dir_for_session(mut self, cwd: Option<&str>) -> Result<Self, AppError> {
        if let Some(cwd) = cwd.filter(|value| !value.trim().is_empty()) {
            self.current_dir(cwd);
        }
        Ok(self)
    }
}

#[cfg(unix)]
fn shell_command(command: &str) -> Command {
    let mut process = Command::new("sh");
    process.arg("-c").arg(command);
    process
}

#[cfg(windows)]
fn shell_command(command: &str) -> Command {
    let mut process = Command::new("cmd");
    process.arg("/C").arg(command);
    process
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(provider_id: &str, session_id: &str) -> SessionMeta {
        SessionMeta {
            provider_id: provider_id.to_string(),
            session_id: session_id.to_string(),
            title: None,
            summary: None,
            project_dir: None,
            created_at: None,
            last_active_at: None,
            source_path: Some(format!("/tmp/{provider_id}/{session_id}.jsonl")),
            resume_command: Some(format!("{provider_id} resume {session_id}")),
        }
    }

    #[test]
    fn resolves_exact_session_id_before_prefix_matches() {
        let sessions = vec![meta("codex", "abc"), meta("codex", "abcdef")];

        let resolved = resolve_session(&sessions, "abc").expect("exact match should resolve");

        assert_eq!(resolved.session_id, "abc");
    }

    #[test]
    fn resolves_unique_session_id_prefix() {
        let sessions = vec![meta("codex", "abcdef"), meta("codex", "xyz")];

        let resolved = resolve_session(&sessions, "abc").expect("prefix should resolve");

        assert_eq!(resolved.session_id, "abcdef");
    }

    #[test]
    fn rejects_ambiguous_session_id_prefix() {
        let sessions = vec![meta("codex", "abcdef"), meta("codex", "abc999")];

        let error = resolve_session(&sessions, "abc").expect_err("prefix should be ambiguous");

        assert!(error.to_string().contains("ambiguous"));
        assert!(error.to_string().contains("codex:abcdef"));
        assert!(error.to_string().contains("codex:abc999"));
    }

    #[test]
    fn selector_resolution_retains_only_bounded_ambiguity_context_at_large_n() {
        let mut resolver = SessionSelectorResolver::new("shared-").expect("resolver");
        for index in 0..50_000 {
            resolver.push(meta("codex", &format!("shared-{index:05}")));
        }

        assert_eq!(resolver.prefixed.count, 50_000);
        assert_eq!(
            resolver.prefixed.choices.len(),
            SelectorMatches::MAX_CHOICES
        );
        let error = resolver.finish().expect_err("prefix must be ambiguous");
        assert!(error.to_string().contains(", ..."));
    }

    #[test]
    fn manifest_json_stream_is_one_valid_array_across_pages() {
        let directory = tempfile::tempdir().expect("manifest directory");
        let store = session_manager::paged_manifest::PagedManifestStore::open_at(directory.path())
            .expect("manifest store");
        let mut builder = store.begin_build("codex").expect("manifest builder");
        for index in 0..205 {
            let mut row = meta("codex", &format!("session-{index:03}"));
            row.last_active_at = Some(index);
            builder.push(row).expect("manifest row");
        }
        let published = builder.publish().expect("published manifest");
        let mut output = Vec::new();

        write_manifest_json(&published.reader, &mut output).expect("stream JSON");

        let decoded: Vec<SessionMeta> = serde_json::from_slice(&output).expect("valid JSON array");
        assert_eq!(decoded.len(), 205);
        assert!(output.ends_with(b"\n"));
    }

    #[test]
    fn parses_provider_scoped_selectors() {
        assert_eq!(
            parse_scoped_selector("codex/abc"),
            Some(("codex".to_string(), "abc".to_string()))
        );
        assert_eq!(
            parse_scoped_selector("claude:xyz"),
            Some(("claude".to_string(), "xyz".to_string()))
        );
        assert_eq!(
            parse_scoped_selector("open-code/abc"),
            Some(("open-code".to_string(), "abc".to_string()))
        );
        assert_eq!(parse_scoped_selector("unknown/abc"), None);
        assert_eq!(parse_scoped_selector("not-scoped"), None);
    }

    #[test]
    fn parses_session_provider_backend_ids_and_clap_aliases() {
        assert_eq!(parse_session_provider("opencode"), Ok(AppType::OpenCode));
        assert_eq!(parse_session_provider("open-code"), Ok(AppType::OpenCode));
        assert_eq!(parse_session_provider("openclaw"), Ok(AppType::OpenClaw));
        assert_eq!(parse_session_provider("open-claw"), Ok(AppType::OpenClaw));
        assert!(parse_session_provider("unknown").is_err());
    }

    #[test]
    fn session_title_prefers_title_project_then_id() {
        let titled = SessionMeta {
            title: Some("Fix bug".to_string()),
            summary: Some("Summary".to_string()),
            project_dir: Some("/tmp/project".to_string()),
            ..meta("codex", "abcdef123456")
        };
        assert_eq!(session_title(&titled), "Fix bug");

        let project = SessionMeta {
            title: None,
            ..titled.clone()
        };
        assert_eq!(session_title(&project), "project");

        let fallback = SessionMeta {
            title: None,
            summary: None,
            project_dir: None,
            ..titled
        };
        assert_eq!(session_title(&fallback), "abcdef123456");
    }
}

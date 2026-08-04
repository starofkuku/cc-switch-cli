//! Lossless Codex rollout export used by `sessions export`.
//!
//! The normal session message reader is deliberately bounded for TUI previews.
//! This exporter instead reads every JSONL record, preserves the original event,
//! builds navigation indexes, and writes the final JSON incrementally.

use chrono::{Local, Utc};
use regex::Regex;
use serde::Serialize;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs::{self, File};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use tempfile::Builder;

use crate::session_manager::SessionMeta;

const SCHEMA_VERSION: u32 = 1;
const CONTEXT_DOC_PREFIX: &str = "CODEX_SESSION_CONTEXT";
const INDEX_EXCERPT_CHARS: usize = 320;
const DOC_EXCERPT_CHARS: usize = 220;

#[derive(Debug)]
pub struct CodexConversationExportResult {
    pub json_path: PathBuf,
    pub context_path: PathBuf,
    pub event_count: usize,
    pub invalid_event_count: usize,
}

#[derive(Debug, Clone, Serialize)]
struct ExportSource {
    original_path: String,
    session_id: String,
    title: Option<String>,
    project_dir: Option<String>,
    generated_at: String,
    file_size: u64,
    line_count: usize,
}

#[derive(Debug, Clone, Default, Serialize)]
struct ExportSummary {
    total_events: usize,
    valid_events: usize,
    invalid_events: usize,
    event_types: BTreeMap<String, usize>,
    payload_types: BTreeMap<String, usize>,
    message_roles: BTreeMap<String, usize>,
    tool_calls: usize,
    tool_outputs: usize,
    patches: usize,
    errors: usize,
    interruptions: usize,
    context_compactions: usize,
    unknown_events: usize,
}

#[derive(Debug, Clone, Serialize)]
struct ExportEvent {
    line: usize,
    timestamp: Option<Value>,
    event_type: String,
    payload_type: String,
    role: Option<String>,
    classification: String,
    extracted_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    parse_error: Option<String>,
    /// Complete parsed top-level event. For malformed JSON this is the raw line.
    raw: Value,
}

#[derive(Debug, Clone, Serialize)]
struct MessageRangeIndex {
    start_line: usize,
    end_line: usize,
    roles: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
struct LocatedTextIndex {
    line: usize,
    timestamp: Option<Value>,
    text: String,
    basis: &'static str,
}

#[derive(Debug, Clone, Serialize)]
struct WorkspaceChangeIndex {
    path: String,
    status: String,
    category: String,
    basis: &'static str,
}

#[derive(Debug, Clone, Serialize)]
struct FileIndex {
    path: String,
    lines: Vec<usize>,
}

#[derive(Debug, Clone, Serialize)]
struct CommandIndex {
    line: usize,
    call_id: Option<String>,
    tool: String,
    command: Option<String>,
    workdir: Option<String>,
    basis: &'static str,
}

#[derive(Debug, Clone, Serialize)]
struct ValidationIndex {
    line: usize,
    call_id: Option<String>,
    command_line: Option<usize>,
    exit_code: Option<i64>,
    output_excerpt: Option<String>,
    basis: &'static str,
}

#[derive(Debug, Clone, Serialize)]
struct PatchIndex {
    line: usize,
    operation: String,
    path: String,
    basis: &'static str,
}

#[derive(Debug, Clone, Serialize)]
struct CommitIndex {
    line: usize,
    commit: String,
    basis: &'static str,
}

#[derive(Debug, Clone, Default, Serialize)]
struct ExportIndexes {
    message_ranges: Vec<MessageRangeIndex>,
    user_decisions: Vec<LocatedTextIndex>,
    handoff_summaries: Vec<LocatedTextIndex>,
    files: Vec<FileIndex>,
    workspace_changes: Vec<WorkspaceChangeIndex>,
    commands: Vec<CommandIndex>,
    validation_results: Vec<ValidationIndex>,
    patches: Vec<PatchIndex>,
    commits: Vec<CommitIndex>,
    pending_items: Vec<LocatedTextIndex>,
}

#[derive(Default)]
struct Analysis {
    summary: ExportSummary,
    message_ranges: Vec<MessageRangeIndex>,
    user_decisions: Vec<LocatedTextIndex>,
    handoff_summaries: Vec<LocatedTextIndex>,
    file_lines: BTreeMap<String, BTreeSet<usize>>,
    commands: Vec<CommandIndex>,
    validation_results: Vec<ValidationIndex>,
    patches: Vec<PatchIndex>,
    commits: Vec<CommitIndex>,
    pending_items: Vec<LocatedTextIndex>,
    pending_calls: HashMap<String, usize>,
}

impl Analysis {
    fn observe(&mut self, event: &ExportEvent) {
        self.summary.total_events += 1;
        if event.parse_error.is_some() {
            self.summary.invalid_events += 1;
        } else {
            self.summary.valid_events += 1;
        }
        increment(&mut self.summary.event_types, &event.event_type);
        increment(&mut self.summary.payload_types, &event.payload_type);
        if let Some(role) = &event.role {
            increment(&mut self.summary.message_roles, role);
        }

        match event.classification.as_str() {
            "tool_call" => self.summary.tool_calls += 1,
            "tool_output" => self.summary.tool_outputs += 1,
            "error" => self.summary.errors += 1,
            "interruption" => self.summary.interruptions += 1,
            "context_compaction" => self.summary.context_compactions += 1,
            "unknown" | "invalid_json" => self.summary.unknown_events += 1,
            _ => {}
        }

        self.observe_message(event);
        self.observe_files(event);
        self.observe_tool(event);
        self.observe_commits(event);
        self.observe_pending(event);
    }

    fn observe_message(&mut self, event: &ExportEvent) {
        if event.classification != "message" {
            return;
        }
        let role = event.role.clone().unwrap_or_else(|| "unknown".to_string());
        if let Some(last) = self.message_ranges.last_mut() {
            if last.end_line + 1 == event.line {
                last.end_line = event.line;
                if !last.roles.contains(&role) {
                    last.roles.push(role.clone());
                }
            } else {
                self.message_ranges.push(MessageRangeIndex {
                    start_line: event.line,
                    end_line: event.line,
                    roles: vec![role.clone()],
                });
            }
        } else {
            self.message_ranges.push(MessageRangeIndex {
                start_line: event.line,
                end_line: event.line,
                roles: vec![role.clone()],
            });
        }

        let Some(text) = event.extracted_text.as_deref() else {
            return;
        };
        if role == "user" && !looks_like_injected_context(text) && looks_like_user_decision(text) {
            self.user_decisions.push(LocatedTextIndex {
                line: event.line,
                timestamp: event.timestamp.clone(),
                text: excerpt(text, INDEX_EXCERPT_CHARS),
                basis: "已确认",
            });
        }
        if role == "assistant" && looks_like_handoff_summary(text) {
            self.handoff_summaries.push(LocatedTextIndex {
                line: event.line,
                timestamp: event.timestamp.clone(),
                text: excerpt(text, INDEX_EXCERPT_CHARS),
                basis: "对话推断",
            });
        }
    }

    fn observe_files(&mut self, event: &ExportEvent) {
        let mut text = event.extracted_text.clone().unwrap_or_default();
        if matches!(event.classification.as_str(), "tool_call" | "tool_output") {
            text.push('\n');
            text.push_str(&event.raw.to_string());
        }
        for path in extract_paths(&text) {
            self.file_lines.entry(path).or_default().insert(event.line);
        }
    }

    fn observe_tool(&mut self, event: &ExportEvent) {
        let payload = event.raw.get("payload").unwrap_or(&Value::Null);
        if event.classification == "tool_call" {
            let tool = tool_name(payload);
            let call_id = call_id(payload);
            let arguments = tool_arguments(payload);
            let command = extract_command(arguments.as_ref());
            let workdir = arguments
                .as_ref()
                .and_then(|value| value.get("workdir").or_else(|| value.get("cwd")))
                .and_then(Value::as_str)
                .map(str::to_string);
            let command_index = CommandIndex {
                line: event.line,
                call_id: call_id.clone(),
                tool: tool.clone(),
                command,
                workdir,
                basis: "已确认",
            };
            if let Some(call_id) = call_id {
                self.pending_calls.insert(call_id, event.line);
            }
            if tool.eq_ignore_ascii_case("apply_patch")
                || event
                    .extracted_text
                    .as_deref()
                    .is_some_and(|text| text.contains("*** Begin Patch"))
            {
                let patch_text = arguments
                    .as_ref()
                    .map(Value::to_string)
                    .or_else(|| event.extracted_text.clone())
                    .unwrap_or_default();
                let patches = extract_patch_entries(event.line, &patch_text);
                self.summary.patches += patches.len().max(1);
                self.patches.extend(patches);
            }
            self.commands.push(command_index);
        } else if event.classification == "tool_output" {
            let call_id = call_id(payload);
            let command_line = call_id
                .as_ref()
                .and_then(|id| self.pending_calls.remove(id));
            let output = tool_output(payload);
            let exit_code = extract_exit_code(payload, output.as_deref());
            if exit_code.is_some_and(|code| code != 0) {
                self.summary.errors += 1;
            }
            self.validation_results.push(ValidationIndex {
                line: event.line,
                call_id,
                command_line,
                exit_code,
                output_excerpt: output.map(|text| excerpt(&text, INDEX_EXCERPT_CHARS)),
                basis: "已确认",
            });
        }
    }

    fn observe_commits(&mut self, event: &ExportEvent) {
        let Some(text) = event.extracted_text.as_deref() else {
            return;
        };
        let lower = text.to_ascii_lowercase();
        if !lower.contains("commit") && !lower.contains("git log") && !lower.contains("git show") {
            return;
        }
        for commit in extract_commit_hashes(text) {
            if !self.commits.iter().any(|item| item.commit == commit) {
                self.commits.push(CommitIndex {
                    line: event.line,
                    commit,
                    basis: "对话推断",
                });
            }
        }
    }

    fn observe_pending(&mut self, event: &ExportEvent) {
        if !matches!(event.role.as_deref(), Some("user") | Some("assistant")) {
            return;
        }
        let Some(text) = event.extracted_text.as_deref() else {
            return;
        };
        if looks_like_pending_item(text) {
            self.pending_items.push(LocatedTextIndex {
                line: event.line,
                timestamp: event.timestamp.clone(),
                text: excerpt(text, INDEX_EXCERPT_CHARS),
                basis: "对话推断",
            });
        }
    }

    fn finish(mut self) -> (ExportSummary, ExportIndexes) {
        for (call_id, line) in self.pending_calls {
            self.pending_items.push(LocatedTextIndex {
                line,
                timestamp: None,
                text: format!("Tool call {call_id} has no matching output event"),
                basis: "代码核实",
            });
        }
        let files = self
            .file_lines
            .into_iter()
            .map(|(path, lines)| FileIndex {
                path,
                lines: lines.into_iter().collect(),
            })
            .collect();
        (
            self.summary,
            ExportIndexes {
                message_ranges: self.message_ranges,
                user_decisions: self.user_decisions,
                handoff_summaries: self.handoff_summaries,
                files,
                workspace_changes: Vec::new(),
                commands: self.commands,
                validation_results: self.validation_results,
                patches: self.patches,
                commits: self.commits,
                pending_items: self.pending_items,
            },
        )
    }
}

pub fn default_export_path(session_id: &str) -> PathBuf {
    let date = Local::now().format("%Y%m%d");
    let short_id = safe_session_id_fragment(session_id);
    PathBuf::from(".tmp").join(format!("ccswitch-codex-{short_id}-{date}.json"))
}

pub fn export_conversation(
    session: &SessionMeta,
    output_path: &Path,
) -> Result<CodexConversationExportResult, String> {
    let source_path = session
        .source_path
        .as_deref()
        .ok_or_else(|| format!("Codex session '{}' has no source path", session.session_id))?;
    let source_path = Path::new(source_path);
    let metadata = fs::metadata(source_path).map_err(|error| {
        format!(
            "Failed to stat Codex rollout {}: {error}",
            source_path.display()
        )
    })?;
    let mut analysis = Analysis::default();
    let line_count = visit_lines(source_path, |line_number, line| {
        analysis.observe(&parse_event(line_number, line));
        Ok(())
    })?;
    let (summary, mut indexes) = analysis.finish();
    indexes.workspace_changes = inspect_workspace_changes(session.project_dir.as_deref());
    let source = ExportSource {
        original_path: source_path.display().to_string(),
        session_id: session.session_id.clone(),
        title: session.title.clone(),
        project_dir: session.project_dir.clone(),
        generated_at: Utc::now().to_rfc3339(),
        file_size: metadata.len(),
        line_count,
    };

    write_structured_json(source_path, output_path, &source, &summary, &indexes)?;
    restrict_sensitive_file(output_path)?;

    let context_path = context_document_path(session, output_path);
    write_context_document(&context_path, session, &source, &summary, &indexes)?;

    Ok(CodexConversationExportResult {
        json_path: output_path.to_path_buf(),
        context_path,
        event_count: summary.total_events,
        invalid_event_count: summary.invalid_events,
    })
}

fn visit_lines(
    path: &Path,
    mut visit: impl FnMut(usize, &str) -> Result<(), String>,
) -> Result<usize, String> {
    let file = File::open(path)
        .map_err(|error| format!("Failed to open Codex rollout {}: {error}", path.display()))?;
    let mut reader = BufReader::new(file);
    let mut bytes = Vec::new();
    let mut line_number = 0usize;
    loop {
        bytes.clear();
        let read = reader
            .read_until(b'\n', &mut bytes)
            .map_err(|error| format!("Failed to read Codex rollout {}: {error}", path.display()))?;
        if read == 0 {
            break;
        }
        line_number += 1;
        if bytes.last() == Some(&b'\n') {
            bytes.pop();
        }
        if bytes.last() == Some(&b'\r') {
            bytes.pop();
        }
        let line = String::from_utf8_lossy(&bytes);
        visit(line_number, &line)?;
    }
    Ok(line_number)
}

fn parse_event(line: usize, raw_line: &str) -> ExportEvent {
    let raw = match serde_json::from_str::<Value>(raw_line) {
        Ok(value) => value,
        Err(error) => {
            return ExportEvent {
                line,
                timestamp: None,
                event_type: "invalid_json".to_string(),
                payload_type: "unknown".to_string(),
                role: None,
                classification: "invalid_json".to_string(),
                extracted_text: non_empty(raw_line),
                parse_error: Some(error.to_string()),
                raw: Value::String(raw_line.to_string()),
            };
        }
    };
    let payload = raw.get("payload").unwrap_or(&Value::Null);
    let event_type = raw
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string();
    let payload_type = payload
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string();
    let role = payload
        .get("role")
        .and_then(Value::as_str)
        .map(str::to_string);
    let timestamp = raw.get("timestamp").cloned();
    let extracted_text = extract_event_text(&raw, payload);
    let classification = classify_event(&event_type, &payload_type, role.as_deref());
    ExportEvent {
        line,
        timestamp,
        event_type,
        payload_type,
        role,
        classification,
        extracted_text,
        parse_error: None,
        raw,
    }
}

fn classify_event(event_type: &str, payload_type: &str, role: Option<&str>) -> String {
    let event = event_type.to_ascii_lowercase();
    let payload = payload_type.to_ascii_lowercase();
    if event.contains("abort")
        || event.contains("interrupt")
        || event.contains("cancel")
        || payload.contains("abort")
        || payload.contains("interrupt")
        || payload.contains("cancel")
    {
        return "interruption".to_string();
    }
    if event.contains("compact") || payload.contains("compact") {
        return "context_compaction".to_string();
    }
    if event.contains("error") || payload.contains("error") || payload.contains("failed") {
        return "error".to_string();
    }
    if payload == "message" || role.is_some() {
        return "message".to_string();
    }
    if is_tool_output_type(&payload) {
        return "tool_output".to_string();
    }
    if is_tool_call_type(&payload) {
        return "tool_call".to_string();
    }
    if payload.contains("reasoning") {
        return "reasoning".to_string();
    }
    if event == "session_meta" {
        return "session_metadata".to_string();
    }
    if event == "turn_context" || payload.contains("context") {
        return "context".to_string();
    }
    if matches!(event.as_str(), "event_msg" | "response_item") {
        return "event".to_string();
    }
    "unknown".to_string()
}

fn is_tool_call_type(payload_type: &str) -> bool {
    payload_type.ends_with("_call")
        || matches!(
            payload_type,
            "function_call" | "custom_tool_call" | "local_shell_call"
        )
}

fn is_tool_output_type(payload_type: &str) -> bool {
    payload_type.ends_with("_call_output")
        || payload_type.ends_with("_output")
        || matches!(
            payload_type,
            "function_call_output" | "custom_tool_call_output" | "local_shell_call_output"
        )
}

fn extract_event_text(raw: &Value, payload: &Value) -> Option<String> {
    let candidates = [
        payload.get("content"),
        payload.get("output"),
        payload.get("text"),
        payload.get("message"),
        payload.get("arguments"),
        payload.get("input"),
        raw.get("message"),
    ];
    let mut parts = Vec::new();
    for value in candidates.into_iter().flatten() {
        let text = match value {
            Value::String(text) => text.clone(),
            Value::Null => String::new(),
            other => super::utils::extract_text(other),
        };
        if !text.trim().is_empty() && !parts.contains(&text) {
            parts.push(text);
        }
    }
    non_empty(&parts.join("\n"))
}

fn tool_name(payload: &Value) -> String {
    payload
        .get("name")
        .or_else(|| payload.get("tool"))
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string()
}

fn call_id(payload: &Value) -> Option<String> {
    payload
        .get("call_id")
        .or_else(|| payload.get("callId"))
        .or_else(|| payload.get("id"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn tool_arguments(payload: &Value) -> Option<Value> {
    let value = payload
        .get("arguments")
        .or_else(|| payload.get("input"))
        .or_else(|| payload.get("args"))
        .or_else(|| payload.get("action"))?;
    match value {
        Value::String(text) => serde_json::from_str(text)
            .ok()
            .or_else(|| Some(Value::String(text.clone()))),
        other => Some(other.clone()),
    }
}

fn extract_command(arguments: Option<&Value>) -> Option<String> {
    let arguments = arguments?;
    if let Some(command) = arguments.as_str() {
        return non_empty(command);
    }
    let command = arguments
        .get("cmd")
        .or_else(|| arguments.get("command"))
        .or_else(|| arguments.get("chars"))?;
    match command {
        Value::String(command) => non_empty(command),
        Value::Array(parts) => non_empty(
            &parts
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(" "),
        ),
        _ => None,
    }
}

fn looks_like_handoff_summary(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    [
        "交接",
        "最终摘要",
        "完成情况",
        "后续步骤",
        "next steps",
        "work completed",
        "current status",
        "handoff",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

fn tool_output(payload: &Value) -> Option<String> {
    payload
        .get("output")
        .or_else(|| payload.get("result"))
        .or_else(|| payload.get("content"))
        .and_then(|value| match value {
            Value::String(text) => Some(text.clone()),
            Value::Null => None,
            other => Some(other.to_string()),
        })
}

fn extract_exit_code(payload: &Value, output: Option<&str>) -> Option<i64> {
    let direct = payload
        .get("exit_code")
        .or_else(|| payload.get("exitCode"))
        .or_else(|| payload.get("status"))
        .and_then(Value::as_i64);
    if direct.is_some() {
        return direct;
    }
    let parsed = output.and_then(|text| serde_json::from_str::<Value>(text).ok())?;
    parsed
        .get("exit_code")
        .or_else(|| parsed.get("exitCode"))
        .or_else(|| parsed.get("status"))
        .and_then(Value::as_i64)
}

fn extract_patch_entries(line: usize, text: &str) -> Vec<PatchIndex> {
    let mut entries = Vec::new();
    for raw_line in text.lines() {
        let trimmed = raw_line.trim().trim_matches('"').replace("\\n", "\n");
        for logical_line in trimmed.lines() {
            let (operation, path) = if let Some(path) = logical_line.strip_prefix("*** Add File: ")
            {
                ("added", path)
            } else if let Some(path) = logical_line.strip_prefix("*** Update File: ") {
                ("modified", path)
            } else if let Some(path) = logical_line.strip_prefix("*** Delete File: ") {
                ("deleted", path)
            } else {
                continue;
            };
            entries.push(PatchIndex {
                line,
                operation: operation.to_string(),
                path: path.trim().to_string(),
                basis: "代码核实",
            });
        }
    }
    entries
}

fn looks_like_user_decision(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    [
        "按这个",
        "就按",
        "确认",
        "决定",
        "采用",
        "必须",
        "不要",
        "无需",
        "直接",
        "改成",
        "confirmed",
        "decide",
        "must ",
        "do not",
        "don't",
        "use this",
        "go with",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

fn looks_like_injected_context(text: &str) -> bool {
    let trimmed = text.trim_start();
    trimmed.starts_with("# AGENTS.md instructions for ")
        || trimmed.starts_with("Another language model started to solve this problem")
        || trimmed.contains("<environment_context>")
        || trimmed.contains("<permissions instructions>")
}

fn looks_like_pending_item(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    [
        "未完成",
        "待处理",
        "后续",
        "尚未验证",
        "阻塞",
        "todo",
        "follow-up",
        "follow up",
        "remaining",
        "not yet",
        "blocked",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

fn extract_paths(text: &str) -> Vec<String> {
    static PATH_RE: OnceLock<Regex> = OnceLock::new();
    let regex = PATH_RE.get_or_init(|| {
        Regex::new(r#"(?:[A-Za-z]:[\\/]|/|\./|\.\./)?(?:[A-Za-z0-9_.-]+[\\/])+[A-Za-z0-9_.-]+"#)
            .expect("valid path regex")
    });
    let mut paths = BTreeSet::new();
    for found in regex.find_iter(text) {
        let path = found
            .as_str()
            .trim_matches(|ch: char| matches!(ch, '"' | '\'' | '`' | ',' | ')' | '('));
        if path.contains("://") || path.starts_with("api/") || path.starts_with("v1/") {
            continue;
        }
        paths.insert(path.replace('\\', "/"));
    }
    paths.into_iter().collect()
}

fn extract_commit_hashes(text: &str) -> Vec<String> {
    static COMMIT_RE: OnceLock<Regex> = OnceLock::new();
    let regex =
        COMMIT_RE.get_or_init(|| Regex::new(r"\b[0-9a-fA-F]{7,40}\b").expect("valid commit regex"));
    regex
        .find_iter(text)
        .map(|value| value.as_str().to_ascii_lowercase())
        .collect()
}

fn inspect_workspace_changes(project_dir: Option<&str>) -> Vec<WorkspaceChangeIndex> {
    let Some(root) = project_dir.map(Path::new).filter(|path| path.is_dir()) else {
        return Vec::new();
    };
    let Ok(output) = Command::new("git")
        .args(["status", "--porcelain=v1", "-z", "--untracked-files=all"])
        .current_dir(root)
        .output()
    else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }

    let fields = output
        .stdout
        .split(|byte| *byte == b'\0')
        .collect::<Vec<_>>();
    let mut changes = Vec::new();
    let mut index = 0usize;
    while index < fields.len() {
        let field = fields[index];
        index += 1;
        if field.len() < 4 {
            continue;
        }
        let status = String::from_utf8_lossy(&field[..2]).into_owned();
        let path = String::from_utf8_lossy(&field[3..]).into_owned();
        if status.contains('R') || status.contains('C') {
            // Porcelain -z emits the second side of a rename/copy as the next
            // NUL-delimited field. The displayed path above is the destination.
            index = index.saturating_add(1);
        }
        let category = workspace_change_category(root, &status, &path);
        changes.push(WorkspaceChangeIndex {
            path,
            status,
            category,
            basis: "代码核实",
        });
    }
    changes
}

fn workspace_change_category(root: &Path, status: &str, path: &str) -> String {
    if status == "??" {
        return "untracked".to_string();
    }
    if status.contains('D') {
        return "deleted".to_string();
    }
    if status.contains('R') {
        return "renamed".to_string();
    }
    if status.contains('C') {
        return "copied".to_string();
    }
    if status.contains('A') {
        return "added".to_string();
    }
    if status.contains('M') && is_line_ending_only_change(root, path) {
        return "line_endings_only".to_string();
    }
    if status.contains('U') {
        return "unmerged".to_string();
    }
    "modified".to_string()
}

fn is_line_ending_only_change(root: &Path, path: &str) -> bool {
    Command::new("git")
        .args([
            "diff",
            "HEAD",
            "--quiet",
            "--ignore-space-at-eol",
            "--",
            path,
        ])
        .current_dir(root)
        .status()
        .is_ok_and(|status| status.success())
}

fn increment(map: &mut BTreeMap<String, usize>, key: &str) {
    *map.entry(key.to_string()).or_default() += 1;
}

fn excerpt(text: &str, max_chars: usize) -> String {
    let trimmed = text.trim();
    let mut output: String = trimmed.chars().take(max_chars).collect();
    if trimmed.chars().count() > max_chars {
        output.push('…');
    }
    output
}

fn non_empty(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn safe_session_id_fragment(session_id: &str) -> String {
    let fragment = session_id
        .chars()
        .take(8)
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_') {
                ch
            } else {
                '-'
            }
        })
        .collect::<String>();
    if fragment.is_empty() {
        "session".to_string()
    } else {
        fragment
    }
}

fn write_structured_json(
    source_path: &Path,
    output_path: &Path,
    source: &ExportSource,
    summary: &ExportSummary,
    indexes: &ExportIndexes,
) -> Result<(), String> {
    let parent = output_path
        .parent()
        .filter(|path| !path.as_os_str().is_empty());
    if let Some(parent) = parent {
        fs::create_dir_all(parent).map_err(|error| {
            format!(
                "Failed to create export directory {}: {error}",
                parent.display()
            )
        })?;
    }
    let temp_parent = parent.unwrap_or_else(|| Path::new("."));
    let mut temp = Builder::new()
        .prefix(".ccswitch-codex-export-")
        .tempfile_in(temp_parent)
        .map_err(|error| format!("Failed to create temporary export file: {error}"))?;
    {
        let mut writer = BufWriter::new(temp.as_file_mut());
        writeln!(writer, "{{").map_err(io_string)?;
        writeln!(writer, "  \"schema_version\": {SCHEMA_VERSION},").map_err(io_string)?;
        write_named_json(&mut writer, "source", source, true)?;
        write_named_json(&mut writer, "summary", summary, true)?;
        writeln!(writer, "  \"events\": [").map_err(io_string)?;
        let mut first = true;
        visit_lines(source_path, |line_number, line| {
            if !first {
                writeln!(writer, ",").map_err(io_string)?;
            }
            first = false;
            let event = parse_event(line_number, line);
            let json = serde_json::to_string_pretty(&event)
                .map_err(|error| format!("Failed to serialize Codex event: {error}"))?;
            write_indented(&mut writer, &json, 4)?;
            Ok(())
        })?;
        if !first {
            writeln!(writer).map_err(io_string)?;
        }
        writeln!(writer, "  ],").map_err(io_string)?;
        write_named_json(&mut writer, "indexes", indexes, false)?;
        writeln!(writer, "}}").map_err(io_string)?;
        writer.flush().map_err(io_string)?;
    }
    temp.as_file().sync_all().map_err(io_string)?;
    temp.persist(output_path).map_err(|error| {
        format!(
            "Failed to persist export {}: {}",
            output_path.display(),
            error.error
        )
    })?;
    Ok(())
}

fn write_named_json(
    writer: &mut impl Write,
    name: &str,
    value: &impl Serialize,
    trailing_comma: bool,
) -> Result<(), String> {
    let json = serde_json::to_string_pretty(value)
        .map_err(|error| format!("Failed to serialize {name}: {error}"))?;
    write!(writer, "  \"{name}\": ").map_err(io_string)?;
    write_indented_after_prefix(writer, &json, 2)?;
    if trailing_comma {
        writeln!(writer, ",").map_err(io_string)?;
    } else {
        writeln!(writer).map_err(io_string)?;
    }
    Ok(())
}

fn write_indented_after_prefix(
    writer: &mut impl Write,
    json: &str,
    continuation_indent: usize,
) -> Result<(), String> {
    let mut lines = json.lines();
    if let Some(first) = lines.next() {
        write!(writer, "{first}").map_err(io_string)?;
    }
    for line in lines {
        write!(writer, "\n{}{}", " ".repeat(continuation_indent), line).map_err(io_string)?;
    }
    Ok(())
}

fn write_indented(writer: &mut impl Write, json: &str, indent: usize) -> Result<(), String> {
    let prefix = " ".repeat(indent);
    for (index, line) in json.lines().enumerate() {
        if index > 0 {
            writeln!(writer).map_err(io_string)?;
        }
        write!(writer, "{prefix}{line}").map_err(io_string)?;
    }
    Ok(())
}

fn io_string(error: std::io::Error) -> String {
    error.to_string()
}

fn context_document_path(session: &SessionMeta, output_path: &Path) -> PathBuf {
    let short_id = safe_session_id_fragment(&session.session_id);
    let filename = format!("{CONTEXT_DOC_PREFIX}-{short_id}.md");
    session
        .project_dir
        .as_deref()
        .map(PathBuf::from)
        .filter(|path| path.is_dir())
        .unwrap_or_else(|| {
            output_path
                .parent()
                .filter(|path| !path.as_os_str().is_empty())
                .unwrap_or_else(|| Path::new("."))
                .to_path_buf()
        })
        .join(filename)
}

fn write_context_document(
    path: &Path,
    session: &SessionMeta,
    source: &ExportSource,
    summary: &ExportSummary,
    indexes: &ExportIndexes,
) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("Failed to create context document directory: {error}"))?;
    }
    let project_root = session.project_dir.as_deref().map(Path::new);
    let mut doc = String::new();
    doc.push_str("# Codex Session Context\n\n");
    doc.push_str(&format!(
        "Generated from Codex session `{}`. The complete event archive is stored separately and may contain sensitive data.\n\n",
        session.session_id
    ));
    doc.push_str("## 项目定位\n\n");
    doc.push_str(&format!(
        "- [已确认] 会话标题：{}\n",
        document_text(
            session.title.as_deref().unwrap_or("未命名会话"),
            project_root
        )
    ));
    if let Some(description) = discover_project_description(project_root) {
        doc.push_str(&format!(
            "- [代码核实] 仓库文档描述：{}\n\n",
            document_text(&description, project_root)
        ));
    } else {
        doc.push_str("- [尚未验证] 项目用途需要结合当前源码和仓库文档确认。\n\n");
    }

    doc.push_str("## 目录边界\n\n");
    doc.push_str("- [已确认] 项目根目录：当前会话记录的工作目录（绝对路径已省略）。\n");
    doc.push_str("- [已确认] 原始会话：Codex rollout JSONL（绝对路径已省略）。\n");
    let directories = discover_project_directories(project_root);
    if !directories.is_empty() {
        doc.push_str(&format!(
            "- [代码核实] 当前存在的主要目录：{}。\n",
            directories.join(", ")
        ));
    }
    doc.push('\n');

    doc.push_str("## 技术栈\n\n");
    let stacks = discover_technology_stack(project_root);
    if stacks.is_empty() {
        doc.push_str("- [尚未验证] 未从项目清单文件识别出技术栈。\n\n");
    } else {
        doc.push_str(&format!("- [代码核实] {}。\n\n", stacks.join("、")));
    }

    doc.push_str("## 业务主线\n\n");
    append_located_items(&mut doc, &indexes.user_decisions, project_root, 8);

    doc.push_str("## 最终交接摘要\n\n");
    append_located_items(&mut doc, &indexes.handoff_summaries, project_root, 8);

    doc.push_str("## 模块关系\n\n");
    if indexes.files.is_empty() {
        doc.push_str("- [尚未验证] 对话中没有提取到明确文件引用。\n\n");
    } else {
        for file in indexes.files.iter().take(30) {
            doc.push_str(&format!(
                "- [对话推断] `{}`：事件行 {}。\n",
                safe_project_path(&file.path, project_root),
                join_numbers(&file.lines)
            ));
        }
        doc.push('\n');
    }

    doc.push_str("## 数据流\n\n");
    doc.push_str(&format!(
        "- [代码核实] 记录包含 {} 次工具调用和 {} 次工具输出。\n",
        summary.tool_calls, summary.tool_outputs
    ));
    doc.push_str(
        "- [代码核实] 工具调用与输出通过 `call_id` 建立索引；缺失配对会进入未完成事项。\n\n",
    );

    doc.push_str("## 已确认规则\n\n");
    append_located_items(&mut doc, &indexes.user_decisions, project_root, 20);

    doc.push_str("## 已实现功能\n\n");
    if indexes.patches.is_empty()
        && indexes.commits.is_empty()
        && indexes.workspace_changes.is_empty()
    {
        doc.push_str("- [尚未验证] 对话中未提取到补丁或提交记录，不能仅依据助手描述判定完成。\n\n");
    } else {
        for change in indexes.workspace_changes.iter().take(50) {
            doc.push_str(&format!(
                "- [{}] 当前工作区 `{}`：{}（Git 状态 `{}`）。\n",
                change.basis,
                safe_project_path(&change.path, project_root),
                change.category,
                change.status
            ));
        }
        for patch in indexes.patches.iter().take(30) {
            doc.push_str(&format!(
                "- [{}] {} `{}`（事件行 {}）。\n",
                patch.basis,
                patch.operation,
                safe_project_path(&patch.path, project_root),
                patch.line
            ));
        }
        for commit in indexes.commits.iter().take(20) {
            doc.push_str(&format!(
                "- [{}] 提交 `{}`（事件行 {}，仍需与当前 Git 状态交叉核对）。\n",
                commit.basis, commit.commit, commit.line
            ));
        }
        doc.push_str("- [尚未验证] 上述工作区变更不自动代表正式实现；原型、预留能力和可发布状态仍需结合源码与验证结果判断。\n");
        doc.push('\n');
    }

    doc.push_str("## 数据库变更\n\n");
    let database_files: Vec<&FileIndex> = indexes
        .files
        .iter()
        .filter(|file| {
            let lower = file.path.to_ascii_lowercase();
            lower.contains("database")
                || lower.contains("migration")
                || lower.ends_with(".sql")
                || lower.contains("/dao/")
        })
        .collect();
    let database_changes: Vec<&WorkspaceChangeIndex> = indexes
        .workspace_changes
        .iter()
        .filter(|change| {
            let lower = change.path.to_ascii_lowercase();
            lower.contains("database")
                || lower.contains("migration")
                || lower.ends_with(".sql")
                || lower.contains("/dao/")
        })
        .collect();
    if database_files.is_empty() && database_changes.is_empty() {
        doc.push_str("- [尚未验证] 未从对话索引或当前 Git 状态中识别到数据库文件变更。\n\n");
    } else {
        for change in database_changes.iter().take(20) {
            doc.push_str(&format!(
                "- [{}] 当前工作区 `{}`：{}。\n",
                change.basis,
                safe_project_path(&change.path, project_root),
                change.category
            ));
        }
        for file in database_files.iter().take(20) {
            doc.push_str(&format!(
                "- [对话推断] `{}`（事件行 {}）。\n",
                safe_project_path(&file.path, project_root),
                join_numbers(&file.lines)
            ));
        }
        doc.push('\n');
    }

    doc.push_str("## 验证结果\n\n");
    if indexes.validation_results.is_empty() {
        doc.push_str("- [尚未验证] 没有提取到可定位的工具验证结果。\n\n");
    } else {
        for result in indexes.validation_results.iter().take(30) {
            let status = result
                .exit_code
                .map(|code| format!("exit_code={code}"))
                .unwrap_or_else(|| "退出状态未提供".to_string());
            doc.push_str(&format!(
                "- [{}] 事件行 {}：{}{}。\n",
                result.basis,
                result.line,
                status,
                result
                    .output_excerpt
                    .as_deref()
                    .map(|text| format!(
                        "，{}",
                        redact_sensitive(&excerpt(text, DOC_EXCERPT_CHARS))
                    ))
                    .unwrap_or_default()
            ));
        }
        doc.push('\n');
    }

    doc.push_str("## 未完成事项\n\n");
    append_located_items(&mut doc, &indexes.pending_items, project_root, 20);

    doc.push_str("## 风险\n\n");
    doc.push_str("- [已确认] 完整结构化 JSON 保留原始 payload，可能包含密钥、账号、机器路径和配置内容，不应直接提交。\n");
    if summary.invalid_events > 0 {
        doc.push_str(&format!(
            "- [代码核实] 有 {} 行无法解析为 JSON，已作为 `invalid_json` 原样保留。\n",
            summary.invalid_events
        ));
    }
    if summary.interruptions > 0 {
        doc.push_str(&format!(
            "- [代码核实] 会话包含 {} 个中断事件，相关回复不能视为完成。\n",
            summary.interruptions
        ));
    }
    doc.push_str(
        "- [对话推断] 自动提取的决策、文件和未决事项只是定位索引，最终结论应以当前工作区为准。\n\n",
    );

    doc.push_str("## 后续工作\n\n");
    doc.push_str(
        "- [尚未验证] 按索引回读相关原始事件，并检查当前仓库规则、源码、SQL、文档和 Git 状态。\n",
    );
    doc.push_str("- [尚未验证] 对照当前工作区确认补丁、验证和提交是否仍然有效。\n\n");
    doc.push_str(&format!(
        "<!-- source events: {}; file size: {} bytes; generated: {} -->\n",
        source.line_count, source.file_size, source.generated_at
    ));

    fs::write(path, doc).map_err(|error| {
        format!(
            "Failed to write context document {}: {error}",
            path.display()
        )
    })
}

fn append_located_items(
    doc: &mut String,
    items: &[LocatedTextIndex],
    project_root: Option<&Path>,
    limit: usize,
) {
    if items.is_empty() {
        doc.push_str("- [尚未验证] 没有提取到明确条目。\n\n");
        return;
    }
    for item in items.iter().take(limit) {
        let text = document_text(&excerpt(&item.text, DOC_EXCERPT_CHARS), project_root);
        doc.push_str(&format!(
            "- [{}] 事件行 {}：{}\n",
            item.basis, item.line, text
        ));
    }
    doc.push('\n');
}

fn discover_project_directories(project_root: Option<&Path>) -> Vec<String> {
    let Some(root) = project_root.filter(|path| path.is_dir()) else {
        return Vec::new();
    };
    [
        "src",
        "src-tauri",
        "tests",
        "docs",
        "migrations",
        "scripts",
        "assets",
        ".github",
    ]
    .into_iter()
    .filter(|name| root.join(name).is_dir())
    .map(str::to_string)
    .collect()
}

fn discover_technology_stack(project_root: Option<&Path>) -> Vec<String> {
    let Some(root) = project_root.filter(|path| path.is_dir()) else {
        return Vec::new();
    };
    [
        ("Cargo.toml", "Rust/Cargo"),
        ("src-tauri/Cargo.toml", "Rust/Cargo"),
        ("package.json", "JavaScript/TypeScript"),
        ("pyproject.toml", "Python"),
        ("requirements.txt", "Python"),
        ("go.mod", "Go"),
        ("pom.xml", "Java/Maven"),
        ("build.gradle", "Java/Gradle"),
    ]
    .into_iter()
    .filter(|(file, _)| root.join(file).is_file())
    .map(|(_, stack)| stack.to_string())
    .collect()
}

fn discover_project_description(project_root: Option<&Path>) -> Option<String> {
    let root = project_root.filter(|path| path.is_dir())?;
    let file = File::open(root.join("README.md")).ok()?;
    let reader = BufReader::new(file);
    for line in reader.lines().take(80) {
        let line = line.ok()?;
        let trimmed = line.trim();
        if trimmed.is_empty()
            || trimmed.starts_with('#')
            || trimmed.starts_with("![")
            || trimmed.starts_with("[![")
            || trimmed.starts_with('<')
        {
            continue;
        }
        return Some(excerpt(trimmed, DOC_EXCERPT_CHARS));
    }
    None
}

fn safe_project_path(path: &str, project_root: Option<&Path>) -> String {
    let candidate = Path::new(path);
    if candidate.is_absolute() {
        if let Some(root) = project_root {
            if let Ok(relative) = candidate.strip_prefix(root) {
                return relative.display().to_string();
            }
        }
        return "[external path]".to_string();
    }
    redact_sensitive(path)
}

fn redact_machine_paths(text: &str, project_root: Option<&Path>) -> String {
    let mut output = text.to_string();
    if let Some(root) = project_root {
        output = output.replace(&root.display().to_string(), "[project root]");
    }
    static ABSOLUTE_PATH_RE: OnceLock<Regex> = OnceLock::new();
    let regex = ABSOLUTE_PATH_RE.get_or_init(|| {
        Regex::new(r#"(^|[\s\"'`(])/(?:[^\s\"'`,)]+)"#).expect("valid absolute path regex")
    });
    regex.replace_all(&output, "$1[ABSOLUTE_PATH]").into_owned()
}

fn redact_sensitive(text: &str) -> String {
    static ASSIGN_RE: OnceLock<Regex> = OnceLock::new();
    static BEARER_RE: OnceLock<Regex> = OnceLock::new();
    static OPENAI_KEY_RE: OnceLock<Regex> = OnceLock::new();
    static URL_RE: OnceLock<Regex> = OnceLock::new();
    static EMAIL_RE: OnceLock<Regex> = OnceLock::new();
    static IP_RE: OnceLock<Regex> = OnceLock::new();
    let assign = ASSIGN_RE.get_or_init(|| {
        Regex::new(
            r#"(?i)\b(api[_-]?key|access[_-]?token|auth(?:orization)?|password|secret)\b(\s*[:=]\s*)[\"']?[^\s,\"'}]+"#,
        )
        .expect("valid secret assignment regex")
    });
    let bearer = BEARER_RE.get_or_init(|| {
        Regex::new(r"(?i)Bearer\s+[A-Za-z0-9._~+/=-]+").expect("valid bearer regex")
    });
    let openai_key = OPENAI_KEY_RE
        .get_or_init(|| Regex::new(r"\bsk-[A-Za-z0-9_-]{8,}\b").expect("valid key regex"));
    let url = URL_RE.get_or_init(|| {
        Regex::new(r#"(?i)\b(?:https?|wss?)://[^\s\"'`<>)]+"#).expect("valid URL regex")
    });
    let email = EMAIL_RE.get_or_init(|| {
        Regex::new(r"(?i)\b[A-Z0-9._%+-]+@[A-Z0-9.-]+\.[A-Z]{2,}\b").expect("valid email regex")
    });
    let ip = IP_RE.get_or_init(|| {
        Regex::new(r"\b(?:\d{1,3}\.){3}\d{1,3}\b").expect("valid IP address regex")
    });
    let output = assign.replace_all(text, "$1$2[REDACTED]");
    let output = bearer.replace_all(&output, "Bearer [REDACTED]");
    let output = openai_key.replace_all(&output, "[REDACTED]");
    let output = url.replace_all(&output, "[REDACTED_URL]");
    let output = email.replace_all(&output, "[REDACTED_EMAIL]");
    ip.replace_all(&output, "[REDACTED_IP]").into_owned()
}

fn document_text(text: &str, project_root: Option<&Path>) -> String {
    let redacted = redact_machine_paths(&redact_sensitive(text), project_root);
    redacted.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn join_numbers(values: &[usize]) -> String {
    values
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

fn restrict_sensitive_file(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(path)
            .map_err(|error| format!("Failed to read export permissions: {error}"))?
            .permissions();
        permissions.set_mode(0o600);
        fs::set_permissions(path, permissions)
            .map_err(|error| format!("Failed to restrict export permissions: {error}"))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::tempdir;

    #[test]
    fn structured_export_preserves_every_event_and_builds_indexes() {
        let temp = tempdir().expect("tempdir");
        let project = temp.path().join("project");
        fs::create_dir_all(project.join("src")).expect("project dirs");
        fs::write(
            project.join("Cargo.toml"),
            "[package]\nname='demo'\nversion='0.1.0'\n",
        )
        .expect("manifest");
        let rollout = temp.path().join("rollout.jsonl");
        let long_message = "x".repeat(20 * 1024);
        let lines = vec![
            json!({"timestamp":"2026-08-04T00:00:00Z","type":"session_meta","payload":{"id":"session-123","cwd":project}}).to_string(),
            json!({"timestamp":"2026-08-04T00:00:01Z","type":"response_item","payload":{"type":"message","role":"user","content":"确认按这个修改 src/main.rs，API_KEY=secret-value，联系 dev@example.com，检查 http://10.0.0.8/internal"}}).to_string(),
            json!({"timestamp":"2026-08-04T00:00:02Z","type":"response_item","payload":{"type":"function_call","name":"exec_command","call_id":"call-1","arguments":serde_json::to_string(&json!({"cmd":"cargo test","workdir":project})).unwrap()}}).to_string(),
            json!({"timestamp":"2026-08-04T00:00:03Z","type":"response_item","payload":{"type":"function_call_output","call_id":"call-1","output":serde_json::to_string(&json!({"exit_code":0,"output":"all tests passed"})).unwrap()}}).to_string(),
            json!({"timestamp":"2026-08-04T00:00:04Z","type":"response_item","payload":{"type":"message","role":"assistant","content":long_message}}).to_string(),
            json!({"timestamp":"2026-08-04T00:00:05Z","type":"event_msg","payload":{"type":"turn_aborted","reason":"user interrupt"}}).to_string(),
            json!({"timestamp":"2026-08-04T00:00:06Z","type":"future_event","payload":{"type":"future_payload","extra":{"kept":true}}}).to_string(),
            "{not-json".to_string(),
        ];
        fs::write(&rollout, format!("{}\n", lines.join("\n"))).expect("rollout");
        let session = SessionMeta {
            provider_id: "codex".to_string(),
            session_id: "session-123".to_string(),
            title: Some("Demo".to_string()),
            project_dir: Some(project.display().to_string()),
            source_path: Some(rollout.display().to_string()),
            ..Default::default()
        };
        let output = temp.path().join(".tmp/export.json");

        let result = export_conversation(&session, &output).expect("export");

        assert_eq!(result.event_count, lines.len());
        assert_eq!(result.invalid_event_count, 1);
        let exported: Value = serde_json::from_str(&fs::read_to_string(&output).unwrap()).unwrap();
        assert_eq!(exported["schema_version"], json!(1));
        assert_eq!(exported["events"].as_array().unwrap().len(), lines.len());
        assert_eq!(
            exported["events"][6]["raw"]["payload"]["extra"]["kept"],
            json!(true)
        );
        assert_eq!(
            exported["events"][7]["classification"],
            json!("invalid_json")
        );
        assert_eq!(
            exported["events"][4]["extracted_text"]
                .as_str()
                .unwrap()
                .len(),
            20 * 1024
        );
        assert_eq!(
            exported["indexes"]["commands"][0]["command"],
            json!("cargo test")
        );
        assert_eq!(
            exported["indexes"]["validation_results"][0]["exit_code"],
            json!(0)
        );
        let context = fs::read_to_string(&result.context_path).expect("context doc");
        assert!(context.contains("## 已确认规则"));
        assert!(context.contains("## 数据库变更"));
        assert!(!context.contains("secret-value"));
        assert!(!context.contains("dev@example.com"));
        assert!(!context.contains("10.0.0.8"));
        assert!(context.contains("[REDACTED]"));
        assert!(context.contains("[REDACTED_EMAIL]"));
        assert!(context.contains("[REDACTED_URL]"));

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&output).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn default_path_places_sensitive_archive_under_tmp() {
        let path = default_export_path("019f123456789");
        assert!(path.starts_with(".tmp"));
        assert!(path.to_string_lossy().contains("019f1234"));
        let untrusted = default_export_path("../../secret");
        assert_eq!(untrusted.parent(), Some(Path::new(".tmp")));
    }

    #[test]
    fn workspace_status_categories_distinguish_change_kinds() {
        let root = Path::new(".");
        assert_eq!(workspace_change_category(root, "??", "new.rs"), "untracked");
        assert_eq!(workspace_change_category(root, "A ", "new.rs"), "added");
        assert_eq!(workspace_change_category(root, " D", "old.rs"), "deleted");
        assert_eq!(workspace_change_category(root, "R ", "new.rs"), "renamed");
        assert_eq!(workspace_change_category(root, "UU", "both.rs"), "unmerged");
    }
}

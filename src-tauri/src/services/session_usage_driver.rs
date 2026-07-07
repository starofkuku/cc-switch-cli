//! 会话日志增量同步的通用 JSONL 驱动
//!
//! 所有基于 JSONL 会话日志的 app（当前 Claude、Codex，未来新增 app）共用
//! 同一条增量扫描线路，职责划分：
//!
//! - **驱动（本模块）**：mtime 跳过、sidecar 字节续传提示的校验与恢复、
//!   seek 或回退、按字节精确计数的逐行读取、行号/字节位置维护。
//! - **app 适配器（各 session_usage_*.rs）**：一个可 serde 的解析器状态机
//!   `S` + 一个逐行回调（解析行、维护状态、收集待写记录），以及各自
//!   语义的写库阶段（去重规则各 app 不同，刻意不统一）。
//!
//! 进度契约：主库 `session_log_sync` 的 `(last_modified, last_line_offset)`
//! 是权威进度（schema 与上游同步，不可扩展）；sidecar 的
//! `session_sync_resume` 只是加速提示——`(last_modified, last_line_offset)`
//! 快照与权威行完全一致且文件未缩短时才生效，任何不一致（整库从别的机器
//! WebDAV 同步进来、文件轮转/截断、提示状态无法反序列化）都回退到从字节 0
//! 按行 offset 跳过的旧路径，并在本轮结束后写回新提示。
//!
//! 非 JSONL 数据源（Gemini 整文件 JSON、OpenCode 外部 SQLite）天然无法按
//! 字节续传，仅遵循 mtime 跳过契约，不经过本驱动。

use std::fs;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::Path;

use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::error::AppError;
use crate::services::session_usage::metadata_modified_nanos;
use crate::session_manager::scan_cache_store::{ScanCacheStore, SyncResumeHint};

/// 一次增量扫描的结果：调用方写库时用 `(file_modified, line_offset)` 更新
/// 主库权威进度，提交成功后用整个 outcome 写回 sidecar 提示。
pub(crate) struct JsonlScanOutcome<S> {
    /// 扫描结束时的解析器状态（序列化进 sidecar 提示，供下次续传恢复）。
    pub state: S,
    /// 扫描结束时的行号（与主库 last_line_offset 语义一致，含最后的不完整行）。
    pub line_offset: i64,
    /// 扫描结束时的字节位置。
    pub byte_pos: u64,
    /// 本次使用的文件 mtime 纳秒值。
    pub file_modified: i64,
}

/// 校验 sidecar 字节续传提示：与主库权威行完全一致、且文件未被截断时才可用。
pub(crate) fn load_valid_resume_hint(
    resume: Option<&ScanCacheStore>,
    file_path: &str,
    last_modified: i64,
    last_offset: i64,
    file_len: u64,
) -> Option<SyncResumeHint> {
    let store = resume?;
    // 首次同步（无权威进度）没有可续传的位置
    if last_offset <= 0 {
        return None;
    }
    let hint = store.load_sync_resume(file_path).ok().flatten()?;
    (hint.last_modified == last_modified
        && hint.last_line_offset == last_offset
        && hint.byte_offset > 0
        && (hint.byte_offset as u64) <= file_len)
        .then_some(hint)
}

/// 增量扫描单个 JSONL 文件。
///
/// 返回 `Ok(None)` 表示文件自上次同步以来未变化（mtime 跳过）；返回
/// `Ok(Some(outcome))` 表示扫描完成，调用方随后执行自己的写库阶段。
///
/// 回调签名为 `(状态机, 行内容, is_new)`：`is_new == false` 的行只在回退
/// 路径出现（字节续传命中时历史行根本不会被读到），供需要重放历史行来
/// 重建状态的 app（如 Codex 的累计值 delta）使用；无此需求的 app 直接
/// `if !is_new return`。空行与无效 UTF-8 行由驱动跳过，不进回调。
pub(crate) fn scan_jsonl_incremental<S, F>(
    file_path: &Path,
    file_mtime: i64,
    last_modified: i64,
    last_offset: i64,
    resume: Option<&ScanCacheStore>,
    init_state: impl FnOnce() -> S,
    mut on_line: F,
) -> Result<Option<JsonlScanOutcome<S>>, AppError>
where
    S: Serialize + DeserializeOwned,
    F: FnMut(&mut S, &str, bool),
{
    let file_path_str = file_path.to_string_lossy();

    // mtime：优先使用 walk 阶段的值，回退到一次 metadata 读取，
    // 保留“元数据不可读即报错”语义。
    let file_modified = if file_mtime > 0 {
        file_mtime
    } else {
        let metadata = fs::metadata(file_path)
            .map_err(|e| AppError::Config(format!("无法读取文件元数据: {e}")))?;
        metadata_modified_nanos(&metadata)
    };

    // 文件未变化则跳过
    if file_modified <= last_modified {
        return Ok(None);
    }

    let mut file =
        fs::File::open(file_path).map_err(|e| AppError::Config(format!("无法打开文件: {e}")))?;
    let file_len = file.metadata().map(|m| m.len()).unwrap_or(0);

    // 字节续传：提示有效且状态机可反序列化时 seek 续读；否则从头回退
    let resumed =
        load_valid_resume_hint(resume, &file_path_str, last_modified, last_offset, file_len)
            .and_then(|hint| {
                let state: S = serde_json::from_str(hint.state.as_deref()?).ok()?;
                Some((hint.byte_offset as u64, state))
            });

    let (mut state, mut line_offset, mut byte_pos) = match resumed {
        Some((byte_offset, state)) => {
            file.seek(SeekFrom::Start(byte_offset))
                .map_err(|e| AppError::Config(format!("无法定位文件偏移: {e}")))?;
            (state, last_offset, byte_offset)
        }
        None => (init_state(), 0i64, 0u64),
    };

    let mut reader = BufReader::new(file);
    let mut raw: Vec<u8> = Vec::new();

    loop {
        raw.clear();
        // read_until 精确返回消耗的字节数（含换行符），字节位置始终可信；
        // IO 错误直接停止，已处理的进度仍然有效（各 app 的去重保证重扫安全）。
        let n = match reader.read_until(b'\n', &mut raw) {
            Ok(0) => break,
            Ok(n) => n,
            Err(_) => break,
        };
        byte_pos += n as u64;
        line_offset += 1;
        let is_new = line_offset > last_offset;

        // 与旧 lines() 语义一致：无效 UTF-8 行跳过
        let Ok(line) = std::str::from_utf8(&raw) else {
            continue;
        };
        let line = line.trim_end_matches('\n').trim_end_matches('\r');
        if line.trim().is_empty() {
            continue;
        }

        on_line(&mut state, line, is_new);
    }

    Ok(Some(JsonlScanOutcome {
        state,
        line_offset,
        byte_pos,
        file_modified,
    }))
}

/// 主库进度提交成功后，把字节位置与状态机写回 sidecar（尽力而为，
/// 失败只损失下次的续传加速，不影响正确性）。
pub(crate) fn save_resume_hint<S: Serialize>(
    resume: Option<&ScanCacheStore>,
    file_path_str: &str,
    outcome: &JsonlScanOutcome<S>,
) {
    let Some(store) = resume else {
        return;
    };
    let hint = SyncResumeHint {
        file_path: file_path_str.to_string(),
        last_modified: outcome.file_modified,
        last_line_offset: outcome.line_offset,
        byte_offset: outcome.byte_pos as i64,
        state: serde_json::to_string(&outcome.state).ok(),
    };
    if let Err(err) = store.save_sync_resume(&hint) {
        log::debug!("[SESSION-SYNC] 写入字节续传提示失败 ({file_path_str}): {err}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;
    use std::io::Write;

    /// 测试用状态机：记录回调看到的每一行及其 is_new 标记。
    /// `seen` 标记 serde(skip)：它是"本轮观察记录"而非跨轮解析状态，
    /// 不应随续传提示往返。
    #[derive(Debug, Default, Serialize, Deserialize)]
    struct RecordingState {
        #[serde(skip)]
        seen: Vec<(String, bool)>,
    }

    /// `file_mtime` 显式传入（模拟 walk 阶段取得的值）：测试不依赖真实文件
    /// 系统时间戳在两次写入之间前进，避免时间粒度导致的偶发跳过。
    fn scan_at(
        path: &std::path::Path,
        file_mtime: i64,
        last_modified: i64,
        last_offset: i64,
        resume: Option<&ScanCacheStore>,
    ) -> Option<JsonlScanOutcome<RecordingState>> {
        scan_jsonl_incremental(
            path,
            file_mtime,
            last_modified,
            last_offset,
            resume,
            RecordingState::default,
            |state, line, is_new| state.seen.push((line.to_string(), is_new)),
        )
        .expect("scan")
    }

    #[test]
    fn first_scan_reads_all_lines_and_reports_positions() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("s.jsonl");
        std::fs::write(&path, "l1\nl2\n").expect("write");

        let outcome = scan_at(&path, 0, 0, 0, None).expect("changed");
        assert_eq!(
            outcome.state.seen,
            vec![("l1".to_string(), true), ("l2".to_string(), true)]
        );
        assert_eq!(outcome.line_offset, 2);
        assert_eq!(outcome.byte_pos, 6);
        assert!(outcome.file_modified > 0);
    }

    #[test]
    fn unchanged_file_is_skipped() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("s.jsonl");
        std::fs::write(&path, "l1\n").expect("write");
        // mtime 未超过已记录的 last_modified → 跳过
        assert!(scan_at(&path, 5, 5, 1, None).is_none());
    }

    #[test]
    fn resume_seeks_past_history_even_when_head_bytes_change() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("s.jsonl");
        std::fs::write(&path, "l1\nl2\n").expect("write");
        let store = ScanCacheStore::in_memory().expect("store");

        let first = scan_at(&path, 1_000, 0, 0, Some(&store)).expect("changed");
        save_resume_hint(Some(&store), &path.to_string_lossy(), &first);

        // 破坏头部但保持总字节数不变：把第一个换行符改成空格，两行并作一行。
        // 行式回退路径会因行号偏移而错跳新行；字节续传路径完全不受影响。
        std::fs::write(&path, "l1 l2\nl3\n").expect("rewrite");

        let second = scan_at(
            &path,
            2_000,
            first.file_modified,
            first.line_offset,
            Some(&store),
        )
        .expect("changed");
        assert_eq!(second.state.seen, vec![("l3".to_string(), true)]);
        assert_eq!(second.line_offset, first.line_offset + 1);
        assert_eq!(second.byte_pos, 9);
    }

    #[test]
    fn mismatched_hint_falls_back_to_line_skip() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("s.jsonl");
        std::fs::write(&path, "l1\nl2\n").expect("write");
        let store = ScanCacheStore::in_memory().expect("store");

        let first = scan_at(&path, 1_000, 0, 0, Some(&store)).expect("changed");
        let path_str = path.to_string_lossy().to_string();
        save_resume_hint(Some(&store), &path_str, &first);

        // 篡改提示的权威快照，模拟主库被外部同步覆盖后的错位
        let mut stale = store
            .load_sync_resume(&path_str)
            .expect("load")
            .expect("hint");
        stale.last_modified += 1;
        store.save_sync_resume(&stale).expect("save");

        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"l3\n")
            .unwrap();

        // 回退路径：历史行以 is_new=false 进回调，新行 is_new=true
        let second = scan_at(
            &path,
            2_000,
            first.file_modified,
            first.line_offset,
            Some(&store),
        )
        .expect("changed");
        assert_eq!(
            second.state.seen,
            vec![
                ("l1".to_string(), false),
                ("l2".to_string(), false),
                ("l3".to_string(), true)
            ]
        );
    }

    #[test]
    fn truncated_file_invalidates_hint() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("s.jsonl");
        std::fs::write(&path, "long-line-1\nlong-line-2\n").expect("write");
        let store = ScanCacheStore::in_memory().expect("store");

        let first = scan_at(&path, 1_000, 0, 0, Some(&store)).expect("changed");
        let path_str = path.to_string_lossy().to_string();
        save_resume_hint(Some(&store), &path_str, &first);

        // 文件被截断重写：长度小于提示的字节位置 → 提示失效，从头回退
        std::fs::write(&path, "x\n").expect("truncate");
        let second = scan_at(
            &path,
            2_000,
            first.file_modified,
            first.line_offset,
            Some(&store),
        )
        .expect("changed");
        // 回退路径按行号跳过：仅 1 行且行号 <= last_offset，全部 is_new=false
        assert_eq!(second.state.seen, vec![("x".to_string(), false)]);
    }
}

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
//! `session_sync_resume` 只是加速提示，校验分三档（见 [`ResumeDecision`]）：
//! 快照与权威行一致且尾部指纹吻合 → 续传；截断/指纹失配证明同路径文件被
//! 重写 → 忽略旧行 offset 全量重扫（去重兜底）；无提示或提示与权威行不符
//! （首次运行、整库从别的机器 WebDAV 同步进来等）→ 回退到从字节 0 按行
//! offset 跳过的旧路径。本轮结束后写回新提示。
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

/// 尾部指纹窗口：`byte_offset` 前至多这么多字节参与 FNV-1a 指纹。
const TAIL_HASH_WINDOW: u64 = 64;

/// FNV-1a 64 位哈希：无依赖、确定性，用作续传边界的内容指纹。
fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// 一次增量扫描的结果：调用方写库时用 `(file_modified, line_offset)` 更新
/// 主库权威进度，提交成功后用整个 outcome 写回 sidecar 提示。
///
/// `line_offset`/`byte_pos` 只推进到最后一个**换行边界**：文件末尾的不完整
/// 行会进回调（旧行为如此，且写满的最终行可能永远不带换行符），但不计入
/// 持久化进度——正在被追加的半行下个周期从边界重读，去重保证不重复导入，
/// 从而修复了旧行 offset 语义下"半行被计数、补全后被跳过"的永久漏导入。
pub(crate) struct JsonlScanOutcome {
    /// 最后一个换行边界处的行号（与主库 last_line_offset 语义一致）。
    pub line_offset: i64,
    /// 最后一个换行边界处的字节位置。
    pub byte_pos: u64,
    /// 本次使用的文件 mtime 纳秒值。
    pub file_modified: i64,
    /// 换行边界处的状态机序列化快照（不含末尾不完整行的影响）；写进
    /// sidecar 提示，保证续传恢复的状态与恢复的字节位置严格对应。
    resume_state_json: Option<String>,
}

/// 基础校验：提示与主库权威行完全一致、且文件未被截断。
fn load_matching_resume_hint(
    resume: Option<&ScanCacheStore>,
    file_path: &str,
    last_modified: i64,
    last_offset: i64,
) -> Option<SyncResumeHint> {
    let store = resume?;
    // 首次同步（无权威进度）没有可续传的位置
    if last_offset <= 0 {
        return None;
    }
    let hint = store.load_sync_resume(file_path).ok().flatten()?;
    (hint.last_modified == last_modified
        && hint.last_line_offset == last_offset
        && hint.byte_offset > 0)
        .then_some(hint)
}

/// 续传决策：区分"能续传"、"文件身份失效需全量重扫"、"沿用行 offset 跳过"。
enum ResumeDecision<S> {
    /// 提示完整有效：从 `byte_offset` 续读，恢复状态机。
    Resume { byte_offset: u64, state: S },
    /// 有确凿证据表明同路径文件已被重写（截断、或续传边界前的内容指纹
    /// 失配）：权威行 offset 描述的是旧文件，必须忽略它从头全量重扫，
    /// 各 app 的 request_id 去重保证重扫不会重复入库。
    RescanFromZero,
    /// 无提示或提示与权威行不符（首次运行、整库从别的机器同步进来、
    /// 提示状态无法反序列化等）：没有身份失效的证据，沿用旧的
    /// 行 offset 跳过路径。
    LineSkipFallback,
}

/// 校验续传提示并做出决策；`Resume` 时文件游标恰好停在 `byte_offset`，
/// 其余情况由调用方负责把游标归零。
fn decide_resume<S: DeserializeOwned>(
    file: &mut fs::File,
    file_len: u64,
    hint: Option<SyncResumeHint>,
) -> ResumeDecision<S> {
    let Some(hint) = hint else {
        return ResumeDecision::LineSkipFallback;
    };

    let byte_offset = hint.byte_offset as u64;
    // 文件比上次的续传位置还短：同路径被截断/重写过
    if byte_offset > file_len {
        return ResumeDecision::RescanFromZero;
    }
    // 旧版提示无指纹：既不能证实也不能证伪身份，保守走行 offset 路径
    let Some(expected) = hint.tail_hash else {
        return ResumeDecision::LineSkipFallback;
    };

    // 尾部指纹：读 byte_offset 前的窗口与保存时比对
    let window = byte_offset.min(TAIL_HASH_WINDOW);
    let tail_ok = (|| {
        file.seek(SeekFrom::Start(byte_offset - window)).ok()?;
        let mut tail = vec![0u8; window as usize];
        std::io::Read::read_exact(file, &mut tail).ok()?;
        Some(fnv1a64(&tail) as i64 == expected)
    })();
    match tail_ok {
        // 指纹失配：边界前内容变了，文件被整体重写
        Some(false) => return ResumeDecision::RescanFromZero,
        // 读不出来（IO 抖动）：无证据，保守回退
        None => return ResumeDecision::LineSkipFallback,
        Some(true) => {}
    }

    // 指纹吻合但状态机无法恢复（多为本项目升级改了状态结构）：文件身份
    // 没问题，按行 offset 跳过即可
    match hint
        .state
        .as_deref()
        .and_then(|s| serde_json::from_str::<S>(s).ok())
    {
        // read_exact 结束后游标恰好位于 byte_offset，无需再次 seek
        Some(state) => ResumeDecision::Resume { byte_offset, state },
        None => ResumeDecision::LineSkipFallback,
    }
}

/// 读取文件 `byte_pos` 前的尾部窗口指纹（保存提示时使用）。对 append-only
/// 文件而言这段字节此后不再变化，即使保存时文件仍在增长也稳定。
fn compute_tail_hash(file_path: &str, byte_pos: u64) -> Option<i64> {
    let mut file = fs::File::open(file_path).ok()?;
    let window = byte_pos.min(TAIL_HASH_WINDOW);
    file.seek(SeekFrom::Start(byte_pos - window)).ok()?;
    let mut tail = vec![0u8; window as usize];
    std::io::Read::read_exact(&mut file, &mut tail).ok()?;
    Some(fnv1a64(&tail) as i64)
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
) -> Result<Option<JsonlScanOutcome>, AppError>
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

    // 字节续传决策（指纹校验可能移动过游标，非 Resume 路径必须归零）
    let hint = load_matching_resume_hint(resume, &file_path_str, last_modified, last_offset);
    let decision = decide_resume::<S>(&mut file, file_len, hint);

    // effective_last_offset：本轮用于 is_new 判断的行 offset。文件身份失效
    // 时权威行 offset 描述的是旧文件，必须归零全量重扫，否则新文件的前
    // N 行会被误当作旧行永久漏导入。
    let (mut state, mut line_offset, mut byte_pos, effective_last_offset) = match decision {
        ResumeDecision::Resume { byte_offset, state } => {
            (state, last_offset, byte_offset, last_offset)
        }
        ResumeDecision::RescanFromZero => {
            file.seek(SeekFrom::Start(0))
                .map_err(|e| AppError::Config(format!("无法定位文件偏移: {e}")))?;
            (init_state(), 0i64, 0u64, 0i64)
        }
        ResumeDecision::LineSkipFallback => {
            file.seek(SeekFrom::Start(0))
                .map_err(|e| AppError::Config(format!("无法定位文件偏移: {e}")))?;
            (init_state(), 0i64, 0u64, last_offset)
        }
    };

    // 持久化进度只推进到换行边界；末尾不完整行进回调但不进进度
    let mut committed_line_offset = line_offset;
    let mut committed_byte_pos = byte_pos;
    let mut resume_state_json: Option<String> = None;

    let mut reader = BufReader::new(file);
    let mut raw: Vec<u8> = Vec::new();

    loop {
        raw.clear();
        // read_until 精确返回消耗的字节数（含换行符），字节位置始终可信。
        // 非 EOF 的 IO 错误必须整体报错：若吞掉错误并返回部分结果，调用方
        // 会把本轮 mtime 写入权威进度，错误点之后的行将永久不再导入。
        // 报错让调用方跳过进度更新，下个周期从旧进度完整重试。
        let n = match reader.read_until(b'\n', &mut raw) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) => {
                return Err(AppError::Config(format!(
                    "读取会话日志失败 ({file_path_str}): {e}"
                )))
            }
        };
        byte_pos += n as u64;
        line_offset += 1;
        let is_new = line_offset > effective_last_offset;

        if raw.last() == Some(&b'\n') {
            committed_byte_pos = byte_pos;
            committed_line_offset = line_offset;
        } else if resume_state_json.is_none() {
            // 第一次遇到不完整尾行：先固化换行边界处的状态机快照，再让该
            // 行进回调。回调可能据此导入（写满但缺换行的最终行必须导入），
            // 但持久化的 (进度, 状态) 停在边界，下个周期从边界重读该行，
            // 各 app 的 request_id 去重保证不会重复入库。
            resume_state_json = serde_json::to_string(&state).ok();
        }

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

    // 无不完整尾行时，边界快照就是最终状态
    if resume_state_json.is_none() {
        resume_state_json = serde_json::to_string(&state).ok();
    }

    Ok(Some(JsonlScanOutcome {
        line_offset: committed_line_offset,
        byte_pos: committed_byte_pos,
        file_modified,
        resume_state_json,
    }))
}

/// 主库进度提交成功后，把字节位置、状态机快照与尾部指纹写回 sidecar
/// （尽力而为，失败只损失下次的续传加速，不影响正确性）。
pub(crate) fn save_resume_hint(
    resume: Option<&ScanCacheStore>,
    file_path_str: &str,
    outcome: &JsonlScanOutcome,
) {
    let Some(store) = resume else {
        return;
    };
    let hint = SyncResumeHint {
        file_path: file_path_str.to_string(),
        last_modified: outcome.file_modified,
        last_line_offset: outcome.line_offset,
        byte_offset: outcome.byte_pos as i64,
        state: outcome.resume_state_json.clone(),
        tail_hash: compute_tail_hash(file_path_str, outcome.byte_pos),
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

    /// 测试用状态机：空壳，仅满足续传往返的 serde 约束；回调观察结果由
    /// 调用方通过外部缓冲捕获（观察记录不是跨轮解析状态，不应进提示）。
    #[derive(Debug, Default, Serialize, Deserialize)]
    struct NoState;

    /// 一次扫描的观察结果：outcome + 回调看到的每一行及其 is_new 标记。
    struct Observed {
        outcome: Option<JsonlScanOutcome>,
        seen: Vec<(String, bool)>,
    }

    impl Observed {
        fn out(&self) -> &JsonlScanOutcome {
            self.outcome.as_ref().expect("changed")
        }
    }

    /// `file_mtime` 显式传入（模拟 walk 阶段取得的值）：测试不依赖真实文件
    /// 系统时间戳在两次写入之间前进，避免时间粒度导致的偶发跳过。
    fn scan_at(
        path: &std::path::Path,
        file_mtime: i64,
        last_modified: i64,
        last_offset: i64,
        resume: Option<&ScanCacheStore>,
    ) -> Observed {
        let mut seen = Vec::new();
        let outcome = scan_jsonl_incremental(
            path,
            file_mtime,
            last_modified,
            last_offset,
            resume,
            NoState::default,
            |_state, line, is_new| seen.push((line.to_string(), is_new)),
        )
        .expect("scan");
        Observed { outcome, seen }
    }

    #[test]
    fn first_scan_reads_all_lines_and_reports_positions() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("s.jsonl");
        std::fs::write(&path, "l1\nl2\n").expect("write");

        let outcome = scan_at(&path, 0, 0, 0, None);
        assert_eq!(
            outcome.seen,
            vec![("l1".to_string(), true), ("l2".to_string(), true)]
        );
        assert_eq!(outcome.out().line_offset, 2);
        assert_eq!(outcome.out().byte_pos, 6);
        assert!(outcome.out().file_modified > 0);
    }

    #[test]
    fn unchanged_file_is_skipped() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("s.jsonl");
        std::fs::write(&path, "l1\n").expect("write");
        // mtime 未超过已记录的 last_modified → 跳过
        assert!(scan_at(&path, 5, 5, 1, None).outcome.is_none());
    }

    #[test]
    fn resume_seeks_past_history_even_when_head_bytes_change() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("s.jsonl");
        // 行足够长，让头部破坏落在尾部指纹窗口（64 字节）之外
        let l1 = "a".repeat(80);
        let l2 = "b".repeat(80);
        std::fs::write(&path, format!("{l1}\n{l2}\n")).expect("write");
        let store = ScanCacheStore::in_memory().expect("store");

        let first = scan_at(&path, 1_000, 0, 0, Some(&store));
        assert_eq!(first.out().byte_pos, 162);
        save_resume_hint(Some(&store), &path.to_string_lossy(), first.out());

        // 破坏头部但保持总字节数不变：把第一个换行符改成空格，两行并作一行。
        // 行式回退路径会因行号偏移而错跳新行；字节续传路径完全不受影响。
        std::fs::write(&path, format!("{l1} {l2}\nl3\n")).expect("rewrite");

        let second = scan_at(
            &path,
            2_000,
            first.out().file_modified,
            first.out().line_offset,
            Some(&store),
        );
        assert_eq!(second.seen, vec![("l3".to_string(), true)]);
        assert_eq!(second.out().line_offset, first.out().line_offset + 1);
        assert_eq!(second.out().byte_pos, 165);
    }

    #[test]
    fn partial_tail_line_does_not_advance_persisted_progress() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("s.jsonl");
        // 末行没有换行符：可能是写到一半，也可能是永远不带换行的最终行
        std::fs::write(&path, "l1\nl2").expect("write");
        let store = ScanCacheStore::in_memory().expect("store");

        let first = scan_at(&path, 1_000, 0, 0, Some(&store));
        // 不完整行仍进回调（写满但缺换行的最终行必须能导入）……
        assert_eq!(
            first.seen,
            vec![("l1".to_string(), true), ("l2".to_string(), true)]
        );
        // ……但持久化进度停在换行边界
        assert_eq!(first.out().line_offset, 1);
        assert_eq!(first.out().byte_pos, 3);
        save_resume_hint(Some(&store), &path.to_string_lossy(), first.out());

        // 半行被补全并追加新行（append-only，前缀字节不变）
        std::fs::write(&path, "l1\nl2-completed\nl3\n").expect("complete");

        let second = scan_at(
            &path,
            2_000,
            first.out().file_modified,
            first.out().line_offset,
            Some(&store),
        );
        // 从边界续读：补全后的完整行与新行都被处理，没有漏也没有错位
        assert_eq!(
            second.seen,
            vec![("l2-completed".to_string(), true), ("l3".to_string(), true)]
        );
        assert_eq!(second.out().line_offset, 3);
        assert_eq!(second.out().byte_pos, 19);
    }

    #[test]
    fn rewritten_larger_file_invalidates_hint() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("s.jsonl");
        let l1 = "a".repeat(80);
        std::fs::write(&path, format!("{l1}\n")).expect("write");
        let store = ScanCacheStore::in_memory().expect("store");

        let first = scan_at(&path, 1_000, 0, 0, Some(&store));
        save_resume_hint(Some(&store), &path.to_string_lossy(), first.out());

        // 同路径整体重写成"更大"的文件：size/offset 校验都能通过，
        // 只有尾部指纹能识破 → 必须回退从头扫描
        let rewritten = "z".repeat(200);
        std::fs::write(&path, format!("{rewritten}\n")).expect("rotate");

        let second = scan_at(
            &path,
            2_000,
            first.out().file_modified,
            first.out().line_offset,
            Some(&store),
        );
        // 指纹识破身份失效 → 忽略旧行 offset，全量重扫（is_new=true，
        // 新文件内容不会被误当旧行漏掉；重复导入由 request_id 去重兜底）
        assert_eq!(second.seen, vec![(rewritten, true)]);
        assert_eq!(second.out().line_offset, 1);
    }

    #[test]
    fn mismatched_hint_falls_back_to_line_skip() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("s.jsonl");
        std::fs::write(&path, "l1\nl2\n").expect("write");
        let store = ScanCacheStore::in_memory().expect("store");

        let first = scan_at(&path, 1_000, 0, 0, Some(&store));
        let path_str = path.to_string_lossy().to_string();
        save_resume_hint(Some(&store), &path_str, first.out());

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
            first.out().file_modified,
            first.out().line_offset,
            Some(&store),
        );
        assert_eq!(
            second.seen,
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

        let first = scan_at(&path, 1_000, 0, 0, Some(&store));
        let path_str = path.to_string_lossy().to_string();
        save_resume_hint(Some(&store), &path_str, first.out());

        // 文件被截断重写：长度小于提示的字节位置 → 身份失效，全量重扫
        std::fs::write(&path, "x\n").expect("truncate");
        let second = scan_at(
            &path,
            2_000,
            first.out().file_modified,
            first.out().line_offset,
            Some(&store),
        );
        // 新文件的行以 is_new=true 全量重放，不会被旧行 offset 误跳
        assert_eq!(second.seen, vec![("x".to_string(), true)]);
        assert_eq!(second.out().line_offset, 1);
    }
}

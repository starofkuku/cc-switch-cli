use crate::cli::i18n::texts;
use crate::error::AppError;
use crate::services::SyncDecision;
use crate::settings::{
    get_webdav_sync_settings, set_webdav_sync_settings, webdav_jianguoyun_preset,
    WebDavSyncSettings,
};

use super::super::app::{
    App, ConfirmAction, ConfirmOverlay, LoadingKind, Overlay, SessionsPane, ToastKind,
};
use super::super::data::{load_state, UiData};
use super::super::runtime_actions::app_display_name;
use super::super::CacheInvalidation;
use super::types::{
    build_stream_check_result_lines, LocalEnvMsg, ManagedAuthMsg, ModelFetchMsg, ProxyMsg,
    QuotaMsg, RequestTracker, SessionMsg, SkillsMsg, SpeedtestMsg, StreamCheckMsg, UpdateMsg,
    WebDavDone, WebDavErr, WebDavMsg, WebDavReqKind,
};

pub(crate) fn handle_stream_check_msg(app: &mut App, msg: StreamCheckMsg) {
    match msg {
        StreamCheckMsg::Finished { req, result } => match result {
            Ok(result) => {
                let lines = build_stream_check_result_lines(&req.provider_name, &result);
                match &app.overlay {
                    Overlay::StreamCheckRunning { provider_id, .. }
                        if provider_id == &req.provider_id =>
                    {
                        app.overlay = Overlay::StreamCheckResult {
                            provider_name: req.provider_name,
                            lines,
                            scroll: 0,
                        };
                    }
                    _ => {
                        app.push_toast(
                            texts::tui_toast_stream_check_finished(),
                            ToastKind::Success,
                        );
                    }
                }
            }
            Err(err) => {
                app.push_toast(texts::tui_toast_stream_check_failed(&err), ToastKind::Error);
                if matches!(&app.overlay, Overlay::StreamCheckRunning { provider_id, .. } if provider_id == &req.provider_id)
                {
                    app.overlay = Overlay::None;
                }
            }
        },
    }
}

pub(crate) fn handle_speedtest_msg(app: &mut App, msg: SpeedtestMsg) {
    match msg {
        SpeedtestMsg::Finished { url, result } => match result {
            Ok(rows) => {
                let mut lines = vec![texts::tui_speedtest_line_url(&url), String::new()];
                for row in rows {
                    let latency = row
                        .latency
                        .map(texts::tui_latency_ms)
                        .unwrap_or_else(|| texts::tui_na().to_string());
                    let status = row
                        .status
                        .map(|v| v.to_string())
                        .unwrap_or_else(|| texts::tui_na().to_string());
                    let err = row.error.unwrap_or_default();

                    lines.push(texts::tui_speedtest_line_latency(&latency));
                    lines.push(texts::tui_speedtest_line_status(&status));
                    if !err.trim().is_empty() {
                        lines.push(texts::tui_speedtest_line_error(&err));
                    }
                }

                match &app.overlay {
                    Overlay::SpeedtestRunning { url: running_url } if running_url == &url => {
                        app.overlay = Overlay::SpeedtestResult {
                            url,
                            lines,
                            scroll: 0,
                        };
                    }
                    _ => {
                        app.push_toast(texts::tui_toast_speedtest_finished(), ToastKind::Success);
                    }
                }
            }
            Err(err) => {
                app.push_toast(texts::tui_toast_speedtest_failed(&err), ToastKind::Error);
                if matches!(&app.overlay, Overlay::SpeedtestRunning { url: running_url } if running_url == &url)
                {
                    app.overlay = Overlay::None;
                }
            }
        },
    }
}

pub(crate) fn handle_local_env_msg(app: &mut App, msg: LocalEnvMsg) {
    match msg {
        LocalEnvMsg::Finished { result } => {
            app.local_env_results = result;
            app.local_env_loading = false;
        }
    }
}

#[derive(Debug)]
struct SessionIdentity {
    provider_id: String,
    session_id: String,
    source_path: Option<String>,
    recency: i64,
}

impl SessionIdentity {
    fn capture(row: &crate::session_manager::SessionMeta) -> Self {
        Self {
            provider_id: row.provider_id.clone(),
            session_id: row.session_id.clone(),
            source_path: row.source_path.clone(),
            recency: row.last_active_at.or(row.created_at).unwrap_or(0),
        }
    }

    fn matches(&self, row: &crate::session_manager::SessionMeta) -> bool {
        self.provider_id == row.provider_id
            && self.session_id == row.session_id
            && self.source_path == row.source_path
    }

    fn key(&self) -> String {
        format!(
            "{}:{}:{}",
            self.provider_id,
            self.session_id,
            self.source_path.as_deref().unwrap_or_default()
        )
    }
}

fn find_session_identity(
    rows: &super::super::app::SessionRowsView<'_>,
    identity: &SessionIdentity,
) -> Option<usize> {
    fn in_recency_order(
        rows: &[crate::session_manager::SessionMeta],
        identity: &SessionIdentity,
    ) -> Option<usize> {
        // Authoritative rows are sorted by descending recency. Narrow the
        // identity lookup to the equal-timestamp run so restoring a row at
        // the bottom of a million-row list stays O(log N) in the common case.
        let start = rows.partition_point(|row| {
            row.last_active_at.or(row.created_at).unwrap_or(0) > identity.recency
        });
        let equal_len = rows[start..].partition_point(|row| {
            row.last_active_at.or(row.created_at).unwrap_or(0) == identity.recency
        });
        rows[start..start + equal_len]
            .iter()
            .position(|row| identity.matches(row))
            .map(|offset| start + offset)
    }

    match rows {
        super::super::app::SessionRowsView::All(rows) => {
            in_recency_order(rows, identity).or_else(|| {
                // Cached/partial first paints are capped at 101 rows and may
                // arrive in discovery order. A bounded fallback makes those
                // sources order-agnostic without reintroducing a million-row
                // UI scan when an identity disappears from authoritative data.
                (rows.len() <= crate::session_manager::SCAN_CACHE_FIRST_PAINT_LIMIT)
                    .then(|| rows.iter().position(|row| identity.matches(row)))
                    .flatten()
            })
        }
        super::super::app::SessionRowsView::Filtered { rows, indices } => {
            if let Some(row_index) = in_recency_order(rows, identity) {
                return indices.binary_search(&row_index).ok();
            }
            (indices.len() <= crate::session_manager::SCAN_CACHE_FIRST_PAINT_LIMIT)
                .then(|| {
                    indices.iter().position(|row_index| {
                        rows.get(*row_index)
                            .is_some_and(|row| identity.matches(row))
                    })
                })
                .flatten()
        }
    }
}

#[derive(Debug)]
struct SessionSelectionAnchor {
    identity: Option<SessionIdentity>,
    index: usize,
    had_detail: bool,
}

fn visible_session_rows(app: &App) -> super::super::app::SessionRowsView<'_> {
    super::super::app::visible_sessions_for_state(
        &app.filter,
        &app.app_type,
        app.sessions.show_all_providers,
        app.sessions.provider_id.as_deref(),
        &app.sessions.rows,
        app.sessions.detail_key.as_deref(),
        app.sessions.messages_loaded,
        &app.sessions.messages,
        app.sessions.deep_search_query.as_deref(),
        &app.sessions.deep_search_results,
        app.sessions
            .materialized_query_is_current(app.filter.query_lower().as_deref()),
        app.sessions.rows_revision,
        app.sessions.messages_revision,
        app.sessions.deep_search_seq,
        &app.sessions.visibility_cache,
    )
}

fn capture_session_selection(app: &App) -> SessionSelectionAnchor {
    let rows = visible_session_rows(app);
    SessionSelectionAnchor {
        identity: rows
            .get(app.sessions.selected_idx)
            .map(SessionIdentity::capture),
        index: app.sessions.selected_idx,
        had_detail: app.sessions.detail_key.is_some(),
    }
}

/// Reconcile a row replacement against a stable session key instead of keeping
/// the old numeric index. Async snapshots and provider partials can reorder the
/// entire list, so retaining the index would silently move the highlight to a
/// different session while the detail pane still showed the old one.
fn restore_session_selection(app: &mut App, anchor: SessionSelectionAnchor) {
    // Locate the previous identity in one pass using field comparisons. The old
    // code built a composite String for every row in two separate scans, which
    // made an async update O(N) allocations on a million-row list.
    let (target_identity, initial_index, initial_len) = {
        let rows = visible_session_rows(app);
        if rows.is_empty() {
            (None, 0, 0)
        } else {
            let preserved = anchor.identity.as_ref().and_then(|identity| {
                find_session_identity(&rows, identity)
                    .and_then(|index| rows.get(index).map(|row| (index, row)))
            });
            let (index, row) = preserved.unwrap_or_else(|| {
                let index = anchor.index.min(rows.len() - 1);
                (
                    index,
                    rows.get(index).expect("clamped visible session index"),
                )
            });
            (Some(SessionIdentity::capture(row)), index, rows.len())
        }
    };
    let target_key = target_identity.as_ref().map(SessionIdentity::key);
    // Loaded messages belong to exactly one session. If the stable selection
    // disappeared (or an earlier inconsistency already existed), dropping the
    // detail is safer than rendering session B highlighted beside session A's
    // messages. A vanished detail pane returns to the list.
    let detail_cleared = if anchor.had_detail && app.sessions.detail_key.is_none() {
        app.sessions.pane = SessionsPane::List;
        false
    } else if app.sessions.detail_key.as_deref() != target_key.as_deref()
        && app.sessions.detail_key.is_some()
    {
        app.sessions.clear_detail();
        app.sessions.pane = SessionsPane::List;
        true
    } else {
        false
    };

    // Clearing a mismatched detail can change a filtered view because a loaded
    // message match participates in filtering. Only that uncommon branch needs
    // another no-allocation lookup; the common path reuses the first pass.
    let (visible_len, selected_idx) = if detail_cleared {
        let rows = visible_session_rows(app);
        let selected_idx = if rows.is_empty() {
            0
        } else {
            target_identity
                .as_ref()
                .and_then(|identity| find_session_identity(&rows, identity))
                .unwrap_or_else(|| anchor.index.min(rows.len() - 1))
        };
        (rows.len(), selected_idx)
    } else {
        (initial_len, initial_index)
    };
    app.sessions.selected_idx = selected_idx;
    let _ = visible_len;
    let absolute = app
        .sessions
        .remote
        .current_page()
        .saturating_mul(crate::session_manager::paged_manifest::PAGE_SIZE)
        .saturating_add(selected_idx);
    let total_rows = app.sessions.logical_total_rows();
    app.sessions.pagination.sync_len(total_rows);
    if total_rows > 0 {
        app.sessions.pagination.select(absolute);
    }
}

pub(crate) fn handle_session_msg(app: &mut App, msg: SessionMsg) {
    match msg {
        SessionMsg::ScopeOpened {
            request_id,
            scope_epoch,
            scope,
            result,
        } => {
            if app.sessions.scan_active != Some(request_id)
                || app.sessions.scope_epoch != scope_epoch
            {
                return;
            }
            match result {
                Ok(Some((reader, page))) => {
                    if app.sessions.scope_cache_is_invalidated(&scope) {
                        super::super::app::retire_session_rows(page.rows);
                        return;
                    }
                    app.sessions.remember_base_manifest(
                        scope_epoch,
                        &scope,
                        page.generation.clone(),
                        page.total_rows,
                        reader.clone(),
                    );
                    let keep_query_visible = app.sessions.page_token().is_some_and(|token| {
                        token.source == super::super::app::SessionPageSource::Query
                    }) && app.sessions.materialized_query.is_some();
                    if keep_query_visible {
                        super::super::app::retire_session_rows(page.rows);
                    } else {
                        app.sessions.apply_opened_manifest(
                            scope_epoch,
                            &scope,
                            page.generation,
                            page.total_rows,
                            page.page_index,
                            page.rows,
                            reader,
                        );
                    }
                }
                Ok(None) => {}
                Err(error) => {
                    app.sessions.last_error = Some(error);
                }
            }
            let query_already_materialized = app
                .sessions
                .materialized_query_is_current(app.sessions.deep_search_query.as_deref());
            if app.sessions.deep_search_active.is_none() && !query_already_materialized {
                if let Some(query) = app.sessions.deep_search_query.clone() {
                    if !query.is_empty() && app.sessions.base_manifest.is_some() {
                        app.pending_deep_search = Some(query);
                    }
                }
            }
        }
        SessionMsg::ScanProvisional {
            request_id,
            scope_epoch,
            scope,
            rows,
        } => {
            app.sessions
                .apply_provisional_page(request_id, scope_epoch, &scope, rows);
        }
        SessionMsg::ManifestPublished {
            request_id,
            scope_epoch,
            scope,
            result,
        } => match result {
            Ok((published, reader)) => {
                if app.sessions.scan_active != Some(request_id)
                    || app.sessions.scope_epoch != scope_epoch
                {
                    super::super::app::retire_session_rows(published.first_page.rows);
                    return;
                }
                let safe_tombstones = app
                    .sessions
                    .take_scan_tombstones_for_manifest(request_id, &scope);
                app.sessions.remember_base_manifest(
                    scope_epoch,
                    &scope,
                    published.generation.clone(),
                    published.total_rows,
                    reader.clone(),
                );
                let desired_query = app
                    .sessions
                    .deep_search_query
                    .clone()
                    .filter(|query| !query.is_empty());
                if app.sessions.page_token().is_some() && desired_query.is_some() {
                    super::super::app::retire_session_rows(published.first_page.rows);
                    app.sessions
                        .mark_query_tombstones_for_rebuild(safe_tombstones);
                    app.sessions.scan_active = None;
                    app.sessions.loading = false;
                    app.sessions.deep_search_active = None;
                    app.pending_deep_search = desired_query;
                    return;
                }
                if app.sessions.page_token().is_none() {
                    let applied = app.sessions.apply_opened_manifest(
                        scope_epoch,
                        &scope,
                        published.generation,
                        published.total_rows,
                        published.first_page.page_index,
                        published.first_page.rows,
                        reader,
                    );
                    if applied {
                        app.sessions
                            .clear_safe_manifest_tombstones(&scope, safe_tombstones);
                    }
                    app.sessions.scan_active = None;
                    app.sessions.loading = false;
                } else {
                    if app.sessions.stage_manifest(
                        request_id,
                        scope_epoch,
                        &scope,
                        published.generation,
                        published.total_rows,
                        published.first_page.rows,
                        reader,
                    ) {
                        app.sessions
                            .attach_pending_manifest_tombstones(safe_tombstones);
                    }
                }
                if let Some(query) = desired_query {
                    app.sessions.deep_search_active = None;
                    app.pending_deep_search = Some(query);
                }
            }
            Err(error) => {
                if app.sessions.scan_active != Some(request_id) {
                    return;
                }
                app.sessions.fail_scan(request_id, error.clone());
                app.push_toast(
                    texts::tui_sessions_toast_refresh_failed(&error),
                    ToastKind::Warning,
                );
            }
        },
        SessionMsg::PageLoaded {
            request_id,
            token,
            page,
            result,
        } => match result {
            Ok(loaded) => {
                if loaded.generation != token.generation {
                    super::super::app::retire_session_rows(loaded.rows);
                    return;
                }
                app.sessions
                    .finish_page_request(request_id, &token, page, loaded.rows);
            }
            Err(error) => {
                app.sessions
                    .fail_page_request(request_id, &token, page, error);
            }
        },
        SessionMsg::ManifestLocated {
            request_id,
            scope_epoch,
            generation,
            result,
        } => match result {
            Ok((reader, page, selected_local)) => {
                app.sessions.finish_manifest_reconcile(
                    request_id,
                    scope_epoch,
                    &generation,
                    page.page_index,
                    page.rows,
                    selected_local,
                    reader,
                );
            }
            Err(error) => {
                app.sessions
                    .fail_manifest_reconcile(request_id, scope_epoch, &generation, error);
            }
        },
        SessionMsg::MessagesLoaded {
            request_id,
            key,
            result,
        } => match result {
            Ok(batch) => {
                if !app.sessions.message_load_is_current(request_id, &key) {
                    super::super::app::retire_session_messages(batch.messages);
                    return;
                }
                if app.sessions.finish_message_load(request_id, &key, batch) {
                    crate::cli::tui::app::clamp_session_message_selection(&mut app.sessions);
                }
            }
            Err(error) => {
                if !app.sessions.message_load_is_current(request_id, &key) {
                    return;
                }
                app.sessions
                    .fail_message_load(request_id, &key, error.clone());
                app.push_toast(
                    texts::tui_sessions_toast_messages_failed(&error),
                    ToastKind::Warning,
                );
            }
        },
        SessionMsg::DeleteFinished {
            request_id,
            key,
            result,
        } => match result {
            Ok(()) => {
                // The delete reaches the UI before manifest repacking by
                // design. Keep a tombstone across that gap so an old pinned
                // page or a scan that read the file earlier cannot resurrect
                // the row.
                let selection = capture_session_selection(app);
                if app.sessions.finish_delete(request_id, &key) {
                    restore_session_selection(app, selection);
                    // Keep the identity tombstoned until the asynchronously
                    // repacked source is installed. This also protects page
                    // loads pinned to the old generation while repacking runs.
                    app.sessions
                        .register_purge_tombstone(request_id, key.clone());
                    app.push_toast(
                        texts::tui_sessions_toast_delete_finished(),
                        ToastKind::Success,
                    );
                }
            }
            Err(error) => {
                if !app.sessions.delete_active.contains(&request_id) {
                    return;
                }
                app.sessions.fail_delete(request_id);
                app.push_toast(
                    texts::tui_sessions_toast_delete_failed(&error),
                    ToastKind::Warning,
                );
            }
        },
        SessionMsg::ManifestsPurged {
            request_id,
            key,
            base,
        } => {
            // `DeleteFinished` is enqueued before the purge thread is spawned.
            // Reject any malformed/out-of-order result rather than clearing a
            // tombstone that has not been acknowledged by the UI.
            if !app.sessions.purge_tombstone_is_current(request_id, &key) {
                for (_, published, _) in base {
                    super::super::app::retire_session_rows(published.first_page.rows);
                }
                return;
            }
            let current_scope = app.sessions.provider_id.clone();
            let mut current_publication = None;
            for (scope, published, reader) in base {
                if current_scope.as_deref() == Some(scope.as_str()) {
                    current_publication = Some((published, reader));
                } else {
                    super::super::app::retire_session_rows(published.first_page.rows);
                    // No reader for a different scope remains active in this
                    // TUI. Its atomically published pointer is therefore a
                    // complete convergence point for that scope.
                    app.sessions
                        .clear_published_purge_scope(request_id, &key, &scope);
                }
            }
            let Some(scope) = current_scope else {
                return;
            };
            let Some((published, reader)) = current_publication else {
                if app
                    .sessions
                    .purge_tombstone_applies_to_scope(request_id, &key, &scope)
                {
                    // No safe generation was produced (cache miss or purge
                    // failure). Keep the exact tombstone and request one fresh
                    // scan; the event loop consumes this flag only once.
                    app.sessions.require_purge_refresh();
                }
                return;
            };
            let _ = app.sessions.remember_base_manifest(
                app.sessions.scope_epoch,
                &scope,
                published.generation.clone(),
                published.total_rows,
                reader,
            );
            super::super::app::retire_session_rows(published.first_page.rows);
            let Some(base) = app.sessions.base_manifest.clone() else {
                app.sessions.require_purge_refresh();
                return;
            };
            let query_is_visible =
                app.sessions.page_token().is_some_and(|token| {
                    token.source == super::super::app::SessionPageSource::Query
                }) && app.sessions.materialized_query.is_some();
            if query_is_visible {
                // Query caches are private to this TUI and are never adopted
                // from a global purge. Keep the old query pinned/tombstoned
                // until the same query is rebuilt from the newest base.
                app.sessions
                    .mark_query_tombstone_for_rebuild(request_id, key);
                if let Some(query) = app.sessions.deep_search_query.clone() {
                    app.sessions.deep_search_active = None;
                    app.pending_deep_search = Some(query);
                }
            } else {
                if !app.sessions.stage_purged_manifest(
                    super::super::app::SessionPageSource::Base,
                    app.sessions.scope_epoch,
                    &scope,
                    base.generation,
                    base.total_rows,
                    Vec::new(),
                    base.reader,
                    request_id,
                    key,
                ) {
                    app.sessions.require_purge_refresh();
                }
            }
        }
        SessionMsg::QueryPublished {
            request_id,
            scope_epoch,
            scope,
            base_generation,
            query,
            query_namespace,
            result,
        } => {
            let normalized_query = query.trim().to_lowercase();
            let current_base = app.sessions.base_manifest.as_ref();
            let current = app.sessions.deep_search_active == Some(request_id)
                && app.sessions.scope_epoch == scope_epoch
                && app.sessions.provider_id.as_deref() == Some(scope.as_str())
                && app.sessions.query_namespace == query_namespace
                && app.sessions.deep_search_query.as_deref() == Some(normalized_query.as_str())
                && current_base.is_some_and(|base| {
                    base.scope_epoch == scope_epoch
                        && base.scope == scope
                        && base.generation == base_generation
                });
            if !current {
                if let Ok((published, _reader)) = result {
                    super::super::app::retire_session_rows(published.first_page.rows);
                }
                // The same query may have completed against a base generation
                // superseded by refresh. Re-run it against the saved newest base.
                if app.sessions.scope_epoch == scope_epoch
                    && app.sessions.query_namespace == query_namespace
                    && app.sessions.deep_search_query.as_deref() == Some(normalized_query.as_str())
                {
                    app.sessions.deep_search_active = None;
                    app.pending_deep_search = Some(normalized_query);
                }
                return;
            }
            app.sessions.deep_search_active = None;
            app.sessions.clear_deep_search_results();
            match result {
                Ok((published, reader)) => {
                    app.sessions.apply_query_manifest(
                        scope_epoch,
                        &scope,
                        &base_generation,
                        normalized_query,
                        published.generation,
                        published.total_rows,
                        published.first_page.page_index,
                        published.first_page.rows,
                        reader,
                    );
                }
                Err(error) => {
                    if app.sessions.has_query_tombstones_to_clear() {
                        // The rebuilt query was the intended convergence point
                        // for one or more deletes. Fall back to the already
                        // published safe base instead of pinning the old query
                        // (and its UI tombstones) indefinitely.
                        app.sessions.deep_search_query = None;
                        app.pending_deep_search = None;
                        if !app.sessions.stage_base_restore() {
                            app.sessions.require_purge_refresh();
                        }
                    }
                    if app.sessions.page_token().is_none() {
                        app.sessions.last_error = Some(error.clone());
                    }
                    app.push_toast(
                        texts::tui_sessions_toast_refresh_failed(&error),
                        ToastKind::Warning,
                    );
                }
            }
        }
    }
}

pub(crate) fn handle_quota_msg(app: &mut App, data: &mut UiData, msg: QuotaMsg) {
    match msg {
        QuotaMsg::Finished { target, result } => {
            if !data.quota.target_is_current(&target) {
                return;
            }

            let was_manual = data.quota.has_manual_loading(&target);
            match result {
                Ok(quota) => {
                    let provider_name = target.provider_name.clone();
                    data.quota.finish(target, quota);
                    if was_manual {
                        app.push_toast(
                            texts::tui_toast_quota_refresh_finished(&provider_name),
                            ToastKind::Success,
                        );
                    }
                }
                Err(error) => {
                    data.quota.finish_error(target, error.clone());
                    app.push_toast(
                        texts::tui_toast_quota_refresh_failed(&error),
                        ToastKind::Warning,
                    );
                }
            }
        }
    }
}

pub(crate) fn handle_model_fetch_msg(app: &mut App, msg: ModelFetchMsg) {
    match msg {
        ModelFetchMsg::Finished {
            request_id,
            field,
            claude_idx,
            result,
        } => {
            if let Overlay::ModelFetchPicker {
                request_id: current_request_id,
                fetching: ref mut f,
                models: ref mut m,
                error: ref mut e,
                field: ref current_field,
                claude_idx: ref current_claude_idx,
                ..
            } = app.overlay
            {
                if current_request_id != request_id {
                    return;
                }
                if current_field == &field && current_claude_idx == &claude_idx {
                    *f = false;
                    match result {
                        Ok(fetched_models) => {
                            if fetched_models.is_empty() {
                                *e = Some(texts::tui_model_fetch_no_models().to_string());
                            } else {
                                *m = fetched_models;
                                *e = None;
                            }
                        }
                        Err(err) => {
                            *e = Some(texts::tui_model_fetch_error_hint(&err));
                        }
                    }
                }
            }
        }
    }
}

pub(crate) fn handle_managed_auth_msg(app: &mut App, msg: ManagedAuthMsg) {
    match msg {
        ManagedAuthMsg::Status {
            auth_provider,
            result,
        } => {
            app.managed_auth_loading = false;
            match result {
                Ok(status) => {
                    app.managed_auth_status = Some(status);
                }
                Err(err) => {
                    app.push_toast(
                        texts::tui_toast_managed_auth_refresh_failed(&err),
                        ToastKind::Warning,
                    );
                    if app
                        .managed_auth_status
                        .as_ref()
                        .is_none_or(|status| status.provider != auth_provider)
                    {
                        app.managed_auth_status = None;
                    }
                }
            }
        }
        ManagedAuthMsg::LoginStarted {
            auth_provider,
            result,
        } => {
            app.managed_auth_loading = false;
            match result {
                Ok(device) => {
                    let now = app.tick;
                    let expires_ticks = seconds_to_tui_ticks(device.expires_in).max(1);
                    let interval_ticks = seconds_to_tui_ticks(device.interval).max(1);
                    let user_code = device.user_code.clone();
                    let verification_uri = device.verification_uri.clone();
                    app.managed_auth_login = Some(crate::cli::tui::app::ManagedAuthLoginState {
                        auth_provider,
                        device_code: device.device_code,
                        expires_at_tick: now.saturating_add(expires_ticks),
                        poll_interval_ticks: interval_ticks,
                        next_poll_tick: now,
                    });
                    app.push_persistent_toast(
                        texts::tui_toast_managed_auth_login_in_progress(
                            &user_code,
                            &verification_uri,
                        ),
                        ToastKind::Info,
                    );
                }
                Err(err) => {
                    app.managed_auth_login = None;
                    app.clear_managed_auth_login_toast();
                    app.clear_managed_auth_cancel_confirm();
                    app.push_toast(
                        texts::tui_toast_managed_auth_login_failed(&err),
                        ToastKind::Error,
                    );
                }
            }
        }
        ManagedAuthMsg::LoginPolled {
            auth_provider,
            device_code,
            result,
        } => match result {
            Ok(Some(account)) => {
                if app
                    .managed_auth_login
                    .as_ref()
                    .is_none_or(|login| login.device_code != device_code)
                {
                    return;
                }
                app.managed_auth_login = None;
                app.managed_auth_loading = false;
                app.clear_managed_auth_login_toast();
                app.clear_managed_auth_cancel_confirm();
                if let Some(status) = app.managed_auth_status.as_mut() {
                    if status.provider == auth_provider {
                        status.authenticated = true;
                        if status.default_account_id.is_none() {
                            status.default_account_id = Some(account.id.clone());
                        }
                        status.accounts.retain(|row| row.id != account.id);
                        status.accounts.insert(0, account.clone());
                    }
                } else {
                    app.managed_auth_status = Some(crate::services::ManagedAuthStatus {
                        provider: auth_provider,
                        authenticated: true,
                        default_account_id: Some(account.id.clone()),
                        migration_error: None,
                        accounts: vec![account.clone()],
                    });
                }
                app.settings_managed_accounts_idx = 0;
                app.push_toast(
                    texts::tui_toast_managed_auth_login_finished(&account.login),
                    ToastKind::Success,
                );
            }
            Ok(None) => {}
            Err(err) => {
                if app
                    .managed_auth_login
                    .as_ref()
                    .is_none_or(|login| login.device_code != device_code)
                {
                    return;
                }
                app.managed_auth_login = None;
                app.managed_auth_loading = false;
                app.clear_managed_auth_login_toast();
                app.clear_managed_auth_cancel_confirm();
                app.push_toast(
                    texts::tui_toast_managed_auth_login_failed(&err),
                    ToastKind::Error,
                );
            }
        },
        ManagedAuthMsg::DefaultSet {
            auth_provider: _,
            account_id: _,
            result,
        } => {
            app.managed_auth_loading = false;
            match result {
                Ok(status) => {
                    app.managed_auth_status = Some(status);
                    app.push_toast(
                        texts::tui_toast_managed_auth_default_updated(),
                        ToastKind::Success,
                    );
                }
                Err(err) => {
                    app.push_toast(
                        texts::tui_toast_managed_auth_default_failed(&err),
                        ToastKind::Error,
                    );
                }
            }
        }
        ManagedAuthMsg::Removed {
            auth_provider: _,
            account_id,
            result,
        } => {
            app.managed_auth_loading = false;
            match result {
                Ok(status) => {
                    app.clear_codex_oauth_binding_if_removed(&account_id);
                    app.managed_auth_status = Some(status);
                    app.push_toast(
                        texts::tui_toast_managed_auth_account_removed(),
                        ToastKind::Success,
                    );
                }
                Err(err) => {
                    app.push_toast(
                        texts::tui_toast_managed_auth_remove_failed(&err),
                        ToastKind::Error,
                    );
                }
            }
        }
    }
}

fn seconds_to_tui_ticks(seconds: u64) -> u64 {
    let millis = seconds.saturating_mul(1000);
    millis.div_ceil(crate::cli::tui::TUI_TICK_RATE.as_millis() as u64)
}

pub(crate) fn handle_skills_msg(
    app: &mut App,
    data: &mut UiData,
    msg: SkillsMsg,
) -> Result<CacheInvalidation, AppError> {
    let mut invalidation = CacheInvalidation::None;
    match msg {
        SkillsMsg::DiscoverFinished {
            request_id,
            query,
            source,
            result,
        } => match result {
            Ok(skills) => {
                if app.skills_discover_active_request_id != Some(request_id) {
                    return Ok(invalidation);
                }
                app.overlay = Overlay::None;
                app.skills_discover_loading = false;
                app.skills_discover_active_request_id = None;
                app.skills_discover_results = skills;
                app.skills_discover_idx = 0;
                app.skills_discover_query = query.clone();
                app.skills_discover_source = source;
                app.skills_discover_cache.insert(
                    (source, query.trim().to_lowercase()),
                    app.skills_discover_results.clone(),
                );
                app.push_toast(
                    texts::tui_toast_skills_discover_finished(app.skills_discover_results.len()),
                    ToastKind::Success,
                );
            }
            Err(err) => {
                if app.skills_discover_active_request_id != Some(request_id) {
                    return Ok(invalidation);
                }
                app.overlay = Overlay::None;
                app.skills_discover_loading = false;
                app.skills_discover_active_request_id = None;
                app.push_toast(
                    texts::tui_toast_skills_discover_failed(&err),
                    ToastKind::Error,
                );
            }
        },
        SkillsMsg::InstallFinished { spec, result } => match result {
            Ok(installed) => {
                app.overlay = Overlay::None;
                *data = UiData::load(&app.app_type)?;
                invalidation = CacheInvalidation::DataReloaded;

                for row in app.skills_discover_results.iter_mut() {
                    if row.key == installed.id
                        || row.directory.eq_ignore_ascii_case(&installed.directory)
                    {
                        row.installed = true;
                    }
                }
                for rows in app.skills_discover_cache.values_mut() {
                    for row in rows.iter_mut() {
                        if row.key == installed.id
                            || row.directory.eq_ignore_ascii_case(&installed.directory)
                        {
                            row.installed = true;
                        }
                    }
                }

                app.push_toast(
                    texts::tui_toast_skill_installed(&installed.directory),
                    ToastKind::Success,
                );
            }
            Err(err) => {
                app.overlay = Overlay::None;
                app.push_toast(
                    texts::tui_toast_skill_install_failed(&spec, &err),
                    ToastKind::Error,
                );
            }
        },
    }

    Ok(invalidation)
}

fn is_webdav_loading_overlay(app: &App) -> bool {
    matches!(
        &app.overlay,
        Overlay::Loading {
            kind: LoadingKind::WebDav,
            ..
        }
    )
}

pub(crate) fn handle_webdav_msg(
    app: &mut App,
    data: &mut UiData,
    webdav_loading: &mut RequestTracker,
    msg: WebDavMsg,
) -> Result<CacheInvalidation, AppError> {
    match msg {
        WebDavMsg::Finished {
            request_id,
            req,
            result,
        } => match result {
            Ok(done) => {
                if webdav_loading.is_stale(request_id) {
                    return Ok(CacheInvalidation::None);
                }

                if webdav_loading.finish_if_active(request_id) && is_webdav_loading_overlay(app) {
                    app.overlay = Overlay::None;
                }

                let done_invalidation = match &done {
                    WebDavDone::Downloaded { decision, .. }
                        if !matches!(decision, SyncDecision::V1MigrationNeeded) =>
                    {
                        CacheInvalidation::AppStateRecreated
                    }
                    WebDavDone::V1Migrated { .. } => CacheInvalidation::AppStateRecreated,
                    _ => CacheInvalidation::DataReloaded,
                };

                match done {
                    WebDavDone::ConnectionChecked => {
                        update_webdav_last_error(None);
                        app.push_toast(texts::tui_toast_webdav_connection_ok(), ToastKind::Success);
                    }
                    WebDavDone::Uploaded { decision, message } => {
                        let msg = match decision {
                            SyncDecision::Upload => texts::tui_toast_webdav_upload_ok().to_string(),
                            _ => message,
                        };
                        app.push_toast(msg, ToastKind::Success);
                    }
                    WebDavDone::Downloaded { decision, message } => {
                        match decision {
                            SyncDecision::V1MigrationNeeded => {
                                app.overlay = Overlay::Confirm(ConfirmOverlay {
                                    title: texts::tui_webdav_v1_migration_title().to_string(),
                                    message: texts::tui_webdav_v1_migration_message().to_string(),
                                    action: ConfirmAction::WebDavMigrateV1ToV2,
                                });
                            }
                            _ => {
                                let msg = match decision {
                                    SyncDecision::Download => {
                                        texts::tui_toast_webdav_download_ok().to_string()
                                    }
                                    _ => message,
                                };
                                if let Ok(state) = load_state() {
                                    if let Err(e) = crate::services::provider::ProviderService::sync_current_to_live(
                                    &state,
                                ) {
                                    log::warn!("WebDAV 下载后同步 live 配置失败: {e}");
                                }
                                }
                                app.push_toast(msg, ToastKind::Success);
                            }
                        }
                    }
                    WebDavDone::V1Migrated { message: _ } => {
                        if let Ok(state) = load_state() {
                            if let Err(e) =
                                crate::services::provider::ProviderService::sync_current_to_live(
                                    &state,
                                )
                            {
                                log::warn!("WebDAV V1 迁移后同步 live 配置失败: {e}");
                            }
                        }
                        app.push_toast(
                            texts::tui_toast_webdav_v1_migration_ok(),
                            ToastKind::Success,
                        );
                    }
                    WebDavDone::JianguoyunConfigured => {
                        app.push_toast(
                            texts::tui_toast_webdav_jianguoyun_configured(),
                            ToastKind::Success,
                        );
                    }
                }
                *data = UiData::load(&app.app_type)?;
                Ok(done_invalidation)
            }
            Err(err) => {
                if webdav_loading.is_stale(request_id) {
                    return Ok(CacheInvalidation::None);
                }

                if webdav_loading.finish_if_active(request_id) && is_webdav_loading_overlay(app) {
                    app.overlay = Overlay::None;
                }
                let error_detail = match &err {
                    WebDavErr::Generic(e)
                    | WebDavErr::QuickSetupSave(e)
                    | WebDavErr::QuickSetupCheck(e) => e.clone(),
                };
                update_webdav_last_error(Some(error_detail));
                let msg = match req {
                    WebDavReqKind::CheckConnection => {
                        let detail = match err {
                            WebDavErr::Generic(e)
                            | WebDavErr::QuickSetupSave(e)
                            | WebDavErr::QuickSetupCheck(e) => e,
                        };
                        texts::tui_toast_webdav_action_failed(
                            texts::tui_webdav_loading_title_check_connection(),
                            &detail,
                        )
                    }
                    WebDavReqKind::Upload => {
                        let detail = match err {
                            WebDavErr::Generic(e)
                            | WebDavErr::QuickSetupSave(e)
                            | WebDavErr::QuickSetupCheck(e) => e,
                        };
                        texts::tui_toast_webdav_action_failed(
                            texts::tui_webdav_loading_title_upload(),
                            &detail,
                        )
                    }
                    WebDavReqKind::Download => {
                        let detail = match err {
                            WebDavErr::Generic(e)
                            | WebDavErr::QuickSetupSave(e)
                            | WebDavErr::QuickSetupCheck(e) => e,
                        };
                        texts::tui_toast_webdav_action_failed(
                            texts::tui_webdav_loading_title_download(),
                            &detail,
                        )
                    }
                    WebDavReqKind::MigrateV1ToV2 => {
                        let detail = match err {
                            WebDavErr::Generic(e)
                            | WebDavErr::QuickSetupSave(e)
                            | WebDavErr::QuickSetupCheck(e) => e,
                        };
                        texts::tui_toast_webdav_action_failed(
                            texts::tui_webdav_loading_title_v1_migration(),
                            &detail,
                        )
                    }
                    WebDavReqKind::JianguoyunQuickSetup { .. } => match err {
                        WebDavErr::QuickSetupCheck(e) => {
                            texts::tui_toast_webdav_quick_setup_failed(&e)
                        }
                        WebDavErr::QuickSetupSave(e) | WebDavErr::Generic(e) => {
                            texts::tui_toast_webdav_action_failed(
                                texts::tui_webdav_loading_title_quick_setup(),
                                &e,
                            )
                        }
                    },
                };
                *data = UiData::load(&app.app_type)?;
                app.push_toast(msg, ToastKind::Error);
                Ok(CacheInvalidation::DataReloaded)
            }
        },
    }
}

pub(crate) fn handle_proxy_msg(
    app: &mut App,
    data: &mut UiData,
    proxy_loading: &mut RequestTracker,
    proxy_snapshot_refresh: &mut RequestTracker,
    msg: ProxyMsg,
) -> Result<CacheInvalidation, AppError> {
    let mut invalidation = CacheInvalidation::None;
    match msg {
        ProxyMsg::ManagedSessionFinished {
            request_id,
            app_type,
            enabled,
            result,
        } => {
            if !proxy_loading.finish_if_active(request_id) {
                return Ok(CacheInvalidation::None);
            }

            if matches!(
                &app.overlay,
                Overlay::Loading {
                    kind: LoadingKind::Proxy,
                    ..
                }
            ) {
                app.overlay = Overlay::None;
            }

            match result {
                Ok(()) => {
                    *data = UiData::load(&app.app_type)?;
                    proxy_snapshot_refresh.cancel();
                    invalidation = CacheInvalidation::DataReloaded;
                    app.reset_proxy_activity(
                        data.proxy.estimated_input_tokens_total,
                        data.proxy.estimated_output_tokens_total,
                    );
                    app.push_toast(
                        texts::tui_toast_proxy_managed_current_app_updated(
                            app_display_name(&app_type),
                            enabled,
                        ),
                        ToastKind::Success,
                    );
                }
                Err(err) => {
                    app.push_toast(err, ToastKind::Error);
                }
            }
        }
        ProxyMsg::SnapshotRefreshed {
            request_id,
            app_type,
            result,
        } => {
            if !proxy_snapshot_refresh.finish_if_active(request_id) {
                return Ok(CacheInvalidation::None);
            }
            if app.app_type != app_type {
                return Ok(CacheInvalidation::None);
            }

            match result {
                Ok(proxy) => {
                    data.proxy = proxy;
                    app.observe_proxy_token_activity(
                        data.proxy.estimated_input_tokens_total,
                        data.proxy.estimated_output_tokens_total,
                    );
                }
                Err(err) => {
                    log::debug!("refresh proxy snapshot failed: {err}");
                }
            }
        }
    }

    Ok(invalidation)
}

#[allow(dead_code)]
pub(crate) fn apply_webdav_jianguoyun_quick_setup<FSave, FCheck>(
    username: &str,
    password: &str,
    save_settings: FSave,
    check_connection: FCheck,
) -> Result<(), AppError>
where
    FSave: FnOnce(WebDavSyncSettings) -> Result<(), AppError>,
    FCheck: FnOnce() -> Result<(), AppError>,
{
    let cfg = webdav_jianguoyun_preset(username, password);
    save_settings(cfg)?;
    check_connection()?;
    Ok(())
}

pub(crate) fn update_webdav_last_error_with<FGet, FSet>(
    last_error: Option<String>,
    get: FGet,
    set: FSet,
) where
    FGet: FnOnce() -> Option<WebDavSyncSettings>,
    FSet: FnOnce(WebDavSyncSettings) -> Result<(), AppError>,
{
    let Some(mut cfg) = get() else {
        return;
    };
    cfg.status.last_error = last_error;
    let _ = set(cfg);
}

fn update_webdav_last_error(last_error: Option<String>) {
    update_webdav_last_error_with(last_error, get_webdav_sync_settings, |cfg| {
        set_webdav_sync_settings(Some(cfg))
    });
}

pub(crate) fn handle_update_msg(app: &mut App, update_check: &mut RequestTracker, msg: UpdateMsg) {
    match msg {
        UpdateMsg::CheckFinished { request_id, result } => {
            if !update_check.finish_if_active(request_id) {
                return;
            }

            match result {
                Ok(info) => {
                    if info.is_already_latest {
                        app.overlay = Overlay::None;
                        app.push_toast(
                            texts::tui_toast_already_latest(&info.current_version),
                            ToastKind::Success,
                        );
                    } else if info.is_homebrew_managed {
                        app.overlay = Overlay::None;
                        app.push_toast(
                            texts::tui_toast_update_homebrew_required(
                                &info.current_version,
                                &info.target_tag,
                            ),
                            ToastKind::Info,
                        );
                    } else if info.is_downgrade {
                        app.overlay = Overlay::None;
                        app.push_toast(
                            texts::tui_toast_update_downgrade(
                                &info.current_version,
                                &info.target_tag,
                            ),
                            ToastKind::Info,
                        );
                    } else {
                        app.overlay = Overlay::UpdateAvailable {
                            current: info.current_version,
                            latest: info.target_tag,
                            selected: 0,
                        };
                    }
                }
                Err(e) => {
                    app.overlay = Overlay::None;
                    app.push_toast(texts::tui_toast_update_check_failed(&e), ToastKind::Error);
                }
            }
        }
        UpdateMsg::DownloadProgress { downloaded, total } => {
            if let Overlay::UpdateDownloading {
                downloaded: ref mut dl,
                total: ref mut t,
            } = app.overlay
            {
                *dl = downloaded;
                *t = total;
            }
        }
        UpdateMsg::DownloadFinished(result) => match result {
            Ok(tag) => {
                app.overlay = Overlay::UpdateResult {
                    success: true,
                    message: texts::tui_update_success(&tag),
                };
            }
            Err(e) => {
                app.overlay = Overlay::UpdateResult {
                    success: false,
                    message: e,
                };
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_config::AppType;
    use crate::cli::tui::data::{ProviderUsageQuota, QuotaTarget, QuotaTargetKind};
    use crate::services::{CredentialStatus, SubscriptionQuota};
    use crate::session_manager::SessionMeta;

    fn session(provider: &str, id: &str, last_active_at: i64) -> SessionMeta {
        SessionMeta {
            provider_id: provider.to_string(),
            session_id: id.to_string(),
            title: Some(id.to_string()),
            last_active_at: Some(last_active_at),
            source_path: Some(format!("/tmp/{provider}-{id}.jsonl")),
            ..SessionMeta::default()
        }
    }

    fn published(
        _generation: &str,
        total_rows: usize,
        rows: Vec<SessionMeta>,
    ) -> (
        crate::session_manager::paged_manifest::PublishedManifest,
        crate::session_manager::paged_manifest::ManifestReader,
    ) {
        assert_eq!(total_rows, rows.len(), "fixture must publish every row");
        let directory = tempfile::tempdir().expect("manifest fixture directory");
        let store =
            crate::session_manager::paged_manifest::PagedManifestStore::open_at(directory.path())
                .expect("manifest fixture store");
        let scope = rows
            .first()
            .map(|row| row.provider_id.clone())
            .unwrap_or_else(|| "claude".to_string());
        let mut builder = store.begin_build(&scope).expect("manifest fixture builder");
        for row in rows {
            builder.push(row).expect("manifest fixture row");
        }
        let published = builder.publish().expect("publish manifest fixture");
        let reader = published.reader.clone();
        static FIXTURE_DIRS: std::sync::OnceLock<std::sync::Mutex<Vec<tempfile::TempDir>>> =
            std::sync::OnceLock::new();
        FIXTURE_DIRS
            .get_or_init(|| std::sync::Mutex::new(Vec::new()))
            .lock()
            .expect("manifest fixture directory lock")
            .push(directory);
        (published, reader)
    }

    fn published_in_store(
        store: &crate::session_manager::paged_manifest::PagedManifestStore,
        rows: Vec<SessionMeta>,
    ) -> (
        crate::session_manager::paged_manifest::PublishedManifest,
        crate::session_manager::paged_manifest::ManifestReader,
    ) {
        let scope = rows
            .first()
            .map(|row| row.provider_id.clone())
            .unwrap_or_else(|| "claude".to_string());
        let mut builder = store.begin_build(&scope).expect("manifest fixture builder");
        for row in rows {
            builder.push(row).expect("manifest fixture row");
        }
        let published = builder.publish().expect("publish manifest fixture");
        let reader = published.reader.clone();
        (published, reader)
    }

    fn highlighted_session_key(app: &App) -> Option<String> {
        visible_session_rows(app)
            .get(app.sessions.selected_idx)
            .map(crate::cli::tui::app::session_key)
    }

    fn quota_target() -> QuotaTarget {
        QuotaTarget {
            app_type: AppType::Claude,
            provider_id: "official".to_string(),
            provider_name: "Claude Official".to_string(),
            kind: QuotaTargetKind::SubscriptionTool {
                tool: "claude".to_string(),
            },
        }
    }

    fn quota_result() -> SubscriptionQuota {
        SubscriptionQuota {
            tool: "claude".to_string(),
            credential_status: CredentialStatus::Valid,
            credential_message: None,
            success: true,
            tiers: Vec::new(),
            extra_usage: None,
            error: None,
            queried_at: Some(chrono::Utc::now().timestamp_millis()),
        }
    }

    #[test]
    fn manual_quota_refresh_success_shows_finished_toast() {
        let mut app = App::new(Some(AppType::Claude));
        let mut data = UiData::default();
        let target = quota_target();
        data.quota.mark_loading(target.clone(), true);

        handle_quota_msg(
            &mut app,
            &mut data,
            QuotaMsg::Finished {
                target: target.clone(),
                result: Ok(ProviderUsageQuota::Subscription(quota_result())),
            },
        );

        let toast = app
            .toast
            .as_ref()
            .expect("manual refresh completion should show a toast");
        assert_eq!(toast.kind, ToastKind::Success);
        assert_eq!(
            toast.message,
            texts::tui_toast_quota_refresh_finished("Claude Official")
        );
        assert!(!data.quota.has_manual_loading(&target));
    }

    #[test]
    fn automatic_quota_refresh_success_stays_quiet() {
        let mut app = App::new(Some(AppType::Claude));
        let mut data = UiData::default();
        let target = quota_target();
        data.quota.mark_loading(target.clone(), false);

        handle_quota_msg(
            &mut app,
            &mut data,
            QuotaMsg::Finished {
                target,
                result: Ok(ProviderUsageQuota::Subscription(quota_result())),
            },
        );

        assert!(
            app.toast.is_none(),
            "automatic background quota refresh should not interrupt the user"
        );
    }

    #[test]
    fn session_scan_result_updates_runtime_state_without_ui_data() {
        let mut app = App::new(Some(AppType::Claude));
        let request_id = app.sessions.start_scan("claude".to_string());
        let scope_epoch = app.sessions.scope_epoch;

        handle_session_msg(
            &mut app,
            SessionMsg::ManifestPublished {
                request_id,
                scope_epoch,
                scope: "claude".to_string(),
                result: Ok(published("g1", 1, vec![session("claude", "session-1", 2)])),
            },
        );

        assert!(app.sessions.loaded_once);
        assert!(!app.sessions.loading);
        assert_eq!(app.sessions.rows.len(), 1);
        assert_eq!(app.sessions.rows[0].provider_id, "claude");
        assert_eq!(app.sessions.pagination.len(), 1);
        assert_eq!(app.sessions.pagination.selected_index(), Some(0));
        assert!(app.sessions.pagination.is_row_focused());
    }

    #[test]
    fn stale_session_scan_result_is_ignored() {
        let mut app = App::new(Some(AppType::Claude));
        let stale_id = app.sessions.start_scan("claude".to_string());
        let scope_epoch = app.sessions.scope_epoch;
        let _current_id = app.sessions.start_scan("claude".to_string());

        handle_session_msg(
            &mut app,
            SessionMsg::ManifestPublished {
                request_id: stale_id,
                scope_epoch,
                scope: "claude".to_string(),
                result: Ok(published("stale", 1, vec![session("claude", "stale", 1)])),
            },
        );

        assert!(app.sessions.rows.is_empty());
        assert!(app.sessions.loading);
    }

    #[test]
    fn stale_session_failures_do_not_interrupt_the_current_request() {
        let mut app = App::new(Some(AppType::Claude));
        let stale_scan = app.sessions.start_scan("claude".to_string());
        let scope_epoch = app.sessions.scope_epoch;
        let current_scan = app.sessions.start_scan("claude".to_string());

        handle_session_msg(
            &mut app,
            SessionMsg::ManifestPublished {
                request_id: stale_scan,
                scope_epoch,
                scope: "claude".to_string(),
                result: Err("stale scan failed".to_string()),
            },
        );

        assert_eq!(app.sessions.scan_active, Some(current_scan));
        assert!(app.sessions.loading);
        assert!(app.toast.is_none());

        app.sessions.detail_key = Some("claude:a:/tmp/a".to_string());
        let stale_messages = app
            .sessions
            .start_message_load("claude:a:/tmp/a".to_string());
        let current_messages = app
            .sessions
            .start_message_load("claude:a:/tmp/a".to_string());
        handle_session_msg(
            &mut app,
            SessionMsg::MessagesLoaded {
                request_id: stale_messages,
                key: "claude:a:/tmp/a".to_string(),
                result: Err("stale messages failed".to_string()),
            },
        );

        assert_eq!(app.sessions.message_active, Some(current_messages));
        assert!(app.sessions.messages_loading);
        assert!(app.toast.is_none());

        let stale_delete = app.sessions.start_delete();
        app.sessions.fail_delete(stale_delete);
        handle_session_msg(
            &mut app,
            SessionMsg::DeleteFinished {
                request_id: stale_delete,
                key: "claude:a:/tmp/a".to_string(),
                result: Err("stale delete failed".to_string()),
            },
        );
        assert!(app.toast.is_none());
    }

    #[test]
    fn refresh_failure_keeps_a_healthy_manifest_page_visible() {
        let mut app = App::new(Some(AppType::Claude));
        let initial = app.sessions.start_scan("claude".to_string());
        let scope_epoch = app.sessions.scope_epoch;
        handle_session_msg(
            &mut app,
            SessionMsg::ManifestPublished {
                request_id: initial,
                scope_epoch,
                scope: "claude".to_string(),
                result: Ok(published(
                    "base-1",
                    1,
                    vec![session("claude", "healthy", 1)],
                )),
            },
        );

        let refresh = app.sessions.start_scan("claude".to_string());
        handle_session_msg(
            &mut app,
            SessionMsg::ManifestPublished {
                request_id: refresh,
                scope_epoch,
                scope: "claude".to_string(),
                result: Err("temporary read failure".to_string()),
            },
        );

        assert_eq!(app.sessions.rows[0].session_id, "healthy");
        assert!(app.sessions.page_token().is_some());
        assert!(!app.sessions.loading);
        assert!(app.sessions.last_error.is_none());
        assert_eq!(
            app.toast.as_ref().map(|toast| toast.kind),
            Some(ToastKind::Warning)
        );
    }

    #[test]
    fn materialized_query_keeps_content_only_matches_visible() {
        let mut app = App::new(Some(AppType::Claude));
        let initial = app.sessions.start_scan("claude".to_string());
        let scope_epoch = app.sessions.scope_epoch;
        handle_session_msg(
            &mut app,
            SessionMsg::ManifestPublished {
                request_id: initial,
                scope_epoch,
                scope: "claude".to_string(),
                result: Ok(published("base-1", 1, vec![session("claude", "base", 1)])),
            },
        );
        let base_generation = app
            .sessions
            .base_manifest
            .as_ref()
            .expect("base manifest")
            .generation
            .clone();
        app.filter.input.set("needle");
        app.sessions.deep_search_query = Some("needle".to_string());
        app.sessions.deep_search_seq = 7;
        app.sessions.deep_search_active = Some(7);
        let content_only = session("claude", "body-only", 2);
        let query_namespace = app.sessions.query_namespace.clone();

        handle_session_msg(
            &mut app,
            SessionMsg::QueryPublished {
                request_id: 7,
                scope_epoch,
                scope: "claude".to_string(),
                base_generation,
                query: "needle".to_string(),
                query_namespace,
                result: Ok(published("query-1", 1, vec![content_only])),
            },
        );

        let visible = visible_session_rows(&app);
        assert_eq!(visible.len(), 1);
        assert_eq!(
            visible.get(0).map(|row| row.session_id.as_str()),
            Some("body-only")
        );
        assert_eq!(
            app.sessions.page_token().map(|token| token.source),
            Some(crate::cli::tui::app::SessionPageSource::Query)
        );
    }

    #[test]
    fn base_refresh_does_not_replace_an_active_query_page() {
        let mut app = App::new(Some(AppType::Claude));
        let initial = app.sessions.start_scan("claude".to_string());
        let scope_epoch = app.sessions.scope_epoch;
        handle_session_msg(
            &mut app,
            SessionMsg::ManifestPublished {
                request_id: initial,
                scope_epoch,
                scope: "claude".to_string(),
                result: Ok(published("base-1", 1, vec![session("claude", "base", 1)])),
            },
        );
        let base_generation = app
            .sessions
            .base_manifest
            .as_ref()
            .expect("base manifest")
            .generation
            .clone();
        app.filter.input.set("needle");
        app.sessions.deep_search_query = Some("needle".to_string());
        app.sessions.deep_search_active = Some(4);
        let query_namespace = app.sessions.query_namespace.clone();
        handle_session_msg(
            &mut app,
            SessionMsg::QueryPublished {
                request_id: 4,
                scope_epoch,
                scope: "claude".to_string(),
                base_generation,
                query: "needle".to_string(),
                query_namespace,
                result: Ok(published(
                    "query-1",
                    1,
                    vec![session("claude", "query-row", 2)],
                )),
            },
        );

        let refresh = app.sessions.start_scan("claude".to_string());
        handle_session_msg(
            &mut app,
            SessionMsg::ManifestPublished {
                request_id: refresh,
                scope_epoch,
                scope: "claude".to_string(),
                result: Ok(published(
                    "base-2",
                    1,
                    vec![session("claude", "new-base", 3)],
                )),
            },
        );

        assert_eq!(app.sessions.rows[0].session_id, "query-row");
        assert_eq!(
            app.sessions.page_token().map(|token| token.source),
            Some(crate::cli::tui::app::SessionPageSource::Query)
        );
        assert_ne!(
            app.sessions
                .base_manifest
                .as_ref()
                .map(|base| base.generation.as_str()),
            app.sessions
                .page_token()
                .map(|token| token.generation.as_str())
        );
        assert_eq!(app.pending_deep_search.as_deref(), Some("needle"));
    }

    #[test]
    fn session_scan_result_clears_missing_detail() {
        let mut app = App::new(Some(AppType::Claude));
        let initial_request = app.sessions.start_scan("claude".to_string());
        let scope_epoch = app.sessions.scope_epoch;
        handle_session_msg(
            &mut app,
            SessionMsg::ManifestPublished {
                request_id: initial_request,
                scope_epoch,
                scope: "claude".to_string(),
                result: Ok(published(
                    "old-generation",
                    1,
                    vec![session("claude", "old", 1)],
                )),
            },
        );
        app.sessions
            .open_detail(crate::cli::tui::app::session_key(&app.sessions.rows[0]));
        app.sessions.messages_loaded = true;
        app.sessions.messages = vec![crate::session_manager::SessionMessage {
            role: "assistant".to_string(),
            content: "stale detail".to_string(),
            ts: None,
        }];
        let request_id = app.sessions.start_scan("claude".to_string());

        handle_session_msg(
            &mut app,
            SessionMsg::ManifestPublished {
                request_id,
                scope_epoch,
                scope: "claude".to_string(),
                result: Ok(published(
                    "new-generation",
                    1,
                    vec![session("claude", "new", 2)],
                )),
            },
        );
        let (locate_id, _, _, generation, _, _, reader) = app
            .sessions
            .next_manifest_reconcile()
            .expect("published manifest should require bounded reconciliation");
        let page = reader.load_page(0).expect("published manifest page");
        handle_session_msg(
            &mut app,
            SessionMsg::ManifestLocated {
                request_id: locate_id,
                scope_epoch,
                generation: generation.clone(),
                result: Ok((reader, page, 0)),
            },
        );

        assert_eq!(app.sessions.rows.len(), 1);
        assert_eq!(app.sessions.rows[0].session_id, "new");
        assert!(app.sessions.detail_key.is_none());
        assert!(app.sessions.messages.is_empty());
        assert!(!app.sessions.messages_loaded);
    }

    #[test]
    fn session_delete_result_clamps_selection_against_current_filter() {
        let mut app = App::new(Some(AppType::Claude));
        app.sessions.rows = vec![
            crate::session_manager::SessionMeta {
                provider_id: "claude".to_string(),
                session_id: "alpha".to_string(),
                title: Some("Alpha".to_string()),
                summary: None,
                project_dir: None,
                created_at: None,
                last_active_at: None,
                source_path: Some("/tmp/alpha.jsonl".to_string()),
                resume_command: None,
            },
            crate::session_manager::SessionMeta {
                provider_id: "claude".to_string(),
                session_id: "beta".to_string(),
                title: Some("Beta".to_string()),
                summary: None,
                project_dir: None,
                created_at: None,
                last_active_at: None,
                source_path: Some("/tmp/beta.jsonl".to_string()),
                resume_command: None,
            },
        ];
        app.filter.input.set("beta");
        app.sessions.selected_idx = 1;
        let key = crate::cli::tui::app::session_key(&app.sessions.rows[1]);
        let request_id = app.sessions.start_delete();

        handle_session_msg(
            &mut app,
            SessionMsg::DeleteFinished {
                request_id,
                key,
                result: Ok(()),
            },
        );

        assert_eq!(app.sessions.selected_idx, 0);
        assert_eq!(app.sessions.rows.len(), 1);
        assert_eq!(app.sessions.rows[0].session_id, "alpha");
    }

    #[test]
    fn deleting_from_a_query_rebuilds_the_same_query_from_the_purged_base() {
        let mut app = App::new(Some(AppType::Claude));
        let initial = app.sessions.start_scan("claude".to_string());
        let scope_epoch = app.sessions.scope_epoch;
        handle_session_msg(
            &mut app,
            SessionMsg::ManifestPublished {
                request_id: initial,
                scope_epoch,
                scope: "claude".to_string(),
                result: Ok(published(
                    "base",
                    2,
                    vec![
                        session("claude", "deleted", 2),
                        session("claude", "kept", 1),
                    ],
                )),
            },
        );
        let base_generation = app
            .sessions
            .base_manifest
            .as_ref()
            .expect("base manifest")
            .generation
            .clone();
        app.sessions.deep_search_query = Some("needle".to_string());
        app.sessions.deep_search_active = Some(7);
        let query_namespace = app.sessions.query_namespace.clone();
        handle_session_msg(
            &mut app,
            SessionMsg::QueryPublished {
                request_id: 7,
                scope_epoch,
                scope: "claude".to_string(),
                base_generation,
                query: "needle".to_string(),
                query_namespace,
                result: Ok(published(
                    "query",
                    2,
                    vec![
                        session("claude", "deleted", 2),
                        session("claude", "kept", 1),
                    ],
                )),
            },
        );
        let deleted_key = crate::cli::tui::app::session_key(&app.sessions.rows[0]);
        let delete_request = app.sessions.start_delete();
        handle_session_msg(
            &mut app,
            SessionMsg::DeleteFinished {
                request_id: delete_request,
                key: deleted_key.clone(),
                result: Ok(()),
            },
        );
        assert!(app.sessions.scan_tombstones.contains(&deleted_key));

        let (base, base_reader) = published("base-purged", 1, vec![session("claude", "kept", 1)]);
        handle_session_msg(
            &mut app,
            SessionMsg::ManifestsPurged {
                request_id: delete_request,
                key: deleted_key.clone(),
                base: vec![("claude".to_string(), base, base_reader)],
            },
        );
        assert_eq!(app.pending_deep_search.as_deref(), Some("needle"));
        assert!(app.sessions.scan_tombstones.contains(&deleted_key));

        let rebuilt_base_generation = app
            .sessions
            .base_manifest
            .as_ref()
            .expect("purged base saved")
            .generation
            .clone();
        app.sessions.deep_search_seq = 8;
        app.sessions.deep_search_active = Some(8);
        let query_namespace = app.sessions.query_namespace.clone();
        handle_session_msg(
            &mut app,
            SessionMsg::QueryPublished {
                request_id: 8,
                scope_epoch,
                scope: "claude".to_string(),
                base_generation: rebuilt_base_generation,
                query: "needle".to_string(),
                query_namespace,
                result: Ok(published(
                    "query-rebuilt",
                    1,
                    vec![session("claude", "kept", 1)],
                )),
            },
        );

        assert_eq!(
            app.sessions.page_token().map(|token| token.source),
            Some(crate::cli::tui::app::SessionPageSource::Query)
        );
        assert_eq!(app.sessions.rows.len(), 1);
        assert_eq!(app.sessions.rows[0].session_id, "kept");
        assert!(!app.sessions.purge_tombstone_applies_to_scope(
            delete_request,
            &deleted_key,
            "claude"
        ));
    }

    #[test]
    fn delayed_purge_requires_the_current_delete_revision_and_cannot_regress_base() {
        let directory = tempfile::tempdir().expect("manifest fixture directory");
        let store =
            crate::session_manager::paged_manifest::PagedManifestStore::open_at(directory.path())
                .expect("manifest fixture store");
        let old = published_in_store(&store, vec![session("claude", "old-generation", 1)]);
        let newer = published_in_store(&store, vec![session("claude", "new-generation", 2)]);
        assert!(newer.0.build_epoch > old.0.build_epoch);

        let mut app = App::new(Some(AppType::Claude));
        let scan_request = app.sessions.start_scan("claude".to_string());
        let scope_epoch = app.sessions.scope_epoch;
        handle_session_msg(
            &mut app,
            SessionMsg::ManifestPublished {
                request_id: scan_request,
                scope_epoch,
                scope: "claude".to_string(),
                result: Ok(newer.clone()),
            },
        );
        let newest_generation = newer.0.generation.clone();

        let deleted_key = crate::cli::tui::app::session_key(&session("claude", "same", 3));
        let first_delete = app.sessions.start_delete();
        // A malformed/out-of-order purge cannot establish or clear a deletion
        // barrier before its DeleteFinished acknowledgement is processed.
        handle_session_msg(
            &mut app,
            SessionMsg::ManifestsPurged {
                request_id: first_delete,
                key: deleted_key.clone(),
                base: vec![("claude".to_string(), old.0.clone(), old.1.clone())],
            },
        );
        assert!(!app.sessions.scan_tombstones.contains(&deleted_key));

        handle_session_msg(
            &mut app,
            SessionMsg::DeleteFinished {
                request_id: first_delete,
                key: deleted_key.clone(),
                result: Ok(()),
            },
        );
        let second_delete = app.sessions.start_delete();
        handle_session_msg(
            &mut app,
            SessionMsg::DeleteFinished {
                request_id: second_delete,
                key: deleted_key.clone(),
                result: Ok(()),
            },
        );

        // The first purge is now stale even though its identity is identical.
        handle_session_msg(
            &mut app,
            SessionMsg::ManifestsPurged {
                request_id: first_delete,
                key: deleted_key.clone(),
                base: vec![("claude".to_string(), old.0.clone(), old.1.clone())],
            },
        );
        assert!(app
            .sessions
            .purge_tombstone_is_current(second_delete, &deleted_key));

        // Even the current deletion's delayed purge publication is older than
        // the accepted refresh. It may reconcile the deletion, but must stage
        // the saved newer base rather than regress the visible source.
        handle_session_msg(
            &mut app,
            SessionMsg::ManifestsPurged {
                request_id: second_delete,
                key: deleted_key,
                base: vec![("claude".to_string(), old.0, old.1)],
            },
        );
        assert_eq!(
            app.sessions
                .base_manifest
                .as_ref()
                .map(|base| base.generation.as_str()),
            Some(newest_generation.as_str())
        );
        assert_eq!(
            app.sessions
                .pending_manifest
                .as_ref()
                .map(|pending| pending.generation.as_str()),
            Some(newest_generation.as_str())
        );
    }

    #[test]
    fn delete_ack_from_an_old_scope_cannot_discard_new_scope_reconciliation() {
        let mut app = App::new(Some(AppType::Claude));
        let old_delete = app.sessions.start_scan("claude".to_string());
        let old_scope_epoch = app.sessions.scope_epoch;
        handle_session_msg(
            &mut app,
            SessionMsg::ManifestPublished {
                request_id: old_delete,
                scope_epoch: old_scope_epoch,
                scope: "claude".to_string(),
                result: Ok(published(
                    "claude-base",
                    1,
                    vec![session("claude", "deleted", 1)],
                )),
            },
        );
        let deleted = session("claude", "deleted", 1);
        let deleted_key = crate::cli::tui::app::session_key(&deleted);
        let delete_request = app.sessions.start_delete();

        let codex_initial = app.sessions.start_scan("codex".to_string());
        let codex_epoch = app.sessions.scope_epoch;
        handle_session_msg(
            &mut app,
            SessionMsg::ManifestPublished {
                request_id: codex_initial,
                scope_epoch: codex_epoch,
                scope: "codex".to_string(),
                result: Ok(published(
                    "codex-base",
                    1,
                    vec![session("codex", "current", 1)],
                )),
            },
        );
        let codex_refresh = app.sessions.start_scan("codex".to_string());
        handle_session_msg(
            &mut app,
            SessionMsg::ManifestPublished {
                request_id: codex_refresh,
                scope_epoch: codex_epoch,
                scope: "codex".to_string(),
                result: Ok(published("codex-new", 1, vec![session("codex", "new", 2)])),
            },
        );
        let pending_generation = app
            .sessions
            .pending_manifest
            .as_ref()
            .expect("codex refresh waits for bounded reconcile")
            .generation
            .clone();

        handle_session_msg(
            &mut app,
            SessionMsg::DeleteFinished {
                request_id: delete_request,
                key: deleted_key,
                result: Ok(()),
            },
        );

        assert_eq!(app.sessions.scan_active, Some(codex_refresh));
        assert!(app.sessions.loading);
        assert_eq!(
            app.sessions
                .pending_manifest
                .as_ref()
                .map(|pending| pending.generation.as_str()),
            Some(pending_generation.as_str())
        );
        assert!(app.sessions.next_manifest_reconcile().is_some());
    }

    #[test]
    fn missing_current_purge_generation_requests_one_fresh_scan() {
        let mut app = App::new(Some(AppType::Claude));
        app.sessions.start_scan("claude".to_string());
        let deleted = session("claude", "deleted", 1);
        let key = crate::cli::tui::app::session_key(&deleted);
        let request_id = app.sessions.start_delete();
        handle_session_msg(
            &mut app,
            SessionMsg::DeleteFinished {
                request_id,
                key: key.clone(),
                result: Ok(()),
            },
        );

        handle_session_msg(
            &mut app,
            SessionMsg::ManifestsPurged {
                request_id,
                key,
                base: Vec::new(),
            },
        );

        assert!(app.sessions.purge_refresh_required());
    }

    #[test]
    fn published_manifest_preserves_selected_session_key_after_bounded_reconcile() {
        let mut app = App::new(Some(AppType::Claude));
        let selected = session("claude", "selected", 20);
        let selected_key = crate::cli::tui::app::session_key(&selected);
        let initial_request = app.sessions.start_scan("claude".to_string());
        let scope_epoch = app.sessions.scope_epoch;
        handle_session_msg(
            &mut app,
            SessionMsg::ManifestPublished {
                request_id: initial_request,
                scope_epoch,
                scope: "claude".to_string(),
                result: Ok(published(
                    "old-generation",
                    2,
                    vec![session("claude", "old", 10), selected.clone()],
                )),
            },
        );
        let initial_selected = app
            .sessions
            .rows
            .iter()
            .position(|row| row.session_id == "selected")
            .expect("selected row is present in the sorted initial manifest");
        app.sessions.selected_idx = initial_selected;
        app.sessions.pagination.select(initial_selected);
        app.sessions.open_detail(selected_key.clone());
        let request_id = app.sessions.start_scan("claude".to_string());
        let replacement = vec![
            session("claude", "new", 30),
            selected,
            session("claude", "old", 10),
        ];

        handle_session_msg(
            &mut app,
            SessionMsg::ManifestPublished {
                request_id,
                scope_epoch,
                scope: "claude".to_string(),
                result: Ok(published("new-generation", 3, replacement.clone())),
            },
        );
        let (locate_id, _, _, generation, _, anchor, reader) = app
            .sessions
            .next_manifest_reconcile()
            .expect("published manifest should be reconciled in a worker");
        assert_eq!(anchor.expect("stable anchor").session_id, "selected");
        let page = reader.load_page(0).expect("published manifest page");
        let selected_local = page
            .rows
            .iter()
            .position(|row| row.session_id == "selected")
            .expect("selected row remains in page");
        handle_session_msg(
            &mut app,
            SessionMsg::ManifestLocated {
                request_id: locate_id,
                scope_epoch,
                generation: generation.clone(),
                result: Ok((reader, page, selected_local)),
            },
        );

        assert_eq!(
            highlighted_session_key(&app).as_deref(),
            Some(selected_key.as_str())
        );
        assert_eq!(app.sessions.selected_idx, 1);
        assert_eq!(app.sessions.pagination.selected_index(), Some(1));
        assert_eq!(
            app.sessions.detail_key.as_deref(),
            Some(selected_key.as_str())
        );
    }

    #[test]
    fn deleting_an_earlier_row_keeps_highlight_and_detail_on_same_session() {
        let mut app = App::new(Some(AppType::Claude));
        app.sessions.provider_id = Some("claude".to_string());
        let deleted = session("claude", "deleted", 30);
        let selected = session("claude", "selected", 10);
        let selected_key = crate::cli::tui::app::session_key(&selected);
        app.sessions.rows = vec![deleted.clone(), session("claude", "middle", 20), selected];
        app.sessions.selected_idx = 2;
        app.sessions.open_detail(selected_key.clone());
        let request_id = app.sessions.start_delete();

        handle_session_msg(
            &mut app,
            SessionMsg::DeleteFinished {
                request_id,
                key: crate::cli::tui::app::session_key(&deleted),
                result: Ok(()),
            },
        );

        assert_eq!(
            highlighted_session_key(&app).as_deref(),
            Some(selected_key.as_str())
        );
        assert_eq!(app.sessions.selected_idx, 1);
        assert_eq!(app.sessions.pagination.selected_index(), Some(1));
        assert_eq!(
            app.sessions.detail_key.as_deref(),
            Some(selected_key.as_str())
        );
    }

    #[test]
    fn deleting_selected_session_clears_detail_and_falls_back_to_list() {
        let mut app = App::new(Some(AppType::Claude));
        app.sessions.provider_id = Some("claude".to_string());
        let deleted = session("claude", "deleted", 20);
        let fallback = session("claude", "fallback", 10);
        let deleted_key = crate::cli::tui::app::session_key(&deleted);
        let fallback_key = crate::cli::tui::app::session_key(&fallback);
        app.sessions.rows = vec![deleted, fallback];
        app.sessions.selected_idx = 0;
        app.sessions.open_detail(deleted_key.clone());
        app.sessions.pane = SessionsPane::Detail;
        let request_id = app.sessions.start_delete();

        handle_session_msg(
            &mut app,
            SessionMsg::DeleteFinished {
                request_id,
                key: deleted_key,
                result: Ok(()),
            },
        );

        assert_eq!(
            highlighted_session_key(&app).as_deref(),
            Some(fallback_key.as_str())
        );
        assert!(app.sessions.detail_key.is_none());
        assert!(app.sessions.messages.is_empty());
        assert_eq!(app.sessions.pane, SessionsPane::List);
        assert_eq!(app.sessions.pagination.selected_index(), Some(0));
    }
}

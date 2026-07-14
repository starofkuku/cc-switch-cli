//! Page-bounded materialization of Sessions metadata and full-text queries.
//!
//! The source reader stays pinned to one immutable generation. Each iteration
//! owns at most one manifest page (100 rows), searches only rows whose metadata
//! did not already match, and streams matches into the destination builder.

use std::collections::HashSet;
use std::path::Path;

use chrono::{Local, TimeZone};

use super::paged_manifest::{
    ManifestError, ManifestReader, PublishedManifest, QueryManifestBuilder,
};
use super::{search_sessions_in_cancellable, SessionMeta, SessionSearchHit};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct SessionIdentity {
    provider_id: String,
    session_id: String,
    source_path: String,
}

impl SessionIdentity {
    fn from_row(row: &SessionMeta) -> Self {
        Self {
            provider_id: row.provider_id.clone(),
            session_id: row.session_id.clone(),
            source_path: row.source_path.clone().unwrap_or_default(),
        }
    }

    fn from_hit(hit: SessionSearchHit) -> Self {
        Self {
            provider_id: hit.provider_id,
            session_id: hit.session_id,
            source_path: hit.source_path,
        }
    }
}

/// Build and atomically publish a filtered manifest from a generation-pinned
/// base reader. Cancellation returns [`ManifestError::Cancelled`]; dropping the
/// builder removes its staging directory and leaves the current pointer intact.
pub(crate) fn build_query_manifest(
    base: &ManifestReader,
    query: &str,
    mut builder: QueryManifestBuilder,
    is_cancelled: &(dyn Fn() -> bool + Sync),
) -> Result<PublishedManifest, ManifestError> {
    if builder.base_scope() != base.scope() || builder.base_generation() != base.generation() {
        return Err(ManifestError::InvalidOptions(
            "query builder does not belong to the supplied base generation".to_string(),
        ));
    }
    let query = query.trim().to_lowercase();

    for page_index in 0..base.page_count() {
        ensure_active(&builder, is_cancelled)?;
        let page = base.load_page(page_index).ok_or_else(|| {
            ManifestError::Corrupt(format!(
                "query source page {page_index} is unreadable in generation {}",
                base.generation()
            ))
        })?;

        let mut matched = HashSet::with_capacity(page.rows.len());
        let mut content_candidates = Vec::with_capacity(page.rows.len());
        for row in &page.rows {
            ensure_active(&builder, is_cancelled)?;
            if query.is_empty() || session_metadata_matches(row, &query) {
                matched.insert(SessionIdentity::from_row(row));
            } else {
                content_candidates.push(row.clone());
            }
        }

        if !content_candidates.is_empty() {
            let hits = search_sessions_in_cancellable(&content_candidates, &query, is_cancelled)
                .ok_or(ManifestError::Cancelled)?;
            ensure_active(&builder, is_cancelled)?;
            matched.extend(hits.into_iter().map(SessionIdentity::from_hit));
        }

        // `matched` is the metadata/content union. The per-page emitted set
        // prevents a duplicated source row from being pushed twice without
        // retaining identities from prior pages.
        let mut emitted = HashSet::with_capacity(matched.len());
        for row in page.rows {
            ensure_active(&builder, is_cancelled)?;
            let identity = SessionIdentity::from_row(&row);
            if matched.contains(&identity) && emitted.insert(identity) {
                builder.push(row)?;
            }
        }
    }

    ensure_active(&builder, is_cancelled)?;
    builder.publish_cancellable(is_cancelled)
}

fn ensure_active(
    builder: &QueryManifestBuilder,
    is_cancelled: &(dyn Fn() -> bool + Sync),
) -> Result<(), ManifestError> {
    if builder.is_cancelled() || is_cancelled() {
        Err(ManifestError::Cancelled)
    } else {
        Ok(())
    }
}

fn session_metadata_matches(session: &SessionMeta, query: &str) -> bool {
    filter_text_matches(&session.provider_id, query)
        || filter_text_matches(&session.session_id, query)
        || filter_option_text_matches(session.title.as_deref(), query)
        || filter_option_text_matches(session.summary.as_deref(), query)
        || filter_option_path_matches(session.project_dir.as_deref(), query)
        || filter_option_path_matches(session.source_path.as_deref(), query)
        || filter_option_text_matches(session.resume_command.as_deref(), query)
        || filter_timestamp_matches(session.last_active_at.or(session.created_at), query)
}

fn filter_option_text_matches(value: Option<&str>, query: &str) -> bool {
    value.is_some_and(|value| filter_text_matches(value, query))
}

fn filter_option_path_matches(value: Option<&str>, query: &str) -> bool {
    let Some(value) = value.filter(|value| !value.trim().is_empty()) else {
        return false;
    };
    filter_text_matches(value, query) || filter_text_matches(&path_basename(value), query)
}

fn filter_text_matches(value: &str, query: &str) -> bool {
    value.to_lowercase().contains(query)
}

fn filter_timestamp_matches(timestamp_ms: Option<i64>, query: &str) -> bool {
    if !query.chars().any(|ch| ch.is_ascii_digit()) {
        return false;
    }
    let Some(timestamp_ms) = timestamp_ms else {
        return false;
    };
    let Some(datetime) = Local.timestamp_millis_opt(timestamp_ms).single() else {
        return false;
    };
    datetime.format("%Y/%m/%d").to_string().contains(query)
        || datetime.format("%Y-%m-%d").to_string().contains(query)
}

fn path_basename(path: &str) -> String {
    let trimmed = path.trim().trim_end_matches(['/', '\\']);
    if trimmed.is_empty() {
        return String::new();
    }
    Path::new(trimmed)
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or(trimmed)
        .to_string()
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use tempfile::TempDir;

    use super::*;
    use crate::session_manager::paged_manifest::{
        PagedManifestStore, QueryManifestNamespace, QueryManifestStore, PAGE_SIZE,
    };

    fn stores() -> (TempDir, PagedManifestStore, QueryManifestStore) {
        let temp = tempfile::tempdir().expect("tempdir");
        let base = PagedManifestStore::open_at(temp.path()).expect("base store");
        let query = QueryManifestStore::open_at(
            temp.path(),
            &QueryManifestNamespace::for_test("tui-query-tests"),
        )
        .expect("query store");
        (temp, base, query)
    }

    fn meta(id: &str, recency: i64, title: &str, source_path: String) -> SessionMeta {
        SessionMeta {
            provider_id: "codex".to_string(),
            session_id: id.to_string(),
            title: Some(title.to_string()),
            summary: Some(format!("summary {id}")),
            project_dir: Some(format!("/tmp/project-{id}")),
            created_at: Some(recency.saturating_sub(1)),
            last_active_at: Some(recency),
            source_path: Some(source_path),
            resume_command: Some(format!("codex resume {id}")),
        }
    }

    fn publish_base(
        store: &PagedManifestStore,
        rows: impl IntoIterator<Item = SessionMeta>,
    ) -> ManifestReader {
        let mut builder = store.begin_build("codex").expect("base builder");
        for row in rows {
            builder.push(row).expect("push base row");
        }
        builder.publish().expect("publish base");
        store.open_reader("codex").expect("base reader")
    }

    fn write_codex_session(temp: &TempDir, id: &str, content: &str) -> String {
        let path = temp.path().join(format!("{id}.jsonl"));
        std::fs::write(
            &path,
            format!(
                "{{\"type\":\"response_item\",\"payload\":{{\"type\":\"message\",\"role\":\"user\",\"content\":\"{content}\"}}}}\n"
            ),
        )
        .expect("write transcript");
        path.to_string_lossy().to_string()
    }

    fn collect_ids(reader: &ManifestReader) -> Vec<String> {
        let mut ids = Vec::new();
        for page_index in 0..reader.page_count() {
            ids.extend(
                reader
                    .load_page(page_index)
                    .expect("query page")
                    .rows
                    .into_iter()
                    .map(|row| row.session_id),
            );
        }
        ids
    }

    #[test]
    fn metadata_query_streams_across_more_than_one_hundred_rows() {
        let (_temp, store, query_store) = stores();
        let rows = (0..205).map(|index| {
            meta(
                &format!("s-{index:03}"),
                index,
                "batch-match",
                format!("/missing/s-{index:03}.jsonl"),
            )
        });
        let base = publish_base(&store, rows);
        let builder = query_store.begin_build(&base).expect("query builder");

        let published =
            build_query_manifest(&base, "batch-match", builder, &|| false).expect("publish query");

        assert_eq!(published.total_rows, 205);
        assert_eq!(published.page_count, 3);
        assert_eq!(published.first_page.rows.len(), PAGE_SIZE);
    }

    #[test]
    fn metadata_and_content_matches_form_a_deduplicated_union() {
        let (temp, store, query_store) = stores();
        let metadata_source = write_codex_session(&temp, "metadata", "needle in body too");
        let content_source = write_codex_session(&temp, "content", "needle only in body");
        let miss_source = write_codex_session(&temp, "miss", "unrelated body");
        let base = publish_base(
            &store,
            [
                meta("metadata", 20, "needle title", metadata_source),
                meta("content", 30, "plain title", content_source),
                meta("miss", 10, "plain title", miss_source),
            ],
        );
        let builder = query_store.begin_build(&base).expect("query builder");

        build_query_manifest(&base, "needle", builder, &|| false).expect("publish query");
        let query = query_store.open_reader("codex").expect("query reader");

        assert_eq!(collect_ids(&query), vec!["content", "metadata"]);
    }

    #[test]
    fn cancellation_drops_staging_and_preserves_previous_generation() {
        let (_temp, store, query_store) = stores();
        let rows = (0..205).map(|index| {
            meta(
                &format!("s-{index:03}"),
                index,
                "match",
                format!("/missing/s-{index:03}.jsonl"),
            )
        });
        let base = publish_base(&store, rows);
        let base_generation = base.generation().to_string();
        let builder = query_store.begin_build(&base).expect("query builder");
        let checks = AtomicUsize::new(0);
        let cancel = || checks.fetch_add(1, Ordering::AcqRel) >= 25;

        let result = build_query_manifest(&base, "match", builder, &cancel);

        assert!(matches!(result, Err(ManifestError::Cancelled)));
        assert_eq!(
            store
                .open_reader("codex")
                .expect("current reader")
                .generation(),
            base_generation
        );
    }

    #[test]
    fn query_manifest_preserves_global_recency_order() {
        let (_temp, store, query_store) = stores();
        let base = publish_base(
            &store,
            [
                meta("old", 1, "match", "/missing/old.jsonl".to_string()),
                meta("new", 300, "match", "/missing/new.jsonl".to_string()),
                meta("middle", 20, "match", "/missing/middle.jsonl".to_string()),
            ],
        );
        let builder = query_store.begin_build(&base).expect("query builder");

        build_query_manifest(&base, "match", builder, &|| false).expect("publish query");
        let query = query_store.open_reader("codex").expect("query reader");

        assert_eq!(collect_ids(&query), vec!["new", "middle", "old"]);
    }

    #[test]
    fn consecutive_queries_replace_only_query_current_and_keep_base_complete() {
        let (_temp, store, query_store) = stores();
        let base = publish_base(
            &store,
            [
                meta(
                    "alpha",
                    20,
                    "alpha-only",
                    "/missing/alpha.jsonl".to_string(),
                ),
                meta("beta", 10, "beta-only", "/missing/beta.jsonl".to_string()),
            ],
        );
        let base_generation = base.generation().to_string();

        let alpha_builder = query_store.begin_build(&base).expect("alpha builder");
        build_query_manifest(&base, "alpha-only", alpha_builder, &|| false)
            .expect("publish alpha query");
        assert_eq!(
            collect_ids(&query_store.open_reader("codex").expect("alpha query")),
            vec!["alpha"]
        );

        let beta_builder = query_store.begin_build(&base).expect("beta builder");
        build_query_manifest(&base, "beta-only", beta_builder, &|| false)
            .expect("publish beta query");
        assert_eq!(
            collect_ids(&query_store.open_reader("codex").expect("beta query")),
            vec!["beta"]
        );

        let current_base = store.open_reader("codex").expect("base remains");
        assert_eq!(current_base.generation(), base_generation);
        assert_eq!(collect_ids(&current_base), vec!["alpha", "beta"]);
    }

    #[test]
    fn query_builder_rejects_a_different_base_generation() {
        let (_temp, store, query_store) = stores();
        let first = publish_base(
            &store,
            [meta(
                "first",
                1,
                "match",
                "/missing/first.jsonl".to_string(),
            )],
        );
        let builder = query_store.begin_build(&first).expect("query builder");
        let second = publish_base(
            &store,
            [meta(
                "second",
                2,
                "match",
                "/missing/second.jsonl".to_string(),
            )],
        );

        assert!(matches!(
            build_query_manifest(&second, "match", builder, &|| false),
            Err(ManifestError::InvalidOptions(_))
        ));
        assert!(query_store.open_reader("codex").is_none());
    }

    #[test]
    fn newer_query_epoch_rejects_an_older_late_publication() {
        let (_temp, store, query_store) = stores();
        let base = publish_base(
            &store,
            [
                meta("alpha", 20, "alpha", "/missing/alpha.jsonl".to_string()),
                meta("beta", 10, "beta", "/missing/beta.jsonl".to_string()),
            ],
        );
        let old = query_store.begin_build(&base).expect("old query");
        let new = query_store.begin_build(&base).expect("new query");
        build_query_manifest(&base, "beta", new, &|| false).expect("publish new query");

        assert!(matches!(
            build_query_manifest(&base, "alpha", old, &|| false),
            Err(ManifestError::Cancelled)
        ));
        assert_eq!(
            collect_ids(&query_store.open_reader("codex").expect("current query")),
            vec!["beta"]
        );
    }
}

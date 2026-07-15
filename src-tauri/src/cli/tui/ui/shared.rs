use super::*;
use crate::cli::tui::data;
use url::Url;

pub(super) fn pane_border_style(app: &App, pane: Focus, theme: &super::theme::Theme) -> Style {
    if app.focus == pane {
        Style::default()
            .fg(theme.accent)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme.dim)
    }
}

pub(super) fn selection_style(theme: &super::theme::Theme) -> Style {
    if theme.no_color {
        Style::default().add_modifier(Modifier::REVERSED)
    } else {
        Style::default()
            .fg(theme.on_accent)
            .bg(theme.accent)
            .add_modifier(Modifier::BOLD)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PaginationFooterStatus {
    Idle,
    NextBoundary,
    PreviousBoundary,
    PreparingNext,
    NextReady,
    LoadingPage(usize),
    LoadError,
    End,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct PaginationFooter {
    pub page: usize,
    pub start: usize,
    pub end: usize,
    pub total: usize,
    pub status: PaginationFooterStatus,
}

/// Adds pagination state to a list's bottom border without consuming a row.
/// The action is kept when space is tight; the range label is intentionally
/// dropped first so narrow terminals still expose the next available input.
pub(super) fn with_pagination_footer<'a>(
    mut block: Block<'a>,
    area_width: u16,
    footer: PaginationFooter,
    theme: &super::theme::Theme,
    tick: u64,
) -> Block<'a> {
    let available = area_width.saturating_sub(2) as usize;
    if available == 0 || footer.total == 0 {
        return block;
    }

    let range_full =
        texts::tui_pagination_range(footer.page.max(1), footer.start, footer.end, footer.total);
    let range_compact = texts::tui_pagination_range_compact(
        footer.page.max(1),
        footer.start,
        footer.end,
        footer.total,
    );
    let range = fit_owned_label(&[range_full, range_compact], available);
    let action = pagination_action_label(footer, available, tick, theme);

    match (range, action) {
        (Some(range), Some((action, action_style)))
            if padded_width(&range)
                .saturating_add(padded_width(&action))
                .saturating_add(1)
                <= available =>
        {
            block = block
                .title_bottom(
                    Line::styled(format!(" {range} "), Style::default().fg(theme.dim))
                        .alignment(Alignment::Left),
                )
                .title_bottom(
                    Line::styled(format!(" {action} "), action_style).alignment(Alignment::Right),
                );
        }
        (_, Some((action, action_style))) => {
            block = block.title_bottom(
                Line::styled(format!(" {action} "), action_style).alignment(Alignment::Right),
            );
        }
        (Some(range), None) => {
            block = block.title_bottom(
                Line::styled(format!(" {range} "), Style::default().fg(theme.dim))
                    .alignment(Alignment::Left),
            );
        }
        (None, None) => {}
    }

    block
}

fn pagination_action_label(
    footer: PaginationFooter,
    available: usize,
    tick: u64,
    theme: &super::theme::Theme,
) -> Option<(String, Style)> {
    let (labels, style): (Vec<String>, Style) = match footer.status {
        PaginationFooterStatus::Idle => return None,
        PaginationFooterStatus::NextBoundary => (
            vec![
                texts::tui_pagination_next_trigger().to_string(),
                texts::tui_pagination_next_trigger_compact().to_string(),
                texts::tui_pagination_next_trigger_minimal().to_string(),
            ],
            selection_style(theme),
        ),
        PaginationFooterStatus::PreviousBoundary => (
            vec![
                texts::tui_pagination_previous_trigger().to_string(),
                texts::tui_pagination_previous_trigger_compact().to_string(),
                texts::tui_pagination_previous_trigger_minimal().to_string(),
            ],
            selection_style(theme),
        ),
        PaginationFooterStatus::PreparingNext => (
            vec![format!(
                "{} {}",
                pagination_spinner(tick),
                texts::tui_pagination_preparing_next()
            )],
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ),
        PaginationFooterStatus::NextReady => (
            vec![format!("> {}", texts::tui_pagination_next_ready())],
            Style::default().fg(theme.ok).add_modifier(Modifier::BOLD),
        ),
        PaginationFooterStatus::LoadingPage(page) => (
            vec![format!(
                "{} {}",
                pagination_spinner(tick),
                texts::tui_pagination_loading_page(page)
            )],
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ),
        PaginationFooterStatus::LoadError => (
            vec![format!("! {}", texts::tui_pagination_load_failed())],
            Style::default().fg(theme.warn).add_modifier(Modifier::BOLD),
        ),
        PaginationFooterStatus::End => (
            vec![format!("[end] {}", texts::tui_pagination_end(footer.total))],
            Style::default().fg(theme.dim),
        ),
    };
    let label = fit_owned_label(&labels, available)?;
    Some((label, style))
}

fn pagination_spinner(tick: u64) -> &'static str {
    match tick % 4 {
        0 => "⠋",
        1 => "⠙",
        2 => "⠹",
        _ => "⠸",
    }
}

fn fit_owned_label(labels: &[String], available: usize) -> Option<String> {
    labels
        .iter()
        .find(|label| padded_width(label) <= available)
        .cloned()
}

fn padded_width(label: &str) -> usize {
    UnicodeWidthStr::width(label).saturating_add(2)
}

pub(super) fn inactive_chip_style(theme: &super::theme::Theme) -> Style {
    if theme.no_color {
        Style::default()
    } else {
        Style::default().fg(theme.fg_strong).bg(theme.surface)
    }
}

pub(super) fn active_chip_style(theme: &super::theme::Theme) -> Style {
    if theme.no_color {
        Style::default().add_modifier(Modifier::REVERSED)
    } else {
        Style::default()
            .fg(theme.on_accent)
            .bg(theme.accent)
            .add_modifier(Modifier::BOLD)
    }
}

/// Border style for overlay dialogs.
/// `attention = true` for overlays that require user action (Confirm, Update prompts).
/// `attention = false` for informational overlays (Help, TextView, pickers).
pub(super) fn overlay_border_style(theme: &super::theme::Theme, attention: bool) -> Style {
    if attention {
        Style::default().fg(theme.accent)
    } else {
        Style::default().fg(theme.dim)
    }
}

pub(super) fn transient_feedback_color(theme: &super::theme::Theme, kind: &ToastKind) -> Color {
    match kind {
        ToastKind::Info | ToastKind::Success => theme.accent,
        ToastKind::Warning => theme.warn,
        ToastKind::Error => theme.err,
    }
}

/// Left-pad a cell value with one space for visual inset inside table rows.
pub(super) fn cell_pad(s: &str) -> String {
    format!(" {s}")
}

pub(super) fn strip_trailing_colon(label: &str) -> &str {
    label.trim_end_matches([':', '：'])
}

pub(super) fn pad_to_display_width(label: &str, width: usize) -> String {
    let clean = strip_trailing_colon(label);
    let w = UnicodeWidthStr::width(clean);
    if w >= width {
        clean.to_string()
    } else {
        format!("{clean}{}", " ".repeat(width - w))
    }
}

pub(super) fn truncate_to_display_width(text: &str, width: u16) -> String {
    let width = width as usize;
    if width == 0 {
        return String::new();
    }

    const MAX_CHARS_PER_COLUMN: usize = 4;
    const EXTRA_ZERO_WIDTH_CHARS: usize = 16;
    let max_chars = width
        .saturating_mul(MAX_CHARS_PER_COLUMN)
        .saturating_add(EXTRA_ZERO_WIDTH_CHARS);
    let mut out = String::new();
    let mut used = 0usize;
    let mut end_byte = 0usize;
    let mut truncated = false;
    for (count, (byte, c)) in text.char_indices().enumerate() {
        if count >= max_chars {
            truncated = true;
            break;
        }
        let w = UnicodeWidthChar::width(c).unwrap_or(0);
        if used.saturating_add(w) > width {
            truncated = true;
            break;
        }
        out.push(c);
        used = used.saturating_add(w);
        end_byte = byte.saturating_add(c.len_utf8());
    }
    truncated |= end_byte < text.len();

    if !truncated {
        return out;
    }
    if width == 1 {
        return "…".to_string();
    }
    while used > width.saturating_sub(1) {
        let Some(c) = out.pop() else {
            break;
        };
        used = used.saturating_sub(UnicodeWidthChar::width(c).unwrap_or(0));
    }
    out.push('…');
    out
}

/// Produce a small passive summary without trimming, measuring, or cloning an
/// arbitrary complete input. Passive rows always show a bounded prefix and an
/// ellipsis when the source exceeds this fixed safety budget.
pub(super) fn bounded_trimmed_text_for_display(text: &str) -> String {
    const MAX_DISPLAY_WIDTH: usize = 512;
    const MAX_SCANNED_CHARS: usize = 2_048;

    if text.is_empty() {
        return String::new();
    }

    let mut out = String::new();
    let mut used = 0usize;
    let mut started = false;
    let mut end_byte = 0usize;
    let mut truncated = false;

    for (count, (byte, ch)) in text.char_indices().enumerate() {
        if count >= MAX_SCANNED_CHARS {
            truncated = true;
            break;
        }
        end_byte = byte.saturating_add(ch.len_utf8());
        if !started && ch.is_whitespace() {
            continue;
        }
        started = true;
        let char_width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if used.saturating_add(char_width) > MAX_DISPLAY_WIDTH {
            truncated = true;
            break;
        }
        out.push(ch);
        used = used.saturating_add(char_width);
    }
    truncated |= end_byte < text.len();

    while out.chars().next_back().is_some_and(char::is_whitespace) {
        let Some(ch) = out.pop() else {
            break;
        };
        used = used.saturating_sub(UnicodeWidthChar::width(ch).unwrap_or(0));
    }

    if truncated {
        while used > MAX_DISPLAY_WIDTH.saturating_sub(1) {
            let Some(ch) = out.pop() else {
                break;
            };
            used = used.saturating_sub(UnicodeWidthChar::width(ch).unwrap_or(0));
        }
        out.push('…');
    }
    out
}

/// Standard page shell: the outer bordered block with a padded title, the
/// page key bar (always visible, dimmed without content focus), and an
/// optional summary bar. Returns the body rect below them.
pub(super) fn render_page_frame(
    frame: &mut Frame<'_>,
    area: Rect,
    theme: &super::theme::Theme,
    app: &App,
    title: &str,
    keys: &[(&str, &str)],
    summary: Option<String>,
) -> Rect {
    let outer = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(pane_border_style(app, Focus::Content, theme))
        .title(format!(" {} ", icons::strip_icon(title)));
    frame.render_widget(outer.clone(), area);
    let inner = outer.inner(area);

    let constraints = if summary.is_some() {
        vec![
            Constraint::Length(1),
            Constraint::Length(3),
            Constraint::Min(0),
        ]
    } else {
        vec![Constraint::Length(1), Constraint::Min(0)]
    };
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(inner);

    render_page_key_bar(frame, chunks[0], theme, keys, app.focus == Focus::Content);
    if let Some(summary) = summary {
        render_summary_bar(frame, chunks[1], theme, summary);
    }

    *chunks.last().expect("page frame always has a body chunk")
}

/// Sub-page titles show their place in the hierarchy (" Usage › Details ")
/// so nesting depth stays visible and Esc's destination is predictable.
pub(super) fn breadcrumb_title(segments: &[&str]) -> String {
    // Hand-rolled callers don't pass through `render_page_frame`, so strip the
    // leading emoji here too (no-op in emoji mode / for non-emoji segments).
    format!(" {} ", icons::strip_icon(&breadcrumb_path(segments)))
}

/// Breadcrumb path without the surrounding padding that `breadcrumb_title`
/// adds. Use with `render_page_frame`, which wraps the title itself.
pub(super) fn breadcrumb_path(segments: &[&str]) -> String {
    segments.join(" › ")
}

/// Centered guidance for empty list screens: a bold title, a muted
/// subtitle, and key chips for the actions that create the first entry.
/// The first action renders as the primary (accent) chip.
pub(super) fn render_empty_state(
    frame: &mut Frame<'_>,
    area: Rect,
    theme: &super::theme::Theme,
    title: &str,
    subtitle: &str,
    actions: &[(&str, &str)],
) {
    let title_style = Style::default().add_modifier(Modifier::BOLD);
    let subtitle_style = Style::default().fg(theme.comment);
    let primary_style = if theme.no_color {
        Style::default().add_modifier(Modifier::BOLD)
    } else {
        Style::default()
            .fg(theme.on_accent)
            .bg(theme.accent)
            .add_modifier(Modifier::BOLD)
    };
    let secondary_style = if theme.no_color {
        Style::default()
    } else {
        Style::default()
            .fg(theme.dim)
            .bg(theme.surface)
            .add_modifier(Modifier::BOLD)
    };

    let mut content_lines = vec![
        Line::styled(title.to_string(), title_style),
        Line::raw(""),
        Line::styled(subtitle.to_string(), subtitle_style),
        Line::raw(""),
    ];
    for (idx, (key, label)) in actions.iter().enumerate() {
        let style = if idx == 0 {
            primary_style
        } else {
            secondary_style
        };
        content_lines.push(Line::from(vec![Span::styled(
            format!("  {key}  {label}  "),
            style,
        )]));
    }

    let top_padding = area.height.saturating_sub(content_lines.len() as u16) / 2;
    let mut lines = Vec::with_capacity(top_padding as usize + content_lines.len());
    for _ in 0..top_padding {
        lines.push(Line::raw(""));
    }
    lines.extend(content_lines);

    frame.render_widget(
        Paragraph::new(lines)
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: false }),
        area,
    );
}

/// Two-column field tables clip the value cell silently at the pane edge;
/// pre-truncate the value with an ellipsis so a cut-off reads as one.
pub(super) fn truncated_value_cell(
    value: &str,
    table_width: u16,
    label_col_width: u16,
    theme: &super::theme::Theme,
) -> String {
    let symbol_width = UnicodeWidthStr::width(highlight_symbol(theme)) as u16;
    // Chrome left of the value column: label column + 1 column spacing +
    // the selection highlight symbol.
    let value_width = table_width
        .saturating_sub(label_col_width)
        .saturating_sub(1)
        .saturating_sub(symbol_width);
    truncate_to_display_width(value, value_width)
}

pub(super) fn format_sync_time_local_to_minute(ts: i64) -> Option<String> {
    Local
        .timestamp_opt(ts, 0)
        .single()
        .map(|dt| dt.format("%Y/%m/%d %H:%M").to_string())
}

pub(super) fn format_uptime_compact(total_seconds: u64) -> String {
    let days = total_seconds / 86_400;
    let hours = (total_seconds % 86_400) / 3_600;
    let minutes = (total_seconds % 3_600) / 60;
    let seconds = total_seconds % 60;

    let mut parts = Vec::new();
    if days > 0 {
        parts.push(format!("{days}d"));
    }
    if hours > 0 {
        parts.push(format!("{hours}h"));
    }
    if minutes > 0 {
        parts.push(format!("{minutes}m"));
    }
    if seconds > 0 || parts.is_empty() {
        parts.push(format!("{seconds}s"));
    }

    parts.join(" ")
}

pub(super) fn format_estimated_token_compact(total: u64) -> String {
    if total < 1_000 {
        return format!("~{total}");
    }

    if total < 10_000 {
        return format!("~{:.1}k", total as f64 / 1_000.0);
    }

    if total < 1_000_000 {
        return format!("~{}k", total / 1_000);
    }

    if total < 10_000_000 {
        return format!("~{:.1}M", total as f64 / 1_000_000.0);
    }

    format!("~{}M", total / 1_000_000)
}

fn quota_tier_label(name: &str) -> String {
    match name {
        "five_hour" => texts::tui_quota_tier_five_hour().to_string(),
        "seven_day" => texts::tui_quota_tier_seven_day().to_string(),
        "seven_day_opus" => texts::tui_quota_tier_seven_day_opus().to_string(),
        "seven_day_sonnet" => texts::tui_quota_tier_seven_day_sonnet().to_string(),
        "weekly_limit" => texts::tui_quota_tier_weekly_limit().to_string(),
        "premium" => texts::tui_quota_tier_premium().to_string(),
        "gemini_pro" => texts::tui_quota_tier_gemini_pro().to_string(),
        "gemini_flash" => texts::tui_quota_tier_gemini_flash().to_string(),
        "gemini_flash_lite" => texts::tui_quota_tier_gemini_flash_lite().to_string(),
        other => other.replace('_', " "),
    }
}

fn quota_percent_text(utilization: f64) -> String {
    format!("{:.0}%", utilization.clamp(0.0, 100.0))
}

fn quota_utilization_style(theme: &super::theme::Theme, utilization: f64) -> Style {
    if theme.no_color {
        return Style::default();
    }

    if utilization >= 90.0 {
        Style::default().fg(theme.err)
    } else if utilization >= 70.0 {
        Style::default().fg(theme.warn)
    } else {
        Style::default().fg(theme.ok)
    }
}

fn quota_relative_time(timestamp_ms: i64) -> String {
    let diff_secs = ((chrono::Utc::now().timestamp_millis() - timestamp_ms).max(0)) / 1000;
    if diff_secs < 60 {
        texts::tui_quota_seconds_ago(diff_secs.max(1))
    } else if diff_secs < 3600 {
        texts::tui_quota_minutes_ago(diff_secs / 60)
    } else if diff_secs < 86_400 {
        texts::tui_quota_hours_ago(diff_secs / 3600)
    } else {
        texts::tui_quota_days_ago(diff_secs / 86_400)
    }
}

fn quota_relative_time_compact(timestamp_ms: i64) -> String {
    let diff_secs = ((chrono::Utc::now().timestamp_millis() - timestamp_ms).max(0)) / 1000;
    let (value, unit) = if diff_secs < 60 {
        (diff_secs.max(1), "s")
    } else if diff_secs < 3600 {
        (diff_secs / 60, "m")
    } else if diff_secs < 86_400 {
        (diff_secs / 3600, "h")
    } else {
        (diff_secs / 86_400, "d")
    };

    if i18n::is_chinese() {
        format!("{value}{unit}前")
    } else {
        format!("{value}{unit} ago")
    }
}

pub(super) fn quota_compact_line(
    state: Option<&data::ProviderQuotaState>,
    theme: &super::theme::Theme,
    quiet_missing: bool,
) -> Option<Line<'static>> {
    let state = state?;

    if state.loading && state.quota.is_none() {
        return Some(Line::from(Span::styled(
            texts::tui_quota_loading().to_string(),
            Style::default().fg(theme.surface),
        )));
    }

    if state.last_error.is_some() && state.quota.is_none() {
        return Some(Line::from(Span::styled(
            texts::tui_quota_query_failed().to_string(),
            Style::default().fg(theme.warn),
        )));
    }

    let quota = state.quota.as_ref()?;
    if let data::ProviderUsageQuota::Script(result) = quota {
        return script_usage_compact_line(
            result,
            state.loading,
            state.updated_at,
            theme,
            quiet_missing,
        );
    }

    let data::ProviderUsageQuota::Subscription(quota) = quota else {
        return None;
    };
    match quota.credential_status {
        crate::services::CredentialStatus::NotFound => {
            if quiet_missing {
                return None;
            }
            return Some(Line::from(Span::styled(
                texts::tui_quota_not_available().to_string(),
                Style::default().fg(theme.surface),
            )));
        }
        crate::services::CredentialStatus::ParseError => {
            if quiet_missing {
                return None;
            }
            return Some(Line::from(Span::styled(
                texts::tui_quota_parse_error().to_string(),
                Style::default().fg(theme.warn),
            )));
        }
        crate::services::CredentialStatus::Expired if !quota.success => {
            return Some(Line::from(Span::styled(
                texts::tui_quota_expired().to_string(),
                Style::default().fg(theme.warn),
            )));
        }
        _ => {}
    }

    if !quota.success {
        return Some(Line::from(Span::styled(
            texts::tui_quota_query_failed().to_string(),
            Style::default().fg(theme.err),
        )));
    }

    let tiers = quota
        .tiers
        .iter()
        .filter(|tier| tier.name != "seven_day_sonnet")
        .take(2)
        .collect::<Vec<_>>();
    if tiers.is_empty() {
        if quiet_missing {
            return None;
        }
        return Some(Line::from(Span::styled(
            texts::tui_quota_not_available().to_string(),
            Style::default().fg(theme.surface),
        )));
    }

    let mut spans = Vec::new();
    for (idx, tier) in tiers.iter().enumerate() {
        if idx > 0 {
            spans.push(Span::raw("  "));
        }
        spans.push(Span::styled(
            format!("{} ", quota_tier_label(&tier.name)),
            Style::default().fg(theme.comment),
        ));
        spans.push(Span::styled(
            quota_percent_text(tier.utilization),
            quota_utilization_style(theme, tier.utilization),
        ));
    }
    if let Some(checked) = quota.queried_at.map(quota_relative_time_compact) {
        if !spans.is_empty() {
            spans.push(Span::styled(" | ", Style::default().fg(theme.comment)));
        }
        spans.push(Span::styled(checked, Style::default().fg(theme.surface)));
    }
    if state.loading {
        if !spans.is_empty() {
            spans.push(Span::styled(" | ", Style::default().fg(theme.comment)));
        }
        spans.push(Span::styled(
            texts::tui_quota_loading().to_string(),
            Style::default().fg(theme.surface),
        ));
    }
    Some(Line::from(spans))
}

fn script_usage_compact_line(
    result: &crate::provider::UsageResult,
    loading: bool,
    updated_at: Option<i64>,
    theme: &super::theme::Theme,
    quiet_missing: bool,
) -> Option<Line<'static>> {
    if !result.success {
        return Some(Line::from(Span::styled(
            texts::tui_quota_query_failed().to_string(),
            Style::default().fg(theme.err),
        )));
    }

    let data = result.data.as_ref()?;
    let mut spans = Vec::new();
    for (idx, item) in data.iter().take(2).enumerate() {
        if idx > 0 {
            spans.push(Span::raw("  "));
        }
        if let Some(name) = display_usage_plan_name(item) {
            spans.push(Span::styled(
                format!("{} ", name.trim()),
                Style::default().fg(theme.comment),
            ));
        }
        spans.push(Span::styled(
            usage_value_summary(item).unwrap_or_else(|| texts::tui_quota_ok().to_string()),
            Style::default().fg(theme.cyan),
        ));
    }

    if spans.is_empty() {
        if quiet_missing {
            return None;
        }
        return Some(Line::from(Span::styled(
            texts::tui_quota_not_available().to_string(),
            Style::default().fg(theme.surface),
        )));
    }

    if loading {
        spans.push(Span::styled(" | ", Style::default().fg(theme.comment)));
        spans.push(Span::styled(
            texts::tui_quota_loading().to_string(),
            Style::default().fg(theme.surface),
        ));
    } else if let Some(checked) = updated_at.map(quota_relative_time) {
        spans.push(Span::styled(" | ", Style::default().fg(theme.comment)));
        spans.push(Span::styled(checked, Style::default().fg(theme.surface)));
    }

    Some(Line::from(spans))
}

fn display_usage_plan_name(item: &crate::provider::UsageData) -> Option<&str> {
    item.plan_name.as_deref().filter(|value| {
        let trimmed = value.trim();
        !trimmed.is_empty() && !trimmed.eq_ignore_ascii_case("default")
    })
}

fn usage_value_summary(item: &crate::provider::UsageData) -> Option<String> {
    let unit = item.unit.as_deref().unwrap_or("");
    match (item.remaining, item.total, item.used) {
        (Some(remaining), Some(total), Some(used)) => Some(format!(
            "{} / {} {} left, {} used",
            usage_number(remaining),
            usage_number(total),
            unit,
            usage_number(used)
        )),
        (Some(remaining), Some(total), None) => Some(format!(
            "{} / {} {} left",
            usage_number(remaining),
            usage_number(total),
            unit
        )),
        (Some(remaining), None, _) => Some(format!("{} {}", usage_number(remaining), unit)),
        (None, Some(total), Some(used)) => Some(format!(
            "{} / {} {} used",
            usage_number(used),
            usage_number(total),
            unit
        )),
        (None, Some(total), None) => Some(format!("total {} {}", usage_number(total), unit)),
        (None, None, Some(used)) => Some(format!("used {} {}", usage_number(used), unit)),
        _ => None,
    }
    .map(|value| value.trim().to_string())
}

fn usage_number(value: f64) -> String {
    if (value.fract()).abs() < f64::EPSILON {
        format!("{value:.0}")
    } else {
        format!("{value:.2}")
    }
}

pub(super) fn kv_line<'a>(
    theme: &super::theme::Theme,
    label: &'a str,
    label_width: usize,
    value_spans: Vec<Span<'a>>,
) -> Line<'a> {
    let mut spans = vec![
        Span::raw(" "), // internal padding: keep content away from │
        Span::styled(
            pad_to_display_width(label, label_width),
            Style::default()
                .fg(theme.comment)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(": "),
    ];
    spans.extend(value_spans);
    Line::from(spans)
}

pub(super) fn highlight_symbol(theme: &super::theme::Theme) -> &'static str {
    if theme.no_color {
        texts::tui_highlight_symbol()
    } else {
        ""
    }
}

pub(super) const CONTENT_INSET_LEFT: u16 = 1;

// Overlay size tiers — percentage-based (large content)
pub(super) const OVERLAY_LG: (u16, u16) = (90, 90);
pub(super) const OVERLAY_MD: (u16, u16) = (78, 62);
// Overlay size tiers — fixed character dimensions (dialogs)
pub(super) const OVERLAY_FIXED_LG: (u16, u16) = (70, 20);
pub(super) const OVERLAY_FIXED_MD: (u16, u16) = (60, 9);
pub(super) const OVERLAY_FIXED_SM: (u16, u16) = (50, 6);
pub(super) const TOAST_MIN_WIDTH: u16 = 28;
pub(super) const TOAST_MAX_WIDTH: u16 = 72;
pub(super) const TOAST_MIN_HEIGHT: u16 = 5;

pub(super) fn key_bar_line(theme: &super::theme::Theme, items: &[(&str, &str)]) -> Line<'static> {
    if theme.no_color {
        let mut parts = Vec::new();
        for (k, v) in items {
            parts.push(format!("{k}={v}"));
        }
        return Line::raw(parts.join("  "));
    }

    let base = inactive_chip_style(theme);
    let key = base.add_modifier(Modifier::BOLD);

    let mut spans: Vec<Span<'static>> = vec![Span::styled(" ", base)];
    for (idx, (k, v)) in items.iter().enumerate() {
        if idx > 0 {
            spans.push(Span::styled("  ", base));
        }
        spans.push(Span::styled((*k).to_string(), key));
        spans.push(Span::styled(" ", base));
        spans.push(Span::styled((*v).to_string(), base));
    }
    spans.push(Span::styled(" ", base));
    Line::from(spans)
}

fn key_bar_chip_width(key: &str, value: &str) -> usize {
    UnicodeWidthStr::width(key) + 1 + UnicodeWidthStr::width(value)
}

/// How many leading chips fit into `width`, mirroring key_bar_line's
/// layout: 1-column padding on each side, 2 columns between chips.
fn key_bar_fit_count(items: &[(&str, &str)], width: u16) -> usize {
    let width = width as usize;
    let mut used = 2usize;
    let mut count = 0usize;
    for (idx, (key, value)) in items.iter().enumerate() {
        let mut chip = key_bar_chip_width(key, value);
        if idx > 0 {
            chip += 2;
        }
        if used + chip > width {
            break;
        }
        used += chip;
        count += 1;
    }
    count
}

/// Key bars are single-row: chips past the available width used to be
/// silently cut off mid-list. Keep the leading (highest-priority) chips
/// that fit and close with a "? more" hint pointing at the help sheet.
fn key_bar_items_for_width<'a>(
    items: &'a [(&'a str, &'a str)],
    width: u16,
) -> Vec<(&'a str, &'a str)> {
    if key_bar_fit_count(items, width) == items.len() {
        return items.to_vec();
    }

    let more = texts::tui_key_more();
    let reserved = (key_bar_chip_width("?", more) + 2) as u16;
    let count = key_bar_fit_count(items, width.saturating_sub(reserved));
    let mut fitted = items[..count].to_vec();
    fitted.push(("?", more));
    fitted
}

fn key_bar_line_dimmed(theme: &super::theme::Theme, items: &[(&str, &str)]) -> Line<'static> {
    if theme.no_color {
        return key_bar_line(theme, items);
    }

    let base = Style::default().fg(theme.comment);
    let key = base.add_modifier(Modifier::BOLD);

    let mut spans: Vec<Span<'static>> = vec![Span::styled(" ", base)];
    for (idx, (k, v)) in items.iter().enumerate() {
        if idx > 0 {
            spans.push(Span::styled("  ", base));
        }
        spans.push(Span::styled((*k).to_string(), key));
        spans.push(Span::styled(" ", base));
        spans.push(Span::styled((*v).to_string(), base));
    }
    spans.push(Span::styled(" ", base));
    Line::from(spans)
}

/// Page-level key bar: always visible so the available actions can be
/// discovered while the nav pane has focus; rendered muted (no chip
/// background) until the content pane is focused.
pub(super) fn render_page_key_bar(
    frame: &mut Frame<'_>,
    area: Rect,
    theme: &super::theme::Theme,
    items: &[(&str, &str)],
    focused: bool,
) {
    let fitted = key_bar_items_for_width(items, area.width);
    let line = if focused {
        key_bar_line(theme, &fitted)
    } else {
        key_bar_line_dimmed(theme, &fitted)
    };
    frame.render_widget(Paragraph::new(line).alignment(Alignment::Center), area);
}

/// Render a left-aligned key bar. Used for main-screen footers where keys
/// are read left-to-right in priority order.
pub(super) fn render_key_bar(
    frame: &mut Frame<'_>,
    area: Rect,
    theme: &super::theme::Theme,
    items: &[(&str, &str)],
) {
    let fitted = key_bar_items_for_width(items, area.width);
    frame.render_widget(
        Paragraph::new(key_bar_line(theme, &fitted)).alignment(Alignment::Left),
        area,
    );
}

/// Render a center-aligned key bar. Used inside overlay dialogs where the
/// available actions are few and visually centered looks balanced.
pub(super) fn render_key_bar_center(
    frame: &mut Frame<'_>,
    area: Rect,
    theme: &super::theme::Theme,
    items: &[(&str, &str)],
) {
    let fitted = key_bar_items_for_width(items, area.width);
    frame.render_widget(
        Paragraph::new(key_bar_line(theme, &fitted)).alignment(Alignment::Center),
        area,
    );
}

pub(super) fn render_summary_bar(
    frame: &mut Frame<'_>,
    area: Rect,
    theme: &super::theme::Theme,
    summary: String,
) {
    let summary_block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(Style::default().fg(theme.dim));
    frame.render_widget(
        Paragraph::new(Line::raw(format!("  {summary}")))
            .style(Style::default().fg(theme.dim))
            .wrap(Wrap { trim: false })
            .block(summary_block),
        area,
    );
}

pub(super) fn inset_left(area: Rect, left: u16) -> Rect {
    if area.width <= left {
        return Rect {
            x: area.x,
            y: area.y,
            width: area.width,
            height: area.height,
        };
    }
    Rect {
        x: area.x + left,
        y: area.y,
        width: area.width - left,
        height: area.height,
    }
}

pub(super) fn inset_horizontal(area: Rect, inset: u16) -> Rect {
    let shrink = inset.saturating_mul(2);
    if area.width <= shrink {
        return area;
    }
    Rect {
        x: area.x + inset,
        y: area.y,
        width: area.width - shrink,
        height: area.height,
    }
}

pub(super) fn inset_top(area: Rect, top: u16) -> Rect {
    if area.height <= top {
        return Rect {
            x: area.x,
            y: area.y,
            width: area.width,
            height: area.height,
        };
    }
    Rect {
        x: area.x,
        y: area.y + top,
        width: area.width,
        height: area.height - top,
    }
}

/// Returns a fixed-size slice around the selected row. Renderers use this
/// before constructing ratatui rows so a large imported collection cannot turn
/// every periodic redraw into O(total rows) work.
pub(super) fn visible_selection_window(
    len: usize,
    selected: usize,
    capacity: usize,
) -> std::ops::Range<usize> {
    if len == 0 || capacity == 0 {
        return 0..0;
    }

    let capacity = capacity.min(len);
    let selected = selected.min(len - 1);
    let start = selected
        .saturating_sub(capacity / 2)
        .min(len.saturating_sub(capacity));
    start..start.saturating_add(capacity)
}

pub(super) fn field_label_column_width<'a, I>(labels: I, left_padding: u16) -> u16
where
    I: IntoIterator<Item = &'a str>,
{
    let max = labels
        .into_iter()
        .map(|label| UnicodeWidthStr::width(label) as u16)
        .max()
        .unwrap_or(0);
    max.saturating_add(left_padding)
}

pub(super) fn redacted_secret_placeholder() -> &'static str {
    "[redacted]"
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PreviewValuePolicy {
    Opaque,
    SecretMap,
    Walk,
    PublicText,
    PublicStringArray,
    SafeUrl,
    SafeCommand,
    PublicBoolean,
    TokenCount,
    NonNegativeNumber,
}

const PREVIEW_NODE_BUDGET: usize = 2_048;
const PREVIEW_MAX_COLLECTION_ITEMS: usize = 128;
const PREVIEW_MAX_DEPTH: usize = 16;
const PREVIEW_KEY_MAX_CHARS: usize = 128;
const PREVIEW_KEY_MAX_INPUT_BYTES: usize = 512;
pub(super) const TOML_PREVIEW_MAX_INPUT_BYTES: usize = 256 * 1024;

struct PreviewBudget {
    remaining: usize,
}

impl PreviewBudget {
    fn new() -> Self {
        Self {
            remaining: PREVIEW_NODE_BUDGET,
        }
    }

    fn take(&mut self) -> bool {
        if self.remaining == 0 {
            return false;
        }
        self.remaining -= 1;
        true
    }
}

pub(super) fn redact_sensitive_json(value: &Value) -> Value {
    let mut budget = PreviewBudget::new();
    redact_sensitive_json_with_budget(value, &mut budget, 0)
}

fn redact_sensitive_json_with_budget(
    value: &Value,
    budget: &mut PreviewBudget,
    depth: usize,
) -> Value {
    if depth >= PREVIEW_MAX_DEPTH || budget.remaining == 0 {
        return preview_truncated_value();
    }

    match value {
        Value::Object(map) => {
            let mut out = serde_json::Map::new();
            let mut rendered = 0usize;
            for (key, value) in map.iter().take(PREVIEW_MAX_COLLECTION_ITEMS) {
                if !budget.take() {
                    break;
                }
                let display_key = unique_preview_key(&out, bounded_preview_key(key));
                out.insert(
                    display_key,
                    redact_json_entry(key, value, budget, depth.saturating_add(1)),
                );
                rendered += 1;
            }
            let hidden = map.len().saturating_sub(rendered);
            if hidden > 0 {
                insert_json_truncation(&mut out, hidden);
            }
            Value::Object(out)
        }
        Value::Array(_) => redact_value_payload(value),
        _ => redact_value_payload(value),
    }
}

/// Codex auth data has no passive-display scalar values. Slot names and shape
/// remain useful, but every leaf stays opaque until the user opens the editor.
pub(super) fn redact_codex_auth_json(value: &Value) -> Value {
    let mut budget = PreviewBudget::new();
    redact_secret_map(value, &mut budget)
}

fn redact_json_entry(key: &str, value: &Value, budget: &mut PreviewBudget, depth: usize) -> Value {
    match preview_value_policy(key) {
        PreviewValuePolicy::Opaque => redact_value_payload(value),
        PreviewValuePolicy::SecretMap => redact_secret_map(value, budget),
        PreviewValuePolicy::Walk => match value {
            Value::Object(_) => redact_sensitive_json_with_budget(value, budget, depth),
            Value::Array(items) => {
                let mut out = Vec::new();
                for item in items.iter().take(PREVIEW_MAX_COLLECTION_ITEMS) {
                    if !budget.take() {
                        break;
                    }
                    out.push(match item {
                        Value::Object(_) => redact_sensitive_json_with_budget(item, budget, depth),
                        _ => redact_value_payload(item),
                    });
                }
                let hidden = items.len().saturating_sub(out.len());
                if hidden > 0 {
                    out.push(preview_truncated_count_value(hidden));
                }
                Value::Array(out)
            }
            _ => redact_value_payload(value),
        },
        PreviewValuePolicy::PublicText => value
            .as_str()
            .filter(|text| is_safe_public_text(text))
            .map_or_else(
                || redact_value_payload(value),
                |text| Value::String(text.to_string()),
            ),
        PreviewValuePolicy::PublicStringArray => match value {
            Value::Array(items) => {
                let mut out = items
                    .iter()
                    .take(PREVIEW_MAX_COLLECTION_ITEMS)
                    .map(|item| {
                        item.as_str()
                            .filter(|text| is_safe_public_text(text))
                            .map_or_else(
                                || redact_value_payload(item),
                                |text| Value::String(text.to_string()),
                            )
                    })
                    .collect::<Vec<_>>();
                let hidden = items.len().saturating_sub(out.len());
                if hidden > 0 {
                    out.push(preview_truncated_count_value(hidden));
                }
                Value::Array(out)
            }
            _ => redact_value_payload(value),
        },
        PreviewValuePolicy::SafeUrl => value.as_str().map_or_else(
            || redact_value_payload(value),
            |text| Value::String(safe_url_origin_for_display(text)),
        ),
        PreviewValuePolicy::SafeCommand => value.as_str().map_or_else(
            || redact_value_payload(value),
            |text| Value::String(safe_executable_basename_for_display(text)),
        ),
        PreviewValuePolicy::PublicBoolean => value
            .as_bool()
            .map(Value::Bool)
            .unwrap_or_else(|| redact_value_payload(value)),
        PreviewValuePolicy::TokenCount => value
            .as_u64()
            .map(|number| Value::Number(number.into()))
            .unwrap_or_else(|| redact_value_payload(value)),
        PreviewValuePolicy::NonNegativeNumber => value
            .as_f64()
            .filter(|number| number.is_finite() && *number >= 0.0)
            .map_or_else(|| redact_value_payload(value), |_| value.clone()),
    }
}

fn redact_secret_map(value: &Value, budget: &mut PreviewBudget) -> Value {
    let Value::Object(map) = value else {
        return redact_value_payload(value);
    };

    let mut out = serde_json::Map::new();
    let mut rendered = 0usize;
    for key in map.keys().take(PREVIEW_MAX_COLLECTION_ITEMS) {
        if !budget.take() {
            break;
        }
        let display_key = unique_preview_key(&out, bounded_preview_key(key));
        out.insert(
            display_key,
            Value::String(redacted_secret_placeholder().to_string()),
        );
        rendered += 1;
    }
    let hidden = map.len().saturating_sub(rendered);
    if hidden > 0 {
        insert_json_truncation(&mut out, hidden);
    }
    Value::Object(out)
}

fn bounded_preview_key(key: &str) -> String {
    let mut chars = key.chars();
    let mut out = chars
        .by_ref()
        .take(PREVIEW_KEY_MAX_CHARS)
        .collect::<String>();
    if chars.next().is_some() {
        out.push('…');
    }
    out
}

fn unique_preview_key(map: &serde_json::Map<String, Value>, base: String) -> String {
    if !map.contains_key(&base) {
        return base;
    }
    for suffix in 2usize.. {
        let candidate = format!("{base} ({suffix})");
        if !map.contains_key(&candidate) {
            return candidate;
        }
    }
    unreachable!("an unused preview key suffix must exist")
}

fn insert_json_truncation(map: &mut serde_json::Map<String, Value>, hidden: usize) {
    let key = unique_preview_key(map, "…".to_string());
    map.insert(key, preview_truncated_count_value(hidden));
}

fn preview_truncated_count_value(hidden: usize) -> Value {
    Value::String(format!("[preview truncated: {hidden} more entries]"))
}

fn preview_truncated_value() -> Value {
    Value::String("[preview truncated]".to_string())
}

/// Builds a display-only Codex TOML preview with secret-bearing values
/// removed. Invalid TOML is deliberately fail-closed: showing a placeholder
/// is safer than falling back to the original, potentially sensitive text.
pub(super) fn redact_sensitive_toml(text: &str) -> String {
    if text.len() > TOML_PREVIEW_MAX_INPUT_BYTES {
        return texts::tui_preview_omitted_too_large().to_string();
    }
    if text.trim().is_empty() {
        return String::new();
    }

    let Ok(mut value) = toml::from_str::<toml::Value>(text) else {
        return redacted_secret_placeholder().to_string();
    };
    let mut budget = PreviewBudget::new();
    redact_sensitive_toml_value(&mut value, &mut budget, 0);
    let rendered = toml::to_string_pretty(&value)
        .unwrap_or_else(|_| redacted_secret_placeholder().to_string());
    if rendered.len() > TOML_PREVIEW_MAX_INPUT_BYTES {
        texts::tui_preview_omitted_too_large().to_string()
    } else {
        rendered
    }
}

fn redact_sensitive_toml_value(value: &mut toml::Value, budget: &mut PreviewBudget, depth: usize) {
    if depth >= PREVIEW_MAX_DEPTH || budget.remaining == 0 {
        redact_toml_truncated(value);
        return;
    }

    match value {
        toml::Value::Table(table) => {
            let original_len = table.len();
            let original = std::mem::take(table);
            let mut rendered = 0usize;
            for (key, mut child) in original.into_iter().take(PREVIEW_MAX_COLLECTION_ITEMS) {
                if !budget.take() {
                    break;
                }
                redact_toml_entry(&key, &mut child, budget, depth.saturating_add(1));
                let display_key = unique_toml_preview_key(table, bounded_preview_key(&key));
                table.insert(display_key, child);
                rendered += 1;
            }
            insert_toml_truncation(table, original_len.saturating_sub(rendered));
        }
        toml::Value::Array(items) => {
            redact_toml_array(items, budget, depth, false);
        }
        _ => redact_toml_payload(value),
    }
}

fn redact_toml_entry(key: &str, value: &mut toml::Value, budget: &mut PreviewBudget, depth: usize) {
    match preview_value_policy(key) {
        PreviewValuePolicy::Opaque => redact_toml_payload(value),
        PreviewValuePolicy::SecretMap => redact_toml_secret_map(value, budget),
        PreviewValuePolicy::Walk => match value {
            toml::Value::Table(_) => redact_sensitive_toml_value(value, budget, depth),
            toml::Value::Array(items) => {
                redact_toml_array(items, budget, depth, true);
            }
            _ => redact_toml_payload(value),
        },
        PreviewValuePolicy::PublicText => {
            if !matches!(value, toml::Value::String(text) if is_safe_public_text(text)) {
                redact_toml_payload(value);
            }
        }
        PreviewValuePolicy::PublicStringArray => match value {
            toml::Value::Array(items) => {
                let original_len = items.len();
                let original = std::mem::take(items);
                for mut item in original.into_iter().take(PREVIEW_MAX_COLLECTION_ITEMS) {
                    if !budget.take() {
                        break;
                    }
                    if !matches!(&item, toml::Value::String(text) if is_safe_public_text(text)) {
                        redact_toml_payload(&mut item);
                    }
                    items.push(item);
                }
                let hidden = original_len.saturating_sub(items.len());
                if hidden > 0 {
                    items.push(toml_preview_truncation_value(hidden));
                }
            }
            _ => redact_toml_payload(value),
        },
        PreviewValuePolicy::SafeUrl => match value {
            toml::Value::String(text) => {
                *text = safe_url_origin_for_display(text);
            }
            _ => redact_toml_payload(value),
        },
        PreviewValuePolicy::SafeCommand => match value {
            toml::Value::String(text) => {
                *text = safe_executable_basename_for_display(text);
            }
            _ => redact_toml_payload(value),
        },
        PreviewValuePolicy::PublicBoolean => {
            if !value.is_bool() {
                redact_toml_payload(value);
            }
        }
        PreviewValuePolicy::TokenCount => {
            if !matches!(value, toml::Value::Integer(number) if *number >= 0) {
                redact_toml_payload(value);
            }
        }
        PreviewValuePolicy::NonNegativeNumber => {
            let valid = match value {
                toml::Value::Integer(number) => *number >= 0,
                toml::Value::Float(number) => number.is_finite() && *number >= 0.0,
                _ => false,
            };
            if !valid {
                redact_toml_payload(value);
            }
        }
    }
}

fn redact_toml_array(
    items: &mut Vec<toml::Value>,
    budget: &mut PreviewBudget,
    depth: usize,
    walk_tables: bool,
) {
    let original_len = items.len();
    let original = std::mem::take(items);
    for mut item in original.into_iter().take(PREVIEW_MAX_COLLECTION_ITEMS) {
        if !budget.take() {
            break;
        }
        if walk_tables && item.is_table() {
            redact_sensitive_toml_value(&mut item, budget, depth.saturating_add(1));
        } else {
            redact_toml_payload(&mut item);
        }
        items.push(item);
    }
    let hidden = original_len.saturating_sub(items.len());
    if hidden > 0 {
        items.push(toml_preview_truncation_value(hidden));
    }
}

fn redact_toml_payload(value: &mut toml::Value) {
    *value = toml::Value::String(redacted_secret_placeholder().to_string());
}

fn redact_toml_secret_map(value: &mut toml::Value, budget: &mut PreviewBudget) {
    let toml::Value::Table(table) = value else {
        redact_toml_payload(value);
        return;
    };

    let original_len = table.len();
    let original = std::mem::take(table);
    for (key, _child) in original.into_iter().take(PREVIEW_MAX_COLLECTION_ITEMS) {
        if !budget.take() {
            break;
        }
        let display_key = unique_toml_preview_key(table, bounded_preview_key(&key));
        table.insert(
            display_key,
            toml::Value::String(redacted_secret_placeholder().to_string()),
        );
    }
    let hidden = original_len.saturating_sub(table.len());
    insert_toml_truncation(table, hidden);
}

fn redact_toml_truncated(value: &mut toml::Value) {
    *value = toml::Value::String("[preview truncated]".to_string());
}

fn toml_preview_truncation_value(hidden: usize) -> toml::Value {
    toml::Value::String(format!("[preview truncated: {hidden} more entries]"))
}

fn unique_toml_preview_key(table: &toml::map::Map<String, toml::Value>, base: String) -> String {
    if !table.contains_key(&base) {
        return base;
    }
    for suffix in 2usize.. {
        let candidate = format!("{base} ({suffix})");
        if !table.contains_key(&candidate) {
            return candidate;
        }
    }
    unreachable!("an unused TOML preview key suffix must exist")
}

fn insert_toml_truncation(table: &mut toml::map::Map<String, toml::Value>, hidden: usize) {
    if hidden == 0 {
        return;
    }
    let key = unique_toml_preview_key(table, "…".to_string());
    table.insert(key, toml_preview_truncation_value(hidden));
}

pub(super) fn redact_value_payload(value: &Value) -> Value {
    match value {
        Value::Null => Value::Null,
        _ => Value::String(redacted_secret_placeholder().to_string()),
    }
}

fn normalized_display_key(key: &str) -> String {
    key.chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .flat_map(|ch| ch.to_lowercase())
        .collect::<String>()
}

fn preview_value_policy(key: &str) -> PreviewValuePolicy {
    // Key classification normalizes the complete key. Imported JSON can use
    // arbitrarily large object keys, so fail closed before normalization and
    // keep passive previews on a fixed per-key budget.
    if key.len() > PREVIEW_KEY_MAX_INPUT_BYTES {
        return PreviewValuePolicy::Opaque;
    }
    let normalized = normalized_display_key(key);

    if is_env_header_reference_container(&normalized)
        || is_literal_header_container(&normalized)
        || is_literal_env_container(&normalized)
    {
        return PreviewValuePolicy::SecretMap;
    }

    if is_token_metric_key(&normalized) {
        return PreviewValuePolicy::TokenCount;
    }

    if is_sensitive_display_key(&normalized) {
        return PreviewValuePolicy::Opaque;
    }

    if is_safe_url_key(&normalized) {
        return PreviewValuePolicy::SafeUrl;
    }

    if normalized == "command" {
        return PreviewValuePolicy::SafeCommand;
    }

    if matches!(normalized.as_str(), "tags" | "capabilities") {
        return PreviewValuePolicy::PublicStringArray;
    }

    if matches!(
        normalized.as_str(),
        "server"
            | "models"
            | "options"
            | "limit"
            | "limits"
            | "apps"
            | "features"
            | "modelproviders"
            | "mcpservers"
            | "catalog"
            | "modelcatalog"
    ) {
        return PreviewValuePolicy::Walk;
    }

    if matches!(
        normalized.as_str(),
        "id" | "name"
            | "description"
            | "model"
            | "modelid"
            | "modelname"
            | "type"
            | "api"
            | "apimode"
            | "wireapi"
            | "npm"
            | "transport"
            | "protocol"
            | "language"
            | "templatetype"
            | "unit"
            | "reasoningeffort"
            | "reasoningsummary"
    ) {
        return PreviewValuePolicy::PublicText;
    }

    if matches!(
        normalized.as_str(),
        "enabled"
            | "disabled"
            | "claude"
            | "codex"
            | "gemini"
            | "opencode"
            | "hermes"
            | "requiresopenaiauth"
            | "supportsthinking"
            | "supportseffort"
            | "store"
            | "stream"
    ) {
        return PreviewValuePolicy::PublicBoolean;
    }

    if matches!(
        normalized.as_str(),
        "timeout"
            | "autointerval"
            | "autoqueryinterval"
            | "ratelimitdelay"
            | "context"
            | "output"
            | "contextwindow"
            | "maxoutput"
            | "maxinput"
    ) {
        return PreviewValuePolicy::NonNegativeNumber;
    }

    PreviewValuePolicy::Opaque
}

fn is_env_header_reference_container(normalized: &str) -> bool {
    normalized == "envhttpheaders"
}

fn is_literal_header_container(normalized: &str) -> bool {
    !is_env_header_reference_container(normalized)
        && (normalized.ends_with("headers") || normalized.ends_with("headeroverrides"))
}

fn is_literal_env_container(normalized: &str) -> bool {
    normalized == "env"
}

fn is_sensitive_display_key(normalized: &str) -> bool {
    let auth_like = normalized.contains("auth")
        && !normalized.contains("author")
        && !normalized.contains("authority");

    auth_like
        || matches!(normalized, "key" | "keys" | "pat" | "pats" | "pin" | "pins")
        || normalized.contains("apikey")
        || normalized.contains("accesskey")
        || normalized.contains("token")
        || normalized.contains("password")
        || normalized.contains("passphrase")
        || normalized.contains("privatekey")
        || normalized.contains("secret")
        || normalized.contains("credential")
        || normalized.contains("cookie")
        || normalized.contains("bearer")
}

fn is_token_metric_key(normalized: &str) -> bool {
    matches!(
        normalized,
        "maxtokens"
            | "mintokens"
            | "maxinputtokens"
            | "maxoutputtokens"
            | "inputtokens"
            | "outputtokens"
            | "totaltokens"
            | "prompttokens"
            | "completiontokens"
            | "reasoningtokens"
            | "cachedtokens"
            | "cachedinputtokens"
            | "cachecreationinputtokens"
            | "tokenlimit"
            | "inputtokenlimit"
            | "outputtokenlimit"
    )
}

fn is_safe_url_key(normalized: &str) -> bool {
    normalized.ends_with("url")
        || normalized.ends_with("uri")
        || matches!(normalized, "homepage" | "docs")
}

fn is_safe_public_text(text: &str) -> bool {
    let mut count = 0usize;
    for ch in text.chars().take(513) {
        if ch.is_control() {
            return false;
        }
        count += 1;
    }
    count <= 512
}

pub(super) fn safe_public_text_for_display(text: &str) -> String {
    if is_safe_public_text(text) {
        text.to_string()
    } else {
        redacted_secret_placeholder().to_string()
    }
}

pub(super) fn safe_url_origin_for_display(raw: &str) -> String {
    const MAX_SAFE_URL_INPUT_BYTES: usize = 8 * 1024;
    if raw.len() > MAX_SAFE_URL_INPUT_BYTES {
        return redacted_secret_placeholder().to_string();
    }
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return String::new();
    }

    let Ok(parsed) = Url::parse(trimmed) else {
        return redacted_secret_placeholder().to_string();
    };
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        return redacted_secret_placeholder().to_string();
    }

    let origin = parsed.origin().ascii_serialization();
    if origin == "null" {
        redacted_secret_placeholder().to_string()
    } else {
        origin
    }
}

pub(super) fn safe_executable_basename_for_display(raw: &str) -> String {
    const MAX_SAFE_COMMAND_INPUT_BYTES: usize = 8 * 1024;
    if raw.len() > MAX_SAFE_COMMAND_INPUT_BYTES {
        return redacted_secret_placeholder().to_string();
    }
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return String::new();
    }

    const SHELL_META: &str = ";&|`$<>(){}[]*?!%=\"'";
    if trimmed.chars().any(|ch| {
        ch.is_whitespace() || ch.is_control() || SHELL_META.chars().any(|meta| meta == ch)
    }) {
        return redacted_secret_placeholder().to_string();
    }

    let basename = trimmed
        .rsplit(['/', '\\'])
        .next()
        .filter(|name| !name.is_empty() && name.chars().count() <= 128);
    basename.map_or_else(|| redacted_secret_placeholder().to_string(), str::to_string)
}

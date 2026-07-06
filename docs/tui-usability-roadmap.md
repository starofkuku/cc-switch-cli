# TUI Usability Roadmap

This note tracks the ongoing TUI usability overhaul: what has landed, and the
remaining work in priority order. Update it as items complete.

## Landed (2026-07)

For context — the foundations the remaining items build on:

- **Keymap registry** (`src/cli/tui/keymap.rs`): one binding table per page
  drives both key dispatch and the page key bar, so hints can never drift
  from handlers. Migrated: Providers, MCP, Prompts, Skills (installed),
  Usage, Sessions. Sessions binds only its action keys (Enter/R/d/r/a);
  pane/list navigation stays explicit in the handler (pane-dependent and
  reused by the filter path), with a static nav-hint prefix on the bar.
- **Overlay frame** (`src/cli/tui/ui/overlay/frame.rs`): all ~24 dialogs
  render through `overlay_frame`/`overlay_frame_at` with unified body
  padding; fixed-count pickers size to their options (`FitRows`).
- **Page frame** (`ui/shared.rs::render_page_frame`): shared page shell
  (bordered padded title + always-visible key bar + summary bar) for the
  five main list screens.
- **Theme system** (`src/cli/tui/theme.rs`): dark/light palettes with
  semantic colors (`fg_strong`, `on_accent`, `on_comment`), Settings ›
  Theme (Auto/Dark/Light, persisted), COLORFGBG auto-detection, curated
  ansi256 pins for both palettes.
- Word-wrapped, message-adaptive dialogs; breadcrumb titles on sub-pages;
  empty-state guidance on empty lists; `? more` degradation for
  overflowing key bars; help sheet synced with actual bindings.

## Remaining work

### 1. Finish the mechanical migrations (low risk, mostly delegatable)

- **config.rs sub-pages → `render_page_frame`**: **done** for Config,
  WebDAV, Settings, and Managed Accounts (the clean 1:1 fits — the last
  uses the frame's `Some(summary)` path then splits the body into two
  columns). Added `shared::breadcrumb_path` (unpadded) for frame callers,
  since the frame wraps the title itself. Still hand-rolled — and
  deliberately skipped because their layouts don't match the frame:
  - **Settings › Proxy** (`render_settings_proxy`): trailing 2-line
    footer (`[1, Min, 2]`) and a *conditional* key bar.
  - **Hermes Memory** (`render_hermes_memory`): a custom info-row
    paragraph (`[1, 2, Min]`), not a summary bar.
  - **OpenClaw** Env/Tools/Agents routes and Workspace/Daily Memory: a
    section-scroll layout, not a table body.
  These need `render_page_frame` variants (footer slot / info-row slot)
  before they can migrate; not maintenance-only.

### 2. Help sheet generated from the keymap registry (now unblocked)

`texts::tui_help_text*` page-key lines are still hand-written prose. With
Sessions migrated, the per-page lines can now be generated from
`keymap::<page>::BINDINGS` (display + label, skipping `shown == never`
aliases) so dispatch, key bars, and help share one source of truth. The
static text should keep the global-keys and text-editing sections.

Not as trivial as it looks — scoping notes for whoever picks it up:
- The label/`shown` fns take `(&App, &UiData)`, but the help entry point
  `help::context_help_for_app(app)` has no `UiData`; thread `data` in
  from `App::open_help` (which does have it).
- A help *catalog* must ignore runtime `shown` state and only drop the
  `shown == never` aliases (e.g. Usage's Shift+Tab). Add a named sentinel
  `never(_, _) -> bool` in `keymap` and skip by fn-pointer identity, or
  add an explicit per-binding "in help" flag — inline `|_, _| false`
  closures can't be compared.
- `tui_help_text*` in `i18n.rs` returns `&'static str`; generating means
  returning `String` and splitting the static global/text-input sections
  from the per-page bullets.
- Config/Settings/Hermes-Memory pages have no keymap module, so those
  bullets stay static. The Hermes help variant differs (provider label,
  extra Memory line) — the `providers` label fn already varies by
  app_type, so a generated line should reproduce it for free.

### 3. Terminal compatibility: icon fallback (issue #314 class)

Nav/emoji glyphs (🏠 🔑 …) render double-width on some SSH/legacy
terminals and break border alignment. Plan:

- `CC_SWITCH_ICONS=ascii|emoji|auto` env override plus a Settings row;
- `auto`: fall back to ASCII markers when the locale is not UTF-8
  (`LC_ALL`/`LC_CTYPE`/`LANG` without `utf-8`), mirroring the
  color-mode philosophy — add per-terminal cases, never flip defaults
  (see the pinned tests in `theme.rs`).

### 4. Provider form decomposition (largest remaining UX item)

The add/edit form spans ~60 fields across six apps in one scrolling
table. Plan:

- **Add flow**: show only the essentials (Name / Base URL / API Key +
  template row); collapse the rest behind an "Advanced" section header
  (the current divider rows become collapsible groups).
- **JSON preview on demand** (e.g. `F3`) instead of a permanent 45%
  column, returning width to the fields table.
- Sub-pages already show breadcrumbs; also surface a toast when `Ctrl+S`
  is ignored on a sub-page (`form_handlers/mod.rs` refuses silently).

### 5. Command palette (optional, largest discoverability win)

`Ctrl+P` (or `:`) fuzzy palette over the 24 routes plus per-page intents.
The `Route` enum and keymap intent tables make the candidate list nearly
free; the work is the overlay UX and dispatch plumbing.

### 6. Key vocabulary leftovers

- Case-pair traps kept for now: Sessions `R` (restore) vs `r` (refresh),
  Skill detail `s` (sync) vs `S` (sync all). If they cause real
  mis-presses, prefer confirm dialogs over rebinding.
- Usage `P` (pricing) vs Main `p` (proxy) cross-screen overload:
  tolerated because both are chip-labeled.

### 7. Upstream housekeeping (not TUI, found along the way)

- **done** — `.gitignore` `skills/` anchored to `/skills/` so it no
  longer swallows `src/cli/tui/ui/skills/`.
- **done** — deleted `src/cli/i18n/texts/` (the uncompiled divergent copy
  of the inline `texts` module in `i18n.rs`).

## Conventions for new work

- New dialogs: describe size/title/keys/body via `overlay_frame` — do
  not hand-roll chrome. Fixed-option pickers use `FitRows`.
- New pages: `render_page_frame` + a `keymap` module binding table.
- New palette colors: add both dark and light RGBs plus a curated
  ansi256 pin test in `theme.rs`.
- Every key visible in a bar must resolve through the same table the
  handler reads.

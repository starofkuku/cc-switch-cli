# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Commands

The Rust crate lives in `src-tauri/`. Run Cargo commands from that directory unless a command explicitly targets repository-root assets or scripts.

```bash
cd src-tauri

cargo run                                  # Run cc-switch in interactive mode
cargo run -- provider list                 # Run a specific CLI command
cargo run -- --app codex provider list     # Run a command for a specific app
cargo run -- proxy show                    # Inspect proxy state
cargo run -- env tools                     # Check local CLI tools
cargo build --release                      # Build release binary at target/release/cc-switch

cargo fmt                                  # Format Rust code
cargo fmt --check                          # Check formatting, matching CI
cargo clippy                               # Run lints
cargo test                                 # Run all tests
cargo test --lib                           # Library unit tests only
cargo test --bin cc-switch                 # Binary unit tests only
cargo test provider_switch                 # Run tests whose names contain provider_switch
cargo test --test provider_commands        # Run a single integration test target
cargo test --test proxy_daemon proxy_enable_and_disable_cli_manage_daemon_worker -- --exact
cargo test --features test-hooks           # Run tests with the test-hooks feature enabled
```

`Cargo.toml` currently declares two features, both off by default: `default = []` and `test-hooks = []`.

The repository pins Rust through `src-tauri/rust-toolchain.toml` to Rust 1.91.1 with `rustfmt` and `clippy`.

CI (`.github/workflows/rust-ci.yml`) runs `cargo fmt --check`, `cargo test --lib`, `cargo test --bin cc-switch`, and three selected integration tests (`proxy_claude_forwarder_alignment`, `proxy_daemon`, `proxy_database`). Every job exports a sandboxed environment before running tests:

```bash
sandbox_home="$(mktemp -d)"
export HOME="$sandbox_home" USERPROFILE="$sandbox_home"
export CC_SWITCH_CONFIG_DIR="$sandbox_home/.cc-switch"
export CLAUDE_CONFIG_DIR="$sandbox_home/.claude"
export CODEX_HOME="$sandbox_home/.codex"
export XDG_CONFIG_HOME="$sandbox_home/.config"
export XDG_RUNTIME_DIR="$sandbox_home/.runtime"
export XDG_STATE_HOME="$sandbox_home/.state"
export RUST_TEST_THREADS=1
```

`.github/workflows/benchmark.yml` builds the release binary and runs `scripts/benchmark_cc_switch.py` plus `scripts/check_benchmark_thresholds.py`, which fails the build when median/p95 latency for named CLI/TUI operations exceeds hardcoded thresholds. Release (`release.yml`) is triggered by pushing a `v*` tag and is gated on that same benchmark job.

## Project overview

CC-Switch CLI (v5.10.x) is a Rust TUI + CLI manager for Claude Code, Codex, Gemini, OpenCode, Hermes, OpenClaw, Pi, and Grok. It manages provider configurations, MCP servers, prompts, skills, WebDAV/S3 sync, local proxy routes, failover, daemon/start flows, deep-link imports, sessions, workspace memory files, and environment checks. It is a CLI fork of the upstream `farion1231/cc-switch` GUI project; WebDAV sync stays wire-compatible with upstream.

The main crate is `src-tauri/`; the repository root contains docs, assets, install/update scripts, packaging metadata, and Nix files.

Key Rust entry points:

- `src/main.rs` parses CLI arguments, initializes logging, creates startup state for most commands, and dispatches to command handlers.
- `src/lib.rs` declares crate modules and re-exports public types used by integration tests and command code. Note that `mod app_config`, `config`, `database`, `proxy`, `session_manager`, etc. are private modules; integration tests reach them through the `pub use` list at the bottom of `lib.rs` (and `tests/support.rs`), so a type that tests need must be re-exported there.
- `src/cli/mod.rs` defines the top-level Clap CLI, global `--app` flag, and command enum.
- `src/cli/commands/` contains direct command implementations: apps, auth, provider (incl. add wizard/clone/inspect/usage-query), mcp, prompts, skills, config (incl. WebDAV/S3), proxy, settings, failover, sessions, hermes, start, daemon, env, deeplink, update, completions, and internal.
- `src/commands/` contains library command helpers that are not top-level Clap subcommands, including OpenClaw workspace file and daily memory operations.
- `src/cli/interactive/` and `src/cli/tui/` contain the interactive ratatui UI, runtime action handlers, forms, overlays, route state, and UI rendering.
- `src/services/` contains durable business logic used by commands and the TUI: providers, auth, MCP, prompts, skills, proxy, WebDAV/S3 sync, stream checks, speed tests, environment checks, session usage, visible apps, subscription/coding-plan quota checks, and state coordination.

`PromptService::copy_live_prompt` (`prompts copy <from> <to> [--force]`) is the one prompt path that deliberately bypasses the preset database: it copies the live global file (`CLAUDE.md`/`AGENTS.md`/`GEMINI.md`) from one app to another without creating or activating a preset. It must keep routing its destination write through `sync_policy::should_sync_live` so an uninitialized app is never given a new config directory.
- `src/session_manager/` scans and queries saved assistant sessions across app providers (manifest/cache/project scope) and backs `sessions list|show|messages|export`.
- `src/database/` is the SQLite persistence layer. `Database` owns a mutex-wrapped rusqlite connection, schema creation/migration, backups, and DAO modules for providers, MCP, prompts, skills, settings, proxy state, stream checks, universal providers, and failover queues.
- `src/app_config.rs`, `src/provider.rs`, and app-specific config modules (`claude_*`, `codex_config.rs`, `codex_state_db.rs`, `gemini_*`, `hermes_config.rs`, `opencode_config.rs`, `openclaw_config.rs`, `pi_config.rs`, `grok_config.rs`) define the shared configuration model and live-file adapters for supported apps.
- `src/deeplink/` implements the `ccswitch://v1/import?...` import protocol for provider/MCP/prompt/skill resources and is exported through `lib.rs` for tests and callers.
- `src/proxy/` implements the local multi-app proxy with Axum handlers, request forwarding, provider routing/failover, provider-specific transformations, response/stream handling, usage logging, model mapping, cache/thinking rectifiers, circuit breaking, and metrics.
- `src/daemon/` implements the Unix supervisor daemon, IPC protocol, logging, pidfile, and restart support.
- `src/store.rs` defines `AppState`, which ties together the database, an in-memory `MultiAppConfig` snapshot, startup live-config imports/recovery, and `ProxyService`.

## Supported apps and their differences

`AppType` lives in `src/app_config.rs` and is the single source of truth for app behavior. Its variants (`claude`, `codex`, `gemini`, `opencode`, `hermes`, `openclaw`, `pi`, `grok`) carry three behavioral flags that decide how most command paths branch:

- `is_additive_mode()` — true for OpenCode, Hermes, OpenClaw, Pi, Grok. These apps accumulate providers in live config instead of keeping one "current" provider, so provider workflows must not assume a switch replaces the previous entry.
- `supports_failover()` — true for Claude, Codex, Gemini only. The proxy and `failover` command surface only these apps.
- `should_sync_live()` in `src/sync_policy.rs` — per-app "is this app initialized?" predicate. When false, live config writes/deletes are skipped and no directories are created. Any new live-config writer must route through this policy rather than assuming the app is installed.

`AppType::from_str` also accepts `grokbuild`/`grok-build`/`grok_build` as aliases for `grok`, because that is the name used by the upstream Windows desktop schema.

Note the two spellings in play: the `--app` Clap flag is a `ValueEnum`, so its labels are hyphenated (`open-code`, `open-claw`), while `AppType::from_str` (used for stored values and the CSV from `apps list`) expects the un-hyphenated `opencode`/`openclaw`. Commands that take a provider id from the user (e.g. `sessions --provider`) normalize by stripping hyphens first, but code that parses config values must use the canonical spelling. Run `apps list` for the authoritative `--app` labels.

## State and configuration model

CC-Switch stores core state in SQLite at `~/.cc-switch/cc-switch.db` by default, or under `$CC_SWITCH_CONFIG_DIR/cc-switch.db` when `CC_SWITCH_CONFIG_DIR` is set. `~/.cc-switch/settings.json` stores app settings, `~/.cc-switch/skills/` stores installed skill source files, and `~/.cc-switch/backups/` holds rotating backups.

`src/database/mod.rs` defines `SCHEMA_VERSION` (currently 16). `Database` refuses to open a database whose version is newer than the binary and creates a pre-migration backup before upgrading an older one — which is why commands like `update`, `completions`, `internal`, and Unix `daemon` deliberately bypass startup state and can still run against a future-schema database.

Legacy `config.json` and `skills.json` are migration/import sources only. `AppState::try_new()` validates and migrates legacy files into SQLite when needed, exports database state into a `MultiAppConfig` snapshot, seeds defaults, migrates old common-config semantics, and constructs `ProxyService`. `AppState::try_new_with_startup_recovery()` also imports live provider configs and recovers proxy takeovers when needed. Interactive mode uses `try_new_with_startup_recovery_deferred()` so Codex history migration can run without blocking the TUI. `AppState::save()` persists the in-memory snapshot back to SQLite.

Live config files are separate from CC-Switch storage and are only synced or imported for initialized apps:

- Claude: `~/.claude/settings.json`, `~/.claude.json`, `~/.claude/CLAUDE.md`
- Codex: `~/.codex/auth.json`, `~/.codex/config.toml`, `~/.codex/AGENTS.md`
- Gemini: `~/.gemini/.env`, `~/.gemini/settings.json`, `~/.gemini/GEMINI.md`
- OpenCode: `~/.config/opencode/opencode.json`, `~/.config/opencode/AGENTS.md`
- Hermes: Hermes config directory from settings or default app location, with app-specific provider/prompt/MCP handling
- OpenClaw: `~/.openclaw/openclaw.json`, `~/.openclaw/AGENTS.md`
- Pi: `~/.pi/agent/models.json`, `~/.pi/agent/settings.json`, `~/.pi/agent/AGENTS.md` (or the directory selected by `PI_CODING_AGENT_DIR`)
- Grok: `$GROK_HOME/config.toml`, default `~/.grok/config.toml`. Editing goes through `toml_edit` so unrelated sections and comments survive; each provider maps to one `[model.<id>]` table and switching sets `[models].default`.

Environment overrides matter when testing or running commands: `CC_SWITCH_CONFIG_DIR` controls CC-Switch storage, `CLAUDE_CONFIG_DIR` controls Claude config directory, `CODEX_HOME` controls Codex config, `GROK_HOME` controls Grok config, and `PI_CODING_AGENT_DIR` controls Pi. Tests also commonly set `HOME`, `XDG_CONFIG_HOME`, `XDG_RUNTIME_DIR`, and `XDG_STATE_HOME`.

## CLI architecture

Adding or changing a user-facing command usually requires updates in three layers:

1. Define the Clap shape in `src/cli/mod.rs` or the relevant `src/cli/commands/*.rs` file.
2. Implement command I/O and prompts in `src/cli/commands/`, keeping durable logic in `src/services/` when behavior is shared with the TUI or other commands.
3. Add or update tests under `src-tauri/tests/` or module-local `#[cfg(test)]` tests.

The global `--app` flag selects an `AppType` (Clap `ValueEnum` labels, e.g. `open-code`/`open-claw`); Claude is the default. Several commands override that default deliberately: `sessions export` requires an explicit `--app` (there is no default app for export), and `sessions list|show|messages` accept `--provider` and `--all` to scan one or every app.

Top-level commands (see `src/cli/mod.rs`): `apps`, `auth`, `provider`, `use` (shortcut for provider switch), `mcp`, `prompts`, `skills`, `config`, `proxy`, `settings`, `failover`, `sessions`, `hermes`, Unix-only `start`/`daemon`, `env`, `deeplink`, `update`, `interactive` (alias `ui`), `completions`, and hidden `internal`.

Commands that normally create startup state call `AppState::try_new_with_startup_recovery()` before dispatch. `update`, `completions`, `internal`, and Unix `daemon` intentionally bypass normal startup state so they can run even when the user database has a future schema version or daemon-specific logging needs apply. When commands run under the daemon socket environment, startup state is also skipped so the daemon-owned process can coordinate state. `update` and `completions` also skip database access checks.

OpenClaw workspace helpers live under `src/commands/workspace.rs`, not the Clap command tree. They restrict file access to the OpenClaw workspace allowlist (`AGENTS.md`, `SOUL.md`, `USER.md`, `IDENTITY.md`, `TOOLS.md`, `MEMORY.md`, `HEARTBEAT.md`, `BOOTSTRAP.md`, `BOOT.md`) and daily memory files, and deliberately reject symlinks/path traversal.

## Localization

All user-facing text is bilingual and lives in `src/cli/i18n.rs`, a very large module of `texts::*` accessors returning the string for the current `Language` (English/Chinese). `AppError::localized(key, zh, en)` carries both strings so errors render in the active language. Language is global mutable state initialized from `settings.json`; under `cfg(test)` it is forced to English so unit tests are deterministic. When adding user-facing strings, add them to `i18n.rs` rather than inlining literals, and keep the `zh`/`en` pair of any `AppError::localized` call in sync.

## TUI interaction guidance

- Keep primary TUI surfaces focused on fields, current values/status, and available actions.
- Put feature explanations, behavioral caveats, validation rules, and other long-form hints in the contextual `?` help for the focused control.
- Do not add persistent instruction or description panels when the same information can live in `?` help.
- Page key bindings are declared once in `src/cli/tui/keymap.rs`; the key handler resolves keys through `intent_for` and the on-screen key bar is generated from the same table. Add a binding to that table instead of hardcoding a key check or a hint string, otherwise the hint and the handler drift.

## Proxy architecture

The proxy command surface is in `src/cli/commands/proxy.rs`, orchestration lives in `src/services/proxy.rs`, and the HTTP server is in `src/proxy/server.rs` and `src/proxy/handlers.rs`.

Request handling flows through `HandlerContext`, `ProviderRouter`, `RequestForwarder`, provider adapters in `src/proxy/providers/`, and response builders/handlers in `src/proxy/response*.rs`. Claude `/v1/messages` traffic may be transformed between Anthropic and OpenAI-compatible formats; Codex/OpenAI, Gemini, Copilot, and streaming-response routes are handled by provider-specific adapters. Proxy tests are split across focused integration targets such as `proxy_claude_streaming`, `proxy_claude_openai_chat`, `proxy_claude_response_parity`, `proxy_claude_forwarder_alignment`, `proxy_multi_app_passthrough`, `proxy_takeover`, `proxy_service`, and `proxy_daemon`.

## Testing requirements

All test cases should be executed under `src-tauri/`.

When adding integration tests that touch HOME, app config directories, or live config files, isolate filesystem state with helpers in `src-tauri/tests/support.rs`. Use `ensure_test_home()`, `reset_test_fs()`, and `lock_test_mutex()` patterns rather than writing to real user directories. Unit tests inside the crate can also use `src/test_support.rs` helpers for test home/settings isolation. CI sets `RUST_TEST_THREADS=1` and sandboxed env vars for unit/integration runs.

Note that app configuration modules cache paths and hold global write locks (e.g. `grok_config.rs` uses a `Mutex`/`OnceLock` per module), so tests that change env overrides must serialize through `lock_test_mutex()` rather than running concurrently.

### IMPORTANT

- **NEVER** change the host configuration in `$CC_SWITCH_CONFIG_DIR/`.
- **NEVER** change the host configuration in `$CLAUDE_CONFIG_DIR/`.
- **NEVER** change the host configuration in `$CODEX_HOME/`.
- **NEVER** change the host configuration in `$PI_CODING_AGENT_DIR/`.
- **NEVER** change the host configuration in `$GROK_HOME/`.
- Create a sandbox before executing test cases or commands that write app configuration.
- Prefer temporary directories and explicit environment overrides for tests that exercise live config sync/import paths.

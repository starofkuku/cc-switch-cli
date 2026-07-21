# Sessions Export

Export one local assistant session to a shareable JSON file (`ccswitch-session` v1).

Supported apps: `claude`, `codex`, `gemini`, `opencode`, `openclaw`, `hermes`, `grok`, `pi`.

## Requirements

- Global `--app` is **required** (no default app for export).
- Export always targets **one** session.

## Commands

### Interactive export (two-step picker)

```bash
cc-switch --app grok sessions export
cc-switch --app claude sessions export -o ./share.json
```

1. Scans sessions for the given app.
2. **Step 1 — work directory**: groups sessions by `projectDir` / cwd (sessions without a workdir are hidden). Shows only the **last path component** (e.g. `cc-switch-cli`). If the process cwd matches a group, that row is pre-selected and marked `(current)`.
3. **Step 2 — session**: lists sessions under the chosen workdir (newest first).
4. After you press `Enter` on a session, writes JSON to disk.

**Workdir picker keys**

| Key | Action |
|-----|--------|
| `↑` / `↓` or `j` / `k` | Move selection |
| `Enter` | Open this workdir’s sessions |
| `Esc` | Cancel |

**Session picker keys**

| Key | Action |
|-----|--------|
| `↑` / `↓` or `j` / `k` | Move selection; when preview is open, scroll the transcript |
| `Enter` | Export the highlighted session |
| `Ctrl+E` | Expand / collapse user + assistant preview (user lines in green) |
| `Esc` | Collapse preview, or cancel if already collapsed |
| `PageUp` / `PageDown` | Jump in the list or preview |

Session list labels:

- Prefer a **session name/title** when the agent provides one.
- Otherwise show the **last user message** (first 10 characters).

### Export by session id (non-interactive)

```bash
# Full id
cc-switch --app grok sessions export --id 019f8253-b95c-7891-aee3-3af7e28cb122

# Unique prefix (same rules as `sessions show`)
cc-switch --app grok sessions export --id 019f8253

# Custom output path
cc-switch --app claude sessions export --id <session-id> -o /tmp/out.json
```

Resolution rules:

1. Prefer **exact** `sessionId` match within that app.
2. Else match a **unique prefix**.
3. Zero matches → error `Session '…' was not found.`
4. Multiple prefix matches → error listing candidates (ambiguous).

`--id` skips the interactive pickers (both workdir and session). You still must pass `--app`. Matching runs across **all** sessions of that app (not limited to the current workdir).

## Output path

| Flag | Behavior |
|------|----------|
| (default) | `./ccswitch-<app>-<id8>-<YYYYMMDD>.json` in the current working directory |
| `-o` / `--output <path>` | Write to the given file (parent dirs are created if needed) |

## Output JSON shape

```json
{
  "format": "ccswitch-session",
  "version": 1,
  "exportedAt": "2026-07-21T…",
  "app": "grok",
  "sessionId": "019f8253-…",
  "title": "…",
  "projectDir": "/path/to/project",
  "sourcePath": "/path/to/native/session",
  "createdAt": 0,
  "lastActiveAt": 0,
  "messages": [
    { "role": "user", "content": "…", "ts": null },
    { "role": "assistant", "content": "…", "ts": null }
  ]
}
```

Notes:

- Only **user** and **assistant** text are included (tools / system / reasoning are dropped).
- `sourcePath` is a local absolute path; strip it before sharing if you care about privacy.
- Very long sessions may be truncated by the session reader; the CLI prints a warning when that happens.
- The file is a **normalized export**, not a native session archive. It is not a drop-in input for `claude --resume` / `grok --resume`.

## How session id mapping works

CC-Switch does **not** invent paths from the id string alone.

1. Scan that app’s known session locations.
2. Match `sessionId` (or unique prefix) against the scan index.
3. Read messages from the resolved `sourcePath`.

| App | Typical session root |
|-----|----------------------|
| Claude | `~/.claude/projects/**/*.jsonl` |
| Codex | `~/.codex/sessions/**/*.jsonl` |
| Gemini | `~/.gemini/tmp/**/chats/*.json` |
| OpenCode | `~/.local/share/opencode/` (JSON + SQLite) |
| OpenClaw | `~/.openclaw/agents/*/sessions/*.jsonl` |
| Hermes | `~/.hermes/sessions/` + `state.db` |
| Grok | `~/.grok/sessions/<cwd>/<id>/` (`GROK_HOME` supported) |
| Pi | `~/.pi/agent/sessions/` (`PI_CODING_AGENT_DIR` supported) |

## Examples

```bash
# List then export by id
cc-switch --app grok sessions list
cc-switch --app grok sessions export --id 019f8253-b95c-7891-aee3-3af7e28cb122 -o ./grok-session.json

# Inspect the result
jq '.app, .sessionId, (.messages|length), .messages[0]' ./grok-session.json
```

## Related commands

```bash
cc-switch --app grok sessions list
cc-switch --app grok sessions show <id>
cc-switch --app grok sessions messages <id>
cc-switch --app grok sessions resume <id>
```

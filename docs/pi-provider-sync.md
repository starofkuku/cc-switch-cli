# Pi Provider Sync & Model Catalog Enrichment

How cc-switch keeps Pi providers in sync with `~/.pi/agent/models.json`, and how
model ids are resolved against the [models.dev](https://models.dev) catalog.

Pi is the only app that currently supports these commands; every other app
rejects the flags instead of silently ignoring them.

## Why Pi is special

Most apps keep one "current" provider and store everything in cc-switch's
SQLite database. Pi instead accumulates providers in a live file:

```
~/.pi/agent/models.json
{
  "providers": {
    "<provider-id>": { "name": ..., "api": ..., "apiKey": ..., "baseUrl": ..., "models": [...] }
  }
}
```

That makes the live file meaningful on its own, so cc-switch offers syncs in
**both** directions rather than picking one as authoritative.

## Provider sync

```bash
# live -> DB: make cc-switch match models.json
cc-switch --app pi provider import-live                  # add new providers only
cc-switch --app pi provider import-live --update         # also overwrite existing ones
cc-switch --app pi provider import-live --update --prune # ...and drop DB rows absent from live

# DB -> live: make models.json match cc-switch
cc-switch --app pi provider export-live                  # write managed providers into live
cc-switch --app pi provider export-live --prune          # ...and drop live entries cc-switch does not define
```

| Flag | Effect |
|------|--------|
| *(none)* | Conservative. Only adds; never overwrites or deletes. |
| `--update` | `import-live` only: overwrite existing providers with live content. |
| `--prune` | Delete entries the other side does not define. |

### What survives a sync

- **Unmanaged entries are kept.** `export-live` starts from the current
  `models.json`, so a provider you hand-wrote and cc-switch never imported is
  left alone unless you pass `--prune`.
- **Unmanaged fields are kept.** Merging only overwrites keys cc-switch models;
  extra fields you added by hand inside a provider survive.
- **`--prune` never deletes db-only rows.** Only providers flagged
  `liveConfigManaged` (i.e. ones that came from live) can be pruned, so
  providers created purely inside cc-switch are safe.

## Adding models to one provider

```bash
cc-switch --app pi provider add-model <provider-id> --model <id> [--model <id>...]
cc-switch --app pi provider add-model <provider-id> --fetch
```

This changes **only** the provider's `models[]` array. `name`, `api`, `apiKey`,
`baseUrl` and every other field — including any you added by hand — are
untouched. No other provider is modified.

| Option | Effect |
|--------|--------|
| `--model <id>` | Add one id. Repeatable. |
| `--fetch` | Discover ids from the provider's own `/v1/models` endpoint. |

Behaviour:

- **Existing ids are skipped** and listed in the output.
- `--fetch` discovers ids, resolves them, and **skips** any that get no catalog
  match, so a discovery run cannot fill your config with placeholder entries.
- The database and `models.json` are both updated, so the next sync does not
  revert the change.

## Catalog matching

Model ids from a provider rarely match models.dev exactly. Before comparing,
cc-switch normalizes both sides:

- lowercases and trims
- strips vendor prefixes: `openai/`, `anthropic/`, `google/`, `meta/`,
  `mistral/`, `deepseek/`, `xai/`, `amazon-bedrock/`, `bedrock/`,
  and the dotted forms `openai.`, `anthropic.`, `google.`
- strips release markers: `:latest`, `-2026-01-15`, `-20260115`, `@2026-01-15`

So `openai/GPT-5.4-2026-01-15`, `gpt-5.4:latest` and `gpt-5.4` all resolve to the
same catalog entry. Meaningful suffixes are preserved — `gpt-5.4-mini` stays
`gpt-5.4-mini` and is not collapsed into `gpt-5.4`.

Candidates are then ranked by similarity: an exact match wins outright, followed
by shared-prefix ratio and containment, with the larger context window breaking
ties. A hit scores at least 800 to be applied automatically.

### When nothing matches

| Situation | Behaviour |
|-----------|-----------|
| Confident fuzzy match | Parameters filled automatically. |
| No match, interactive TTY | A searchable picker appears: the top 20 fuzzy candidates first, plus an option to search the entire models.dev catalog. You can also keep the id-only entry. |
| No match, no TTY (scripts, CI) | The entry is written as `{"id": "..."}` and reported, instead of blocking on a prompt nobody can answer. |

In `add-model --fetch` an unmatched id is skipped entirely rather than written
id-only.

### What gets filled

A matched entry receives the catalog values for:

| Field | Source |
|-------|--------|
| `name` | model display name |
| `contextWindow` | `limit.context` (falling back to `limit.input`) |
| `maxTokens` | `limit.output` |
| `reasoning` | `reasoning` |
| `toolCall` | `tool_call` |
| `attachment` | `attachment` |
| `input` | `modalities.input`, filtered to `text`/`image` only |

Pi's `models.json` schema only accepts `text` and `image` modalities, so
pdf/audio/video values from models.dev are dropped rather than written.

## Refreshing the catalog

The catalog is cached, and a cached copy is **never** refreshed automatically:

```bash
cc-switch provider catalog refresh
```

| Item | Value |
|------|-------|
| Source | `https://models.dev/api.json` |
| Cache | `$CC_SWITCH_CONFIG_DIR/cache/models.dev.json` (default `~/.cc-switch/cache/models.dev.json`) |
| Write | download → validate JSON → write `.tmp` → atomic rename |

Run this after models.dev adds or adjusts models you care about. The cache is
per-machine and is **not** part of WebDAV/S3 sync (which only transfers
`db.sql` and `skills.zip`).

## Notes and gotchas

- `~/.pi/agent` must already exist for any write to happen. When it does not,
  the command refuses and never creates the directory.
- Hosted IDs that differ beyond prefixes and dates (e.g. a relay prefix like
  `myrelay/gpt-5.4`, or an unlisted new model) still need the interactive picker
  or a manual `provider edit`.
- `provider fetch-models <id>` only prints ids; it never writes. Use
  `add-model --fetch` to actually add them.

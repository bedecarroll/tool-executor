## 1. Purpose

A single-binary Rust CLI (`tx`) for managing AI code-assistant sessions with a thin philosophy:

* Build a deterministic pipeline: `stdin -> PRE | PROVIDER <args> | POST`.
* Optionally execute that pipeline inside a WRAP template (`{{CMD}}`).
* Provide a fast TUI to search, preview, and launch or resume sessions.

## 2. User Stories

* **TUI first**: run `tx`; select a recent session or a profile; **Tab** inserts the command into the shell; **Enter** executes it.
* **Search**: `tx search foo` searches the first user prompt; `--full-text` searches transcript content via FTS.
* **Pipelines**: pre and post steps (for example prompt assembly or saving output) without bespoke DSLs.
* **Wrappers**: tmux, nohup, docker, time, sudo -- all as templates; tx does not manage them.
* **Drop-ins**: different machines can add or remove `.toml` files to alter behavior.

## 3. CLI Surface

```
# TUI
tx [--json --emit-command]    # --emit-command used by shell keybinding

# List/search
tx sessions [--provider P] [--since 7d] [--actionable] [--json]
tx search <term> [--full-text] [--json]

# Launch/resume
tx launch <provider> [--profile NAME] [--pre NAME...] [--post NAME...] [--wrap NAME] [--var K=V ...] [--] [prov-args...]
tx resume <session-id> [--profile NAME] [--pre ...] [--post ...] [--wrap NAME] [--var ...] [--] [prov-args...]

# Export
tx export <session-id> --md

# Config/diagnostics
tx config list|dump|where|lint
tx doctor

# Self-update (cargo feature: self-update)
tx self-update [--version TAG]
```

Exit codes: `0` ok; `1` invalid input or not found; `2` provider mismatch; `3` config error.

## 4. TUI Behavior

* List shows recent sessions (provider chip, label, first user prompt, `last_active`) and profiles (provider plus tags for pre, post, wrap).
* Preview pane shows first N messages, path, timestamps.
* Keys: `ArrowUp` or `ArrowDown`, `/`, `Tab`, `Enter`, `Ctrl-F`, `p`, `Alt-A`, `R`, `e`, `?`.
* Insertion: with `--emit-command` or pressing `Tab`, print final command to stdout (no newline) and exit 0.
* Provide minimal runtime prompts for missing `--var` required by chosen pre or wrap.

## 5. Pipeline Assembly (Core Logic)

Given user selection and config:

1. Build base: provider bin plus ordered default flags plus user flags.
2. Stdin handling:
   * If `pre` exists: `stdin -> PRE | BASE`.
   * Else: `stdin -> BASE`.
   * If provider does not read stdin, `stdin_to` can map stdin to a provider flag.
3. Append `post` if present: `... | POST`.
4. Apply wrap if present: substitute `{{CMD}}` with pipeline string and exec wrapper.

Merging rules:

* `--profile` loads `pre[]`, `post[]`, `wrap`, `provider`.
* CLI `--pre` and `--post` append to profile-defined lists.
* CLI `--wrap` overrides profile wrap.

## 6. Config Loading and Merging

Locations and order:

1. `~/.config/tx/config.toml` (optional)
2. `~/.config/tx/conf.d/*.toml` (lexicographic)
3. `./.tx.toml` (optional)
4. `./.tx.d/*.toml` (lexicographic)

Rules:

* Deep-merge tables; scalars overwrite.
* Arrays replace unless `key+` is used to append.
* Set a key to `null` to delete it.
* Env expansion form: `${env:NAME}`.
* Commands can be strings (`shell=true`) or argv arrays.

## 7. Providers

Each provider entry supplies:

* `bin`, `flags[]`, `env[]`, `session_roots[]`, optional `stdin_to`.
* Resume safety: `resume <sid>` reads provider from DB; attempting to override fails with exit 2.
* Adapters implement minimal discovery of sessions under `session_roots` and normalization into SQLite.

## 8. Prompt-assembler Integration (Optional)

* Feature gate: `[features.pa] enabled=true`.
* Strategy: exec-only -- call `pa list --json`.
* Convert prompts to virtual profiles `pa/<name>` that set `pre=["assemble"]`.
* If `pa` is missing or returns non-zero or invalid JSON, hide PA items and show a small notice.
* Cache results in memory; refresh on `R` or after `cache_ttl_ms`.

## 9. Session Indexing and Database

On each `tx` invocation (and on `R`):

* Walk `session_roots` per provider; detect new or changed entries via `(path, size, mtime)`; parse minimal header.
* Upsert into SQLite in a transaction.

Schema:

```sql
CREATE TABLE sessions(
  id TEXT PRIMARY KEY,
  provider TEXT NOT NULL,
  label TEXT,
  path TEXT NOT NULL,
  first_prompt TEXT,
  actionable INTEGER NOT NULL DEFAULT 1,
  created_at INTEGER,
  last_active INTEGER
);

CREATE TABLE messages(
  session_id TEXT NOT NULL,
  idx INTEGER NOT NULL,
  role TEXT NOT NULL,
  content TEXT NOT NULL,
  PRIMARY KEY (session_id, idx)
);

CREATE VIRTUAL TABLE messages_fts USING fts5(
  session_id UNINDEXED,
  role UNINDEXED,
  content,
  content=''
);
```

* First-prompt search queries `sessions.first_prompt`.
* Full-text search joins `messages_fts` to `sessions`.

## 10. Export

* `tx export <sid> --md` prints a Markdown transcript using stored messages and includes session header (provider, timestamps, label).

## 11. Security and Quoting

* For `shell=true` wrappers, `{{CMD}}` is single-quoted with embedded quotes escaped.
* No evaluation of transcript contents.
* Env expansion only for `${env:VAR}` in config; never evaluate arbitrary strings.

## 12. Diagnostics

* `tx doctor`: verify binaries on PATH, env keys present, session roots exist, DB reachable; optional `pa validate` if enabled.
* `tx config lint`: schema validation; show file and key for errors.

## 13. Performance Targets

* Cold ingest of about 10k files: < 200 ms; warm: < 50 ms (skip unchanged by mtime or size).
* FTS insert batched; vacuum task exposed via `tx vacuum` (optional).

## 14. Testing Matrix

* Config merge (tables, arrays, append `+`, delete via `null`).
* Provider safety on resume.
* TUI: filter, full-text toggle, insert vs run, vars prompting.
* Wrappers: `shell=true` or `false` quoting; `--dry-run` output.
* Prompt assembler integration: success, missing binary, invalid JSON.
* DB ingest idempotence and search correctness.

## 15. Non-Goals

* Managing tmux panes or sessions beyond executing a user-provided wrapper.
* Editing provider logs.
* Implementing prompt selection logic beyond `pa list --json`.

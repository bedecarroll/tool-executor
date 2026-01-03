# Architecture

This document describes the high-level architecture of tool-executor (tx). It mirrors the checklist in rust-analyzer's architecture notes: Bird's Eye View, entry points, code map, architecture invariants, and API boundaries.

## Bird's Eye View

tx accepts three kinds of inputs and produces two kinds of outputs:

Inputs (ground state):

- CLI arguments and flags (profile selection, emit mode, etc.).
- TOML configuration files (providers, profiles, snippets, wrappers).
- Session transcripts on disk (JSONL files under provider session roots).

Derived state and outputs:

- A SQLite index of sessions and messages used by search/resume/export.
- A pipeline plan that is either executed or emitted as JSON/command output.

High-level flow (most paths converge on this):

```text
CLI -> App::bootstrap -> Config + DB + Indexer
                     -> Command handler (search/resume/export/config/doctor/self-update)
                     -> PipelinePlan (optional) -> Execute or emit
```

Request lifecycle (happy path):

1. Parse CLI args into a `Cli` struct.
2. Load config + open the database (and reindex unless `TX_SKIP_INDEX` is set). Internal commands skip this bootstrap.
3. Build a `PipelinePlan` if the command needs one (resume, emit modes, or when the TUI selects a row).
4. Execute the plan or emit it as JSON/command output.

## Entry points

- `crates/tx/src/main.rs`: binary entry point, installs `color-eyre`, then calls `tool_executor::run`.
- `src/lib.rs`: library entry point, wires CLI parsing, tracing, and dispatch to `App`.
- `src/app.rs`: runtime coordinator; owns loaded config, DB connection, and prompt assembler handle.

## Code map

Pay attention to the **Architecture Invariant** and **API Boundary** notes; they describe intentional design limits and intentionally missing functionality. Remember, rules at the boundary are different.

### `crates/tx`

Binary crate for the `tx` CLI.

**API Boundary:** the CLI surface and its emitted JSON/command output.

### `src/lib.rs`

Public library entry point for command dispatch.

**API Boundary:** the public `tool_executor` API consumed by the CLI (and potential external callers).

### `src/app.rs`

Orchestration and command dispatch, plus pipeline execution.

**Architecture Invariant:** `App` executes user pipelines for the main CLI/TUI flows and owns runtime IO orchestration. Internal helper commands may spawn subprocesses for provider execution or metadata (for example, prompt-assembler).

### `src/cli.rs`

Clap definitions for commands and flags.

**Architecture Guideline:** CLI parsing should not perform IO or DB access.

### `src/internal.rs`

Internal subcommands (`tx internal ...`) that bypass full app bootstrap.

**Architecture Invariant:** internal helpers may execute provider binaries or `pa`, but they do not touch the database or configuration merging.

### `src/config/`

Configuration loading, merging, schema validation, and diagnostics.

**Architecture Invariant:** `config::load` is the only supported entry point for configuration. It merges:

1. `config.toml` under the config directory.
2. `conf.d/*.toml` (lexical order).
3. Optional project overrides: `.tx.toml` and `.tx.d/` in the working directory.

**Architecture Invariant:** configuration loading does not execute external commands or touch the database. Filesystem IO is expected (creating directories, writing default templates).

### `src/db/`

SQLite schema, migrations, and queries (including FTS).

**Architecture Invariant:** the database is the authoritative source for search/resume/export.
Data model summary: `sessions` (one row per transcript), `messages` (ordered records per session),
and `messages_fts` (full-text search index).

### `src/indexer.rs`

Filesystem scan + JSONL ingest to keep the DB in sync.

**Architecture Invariant:** the indexer only reads from `provider.session_roots`; it never mutates session logs.

### `src/pipeline.rs`

Resolves providers/profiles/snippets/wrappers into an execution plan.

**Architecture Invariant:** `pipeline::build_pipeline` is deterministic for the same config + session metadata and does not perform IO.
**Architecture Invariant:** pipeline planning uses config + session data only; the filesystem is not consulted here.

### `src/prompts.rs`

Optional prompt-assembler integration for virtual profiles.

**Architecture Invariant:** prompt assembler failures degrade to a status message rather than hard errors.

### `src/providers/`

Provider-specific helpers (for example, resume metadata extraction).

**Architecture Invariant:** provider helpers do not mutate the DB directly.

### `src/session.rs`

Shared data types for sessions, transcripts, and rendering.

**Architecture Invariant:** transcript rendering uses indexed data and does not re-read the filesystem.

### `src/tui/`

Terminal UI for browsing sessions and profiles.

**API Boundary:** the TUI is a consumer of the same config + DB data as the CLI subcommands.

### `crates/tx/tests` and `tests/`

CLI integration tests using `assert_cmd`.

## Extension points

- Add a new provider: extend the config model, add helper logic under `src/providers/`, and update the indexer if the transcript format differs.
- Add a new command: update `src/cli.rs`, route it in `src/app.rs`, and add coverage under `crates/tx/tests` or `tests/`.
- Add new config keys: update the config schema, default template (`assets/default_config.toml`), and lint diagnostics.

## Testing strategy

- Unit tests live alongside modules under `src/`.
- CLI behavior is tested via `assert_cmd` in `crates/tx/tests` and `tests/`.

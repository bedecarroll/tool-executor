# tool-executor (tx)

tool-executor (`tx`) is a terminal-first launcher for AI code-assistant sessions, so you can replay pipelines, resume conversations instantly, and script the boring parts.

[Installation](#installation) · [Getting Started](#getting-started) · [Docs](#documentation) · [Contributing](#contributing)

tx keeps pipelines reproducible by capturing the full command graph, not just the last shell invocation. Its session browser helps you hop between transcripts without losing state, while guardrails like `tx doctor` and configuration linting make it safe to automate in shared environments.

## Who is tx for?

- Builders who bounce between multiple AI copilots and need consistent, auditable prompts.
- Teams that publish prompt catalogs and want a shared launch menu without syncing shell history.
- Automation engineers wiring AI flows into CI/CD and wanting resumable, inspectable runs.

## Key features

### Session management

Stay oriented across many conversations with the terminal UI. Browse recent sessions and saved profiles, preview transcripts inline, and resume a run without reassembling the pipeline manually.

- Recent-session browser with transcript previews.
- Search by prompt, role, or full-text transcript content.
- Quick resume actions that reuse the exact recorded pipeline.

### Pipeline composition

Define pre/post snippets, wrappers, and providers declaratively. tx stitches them together on resume so the same request is sent every time.

- Declarative pipeline definitions with reuse across profiles.
- Virtual profiles surface remote prompt catalogs beside local configs.
- Emission commands (`--emit-command`, `--emit-json`) for scripting and auditing.

### Reliability guardrails

Ship reproducible automation with safety tooling.

- `tx doctor` validates dependencies and config directories.
- Configuration linting warns about missing providers or invalid snippets.
- Resume tokens make it clear which pipeline version will execute.

## Getting started

### Installation

#### Download a release

Prebuilt archives are produced with `cargo dist`. Download the latest installer or archive from GitHub Releases:

```bash
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/bedecarroll/tool-executor/releases/latest/download/tx-installer.sh | sh
```

The installer places `tx` in `${CARGO_HOME:-$HOME/.cargo}/bin`. To download an archive manually, grab the matching `tx-<target>.tar.gz` or `.zip` from the same release page, unpack it somewhere on your `PATH`, then run `tx doctor` to confirm the reported version matches the tag you installed.

#### Build from source

```bash
git clone https://github.com/bedecarroll/tool-executor.git
cd tool-executor
cargo build --release
./target/release/tx doctor
```

The repository pins Rust 1.90 in `rust-toolchain.toml`, so `cargo` automatically selects the correct toolchain.

### First run walkthrough

1. Run `tx doctor` to create the configuration directory and confirm your environment.
2. Inspect the bundled template with `tx config default --raw` and copy the parts you need into `~/.config/tx/config.toml`.
3. Add a provider definition, for example:

   ```toml
   # ~/.config/tx/config.toml
   provider = "codex"

   [providers.codex]
   bin = "codex"
   flags = ["--search"]
   env = ["CODEX_TOKEN=${env:CODEX_TOKEN}"]
   ```

4. Launch `tx` and select a profile or previous session. The preview pane shows a transcript snippet or the assembled pipeline before execution.

### Resume a session

```bash
tx search onboarding
tx resume a1b2c3 --emit-command
```

Search narrows results to relevant prompts, and resume replays the captured pipeline. Use `--emit-command` to print the shell command tx would run, which helps with debugging or scripting.

## Command cheatsheet

```text
# Terminal UI
tx

# Search (prompt mode by default)
tx search refactor
tx search refactor --full-text --role assistant

# Resume and inspect pipelines
tx resume <session-id>
tx resume <session-id> --emit-command --emit-json

# Configuration helpers
$ tx config list
$ tx config dump
$ tx config where
$ tx config lint
$ tx config default --raw > ~/.config/tx/config.toml

# Diagnostics
$ tx doctor

# Transcript export
$ tx export <session-id> > notes.md
```

## Configuration essentials

Configuration lives under `~/.config/tx/` (or a custom directory via `--config-dir` or `TX_CONFIG_DIR`). Profiles reference providers, snippets, and wrappers so pipelines stay declarative. Use virtual profiles to surface external prompt catalogs alongside local definitions, and rerun `tx config lint` whenever you update configuration to catch missing dependencies.

## Developer guide

### Workspace layout

The repository publishes two crates: the `tool-executor` library and the `tx` CLI wrapper. Both crates share the same semantic version and must stay in lockstep—bump versions together and verify the library’s `Cargo.toml` and the CLI crate’s `Cargo.toml` agree before tagging a release.

### Coverage

Run the coverage suite with `mise`:

```bash
mise run coverage
```

Both `mise run test` and `mise run coverage` drive `cargo llvm-cov` with the `ci` Nextest profile. The coverage task starts from a clean workspace, writes reports to `coverage/html/`, emits an lcov file, and fails if line coverage drops below 90%. Running `mise run test` performs the instrumented run, produces `target/nextest/ci/junit.xml` for Codecov’s Test Analytics dashboard, and follows up with `cargo insta test`.

### Build cache

CI enables [`sccache`](https://github.com/mozilla/sccache) (`RUSTC_WRAPPER=sccache`, `SCCACHE_GHA_ENABLED=true`) to reuse compiler outputs across jobs. To mirror that locally, install sccache via your package manager and export `RUSTC_WRAPPER=sccache` before running cargo; `sccache --show-stats` reports cache hits.

### Benchmarks

Measure pipeline construction performance with Criterion:

```bash
mise run bench
```

Output lands in `target/criterion/`. The task enables the `benchmarks` feature automatically; run `cargo bench --features benchmarks --bench pipeline` when you need raw cargo commands. Close extra interactive apps like `tmux` to keep runs consistent and add new benchmarks under `benches/` as hotspots emerge.

### TUI smoke test

### TUI shortcuts

- `↑/↓`, `PgUp/PgDn` – navigate the active list.
- `Ctrl+F` – toggle between prompt search and full-text search.
- `Ctrl+P` – cycle provider filters when multiple backends are configured.
- `Tab` – emit the assembled pipeline to stdout.
- `Ctrl+Y` – print the selected session ID and close the TUI.
- `Ctrl+E` – export the selected session transcript and close the TUI.
- `Enter` – launch the selected session or profile.
- `Esc` – leave filter mode or close the TUI.

## Documentation

Browse the rendered mdBook at [tx.bedecarroll.com](https://tx.bedecarroll.com), or read the Markdown sources under `docs/`. Run `mdbook serve docs --open` (requires `mdbook`) for a live preview.

- Getting started: `docs/src/getting-started/`
- Configuration guide: `docs/src/configuration/`
- Advanced topics: `docs/src/advanced/`
- Reference material: `docs/src/reference/`

## Support

Questions or bug reports? Open an issue on GitHub and we’ll take a look.

## Contributing

1. Install the toolchain referenced by `rust-toolchain.toml` (Rust 1.90). Using [`mise`](https://mise.jdx.dev/) is recommended: `mise trust && mise install`.
2. Run the guard tasks in a red–green–refactor loop:

   ```bash
   mise run fmt
   mise run lint
   mise run test
   ```

3. Keep docs up to date alongside code (`mdbook build docs`).

Issues and pull requests are welcome. Please avoid committing `coverage/` artefacts.

## License

MIT

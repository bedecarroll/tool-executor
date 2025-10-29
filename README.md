# tx

tx is a terminal-first launcher for AI code-assistant sessions. It keeps your pipelines reproducible, lets you hop between conversations instantly, and gives you tooling hooks to automate prompt assembly, logging, and resume flows.

## Features

- **Focused TUI** – launch `tx` to browse recent sessions and saved profiles, preview transcripts, and run pipelines without touching shell history.
- **Rich search** – filter by prompt or toggle into full-text search for transcripts, then resume exactly where you left off.
- **Composable pipelines** – stitch pre/post snippets, wrappers, and providers together declaratively; tx reuses the recorded pipeline when you resume a session.
- **Virtual profiles** – surface external prompt catalogs (e.g. prompt-assembler) beside static profiles so teams share the same launch menu.
- **Safety rails** – `tx doctor`, configuration linting, and explicit resume tokens keep pipelines predictable across machines.

## Installation

### Download a release

Prebuilt archives are produced with `cargo dist`. Grab the latest release from this repository’s **Releases** page, unpack it somewhere on your `PATH`, and run `tx doctor` to verify dependencies.

### Build from source

```bash
git clone https://github.com/bedecarroll/tool-executor.git
cd tool-executor
cargo build --release
./target/release/tx doctor
```

The repository pins Rust 1.90 in `rust-toolchain.toml`, so `cargo` will automatically use the correct toolchain.

## First Run

1. Run `tx doctor` once to bootstrap the configuration/data directories and confirm your environment.
2. Inspect the bundled template with `tx config default --raw` and save the parts you need to `~/.config/tx/config.toml` (the directory is created for you).
3. Add at least one provider definition. A minimal example:

   ```toml
   # ~/.config/tx/config.toml
   provider = "codex"

   [providers.codex]
   bin = "codex"
   flags = ["--search"]
   env = ["CODEX_TOKEN=${env:CODEX_TOKEN}"]
   ```

4. Launch `tx` and pick a profile or previous session. The preview pane shows a transcript snippet or the assembled pipeline before you run it.

Additional configuration patterns—wrappers, snippets, prompt-assembler integration, and environment overrides—are covered in the mdBook under `docs/src/`.

## CLI Overview

```text
# Terminal UI
$ tx

# Search (prompt mode by default)
$ tx search refactor
$ tx search refactor --full-text --role assistant

# Resume and inspect pipelines
$ tx resume <session-id>
$ tx resume <session-id> --emit-command --emit-json

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

## Coverage

Run the coverage suite with `mise`:

```bash
mise run coverage
```

The task runs `cargo llvm-cov` against the full test matrix, writes HTML output under `coverage/html/`, emits an lcov file, and fails the build when line coverage drops below 95%. Regenerate the reports after adding tests and open `coverage/html/index.html` in a browser to inspect the annotated sources.

## Benchmarks

Measure pipeline construction performance with Criterion:

```bash
mise run bench
```

Benchmark outputs land in `target/criterion/`. The task enables the `benchmarks` feature automatically; run `cargo bench --features benchmarks --bench pipeline` if you prefer raw cargo commands. Keep `tmux` and other interactive dependencies closed while benching so results stay consistent. Add new benchmarks under `benches/` to track other hotspots.

## TUI Smoke Test

The integration test in `tests/tui_tmux.rs` launches the TUI inside `tmux` and verifies it exits cleanly when `Esc` is sent. The test skips automatically when `tmux` is unavailable or when the `CI` environment variable is set, so continuous integration environments are safe. To exercise the smoke test locally, install `tmux` and run `cargo test --test tui_tmux` (or `mise run test`).

## TUI Shortcuts

- `↑/↓`, `PgUp/PgDn` – move around the active list.
- Typing – start entering text to filter; `Backspace` edits the filter.
- `Ctrl+F` – toggle between prompt search and full-text search.
- `Ctrl+P` – cycle provider filters when multiple backends are configured.
- `Tab` / `Ctrl+I` – print the selected session id to stdout.
- `Ctrl+Shift+P` – emit the assembled pipeline to stdout (useful for scripting).
- `Ctrl+E` – export the current session transcript as Markdown.
- `Enter` – launch the selected session or profile.
- `Esc` – leave filter mode or close the TUI.

## Documentation

Browse the rendered mdBook at [tx.bedecarroll.com](https://tx.bedecarroll.com), or read the Markdown sources locally under `docs/`. Run `mdbook serve docs --open` (requires `mdbook`) for a live preview.

- Getting started: `docs/src/getting-started/`
- Configuration guide: `docs/src/configuration/`
- Advanced topics: `docs/src/advanced/`
- Reference material: `docs/src/reference/`

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

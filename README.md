# llml

The LLM launching tool

## Installation

Install via `cargo`:

```bash
cargo install --git https://github.com/bedecarroll/llml llml
```

Or install locally as a mise tool:

```bash
mise run pkg-install
```

## Usage

Show CLI help:

```bash
llml --help
```

Try the example command:

```bash
llml greet --name you
```

## Configuration

The CLI reads configuration from `$XDG_CONFIG_HOME/llml/config.toml` (or `~/.config/llml/config.toml` when `XDG_CONFIG_HOME` is unset). Files under `conf.d/*.toml` are loaded afterward in lexical order, letting you layer overrides. Provide an explicit directory with `--config-dir <dir>` or by setting the `LLML_CONFIG_DIR` environment variable.

Example configuration:

```toml
[greet]
default_name = "agent"
```

## Logging

Control verbosity with `-v/--verbose` (repeat for trace) or silence logs entirely with `-q/--quiet`. The `RUST_LOG` environment variable still overrides the log filter when you need custom directives.

## Completions & Man Page

Generate shell completions directly through the CLI:

```bash
cargo run --bin llml -- completions bash --dir docs/completions
```

Man pages can be emitted the same way:

```bash
cargo run --bin llml -- manpage --dir docs/man
```

Both commands print to standard output when `--dir` is omitted, so you can pipe the results wherever you like.

## Command Organization

Domain-specific subcommands live in `src/commands/`, each exposing a small `run` function that accepts a shared `CommandContext`. Add new modules here to keep the top-level CLI glue thin as the tool grows.

## Development Workflow

```bash
mise trust
mise install
```

This project targets Rust edition 2024. The toolchain is pinned to 1.90.0 in `rust-toolchain.toml`, comfortably above the edition's 1.85 minimum.

### Common Tasks

- `mise run fmt`
- `mise run lint` (clippy, typos, markdownlint, cargo-deny)
- `mise run test` (nextest + cargo-insta snapshots)
- `mise run coverage`
- `mise run test-doc`
- `mise run generate-completions`
- `mise run generate-manpage`
- `mise run doc`
- `mise run snapshot`
- `mise run snapshot-review`
- `mise run dist-plan`
- `mise run dist-build`

### Red-Green-Refactor Cycle

1. Add or update a test in `tests/` that expresses the new behaviour.
2. `mise run test` to watch the failure.
3. Implement the smallest change in `src/` to make the test pass.
4. Clean up before committing (`mise run fmt` and `mise run lint`).

### Coverage

Ensure the `llvm-tools-preview` component is installed (handled by `rust-toolchain.toml`).
Generate an lcov report and HTML output:

```bash
mise run coverage
open coverage/html/index.html
```

### Snapshot Testing

Snapshot assertions live under `tests/snapshots/` and use the [`insta`](https://crates.io/crates/insta) crate. Run `mise run snapshot` to execute them, and `mise run snapshot-review` to accept new snapshots after intentional changes.

### Releasing

Follow `docs/releasing.md` for manual `cargo dist` setup and release automation guidance. The `dist-*` tasks wrap common `cargo dist` flows once you are ready to ship archives.
Release artifacts default to `.tar.gz` on Unix and `.zip` on Windows so the optional `self-update` command can unpack them out of the box.

### Self-Update (Optional)

Enable the `self-update` cargo feature to compile the `self-update` subcommand, which pulls binaries from GitHub releases for `bedecarroll/llml`:

```bash
cargo install --features self-update --path .
llml self-update
```

### Toolchain Maintenance

To adopt a newer stable Rust toolchain:

1. `rustup update stable`
2. Update the pinned versions in `rust-toolchain.toml`, `Cargo.toml` (`rust-version`), and `mise.toml` (`[tools].rust`).
3. Refresh any documentation that mentions the toolchain version.
4. Re-run `mise run fmt`, `mise run lint`, and `mise run test` to confirm the upgrade.

Commit the resulting changes once everything is green.

## Contributing

Open an issue or pull request on GitHub at `https://github.com/bedecarroll/llml`.

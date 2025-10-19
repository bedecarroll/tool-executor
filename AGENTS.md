# AGENTS

## Purpose

This document describes how automated agents should interact with this repository. Follow every instruction unless the user explicitly overrides it.

## Quickstart

0. If this project was regenerated, choose a `project_name` with alphanumerics so the derived slug remains valid.
1. Run `mise trust` followed by `mise install` to provision local tools.
2. Use `mise run fmt`, `mise run lint`, and `mise run test` before opening a pull request.
3. Execute work in a red–green–refactor loop: write a failing test, make it pass, then tidy the code.
4. Never commit generated artefacts from `coverage/` or `target/`.
5. Stay on the pinned Rust toolchain (`rust-toolchain.toml`) to guarantee edition 2024 compatibility and MSRV 1.90 features.

## Tooling

- Tool versions are pinned in `mise.toml`. Do not install tools globally.
- `rust-toolchain.toml` keeps `rustfmt`, `clippy`, and `llvm-tools-preview` aligned at Rust 1.90.0 (edition 2024). Avoid overriding the toolchain without discussion.
- `cargo-nextest` powers the primary test task. Prefer it over `cargo test` unless you need doctests.
- `cargo-llvm-cov` generates coverage metrics. Output lives in `coverage/`.
- `cargo-insta` streamlines snapshot updates (`mise run snapshot-review`).

## Tasks

Run tasks with `mise run <task>`:

- `fmt` — format the workspace with `cargo fmt --all`.
- `lint` — clippy (pedantic, warnings-as-errors), typos, markdownlint, and `cargo deny`.
- `lint-fix` — auto-fix typos and Markdown issues, then format code.
- `test` — execute the nextest suite (nextest + cargo-insta snapshots).
- `test-doc` — run `cargo test --doc` for doctests.
- `coverage` — produce lcov and HTML reports via llvm-cov.
- `pkg-install` — install the binary locally using `cargo install --path .`.
- `check` — aggregated guard that depends on `fmt`, `lint`, `test`, and `test-doc`.
- `generate-completions` — write shell completion scripts under `docs/completions/`.
- `generate-manpage` — render the CLI man page under `docs/man/`.
- `doc` — build API documentation locally.
- `snapshot` / `snapshot-review` — execute and accept insta snapshot tests.
- `dist-init`, `dist-plan`, `dist-build` — wrap common `cargo dist` flows.

## Coding Standards

- Keep modules and functions small; avoid speculative abstractions.
- Return rich errors using `color-eyre`'s `Result` alias for consistent diagnostics.
- Route domain logic through modules in `src/commands/` and keep `src/main.rs` minimal.
- Public functions that return `Result` must explain failure modes in doc comments.
- Do not introduce new global state or singletons.
- Prefer adjusting lint levels via the `[lints.clippy]` table in `Cargo.toml` rather than sprinkling `#[allow]` attributes.

## Configuration

- User configuration lives in `$XDG_CONFIG_HOME/tx/config.toml` (falling back to `~/.config/...`). Additional overrides load from the `conf.d/` directory in lexical order.
- Honour the `--config-dir` flag or `TX_CONFIG_DIR` when scripting automated workflows.

## Logging

- Default log level is `info`. Increase verbosity with `-v`/`-vv`, or suppress logs with `-q`.
- Respect `RUST_LOG` in CI automation when you need fine-grained control.

## Testing & Coverage

- Add tests under `tests/` using `assert_cmd` to exercise CLI surface area.
- When adding new behaviour to `src/lib.rs`, prefer unit tests located alongside the module.
- Keep fixtures under `tests/fixtures/` (create the directory when needed).
- Generate coverage with `mise run coverage`; upload artefacts in CI if required by downstream tooling.
- Use `insta` snapshots (`mise run snapshot`) for multi-line or complex output assertions; commit updated snapshots after reviewing `mise run snapshot-review`.

## Continuous Integration

- Workflows use `jdx/mise-action` to sync tool versions. Avoid adding ad-hoc install steps.
- Jobs run `mise run fmt`, `mise run lint`, `mise run test`, and `mise run coverage`. Keep tasks deterministic and idempotent.
- Update caching configuration rather than skipping tasks if build time increases.

## Releases

- `cargo dist` configuration is user-managed. Run `cargo dist init` when you are ready to automate releases, then commit the generated files (see the `dist-*` tasks).
- Cargo-dist archives default to `.tar.gz` on Unix and `.zip` on Windows so the optional `self-update` feature can unpack release assets without extra configuration.
- Document each release in `CHANGELOG.md` following Keep a Changelog conventions.

## Optional Features

- Enable the `self-update` cargo feature to compile the `self-update` subcommand, which updates from GitHub releases.

## Housekeeping

- Update `typos.toml` when introducing new proper nouns to prevent CI failures.
- Keep `rust-toolchain.toml`, `Cargo.toml`, and `mise.toml` in sync when bumping the MSRV.
- To adopt a new stable Rust release: `rustup update stable`, update the pinned versions in `rust-toolchain.toml`, `Cargo.toml (rust-version)`, and `mise.toml`, refresh documentation that cites the version, then rerun `mise run fmt`, `mise run lint`, and `mise run test`.
- Confirm `AGENTS.md` stays current whenever tooling changes.

## Escalations

If a task requires new tooling or significant workflow changes, open an issue tagged `workflow` and document the proposal before implementing.

# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/)
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

<!-- markdownlint-disable-next-line MD024 -->
### Breaking

- Release artifacts now build from the dedicated `tx` CLI crate, producing `tx-<target>` archives and installer scripts. Existing automation that downloaded `tool-executor-<target>` assets must update to the new filenames. Keep the `tool-executor` library and `tx` CLI crate versions in sync for the v0.4.0 release onward.

<!-- markdownlint-disable-next-line MD024 -->
### Added

- Nothing yet.

<!-- markdownlint-disable-next-line MD024 -->
### Changed

- Nothing yet.

<!-- markdownlint-disable-next-line MD024 -->
### Fixed

- Nothing yet.

## [0.5.0] - 2025-11-01

<!-- markdownlint-disable-next-line MD024 -->
### Breaking

- Removed the `preview_filter` configuration option and its runtime plumbing. The TUI no longer shells out to external preview commands, so existing configs referencing tools like `glow` must delete the key.

<!-- markdownlint-disable-next-line MD024 -->
### Added

- Built-in Markdown rendering for session and profile previews via `tui-markdown`, so headings, emphasis, and code blocks display without relying on external filters.

<!-- markdownlint-disable-next-line MD024 -->
### Changed

- Upgraded the TUI stack to `ratatui` 0.29.0 (and aligned `unicode-width` to 0.2.0) to support the embedded Markdown renderer.

<!-- markdownlint-disable-next-line MD024 -->
### Fixed

- Preview panes no longer emit OSC/DCS escape sequences triggered by external filters because rendering now happens entirely inside the binary.

## [0.4.1] - 2025-11-01

<!-- markdownlint-disable-next-line MD024 -->
### Changed

- Added per-task timeouts (`timeout = "20s"`) to the `test` and `coverage` mise tasks so long-running suites finish without needing environment overrides.
- Removed the macOS-specific clap color override and now parse CLI arguments uniformly across platforms.

<!-- markdownlint-disable-next-line MD024 -->
### Fixed

- Sanitized terminal preview filters to strip OSC/DCS control strings before rendering, preventing accidental hyperlink, clipboard, or background-color injections when external filter commands emit escape sequences.

## [0.3.0] - 2025-10-31

<!-- markdownlint-disable-next-line MD024 -->
### Added

- Prompt-assembler integration now calls `pa show --json` so the TUI preview pane can display assembled prompt contents alongside metadata.

<!-- markdownlint-disable-next-line MD024 -->
### Changed

- The preview pane renders Markdown metadata plus a fenced `markdown` block for both virtual prompt entries and provider profiles, making filtered output easier to read.

## [0.2.0] - 2025-10-31

<!-- markdownlint-disable-next-line MD024 -->
### Added

- Introduced the `tx config schema` subcommand and bundled JSON Schema (`docs/src/assets/config-schema.json`) so tooling can validate configuration files.
- Added new TUI shortcuts: `Ctrl+Y` prints the highlighted session ID and exits; `Ctrl+E` exports the highlighted transcript before exiting.

<!-- markdownlint-disable-next-line MD024 -->
### Changed

- **BREAKING (macOS):** Default config/data/cache directories now follow the XDG spec (e.g. `~/.config/tx`); existing setups under `~/Library` must be migrated or point `TX_CONFIG_DIR`, `TX_DATA_DIR`, and `TX_CACHE_DIR` to the legacy locations.
- `tx self-update` now ships in every build; the updater no longer requires an optional feature flag.

<!-- markdownlint-disable-next-line MD024 -->
### Fixed

- macOS terminals no longer receive OSC 11 background-color probes, avoiding hangs and warnings when the sequence is unsupported.

## [0.1.0] - 2025-10-28

<!-- markdownlint-disable-next-line MD024 -->
### Added

- Initial project scaffold created via `rust-cli` cookiecutter.
- `tx` terminal UI for browsing sessions, resuming pipelines, and launching providers.
- Configuration helpers (`tx config *`) and environment doctor (`tx doctor`).
- Search and resume commands for scripting workflows without the TUI.

# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/)
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Nothing yet.

### Changed

- Nothing yet.

### Fixed

- Nothing yet.

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

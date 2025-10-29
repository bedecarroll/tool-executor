# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/)
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- `tx self-update` now ships in every build; the updater no longer requires an optional feature flag.

## [0.1.0] - 2025-10-28

<!-- markdownlint-disable-next-line MD024 -->
### Added

- Initial project scaffold created via `rust-cli` cookiecutter.
- `tx` terminal UI for browsing sessions, resuming pipelines, and launching providers.
- Configuration helpers (`tx config *`) and environment doctor (`tx doctor`).
- Search and resume commands for scripting workflows without the TUI.

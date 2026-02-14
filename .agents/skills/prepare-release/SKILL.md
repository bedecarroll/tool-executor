---
name: prepare-release
description: Prepare release pull requests for this repository using Jujutsu (`jj`) and GitHub CLI (`gh`). Use when asked to cut a release, bump versions, move `CHANGELOG.md` entries from `Unreleased` into a dated version section, run release validation (`mise run fmt`, `mise run lint`, `mise run test`, `mise run coverage`), and open or update a release PR. Stop after the PR is ready; do not create GitHub tags.
---

# Prepare Release

## Overview

Create or update a release PR in this repo with consistent versioning, changelog structure, and verification. Follow the same conventions used by recent release PRs (`chore(release): vX.Y.Z`, optional `!` for breaking changes).

## Inputs

Collect these first:

- Target version (`X.Y.Z`)
- Release date (`YYYY-MM-DD`)
- Breaking change status (`yes` or `no`)
- PR mode (`create` new release PR or `update` existing release PR)

## Repository Conventions

- Use `jj`, not `git`, for branch/bookmark/commit/push operations.
- Keep versions in sync across:
  - `Cargo.toml` (`[workspace.package].version`)
  - `crates/tx/Cargo.toml` (`[package].version`)
  - `Cargo.lock` package entries for `tool-executor` and `tx`
- Update `CHANGELOG.md` with Keep a Changelog format:
  - Promote relevant `## [Unreleased]` entries into `## [X.Y.Z] - YYYY-MM-DD`
  - Reset `Unreleased` subsections to placeholders (`Nothing yet.`)
- Use release commit/PR titles:
  - Non-breaking: `chore(release): vX.Y.Z`
  - Breaking: `chore(release)!: vX.Y.Z`
- Do not tag the release. The user creates GitHub tags manually.

## Workflow

1. Prepare working copy.

- Run `jj status`.
- If creating a new release PR: run `jj new 'trunk()'`.
- If updating an existing release PR: stay on that PR change.

1. Update versions.

- Edit `Cargo.toml` and `crates/tx/Cargo.toml` to the target version.
- Refresh `Cargo.lock` so `tool-executor` and `tx` package versions match.

1. Update changelog.

- In `CHANGELOG.md`, create/update `## [X.Y.Z] - YYYY-MM-DD`.
- Move release-ready notes out of `## [Unreleased]`.
- If breaking, include a `### Breaking` section.
- Reset `Unreleased` headings with `Nothing yet.` placeholders.

1. Validate.

- Run:
  - `mise run fmt`
  - `mise run lint`
  - `mise run test`
  - `mise run coverage`

1. Commit.

- Commit with:
  - `chore(release): vX.Y.Z`, or
  - `chore(release)!: vX.Y.Z` for breaking releases.

1. Push with `jj`.

- If creating a new PR:
  - `jj push-new`
- If updating an existing PR:
  - Move bookmark to latest change: `jj bookmark move <bookmark> --to @-`
  - Push update: `jj push`

1. Open or update PR with `gh`.

- Base branch: `master`.
- Title matches commit (`chore(release)...`).
- Body format:
  - `## Summary` with concrete release changes (version bump, changelog, lockfile, notable breaking/support changes)
  - `## Testing` listing the four `mise run ...` commands
- Create PR if new; edit PR if updating.

1. Stop and hand off.

- Report PR URL/number and final status.
- Explicitly state: tagging in GitHub is manual and left to the user.

## PR Template

Use this structure:

```markdown
## Summary
- bump workspace and CLI crate versions to `X.Y.Z`
- update `CHANGELOG.md` with `## [X.Y.Z] - YYYY-MM-DD`
- refresh `Cargo.lock` package versions for `tool-executor` and `tx`
- include any breaking/supportability notes (if applicable)

## Testing
- `mise run fmt`
- `mise run lint`
- `mise run test`
- `mise run coverage`
```

## Guardrails

- Do not change release tagging workflow; user tags manually on GitHub.
- Do not skip validation commands unless the user explicitly requests skipping.
- Do not revert unrelated working-copy changes.

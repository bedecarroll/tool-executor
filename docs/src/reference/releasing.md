# Release Process

The `cargo dist` configuration lives under `[workspace.metadata.dist]` in `Cargo.toml`. Follow these steps when preparing a release:

1. Start from a clean working tree and an updated `CHANGELOG.md`. Once familiar, prefer the wrapper tasks `mise run dist-plan` and `mise run dist-build`.
2. Install the tooling:

   ```bash
   cargo install cargo-dist
   ```

3. Initialize metadata the first time:

   ```bash
   cargo dist init
   ```

   Archives default to `.tar.gz` on Unix and `.zip` on Windows so `tx self-update` can consume them without extra configuration.
4. Review the changes to `Cargo.toml` and any suggested workflow files; commit them together.
5. For each release candidate:

   ```bash
   cargo dist plan
   cargo dist build
   cargo dist host
   cargo dist announce
   ```

6. Tag the release and push to GitHub.

When `cargo dist` edits CI workflows, prefer running it locally and committing the diff instead of patching YAML by hand.

# Release Process

The release configuration for cargo-dist lives under `[workspace.metadata.dist]` in `Cargo.toml`. Follow these steps when you are ready to automate binary distribution:

1. Ensure a clean working tree and up-to-date `CHANGELOG.md` entry. Once the process below is familiar, `mise run dist-plan` and `mise run dist-build` provide convenient wrappers around the key commands.
2. Install dist tooling:

   ```bash
   cargo install cargo-dist
   ```

3. Initialize the configuration:

   ```bash
   cargo dist init
   ```

   The template pins default archive formats to `.tar.gz` (Unix) and `.zip` (Windows) so the optional `self-update` command can consume the published assets without extra tweaks.
4. Review the modifications to `Cargo.toml` (workspace metadata) and commit them alongside any workflow updates that `cargo dist` suggests.
5. For each release candidate:

   ```bash
   cargo dist plan
   cargo dist build
   cargo dist host
   cargo dist announce
   ```

6. Tag the release and push to GitHub.

If cargo-dist modifies workflow files, prefer running it locally and committing the diff rather than editing manually.

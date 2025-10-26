# Self-Update Feature

Enable the optional `self-update` feature when you want tx to download releases from GitHub.

Add the feature during compile time:

```bash
cargo install --features self-update --path .
```

The resulting binary exposes a `tx self-update` command. Usage:

```bash
tx self-update             # update to the latest GitHub release
tx self-update --version v0.2.0
```

The release archive formats are aligned with `cargo dist` defaults (`.tar.gz` on Unix, `.zip` on Windows). Keep them in sync if you customize the distribution pipeline.

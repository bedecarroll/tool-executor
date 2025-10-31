# Self-Update Feature

`tx` ships with a built-in `self-update` subcommand that pulls releases from GitHub. Install from source as usual—the command is available without extra feature flags:

```bash
cargo install --path .
```

Usage:

```bash
tx self-update             # update to the latest GitHub release
tx self-update --version v0.3.0
```

The release archive formats are aligned with `cargo dist` defaults (`.tar.gz` on Unix, `.zip` on Windows). Keep them in sync if you customize the distribution pipeline.

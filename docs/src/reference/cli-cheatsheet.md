# CLI Cheat Sheet

| Command | Description |
| --- | --- |
| `tx` | Launch the TUI. |
| `tx search [query]` | List sessions. Use `--full-text` to search every message and `--role` to filter by `user` or `assistant`. |
| `tx resume <session-id>` | Resume a session with its original configuration. |
| `tx export <session-id>` | Export a transcript as Markdown. |
| `tx config list` | Enumerate currently active configuration files. |
| `tx config dump` | Print the merged configuration. |
| `tx config where` | Show the source location for a specific key. |
| `tx config lint` | Run configuration validation checks. |
| `tx doctor` | Diagnose common environment and dependency issues. |
| `tx self-update [--version]` | Update the binary to the latest (or specified) GitHub release. |

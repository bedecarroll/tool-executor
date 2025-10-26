# Troubleshooting

| Symptom | Fix |
| --- | --- |
| Providers missing from the TUI | Run `tx config lint` to confirm the configuration loaded, then check that provider names match the profiles referencing them. |
| Virtual profiles absent | Ensure prompt-assembler is installed and `features.pa.enabled = true`. Press `R` in the TUI to refresh. |
| Search returns no hits | Rebuild the index by clearing `~/.cache/tx` and restarting. Verify the session logs still exist in the expected directories. |
| Pipelines fail with exit code 127 | The provider or snippet binary is not discoverable. Run `tx doctor` and update the `PATH` or absolute paths. |
| tmux wrappers exit immediately | Include quotes around `{{CMD}}` when using `shell = true` so tmux receives the full pipeline command. |

For verbose tracing, add `-vv` or set `RUST_LOG=tx=debug`. Capture logs when filing issues so maintainers can reproduce the environment.

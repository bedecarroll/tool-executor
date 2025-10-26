# Logging and Diagnostics

Increase verbosity with `-v` or `-vv`:

```bash
tx -v              # info + debug summaries
tx -vv             # full debug traces
```

Set `RUST_LOG` for granular control over subsystems:

```bash
RUST_LOG=tx=debug,rusqlite=warn tx
```

When troubleshooting configuration or prompt-assembler integration, run `tx doctor`. It verifies provider executables, checks for schema drift, and surfaces missing configuration keys. Combine it with verbose logging to track down path issues or environment variables.

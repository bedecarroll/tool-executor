# Configuration Files

tx loads configuration in this order:

1. `$TX_CONFIG_DIR/config.toml` (or `$XDG_CONFIG_HOME/tx/config.toml` when the environment variable is unset).
2. All files under `$TX_CONFIG_DIR/conf.d/` sorted lexicographically.
3. Command-line overrides such as `--config-dir` rewrite the base path and restart the process above.

Each file is optional; tx tolerates partial definitions. The loader merges tables rather than replacing the entire structure, so you can keep shared defaults in `config.toml` and apply local overrides in numbered drop-ins.

Use small files with predictable numbers: `00-providers.toml` for core provider declarations, `10-profiles.toml` for team workflows, `20-local.toml` for personal overrides. This keeps diffs targeted and makes it easy to sync only the pieces collaborators need.

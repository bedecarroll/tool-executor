# Directory Layout

tx follows XDG conventions for configuration and caches. The default locations are:

| Purpose | Environment | Default Path |
| --- | --- | --- |
| Configuration | `TX_CONFIG_DIR` or `XDG_CONFIG_HOME` | `~/.config/tx` |
| Data (session log index, database) | `XDG_DATA_HOME` | `~/.local/share/tx` |
| Cache (search index, transient state) | `XDG_CACHE_HOME` | `~/.cache/tx` |

Configuration is layered. tx loads the base `config.toml` from the config directory and merges overrides from `conf.d/*.toml` in lexical order. Use small, well-scoped drop-in files (`00-providers.toml`, `10-profiles.toml`, and so on) so teams can share or version-control only the pieces that change.

If you prefer a custom layout, pass `--config-dir <path>` or export `TX_CONFIG_DIR`. The override applies to both the main file and the `conf.d/` directory.

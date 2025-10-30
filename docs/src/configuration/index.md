# Configuration Guide

tx reads declarative TOML. This section explains how configuration files are discovered, how individual sections work, and how to combine them into reusable profiles.

## Command Syntax

Any field in this guide that asks for a command accepts either a single string or an explicit array. A string such as `"glow -s dark"` is parsed with shell-style quoting, while an array like `["glow", "-s", "dark"]` keeps arguments exactly as written. Pick the form that keeps the intent clearest in your configuration files.

## Schema Reference

Generate the complete JSON Schema with `tx config schema --pretty` whenever you need editor integration or automated validation. The published documentation bundles the latest schema at [config-schema.json](../assets/config-schema.json); download it directly or pipe the CLI output to a file to stay in sync.

### Editor Integration

Many editors understand the `#:schema` directive at the top of a TOML file. Add the following line to any `tx` configuration file to enable inline validation, autocomplete, and hover docs:

```
#:schema https://tx.bedecarroll.com/assets/config-schema.json
```

After saving, supported editors will:

- Suggest configuration keys as you type.
- Flag invalid values with diagnostics.
- Surface field documentation on hover.

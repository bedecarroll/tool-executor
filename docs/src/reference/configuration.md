# Configuration Reference

This table lists every key tx reads from configuration files. Types refer to TOML types.

## Defaults

| Key | Type | Default | Description |
| --- | --- | --- | --- |
| `provider` | `string` | `codex` | Provider used when starting a new session without a profile. Must match a key under `[providers]`. |
| `profile` | `string` | _unset_ | Preferred profile when creating sessions. Must match a key under `[profiles]`. |
| `search_mode` | `string` | `first_prompt` | Initial search mode in the TUI. Accepts `first_prompt` or `full_text`. |

Sessions the indexer marks as unactionable stay hidden from default listings but remain searchable.

## Providers (`[providers.<name>]`)

| Key | Type | Required | Description |
| --- | --- | --- | --- |
| `bin` | `string` | ✅ | Executable or absolute path. |
| `flags` | `array<string>` | | Default arguments passed to the provider. |
| `env` | `array<string>` | | Environment entries (`KEY=value`). Supports `${env:VAR}` interpolation. |
| `stdin_to` | `string` | | Template describing how to inject captured stdin into the argv list. Requires `stdin_mode = "capture-arg"`. |
| `stdin_mode` | `string` | | Delivery mode: `pipe` (default) streams stdin; `capture-arg` passes stdin as an argument. |

## Snippet Commands (`[snippets.pre]`, `[snippets.post]`)

Values are shell commands executed before or after the provider. They can reference template tokens such as `{{session.id}}` or `{{var:KEY}}`.

## Wrappers (`[wrappers.<name>]`)

| Key | Type | Required | Description |
| --- | --- | --- | --- |
| `shell` | `bool` | | When `true`, invoke the wrapper via `/bin/sh -c`. Defaults to `false`. |
| `cmd` | `string` or `array<string>` | ✅ | Wrapper command. Use a string when `shell = true`; use an array for argv-style declarations. `{{CMD}}` expands to the provider command. |

## Profiles (`[profiles.<name>]`)

| Key | Type | Required | Description |
| --- | --- | --- | --- |
| `provider` | `string` | ✅ | Provider key to run. |
| `description` | `string` | | Label shown in the TUI preview. |
| `pre` | `array<string>` | | Ordered list of pre-snippet names. |
| `post` | `array<string>` | | Ordered list of post-snippet names. |
| `wrap` | `string` | | Wrapper name. |

## Prompt Assembler (`[features.pa]`)

| Key | Type | Default | Description |
| --- | --- | --- | --- |
| `enabled` | `bool` | `false` | Enable the integration. |
| `namespace` | `string` | `pa` | Prefix applied to virtual profile names. |

## Derived Values

- Session log roots for the `codex` provider live under `$CODEX_HOME` or fall back to `~/.codex/session[s]`.
- Directory paths expand `~` and environment variables using `shellexpand` with the same rules the runtime uses.

Keys not listed here are ignored. Keep configuration minimal and additive so drop-ins compose cleanly.

# Configuration Reference

This document lists every configuration key that `tx` understands, along with
its type, default value, and runtime behavior. Configuration is expressed in
TOML and loaded from the locations described in the `README`.

## Default Settings

Top-level scalar keys set application-wide defaults.

| Key | Type | Default | Description |
| --- | --- | --- | --- |
| `provider` | `string` | `codex` | Provider used when launching a new session with no explicit provider flag. Must match a key under `[providers]`. |
| `profile` | `string` | _unset_ | Preferred profile when creating sessions. Must match a key under `[profiles]`. |
| `search_mode` | `string` | `first_prompt` | Chooses the initial search strategy in the TUI: `first_prompt` (default, prefix search on the first user message) or `full_text` (FTS across the whole transcript). |
| `preview_filter` | `string` or `array` | _unset_ | Optional command that post-processes preview text. A string is parsed with `shlex` (e.g. `"glow -s dark"`). An array form (e.g. `["glow","-s","dark"]`) remains accepted for compatibility. Empty strings or arrays disable the filter. |

Sessions that the indexer marks as _unactionable_ stay hidden from default listings. They still appear when you use search (either prompt-only or full-text) so you can retrieve them when needed.

## Provider Definitions (`[providers.<name>]`)

Each provider entry defines how to launch an external tool.

| Key | Type | Required | Description |
| --- | --- | --- | --- |
| `bin` | `string` | ✅ | Executable name or absolute path to invoke. |
| `flags` | `array<string>` | | Default arguments passed to the command. |
| `env` | `array<string>` | | Environment entries written as `KEY=value`. Values support `${env:VAR}` interpolation. |
| `stdin_to` | `string` | | Optional mapping from assembled stdin to provider arguments. Tokens are parsed with `shlex`. Include `"{prompt}"` where the captured prompt should be substituted (omit it to append the prompt as a final argument). |
| `stdin_mode` | `string` | | How stdin should be delivered. `"pipe"` (default) streams stdin directly. `"capture-arg"` consumes stdin (after running pre snippets) and passes it as a single positional argument. |

## Snippet Commands (`[snippets.pre]`, `[snippets.post]`)

The `pre` and `post` tables map snippet names to shell commands executed before
or after a provider runs. Each value is a single string; template variables such
as `{{var:KEY}}` and `{{session.id}}` match the placeholders described in the
`README`.

## Wrappers (`[wrappers.<name>]`)

Wrappers let you run providers inside another process (for example `tmux` or
`docker`).

| Key | Type | Required | Description |
| --- | --- | --- | --- |
| `shell` | `bool` | | When `true`, treat `cmd` as a shell string executed with `/bin/sh -c`. Defaults to `false`. |
| `cmd` | `string` or `array<string>` | ✅ | Wrapper command. Use the string form when `shell = true` (e.g. `"tmux new -s tx-{{session.id}} '{{CMD}}'"`). Use the array form when `shell = false` to provide an argv list. The `{{CMD}}` placeholder expands to the final provider command. |

## Profiles (`[profiles.<name>]`)

Profiles bundle providers with optional snippets and wrappers.

| Key | Type | Required | Description |
| --- | --- | --- | --- |
| `provider` | `string` | ✅ | Provider key to launch. |
| `description` | `string` | | Short label shown in the TUI next to the profile name. Provide launch guidance here. |
| `pre` | `array<string>` | | Ordered list of snippet names from `[snippets.pre]` to run beforehand. |
| `post` | `array<string>` | | Snippet names from `[snippets.post]` to run after the session. |
| `wrap` | `string` | | Name of a wrapper from `[wrappers]`. |

## Features (`[features.pa]`)

The prompt-assembler feature is currently the only optional module.

| Key | Type | Default | Description |
| --- | --- | --- | --- |
| `enabled` | `bool` | `false` | Enable the prompt assembler. When `false` the namespace is ignored. |
| `namespace` | `string` | `pa` | Prefix used when storing assembler output. |

`tx` invokes `pa list --json` directly; additional strategies or caching knobs
will be added here if the upstream tool grows alternative APIs.

## Derived Values

The application also derives a few values from the environment:

- Session log roots for the `codex` provider are discovered under `$CODEX_HOME`
  (if set) or `$HOME/.codex/session[s]`.
- Directory paths interpolate `~` and environment variables using the same rules
  as the runtime (`shellexpand::full`).

No additional user-configurable flags exist outside the keys listed above.

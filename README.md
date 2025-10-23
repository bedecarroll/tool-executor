# tx -- Tool eXecutor

A thin, configurable launcher that starts, resumes, and searches **AI code-assistant sessions** (e.g., Codex, Claude Code, Aider). It gives you an Atuin-style TUI, composable **pre/post** pipelines, and **wrappers** (e.g., tmux, docker) -- all via simple `conf.d/` drop-ins.

* **Binary:** `tx`
* **Repo:** `tool-executor`

## Highlights

* **One-command TUI**: run `tx`, pick a recent session or a profile, **Tab** to insert the command into your shell, **Enter** to run.
* **Search**: first-prompt (fast) or **full-text** (FTS) across stored session logs.
* **Pipelines**: `pre | provider <args> | post` -- stdin flows naturally.
* **Wrappers**: run the assembled pipeline inside tmux/nohup/docker/etc. via `{{CMD}}` templates.
* **Drop-in config**: `~/.config/tx/conf.d/*.toml` overrides/extends without mode switches.
* **Prompt-assembler integration (optional)**: discover prompts via `pa list --json` and treat them as virtual profiles.
* **Provider safety**: resuming a session always uses its original provider.

---

## Install

```bash
cargo install --path .              # from a clone
# or once published:
# cargo install tool-executor
```

### Shell keybinding (Atuin-style insertion)

Bind **Alt-t** to open `tx`, select an item, then **Tab** to insert the resolved command into your current line (it will not run).

Inside the TUI, press **Tab** to emit the assembled command to stdout and exit. You can wire that into your shell so the command replaces the current line:

#### bash

```bash
# Add to ~/.bashrc
_tx_insert() {
  local cmd
  cmd="$(tx 2>/dev/null)" || return  # launch the TUI, pick an entry, press Tab
  READLINE_LINE="$cmd"
  READLINE_POINT=${#READLINE_LINE}
}
bind -x '"\et":"_tx_insert"'
```

#### zsh

```zsh
# Add to ~/.zshrc
_tx_insert() {
  local cmd
  cmd="$(tx 2>/dev/null)" || return   # launch the TUI, pick an entry, press Tab
  LBUFFER="$cmd"
  CURSOR=${#LBUFFER}
}
zle -N _tx_insert
bindkey "\et" _tx_insert
```

#### fish

```fish
# Add to ~/.config/fish/config.fish
function _tx_insert
  set -l cmd (tx 2>/dev/null)
  test -n "$cmd"; or return
  commandline -r -- $cmd
end
bind \et '_tx_insert'
```

---

## Quick Start

1. Create config directories:

```bash
mkdir -p ~/.config/tx/conf.d ~/.local/share/tx ~/.cache/tx
```

1. Drop a minimal provider config (example):

```toml
# ~/.config/tx/conf.d/00-providers.toml
[providers.codex]
bin = "codex"
flags = ["--search"]
env = ["CODEX_TOKEN=${env:CODEX_TOKEN}"]
# Session logs are discovered automatically from $CODEX_HOME (defaults to ~/.codex).

[providers.claude]
bin = "claude-code"
flags = []
env = ["CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1"]
# Session discovery is not yet implemented.
```

1. (Optional) Add wrappers and a profile:

```toml
# ~/.config/tx/conf.d/10-profiles.toml
[wrappers.tmux_simple]
shell = true
cmd   = "tmux new -s tx-{{session.id}} '{{CMD}}'"

[profiles.kickoff]
provider = "codex"
pre  = []
post = []
wrap = "tmux_simple"
```

1. Run `tx`, pick **kickoff** or a recent session; **Enter** runs; **Tab** inserts.

---

## Usage

```text
# TUI
tx                     # open UI: recent sessions + profiles (new sessions)

# Search
tx search                         # list recent sessions (archived entries stay hidden)
tx search asset                   # first user-prompt only
tx search asset --full-text       # full-text FTS search (JSON output by default)
tx search context --full-text --role assistant   # filter FTS hits to assistant replies

# Launch / resume (non-TUI paths also supported)
tx launch <provider> [--profile NAME] [--pre NAME...] [--post NAME...] [--wrap NAME] [--] [provider-args...]
tx resume <session-id> [--profile NAME] [--pre ...] [--post ...] [--wrap NAME] [--] [provider-args...]

# Export transcript
tx export <session-id>

# Config helpers
tx config list|dump|where|lint

# Diagnostics
tx doctor

# Self-update (requires `--features self-update`)
tx self-update [--version TAG]
```

### Search output

`tx search` always emits pretty JSON. For full-text hits, the payload includes:

* `snippet`: the full message line that matched.
* `snippet_role`: `"user"` or `"assistant"` so you can filter or style results.
* `last_active`: seconds since epoch (UTC) for easy recency sorting.

Prompt-only searches populate `snippet` with the first user prompt and omit `snippet_role`.

### Keys inside TUI

* `Up/Down` move
* `/` filter
* `Tab` insert
* `Enter` run
* `Ctrl-F` toggle full-text
* `p` provider filter
* `R` reindex
* `e` export
* `?` help

---

## Configuration (TOML)

`tx` loads in order (later overrides earlier):

```text
~/.config/tx/config.toml           # optional base
~/.config/tx/conf.d/*.toml         # lexicographic order (00-... 50-... 99-...)
./.tx.toml                         # project-local (optional)
./.tx.d/*.toml                     # project-local drop-ins (optional)
```

### Merge rules

* Tables deep-merge; scalars overwrite.
* Arrays replace unless you use the `+` form to append: `pre+ = ["assemble"]`.
* Set a key to `null` to delete it.

### Schema overview

```toml
provider = "codex"
profile  = "kickoff"           # default profile for new sessions
search_mode = "first_prompt"   # or "full_text"
# preview_filter = "glow -s dark"  # optional: pipe the preview pane through an external formatter
# preview_filter = ["glow", "-p"]  # array form also accepted

[providers.<name>]
bin = "codex"                  # executable name or path
flags = ["--search"]           # ordered default flags
env = ["KEY=${env:KEY}"]      # per-provider env vars (expanded)
# Session logs for Codex are discovered automatically from $CODEX_HOME.
stdin_to = "codex:--prompt -"  # (optional) map stdin into a provider flag

[snippets.pre]
assemble = "pa {{var:pa_prompt}} {{var:pa_data}}"   # example

[snippets.post]
save_md  = "tee session.md"

[wrappers.<name>]
shell = true|false
cmd   = "tmux new -s tx-{{session.id}} '{{CMD}}'"     # if shell=true
# or cmd = ["docker","run","-i","...","/bin/sh","-lc","{{CMD}}"]   # if shell=false

[profiles.<name>]
provider = "codex"
pre  = ["assemble"]
post = ["save_md"]
wrap = "tmux_simple"
```

### Variables available in templates

* `{{CMD}}` -- the fully assembled pipeline string
* `{{provider}}`, `{{session.id}}`, `{{session.label}}`, `{{cwd}}`
* `{{var:NAME}}` -- from `--var NAME=VALUE` or TUI prompts

---

## Prompt-assembler (optional)

```toml
# ~/.config/tx/conf.d/20-pa.toml
[features.pa]
enabled = true
strategy = "exec_only"      # tx will call: pa list --json
namespace = "pa"
cache_ttl_ms = 5000
```

* When enabled, `tx` runs `pa list --json`, turns each prompt into a virtual profile `pa/<name>` with `pre=["assemble"]`.
* If pa is missing or `list` fails, `tx` hides pa items and shows a small header notice.

---

## Optional Features

Enable the `self-update` cargo feature to build the `tx self-update` command:

```bash
cargo install --path . --features self-update
```

---

## Data locations (XDG)

* Config: `${XDG_CONFIG_HOME:-~/.config}/tx/`
* Database: `${XDG_DATA_HOME:-~/.local/share}/tx/tx.sqlite3`
* Cache: `${XDG_CACHE_HOME:-~/.cache}/tx/`

---

## Export

```bash
tx export <session-id>   # prints Markdown to stdout
```

---

## Safety

* Resuming a session always uses its original provider; forcing a different provider fails with exit code 2.
* `--dry-run` prints the final command or wrapper (after substitutions) and exits 0.
* For `shell=true` wrappers, `{{CMD}}` is safely single-quoted; embedded single quotes are escaped.

---

## License

MIT

---

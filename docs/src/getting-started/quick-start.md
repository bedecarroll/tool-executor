# Quick Start

Follow this checklist to boot a useful environment.

1. Create the storage directories tx expects:

   ```bash
   mkdir -p ~/.config/tx/conf.d ~/.local/share/tx ~/.cache/tx
   ```

2. Drop a minimal provider configuration in the first drop-in file:

   ```toml
   # ~/.config/tx/conf.d/00-providers.toml
   [providers.codex]
   bin = "codex"
   flags = ["--search"]
   env = ["CODEX_TOKEN=${env:CODEX_TOKEN}"]
   stdin_mode = "pipe"
   ```

3. Optionally register a profile that chains snippets or wrappers:

   ```toml
   # ~/.config/tx/conf.d/10-profiles.toml
   [profiles.kickoff]
   provider = "codex"
   description = "Start a fresh Codex session"
   pre = []
   post = []
   wrap = "tmux_simple"

   [wrappers.tmux_simple]
   shell = true
   cmd = "tmux new -s tx-{{session.id}} '{{CMD}}'"
   ```

4. Launch `tx` in the terminal. Use `Tab` to insert the assembled command into your shell, press `Enter` to execute it immediately, `Ctrl+Y` to print the selected session ID, or `Ctrl+E` to export the session transcript without leaving the UI.

At this point the TUI lists recent sessions on the left and configured profiles on the right. Select an item, review the preview pane, then run it.

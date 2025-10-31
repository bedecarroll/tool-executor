# Using the TUI

Launch `tx` with no arguments to open the terminal UI. The layout includes:

- **Session list** on the left with your most recent conversations.
- **Profiles pane** on the right listing saved configurations and virtual entries (such as prompt-assembler prompts).
- **Preview area** beneath the lists that shows the assembled pipeline, recent transcript highlights, or provider descriptions.

Keyboard highlights:

- `↑` / `↓` or `j` / `k` move through the active list.
- `PgUp` / `PgDn` jump roughly ten entries at a time.
- Typing letters, numbers, or punctuation filters the list; use `Backspace` to edit the filter.
- `Tab` switches focus between sessions and profiles and emits the assembled command to stdout.
- `Ctrl+Tab` performs the same emit action for terminals that forward the modifier.
- `Enter` runs the selected entry immediately.
- `Ctrl+F` toggles between prompt search and full-text search.
- `Ctrl+P` cycles the provider filter.
- `Ctrl+Y` prints the highlighted session ID to stdout and exits the TUI.
- `Ctrl+E` exports the highlighted session transcript (matching `tx export`) and exits the TUI.
- `Esc` backs out of filter overlays or closes the TUI entirely.

The footer displays diagnostics such as hidden providers or stale configuration. Increase verbosity with `-v` or `-vv` when launching `tx` if you want extra logging while you explore the UI.

# Using the TUI

Launch `tx` with no arguments to open the terminal UI. The layout includes:

- **Session list** on the left with your most recent conversations.
- **Profiles pane** on the right listing saved configurations and virtual entries (such as prompt-assembler prompts).
- **Preview area** beneath the lists that shows the assembled pipeline, recent transcript highlights, or provider descriptions.

Keyboard highlights:

- `↑` / `↓` or `j` / `k` move through the active list.
- `Tab` switches focus between sessions and profiles.
- `Enter` runs the selected entry immediately.
- `Tab` emits the assembled command to stdout so your shell can capture it.
- `R` refreshes metadata (for example to re-run prompt-assembler discovery).
- `/` opens search mode; type to filter, `Esc` to close.

The footer displays diagnostics such as hidden providers or stale configuration. Increase verbosity with `-v` or `-vv` when launching `tx` if you want extra logging while you explore the UI.

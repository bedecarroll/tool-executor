# Session Lifecycle

tx keeps sessions lightweight so you can jump between experiments quickly.

- **Start a new session** by choosing a profile or provider from the TUI. `tx` records metadata immediately so the entry appears in recent sessions.
- **Resume** a session by selecting it in the list or running `tx resume <session-id>`. Press `Ctrl+Y` in the TUI to print the highlighted session ID for copy/paste. The original provider, snippets, and wrappers are reused to avoid surprises.
- **Export** transcripts with `tx export <session-id>`, or press `Ctrl+E` in the TUI to stream the same export to stdout without leaving the UI. The output is plain text so you can archive it or share context with collaborators.
- **Archive** sessions by removing or moving the log files outside the tracked directories. They disappear from the default listing but remain searchable if the index still references them.

Each session stores its configuration snapshot. That means later configuration changes do not retroactively modify old runs; you stay reproducible even when options evolve.

# Terminology

| Term | Meaning |
| --- | --- |
| **Provider** | External AI assistant executable that tx launches. |
| **Profile** | Named bundle of a provider, snippets, and optional wrapper. Appears in the TUI profiles pane. |
| **Snippet** | Shell command that runs before or after the provider to prepare or post-process context. |
| **Wrapper** | Process that surrounds the provider command (tmux, docker, etc.). |
| **Session** | A recorded conversation with metadata, visible in the TUI and via `tx search`. |
| **Virtual profile** | Profile generated dynamically from prompt-assembler. |
| **Pipeline** | The assembled flow of snippets, provider, and wrapper that tx executes. |

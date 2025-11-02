# Profiles

Profiles bundle providers, snippets, and wrappers into a menu entry. They appear under `[profiles.<name>]`.

```toml
[profiles.kickoff]
provider = "codex"
description = "Start a new Codex session"
pre = ["refresh_context"]
post = ["archive"]
wrap = "tmux"
prompt_assembler = "troubleshooting"
prompt_assembler_args = ["--limit", "5"]
```

Fields:

- `provider` (required): references a provider key.
- `description`: short label shown in the TUI preview.
- `pre` / `post`: arrays of snippet names.
- `wrap`: wrapper name.
- `prompt_assembler`: optional prompt to render with the `pa` binary before the wrapper or provider starts (requires `[features.pa]`). tx asks for missing positional arguments up front and reuses the assembled text when it launches the pipeline.
- `prompt_assembler_args`: optional array of additional arguments forwarded to the helper. Useful when a prompt expects fixed positional values such as `--limit 5`.

Profiles can represent common workflows (bug triage, onboarding, runbooks) without duplicating configuration. Pair them with prompt-assembler integration to surface dynamic prompts alongside static entries.

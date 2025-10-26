# Profiles

Profiles bundle providers, snippets, and wrappers into a menu entry. They appear under `[profiles.<name>]`.

```toml
[profiles.kickoff]
provider = "codex"
description = "Start a new Codex session"
pre = ["refresh_context"]
post = ["archive"]
wrap = "tmux"
```

Fields:

- `provider` (required): references a provider key.
- `description`: short label shown in the TUI preview.
- `pre` / `post`: arrays of snippet names.
- `wrap`: wrapper name.

Profiles can represent common workflows (bug triage, onboarding, runbooks) without duplicating configuration. Pair them with prompt-assembler integration to surface dynamic prompts alongside static entries.

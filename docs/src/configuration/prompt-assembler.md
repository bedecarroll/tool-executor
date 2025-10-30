# Prompt Assembler Integration

Enable prompt-assembler support to expose reusable prompt recipes as virtual profiles.

## Requirements

- `prompt-assembler` (the `pa` CLI) must be on `PATH`.
- tx v0.1.0 or newer.
- Configuration managed under the usual directories (`~/.config/tx` by default).

If `pa` returns an error or invalid JSON, tx hides the virtual profiles and prints a notice in the TUI footer.

## Configuration

Create a drop-in file such as `~/.config/tx/conf.d/20-pa.toml`:

```toml
[features.pa]
enabled = true
namespace = "pa"
```

- `namespace` controls the prefix shown in the TUI (for example `pa/bootstrap`).
- Restart tx to refresh the list; the TUI re-runs `pa list --json` when it launches.

## Using Virtual Profiles

When the feature is active:

- Virtual entries appear in the profiles pane alongside regular profiles.
- The preview pane shows the prompt description emitted by `pa`.
- `Enter` launches the pipeline immediately; `Tab` prints it for shell reuse.

Behind the scenes tx inserts `pa <prompt>` as a pre snippet before your provider. Combine the feature with `stdin_mode = "capture-arg"` to pass assembled prompts as arguments:

```toml
[providers.codex]
bin = "codex"
flags = ["--search"]
stdin_mode = "capture-arg"
stdin_to = "codex:{prompt}"
```

With that configuration, selecting `pa/hello` captures the generator output and invokes the provider with `codex --search "<prompt-text>"`.

## Troubleshooting

- Run `tx doctor` to confirm `pa` is discoverable.
- Increase logging with `RUST_LOG=tx=debug tx` if profiles disappear.
- Restart tx after changing prompts in the `prompt-assembler` repository.

Known limitation: only the prompt name, description, and tags are captured today. Future releases may forward richer metadata.

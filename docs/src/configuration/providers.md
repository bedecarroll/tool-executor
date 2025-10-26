# Providers

Providers describe how tx should invoke an external AI assistant. Each provider entry lives under `[providers.<name>]` and requires at least a `bin` key.

```toml
[providers.codex]
bin = "codex"
flags = ["--search"]
env = ["CODEX_TOKEN=${env:CODEX_TOKEN}"]
stdin_mode = "pipe"
```

Key fields:

- `bin`: executable name or absolute path.
- `flags`: default arguments passed to the provider.
- `env`: environment entries formatted as `KEY=value`. Use `${env:VAR}` to interpolate environment variables at runtime.
- `stdin_mode`: choose how stdin flows to the provider. `pipe` (default) streams data directly; `capture-arg` collects stdin and passes it as a positional argument.
- `stdin_to`: set when `stdin_mode = "capture-arg"` to describe how the captured text should be substituted into the argument list. Include `"{prompt}"` to position the captured text.

Keep provider definitions small and descriptive. If a backend exposes many toggles, prefer encoding the common ones in `flags` and exposing the rest as profile-level options so users can switch between variants.

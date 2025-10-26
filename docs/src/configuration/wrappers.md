# Wrappers

Wrappers launch providers inside another process. They live under `[wrappers.<name>]` and must declare a `cmd`.

```toml
[wrappers.tmux]
shell = true
cmd = "tmux new -s tx-{{session.id}} '{{CMD}}'"
```

Options:

- `shell`: when `true`, run the command via `/bin/sh -c`. Leave it unset (or `false`) to provide an argv array instead.
- `cmd`: shell string or array describing the wrapper invocation. The token `{{CMD}}` expands to the provider command after snippets are applied.

Use wrappers for tmux sessions, nohup/detached runs, or containerized backends. Wrappers stack with snippets, so you can prepare files, launch the wrapper, then process results without leaving the TOML layer.

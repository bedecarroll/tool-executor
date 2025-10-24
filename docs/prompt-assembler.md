# Prompt Assembler Guide

The prompt-assembler integration lets `tx` discover reusable prompt recipes
from the `pa` CLI and surface them as launch-ready virtual profiles. This guide
explains how to turn the feature on, what to expect inside the TUI, and how you
can extend it for your own workflows.

## What You Need

- A working copy of the [`prompt-assembler`](https://github.com/bedecarroll/prompt-assembler)
  tool on your `PATH`. The integration shells out to `pa list --json`, so the
  binary must be discoverable without extra wrapping.
- `tx` v0.1.0 or newer.
- Configuration managed through the usual locations (`~/.config/tx/` for most
  setups).

If `pa` returns an error or prints invalid JSON, `tx` will quietly hide the
virtual profiles and show a notice in the TUI footer.

## Turn It On

Create a drop-in such as `~/.config/tx/conf.d/20-pa.toml`:

```toml
[features.pa]
enabled = true
namespace = "pa"
```

- `namespace` determines the profile name prefix. With the default above, a
  prompt called `bootstrap` appears as `pa/bootstrap` in the TUI.
- `tx` always invokes `pa list --json` when it needs fresh data; press `R` in
  the TUI to re-run the command on demand.

Restart `tx` (or hit `R` inside the TUI) after enabling the feature.

## What You See in the TUI

When the feature is active:

- Virtual profiles appear in the list alongside your saved sessions. They are
  sorted under a “Profiles” header and tagged with their `namespace/name`.
- Selecting a virtual profile populates the preview pane with the description
  or summary exposed by `pa`.
- Press `Enter` to launch immediately or `Tab` to print the assembled command to
  stdout (just like any other profile).

Behind the scenes `tx` inserts an inline `pa <prompt>` stage before your
provider. No additional snippets are required—the TUI selection is all you
need. The pipeline that launches looks like this:

```text
stdin -> pa <prompt-name> | <provider pipeline...>
```

### Delivering the assembled prompt to your provider

Most providers expect text via arguments rather than stdin. Configure your
provider once and `tx` will pass the PA output automatically:

```toml
[providers.codex]
bin = "codex"
flags = ["--search"]
stdin_mode = "capture-arg"
stdin_to = "codex:{prompt}"
```

With that in place, selecting `pa/hello` produces an internal command like:

```bash
tx internal capture-arg --provider codex --bin codex \
  --pre 'pa hello' \
  --arg --search --arg '{prompt}'
```

`tx` runs the `pa hello` pipeline, captures its stdout, and substitutes it for
`{prompt}` so the provider launches as `codex --search "<PA output>"`.

### Manual CLI launches

Prefer to stay in the shell? Assemble the prompt first, then hand it to `tx`:

```bash
pa support-ticket | tx launch codex
```

If the prompt expects additional arguments, pass them straight to `pa` before
piping:

```bash
pa support-ticket abc-123 | tx launch codex
```

`pa` still receives stdin as `{0}` (the text you piped in), followed by any
positional arguments you supplied. If you forget a required value, `pa` exits
non-zero and `tx` surfaces its error output so you can fix the command.

## Cool Things to Try

- **Prompt catalogs for teams:** Share a `prompt-assembler` repo that lists
  onboarding flows (`pa/onboarding`), code review checklists, or bug triage
  scripts. Everyone gets the same menu in `tx` without touching local config.
- **Environment-aware prompts:** Have `pa` emit different defaults depending on
  the current directory or Git remotes. Combine this with wrapper snippets that
  set environment variables before launching your provider.
- **One-shot runbooks:** Use `pa` entries to describe emergency procedures.
  Launching the virtual profile preps context, sets log capture, and opens the
  right dashboard with a single keypress inside `tx`.
- **Workspace scaffolding:** Pair `pa` with a post-snippet that writes files or
  records metadata so every session starts with a structured brief.

## Troubleshooting

- Run `tx doctor` to confirm `pa` is discoverable; the diagnostics print whether
  the binary responded.
- Press `R` at any time to re-run `pa list --json` and refresh the list.
- Enable verbose logging (`RUST_LOG=tx=debug tx`) to inspect the `pa list`
  payload if profiles are missing.
- Restart `tx` after changing prompts in `prompt-assembler` if you are not in
  the TUI.

## Known Limitations

- Only the prompt name, description, and tags are captured from `pa list`. The
  integration does not yet forward rich prompt payloads into the pipeline, so
  snippets must fetch additional data on demand.
- Virtual profiles currently inherit the default provider (`[defaults] provider`
  or the first configured provider). Set that default explicitly if you want
  PA entries to always launch with a specific backend.

Feedback and contributions are welcome—open an issue or PR if you need more
flexibility from the integration.

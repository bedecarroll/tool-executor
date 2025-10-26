# Pipelines and Wrappers

Every tx launch assembles a pipeline:

```text
stdin -> <pre snippets> -> provider -> <post snippets>
```

Snippets run as shell commands. Define them under `[snippets.pre]` or `[snippets.post]` in configuration and refer to them by name. Use pre snippets to stage prompts, template files, or environment setup; use post snippets to collect output, sync logs, or notify teammates.

Wrappers enclose the provider inside another process. Common patterns include running providers inside tmux sessions, Docker containers, or detached shells. A wrapper declares whether it should be invoked via `/bin/sh -c` (`shell = true`) and describes the command or argv array to execute. The placeholder `{{CMD}}` expands to the final provider command.

Combining snippets and wrappers lets you build repeatable, sharable workflows without shell scripts. Change the configuration once and the TUI, CLI, and exported commands follow suit.

# Snippets

Snippets are reusable shell commands that run before or after the provider. Declare them in two tables:

```toml
[snippets.pre]
refresh_context = "scripts/generate-context"
format_prompt = "prompt-tool --file prompt.md"

[snippets.post]
archive = "scripts/archive-session {{session.id}}"
```

Reference snippets by name in profiles. tx executes pre snippets in declaration order before the provider starts, piping stdin through each command. Post snippets run after the provider exits and receive the provider's stdout via stdin.

Snippets support the same templating tokens as wrappers, including `{{session.id}}`, `{{var:KEY}}`, and environment lookups. Use them to stitch together existing tooling without modifying tx itself.

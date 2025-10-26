# Documentation Workflow

The documentation lives in an mdBook located under `docs/`. Follow this loop to keep it healthy:

1. Edit chapters under `docs/src/`. Update `SUMMARY.md` whenever you add, remove, or move a page so the navigation stays accurate.
2. Run `mdbook build docs` to generate the HTML output locally. The build directory defaults to `docs/build/`.
3. Validate code samples with `mdbook test docs`. It compiles Rust snippets and catches copy-and-paste errors early.
4. Lint Markdown links and anchors with `mdbook-lint` or `mdbook-linkcheck`. Install them via `cargo install mdbook-lint mdbook-linkcheck` and wire into CI.
5. Commit both content and configuration (`docs/book.toml`, `docs/src/SUMMARY.md`). Avoid committing the generated `build/` output.

When writing pages:

- Prefer short headings and consistent hierarchy to keep the sidebar readable. Break long procedures into numbered lists.
- Use fenced code blocks (` ```bash `, ` ```toml `) so syntax highlighting works in the rendered book.
- Hide boilerplate lines with mdBook's extension syntax (`#`-prefixed hidden lines, `// fn main() { # }`, etc.) when the snippet needs to compile but should stay focused on the key lines.
- Test external links periodically; mdBook's link checker helps catch outdated URLs during CI runs.

For automated environments, integrate the following into `mise` or CI job steps:

```bash
mdbook build docs
mdbook test docs
mdbook lint docs       # provided by mdbook-lint
```

Running these alongside `mise run fmt`, `mise run lint`, and `mise run test` keeps code and docs aligned.

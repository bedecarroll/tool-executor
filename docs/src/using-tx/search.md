# Session Search

`tx search` surfaces the same data that powers the TUI, but in a script-friendly format. By default it lists recent sessions. Add arguments to narrow the results:

- `tx search asset` performs a prompt-only (first user message) search for `asset`.
- `tx search context --full-text` scans the entire transcript with the full-text index.
- `tx search context --full-text --role assistant` limits hits to the assistant replies.

The JSON output includes the snippet that matched, the role (`user` or `assistant`), and `last_active` timestamps. Use it to feed dashboards, quick filters, or shell pipelines.

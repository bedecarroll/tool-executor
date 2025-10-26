# Automation and Scripting

tx offers dedicated CLI commands for non-interactive workflows:

- `tx search` + `--full-text` exposes JSON suitable for quick filters or dashboards.
- `tx resume <session-id>` relaunches an existing session with its original configuration.
- `tx export <session-id>` prints transcripts for archiving or sharing.
- `tx config list|dump|where|lint` inspects configuration state in batch jobs.

Combine them with shell tools to build automation. Examples:

```bash
# Resume the most recent session tagged "review"
tx search review --full-text --role user \
  | jq -r 'sort_by(-.last_active)[0].session_id' \
  | xargs tx resume

# Export the most recent assistant reply for quick sharing
tx search --full-text --role assistant \
  | jq -r 'sort_by(-.last_active)[0].snippet' \
  | tee /tmp/tx-latest.txt
```

When you need structured pipelines, rely on snippets and wrappers instead of bespoke scripts. Record the behaviour in configuration so other users receive the same automation by default.

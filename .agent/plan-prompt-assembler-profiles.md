# Purpose

We want people to launch tx profiles that front-load a prompt from prompt-assembler (pa) while still honoring the profile’s configured wrapper, snippets, and other options. Today pa entries only exist as “virtual profiles” that ignore wrappers, so we built a troubleshooting profile with an inline `printf` snippet instead of reusing our pa prompt. This plan adds first-class support for a `prompt_assembler` key on regular profiles so selecting that profile runs `pa namespace/prompt` automatically and still routes through the profile’s wrapper. A user can set `prompt_assembler = "troubleshooting"` alongside `wrap = "troubleshooting_tmux"`, launch tx, choose that profile, and see the tmux layout plus the pa-generated prompt without extra warnings.

## Progress

- [x] (2025-11-02 01:32Z) Documented the existing profile/wrapper flow, prompt-assembler virtual profiles, and capture handling in `pipeline.rs`.
- [x] (2025-11-02 01:45Z) Extended the configuration model, schema asset, and docs to accept `prompt_assembler` on profiles.
- [x] (2025-11-02 01:55Z) Updated TUI profile loading to inject `pa <name>` inline pre-commands for config profiles with `prompt_assembler`.
- [x] (2025-11-02 02:05Z) Confirmed pipeline planning keeps `capture_has_pre_commands` true when inline pre commands exist and propagates them through `App::resume`.
- [x] (2025-11-02 02:15Z) Added schema, TUI, and helper tests covering the new field plus wrapper preservation.
- [x] (2025-11-02 02:25Z) Ran `mise run fmt`, `mise run lint`, `mise run test`; manual TUI validation remains for the user to perform with their prompts.
- [x] (2025-11-02 02:25Z) Recorded surprises, decisions, and outcomes in this plan.

## Surprises & Discoveries

- Observation: Resume flows that overlay a profile ignored prompt-assembler commands until we explicitly threaded inline pre-commands through `App::resume`.
  Evidence: Added `shell_escape` logic in `App::resume` and verified with the new configuration test suite.

- Observation: Markdown linting flagged the initial ExecPlan layout because multiple `#` headings violated MD025.
  Evidence: `mise run lint` pointed to `.agent/plan-prompt-assembler-profiles.md` before demoting headings.

- Observation: Unit tests require a stub `pa` binary so prompt validation can run without the real tool.
  Evidence: Added temporary shell scripts in `src/tui/mod.rs` tests and confirmed they supply both `pa list --json` and `pa show --json` responses.

## Decision Log

- Decision: Invoke the `pa` binary exactly with the configured prompt string (escaping via `shell_escape`) rather than auto-prefixing the namespace.
  Rationale: Matches existing virtual profile behavior and lets advanced users pass fully-qualified prompt names.
  Date/Author: 2025-11-02 / bc-ai

- Decision: Propagate prompt-assembler inline commands through `App::resume` so CLI resumes with a profile behave like TUI launches.
  Rationale: Keeps parity between UI and CLI paths and prevents regressions when resuming troubleshooting sessions.
  Date/Author: 2025-11-02 / bc-ai

- Decision: Fail fast when a profile references an unknown prompt by checking availability before executing the pipeline (both in the TUI plan builder and in CLI resume).
  Rationale: Avoids dropping users into `internal capture-arg` with opaque shell errors and surfaces actionable diagnostics instead.
  Date/Author: 2025-11-02 / bc-ai

## Outcomes & Retrospective

Profiles now support a `prompt_assembler` key that runs `pa <prompt>` before the provider while preserving wrappers and snippets. The TUI displays the new inline command, pipeline capture stays quiet (no redundant prompt warning), and resume flows inherit the same behavior. Tx now validates prompt existence up front, returning a clear error when a profile references a missing entry instead of launching `internal capture-arg` blindly. Automated formatting, linting, and the full nextest suite pass, and the troubleshooting profile in `~/.config/tx/config.toml` now uses the new knob rather than a helper snippet.

## Context and Orientation

Profiles live in `src/config/model.rs` as `ProfileConfig`. They currently expose `provider`, `pre`, `post`, and `wrap`. Prompt-assembler integration is handled in `src/prompts.rs`; it loads prompts via the `pa` binary and surfaces them in the TUI as “virtual profiles” with `inline_pre = vec!["pa <name>"]`. When the TUI (`src/tui/mod.rs`) builds its profile list it sets `inline_pre` to empty for config-based profiles, so wrappers work but there is no way to inject a pa command. Pipeline planning (`src/pipeline.rs`) merges profile snippets and inline pre commands, and the newly introduced `capture_has_pre_commands` flag suppresses the capture warning when `pre_commands` is not empty. We will extend the config to carry an optional pa prompt name and have the TUI give config profiles the same inline pre behavior as virtual ones. We also need to update docs (`docs/src/configuration/*.md`, `docs/src/assets/config-schema.json`) and tests (`tests/config_schema.rs`, relevant Rust unit tests).

## Plan of Work

First, modify the configuration model: add an optional `prompt_assembler` string to `RawProfile` and `ProfileConfig`, plus serde support, schema enumeration, and docs. Keep validation simple: accept non-empty strings and trim whitespace. Next, when building the profile list in `AppState::new` (look for where `ProfileEntry` values are pushed for config profiles), detect `prompt_assembler`, populate `inline_pre` with a single `pa` command, and consider setting `stdin_supported` false so we do not collect user input. Ensure the namespace prefix is respected: if `features.pa.namespace = "pa"` and the profile supplies `bootstrap`, we should call `pa bootstrap`; if callers specify an absolute command like `foo/bar`, use it verbatim. After that, confirm `PipelinePlan::capture_has_pre_commands` already accounts for inline pre commands; if not, adjust the computation to look at `request.inline_pre`. Add coverage: extend `tests/config_schema.rs` to assert the schema mentions the new key, create a unit test in `src/tui/mod.rs` proving that a config profile with `prompt_assembler` produces a plan whose pipeline includes `pa <prompt>` and that wrappers remain attached, and verify the capture warning helper stays quiet (existing test may need tweaks). Finally, update user docs with a short example showing `prompt_assembler` on a profile and mention it in `docs/src/configuration/reference.md`.

## Concrete Steps

Work in `/home/bc/code/tool-executor`:

    rg "ProfileConfig"
    rg "inline_pre" src/tui/mod.rs
    mise run fmt
    mise run lint
    mise run test
    tx  # manual validation with troubleshooting profile

Show relevant snippets in the Artifacts section once they exist.

## Validation and Acceptance

After implementation, launch `tx`, select the troubleshooting profile that sets `prompt_assembler`, and verify the tmux wrapper opens with the pa-generated prompt without printing the manual capture warning. Run `mise run fmt`, `mise run lint`, and `mise run test`; expect all tasks to pass. Add an automated test such as `tui::tests::plan_for_profile_uses_prompt_assembler` that fails before the change and passes afterward, demonstrating the pipeline contains the pa command and that the wrapper name is preserved.

## Idempotence and Recovery

Configuration parsing and TUI initialization are deterministic. Editing the config schema and code is safe to repeat; rerunning the validation commands restores a clean state. If a change breaks parsing, revert the relevant file and re-run `cargo fmt` followed by the test suite.

## Artifacts and Notes

Key references:

- `src/config/model.rs`: `ProfileConfig` gains `prompt_assembler` with trimming and schema metadata.
- `src/tui/mod.rs`: config profiles now fill `inline_pre` with `pa <prompt>` and the new test `plan_for_profile_injects_prompt_assembler` exercises wrapper preservation.
- `tests/config_schema.rs`: schema assertions include the new property.
- `src/app.rs`: `App::resume` forwards prompt-assembler inline commands to maintain parity with the TUI.

## Interfaces and Dependencies

Introduce `prompt_assembler: Option<String>` on `crate::config::model::ProfileConfig` and treat it like other optional profile attributes. Update the JSON schema (`docs/src/assets/config-schema.json`) so `prompt_assembler` is recognized and described. Ensure `AppState::new` in `src/tui/mod.rs` populates `ProfileEntry.inline_pre` with `pa <prompt>` when this field is set, and keep wrappers intact by leaving `ProfileEntry.wrap` unchanged. Have `App::resume` mirror the same inline pre behavior so CLI resumes stay in sync with the TUI path. The helper that determines capture warnings must continue to read `PipelinePlan.capture_has_pre_commands`, so confirm inline pre commands set that flag.

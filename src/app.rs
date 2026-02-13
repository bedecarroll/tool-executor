use std::collections::HashMap;
use std::ffi::OsString;
use std::io::{self, BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use color_eyre::Result;
use color_eyre::eyre::{WrapErr, eyre};
use regex::Regex;
use serde_json::json;
use std::sync::LazyLock;
use tracing::debug;
use which::which;

use crate::cli::{
    Cli, ConfigCommand, ConfigDefaultCommand, ConfigSchemaCommand, ExportCommand,
    InternalPromptAssemblerCommand, ResumeCommand, SearchCommand, SelfUpdateCommand, StatsCommand,
};
use crate::commands::stats;
use crate::config::model::{DiagnosticLevel, PromptAssemblerConfig};
use crate::config::{ConfigSourceKind, LoadedConfig};
use crate::db::Database;
use crate::indexer::{IndexError, IndexReport, Indexer};
use crate::internal::assemble_prompt;
use crate::pipeline::{
    Invocation, PipelinePlan, PipelineRequest, PromptInvocation, SessionContext, build_pipeline,
};
use crate::prompts::{PromptAssembler, PromptStatus};
use crate::providers;
use crate::session::{SearchHit, SessionSummary, Transcript};
use crate::tui;
use crate::util;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("provider mismatch: session uses '{expected}' but pipeline asked for '{actual}'")]
    ProviderMismatch { expected: String, actual: String },
}

pub struct App<'cli> {
    pub cli: &'cli Cli,
    pub loaded: LoadedConfig,
    pub db: Database,
    pub prompt: Option<PromptAssembler>,
}

pub struct UiContext<'app> {
    pub config: &'app crate::config::model::Config,
    pub directories: &'app crate::config::AppDirectories,
    pub db: &'app mut Database,
    pub prompt: Option<&'app mut PromptAssembler>,
}

impl<'cli> App<'cli> {
    /// Construct the application, loading configuration and initializing the database.
    ///
    /// # Errors
    ///
    /// Returns an error if configuration files cannot be read or the `SQLite` database
    /// cannot be opened and prepared.
    pub fn bootstrap(cli: &'cli Cli) -> Result<Self> {
        let loaded = crate::config::load(cli.config_dir.as_deref())?;
        let db_path = loaded.directories.data_dir.join("tx.sqlite3");
        let mut db = Database::open(&db_path)?;

        if std::env::var_os("TX_SKIP_INDEX").is_none() {
            let mut indexer = Indexer::new(&mut db, &loaded.config);
            let index_report = indexer.run()?;
            log_index_report(&index_report);
        } else {
            tracing::debug!("skipping indexer run due to TX_SKIP_INDEX");
        }

        let prompt = loaded
            .config
            .features
            .prompt_assembler
            .clone()
            .map(PromptAssembler::new);

        Ok(Self {
            cli,
            loaded,
            db,
            prompt,
        })
    }

    /// Execute a sessions search (first prompt or full-text) depending on flags.
    ///
    /// # Errors
    ///
    /// Returns an error if the database search fails or the results cannot be
    /// serialized to JSON.
    pub fn search(&self, cmd: &SearchCommand) -> Result<()> {
        let since_epoch = cmd
            .since
            .map(|seconds| util::unix_timestamp().saturating_sub(seconds));
        let term = cmd
            .term
            .as_deref()
            .map(str::trim)
            .filter(|term| !term.is_empty());

        if cmd.role.is_some() && (!cmd.full_text || term.is_none()) {
            return Err(eyre!(
                "--role requires --full-text and a non-empty search term"
            ));
        }

        if term.is_none() {
            let sessions =
                self.db
                    .list_sessions(cmd.provider.as_deref(), true, since_epoch, cmd.limit)?;

            let mut payload = Vec::new();
            for session in &sessions {
                if let Some(summary) = self.db.session_summary(&session.id)? {
                    payload.push(summary_to_json(
                        &summary,
                        summary.first_prompt.as_deref(),
                        None,
                    ));
                }
            }

            println!("{}", serde_json::to_string_pretty(&payload)?);
            return Ok(());
        }

        let term = term.unwrap();
        let hits = if cmd.full_text {
            self.db
                .search_full_text(term, cmd.provider.as_deref(), false)?
        } else {
            self.db
                .search_first_prompt(term, cmd.provider.as_deref(), false)?
        };

        let role_filter = cmd.role.as_deref();

        let detailed =
            self.collate_search_results_for_command(hits, since_epoch, role_filter, cmd.limit)?;

        let payload: Vec<_> = detailed
            .iter()
            .map(|(hit, summary)| {
                let snippet = hit.snippet.as_deref().or(summary.first_prompt.as_deref());
                let snippet_role = hit.role.as_deref();
                summary_to_json(summary, snippet, snippet_role)
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&payload)?);
        Ok(())
    }

    fn collate_search_results<F>(
        hits: Vec<SearchHit>,
        since_epoch: Option<i64>,
        role_filter: Option<&str>,
        limit: Option<usize>,
        mut summary_lookup: F,
    ) -> Result<Vec<(SearchHit, SessionSummary)>>
    where
        F: FnMut(&str) -> Result<Option<SessionSummary>>,
    {
        let mut detailed = Vec::new();
        for hit in hits {
            if let Some(filter) = role_filter
                && !hit
                    .role
                    .as_deref()
                    .is_some_and(|role| role.eq_ignore_ascii_case(filter))
            {
                continue;
            }
            let Some(summary) = summary_lookup(&hit.session_id)? else {
                continue;
            };
            if let Some(cutoff) = since_epoch
                && summary.last_active.is_some_and(|last| last < cutoff)
            {
                continue;
            }
            detailed.push((hit, summary));
        }

        if let Some(limit) = limit
            && detailed.len() > limit
        {
            detailed.truncate(limit);
        }

        Ok(detailed)
    }

    fn collate_search_results_for_command(
        &self,
        hits: Vec<SearchHit>,
        since_epoch: Option<i64>,
        role_filter: Option<&str>,
        limit: Option<usize>,
    ) -> Result<Vec<(SearchHit, SessionSummary)>> {
        Self::collate_search_results(hits, since_epoch, role_filter, limit, |session_id| {
            self.db.session_summary(session_id)
        })
    }

    /// Resolve a session summary from an explicit identifier or `last`.
    ///
    /// # Errors
    ///
    /// Returns an error when no matching session can be found.
    fn resolve_resume_summary(&self, session_id: &str) -> Result<SessionSummary> {
        if let Some(summary) = self.db.session_summary_for_identifier(session_id)? {
            return Ok(summary);
        }
        if session_id.eq_ignore_ascii_case("last") {
            return self
                .db
                .latest_actionable_session()?
                .ok_or_else(|| eyre!("no previous sessions available to resume"));
        }

        Err(eyre!("session '{}' not found", session_id))
    }

    fn resolve_resume_profile(
        &mut self,
        profile_name: Option<&str>,
        summary_provider: &str,
    ) -> Result<(Option<PromptInvocation>, bool)> {
        let Some(profile_name) = profile_name else {
            return Ok((None, false));
        };
        let profile = self
            .loaded
            .config
            .profiles
            .get(profile_name)
            .ok_or_else(|| eyre!("profile '{}' not found", profile_name))?;
        if profile.provider != summary_provider {
            return Err(AppError::ProviderMismatch {
                expected: summary_provider.to_string(),
                actual: profile.provider.clone(),
            }
            .into());
        }
        let prompt_invocation = profile
            .prompt_assembler
            .as_ref()
            .map(|prompt| PromptInvocation {
                name: prompt.clone(),
                args: profile.prompt_assembler_args.clone(),
            });
        let has_pre_snippets = !profile.pre.is_empty();
        if let Some(invocation) = prompt_invocation.as_ref() {
            self.ensure_prompt_available(&invocation.name)?;
        }

        Ok((prompt_invocation, has_pre_snippets))
    }

    /// Build and optionally execute a pipeline to resume an existing session.
    ///
    /// # Errors
    ///
    /// Returns an error when the session cannot be loaded, the requested profile is
    /// incompatible, the pipeline cannot be constructed, or execution fails.
    pub fn resume(&mut self, cmd: &ResumeCommand) -> Result<()> {
        let summary = self.resolve_resume_summary(&cmd.session_id)?;

        let (prompt_invocation, profile_has_pre_snippets) =
            self.resolve_resume_profile(cmd.profile.as_deref(), &summary.provider)?;

        let vars = parse_vars(&cmd.vars)?;
        let working_dir = summary.path.parent().map_or_else(
            || std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            Path::to_path_buf,
        );

        let resume_plan = providers::resume_info(&summary)?;
        let mut provider_args = Vec::new();
        let mut resume_token = None;
        if let Some(mut plan) = resume_plan {
            resume_token = plan.resume_token.take();
            provider_args.extend(plan.args);
        }
        provider_args.extend(cmd.provider_args.clone());

        let capture_prompt = should_capture_prompt_for_resume(
            prompt_invocation.as_ref(),
            profile_has_pre_snippets,
            &cmd.pre_snippets,
        );

        let request = PipelineRequest {
            config: &self.loaded.config,
            provider_hint: Some(summary.provider.as_str()),
            profile: cmd.profile.as_deref(),
            additional_pre: cmd.pre_snippets.clone(),
            additional_post: cmd.post_snippets.clone(),
            inline_pre: Vec::new(),
            wrap: cmd.wrap.as_deref(),
            provider_args,
            capture_prompt,
            prompt_assembler: prompt_invocation,
            vars,
            session: SessionContext {
                id: Some(summary.id.clone()),
                label: summary.label.clone(),
                path: Some(summary.path.to_string_lossy().to_string()),
                resume_token,
            },
            cwd: working_dir,
        };

        let plan = build_pipeline(&request)?;
        if cmd.emit_json && !(cmd.dry_run || cmd.emit_command) {
            return Err(eyre!("--emit-json requires --dry-run or --emit-command"));
        }

        if cmd.dry_run || cmd.emit_command {
            let mode = if cmd.emit_json {
                EmitMode::Json
            } else {
                EmitMode::Plain {
                    newline: true,
                    friendly: false,
                }
            };
            emit_command(&plan, mode)?;
            return Ok(());
        }

        execute_plan(&plan).wrap_err("failed to execute pipeline")
    }

    fn ensure_prompt_available(&mut self, prompt_name: &str) -> Result<()> {
        let status = if let Some(prompt) = self.prompt.as_mut() {
            if self.loaded.config.features.prompt_assembler.is_none() {
                return Err(eyre!(
                    "profile references prompt '{}' but prompt-assembler is disabled",
                    prompt_name
                ));
            }
            prompt.refresh(false)
        } else {
            let cfg = self
                .loaded
                .config
                .features
                .prompt_assembler
                .clone()
                .ok_or_else(|| {
                    eyre!(
                        "profile references prompt '{}' but prompt-assembler is disabled",
                        prompt_name
                    )
                })?;
            let mut prompt = PromptAssembler::new(cfg);
            let status = prompt.refresh(false);
            self.prompt = Some(prompt);
            status
        };

        match status {
            PromptStatus::Ready { profiles, .. } => {
                if profiles.iter().any(|vp| vp.name == prompt_name) {
                    Ok(())
                } else {
                    Err(eyre!("prompt assembler prompt '{}' not found", prompt_name))
                }
            }
            PromptStatus::Unavailable { message } => Err(eyre!(message)),
        }
    }

    /// Export a session transcript in human or Markdown form.
    ///
    /// # Errors
    ///
    /// Returns an error if the requested session does not exist.
    pub fn export(&self, cmd: &ExportCommand) -> Result<()> {
        let transcript = self
            .db
            .fetch_transcript(&cmd.session_id)?
            .ok_or_else(|| eyre!("session '{}' not found", cmd.session_id))?;

        export_markdown(&transcript);

        Ok(())
    }

    /// Render usage statistics for the selected provider.
    ///
    /// # Errors
    ///
    /// Returns an error if stats cannot be retrieved or rendered.
    pub fn stats(&self, cmd: &StatsCommand) -> Result<()> {
        match cmd {
            StatsCommand::Codex => {
                let db_path = self.loaded.directories.data_dir.join("tx.sqlite3");
                stats::codex(&self.db, &db_path)
            }
        }
    }

    /// Execute one of the configuration subcommands.
    ///
    /// # Errors
    ///
    /// Returns an error if dumping, linting, or other configuration operations fail.
    pub fn config(&self, cmd: &ConfigCommand) -> Result<()> {
        match cmd {
            ConfigCommand::List => {
                self.config_list();
                Ok(())
            }
            ConfigCommand::Dump => self.config_dump(),
            ConfigCommand::Where => {
                self.config_where();
                Ok(())
            }
            ConfigCommand::Lint => self.config_lint(),
            ConfigCommand::Default(cmd) => self.config_default(cmd),
            ConfigCommand::Schema(cmd) => Self::config_schema(cmd),
        }
    }

    /// Run diagnostic checks to validate binaries, directories, and database state.
    ///
    /// # Errors
    ///
    /// Returns an error if the database cannot be inspected or other IO operations fail.
    pub fn doctor(&self) -> Result<()> {
        run_doctor(&self.loaded, &self.db)
    }

    /// Attempt to update the `tx` binary using GitHub releases.
    ///
    /// # Errors
    ///
    /// Returns an error if the updater cannot be configured or the download/apply step
    /// fails.
    pub fn self_update(&self, cmd: &SelfUpdateCommand) -> Result<()> {
        let message = run_self_update_with(cmd, self.cli.quiet, apply_self_update_release)?;
        println!("{message}");
        Ok(())
    }

    /// Launch the interactive TUI for session selection or profile execution.
    ///
    /// # Errors
    ///
    /// Returns an error if the TUI cannot be displayed or if underlying database
    /// interactions fail during operation.
    pub fn run_ui(&mut self) -> Result<()> {
        let prompt = self.prompt.as_mut();
        let mut ctx = UiContext {
            config: &self.loaded.config,
            directories: &self.loaded.directories,
            db: &mut self.db,
            prompt,
        };
        tui::run(&mut ctx)
    }

    fn config_list(&self) {
        println!("Providers:");
        for (name, provider) in &self.loaded.config.providers {
            let roots = provider
                .session_roots
                .iter()
                .map(|root| root.display().to_string())
                .collect::<Vec<_>>()
                .join(", ");
            println!("  - {} (bin: {}, roots: [{}])", name, provider.bin, roots);
        }

        println!("\nWrappers:");
        for (name, wrapper) in &self.loaded.config.wrappers {
            let mode = match &wrapper.mode {
                crate::config::model::WrapperMode::Shell { .. } => "shell",
                crate::config::model::WrapperMode::Exec { .. } => "exec",
            };
            println!("  - {name} ({mode})");
        }

        println!("\nProfiles:");
        for (name, profile) in &self.loaded.config.profiles {
            println!(
                "  - {} (provider: {}, pre: [{}], post: [{}], wrap: {}, description: {})",
                name,
                profile.provider,
                profile.pre.join(", "),
                profile.post.join(", "),
                profile.wrap.as_deref().unwrap_or("-"),
                profile.description.as_deref().unwrap_or("-"),
            );
        }
    }

    /// Print the bundled default configuration to stdout.
    ///
    /// # Errors
    ///
    /// Returns an error if writing to stdout fails.
    fn config_default(&self, cmd: &ConfigDefaultCommand) -> Result<()> {
        let mut stdout = io::stdout().lock();
        write_config_default(&mut stdout, cmd, &self.loaded)
    }

    fn config_schema(cmd: &ConfigSchemaCommand) -> Result<()> {
        let mut stdout = io::stdout().lock();
        write_config_schema(&mut stdout, cmd)
    }

    fn config_dump(&self) -> Result<()> {
        let mut stdout = io::stdout().lock();
        write_config_dump(&mut stdout, &self.loaded.merged)
    }

    fn config_where(&self) {
        println!(
            "Configuration directory: {}",
            self.loaded.directories.config_dir.display()
        );
        println!(
            "Data directory: {}",
            self.loaded.directories.data_dir.display()
        );
        println!(
            "Cache directory: {}",
            self.loaded.directories.cache_dir.display()
        );
        println!("Sources (in load order):");
        for source in &self.loaded.sources {
            let kind = match source.kind {
                ConfigSourceKind::Main => "main",
                ConfigSourceKind::DropIn => "drop-in",
                ConfigSourceKind::Project => "project",
                ConfigSourceKind::ProjectDropIn => "project-drop-in",
            };
            println!("  - {} ({})", source.path.display(), kind);
        }
    }

    fn config_lint(&self) -> Result<()> {
        if self.loaded.diagnostics.is_empty() {
            println!("Configuration looks good.");
            return Ok(());
        }

        let mut has_error = false;
        for diag in &self.loaded.diagnostics {
            match diag.level {
                DiagnosticLevel::Warning => println!("warning: {}", diag.message),
                DiagnosticLevel::Error => {
                    println!("error: {}", diag.message);
                    has_error = true;
                }
            }
        }

        if has_error {
            Err(eyre!("configuration contains errors"))
        } else {
            Ok(())
        }
    }
}

fn should_capture_prompt_for_resume(
    prompt_invocation: Option<&PromptInvocation>,
    profile_has_pre_snippets: bool,
    cmd_pre_snippets: &[String],
) -> bool {
    prompt_invocation.is_some() || profile_has_pre_snippets || !cmd_pre_snippets.is_empty()
}

#[derive(Debug, Clone, Copy)]
pub enum EmitMode {
    Json,
    Plain { newline: bool, friendly: bool },
}

pub(crate) fn emit_command(plan: &PipelinePlan, mode: EmitMode) -> Result<()> {
    let mut stdout = io::stdout().lock();
    emit_command_with_writer(&mut stdout, plan, mode)
}

fn emit_command_with_writer<W: Write>(
    writer: &mut W,
    plan: &PipelinePlan,
    mode: EmitMode,
) -> Result<()> {
    match mode {
        EmitMode::Json => {
            let payload = json!({ "command": plan.display, "env": plan.env });
            let rendered = serde_json::to_string_pretty(&payload)?;
            writer.write_all(rendered.as_bytes())?;
            writer.write_all(b"\n")?;
        }
        EmitMode::Plain { newline, friendly } => {
            let command = if friendly {
                &plan.friendly_display
            } else {
                &plan.display
            };
            if newline {
                writer.write_all(command.as_bytes())?;
                writer.write_all(b"\n")?;
            } else {
                writer.write_all(command.as_bytes())?;
                writer.flush()?;
            }
        }
    }
    Ok(())
}

fn write_config_default<W: Write>(
    writer: &mut W,
    cmd: &ConfigDefaultCommand,
    loaded: &LoadedConfig,
) -> Result<()> {
    let rendered = if cmd.raw {
        crate::config::default_template().to_string()
    } else {
        crate::config::bundled_default_config(&loaded.directories)
    };
    writer.write_all(rendered.as_bytes())?;
    writer.flush()?;
    Ok(())
}

fn write_config_schema<W: Write>(writer: &mut W, cmd: &ConfigSchemaCommand) -> Result<()> {
    let schema = crate::config::schema(cmd.pretty)?;
    writer.write_all(schema.as_bytes())?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(())
}

fn write_config_dump<W: Write>(writer: &mut W, merged: &toml::Value) -> Result<()> {
    let toml_text = toml::to_string_pretty(merged)?;
    writer.write_all(toml_text.as_bytes())?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(())
}

fn emit_terminal_title_with_writer<W: Write>(
    title: &str,
    is_terminal: bool,
    writer: &mut W,
) -> Result<()> {
    if !is_terminal {
        return Ok(());
    }
    write!(writer, "\x1b]0;{title}\x07")?;
    writer.flush()?;
    Ok(())
}

#[cfg(not(coverage))]
pub(crate) fn execute_plan(plan: &PipelinePlan) -> Result<()> {
    execute_plan_with_stdin_prompt(plan, io::stdin().is_terminal(), |label| {
        let stdin = io::stdin();
        let mut handle = stdin.lock();
        prompt_for_stdin_with_reader(label, &mut handle)
    })
}

#[cfg(coverage)]
pub(crate) fn execute_plan(plan: &PipelinePlan) -> Result<()> {
    execute_plan_with_stdin_prompt(plan, io::stdin().is_terminal(), |_label| Ok(String::new()))
}

fn emit_terminal_title(title: &str) -> Result<()> {
    let stdout = io::stdout();
    let is_terminal = stdout.is_terminal();
    let mut locked = stdout.lock();
    emit_terminal_title_with_writer(title, is_terminal, &mut locked)
}

fn execute_plan_with_stdin_prompt<P>(
    plan: &PipelinePlan,
    stdin_is_terminal: bool,
    mut read_prompt: P,
) -> Result<()>
where
    P: FnMut(Option<&str>) -> Result<String>,
{
    const DEFAULT_PROMPT_LIMIT: usize = 1_048_576;
    let assembled_prompt = if let Some(invocation) = &plan.prompt_assembler {
        let cmd = InternalPromptAssemblerCommand {
            prompt: invocation.name.clone(),
            prompt_args: invocation.args.clone(),
            prompt_limit: DEFAULT_PROMPT_LIMIT,
        };
        Some(assemble_prompt(&cmd)?)
    } else {
        None
    };

    execute_plan_with_prompt(plan, stdin_is_terminal, assembled_prompt, |label| {
        read_prompt(label).map(Some)
    })
}

fn should_warn_capture(
    plan: &PipelinePlan,
    capture_input: Option<&str>,
    stdin_is_terminal: bool,
) -> bool {
    capture_input.is_none()
        && stdin_is_terminal
        && plan.uses_capture_arg
        && !plan.capture_has_pre_commands
}

fn execute_plan_with_prompt<P>(
    plan: &PipelinePlan,
    stdin_is_terminal: bool,
    mut capture_input: Option<String>,
    mut prompt: P,
) -> Result<()>
where
    P: FnMut(Option<&str>) -> Result<Option<String>>,
{
    if capture_input.is_none() && plan.needs_stdin_prompt && stdin_is_terminal {
        capture_input = prompt(plan.stdin_prompt_label.as_deref())?;
    }

    if should_warn_capture(plan, capture_input.as_deref(), stdin_is_terminal) {
        eprintln!(
            "tx: capturing prompt input. Type your prompt, then press Ctrl-D (Ctrl-Z on Windows) to continue."
        );
    }

    emit_terminal_title(&plan.terminal_title)?;

    match &plan.invocation {
        Invocation::Shell { command } => {
            let shell = default_shell();
            let mut cmd = Command::new(&shell.path);
            cmd.arg(shell.flag);
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                if matches!(shell.flag, "/C" | "/K") {
                    cmd.raw_arg(command);
                } else {
                    cmd.arg(command);
                }
            }
            #[cfg(not(windows))]
            {
                cmd.arg(command);
            }
            cmd.current_dir(&plan.cwd);
            cmd.envs(plan.env.iter().map(|(k, v)| (k, v)));
            if let Some(ref input) = capture_input {
                cmd.env("TX_CAPTURE_STDIN_DATA", input);
            }
            let status = cmd.status()?;
            if !status.success() {
                return Err(eyre!("command exited with status {status}"));
            }
        }
        Invocation::Exec { argv } => {
            let program = argv
                .first()
                .ok_or_else(|| eyre!("wrapper produced empty argv"))?;
            let mut cmd = Command::new(program);
            cmd.args(&argv[1..]);
            cmd.current_dir(&plan.cwd);
            cmd.envs(plan.env.iter().map(|(k, v)| (k, v)));
            if let Some(ref input) = capture_input {
                cmd.env("TX_CAPTURE_STDIN_DATA", input);
            }
            let status = cmd.status()?;
            if !status.success() {
                return Err(eyre!("command exited with status {status}"));
            }
        }
    }

    Ok(())
}

fn parse_vars(vars: &[String]) -> Result<HashMap<String, String>> {
    let mut map = HashMap::new();
    for entry in vars {
        let (key, value) = entry
            .split_once('=')
            .ok_or_else(|| eyre!("invalid var '{}', expected KEY=VALUE", entry))?;
        map.insert(key.trim().to_string(), value.to_string());
    }
    Ok(map)
}

struct ShellCommand {
    path: OsString,
    flag: &'static str,
}

fn default_shell() -> ShellCommand {
    #[cfg(windows)]
    {
        let shell_env = std::env::var("SHELL").ok();
        let comspec = std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".to_string());

        let mut path = shell_env.unwrap_or_default();
        let mut use_cmd = false;

        if path.is_empty() {
            path.clone_from(&comspec);
            use_cmd = true;
        } else {
            let lower = path.to_ascii_lowercase();
            if lower.ends_with("powershell.exe")
                || lower.ends_with("pwsh.exe")
                || lower.ends_with("powershell")
                || lower.ends_with("pwsh")
            {
                path.clone_from(&comspec);
                use_cmd = true;
            } else if lower.ends_with("cmd.exe") || lower.ends_with("\\cmd") || lower == "cmd" {
                use_cmd = true;
            }
        }

        let flag = if use_cmd { "/C" } else { "-c" };
        if use_cmd && path.is_empty() {
            path = comspec;
        }

        ShellCommand {
            path: OsString::from(path),
            flag,
        }
    }
    #[cfg(not(windows))]
    {
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
        ShellCommand {
            path: OsString::from(shell),
            flag: "-c",
        }
    }
}

fn prompt_for_stdin_with_reader<R: BufRead>(label: Option<&str>, reader: &mut R) -> Result<String> {
    if let Some(name) = label {
        eprintln!(
            "tx: {name} can accept extra context. Enter text and press Enter (leave blank to skip):"
        );
    } else {
        eprintln!("tx: enter prompt input and press Enter (leave blank to skip).");
    }
    eprint!("› ");
    io::stderr().flush()?;

    let mut buffer = String::new();
    reader.read_line(&mut buffer)?;

    if buffer.ends_with("\r\n") {
        buffer.truncate(buffer.len() - 2);
        buffer.push('\n');
    }

    if !buffer.ends_with('\n') {
        buffer.push('\n');
    }

    Ok(buffer)
}

fn summary_to_json(
    summary: &SessionSummary,
    snippet: Option<&str>,
    snippet_role: Option<&str>,
) -> serde_json::Value {
    json!({
        "id": summary.id,
        "provider": summary.provider,
        "wrapper": summary.wrapper,
        "label": summary.label,
        "path": summary.path.to_string_lossy(),
        "uuid": summary.uuid,
        "first_prompt": summary.first_prompt,
        "actionable": summary.actionable,
        "created_at": summary.created_at,
        "started_at": summary.started_at,
        "last_active": summary.last_active,
        "size": summary.size,
        "mtime": summary.mtime,
        "snippet": snippet,
        "snippet_role": snippet_role,
    })
}

fn export_markdown(transcript: &Transcript) {
    for line in transcript.markdown_lines(None) {
        println!("{line}");
    }
}

fn log_index_report(report: &IndexReport) {
    if report.errors.is_empty() {
        debug!(
            scanned = report.scanned,
            updated = report.updated,
            skipped = report.skipped,
            removed = report.removed,
            "session index complete"
        );
    } else {
        for IndexError { path, error } in &report.errors {
            tracing::warn!(path = %path.display(), error = ?error, "session ingestion failure");
        }
    }
}

static VAR_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\$\{env:([A-Za-z0-9_]+)\}").unwrap());

fn run_doctor(loaded: &LoadedConfig, db: &Database) -> Result<()> {
    println!("tx doctor");
    println!("=========");

    if !loaded.diagnostics.is_empty() {
        for diag in &loaded.diagnostics {
            match diag.level {
                DiagnosticLevel::Warning => println!("warning: {}", diag.message),
                DiagnosticLevel::Error => println!("error: {}", diag.message),
            }
        }
        println!();
    }

    for (name, provider) in &loaded.config.providers {
        match which(&provider.bin) {
            Ok(path) => println!("✔ provider {} binary found at {}", name, path.display()),
            Err(_) => println!(
                "✘ provider {} binary '{}' not found on PATH",
                name, provider.bin
            ),
        }

        for env in &provider.env {
            for caps in VAR_RE.captures_iter(&env.value_template) {
                let var = caps.get(1).unwrap().as_str();
                if std::env::var(var).is_ok() {
                    println!("  ✔ env {var} is set");
                } else {
                    println!("  ✘ env {var} is missing");
                }
            }
        }

        for root in &provider.session_roots {
            if root.exists() {
                println!("  ✔ session root {}", root.display());
            } else {
                println!("  ✘ session root missing: {}", root.display());
            }
        }
    }

    let db_path = loaded.directories.data_dir.join("tx.sqlite3");
    println!("\nDatabase: {}", db_path.display());
    println!("Known sessions: {}", db.count_sessions()?);

    if let Some(cfg) = loaded.config.features.prompt_assembler.clone() {
        check_prompt_assembler(&cfg);
    }

    Ok(())
}

fn check_prompt_assembler(cfg: &PromptAssemblerConfig) {
    let mut assembler = PromptAssembler::new(cfg.clone());
    match assembler.refresh(true) {
        PromptStatus::Ready { .. } => println!("✔ prompt assembler responded"),
        PromptStatus::Unavailable { message } => {
            println!("✘ prompt assembler unavailable: {message}");
        }
    }
}

fn normalize_target_version_tag(tag: Option<&str>) -> Option<String> {
    tag.map(|value| {
        if value.starts_with('v') {
            value.to_string()
        } else {
            format!("v{value}")
        }
    })
}

fn self_update_message(updated: bool, version: &str) -> String {
    if updated {
        format!("Updated tx to {version}")
    } else {
        format!("tx is already up to date ({version})")
    }
}

fn run_self_update_with<F>(cmd: &SelfUpdateCommand, quiet: bool, mut updater: F) -> Result<String>
where
    F: FnMut(bool, Option<&str>) -> Result<(bool, String)>,
{
    let target_tag = normalize_target_version_tag(cmd.version.as_deref());
    let (did_update, version) = updater(quiet, target_tag.as_deref())?;
    Ok(self_update_message(did_update, &version))
}

#[cfg(all(not(test), not(coverage)))]
fn apply_self_update_release(quiet: bool, target_tag: Option<&str>) -> Result<(bool, String)> {
    use self_update::backends::github::Update;

    const REPO_OWNER: &str = "bedecarroll";
    const REPO_NAME: &str = "tool-executor";
    const BIN_NAME: &str = "tx";

    let mut builder = Update::configure();
    builder
        .repo_owner(REPO_OWNER)
        .repo_name(REPO_NAME)
        .bin_name(BIN_NAME)
        .current_version(env!("CARGO_PKG_VERSION"))
        .show_download_progress(!quiet);

    if let Some(tag) = target_tag {
        builder.target_version_tag(tag);
    }

    let status = builder
        .build()
        .wrap_err("failed to configure self-updater")?
        .update()
        .wrap_err("failed to apply update")?;

    Ok((status.updated(), status.version().to_string()))
}

#[cfg(any(test, coverage))]
fn apply_self_update_release(_quiet: bool, target_tag: Option<&str>) -> Result<(bool, String)> {
    if std::env::var_os("TX_SELF_UPDATE_TEST_FAIL").is_some() {
        return Err(eyre!("self-update backend intentionally failed for tests"));
    }
    let version = target_tag.unwrap_or(env!("CARGO_PKG_VERSION")).to_string();
    Ok((false, version))
}

#[cfg(test)]
mod tests;

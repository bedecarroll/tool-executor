use std::collections::HashMap;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use color_eyre::Result;
use color_eyre::eyre::{WrapErr, eyre};
use regex::Regex;
use serde_json::json;
use std::sync::LazyLock;
use tracing::debug;
use which::which;

#[cfg(feature = "self-update")]
use crate::cli::SelfUpdateCommand;
use crate::cli::{
    Cli, ConfigCommand, ConfigDefaultCommand, ExportCommand, LaunchCommand, ResumeCommand,
    SearchCommand,
};
use crate::config::model::{DiagnosticLevel, PromptAssemblerConfig};
use crate::config::{ConfigSourceKind, LoadedConfig};
use crate::db::Database;
use crate::indexer::{IndexError, IndexReport, Indexer};
use crate::pipeline::{Invocation, PipelinePlan, PipelineRequest, SessionContext, build_pipeline};
use crate::prompts::{PromptAssembler, PromptStatus};
use crate::providers;
use crate::session::{SessionSummary, Transcript};
use crate::tui;
use crate::util;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("provider mismatch: session uses '{expected}' but pipeline asked for '{actual}'")]
    ProviderMismatch { expected: String, actual: String },
}

pub struct App<'cli> {
    #[cfg_attr(not(feature = "self-update"), allow(dead_code))]
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

        let mut indexer = Indexer::new(&mut db, &loaded.config);
        let index_report = indexer.run()?;
        log_index_report(&index_report);

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
            if let Some(summary) = self.db.session_summary(&hit.session_id)? {
                if let Some(cutoff) = since_epoch
                    && summary.last_active.is_some_and(|last| last < cutoff)
                {
                    continue;
                }
                detailed.push((hit, summary));
            }
        }

        if let Some(limit) = cmd.limit
            && detailed.len() > limit
        {
            detailed.truncate(limit);
        }

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

    /// Build and optionally execute a pipeline for launching a new provider session.
    ///
    /// # Errors
    ///
    /// Returns an error if variables are invalid, the pipeline cannot be constructed,
    /// or the resulting command fails to run.
    pub fn launch(&mut self, cmd: &LaunchCommand) -> Result<()> {
        let vars = parse_vars(&cmd.vars)?;
        let request = PipelineRequest {
            config: &self.loaded.config,
            provider_hint: Some(cmd.provider.as_str()),
            profile: cmd.profile.as_deref(),
            additional_pre: cmd.pre_snippets.clone(),
            additional_post: cmd.post_snippets.clone(),
            inline_pre: Vec::new(),
            wrap: cmd.wrap.as_deref(),
            provider_args: cmd.provider_args.clone(),
            vars,
            session: SessionContext::default(),
            cwd: std::env::current_dir()?,
        };

        let plan = build_pipeline(&request)?;
        if cmd.emit_json && !(cmd.dry_run || cmd.emit_command) {
            return Err(eyre!("--emit-json requires --dry-run or --emit-command"));
        }

        if cmd.dry_run || cmd.emit_command {
            let mode = if cmd.emit_json {
                EmitMode::Json
            } else {
                EmitMode::Plain { newline: true }
            };
            emit_command(&plan, mode)?;
            return Ok(());
        }

        execute_plan(&plan).wrap_err("failed to execute pipeline")
    }

    /// Build and optionally execute a pipeline to resume an existing session.
    ///
    /// # Errors
    ///
    /// Returns an error when the session cannot be loaded, the requested profile is
    /// incompatible, the pipeline cannot be constructed, or execution fails.
    pub fn resume(&mut self, cmd: &ResumeCommand) -> Result<()> {
        let summary = self
            .db
            .session_summary(&cmd.session_id)?
            .ok_or_else(|| eyre!("session '{}' not found", cmd.session_id))?;

        if let Some(profile_name) = &cmd.profile {
            let profile = self
                .loaded
                .config
                .profiles
                .get(profile_name)
                .ok_or_else(|| eyre!("profile '{}' not found", profile_name))?;
            if profile.provider != summary.provider {
                return Err(AppError::ProviderMismatch {
                    expected: summary.provider.clone(),
                    actual: profile.provider.clone(),
                }
                .into());
            }
        }

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

        let request = PipelineRequest {
            config: &self.loaded.config,
            provider_hint: Some(summary.provider.as_str()),
            profile: cmd.profile.as_deref(),
            additional_pre: cmd.pre_snippets.clone(),
            additional_post: cmd.post_snippets.clone(),
            inline_pre: Vec::new(),
            wrap: cmd.wrap.as_deref(),
            provider_args,
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
                EmitMode::Plain { newline: true }
            };
            emit_command(&plan, mode)?;
            return Ok(());
        }

        execute_plan(&plan).wrap_err("failed to execute pipeline")
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
    #[cfg(feature = "self-update")]
    pub fn self_update(&self, cmd: &SelfUpdateCommand) -> Result<()> {
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
            .show_download_progress(!self.cli.quiet);

        let target_tag = cmd.version.as_ref().map(|tag| {
            if tag.starts_with('v') {
                tag.clone()
            } else {
                format!("v{tag}")
            }
        });
        if let Some(tag) = target_tag.as_ref() {
            builder.target_version_tag(tag);
        }

        let status = builder
            .build()
            .wrap_err("failed to configure self-updater")?
            .update()
            .wrap_err("failed to apply update")?;

        if status.updated() {
            println!("Updated tx to {}", status.version());
        } else {
            println!("tx is already up to date ({})", status.version());
        }

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
        if cmd.raw {
            stdout.write_all(crate::config::default_template().as_bytes())?;
        } else {
            let config = crate::config::bundled_default_config(&self.loaded.directories);
            stdout.write_all(config.as_bytes())?;
        }
        stdout.flush()?;
        Ok(())
    }

    fn config_dump(&self) -> Result<()> {
        let toml_text = toml::to_string_pretty(&self.loaded.merged)?;
        println!("{toml_text}");
        Ok(())
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

#[derive(Debug, Clone, Copy)]
pub enum EmitMode {
    Json,
    Plain { newline: bool },
}

pub(crate) fn emit_command(plan: &PipelinePlan, mode: EmitMode) -> Result<()> {
    match mode {
        EmitMode::Json => {
            let payload = json!({ "command": plan.display, "env": plan.env });
            println!("{}", serde_json::to_string_pretty(&payload)?);
        }
        EmitMode::Plain { newline } => {
            if newline {
                println!("{}", plan.display);
            } else {
                print!("{}", plan.display);
                io::stdout().flush()?;
            }
        }
    }
    Ok(())
}

pub(crate) fn execute_plan(plan: &PipelinePlan) -> Result<()> {
    match &plan.invocation {
        Invocation::Shell { command } => {
            let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
            let mut cmd = Command::new(shell);
            cmd.arg("-c").arg(command);
            cmd.current_dir(&plan.cwd);
            cmd.envs(plan.env.iter().map(|(k, v)| (k, v)));
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

fn summary_to_json(
    summary: &SessionSummary,
    snippet: Option<&str>,
    snippet_role: Option<&str>,
) -> serde_json::Value {
    json!({
        "id": summary.id,
        "provider": summary.provider,
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
        PromptStatus::Disabled => {}
    }
}

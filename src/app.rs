use std::collections::HashMap;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use color_eyre::Result;
use color_eyre::eyre::{WrapErr, eyre};
use regex::Regex;
use serde_json::json;
use std::sync::LazyLock;
use tracing::info;
use which::which;

#[cfg(feature = "self-update")]
use crate::cli::SelfUpdateCommand;
use crate::cli::{
    Cli, ConfigCommand, ExportCommand, LaunchCommand, ResumeCommand, SearchCommand, SessionsCommand,
};
use crate::config::model::{DiagnosticLevel, PromptAssemblerConfig};
use crate::config::{ConfigSourceKind, LoadedConfig};
use crate::db::Database;
use crate::indexer::{IndexError, IndexReport, Indexer};
use crate::pipeline::{Invocation, PipelinePlan, PipelineRequest, SessionContext, build_pipeline};
use crate::prompts::{PromptAssembler, PromptStatus};
use crate::session::{SearchHit, SessionQuery, Transcript};
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
    pub cli: &'app Cli,
    pub config: &'app crate::config::model::Config,
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

    /// Render the sessions listing according to the provided filters.
    ///
    /// # Errors
    ///
    /// Returns an error if querying the session database fails or JSON serialization
    /// of the output cannot be completed.
    pub fn list_sessions(&self, cmd: &SessionsCommand) -> Result<()> {
        let since_epoch = cmd
            .since
            .map(|seconds| util::unix_timestamp().saturating_sub(seconds));
        let sessions = self.db.list_sessions(
            cmd.provider.as_deref(),
            cmd.actionable,
            since_epoch,
            cmd.limit,
        )?;

        if cmd.json || self.cli.json {
            let payload: Vec<_> = sessions.iter().map(session_to_json).collect();
            println!("{}", serde_json::to_string_pretty(&payload)?);
            return Ok(());
        }

        if sessions.is_empty() {
            println!("No sessions found.");
            return Ok(());
        }

        print_sessions_table(&sessions);
        Ok(())
    }

    /// Execute a sessions search (first prompt or full-text) depending on flags.
    ///
    /// # Errors
    ///
    /// Returns an error if the database search fails or the results cannot be
    /// serialized to JSON when requested.
    pub fn search(&self, cmd: &SearchCommand) -> Result<()> {
        let hits = if cmd.full_text {
            self.db
                .search_full_text(&cmd.term, cmd.provider.as_deref(), cmd.actionable)?
        } else {
            self.db
                .search_first_prompt(&cmd.term, cmd.provider.as_deref(), cmd.actionable)?
        };

        if cmd.json || self.cli.json {
            let payload: Vec<_> = hits.iter().map(search_to_json).collect();
            println!("{}", serde_json::to_string_pretty(&payload)?);
            return Ok(());
        }

        if hits.is_empty() {
            println!("No matches found.");
            return Ok(());
        }

        print_search_table(&hits);
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
            wrap: cmd.wrap.as_deref(),
            provider_args: cmd.provider_args.clone(),
            vars,
            session: SessionContext::default(),
            cwd: std::env::current_dir()?,
        };

        let plan = build_pipeline(&request)?;
        if cmd.dry_run || self.cli.emit_command {
            emit_command(&plan, cmd.dry_run || self.cli.json)?;
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

        let request = PipelineRequest {
            config: &self.loaded.config,
            provider_hint: Some(summary.provider.as_str()),
            profile: cmd.profile.as_deref(),
            additional_pre: cmd.pre_snippets.clone(),
            additional_post: cmd.post_snippets.clone(),
            wrap: cmd.wrap.as_deref(),
            provider_args: cmd.provider_args.clone(),
            vars,
            session: SessionContext {
                id: Some(summary.id.clone()),
                label: summary.label.clone(),
            },
            cwd: working_dir,
        };

        let plan = build_pipeline(&request)?;
        if cmd.dry_run || self.cli.emit_command {
            emit_command(&plan, cmd.dry_run || self.cli.json)?;
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

        if cmd.md {
            export_markdown(&transcript);
        } else {
            export_human(&transcript);
        }

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
        if self.cli.emit_command {
            return Err(eyre!("--emit-command requires a non-interactive selection"));
        }

        let prompt = self.prompt.as_mut();
        let mut ctx = UiContext {
            cli: self.cli,
            config: &self.loaded.config,
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
                "  - {} (provider: {}, pre: [{}], post: [{}], wrap: {})",
                name,
                profile.provider,
                profile.pre.join(", "),
                profile.post.join(", "),
                profile.wrap.as_deref().unwrap_or("-"),
            );
        }
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

pub(crate) fn emit_command(plan: &PipelinePlan, json: bool) -> Result<()> {
    if json {
        let payload = json!({ "command": plan.display, "env": plan.env });
        println!("{}", serde_json::to_string_pretty(&payload)?);
    } else {
        print!("{}", plan.display);
        io::stdout().flush()?;
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

fn session_to_json(session: &SessionQuery) -> serde_json::Value {
    json!({
        "id": session.id,
        "provider": session.provider,
        "label": session.label,
        "first_prompt": session.first_prompt,
        "actionable": session.actionable,
        "last_active": session.last_active,
    })
}

fn search_to_json(hit: &SearchHit) -> serde_json::Value {
    json!({
        "session_id": hit.session_id,
        "provider": hit.provider,
        "label": hit.label,
        "snippet": hit.snippet,
        "last_active": hit.last_active,
    })
}

fn print_sessions_table(sessions: &[SessionQuery]) {
    println!(
        "{:<32} {:<12} {:<14} {:<10} {:<19}",
        "Session ID", "Provider", "Label", "Actionable", "Last Active"
    );
    println!("{}", "-".repeat(96));
    for session in sessions {
        println!(
            "{:<32} {:<12} {:<14} {:<10} {:<19}",
            truncate(&session.id, 32),
            session.provider,
            truncate(session.label.as_deref().unwrap_or("-"), 14),
            if session.actionable { "yes" } else { "no" },
            util::format_timestamp(session.last_active),
        );
    }
}

fn print_search_table(hits: &[SearchHit]) {
    println!(
        "{:<32} {:<12} {:<14} {:<19} Snippet",
        "Session ID", "Provider", "Label", "Last Active"
    );
    println!("{}", "-".repeat(120));
    for hit in hits {
        println!(
            "{:<32} {:<12} {:<14} {:<19} {}",
            truncate(&hit.session_id, 32),
            hit.provider,
            truncate(hit.label.as_deref().unwrap_or("-"), 14),
            util::format_timestamp(hit.last_active),
            truncate(hit.snippet.as_deref().unwrap_or(""), 50),
        );
    }
}

fn truncate(input: &str, max: usize) -> String {
    if input.chars().count() <= max {
        input.to_string()
    } else {
        let mut out = input.chars().take(max - 1).collect::<String>();
        out.push('…');
        out
    }
}

fn export_markdown(transcript: &Transcript) {
    println!("# Session {}", transcript.session.id);
    println!("- Provider: {}", transcript.session.provider);
    if let Some(label) = &transcript.session.label {
        println!("- Label: {label}");
    }
    println!(
        "- Created: {}",
        util::format_timestamp(transcript.session.created_at)
    );
    println!(
        "- Last active: {}",
        util::format_timestamp(transcript.session.last_active)
    );
    println!();
    println!("## Transcript");

    for message in &transcript.messages {
        println!("\n### {}", message.role);
        println!();
        println!("{}", message.content);
    }
}

fn export_human(transcript: &Transcript) {
    println!(
        "Session {} ({})",
        transcript.session.id, transcript.session.provider
    );
    if let Some(label) = &transcript.session.label {
        println!("Label: {label}");
    }
    println!(
        "Created: {}",
        util::format_timestamp(transcript.session.created_at)
    );
    println!(
        "Last active: {}",
        util::format_timestamp(transcript.session.last_active)
    );
    println!("{}", "-".repeat(48));
    for message in &transcript.messages {
        println!("{}:", message.role);
        println!("{}", message.content);
        println!("{}", "-".repeat(48));
    }
}

fn log_index_report(report: &IndexReport) {
    if report.errors.is_empty() {
        info!(
            scanned = report.scanned,
            updated = report.updated,
            skipped = report.skipped,
            removed = report.removed,
            "session index complete"
        );
    } else {
        for IndexError { path, error } in &report.errors {
            tracing::warn!(path = %path.display(), error = %error, "session ingestion failure");
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

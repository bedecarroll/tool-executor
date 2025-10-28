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

#[cfg(feature = "self-update")]
use crate::cli::SelfUpdateCommand;
use crate::cli::{
    Cli, ConfigCommand, ConfigDefaultCommand, ExportCommand, ResumeCommand, SearchCommand,
};
use crate::config::model::{DiagnosticLevel, PromptAssemblerConfig};
use crate::config::{ConfigSourceKind, LoadedConfig};
use crate::db::Database;
use crate::indexer::{IndexError, IndexReport, Indexer};
use crate::pipeline::{Invocation, PipelinePlan, PipelineRequest, SessionContext, build_pipeline};
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

        let detailed = Self::collate_search_results(
            hits,
            since_epoch,
            role_filter,
            cmd.limit,
            |session_id| self.db.session_summary(session_id),
        )?;

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

    /// Build and optionally execute a pipeline to resume an existing session.
    ///
    /// # Errors
    ///
    /// Returns an error when the session cannot be loaded, the requested profile is
    /// incompatible, the pipeline cannot be constructed, or execution fails.
    pub fn resume(&mut self, cmd: &ResumeCommand) -> Result<()> {
        let summary = self
            .db
            .session_summary_for_identifier(&cmd.session_id)?
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
            capture_prompt: false,
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
    Plain { newline: bool, friendly: bool },
}

pub(crate) fn emit_command(plan: &PipelinePlan, mode: EmitMode) -> Result<()> {
    match mode {
        EmitMode::Json => {
            let payload = json!({ "command": plan.display, "env": plan.env });
            println!("{}", serde_json::to_string_pretty(&payload)?);
        }
        EmitMode::Plain { newline, friendly } => {
            let command = if friendly {
                &plan.friendly_display
            } else {
                &plan.display
            };
            if newline {
                println!("{command}");
            } else {
                print!("{command}");
                io::stdout().flush()?;
            }
        }
    }
    Ok(())
}

pub(crate) fn execute_plan(plan: &PipelinePlan) -> Result<()> {
    execute_plan_with_prompt(plan, io::stdin().is_terminal(), |label| {
        prompt_for_stdin(label).map(Some)
    })
}

fn execute_plan_with_prompt<P>(
    plan: &PipelinePlan,
    stdin_is_terminal: bool,
    mut prompt: P,
) -> Result<()>
where
    P: FnMut(Option<&str>) -> Result<Option<String>>,
{
    let capture_input = if plan.needs_stdin_prompt && stdin_is_terminal {
        prompt(plan.stdin_prompt_label.as_deref())?
    } else {
        None
    };

    if capture_input.is_none()
        && plan.pipeline.contains("internal capture-arg")
        && stdin_is_terminal
    {
        eprintln!(
            "tx: capturing prompt input. Type your prompt, then press Ctrl-D (Ctrl-Z on Windows) to continue."
        );
    }

    match &plan.invocation {
        Invocation::Shell { command } => {
            let shell = default_shell();
            let mut cmd = Command::new(&shell.path);
            cmd.arg(shell.flag).arg(command);
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
        let shell = std::env::var("SHELL")
            .or_else(|_| std::env::var("COMSPEC"))
            .unwrap_or_else(|_| "cmd.exe".to_string());
        let flag = if shell.to_ascii_lowercase().ends_with("cmd.exe")
            || shell.to_ascii_lowercase().ends_with("\\cmd")
            || shell.eq_ignore_ascii_case("cmd")
        {
            "/C"
        } else {
            "-c"
        };
        ShellCommand {
            path: OsString::from(shell),
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

fn prompt_for_stdin(label: Option<&str>) -> Result<String> {
    let stdin = io::stdin();
    let mut handle = stdin.lock();
    prompt_for_stdin_with_reader(label, &mut handle)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{
        ConfigCommand, ConfigDefaultCommand, ExportCommand, ResumeCommand, SearchCommand,
    };
    use crate::config::model::{
        Config, ConfigDiagnostic, Defaults, DiagnosticLevel, EnvVar, FeatureConfig, ProfileConfig,
        ProviderConfig, SearchMode, Snippet, SnippetConfig, WrapperConfig, WrapperMode,
    };
    use crate::config::{AppDirectories, ConfigSource, ConfigSourceKind, LoadedConfig};
    use crate::db::Database;
    use crate::indexer::IndexError;
    use crate::session::{MessageRecord, SearchHit, SessionIngest, SessionSummary};
    use assert_fs::TempDir;
    use assert_fs::prelude::*;
    use color_eyre::eyre::eyre;
    use indexmap::IndexMap;
    use std::collections::HashMap;
    #[cfg(unix)]
    use std::env;
    use std::fs;
    use std::io::{Cursor, Write};
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;
    use std::sync::{LazyLock, Mutex};
    use time::OffsetDateTime;
    use toml::Value;

    static ENV_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

    struct EnvOverride {
        key: &'static str,
        original: Option<String>,
    }

    impl EnvOverride {
        fn set_path(key: &'static str, path: &Path) -> Self {
            let original = std::env::var(key).ok();
            unsafe {
                std::env::set_var(key, path);
            }
            Self { key, original }
        }
    }

    impl Drop for EnvOverride {
        fn drop(&mut self) {
            if let Some(value) = &self.original {
                unsafe {
                    std::env::set_var(self.key, value);
                }
            } else {
                unsafe {
                    std::env::remove_var(self.key);
                }
            }
        }
    }

    fn sample_summary() -> SessionSummary {
        SessionSummary {
            id: "id".into(),
            provider: "codex".into(),
            label: Some("demo".into()),
            path: PathBuf::from("/tmp/file.jsonl"),
            uuid: Some("abc".into()),
            first_prompt: Some("Hello".into()),
            actionable: true,
            created_at: Some(1),
            started_at: Some(2),
            last_active: Some(3),
            size: 4,
            mtime: 5,
        }
    }

    #[test]
    fn parse_vars_splits_key_value_pairs() -> Result<()> {
        let vars = vec!["FOO=bar".into(), "BAZ=qux=quux".into()];
        let parsed = parse_vars(&vars)?;
        assert_eq!(parsed.get("FOO"), Some(&"bar".to_string()));
        assert_eq!(parsed.get("BAZ"), Some(&"qux=quux".to_string()));
        Ok(())
    }

    fn toml_path(path: &Path) -> String {
        let mut rendered = path.to_string_lossy().to_string();
        if cfg!(windows) {
            rendered = rendered.replace('\\', "\\\\");
        }
        rendered
    }

    #[test]
    fn parse_vars_rejects_missing_equals() {
        let err = parse_vars(&["invalid".into()]).unwrap_err();
        assert!(err.to_string().contains("expected KEY=VALUE"));
    }

    #[test]
    fn prompt_for_stdin_with_reader_appends_newline() -> Result<()> {
        let mut reader = Cursor::new("value");
        let captured = prompt_for_stdin_with_reader(None, &mut reader)?;
        assert_eq!(captured, "value\n");
        Ok(())
    }

    #[test]
    fn prompt_for_stdin_with_reader_normalizes_crlf() -> Result<()> {
        let mut reader = Cursor::new("value\r\n");
        let captured = prompt_for_stdin_with_reader(Some("Label"), &mut reader)?;
        assert_eq!(captured, "value\n");
        Ok(())
    }

    #[test]
    fn summary_to_json_includes_snippet() {
        let summary = sample_summary();
        let value = summary_to_json(&summary, Some("snippet"), Some("user"));
        assert_eq!(value["id"], "id");
        assert_eq!(value["snippet"], "snippet");
        assert_eq!(value["snippet_role"], "user");
    }

    #[test]
    fn collate_search_results_applies_filters_and_limit() -> Result<()> {
        let hits = vec![
            SearchHit {
                session_id: "keep-1".into(),
                provider: "codex".into(),
                label: None,
                role: Some("user".into()),
                snippet: Some("hello".into()),
                last_active: Some(120),
                actionable: true,
            },
            SearchHit {
                session_id: "skip-role".into(),
                provider: "codex".into(),
                label: None,
                role: Some("assistant".into()),
                snippet: None,
                last_active: Some(140),
                actionable: true,
            },
            SearchHit {
                session_id: "stale".into(),
                provider: "codex".into(),
                label: None,
                role: Some("user".into()),
                snippet: None,
                last_active: Some(10),
                actionable: true,
            },
            SearchHit {
                session_id: "keep-2".into(),
                provider: "codex".into(),
                label: None,
                role: Some("USER".into()),
                snippet: None,
                last_active: Some(200),
                actionable: true,
            },
        ];

        let mut summaries = HashMap::new();
        let mut summary_one = sample_summary();
        summary_one.id = "keep-1".into();
        summary_one.last_active = Some(120);
        summaries.insert(summary_one.id.clone(), summary_one);

        let mut summary_two = sample_summary();
        summary_two.id = "skip-role".into();
        summary_two.last_active = Some(140);
        summaries.insert(summary_two.id.clone(), summary_two);

        let mut summary_three = sample_summary();
        summary_three.id = "stale".into();
        summary_three.last_active = Some(10);
        summaries.insert(summary_three.id.clone(), summary_three);

        let mut summary_four = sample_summary();
        summary_four.id = "keep-2".into();
        summary_four.last_active = Some(200);
        summaries.insert(summary_four.id.clone(), summary_four);

        let mut lookup =
            |id: &str| -> Result<Option<SessionSummary>> { Ok(summaries.get(id).cloned()) };

        let detailed =
            App::collate_search_results(hits, Some(100), Some("user"), Some(1), &mut lookup)?;
        assert_eq!(detailed.len(), 1);
        assert_eq!(detailed[0].0.session_id, "keep-1");
        Ok(())
    }

    #[test]
    fn collate_search_results_skips_missing_summary() -> Result<()> {
        let hits = vec![SearchHit {
            session_id: "missing".into(),
            provider: "codex".into(),
            label: None,
            role: Some("user".into()),
            snippet: None,
            last_active: Some(50),
            actionable: true,
        }];

        let mut lookup = |_id: &str| -> Result<Option<SessionSummary>> { Ok(None) };
        let detailed = App::collate_search_results(hits, None, None, None, &mut lookup)?;
        assert!(detailed.is_empty());
        Ok(())
    }

    #[test]
    fn log_index_report_emits_warnings_for_errors() {
        let mut report = IndexReport {
            scanned: 1,
            updated: 0,
            skipped: 0,
            removed: 0,
            errors: vec![IndexError {
                path: PathBuf::from("missing.jsonl"),
                error: eyre!("boom"),
            }],
        };
        log_index_report(&report);
        report.errors.clear();
        log_index_report(&report);
    }

    fn setup_directories(temp: &TempDir) -> Result<AppDirectories> {
        let directories = AppDirectories {
            config_dir: temp.path().join("config"),
            data_dir: temp.path().join("data"),
            cache_dir: temp.path().join("cache"),
        };
        directories.ensure_all()?;
        Ok(directories)
    }

    fn create_session_artifacts(temp: &TempDir) -> Result<(PathBuf, PathBuf)> {
        let sessions_dir = temp.path().join("sessions");
        fs::create_dir_all(&sessions_dir)?;
        let session_path = sessions_dir.join("session.jsonl");
        fs::File::create(&session_path)?.write_all(b"{\"event\":\"test\"}\n")?;
        Ok((sessions_dir, session_path))
    }

    fn fixture_config(sessions_dir: &Path) -> Config {
        let mut providers = IndexMap::new();
        providers.insert(
            "codex".into(),
            ProviderConfig {
                name: "codex".into(),
                bin: "echo".into(),
                flags: vec!["--from-config".into()],
                env: vec![EnvVar {
                    key: "TEST_PRESENT".into(),
                    value_template: "${env:TEST_PRESENT}".into(),
                }],
                session_roots: vec![sessions_dir.to_path_buf()],
                stdin: None,
            },
        );
        providers.insert(
            "alt".into(),
            ProviderConfig {
                name: "alt".into(),
                bin: "echo".into(),
                flags: Vec::new(),
                env: vec![EnvVar {
                    key: "TEST_MISSING".into(),
                    value_template: "${env:TEST_MISSING}".into(),
                }],
                session_roots: vec![sessions_dir.to_path_buf()],
                stdin: None,
            },
        );

        let mut pre_snippets = IndexMap::new();
        pre_snippets.insert(
            "pre".into(),
            Snippet {
                name: "pre".into(),
                command: "echo pre".into(),
            },
        );

        let snippets = SnippetConfig {
            pre: pre_snippets,
            post: IndexMap::new(),
        };

        let mut profiles = IndexMap::new();
        profiles.insert(
            "default".into(),
            ProfileConfig {
                name: "default".into(),
                provider: "codex".into(),
                description: Some("Primary profile".into()),
                pre: vec!["pre".into()],
                post: Vec::new(),
                wrap: Some("wrap".into()),
            },
        );
        profiles.insert(
            "mismatch".into(),
            ProfileConfig {
                name: "mismatch".into(),
                provider: "alt".into(),
                description: None,
                pre: Vec::new(),
                post: Vec::new(),
                wrap: None,
            },
        );

        let mut wrappers = IndexMap::new();
        wrappers.insert(
            "wrap".into(),
            WrapperConfig {
                name: "wrap".into(),
                mode: WrapperMode::Shell {
                    command: "echo {{CMD}}".into(),
                },
            },
        );
        wrappers.insert(
            "execwrap".into(),
            WrapperConfig {
                name: "execwrap".into(),
                mode: WrapperMode::Exec {
                    argv: vec!["exec-binary".into()],
                },
            },
        );

        Config {
            defaults: Defaults {
                provider: Some("codex".into()),
                profile: Some("default".into()),
                search_mode: SearchMode::FirstPrompt,
                preview_filter: Some(vec!["cat".into()]),
            },
            providers,
            snippets,
            wrappers,
            profiles,
            features: FeatureConfig {
                prompt_assembler: None,
            },
        }
    }

    fn fixture_sources(directories: &AppDirectories) -> Vec<ConfigSource> {
        vec![
            ConfigSource {
                kind: ConfigSourceKind::Main,
                path: directories.config_dir.join("config.toml"),
            },
            ConfigSource {
                kind: ConfigSourceKind::DropIn,
                path: directories.config_dir.join("conf.d/10-extra.toml"),
            },
            ConfigSource {
                kind: ConfigSourceKind::Project,
                path: directories.config_dir.join("..").join("project.toml"),
            },
            ConfigSource {
                kind: ConfigSourceKind::ProjectDropIn,
                path: directories
                    .config_dir
                    .join("..")
                    .join("project.d")
                    .join("00-extra.toml"),
            },
        ]
    }

    fn seed_database(
        directories: &AppDirectories,
        session_path: PathBuf,
    ) -> Result<(Database, SessionSummary)> {
        let db_path = directories.data_dir.join("tx.sqlite3");
        let mut db = Database::open(&db_path)?;
        let now = OffsetDateTime::now_utc().unix_timestamp();
        let summary = SessionSummary {
            id: "sess-1".into(),
            provider: "codex".into(),
            label: Some("Demo Session".into()),
            path: session_path,
            uuid: Some("uuid-1".into()),
            first_prompt: Some("Hello world".into()),
            actionable: true,
            created_at: Some(now),
            started_at: Some(now),
            last_active: Some(now),
            size: 42,
            mtime: now,
        };
        let mut message = MessageRecord::new(
            summary.id.clone(),
            0,
            "user",
            "Hello world",
            Some("event_msg".into()),
            Some(now),
        );
        message.is_first = true;
        db.upsert_session(&SessionIngest::new(summary.clone(), vec![message]))?;
        Ok((db, summary))
    }

    fn configure_provider_env() {
        unsafe {
            std::env::set_var("TEST_PRESENT", "1");
            std::env::remove_var("TEST_MISSING");
        }
    }

    fn build_app_fixture(
        diagnostics: Vec<ConfigDiagnostic>,
    ) -> Result<(TempDir, App<'static>, SessionSummary)> {
        let temp = TempDir::new()?;
        let directories = setup_directories(&temp)?;
        let (sessions_dir, session_path) = create_session_artifacts(&temp)?;
        let config = fixture_config(&sessions_dir);
        let sources = fixture_sources(&directories);
        let (db, summary) = seed_database(&directories, session_path)?;

        let mut merged = toml::map::Map::new();
        merged.insert("provider".into(), Value::String("codex".into()));
        let loaded = LoadedConfig {
            config: config.clone(),
            merged: Value::Table(merged),
            directories: directories.clone(),
            sources,
            diagnostics,
        };

        configure_provider_env();

        let cli = Box::leak(Box::new(Cli {
            config_dir: Some(directories.config_dir.clone()),
            verbose: 0,
            quiet: false,
            command: None,
        }));

        let app = App {
            cli,
            loaded,
            db,
            prompt: None,
        };

        Ok((temp, app, summary))
    }

    #[test]
    fn app_search_and_export_paths() -> Result<()> {
        let (_temp, app, summary) = build_app_fixture(Vec::new())?;
        let mut search_cmd = SearchCommand {
            term: None,
            full_text: false,
            provider: None,
            since: None,
            role: None,
            limit: None,
        };
        app.search(&search_cmd)?;

        search_cmd.term = Some("Hello".into());
        app.search(&search_cmd)?;

        search_cmd.full_text = true;
        search_cmd.role = Some("user".into());
        search_cmd.since = Some(60);
        app.search(&search_cmd)?;

        let export_cmd = ExportCommand {
            session_id: summary.id.clone(),
        };
        app.export(&export_cmd)?;
        Ok(())
    }

    #[test]
    fn app_search_rejects_role_without_full_text() -> Result<()> {
        let (_temp, app, _summary) = build_app_fixture(Vec::new())?;
        let cmd = SearchCommand {
            term: Some("hello".into()),
            full_text: false,
            provider: None,
            since: None,
            role: Some("user".into()),
            limit: None,
        };
        let err = app.search(&cmd).expect_err("role requires full-text");
        assert!(err.to_string().contains("--role requires --full-text"));
        Ok(())
    }

    #[test]
    fn app_resume_and_config_commands() -> Result<()> {
        let (_temp, mut app, summary) = build_app_fixture(Vec::new())?;
        let mut resume_cmd = ResumeCommand {
            session_id: summary.uuid.clone().unwrap(),
            profile: Some("default".into()),
            pre_snippets: vec!["pre".into()],
            post_snippets: Vec::new(),
            wrap: None,
            emit_command: true,
            emit_json: true,
            vars: vec!["KEY=value".into()],
            dry_run: false,
            provider_args: vec!["--flag".into()],
        };
        app.resume(&resume_cmd)?;

        resume_cmd.profile = Some("mismatch".into());
        let err = app.resume(&resume_cmd).unwrap_err();
        assert!(err.to_string().contains("provider mismatch"));

        app.config(&ConfigCommand::List)?;
        app.config(&ConfigCommand::Dump)?;
        app.config(&ConfigCommand::Where)?;
        app.config(&ConfigCommand::Default(ConfigDefaultCommand { raw: false }))?;
        Ok(())
    }

    #[test]
    fn app_resume_requires_emit_json_companion_flag() -> Result<()> {
        let (_temp, mut app, summary) = build_app_fixture(Vec::new())?;
        let cmd = ResumeCommand {
            session_id: summary.uuid.clone().unwrap(),
            profile: Some("default".into()),
            pre_snippets: Vec::new(),
            post_snippets: Vec::new(),
            wrap: None,
            emit_command: false,
            emit_json: true,
            vars: Vec::new(),
            dry_run: false,
            provider_args: Vec::new(),
        };
        let err = app
            .resume(&cmd)
            .expect_err("emit-json should require dry-run or emit-command");
        assert!(
            err.to_string()
                .contains("--emit-json requires --dry-run or --emit-command")
        );
        Ok(())
    }

    #[test]
    fn app_config_default_supports_raw_mode() -> Result<()> {
        let (_temp, app, _) = build_app_fixture(Vec::new())?;
        app.config(&ConfigCommand::Default(ConfigDefaultCommand { raw: true }))?;
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn execute_plan_shell_succeeds() -> Result<()> {
        let _env = ENV_LOCK.lock().unwrap();
        let original_shell = std::env::var("SHELL").ok();
        unsafe {
            std::env::set_var("SHELL", "/bin/sh");
        }

        let cwd = std::env::current_dir()?;
        let plan = PipelinePlan {
            pipeline: "true".into(),
            display: "true".into(),
            friendly_display: "true".into(),
            env: Vec::new(),
            invocation: Invocation::Shell {
                command: "true".into(),
            },
            provider: "codex".into(),
            pre_snippets: Vec::new(),
            post_snippets: Vec::new(),
            wrapper: None,
            needs_stdin_prompt: false,
            stdin_prompt_label: None,
            cwd,
        };

        execute_plan(&plan)?;

        if let Some(shell) = original_shell {
            unsafe {
                std::env::set_var("SHELL", shell);
            }
        } else {
            unsafe {
                std::env::remove_var("SHELL");
            }
        }

        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn execute_plan_shell_propagates_failure() -> Result<()> {
        let _env = ENV_LOCK.lock().unwrap();
        let original_shell = std::env::var("SHELL").ok();
        unsafe {
            std::env::set_var("SHELL", "/bin/sh");
        }

        let cwd = std::env::current_dir()?;
        let plan = PipelinePlan {
            pipeline: "false".into(),
            display: "false".into(),
            friendly_display: "false".into(),
            env: Vec::new(),
            invocation: Invocation::Shell {
                command: "false".into(),
            },
            provider: "codex".into(),
            pre_snippets: Vec::new(),
            post_snippets: Vec::new(),
            wrapper: None,
            needs_stdin_prompt: false,
            stdin_prompt_label: None,
            cwd,
        };

        let err = execute_plan(&plan).unwrap_err();
        assert!(
            err.to_string().contains("command exited with status"),
            "unexpected error: {err:?}"
        );

        if let Some(shell) = original_shell {
            unsafe {
                std::env::set_var("SHELL", shell);
            }
        } else {
            unsafe {
                std::env::remove_var("SHELL");
            }
        }

        Ok(())
    }

    #[test]
    fn execute_plan_shell_captures_prompt_input() -> Result<()> {
        let _env = ENV_LOCK.lock().unwrap();
        let temp = TempDir::new()?;
        let output = temp.child("captured.txt");
        let script = temp.child(if cfg!(windows) {
            "capture.cmd"
        } else {
            "capture.sh"
        });
        if cfg!(windows) {
            script.write_str(
                "@echo off\r\nsetlocal EnableExtensions EnableDelayedExpansion\r\n<nul set /p=\"%TX_CAPTURE_STDIN_DATA%\" > \"%~1\"\r\n",
            )?;
        } else {
            script.write_str("#!/bin/sh\nprintf '%s' \"$TX_CAPTURE_STDIN_DATA\" > \"$1\"\n")?;
            #[cfg(unix)]
            {
                let perms = fs::Permissions::from_mode(0o755);
                fs::set_permissions(script.path(), perms)?;
            }
        }

        let command = format!("{} {}", script.path().display(), output.path().display());

        let plan = PipelinePlan {
            pipeline: command.clone(),
            display: "capture".into(),
            friendly_display: "capture".into(),
            env: Vec::new(),
            invocation: Invocation::Shell { command },
            provider: "codex".into(),
            pre_snippets: Vec::new(),
            post_snippets: Vec::new(),
            wrapper: None,
            needs_stdin_prompt: true,
            stdin_prompt_label: Some("Prompt".into()),
            cwd: temp.path().to_path_buf(),
        };

        execute_plan_with_prompt(&plan, true, |_| Ok(Some("payload".into())))?;
        assert_eq!(std::fs::read_to_string(output.path())?, "payload");
        Ok(())
    }

    #[test]
    fn execute_plan_emits_capture_warning_for_internal_pipeline() -> Result<()> {
        #[cfg(windows)]
        let original_shell = std::env::var("SHELL").ok();
        #[cfg(windows)]
        unsafe {
            if std::env::var_os("SHELL").is_none()
                && let Ok(comspec) = std::env::var("COMSPEC")
            {
                std::env::set_var("SHELL", comspec);
            }
        }

        let command = if cfg!(windows) {
            "exit 0".to_string()
        } else {
            "true".to_string()
        };
        let plan = PipelinePlan {
            pipeline: "internal capture-arg".into(),
            display: "capture".into(),
            friendly_display: "capture".into(),
            env: Vec::new(),
            invocation: Invocation::Shell { command },
            provider: "codex".into(),
            pre_snippets: Vec::new(),
            post_snippets: Vec::new(),
            wrapper: None,
            needs_stdin_prompt: false,
            stdin_prompt_label: None,
            cwd: std::env::current_dir()?,
        };

        execute_plan_with_prompt(&plan, true, |_| Ok(None))?;

        #[cfg(windows)]
        {
            if let Some(shell) = original_shell {
                unsafe {
                    std::env::set_var("SHELL", shell);
                }
            } else {
                unsafe {
                    std::env::remove_var("SHELL");
                }
            }
        }
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn execute_plan_exec_succeeds_and_failures() -> Result<()> {
        let cwd = std::env::current_dir()?;
        let mut success_plan = PipelinePlan {
            pipeline: "exec success".into(),
            display: "exec success".into(),
            friendly_display: "exec success".into(),
            env: Vec::new(),
            invocation: Invocation::Exec {
                argv: vec!["/bin/sh".into(), "-c".into(), "exit 0".into()],
            },
            provider: "codex".into(),
            pre_snippets: Vec::new(),
            post_snippets: Vec::new(),
            wrapper: None,
            needs_stdin_prompt: false,
            stdin_prompt_label: None,
            cwd: cwd.clone(),
        };

        execute_plan(&success_plan)?;

        success_plan.invocation = Invocation::Exec {
            argv: vec!["/bin/sh".into(), "-c".into(), "exit 5".into()],
        };
        let err = execute_plan(&success_plan).unwrap_err();
        assert!(
            err.to_string().contains("command exited with status"),
            "unexpected error: {err:?}"
        );

        Ok(())
    }

    #[test]
    fn app_doctor_and_config_lint() -> Result<()> {
        let (_temp, app, _summary) = build_app_fixture(Vec::new())?;
        app.doctor()?;
        app.config(&ConfigCommand::Lint)?;

        let diag = ConfigDiagnostic {
            level: DiagnosticLevel::Error,
            message: "bad configuration".into(),
        };
        let (_temp, app_with_error, _) = build_app_fixture(vec![diag])?;
        let err = app_with_error.config(&ConfigCommand::Lint).unwrap_err();
        assert!(err.to_string().contains("configuration contains errors"));
        Ok(())
    }

    #[test]
    fn emit_command_covers_all_modes() -> Result<()> {
        let cwd = std::env::current_dir()?;
        let plan = PipelinePlan {
            pipeline: "echo hi".into(),
            display: "echo hi".into(),
            friendly_display: "friendly hi".into(),
            env: vec![("KEY".into(), "VALUE".into())],
            invocation: Invocation::Shell {
                command: "true".into(),
            },
            provider: "codex".into(),
            pre_snippets: Vec::new(),
            post_snippets: Vec::new(),
            wrapper: None,
            needs_stdin_prompt: false,
            stdin_prompt_label: None,
            cwd,
        };

        emit_command(&plan, EmitMode::Json)?;
        emit_command(
            &plan,
            EmitMode::Plain {
                newline: false,
                friendly: false,
            },
        )?;
        emit_command(
            &plan,
            EmitMode::Plain {
                newline: true,
                friendly: true,
            },
        )?;
        Ok(())
    }

    #[test]
    fn config_lint_emits_warnings_without_error() -> Result<()> {
        let warning = ConfigDiagnostic {
            level: DiagnosticLevel::Warning,
            message: "deprecated option".into(),
        };
        let (_temp, app, _) = build_app_fixture(vec![warning])?;
        app.config(&ConfigCommand::Lint)?;
        Ok(())
    }

    #[test]
    fn search_requires_full_text_for_role() -> Result<()> {
        let (_temp, app, _summary) = build_app_fixture(Vec::new())?;
        let cmd = SearchCommand {
            term: Some("hello".into()),
            full_text: false,
            provider: None,
            since: None,
            role: Some("user".into()),
            limit: None,
        };
        let err = app
            .search(&cmd)
            .expect_err("role filter without full-text should error");
        assert!(
            err.to_string().contains("--role requires --full-text"),
            "unexpected error: {err:?}"
        );
        Ok(())
    }

    #[test]
    fn app_run_ui_starts_event_loop() -> Result<()> {
        let (_temp, mut app, _) = build_app_fixture(Vec::new())?;
        app.run_ui()?;
        Ok(())
    }

    #[test]
    fn app_export_errors_when_session_missing() -> Result<()> {
        let (_temp, app, _) = build_app_fixture(Vec::new())?;
        let cmd = ExportCommand {
            session_id: "missing".into(),
        };
        let err = app.export(&cmd).unwrap_err();
        assert!(err.to_string().contains("not found"));
        Ok(())
    }

    #[test]
    fn run_doctor_reports_missing_session_root() -> Result<()> {
        let (temp, app, _summary) = build_app_fixture(Vec::new())?;
        let sessions_dir = app
            .loaded
            .config
            .providers
            .get("codex")
            .unwrap()
            .session_roots
            .first()
            .unwrap()
            .clone();
        if sessions_dir.exists() {
            fs::remove_dir_all(&sessions_dir)?;
        }
        run_doctor(&app.loaded, &app.db)?;
        temp.close()?;
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn check_prompt_assembler_handles_unavailable() -> Result<()> {
        let temp = TempDir::new()?;
        let script = temp.child("pa");
        script.write_str("#!/bin/sh\nexit 1\n")?;
        let perms = fs::Permissions::from_mode(0o755);
        fs::set_permissions(script.path(), perms)?;

        let mut paths =
            env::split_paths(&std::env::var_os("PATH").unwrap_or_default()).collect::<Vec<_>>();
        paths.insert(0, script.path().parent().unwrap().to_path_buf());
        let joined = env::join_paths(paths)?;
        unsafe {
            std::env::set_var("PATH", joined);
        }

        let cfg = PromptAssemblerConfig {
            namespace: "tests".into(),
        };
        check_prompt_assembler(&cfg);
        Ok(())
    }

    #[test]
    fn bootstrap_initializes_prompt_feature() -> Result<()> {
        let _guard = ENV_LOCK.lock().unwrap();
        let temp = TempDir::new()?;
        let config_dir = temp.child("config");
        config_dir.create_dir_all()?;
        let sessions_dir = config_dir.child("sessions");
        sessions_dir.create_dir_all()?;
        let codex_home = temp.child("codex-home");
        codex_home.create_dir_all()?;
        let home_dir = temp.child("home");
        home_dir.create_dir_all()?;
        let config_toml = format!(
            r#"
provider = "codex"

[providers.codex]
bin = "echo"
session_roots = ["{root}"]

[features.pa]
enabled = true
namespace = "tests"
"#,
            root = toml_path(sessions_dir.path()),
        );
        config_dir.child("config.toml").write_str(&config_toml)?;

        let data_dir = temp.child("data");
        data_dir.create_dir_all()?;
        let cache_dir = temp.child("cache");
        cache_dir.create_dir_all()?;

        let _data_guard = EnvOverride::set_path("TX_DATA_DIR", data_dir.path());
        let _cache_guard = EnvOverride::set_path("TX_CACHE_DIR", cache_dir.path());
        let _codex_guard = EnvOverride::set_path("CODEX_HOME", codex_home.path());
        let _home_guard = EnvOverride::set_path("HOME", home_dir.path());
        let _profile_guard = EnvOverride::set_path("USERPROFILE", home_dir.path());

        let cli = Cli {
            config_dir: Some(config_dir.path().to_path_buf()),
            verbose: 0,
            quiet: false,
            command: None,
        };

        let app = App::bootstrap(&cli)?;
        assert!(app.prompt.is_some());
        assert_eq!(app.loaded.directories.config_dir, config_dir.path());
        assert_eq!(app.loaded.directories.data_dir, data_dir.path());
        assert_eq!(app.loaded.directories.cache_dir, cache_dir.path());
        assert_eq!(app.db.count_sessions()?, 0);
        Ok(())
    }
}

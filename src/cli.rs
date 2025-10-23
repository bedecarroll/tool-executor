use std::path::PathBuf;

use clap::{ArgAction, Args, Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(author, version, about, long_about = None, name = "tx", bin_name = "tx")]
pub struct Cli {
    /// Override the configuration directory.
    #[arg(long, value_name = "DIR")]
    pub config_dir: Option<PathBuf>,
    /// Emit machine-readable output when supported.
    #[arg(long, action = ArgAction::SetTrue, global = true)]
    pub json: bool,
    /// Print the fully-resolved command instead of executing it.
    #[arg(long, action = ArgAction::SetTrue, global = true)]
    pub emit_command: bool,
    /// Increase log verbosity (use -vv for trace).
    #[arg(short, long, action = ArgAction::Count, global = true)]
    pub verbose: u8,
    /// Silence all log output.
    #[arg(short, long, action = ArgAction::SetTrue, global = true)]
    pub quiet: bool,
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Search session transcripts.
    Search(SearchCommand),
    /// Launch a new session pipeline.
    Launch(LaunchCommand),
    /// Resume an existing session pipeline.
    Resume(ResumeCommand),
    /// Export a session transcript.
    Export(ExportCommand),
    /// Inspect configuration files.
    #[command(subcommand)]
    Config(ConfigCommand),
    /// Run environment diagnostics.
    Doctor,
    /// Update tx to the latest released version.
    #[cfg(feature = "self-update")]
    SelfUpdate(SelfUpdateCommand),
}

#[derive(Debug, Args)]
pub struct SearchCommand {
    /// Search term to find (omit for latest sessions).
    pub term: Option<String>,
    /// Search the full transcript instead of just the first prompt.
    #[arg(long, action = ArgAction::SetTrue)]
    pub full_text: bool,
    /// Restrict to a specific provider.
    #[arg(long)]
    pub provider: Option<String>,
    /// Only include sessions active since this duration ago (e.g. 7d, 12h).
    #[arg(long, value_parser = parse_since)]
    pub since: Option<i64>,
    /// Maximum number of sessions to return.
    #[arg(long)]
    pub limit: Option<usize>,
    /// Emit JSON results.
    #[arg(long, action = ArgAction::SetTrue)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct LaunchCommand {
    /// Provider to launch (overridden by --profile when specified).
    pub provider: String,
    /// Profile to apply (pre/post/wrap/provider overrides).
    #[arg(long)]
    pub profile: Option<String>,
    /// Append an additional pre snippet by name (repeatable).
    #[arg(long = "pre", action = ArgAction::Append)]
    pub pre_snippets: Vec<String>,
    /// Append an additional post snippet by name (repeatable).
    #[arg(long = "post", action = ArgAction::Append)]
    pub post_snippets: Vec<String>,
    /// Override the wrapper by name.
    #[arg(long)]
    pub wrap: Option<String>,
    /// Provide a variable binding (KEY=VALUE).
    #[arg(long = "var", action = ArgAction::Append)]
    pub vars: Vec<String>,
    /// Print the final command and exit without running it.
    #[arg(long, action = ArgAction::SetTrue)]
    pub dry_run: bool,
    /// Arguments forwarded to the provider after `--`.
    #[arg(last = true)]
    pub provider_args: Vec<String>,
}

#[derive(Debug, Args)]
pub struct ResumeCommand {
    /// Session identifier to resume.
    pub session_id: String,
    /// Optional profile to merge.
    #[arg(long)]
    pub profile: Option<String>,
    /// Append an additional pre snippet by name (repeatable).
    #[arg(long = "pre", action = ArgAction::Append)]
    pub pre_snippets: Vec<String>,
    /// Append an additional post snippet by name (repeatable).
    #[arg(long = "post", action = ArgAction::Append)]
    pub post_snippets: Vec<String>,
    /// Override the wrapper by name.
    #[arg(long)]
    pub wrap: Option<String>,
    /// Provide a variable binding (KEY=VALUE).
    #[arg(long = "var", action = ArgAction::Append)]
    pub vars: Vec<String>,
    /// Print the final command and exit without running it.
    #[arg(long, action = ArgAction::SetTrue)]
    pub dry_run: bool,
    /// Arguments forwarded to the provider after `--`.
    #[arg(last = true)]
    pub provider_args: Vec<String>,
}

#[derive(Debug, Args)]
pub struct ExportCommand {
    /// Session identifier to export.
    pub session_id: String,
}

#[derive(Debug, Subcommand)]
pub enum ConfigCommand {
    /// List configured providers, profiles, and wrappers.
    List,
    /// Dump the merged configuration TOML.
    Dump,
    /// Show configuration search paths.
    Where,
    /// Validate configuration references.
    Lint,
    /// Print the bundled default configuration.
    Default(ConfigDefaultCommand),
}

#[derive(Debug, Args)]
pub struct ConfigDefaultCommand {
    /// Show the raw bundled template without resolving runtime paths.
    #[arg(long, action = ArgAction::SetTrue)]
    pub raw: bool,
}

fn parse_since(raw: &str) -> Result<i64, String> {
    humantime::parse_duration(raw)
        .map(|duration| i64::try_from(duration.as_secs()).unwrap_or(i64::MAX))
        .map_err(|err| format!("invalid duration '{raw}': {err}"))
}

#[cfg(feature = "self-update")]
#[derive(Debug, Args)]
pub struct SelfUpdateCommand {
    /// Update to a specific release tag (defaults to the latest).
    #[arg(long, value_name = "TAG")]
    pub version: Option<String>,
}

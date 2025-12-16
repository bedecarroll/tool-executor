use std::path::PathBuf;

use clap::{ArgAction, Args, Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(author, version, about, long_about = None, name = "tx", bin_name = "tx")]
pub struct Cli {
    /// Override the configuration directory.
    #[arg(long, value_name = "DIR")]
    pub config_dir: Option<PathBuf>,
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
    SelfUpdate(SelfUpdateCommand),
    /// Internal helpers (unstable, subject to change).
    #[command(subcommand, hide = true)]
    Internal(InternalCommand),
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
    /// Restrict results to messages with this role (user or assistant).
    #[arg(long, value_parser = parse_role)]
    pub role: Option<String>,
    /// Maximum number of sessions to return.
    #[arg(long)]
    pub limit: Option<usize>,
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
    /// Print the fully-resolved command instead of executing it.
    #[arg(long, action = ArgAction::SetTrue)]
    pub emit_command: bool,
    /// Emit pipeline details as JSON when combined with --dry-run or --emit-command.
    #[arg(long, action = ArgAction::SetTrue)]
    pub emit_json: bool,
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
pub enum InternalCommand {
    /// Run a provider after capturing stdin as a positional prompt argument.
    #[command(name = "capture-arg", hide = true)]
    CaptureArg(InternalCaptureArgCommand),
    /// Assemble a prompt via the prompt-assembler CLI, prompting for missing arguments.
    #[command(name = "prompt-assembler", hide = true)]
    PromptAssembler(InternalPromptAssemblerCommand),
}

#[derive(Debug, Args)]
pub struct InternalCaptureArgCommand {
    /// Provider name (for diagnostics only).
    #[arg(long)]
    pub provider: String,
    /// Executable to invoke for the provider.
    #[arg(long)]
    pub bin: String,
    /// Commands that produce the prompt before launching the provider.
    #[arg(long = "pre", action = ArgAction::Append)]
    pub pre_commands: Vec<String>,
    /// Arguments forwarded to the provider before inserting the prompt.
    #[arg(long = "arg", action = ArgAction::Append, allow_hyphen_values = true)]
    pub provider_args: Vec<String>,
    /// Maximum captured prompt size in bytes.
    #[arg(long = "prompt-limit", default_value = "1048576")]
    pub prompt_limit: usize,
}

#[derive(Debug, Args)]
pub struct InternalPromptAssemblerCommand {
    /// Prompt name to render via the prompt-assembler.
    #[arg(long)]
    pub prompt: String,
    /// Additional arguments forwarded to the prompt-assembler.
    #[arg(long = "arg", action = ArgAction::Append, allow_hyphen_values = true)]
    pub prompt_args: Vec<String>,
    /// Maximum assembled prompt size in bytes.
    #[arg(long = "prompt-limit", default_value = "1048576")]
    pub prompt_limit: usize,
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
    /// Emit the JSON Schema for configuration files.
    Schema(ConfigSchemaCommand),
}

#[derive(Debug, Args)]
pub struct ConfigDefaultCommand {
    /// Show the raw bundled template without resolving runtime paths.
    #[arg(long, action = ArgAction::SetTrue)]
    pub raw: bool,
}

#[derive(Debug, Args)]
pub struct ConfigSchemaCommand {
    /// Pretty-print the generated schema.
    #[arg(long, action = ArgAction::SetTrue)]
    pub pretty: bool,
}

fn parse_since(raw: &str) -> Result<i64, String> {
    humantime::parse_duration(raw)
        .map(|duration| i64::try_from(duration.as_secs()).unwrap_or(i64::MAX))
        .map_err(|err| format!("invalid duration '{raw}': {err}"))
}

fn parse_role(raw: &str) -> Result<String, String> {
    match raw.to_ascii_lowercase().as_str() {
        "user" | "assistant" => Ok(raw.to_ascii_lowercase()),
        other => Err(format!(
            "invalid role '{other}', expected 'user' or 'assistant'"
        )),
    }
}

#[derive(Debug, Args)]
pub struct SelfUpdateCommand {
    /// Update to a specific release tag (defaults to the latest).
    #[arg(long, value_name = "TAG")]
    pub version: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_search_with_full_text_role_and_since() {
        let cli = Cli::try_parse_from([
            "tx",
            "search",
            "--full-text",
            "--role",
            "user",
            "--since",
            "5m",
            "--limit",
            "10",
            "context",
        ])
        .expect("parse search command");

        let Command::Search(cmd) = cli.command.expect("search command") else {
            panic!("expected search command");
        };
        assert!(cmd.full_text);
        assert_eq!(cmd.term.as_deref(), Some("context"));
        assert_eq!(cmd.role.as_deref(), Some("user"));
        assert_eq!(cmd.since, Some(300));
        assert_eq!(cmd.limit, Some(10));
    }

    #[test]
    fn parse_search_rejects_invalid_role() {
        let err = Cli::try_parse_from(["tx", "search", "--full-text", "--role", "admin", "term"])
            .expect_err("invalid role should fail");
        let message = err.to_string();
        assert!(message.contains("invalid role 'admin'"));
    }

    #[test]
    fn parse_search_rejects_bad_since() {
        let err = Cli::try_parse_from(["tx", "search", "--since", "later"])
            .expect_err("invalid duration should fail");
        let message = err.to_string();
        assert!(message.contains("invalid duration 'later'"));
    }

    #[test]
    fn parse_resume_collects_provider_args() {
        let cli = Cli::try_parse_from([
            "tx",
            "resume",
            "sess-1",
            "--emit-command",
            "--",
            "--flag",
            "value",
        ])
        .expect("parse resume");

        let Command::Resume(cmd) = cli.command.expect("resume command") else {
            panic!("expected resume command");
        };
        assert_eq!(cmd.session_id, "sess-1");
        assert!(cmd.emit_command);
        assert_eq!(cmd.provider_args, vec!["--flag", "value"]);
    }

    #[test]
    fn parse_config_default_raw_flag() {
        let cli = Cli::try_parse_from(["tx", "config", "default", "--raw"])
            .expect("parse config default");
        let Command::Config(ConfigCommand::Default(cmd)) = cli.command.expect("config command")
        else {
            panic!("expected config default command");
        };
        assert!(cmd.raw);
    }
}

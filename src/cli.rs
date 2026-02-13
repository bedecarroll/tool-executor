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
    /// Show usage statistics.
    #[command(subcommand)]
    Stats(StatsCommand),
    /// Inspect configuration files.
    #[command(subcommand)]
    Config(ConfigCommand),
    /// Manage the local database.
    #[command(subcommand)]
    Db(DbCommand),
    /// Experimental semantic retrieval over indexed session history.
    #[command(subcommand)]
    Rag(RagCommand),
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
    /// Session identifier to resume (use 'last' for the most recent session).
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
pub enum StatsCommand {
    /// Show Codex usage statistics.
    Codex,
}

#[derive(Debug, Subcommand)]
pub enum DbCommand {
    /// Delete the indexed session database.
    Reset(DbResetCommand),
}

#[derive(Debug, Subcommand)]
pub enum RagCommand {
    /// Experimental: generate and store embeddings for session history chunks.
    Index(RagIndexCommand),
    /// Experimental: run semantic KNN search over indexed history chunks.
    Search(RagSearchCommand),
}

#[derive(Debug, Args)]
pub struct RagIndexCommand {
    /// Only index rows at or after this unix timestamp in milliseconds.
    #[arg(long)]
    pub since: Option<i64>,
    /// Restrict indexing to a single session id.
    #[arg(long)]
    pub session: Option<String>,
    /// Delete scoped vectors before rebuilding.
    #[arg(long, action = ArgAction::SetTrue)]
    pub reindex: bool,
    /// Number of chunks to process per embeddings batch.
    #[arg(long, default_value_t = 64, value_parser = parse_positive_usize)]
    pub batch_size: usize,
}

#[derive(Debug, Args)]
pub struct RagSearchCommand {
    /// Natural-language query to embed and search against history vectors.
    #[arg(long)]
    pub query: String,
    /// Number of nearest neighbors to return.
    #[arg(long, default_value_t = 20, value_parser = parse_positive_usize)]
    pub k: usize,
    /// Restrict to a single session id.
    #[arg(long)]
    pub session: Option<String>,
    /// Restrict to a specific tool/source name.
    #[arg(long)]
    pub tool: Option<String>,
    /// Only include results at or after this unix timestamp in milliseconds.
    #[arg(long)]
    pub since: Option<i64>,
    /// Only include results at or before this unix timestamp in milliseconds.
    #[arg(long)]
    pub until: Option<i64>,
    /// Emit structured JSON instead of the default text output.
    #[arg(long, action = ArgAction::SetTrue)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct DbResetCommand {
    /// Confirm deleting the database files.
    #[arg(long, action = ArgAction::SetTrue)]
    pub yes: bool,
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

fn parse_positive_usize(raw: &str) -> Result<usize, String> {
    let value = raw
        .parse::<usize>()
        .map_err(|err| format!("invalid positive integer '{raw}': {err}"))?;
    if value == 0 {
        return Err(format!("invalid positive integer '{raw}': must be > 0"));
    }
    Ok(value)
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

    fn into_search(command: Command) -> Option<SearchCommand> {
        if let Command::Search(cmd) = command {
            Some(cmd)
        } else {
            None
        }
    }

    fn into_resume(command: Command) -> Option<ResumeCommand> {
        if let Command::Resume(cmd) = command {
            Some(cmd)
        } else {
            None
        }
    }

    fn into_config_default(command: Command) -> Option<ConfigDefaultCommand> {
        if let Command::Config(ConfigCommand::Default(cmd)) = command {
            Some(cmd)
        } else {
            None
        }
    }

    fn into_rag_search(command: Command) -> Option<RagSearchCommand> {
        if let Command::Rag(RagCommand::Search(cmd)) = command {
            Some(cmd)
        } else {
            None
        }
    }

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

        let cmd = cli.command.and_then(into_search).expect("search command");
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
    fn parse_role_accepts_assistant() {
        let role = parse_role("assistant").expect("assistant is valid");
        assert_eq!(role, "assistant");
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

        let cmd = cli.command.and_then(into_resume).expect("resume command");
        assert_eq!(cmd.session_id, "sess-1");
        assert!(cmd.emit_command);
        assert_eq!(cmd.provider_args, vec!["--flag", "value"]);
    }

    #[test]
    fn parse_config_default_raw_flag() {
        let cli = Cli::try_parse_from(["tx", "config", "default", "--raw"])
            .expect("parse config default");
        let cmd = cli
            .command
            .and_then(into_config_default)
            .expect("config default command");
        assert!(cmd.raw);
    }

    #[test]
    fn parse_stats_codex() {
        let cli = Cli::try_parse_from(["tx", "stats", "codex"]).expect("parse stats");
        assert!(
            cli.command
                .is_some_and(|command| matches!(command, Command::Stats(StatsCommand::Codex)))
        );
    }

    #[test]
    fn parse_rag_index_command() {
        let cli = Cli::try_parse_from([
            "tx",
            "rag",
            "index",
            "--since",
            "1000",
            "--session",
            "sess-1",
            "--reindex",
            "--batch-size",
            "32",
        ])
        .expect("parse rag index");

        assert!(cli.command.is_some_and(|command| matches!(
            command,
            Command::Rag(RagCommand::Index(RagIndexCommand {
                since: Some(1000),
                session: Some(_),
                reindex: true,
                batch_size: 32
            }))
        )));
    }

    #[test]
    fn parse_rag_search_command() {
        let cli = Cli::try_parse_from([
            "tx",
            "rag",
            "search",
            "--query",
            "find timeout bug",
            "--k",
            "7",
            "--session",
            "sess-2",
            "--tool",
            "event_msg",
            "--since",
            "10",
            "--until",
            "20",
            "--json",
        ])
        .expect("parse rag search");

        let cmd = cli
            .command
            .and_then(into_rag_search)
            .expect("rag search command");
        assert_eq!(cmd.query, "find timeout bug");
        assert_eq!(cmd.k, 7);
        assert_eq!(cmd.session.as_deref(), Some("sess-2"));
        assert_eq!(cmd.tool.as_deref(), Some("event_msg"));
        assert_eq!(cmd.since, Some(10));
        assert_eq!(cmd.until, Some(20));
        assert!(cmd.json);
    }

    #[test]
    fn parse_rag_search_rejects_zero_k() {
        let err = Cli::try_parse_from([
            "tx",
            "rag",
            "search",
            "--query",
            "find timeout bug",
            "--k",
            "0",
        ])
        .expect_err("zero k should fail");
        let message = err.to_string();
        assert!(message.contains("must be > 0"));
    }

    #[test]
    fn command_extractors_return_none_for_mismatch() {
        assert!(into_search(Command::Stats(StatsCommand::Codex)).is_none());
        assert!(into_resume(Command::Stats(StatsCommand::Codex)).is_none());
        assert!(into_config_default(Command::Stats(StatsCommand::Codex)).is_none());
        assert!(into_rag_search(Command::Stats(StatsCommand::Codex)).is_none());
    }
}

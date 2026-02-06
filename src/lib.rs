#![allow(unexpected_cfgs)]

pub mod commands;
pub mod config;
pub mod db;
pub mod indexer;
pub mod pipeline;
pub mod prompts;
pub mod providers;
pub mod session;

mod app;
pub mod cli;
pub mod internal;
#[doc(hidden)]
pub mod test_support;
mod tui;
mod util;

use clap::{CommandFactory, Parser};
use cli::Command;

pub use cli::Cli;

/// Run the tx CLI entrypoint.
///
/// # Errors
///
/// Returns an error when initialization or the chosen command fails to execute.
pub fn run(cli: &Cli) -> color_eyre::Result<()> {
    init_tracing(cli);

    if let Some(Command::Internal(cmd)) = &cli.command {
        return internal::run(cmd);
    }

    if let Some(Command::Db(cmd)) = &cli.command {
        return commands::db::run(cmd, cli.config_dir.as_deref(), cli.quiet);
    }

    let mut app = app::App::bootstrap(cli)?;

    match &cli.command {
        Some(Command::Search(cmd)) => app.search(cmd),
        Some(Command::Resume(cmd)) => app.resume(cmd),
        Some(Command::Export(cmd)) => app.export(cmd),
        Some(Command::Stats(cmd)) => app.stats(cmd),
        Some(Command::Config(cmd)) => app.config(cmd),
        Some(Command::Doctor) => app.doctor(),
        Some(Command::Db(_)) => unreachable!("db command handled before bootstrap"),
        Some(Command::Internal(_)) => unreachable!("internal command handled above"),
        Some(Command::SelfUpdate(cmd)) => app.self_update(cmd),
        None => app.run_ui(),
    }
}

fn init_tracing(cli: &Cli) {
    let level = desired_level(cli);
    let mut filter = tracing_subscriber::EnvFilter::builder()
        .with_default_directive(level.into())
        .from_env_lossy();
    if let Ok(directive) = "tui_markdown=off".parse::<tracing_subscriber::filter::Directive>() {
        filter = filter.add_directive(directive);
    }

    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .with_writer(std::io::stderr)
        .try_init();
}

fn desired_level(cli: &Cli) -> tracing::level_filters::LevelFilter {
    if cli.quiet {
        return tracing::level_filters::LevelFilter::ERROR;
    }

    match cli.verbose {
        0 => tracing::level_filters::LevelFilter::INFO,
        1 => tracing::level_filters::LevelFilter::DEBUG,
        _ => tracing::level_filters::LevelFilter::TRACE,
    }
}

#[must_use]
pub fn command() -> clap::Command {
    Cli::command()
}

/// Parse CLI arguments.
#[must_use]
pub fn parse_cli() -> Cli {
    Cli::parse()
}

/// Return a suggested process exit code for a CLI error.
#[must_use]
pub fn exit_code_for_error(err: &color_eyre::Report) -> i32 {
    if let Some(app_error) = err.downcast_ref::<app::AppError>()
        && matches!(app_error, app::AppError::ProviderMismatch { .. })
    {
        return 2;
    }
    1
}

/// Write a CLI error and its cause chain to the provided writer.
///
/// The first line is prefixed with `tx:` followed by any chained causes.
///
/// # Errors
///
/// Returns an error when writing to `writer` fails.
pub fn write_cli_error(
    err: &color_eyre::Report,
    mut writer: impl std::io::Write,
) -> std::io::Result<()> {
    let mut chain = err.chain();
    if let Some(head) = chain.next() {
        writeln!(writer, "tx: {head}")?;
    }
    for cause in chain {
        writeln!(writer, "    caused by: {cause}")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{Command, ResumeCommand};
    #[cfg(unix)]
    use crate::cli::{InternalCaptureArgCommand, InternalCommand};
    use crate::db::Database;
    use crate::session::{MessageRecord, SessionIngest, SessionSummary};
    use crate::test_support::{ENV_LOCK, toml_path};
    use assert_fs::TempDir;
    use assert_fs::prelude::*;
    #[cfg(unix)]
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn desired_level_handles_quiet_and_verbose() {
        let mut cli = Cli {
            config_dir: None,
            verbose: 0,
            quiet: false,
            command: None,
        };
        assert_eq!(
            desired_level(&cli),
            tracing::level_filters::LevelFilter::INFO
        );

        cli.verbose = 1;
        assert_eq!(
            desired_level(&cli),
            tracing::level_filters::LevelFilter::DEBUG
        );

        cli.verbose = 2;
        assert_eq!(
            desired_level(&cli),
            tracing::level_filters::LevelFilter::TRACE
        );

        cli.quiet = true;
        assert_eq!(
            desired_level(&cli),
            tracing::level_filters::LevelFilter::ERROR
        );
    }

    #[test]
    fn command_factory_returns_named_command() {
        let cmd = command();
        assert_eq!(cmd.get_name(), "tx");
        assert!(cmd.get_about().is_some());
    }

    #[test]
    fn exit_code_for_error_maps_provider_mismatch() {
        let mismatch = color_eyre::Report::new(app::AppError::ProviderMismatch {
            expected: "codex".into(),
            actual: "alt".into(),
        });
        assert_eq!(exit_code_for_error(&mismatch), 2);

        let other = color_eyre::eyre::eyre!("boom");
        assert_eq!(exit_code_for_error(&other), 1);
    }

    #[test]
    fn write_cli_error_renders_error_chain() {
        let err = color_eyre::eyre::eyre!("root")
            .wrap_err("middle")
            .wrap_err("top");
        let mut output = Vec::new();
        write_cli_error(&err, &mut output).expect("write should succeed");
        let rendered = String::from_utf8(output).expect("utf8");
        assert!(rendered.contains("tx: top"));
        assert!(rendered.contains("caused by: middle"));
        assert!(rendered.contains("caused by: root"));
    }

    #[test]
    fn run_executes_config_where_command() -> color_eyre::Result<()> {
        let _guard = ENV_LOCK.lock().unwrap();
        let temp = TempDir::new()?;
        let config_dir = temp.child("config");
        config_dir.create_dir_all()?;
        let sessions_dir = config_dir.child("sessions");
        sessions_dir.create_dir_all()?;
        let config_toml = format!(
            r#"
provider = "codex"

[providers.codex]
bin = "echo"
session_roots = ["{root}"]
"#,
            root = toml_path(sessions_dir.path()),
        );
        config_dir.child("config.toml").write_str(&config_toml)?;

        let data_dir = temp.child("data");
        data_dir.create_dir_all()?;
        let cache_dir = temp.child("cache");
        cache_dir.create_dir_all()?;

        unsafe {
            std::env::set_var("TX_DATA_DIR", "preset-data");
            std::env::set_var("TX_CACHE_DIR", "preset-cache");
        }
        let original_data = std::env::var("TX_DATA_DIR").ok();
        let original_cache = std::env::var("TX_CACHE_DIR").ok();
        unsafe {
            std::env::set_var("TX_DATA_DIR", data_dir.path());
            std::env::set_var("TX_CACHE_DIR", cache_dir.path());
        }

        let cli = Cli {
            config_dir: Some(config_dir.path().to_path_buf()),
            verbose: 0,
            quiet: false,
            command: Some(Command::Config(crate::cli::ConfigCommand::Where)),
        };

        run(&cli)?;

        if let Some(value) = original_data {
            unsafe { std::env::set_var("TX_DATA_DIR", value) };
        } else {
            unsafe { std::env::remove_var("TX_DATA_DIR") };
        }
        if let Some(value) = original_cache {
            unsafe { std::env::set_var("TX_CACHE_DIR", value) };
        } else {
            unsafe { std::env::remove_var("TX_CACHE_DIR") };
        }

        Ok(())
    }

    #[test]
    fn run_invokes_ui_when_no_command() -> color_eyre::Result<()> {
        let _guard = ENV_LOCK.lock().unwrap();
        let temp = TempDir::new()?;
        let config_dir = temp.child("config");
        config_dir.create_dir_all()?;
        let sessions_dir = config_dir.child("sessions");
        sessions_dir.create_dir_all()?;
        let config_toml = format!(
            r#"
provider = "codex"

[providers.codex]
bin = "echo"
session_roots = ["{root}"]
"#,
            root = toml_path(sessions_dir.path()),
        );
        config_dir.child("config.toml").write_str(&config_toml)?;

        let data_dir = temp.child("data");
        data_dir.create_dir_all()?;
        let cache_dir = temp.child("cache");
        cache_dir.create_dir_all()?;

        unsafe {
            std::env::set_var("TX_DATA_DIR", "preset-data");
            std::env::set_var("TX_CACHE_DIR", "preset-cache");
        }
        let original_data = std::env::var("TX_DATA_DIR").ok();
        let original_cache = std::env::var("TX_CACHE_DIR").ok();
        unsafe {
            std::env::set_var("TX_DATA_DIR", data_dir.path());
            std::env::set_var("TX_CACHE_DIR", cache_dir.path());
        }

        let cli = Cli {
            config_dir: Some(config_dir.path().to_path_buf()),
            verbose: 0,
            quiet: false,
            command: None,
        };

        run(&cli)?;

        if let Some(value) = original_data {
            unsafe { std::env::set_var("TX_DATA_DIR", value) };
        } else {
            unsafe { std::env::remove_var("TX_DATA_DIR") };
        }
        if let Some(value) = original_cache {
            unsafe { std::env::set_var("TX_CACHE_DIR", value) };
        } else {
            unsafe { std::env::remove_var("TX_CACHE_DIR") };
        }
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn run_executes_internal_capture_arg_command() -> color_eyre::Result<()> {
        let _guard = ENV_LOCK.lock().unwrap();
        let temp = TempDir::new()?;
        let output = temp.child("prompt.txt");
        let script = temp.child("provider.sh");
        script.write_str("#!/bin/sh\nprintf '%s' \"$2\" > \"$1\"\n")?;
        let perms = fs::Permissions::from_mode(0o755);
        fs::set_permissions(script.path(), perms)?;

        unsafe {
            std::env::set_var("TX_CAPTURE_STDIN_DATA", "original");
        }
        let original = std::env::var("TX_CAPTURE_STDIN_DATA").ok();
        unsafe {
            std::env::set_var("TX_CAPTURE_STDIN_DATA", "payload");
        }

        let cli = Cli {
            config_dir: None,
            verbose: 0,
            quiet: false,
            command: Some(Command::Internal(InternalCommand::CaptureArg(
                InternalCaptureArgCommand {
                    provider: "demo".into(),
                    bin: script.path().display().to_string(),
                    pre_commands: Vec::new(),
                    provider_args: vec![output.path().display().to_string(), "{prompt}".into()],
                    prompt_limit: 128,
                },
            ))),
        };

        run(&cli)?;

        if let Some(value) = original {
            unsafe { std::env::set_var("TX_CAPTURE_STDIN_DATA", value) };
        } else {
            unsafe { std::env::remove_var("TX_CAPTURE_STDIN_DATA") };
        }

        let contents = std::fs::read_to_string(output.path())?;
        assert_eq!(contents, "payload");
        Ok(())
    }

    #[test]
    fn run_returns_error_for_provider_mismatch() -> color_eyre::Result<()> {
        let _guard = ENV_LOCK.lock().unwrap();
        let temp = TempDir::new()?;
        let config_dir = temp.child("config");
        config_dir.create_dir_all()?;
        let sessions_dir = config_dir.child("sessions");
        sessions_dir.create_dir_all()?;
        let config_toml = format!(
            r#"
provider = "demo"

[providers.demo]
bin = "echo"
session_roots = ["{root}"]

[profiles.default]
provider = "demo"

[profiles.alt]
provider = "alt"
"#,
            root = toml_path(sessions_dir.path()),
        );
        config_dir.child("config.toml").write_str(&config_toml)?;

        let data_dir = temp.child("data");
        data_dir.create_dir_all()?;
        let cache_dir = temp.child("cache");
        cache_dir.create_dir_all()?;

        unsafe {
            std::env::set_var("TX_DATA_DIR", "preset-data");
            std::env::set_var("TX_CACHE_DIR", "preset-cache");
        }
        let original_data = std::env::var("TX_DATA_DIR").ok();
        let original_cache = std::env::var("TX_CACHE_DIR").ok();
        unsafe {
            std::env::set_var("TX_DATA_DIR", data_dir.path());
            std::env::set_var("TX_CACHE_DIR", cache_dir.path());
        }

        let db_path = data_dir.child("tx.sqlite3");
        let mut db = Database::open(db_path.path())?;
        let session_path = sessions_dir.child("sess-1.jsonl");
        session_path.write_str("{\"type\":\"event_msg\",\"payload\":{\"type\":\"user_message\",\"message\":\"hello\"}}\n")?;

        let summary = SessionSummary {
            id: "sess-1".into(),
            provider: "demo".into(),
            wrapper: None,
            model: None,
            label: Some("Demo".into()),
            path: session_path.path().to_path_buf(),
            uuid: Some("uuid-1".into()),
            first_prompt: Some("Hello".into()),
            actionable: true,
            created_at: Some(0),
            started_at: Some(0),
            last_active: Some(0),
            size: 1,
            mtime: 0,
        };
        let mut message = MessageRecord::new(summary.id.clone(), 0, "user", "Hello", None, Some(0));
        message.is_first = true;
        db.upsert_session(&SessionIngest::new(summary.clone(), vec![message]))?;

        let session_id = summary.id.clone();
        let cli = Cli {
            config_dir: Some(config_dir.path().to_path_buf()),
            verbose: 0,
            quiet: false,
            command: Some(Command::Resume(ResumeCommand {
                session_id,
                profile: Some("alt".into()),
                pre_snippets: Vec::new(),
                post_snippets: Vec::new(),
                wrap: None,
                emit_command: false,
                emit_json: false,
                vars: Vec::new(),
                dry_run: false,
                provider_args: Vec::new(),
            })),
        };

        let err = run(&cli).expect_err("expected provider mismatch error");
        let message = err.to_string();
        assert!(
            message.contains("provider mismatch"),
            "unexpected error: {message}"
        );

        if let Some(value) = original_data {
            unsafe { std::env::set_var("TX_DATA_DIR", value) };
        } else {
            unsafe { std::env::remove_var("TX_DATA_DIR") };
        }
        if let Some(value) = original_cache {
            unsafe { std::env::set_var("TX_CACHE_DIR", value) };
        } else {
            unsafe { std::env::remove_var("TX_CACHE_DIR") };
        }

        Ok(())
    }
}

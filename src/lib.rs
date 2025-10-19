pub mod config;
pub mod db;
pub mod indexer;
pub mod pipeline;
pub mod prompts;
pub mod session;

mod app;
pub mod cli;
mod tui;
mod util;

use clap::CommandFactory;
use cli::Command;

pub use cli::Cli;

pub fn run(cli: Cli) -> color_eyre::Result<()> {
    init_tracing(&cli);
    let mut app = app::App::bootstrap(&cli)?;

    let outcome = match &cli.command {
        Some(Command::Sessions(cmd)) => app.list_sessions(cmd),
        Some(Command::Search(cmd)) => app.search(cmd),
        Some(Command::Launch(cmd)) => app.launch(cmd),
        Some(Command::Resume(cmd)) => app.resume(cmd),
        Some(Command::Export(cmd)) => app.export(cmd),
        Some(Command::Config(cmd)) => app.config(cmd),
        Some(Command::Doctor) => app.doctor(),
        #[cfg(feature = "self-update")]
        Some(Command::SelfUpdate(cmd)) => app.self_update(cmd),
        None => app.run_ui(),
    };

    match outcome {
        Ok(()) => Ok(()),
        Err(err) => {
            if let Some(app_error) = err.downcast_ref::<app::AppError>() {
                if matches!(app_error, app::AppError::ProviderMismatch { .. }) {
                    eprintln!("{}", app_error);
                    std::process::exit(2);
                }
            }
            Err(err)
        }
    }
}

fn init_tracing(cli: &Cli) {
    let level = desired_level(cli);
    let filter = tracing_subscriber::EnvFilter::builder()
        .with_default_directive(level.into())
        .from_env_lossy();

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

pub fn command() -> clap::Command {
    Cli::command()
}

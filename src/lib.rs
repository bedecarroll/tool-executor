use std::{
    fs::{self, File},
    io::{self, Write},
    path::PathBuf,
};

use clap::{ArgAction, CommandFactory, Parser, Subcommand, ValueEnum};
use clap_complete::{Shell, generate_to};
use color_eyre::eyre::Context;
use tracing::level_filters::LevelFilter;

mod commands;
mod config;

use commands::CommandContext;

pub use config::Config;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None, name = "tx", bin_name = "tx")]
pub struct Cli {
    /// Override the configuration directory.
    #[arg(long, value_name = "DIR")]
    config_dir: Option<PathBuf>,
    /// Increase log verbosity (use -vv for trace).
    #[arg(short, long, action = ArgAction::Count, global = true)]
    verbose: u8,
    /// Silence all log output.
    #[arg(short, long, action = ArgAction::SetTrue, global = true)]
    quiet: bool,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Print a friendly greeting.
    Greet(commands::greet::GreetCommand),
    /// Emit shell completion scripts.
    Completions {
        /// Target shell for the generated script.
        #[arg(value_enum)]
        shell: CompletionShell,
        /// Optional directory to write the script into (prints to stdout when omitted).
        #[arg(long, value_name = "DIR")]
        dir: Option<PathBuf>,
    },
    /// Generate a man page in roff format.
    Manpage {
        /// Optional directory to write the man page into (prints to stdout when omitted).
        #[arg(long, value_name = "DIR")]
        dir: Option<PathBuf>,
    },
    /// Update the CLI binary from GitHub releases.
    #[cfg(feature = "self-update")]
    SelfUpdate(commands::self_update::SelfUpdateCommand),
}

#[derive(Copy, Clone, Debug, ValueEnum)]
pub enum CompletionShell {
    Bash,
    Elvish,
    Fish,
    PowerShell,
    Zsh,
}

/// Execute the CLI entry point.
///
/// # Errors
///
/// Returns an error when writing to standard output fails.
pub fn run(cli: Cli) -> color_eyre::Result<()> {
    init_tracing(&cli);
    let config = Config::load(cli.config_dir.as_deref())?;
    tracing::debug!(?config, "Loaded configuration");
    let ctx = CommandContext::new(&config);

    match cli.command {
        Some(Command::Greet(cmd)) => {
            commands::greet::run(cmd, &ctx)?;
        }
        Some(Command::Completions { shell, dir }) => {
            let mut command = Cli::command();
            let bin_name = command.get_name().to_string();
            let shell_label = shell.label();
            let actual_shell = shell.into_shell();

            if let Some(output_dir) = dir {
                fs::create_dir_all(&output_dir)
                    .with_context(|| format!("failed to create {}", output_dir.display()))?;
                let path = generate_to(actual_shell, &mut command, &bin_name, &output_dir)
                    .with_context(|| {
                        format!("failed to write completions into {}", output_dir.display())
                    })?;
                tracing::info!(shell = shell_label, path = %path.display(), "Wrote completion script");
            } else {
                let mut stdout = io::stdout();
                clap_complete::generate(actual_shell, &mut command, bin_name, &mut stdout);
            }
        }
        Some(Command::Manpage { dir }) => {
            let command = Cli::command();
            let bin_name = command.get_name().to_string();
            let man = clap_mangen::Man::new(command.clone());
            let mut buffer = Vec::new();
            man.render(&mut buffer)?;

            if let Some(output_dir) = dir {
                fs::create_dir_all(&output_dir)
                    .with_context(|| format!("failed to create {}", output_dir.display()))?;
                let path = output_dir.join(format!("{bin_name}.1"));
                let mut file = File::create(&path)
                    .with_context(|| format!("failed to create {}", path.display()))?;
                file.write_all(&buffer)?;
                tracing::info!(path = %path.display(), "Wrote man page");
            } else {
                io::stdout().write_all(&buffer)?;
            }
        }
        #[cfg(feature = "self-update")]
        Some(Command::SelfUpdate(cmd)) => {
            commands::self_update::run(cmd)?;
        }
        None => {
            let mut command = Cli::command();
            command.print_help()?;
            println!();
        }
    }

    Ok(())
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

fn desired_level(cli: &Cli) -> LevelFilter {
    if cli.quiet {
        return LevelFilter::ERROR;
    }

    match cli.verbose {
        0 => LevelFilter::INFO,
        1 => LevelFilter::DEBUG,
        _ => LevelFilter::TRACE,
    }
}

impl CompletionShell {
    fn into_shell(self) -> Shell {
        match self {
            CompletionShell::Bash => Shell::Bash,
            CompletionShell::Elvish => Shell::Elvish,
            CompletionShell::Fish => Shell::Fish,
            CompletionShell::PowerShell => Shell::PowerShell,
            CompletionShell::Zsh => Shell::Zsh,
        }
    }

    fn label(self) -> &'static str {
        match self {
            CompletionShell::Bash => "bash",
            CompletionShell::Elvish => "elvish",
            CompletionShell::Fish => "fish",
            CompletionShell::PowerShell => "powershell",
            CompletionShell::Zsh => "zsh",
        }
    }
}

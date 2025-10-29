use clap::Parser;

#[cfg(target_os = "macos")]
use clap::{ColorChoice, CommandFactory, FromArgMatches};

fn main() -> color_eyre::Result<()> {
    color_eyre::install()?;
    let cli = parse_cli();
    tool_executor::run(&cli)
}

#[cfg(target_os = "macos")]
fn parse_cli() -> tool_executor::Cli {
    let matches = tool_executor::Cli::command()
        .color(ColorChoice::Never)
        .get_matches();
    tool_executor::Cli::from_arg_matches(&matches).unwrap_or_else(|err| err.exit())
}

#[cfg(not(target_os = "macos"))]
fn parse_cli() -> tool_executor::Cli {
    tool_executor::Cli::parse()
}

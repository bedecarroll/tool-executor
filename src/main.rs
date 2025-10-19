use clap::Parser;

fn main() -> color_eyre::Result<()> {
    color_eyre::install()?;
    let cli = tool_executor::Cli::parse();
    tool_executor::run(cli)
}

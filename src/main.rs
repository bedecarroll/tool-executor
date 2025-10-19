use clap::Parser;

fn main() -> color_eyre::Result<()> {
    color_eyre::install()?;
    let cli = llml::Cli::parse();
    llml::run(cli)
}

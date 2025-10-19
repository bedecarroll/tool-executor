use std::io::{self, Write};

use clap::Args;

use super::CommandContext;

#[derive(Args, Debug)]
pub struct GreetCommand {
    /// Person to greet.
    #[arg(short, long)]
    pub name: Option<String>,
}

pub fn run(cmd: GreetCommand, ctx: &CommandContext<'_>) -> color_eyre::Result<()> {
    let target = cmd
        .name
        .or_else(|| ctx.config.greet.default_name.clone())
        .unwrap_or_else(|| "world".to_string());

    writeln!(io::stdout(), "Hello, {target}!")?;
    tracing::info!(target, "Rendered greeting");

    Ok(())
}

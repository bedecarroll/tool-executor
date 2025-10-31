fn main() -> color_eyre::Result<()> {
    color_eyre::install()?;
    let cli = tool_executor::parse_cli();
    tool_executor::run(&cli)
}

fn main() -> color_eyre::Result<()> {
    color_eyre::install()?;
    let cli = tool_executor::parse_cli();
    match tool_executor::run(&cli) {
        Ok(()) => Ok(()),
        Err(err) => {
            let exit_code = tool_executor::exit_code_for_error(&err);
            tool_executor::write_cli_error(&err, &mut std::io::stderr())?;
            std::process::exit(exit_code);
        }
    }
}

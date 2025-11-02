fn main() -> color_eyre::Result<()> {
    color_eyre::install()?;
    let cli = tool_executor::parse_cli();
    match tool_executor::run(&cli) {
        Ok(()) => Ok(()),
        Err(err) => {
            use std::io::Write;

            let mut stderr = std::io::stderr();
            let mut chain = err.chain();
            if let Some(head) = chain.next() {
                writeln!(stderr, "tx: {head}")?;
            }
            for cause in chain {
                writeln!(stderr, "    caused by: {cause}")?;
            }
            std::process::exit(1);
        }
    }
}

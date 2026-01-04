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

#[cfg(test)]
mod tests {
    use assert_fs::TempDir;
    use tool_executor::{
        Cli,
        cli::{Command as TopLevelCommand, ConfigCommand},
        config::AppDirectories,
        test_support::ENV_LOCK,
    };
    use std::env;

    #[test]
    fn main_handles_successful_run() -> color_eyre::Result<()> {
        let _guard = ENV_LOCK.lock().unwrap();
        let temp = TempDir::new()?;
        let directories = AppDirectories {
            config_dir: temp.path().join("config"),
            data_dir: temp.path().join("data"),
            cache_dir: temp.path().join("cache"),
        };
        directories.ensure_all()?;

        // Provide the minimal config needed for a no-op run.
        std::fs::write(
            directories.config_dir.join("config.toml"),
            r#"provider = "echo"
[providers.echo]
bin = "echo"
session_roots = []
"#,
        )?;

        let cli = Cli {
            config_dir: Some(directories.config_dir.clone()),
            verbose: 0,
            quiet: false,
            command: Some(TopLevelCommand::Config(ConfigCommand::Where)),
        };

        // Should complete without invoking process::exit.
        let original_data = env::var("TX_DATA_DIR").ok();
        let original_cache = env::var("TX_CACHE_DIR").ok();
        unsafe {
            env::set_var("TX_DATA_DIR", &directories.data_dir);
            env::set_var("TX_CACHE_DIR", &directories.cache_dir);
        }

        let result = tool_executor::run(&cli);

        if let Some(value) = original_data {
            unsafe { env::set_var("TX_DATA_DIR", value) };
        } else {
            unsafe { env::remove_var("TX_DATA_DIR") };
        }
        if let Some(value) = original_cache {
            unsafe { env::set_var("TX_CACHE_DIR", value) };
        } else {
            unsafe { env::remove_var("TX_CACHE_DIR") };
        }

        result
    }
}

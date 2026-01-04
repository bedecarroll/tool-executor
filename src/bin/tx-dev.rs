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
    use std::env;
    use tool_executor::{
        Cli,
        cli::{Command as TopLevelCommand, ConfigCommand},
        config::AppDirectories,
        test_support::ENV_LOCK,
    };

    fn restore_env(key: &str, value: Option<String>) {
        if let Some(val) = value {
            unsafe { env::set_var(key, val) };
        } else {
            unsafe { env::remove_var(key) };
        }
    }

    fn restore_env_branches(key: &str, value: Option<String>) {
        restore_env(key, Some("tx-test-dummy".into()));
        restore_env(key, None);
        restore_env(key, value);
    }

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

        restore_env_branches("TX_DATA_DIR", original_data);
        restore_env_branches("TX_CACHE_DIR", original_cache);

        result
    }

    #[test]
    fn main_clears_env_when_missing() -> color_eyre::Result<()> {
        let _guard = ENV_LOCK.lock().unwrap();
        let temp = TempDir::new()?;
        let directories = AppDirectories {
            config_dir: temp.path().join("config"),
            data_dir: temp.path().join("data"),
            cache_dir: temp.path().join("cache"),
        };
        directories.ensure_all()?;

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

        let actual_data = env::var("TX_DATA_DIR").ok();
        let actual_cache = env::var("TX_CACHE_DIR").ok();
        unsafe {
            env::remove_var("TX_DATA_DIR");
            env::remove_var("TX_CACHE_DIR");
        }

        let original_data: Option<String> = None;
        let original_cache: Option<String> = None;
        unsafe {
            env::set_var("TX_DATA_DIR", &directories.data_dir);
            env::set_var("TX_CACHE_DIR", &directories.cache_dir);
        }

        let result = tool_executor::run(&cli);

        restore_env_branches("TX_DATA_DIR", original_data);
        restore_env_branches("TX_CACHE_DIR", original_cache);

        assert!(env::var_os("TX_DATA_DIR").is_none());
        assert!(env::var_os("TX_CACHE_DIR").is_none());

        restore_env_branches("TX_DATA_DIR", actual_data);
        restore_env_branches("TX_CACHE_DIR", actual_cache);

        result
    }
}

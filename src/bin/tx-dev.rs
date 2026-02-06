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

#[cfg(test)]
mod tests {
    use assert_fs::TempDir;
    use tool_executor::{
        Cli,
        cli::{Command as TopLevelCommand, ConfigCommand},
        config::AppDirectories,
        test_support::{ENV_LOCK, EnvOverride},
    };

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
        let _data_override = EnvOverride::set_path("TX_DATA_DIR", &directories.data_dir);
        let _cache_override = EnvOverride::set_path("TX_CACHE_DIR", &directories.cache_dir);

        tool_executor::run(&cli)
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

        let _actual_data = EnvOverride::remove("TX_DATA_DIR");
        let _actual_cache = EnvOverride::remove("TX_CACHE_DIR");
        {
            let _data_override = EnvOverride::set_path("TX_DATA_DIR", &directories.data_dir);
            let _cache_override = EnvOverride::set_path("TX_CACHE_DIR", &directories.cache_dir);
            tool_executor::run(&cli)?;
        }

        assert!(std::env::var_os("TX_DATA_DIR").is_none());
        assert!(std::env::var_os("TX_CACHE_DIR").is_none());
        Ok(())
    }
}

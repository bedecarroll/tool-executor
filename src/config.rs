use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

use color_eyre::{Result, Section, SectionExt, eyre::Context};
use directories::BaseDirs;
use serde::Deserialize;

const CONFIG_FILE: &str = "config.toml";
const CONF_D_DIR: &str = "conf.d";
const APP_CONFIG_DIR: &str = "tx";
const ENV_CONFIG_DIR: &str = "TX_CONFIG_DIR";

#[derive(Debug, Clone, Default)]
pub struct Config {
    pub greet: GreetConfig,
}

#[derive(Debug, Clone, Default)]
pub struct GreetConfig {
    pub default_name: Option<String>,
}

impl Config {
    /// Load configuration from the standard directories.
    ///
    /// # Errors
    ///
    /// Returns an error if a configuration file cannot be read or parsed as TOML.
    pub fn load(dir_override: Option<&Path>) -> Result<Self> {
        let mut config = Config::default();
        let Some(root) = locate_config_dir(dir_override) else {
            return Ok(config);
        };

        if !root.exists() {
            return Ok(config);
        }

        if root.is_file() {
            tracing::warn!(
                path = %root.display(),
                "Config path points to a file; skipping configuration load"
            );
            return Ok(config);
        }

        let main = root.join(CONFIG_FILE);
        if main.is_file() {
            config.apply_file(&main)?;
        }

        let conf_d = root.join(CONF_D_DIR);
        if conf_d.is_dir() {
            let mut entries = fs::read_dir(&conf_d)
                .with_context(|| format!("failed to read {}", conf_d.display()))?
                .filter_map(std::result::Result::ok)
                .filter(|entry| entry.file_type().map(|ty| ty.is_file()).unwrap_or(false))
                .map(|entry| entry.path())
                .filter(|path| is_toml_file(path))
                .collect::<Vec<PathBuf>>();

            entries.sort();

            for entry in entries {
                config.apply_file(&entry)?;
            }
        }

        Ok(config)
    }

    fn apply_file(&mut self, path: &Path) -> Result<()> {
        let contents = fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display()))
            .map_err(|err| {
                err.with_section(|| {
                    format!(
                        "Ensure the file exists and is readable.\nResolved path: {}",
                        path.display()
                    )
                    .header("Suggested fix")
                })
            })?;
        let partial: PartialConfig = toml::from_str(&contents)
            .with_context(|| format!("failed to parse {}", path.display()))
            .map_err(|err| {
                err.with_section(|| {
                    format!(
                        "Double-check the TOML syntax or remove the file if it is no longer needed.\nResolved path: {}",
                        path.display()
                    )
                    .header("Suggested fix")
                })
            })?;
        self.merge(partial);

        Ok(())
    }

    fn merge(&mut self, partial: PartialConfig) {
        if let Some(name) = partial.greet.default_name {
            self.greet.default_name = Some(name);
        }
    }
}

fn locate_config_dir(dir_override: Option<&Path>) -> Option<PathBuf> {
    if let Some(path) = dir_override {
        return Some(path.to_path_buf());
    }

    if let Some(raw) = std::env::var(ENV_CONFIG_DIR)
        .ok()
        .filter(|raw| !raw.trim().is_empty())
    {
        return Some(PathBuf::from(raw));
    }

    if let Some(xdg_home) = std::env::var("XDG_CONFIG_HOME")
        .ok()
        .filter(|home| !home.trim().is_empty())
    {
        return Some(Path::new(&xdg_home).join(APP_CONFIG_DIR));
    }

    let base_dirs = BaseDirs::new()?;
    Some(base_dirs.config_dir().join(APP_CONFIG_DIR))
}

fn is_toml_file(path: &Path) -> bool {
    path.extension()
        .and_then(OsStr::to_str)
        .is_some_and(|ext| ext.eq_ignore_ascii_case("toml"))
}

#[derive(Debug, Default, Deserialize)]
struct PartialConfig {
    #[serde(default)]
    greet: PartialGreetConfig,
}

#[derive(Debug, Default, Deserialize)]
struct PartialGreetConfig {
    default_name: Option<String>,
}

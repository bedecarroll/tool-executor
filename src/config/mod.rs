use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use color_eyre::Result;
use color_eyre::eyre::{Context, eyre};
use directories::{BaseDirs, ProjectDirs};
use itertools::Itertools;
use toml::Value;

const DEFAULT_CONFIG_TEMPLATE: &str = include_str!("../../assets/default_config.toml");

mod merge;
pub mod model;

pub use model::{Config, ConfigDiagnostic};

const MAIN_CONFIG: &str = "config.toml";
const DROPIN_DIR: &str = "conf.d";
const PROJECT_FILE: &str = ".tx.toml";
const PROJECT_DROPIN_DIR: &str = ".tx.d";
const APP_NAME: &str = "tx";

#[derive(Debug, Clone)]
pub struct LoadedConfig {
    pub config: Config,
    pub merged: Value,
    pub directories: AppDirectories,
    pub sources: Vec<ConfigSource>,
    pub diagnostics: Vec<ConfigDiagnostic>,
}

#[derive(Debug, Clone)]
pub struct AppDirectories {
    pub config_dir: PathBuf,
    pub data_dir: PathBuf,
    pub cache_dir: PathBuf,
}

impl AppDirectories {
    /// Create the configuration, data, and cache directories if they are missing.
    ///
    /// # Errors
    ///
    /// Returns an error when any directory cannot be created or is otherwise
    /// inaccessible.
    pub fn ensure_all(&self) -> Result<()> {
        for dir in [&self.config_dir, &self.data_dir, &self.cache_dir] {
            if !dir.exists() {
                fs::create_dir_all(dir)
                    .with_context(|| format!("failed to create directory {}", dir.display()))?;
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigSourceKind {
    Main,
    DropIn,
    Project,
    ProjectDropIn,
}

#[derive(Debug, Clone)]
pub struct ConfigSource {
    pub kind: ConfigSourceKind,
    pub path: PathBuf,
}

/// Load and merge configuration files into a [`LoadedConfig`].
///
/// # Errors
///
/// Returns an error if any configuration file cannot be read, parsed, or merged
/// according to the schema.
pub fn load(dir_override: Option<&Path>) -> Result<LoadedConfig> {
    let dirs = resolve_directories(dir_override)?;
    dirs.ensure_all()?;
    ensure_default_layout(&dirs)?;

    let mut sources = gather_sources(&dirs.config_dir)?;
    sources.extend(gather_project_sources()?);

    let mut merged_table = toml::map::Map::new();

    for source in &sources {
        let contents = fs::read_to_string(&source.path)
            .with_context(|| format!("failed to read {}", source.path.display()))?;
        let value: Value = toml::from_str(&contents)
            .with_context(|| format!("failed to parse {}", source.path.display()))?;
        let table = value.as_table().cloned().ok_or_else(|| {
            eyre!(
                "{} must contain a TOML table at the top level",
                source.path.display()
            )
        })?;
        merge::merge_tables(&mut merged_table, table, Some(&source.path))?;
    }

    let merged_value = Value::Table(merged_table);
    let config = Config::from_value(&merged_value)?;
    let diagnostics = config.lint();

    Ok(LoadedConfig {
        config,
        merged: merged_value,
        directories: dirs,
        sources,
        diagnostics,
    })
}

fn resolve_directories(dir_override: Option<&Path>) -> Result<AppDirectories> {
    let (default_config_dir, default_data_dir, default_cache_dir) = resolve_default_directories()?;

    let config_dir = dir_override
        .map(PathBuf::from)
        .or_else(|| env::var("TX_CONFIG_DIR").ok().map(PathBuf::from))
        .unwrap_or(default_config_dir);

    let data_dir = env::var("TX_DATA_DIR").map_or_else(|_| default_data_dir.clone(), PathBuf::from);
    let cache_dir =
        env::var("TX_CACHE_DIR").map_or_else(|_| default_cache_dir.clone(), PathBuf::from);

    Ok(AppDirectories {
        config_dir,
        data_dir,
        cache_dir,
    })
}

fn resolve_default_directories() -> Result<(PathBuf, PathBuf, PathBuf)> {
    if let Some(project_dirs) = ProjectDirs::from("", "", APP_NAME) {
        return Ok((
            project_dirs.config_dir().to_path_buf(),
            project_dirs.data_dir().to_path_buf(),
            project_dirs.cache_dir().to_path_buf(),
        ));
    }

    if let Some(base_dirs) = BaseDirs::new() {
        return Ok((
            base_dirs.config_dir().join(APP_NAME),
            base_dirs.data_dir().join(APP_NAME),
            base_dirs.cache_dir().join(APP_NAME),
        ));
    }

    #[cfg(windows)]
    {
        for key in ["LOCALAPPDATA", "APPDATA"] {
            if let Ok(path) = env::var(key) {
                let base = PathBuf::from(path);
                let config = base.join(APP_NAME);
                let data = config.clone();
                let cache = config.clone();
                return Ok((config, data, cache));
            }
        }
    }

    Err(eyre!(
        "unable to resolve platform directories for {APP_NAME}"
    ))
}

fn ensure_default_layout(dirs: &AppDirectories) -> Result<()> {
    if dirs.config_dir.is_file() {
        return Ok(());
    }

    let conf_d = dirs.config_dir.join(DROPIN_DIR);
    if !conf_d.exists() {
        fs::create_dir_all(&conf_d)
            .with_context(|| format!("failed to create directory {}", conf_d.display()))?;
    }

    let main = dirs.config_dir.join(MAIN_CONFIG);
    if main.exists() {
        return Ok(());
    }

    let contents = default_config_contents(dirs);

    fs::write(&main, contents).with_context(|| {
        format!(
            "failed to write default configuration to {}",
            main.display()
        )
    })?;

    Ok(())
}

fn default_config_contents(_dirs: &AppDirectories) -> String {
    DEFAULT_CONFIG_TEMPLATE.to_string()
}

/// Return the bundled default configuration template.
#[must_use]
pub fn default_template() -> &'static str {
    DEFAULT_CONFIG_TEMPLATE
}

/// Render the bundled default configuration using resolved application directories.
#[must_use]
pub fn bundled_default_config(dirs: &AppDirectories) -> String {
    default_config_contents(dirs)
}

fn gather_sources(root: &Path) -> Result<Vec<ConfigSource>> {
    let mut sources = Vec::new();

    if root.is_file() {
        sources.push(ConfigSource {
            kind: ConfigSourceKind::Main,
            path: root.to_path_buf(),
        });
        return Ok(sources);
    }

    if !root.exists() {
        return Ok(vec![]);
    }

    let main = root.join(MAIN_CONFIG);
    if main.is_file() {
        sources.push(ConfigSource {
            kind: ConfigSourceKind::Main,
            path: main,
        });
    }

    let conf_d = root.join(DROPIN_DIR);
    if conf_d.is_dir() {
        let mut entries = read_toml_files(&conf_d)?;
        entries.sort();
        sources.extend(entries.into_iter().map(|path| ConfigSource {
            kind: ConfigSourceKind::DropIn,
            path,
        }));
    }

    Ok(sources)
}

fn gather_project_sources() -> Result<Vec<ConfigSource>> {
    let cwd = std::env::current_dir().context("failed to resolve current directory")?;
    let mut sources = Vec::new();
    let project_file = cwd.join(PROJECT_FILE);
    if project_file.is_file() {
        sources.push(ConfigSource {
            kind: ConfigSourceKind::Project,
            path: project_file,
        });
    }
    let project_dir = cwd.join(PROJECT_DROPIN_DIR);
    if project_dir.is_dir() {
        let mut entries = read_toml_files(&project_dir)?;
        entries.sort();
        sources.extend(entries.into_iter().map(|path| ConfigSource {
            kind: ConfigSourceKind::ProjectDropIn,
            path,
        }));
    }
    Ok(sources)
}

fn read_toml_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut files = BTreeSet::new();
    for entry in
        fs::read_dir(dir).with_context(|| format!("failed to read directory {}", dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        if path
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| ext.eq_ignore_ascii_case("toml"))
        {
            files.insert(path);
        }
    }
    Ok(files.into_iter().collect_vec())
}

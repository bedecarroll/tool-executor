use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use color_eyre::Result;
use color_eyre::eyre::{Context, eyre};
use directories::BaseDirs;
#[cfg(windows)]
use directories::ProjectDirs;
use itertools::Itertools;
use schemars::schema_for;
use toml::Value;
use tracing::warn;

const DEFAULT_CONFIG_TEMPLATE: &str = include_str!("../../assets/default_config.toml");

mod merge;
pub mod model;

use self::model::DiagnosticLevel;

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

    let mut saw_preview_filter = false;
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
        if value
            .as_table()
            .and_then(|table| table.get("preview_filter"))
            .is_some()
        {
            saw_preview_filter = true;
        }
        merge::merge_tables(&mut merged_table, table, Some(&source.path))?;
    }

    if saw_preview_filter {
        merged_table.remove("preview_filter");
    }

    let merged_value = Value::Table(merged_table);
    if saw_preview_filter {
        warn!(
            "Ignoring configuration key `preview_filter`; the preview filter feature has been removed."
        );
    }
    let config = Config::from_value(&merged_value)?;
    let mut diagnostics = config.lint();
    if saw_preview_filter {
        diagnostics.push(ConfigDiagnostic {
            level: DiagnosticLevel::Warning,
            message:
                "Ignoring configuration key `preview_filter`; remove this key from your config."
                    .into(),
        });
    }

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

    let config_override = dir_override
        .map(PathBuf::from)
        .or_else(|| env::var("TX_CONFIG_DIR").ok().map(PathBuf::from));
    let data_override = env::var("TX_DATA_DIR").ok().map(PathBuf::from);
    let cache_override = env::var("TX_CACHE_DIR").ok().map(PathBuf::from);

    let config_dir = config_override
        .clone()
        .unwrap_or_else(|| default_config_dir.clone());
    let data_dir = data_override
        .clone()
        .unwrap_or_else(|| default_data_dir.clone());
    let cache_dir = cache_override
        .clone()
        .unwrap_or_else(|| default_cache_dir.clone());

    Ok(AppDirectories {
        config_dir,
        data_dir,
        cache_dir,
    })
}

fn resolve_default_directories() -> Result<(PathBuf, PathBuf, PathBuf)> {
    #[cfg(windows)]
    {
        resolve_default_directories_with(
            || {
                ProjectDirs::from("", "", APP_NAME).map(|dirs| {
                    (
                        dirs.config_dir().to_path_buf(),
                        dirs.data_dir().to_path_buf(),
                        dirs.cache_dir().to_path_buf(),
                    )
                })
            },
            || {
                BaseDirs::new().map(|dirs| {
                    (
                        dirs.config_dir().join(APP_NAME),
                        dirs.data_dir().join(APP_NAME),
                        dirs.cache_dir().join(APP_NAME),
                    )
                })
            },
        )
    }

    #[cfg(not(windows))]
    {
        fn xdg_dir(var: &str, home: &Path, fallback: &str) -> PathBuf {
            env::var_os(var).map_or_else(|| home.join(fallback), PathBuf::from)
        }

        let base_dirs = BaseDirs::new()
            .ok_or_else(|| eyre!("unable to resolve home directory for {APP_NAME}"))?;
        let home = base_dirs.home_dir();

        let config_root = xdg_dir("XDG_CONFIG_HOME", home, ".config");
        let data_root = xdg_dir("XDG_DATA_HOME", home, ".local/share");
        let cache_root = xdg_dir("XDG_CACHE_HOME", home, ".cache");

        Ok((
            config_root.join(APP_NAME),
            data_root.join(APP_NAME),
            cache_root.join(APP_NAME),
        ))
    }
}

#[cfg(any(test, windows))]
fn resolve_default_directories_with<P, B>(
    project_dirs: P,
    base_dirs: B,
) -> Result<(PathBuf, PathBuf, PathBuf)>
where
    P: FnOnce() -> Option<(PathBuf, PathBuf, PathBuf)>,
    B: FnOnce() -> Option<(PathBuf, PathBuf, PathBuf)>,
{
    if let Some(paths) = project_dirs() {
        return Ok(paths);
    }

    if let Some(paths) = base_dirs() {
        return Ok(paths);
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

/// Generate the JSON Schema for the configuration file format.
///
/// # Errors
///
/// Returns an error if the schema cannot be serialized to JSON.
pub fn schema(pretty: bool) -> Result<String> {
    let root = schema_for!(model::RawConfig);
    let rendered = if pretty {
        serde_json::to_string_pretty(&root)
    } else {
        serde_json::to_string(&root)
    }
    .wrap_err("failed to serialize configuration schema")?;
    Ok(rendered)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::ENV_LOCK;
    use assert_fs::TempDir;
    use assert_fs::fixture::{FileTouch, FileWriteStr, PathChild, PathCreateDir};
    use color_eyre::Result;
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;
    fn path_contains_component(path: &std::path::Path, needle: &str) -> bool {
        path.components().any(|component| {
            component
                .as_os_str()
                .to_string_lossy()
                .eq_ignore_ascii_case(needle)
        })
    }

    fn restore_env(key: &str, value: Option<String>) {
        if let Some(val) = value {
            unsafe { std::env::set_var(key, val) };
        } else {
            unsafe { std::env::remove_var(key) };
        }
    }

    fn restore_env_branches(key: &str, value: Option<String>) {
        restore_env(key, Some("tx-test-dummy".into()));
        restore_env(key, None);
        restore_env(key, value);
    }

    #[test]
    fn resolve_directories_uses_env_overrides() -> Result<()> {
        let _guard = ENV_LOCK.lock().unwrap();
        let temp = TempDir::new()?;
        let config_dir = temp.child("config");
        let data_dir = temp.child("data");
        let cache_dir = temp.child("cache");
        config_dir.create_dir_all()?;
        data_dir.create_dir_all()?;
        cache_dir.create_dir_all()?;

        let orig_config = std::env::var("TX_CONFIG_DIR").ok();
        let orig_data = std::env::var("TX_DATA_DIR").ok();
        let orig_cache = std::env::var("TX_CACHE_DIR").ok();

        unsafe {
            std::env::set_var("TX_CONFIG_DIR", config_dir.path());
            std::env::set_var("TX_DATA_DIR", data_dir.path());
            std::env::set_var("TX_CACHE_DIR", cache_dir.path());
        }

        let dirs = resolve_directories(None)?;
        assert_eq!(dirs.config_dir, config_dir.path());
        assert_eq!(dirs.data_dir, data_dir.path());
        assert_eq!(dirs.cache_dir, cache_dir.path());

        restore_env_branches("TX_CONFIG_DIR", orig_config);
        restore_env_branches("TX_DATA_DIR", orig_data);
        restore_env_branches("TX_CACHE_DIR", orig_cache);

        Ok(())
    }

    #[test]
    fn resolve_directories_defaults_without_env() -> Result<()> {
        let _guard = ENV_LOCK.lock().unwrap();
        let orig_config = std::env::var("TX_CONFIG_DIR").ok();
        let orig_data = std::env::var("TX_DATA_DIR").ok();
        let orig_cache = std::env::var("TX_CACHE_DIR").ok();

        unsafe {
            std::env::remove_var("TX_CONFIG_DIR");
            std::env::remove_var("TX_DATA_DIR");
            std::env::remove_var("TX_CACHE_DIR");
        }

        let dirs = resolve_directories(None)?;
        assert!(path_contains_component(&dirs.config_dir, APP_NAME));
        assert!(path_contains_component(&dirs.data_dir, APP_NAME));
        assert!(path_contains_component(&dirs.cache_dir, APP_NAME));

        restore_env_branches("TX_CONFIG_DIR", orig_config);
        restore_env_branches("TX_DATA_DIR", orig_data);
        restore_env_branches("TX_CACHE_DIR", orig_cache);

        Ok(())
    }

    #[test]
    fn resolve_directories_prefers_override_over_env() -> Result<()> {
        let _guard = ENV_LOCK.lock().unwrap();
        let temp = TempDir::new()?;
        let override_dir = temp.child("override");
        override_dir.create_dir_all()?;
        let data_dir = temp.child("data");
        data_dir.create_dir_all()?;
        let cache_dir = temp.child("cache");
        cache_dir.create_dir_all()?;

        let orig_config = std::env::var("TX_CONFIG_DIR").ok();
        let orig_data = std::env::var("TX_DATA_DIR").ok();
        let orig_cache = std::env::var("TX_CACHE_DIR").ok();

        unsafe {
            std::env::set_var("TX_CONFIG_DIR", temp.child("other").path());
            std::env::set_var("TX_DATA_DIR", data_dir.path());
            std::env::set_var("TX_CACHE_DIR", cache_dir.path());
        }

        let dirs = resolve_directories(Some(override_dir.path()))?;
        assert_eq!(dirs.config_dir, override_dir.path());
        assert_eq!(dirs.data_dir, data_dir.path());
        assert_eq!(dirs.cache_dir, cache_dir.path());

        restore_env_branches("TX_CONFIG_DIR", orig_config);
        restore_env_branches("TX_DATA_DIR", orig_data);
        restore_env_branches("TX_CACHE_DIR", orig_cache);

        Ok(())
    }

    #[test]
    fn ensure_default_layout_creates_default_config() -> Result<()> {
        let temp = TempDir::new()?;
        let dirs = AppDirectories {
            config_dir: temp.child("config").path().to_path_buf(),
            data_dir: temp.child("data").path().to_path_buf(),
            cache_dir: temp.child("cache").path().to_path_buf(),
        };

        dirs.ensure_all()?;
        ensure_default_layout(&dirs)?;

        let main = dirs.config_dir.join(MAIN_CONFIG);
        assert!(main.exists(), "expected main config file to exist");
        let contents = fs::read_to_string(&main)?;
        assert!(contents.contains("provider = \"codex\""));
        assert!(
            !dirs.config_dir.join(DROPIN_DIR).exists(),
            "drop-in directory should not be created until needed"
        );
        Ok(())
    }

    #[test]
    fn ensure_all_creates_missing_directories() -> Result<()> {
        let temp = TempDir::new()?;
        let dirs = AppDirectories {
            config_dir: temp.child("config").path().to_path_buf(),
            data_dir: temp.child("data").path().to_path_buf(),
            cache_dir: temp.child("cache").path().to_path_buf(),
        };

        dirs.ensure_all()?;
        assert!(dirs.config_dir.is_dir());
        assert!(dirs.data_dir.is_dir());
        assert!(dirs.cache_dir.is_dir());
        Ok(())
    }

    #[test]
    fn resolve_default_directories_returns_paths() -> Result<()> {
        let (config, data, cache) = resolve_default_directories()?;
        assert!(
            path_contains_component(&config, APP_NAME),
            "config dir {} should contain '{}'",
            config.display(),
            APP_NAME
        );
        assert!(
            path_contains_component(&data, APP_NAME),
            "data dir {} should contain '{}'",
            data.display(),
            APP_NAME
        );
        assert!(
            path_contains_component(&cache, APP_NAME),
            "cache dir {} should contain '{}'",
            cache.display(),
            APP_NAME
        );
        Ok(())
    }

    #[test]
    fn resolve_default_directories_prefers_project_dirs() -> Result<()> {
        let fake = (
            PathBuf::from("/tmp/config"),
            PathBuf::from("/tmp/data"),
            PathBuf::from("/tmp/cache"),
        );
        let resolved = resolve_default_directories_with(|| Some(fake.clone()), || unreachable!())?;
        assert_eq!(resolved, fake);
        Ok(())
    }

    #[test]
    fn resolve_default_directories_falls_back_to_base_dirs() -> Result<()> {
        let fake = (
            PathBuf::from("/tmp/base-config"),
            PathBuf::from("/tmp/base-data"),
            PathBuf::from("/tmp/base-cache"),
        );
        let resolved = resolve_default_directories_with(|| None, || Some(fake.clone()))?;
        assert_eq!(resolved, fake);
        Ok(())
    }

    #[cfg(not(windows))]
    #[test]
    fn resolve_default_directories_uses_xdg_env() -> Result<()> {
        let _guard = ENV_LOCK.lock().unwrap();
        let temp = TempDir::new()?;
        let config_root = temp.child("xdg-config");
        let data_root = temp.child("xdg-data");
        let cache_root = temp.child("xdg-cache");
        config_root.create_dir_all()?;
        data_root.create_dir_all()?;
        cache_root.create_dir_all()?;

        let orig_config = std::env::var("XDG_CONFIG_HOME").ok();
        let orig_data = std::env::var("XDG_DATA_HOME").ok();
        let orig_cache = std::env::var("XDG_CACHE_HOME").ok();

        unsafe {
            std::env::set_var("XDG_CONFIG_HOME", config_root.path());
            std::env::set_var("XDG_DATA_HOME", data_root.path());
            std::env::set_var("XDG_CACHE_HOME", cache_root.path());
        }

        let (config, data, cache) = resolve_default_directories()?;
        assert_eq!(config, config_root.path().join(APP_NAME));
        assert_eq!(data, data_root.path().join(APP_NAME));
        assert_eq!(cache, cache_root.path().join(APP_NAME));

        restore_env_branches("XDG_CONFIG_HOME", orig_config);
        restore_env_branches("XDG_DATA_HOME", orig_data);
        restore_env_branches("XDG_CACHE_HOME", orig_cache);

        Ok(())
    }

    #[cfg(not(windows))]
    #[test]
    fn resolve_default_directories_errors_without_sources() {
        let err = resolve_default_directories_with(|| None, || None).expect_err("expected error");
        assert!(
            err.to_string()
                .contains("unable to resolve platform directories")
        );
    }

    #[cfg(windows)]
    #[test]
    fn resolve_default_directories_errors_without_sources() {
        let _guard = ENV_LOCK.lock().unwrap();
        let original_local = std::env::var("LOCALAPPDATA").ok();
        let original_app = std::env::var("APPDATA").ok();

        unsafe {
            std::env::remove_var("LOCALAPPDATA");
            std::env::remove_var("APPDATA");
        }

        let err = resolve_default_directories_with(|| None, || None).expect_err("expected error");
        assert!(
            err.to_string()
                .contains("unable to resolve platform directories")
        );

        if let Some(value) = original_local {
            unsafe { std::env::set_var("LOCALAPPDATA", value) };
        } else {
            unsafe { std::env::remove_var("LOCALAPPDATA") };
        }
        if let Some(value) = original_app {
            unsafe { std::env::set_var("APPDATA", value) };
        } else {
            unsafe { std::env::remove_var("APPDATA") };
        }
    }

    #[test]
    fn gather_sources_returns_empty_for_missing_root() -> Result<()> {
        let temp = TempDir::new()?;
        let missing = temp.child("missing");
        let sources = gather_sources(missing.path())?;
        assert!(sources.is_empty());
        Ok(())
    }

    #[test]
    fn read_toml_files_filters_non_toml_entries() -> Result<()> {
        let temp = TempDir::new()?;
        let dir = temp.child("conf.d");
        dir.create_dir_all()?;
        dir.child("00-main.toml")
            .write_str("provider = \"codex\"")?;
        dir.child("notes.md").write_str("# ignore")?;
        let files = read_toml_files(dir.path())?;
        assert_eq!(files.len(), 1);
        assert!(files[0].ends_with("00-main.toml"));
        Ok(())
    }

    #[test]
    fn read_toml_files_skips_subdirectories() -> Result<()> {
        let temp = TempDir::new()?;
        let dir = temp.child("conf.d");
        dir.create_dir_all()?;
        dir.child("00-main.toml")
            .write_str("provider = \"codex\"")?;
        let nested = dir.child("nested");
        nested.create_dir_all()?;
        nested
            .child("01-nested.toml")
            .write_str("provider = \"codex\"")?;

        let files = read_toml_files(dir.path())?;
        assert_eq!(files.len(), 1);
        assert!(files[0].ends_with("00-main.toml"));
        Ok(())
    }

    #[test]
    fn load_merges_dropins_in_lexical_order() -> Result<()> {
        let _guard = ENV_LOCK.lock().unwrap();
        let temp = TempDir::new()?;
        let config_dir = temp.child("config");
        config_dir.create_dir_all()?;
        config_dir
            .child("config.toml")
            .write_str("provider = \"echo\"\n[providers.echo]\nbin = \"echo\"\n")?;
        let dropin = config_dir.child("conf.d");
        dropin.create_dir_all()?;
        dropin
            .child("10-wrapper.toml")
            .write_str("[wrappers.wrap]\nshell = true\ncmd = \"echo {pipeline}\"\n")?;
        dropin
            .child("20-profile.toml")
            .write_str("[profiles.test]\nprovider = \"echo\"\n")?;

        let loaded = load(Some(config_dir.path()))?;
        assert!(loaded.config.wrappers.contains_key("wrap"));
        assert!(loaded.config.profiles.contains_key("test"));
        Ok(())
    }

    #[test]
    fn load_collects_sources_and_parses_configs() -> Result<()> {
        let _guard = ENV_LOCK.lock().unwrap();
        let temp = TempDir::new()?;
        let config_dir = temp.child("config");
        config_dir.create_dir_all()?;
        let conf_d = config_dir.child("conf.d");
        conf_d.create_dir_all()?;
        fs::write(
            config_dir.child("config.toml").path(),
            "provider = \"echo\"\n[providers.echo]\nbin = \"echo\"\n",
        )?;
        fs::write(
            conf_d.child("00-extra.toml").path(),
            "[providers.extra]\nbin = \"echo\"\n",
        )?;

        let orig_data = std::env::var("TX_DATA_DIR").ok();
        let orig_cache = std::env::var("TX_CACHE_DIR").ok();
        let data_override = temp.child("data");
        data_override.create_dir_all()?;
        let cache_override = temp.child("cache");
        cache_override.create_dir_all()?;
        unsafe {
            std::env::set_var("TX_DATA_DIR", data_override.path());
            std::env::set_var("TX_CACHE_DIR", cache_override.path());
        }

        let loaded = load(Some(config_dir.path()))?;
        assert!(loaded.config.providers.contains_key("echo"));
        assert!(loaded.config.providers.contains_key("extra"));
        assert!(loaded.sources.len() >= 2);

        restore_env_branches("TX_DATA_DIR", orig_data);
        restore_env_branches("TX_CACHE_DIR", orig_cache);
        Ok(())
    }

    #[test]
    fn load_ignores_legacy_preview_filter_key() -> Result<()> {
        let _guard = ENV_LOCK.lock().unwrap();
        let temp = TempDir::new()?;
        let config_dir = temp.child("config");
        config_dir.create_dir_all()?;
        config_dir.child("config.toml").write_str(
            "\
preview_filter = \"glow\"
provider = \"echo\"
[providers.echo]
bin = \"echo\"
",
        )?;

        let loaded = load(Some(config_dir.path()))?;
        let merged_table = loaded
            .merged
            .as_table()
            .expect("merged config should be a table");
        assert!(!merged_table.contains_key("preview_filter"));
        assert!(loaded.config.providers.contains_key("echo"));
        assert!(
            loaded
                .diagnostics
                .iter()
                .any(|diag| diag.message.contains("preview_filter"))
        );
        Ok(())
    }

    #[test]
    fn load_surfaces_merge_errors() -> Result<()> {
        let _guard = ENV_LOCK.lock().unwrap();
        let temp = TempDir::new()?;
        let config_dir = temp.child("config");
        config_dir.create_dir_all()?;
        config_dir.child(MAIN_CONFIG).write_str(
            "\
provider = \"echo\"
[providers.echo]
bin = \"echo\"
list = \"value\"
",
        )?;
        let dropins = config_dir.child(DROPIN_DIR);
        dropins.create_dir_all()?;
        dropins
            .child("10-list.toml")
            .write_str("[providers.echo]\n\"list+\" = [\"extra\"]\n")?;

        let data_dir = temp.child("data");
        data_dir.create_dir_all()?;
        let cache_dir = temp.child("cache");
        cache_dir.create_dir_all()?;

        let orig_data = std::env::var("TX_DATA_DIR").ok();
        let orig_cache = std::env::var("TX_CACHE_DIR").ok();
        unsafe {
            std::env::set_var("TX_DATA_DIR", data_dir.path());
            std::env::set_var("TX_CACHE_DIR", cache_dir.path());
        }

        let err =
            load(Some(config_dir.path())).expect_err("merge should fail for non-array append");
        let message = format!("{err:?}");
        assert!(
            message.contains("cannot append to non-array key 'list'"),
            "unexpected error: {message}"
        );

        if let Some(val) = orig_data {
            unsafe {
                std::env::set_var("TX_DATA_DIR", val);
            }
        } else {
            unsafe {
                std::env::remove_var("TX_DATA_DIR");
            }
        }
        if let Some(val) = orig_cache {
            unsafe {
                std::env::set_var("TX_CACHE_DIR", val);
            }
        } else {
            unsafe {
                std::env::remove_var("TX_CACHE_DIR");
            }
        }

        Ok(())
    }

    #[test]
    fn ensure_default_layout_skips_when_config_path_is_file() -> Result<()> {
        let temp = TempDir::new()?;
        let config_file = temp.child("config.toml");
        config_file.touch()?;

        let dirs = AppDirectories {
            config_dir: config_file.path().to_path_buf(),
            data_dir: temp.child("data").path().to_path_buf(),
            cache_dir: temp.child("cache").path().to_path_buf(),
        };

        ensure_default_layout(&dirs)?;
        assert!(
            !dirs.config_dir.parent().unwrap().join(DROPIN_DIR).exists(),
            "drop-in directory should not be created when config path is a file"
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn ensure_default_layout_errors_when_config_write_fails() -> Result<()> {
        let temp = TempDir::new()?;
        let config_dir = temp.child("config");
        config_dir.create_dir_all()?;
        let perms = fs::Permissions::from_mode(0o555);
        fs::set_permissions(config_dir.path(), perms)?;

        let dirs = AppDirectories {
            config_dir: config_dir.path().to_path_buf(),
            data_dir: temp.child("data").path().to_path_buf(),
            cache_dir: temp.child("cache").path().to_path_buf(),
        };

        let err = ensure_default_layout(&dirs).unwrap_err();
        assert!(
            err.to_string()
                .contains("failed to write default configuration"),
            "unexpected error: {err:?}"
        );
        Ok(())
    }

    #[test]
    fn default_template_contains_provider() {
        let template = default_template();
        assert!(
            template.contains("provider = \"codex\""),
            "expected bundled template to include provider stanza"
        );
    }

    #[test]
    fn bundled_default_config_renders_template() -> Result<()> {
        let temp = TempDir::new()?;
        let dirs = AppDirectories {
            config_dir: temp.child("config").path().to_path_buf(),
            data_dir: temp.child("data").path().to_path_buf(),
            cache_dir: temp.child("cache").path().to_path_buf(),
        };

        let rendered = bundled_default_config(&dirs);
        assert!(
            rendered.contains("provider = \"codex\""),
            "expected rendered template to include provider stanza"
        );
        Ok(())
    }

    #[test]
    fn load_rejects_non_table_config_files() -> Result<()> {
        let _guard = ENV_LOCK.lock().unwrap();
        let temp = TempDir::new()?;
        let config_dir = temp.child("config");
        config_dir.create_dir_all()?;
        std::fs::write(config_dir.child("config.toml").path(), "123")?;

        let err = load(Some(config_dir.path())).unwrap_err();
        assert!(
            err.to_string().contains("failed to parse"),
            "unexpected error: {err}"
        );
        Ok(())
    }

    #[test]
    fn gather_sources_supports_single_file_roots() -> Result<()> {
        let temp = TempDir::new()?;
        let config_file = temp.child("inline.toml");
        config_file.write_str("provider = \"codex\"")?;

        let sources = gather_sources(config_file.path())?;
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].kind, ConfigSourceKind::Main);
        assert_eq!(sources[0].path, config_file.path());
        Ok(())
    }

    #[test]
    fn gather_project_sources_collects_project_files() -> Result<()> {
        let _guard = ENV_LOCK.lock().unwrap();
        let temp = TempDir::new()?;
        let project_file = temp.child(PROJECT_FILE);
        project_file.write_str("provider = \"codex\"")?;
        let dropin_dir = temp.child(PROJECT_DROPIN_DIR);
        dropin_dir.create_dir_all()?;
        dropin_dir
            .child("10-extra.toml")
            .write_str("search_mode = \"full_text\"")?;

        let current = std::env::current_dir()?;
        std::env::set_current_dir(temp.path())?;
        let sources = gather_project_sources()?;
        std::env::set_current_dir(current)?;

        assert_eq!(sources.len(), 2);
        assert!(
            sources
                .iter()
                .any(|src| matches!(src.kind, ConfigSourceKind::Project))
        );
        assert!(
            sources
                .iter()
                .any(|src| matches!(src.kind, ConfigSourceKind::ProjectDropIn))
        );
        Ok(())
    }

    #[test]
    fn read_toml_files_skips_directories() -> Result<()> {
        let temp = TempDir::new()?;
        let dir = temp.child("conf.d");
        dir.create_dir_all()?;
        dir.child("10-valid.toml").write_str("value = 1")?;
        dir.child("subdir").create_dir_all()?;
        dir.child("subdir")
            .child("20-extra.toml")
            .write_str("ignored = true")?;
        dir.child("notes.txt").write_str("nope")?;

        let files = read_toml_files(dir.path())?;
        assert_eq!(files.len(), 1);
        assert!(files[0].ends_with("10-valid.toml"));
        Ok(())
    }

    #[test]
    fn resolve_directories_prefers_override_argument() -> Result<()> {
        let temp = TempDir::new()?;
        let override_dir = temp.child("override");
        override_dir.create_dir_all()?;

        unsafe {
            std::env::remove_var("TX_CONFIG_DIR");
        }

        let dirs = resolve_directories(Some(override_dir.path()))?;
        assert_eq!(dirs.config_dir, override_dir.path());
        Ok(())
    }

    #[test]
    fn load_creates_default_layout_when_dirs_missing() -> Result<()> {
        let _guard = ENV_LOCK.lock().unwrap();
        let temp = TempDir::new()?;
        let config_dir = temp.child("config");
        let data_dir = temp.child("data");
        let cache_dir = temp.child("cache");
        let home_dir = temp.child("home");
        home_dir.create_dir_all()?;
        let codex_home = temp.child("codex-home");
        codex_home.create_dir_all()?;

        let orig_env = [
            ("TX_CONFIG_DIR", std::env::var("TX_CONFIG_DIR").ok()),
            ("TX_DATA_DIR", std::env::var("TX_DATA_DIR").ok()),
            ("TX_CACHE_DIR", std::env::var("TX_CACHE_DIR").ok()),
            ("HOME", std::env::var("HOME").ok()),
            ("USERPROFILE", std::env::var("USERPROFILE").ok()),
            ("CODEX_HOME", std::env::var("CODEX_HOME").ok()),
        ];

        unsafe {
            std::env::set_var("TX_CONFIG_DIR", config_dir.path());
            std::env::set_var("TX_DATA_DIR", data_dir.path());
            std::env::set_var("TX_CACHE_DIR", cache_dir.path());
            std::env::set_var("HOME", home_dir.path());
            std::env::set_var("USERPROFILE", home_dir.path());
            std::env::set_var("CODEX_HOME", codex_home.path());
        }

        let loaded = load(None)?;
        assert!(loaded.directories.config_dir.exists());
        assert!(loaded.directories.data_dir.exists());
        assert!(loaded.directories.cache_dir.exists());
        assert!(loaded.directories.config_dir.join(MAIN_CONFIG).is_file());
        assert!(
            !loaded.directories.config_dir.join(DROPIN_DIR).exists(),
            "drop-in directory should be created lazily"
        );
        assert!(
            loaded
                .sources
                .iter()
                .any(|src| matches!(src.kind, ConfigSourceKind::Main)),
            "expected main config source"
        );

        drop(loaded);

        for (key, value) in orig_env {
            if let Some(val) = value {
                unsafe { std::env::set_var(key, val) };
            } else {
                unsafe { std::env::remove_var(key) };
            }
        }

        Ok(())
    }
}

use assert_fs::TempDir;
use assert_fs::prelude::*;
use color_eyre::Result;
#[cfg(windows)]
use std::path::Path;
#[cfg(windows)]
fn lower_path(path: &Path) -> String {
    path.to_string_lossy().to_ascii_lowercase()
}
#[cfg(not(windows))]
use std::path::PathBuf;
use tool_executor::config;
use tool_executor::test_support::ENV_LOCK;

fn set_env(key: &str, value: &std::path::Path) {
    unsafe {
        std::env::set_var(key, value);
    }
}

fn clear_env(key: &str, original: Option<String>) {
    unsafe {
        if let Some(value) = original {
            std::env::set_var(key, value);
        } else {
            std::env::remove_var(key);
        }
    }
}

#[test]
fn load_merges_dropins_with_override_directory() -> Result<()> {
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
        .child("10-provider.toml")
        .write_str("[providers.extra]\nbin = \"cat\"\n")?;

    let data_dir = temp.child("data");
    let cache_dir = temp.child("cache");
    data_dir.create_dir_all()?;
    cache_dir.create_dir_all()?;

    let orig_data = std::env::var("TX_DATA_DIR").ok();
    let orig_cache = std::env::var("TX_CACHE_DIR").ok();
    set_env("TX_DATA_DIR", data_dir.path());
    set_env("TX_CACHE_DIR", cache_dir.path());

    let loaded = config::load(Some(config_dir.path()))?;
    assert!(loaded.config.providers.contains_key("echo"));
    assert!(loaded.config.providers.contains_key("extra"));
    assert!(loaded.directories.config_dir.join("conf.d").is_dir());

    clear_env("TX_DATA_DIR", orig_data);
    clear_env("TX_CACHE_DIR", orig_cache);
    Ok(())
}

#[test]
fn load_creates_default_layout_when_config_missing() -> Result<()> {
    let _guard = ENV_LOCK.lock().unwrap();
    let temp = TempDir::new()?;
    let config_dir = temp.child("config");
    let data_dir = temp.child("data");
    let cache_dir = temp.child("cache");
    let home_dir = temp.child("home");
    let codex_home = temp.child("codex-home");
    home_dir.create_dir_all()?;
    codex_home.create_dir_all()?;
    data_dir.create_dir_all()?;
    cache_dir.create_dir_all()?;

    let orig_env = [
        ("TX_CONFIG_DIR", std::env::var("TX_CONFIG_DIR").ok()),
        ("TX_DATA_DIR", std::env::var("TX_DATA_DIR").ok()),
        ("TX_CACHE_DIR", std::env::var("TX_CACHE_DIR").ok()),
        ("HOME", std::env::var("HOME").ok()),
        ("USERPROFILE", std::env::var("USERPROFILE").ok()),
        ("CODEX_HOME", std::env::var("CODEX_HOME").ok()),
    ];

    set_env("TX_CONFIG_DIR", config_dir.path());
    set_env("TX_DATA_DIR", data_dir.path());
    set_env("TX_CACHE_DIR", cache_dir.path());
    set_env("HOME", home_dir.path());
    set_env("USERPROFILE", home_dir.path());
    set_env("CODEX_HOME", codex_home.path());

    let loaded = config::load(None)?;
    assert!(loaded.directories.config_dir.join("config.toml").is_file());
    assert!(
        !loaded.directories.config_dir.join("conf.d").exists(),
        "drop-in directory should be created lazily"
    );
    assert_eq!(loaded.directories.config_dir, config_dir.path());

    for (key, original) in orig_env {
        clear_env(key, original);
    }
    Ok(())
}

#[test]
#[allow(clippy::too_many_lines)]
fn load_creates_default_layout_with_default_directories() -> Result<()> {
    let _guard = ENV_LOCK.lock().unwrap();
    let temp = TempDir::new()?;
    let home_dir = temp.child("home");
    let codex_home = temp.child("codex-home");
    home_dir.create_dir_all()?;
    codex_home.create_dir_all()?;

    #[cfg(not(windows))]
    let xdg_config = temp.child("xdg-config");
    #[cfg(not(windows))]
    let xdg_data = temp.child("xdg-data");
    #[cfg(not(windows))]
    let xdg_cache = temp.child("xdg-cache");

    #[cfg(not(windows))]
    {
        xdg_config.create_dir_all()?;
        xdg_data.create_dir_all()?;
        xdg_cache.create_dir_all()?;
    }

    #[cfg(windows)]
    let appdata_dir = temp.child("appdata");
    #[cfg(windows)]
    let localappdata_dir = temp.child("localappdata");
    #[cfg(windows)]
    {
        appdata_dir.create_dir_all()?;
        localappdata_dir.create_dir_all()?;
    }

    let original_home = std::env::var("HOME").ok();
    let original_user = std::env::var("USERPROFILE").ok();
    let original_codex = std::env::var("CODEX_HOME").ok();
    #[cfg(not(windows))]
    let original_xdg_config = std::env::var("XDG_CONFIG_HOME").ok();
    #[cfg(not(windows))]
    let original_xdg_data = std::env::var("XDG_DATA_HOME").ok();
    #[cfg(not(windows))]
    let original_xdg_cache = std::env::var("XDG_CACHE_HOME").ok();
    #[cfg(windows)]
    let original_appdata = std::env::var("APPDATA").ok();
    #[cfg(windows)]
    let original_localappdata = std::env::var("LOCALAPPDATA").ok();
    let original_tx_config = std::env::var("TX_CONFIG_DIR").ok();
    let original_tx_data = std::env::var("TX_DATA_DIR").ok();
    let original_tx_cache = std::env::var("TX_CACHE_DIR").ok();

    let run_result = (|| -> Result<()> {
        set_env("HOME", home_dir.path());
        set_env("USERPROFILE", home_dir.path());
        set_env("CODEX_HOME", codex_home.path());
        #[cfg(not(windows))]
        {
            set_env("XDG_CONFIG_HOME", xdg_config.path());
            set_env("XDG_DATA_HOME", xdg_data.path());
            set_env("XDG_CACHE_HOME", xdg_cache.path());
        }
        #[cfg(windows)]
        {
            set_env("APPDATA", appdata_dir.path());
            set_env("LOCALAPPDATA", localappdata_dir.path());
        }
        unsafe {
            std::env::remove_var("TX_CONFIG_DIR");
            std::env::remove_var("TX_DATA_DIR");
            std::env::remove_var("TX_CACHE_DIR");
        }

        let loaded = config::load(None)?;
        #[cfg(not(windows))]
        let expected_config_dir =
            PathBuf::from(std::env::var("XDG_CONFIG_HOME").expect("XDG_CONFIG_HOME")).join("tx");
        #[cfg(not(windows))]
        let expected_data_dir =
            PathBuf::from(std::env::var("XDG_DATA_HOME").expect("XDG_DATA_HOME")).join("tx");
        #[cfg(not(windows))]
        let expected_cache_dir =
            PathBuf::from(std::env::var("XDG_CACHE_HOME").expect("XDG_CACHE_HOME")).join("tx");
        #[cfg(not(windows))]
        {
            assert_eq!(loaded.directories.config_dir, expected_config_dir);
            assert_eq!(loaded.directories.data_dir, expected_data_dir);
            assert_eq!(loaded.directories.cache_dir, expected_cache_dir);
            assert!(expected_config_dir.join("config.toml").is_file());
            assert!(
                !expected_config_dir.join("conf.d").exists(),
                "drop-in directory should not be created by default"
            );
            assert!(expected_data_dir.exists());
            assert!(expected_cache_dir.exists());
        }
        #[cfg(windows)]
        {
            let config_tail = lower_path(&loaded.directories.config_dir);
            let data_tail = lower_path(&loaded.directories.data_dir);
            let cache_tail = lower_path(&loaded.directories.cache_dir);

            assert!(
                config_tail.ends_with("tx\\config")
                    || config_tail.ends_with("tx\\")
                    || config_tail.ends_with("tx"),
                "expected path ending with tx\\config or tx, got {config_tail}"
            );
            assert!(
                data_tail.ends_with("tx\\data")
                    || data_tail.ends_with("tx\\")
                    || data_tail.ends_with("tx"),
                "expected path ending with tx\\data or tx, got {data_tail}"
            );
            assert!(
                cache_tail.ends_with("tx\\cache")
                    || cache_tail.ends_with("tx\\")
                    || cache_tail.ends_with("tx"),
                "expected path ending with tx\\cache or tx, got {cache_tail}"
            );
            assert!(loaded.directories.config_dir.join("config.toml").is_file());
            assert!(
                !loaded.directories.config_dir.join("conf.d").exists(),
                "drop-in directory should not be created by default"
            );
            assert!(loaded.directories.data_dir.exists());
            assert!(loaded.directories.cache_dir.exists());
        }
        Ok(())
    })();

    clear_env("HOME", original_home);
    clear_env("USERPROFILE", original_user);
    clear_env("CODEX_HOME", original_codex);
    #[cfg(not(windows))]
    clear_env("XDG_CONFIG_HOME", original_xdg_config);
    #[cfg(not(windows))]
    clear_env("XDG_DATA_HOME", original_xdg_data);
    #[cfg(not(windows))]
    clear_env("XDG_CACHE_HOME", original_xdg_cache);
    #[cfg(windows)]
    clear_env("APPDATA", original_appdata);
    #[cfg(windows)]
    clear_env("LOCALAPPDATA", original_localappdata);
    clear_env("TX_CONFIG_DIR", original_tx_config);
    clear_env("TX_DATA_DIR", original_tx_data);
    clear_env("TX_CACHE_DIR", original_tx_cache);

    run_result
}

#[test]
fn load_trims_profile_description_and_prompt_assembler() -> Result<()> {
    let _guard = ENV_LOCK.lock().unwrap();
    let temp = TempDir::new()?;
    let config_dir = temp.child("config");
    config_dir.create_dir_all()?;
    config_dir.child("config.toml").write_str(
        r#"
provider = "codex"

[providers.codex]
bin = "codex"

[profiles.trimmed]
provider = "codex"
description = "  useful summary  "
prompt_assembler = "  prompts/demo  "

[profiles.empty]
provider = "codex"
description = "   "
prompt_assembler = "   "
"#,
    )?;

    let data_dir = temp.child("data");
    let cache_dir = temp.child("cache");
    data_dir.create_dir_all()?;
    cache_dir.create_dir_all()?;

    let orig_data = std::env::var("TX_DATA_DIR").ok();
    let orig_cache = std::env::var("TX_CACHE_DIR").ok();
    set_env("TX_DATA_DIR", data_dir.path());
    set_env("TX_CACHE_DIR", cache_dir.path());

    let loaded = config::load(Some(config_dir.path()))?;
    let trimmed = loaded
        .config
        .profiles
        .get("trimmed")
        .expect("trimmed profile");
    assert_eq!(trimmed.description.as_deref(), Some("useful summary"));
    assert_eq!(trimmed.prompt_assembler.as_deref(), Some("prompts/demo"));
    let empty = loaded.config.profiles.get("empty").expect("empty profile");
    assert_eq!(empty.description, None);
    assert_eq!(empty.prompt_assembler, None);

    clear_env("TX_DATA_DIR", orig_data);
    clear_env("TX_CACHE_DIR", orig_cache);
    Ok(())
}

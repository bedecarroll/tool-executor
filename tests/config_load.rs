use assert_fs::TempDir;
use assert_fs::prelude::*;
use color_eyre::Result;
use tool_executor::config;

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
fn load_creates_default_layout_with_default_directories() -> Result<()> {
    let temp = TempDir::new()?;
    let home_dir = temp.child("home");
    let codex_home = temp.child("codex-home");
    home_dir.create_dir_all()?;
    codex_home.create_dir_all()?;

    let xdg_config = temp.child("xdg-config");
    let xdg_data = temp.child("xdg-data");
    let xdg_cache = temp.child("xdg-cache");

    let original_home = std::env::var("HOME").ok();
    let original_user = std::env::var("USERPROFILE").ok();
    let original_codex = std::env::var("CODEX_HOME").ok();
    let original_xdg_config = std::env::var("XDG_CONFIG_HOME").ok();
    let original_xdg_data = std::env::var("XDG_DATA_HOME").ok();
    let original_xdg_cache = std::env::var("XDG_CACHE_HOME").ok();
    let original_tx_config = std::env::var("TX_CONFIG_DIR").ok();
    let original_tx_data = std::env::var("TX_DATA_DIR").ok();
    let original_tx_cache = std::env::var("TX_CACHE_DIR").ok();

    let run_result = (|| -> Result<()> {
        set_env("HOME", home_dir.path());
        set_env("USERPROFILE", home_dir.path());
        set_env("CODEX_HOME", codex_home.path());
        set_env("XDG_CONFIG_HOME", xdg_config.path());
        set_env("XDG_DATA_HOME", xdg_data.path());
        set_env("XDG_CACHE_HOME", xdg_cache.path());
        unsafe {
            std::env::remove_var("TX_CONFIG_DIR");
            std::env::remove_var("TX_DATA_DIR");
            std::env::remove_var("TX_CACHE_DIR");
        }

        let loaded = config::load(None)?;
        let expected_config_dir = xdg_config.child("tx");
        assert_eq!(loaded.directories.config_dir, expected_config_dir.path());
        assert!(expected_config_dir.child("config.toml").path().is_file());
        assert!(
            !expected_config_dir.child("conf.d").path().exists(),
            "drop-in directory should not be created by default"
        );
        Ok(())
    })();

    clear_env("HOME", original_home);
    clear_env("USERPROFILE", original_user);
    clear_env("CODEX_HOME", original_codex);
    clear_env("XDG_CONFIG_HOME", original_xdg_config);
    clear_env("XDG_DATA_HOME", original_xdg_data);
    clear_env("XDG_CACHE_HOME", original_xdg_cache);
    clear_env("TX_CONFIG_DIR", original_tx_config);
    clear_env("TX_DATA_DIR", original_tx_data);
    clear_env("TX_CACHE_DIR", original_tx_cache);

    run_result
}

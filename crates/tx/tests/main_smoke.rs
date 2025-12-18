use assert_cmd::Command;
use assert_fs::TempDir;
use assert_fs::prelude::*;
use std::error::Error;
use std::fs;

/// Basic smoke test to exercise the thin re-exporting binary in this crate.
#[test]
fn main_runs_config_where() -> Result<(), Box<dyn Error>> {
    // Provide a minimal config so tx can start.
    let temp = TempDir::new()?;
    let config_dir = temp.child("config");
    config_dir.create_dir_all()?;
    fs::write(config_dir.child("config.toml"), "provider = \"echo\"\n")?;

    let data_dir = temp.child("data");
    data_dir.create_dir_all()?;
    let cache_dir = temp.child("cache");
    cache_dir.create_dir_all()?;

    Command::cargo_bin("tx")?
        .env("TX_CONFIG_DIR", config_dir.path())
        .env("TX_DATA_DIR", data_dir.path())
        .env("TX_CACHE_DIR", cache_dir.path())
        .arg("config")
        .arg("where")
        .assert()
        .success();

    Ok(())
}

#![cfg(unix)]

use assert_cmd::Command;
use assert_fs::TempDir;
use assert_fs::prelude::*;
use predicates::str::contains;
use tool_executor::test_support::ENV_LOCK;

/// Exercises the error-printing path in src/bin/tx.rs (caused by missing session).
#[test]
fn resume_missing_session_prints_error_chain() -> Result<(), Box<dyn std::error::Error>> {
    let _guard = ENV_LOCK.lock().unwrap();
    let temp = TempDir::new()?;
    let config_dir = temp.child("config-root");
    config_dir.create_dir_all()?;
    // Minimal provider definition so CLI parsing succeeds.
    config_dir
        .child("config.toml")
        .write_str("provider = \"echo\"\n[providers.echo]\nbin = \"echo\"\n")?;

    let data_dir = temp.child("data-root");
    data_dir.create_dir_all()?;
    let cache_dir = temp.child("cache-root");
    cache_dir.create_dir_all()?;

    #[allow(deprecated)]
    Command::cargo_bin("tx-dev")?
        .env("TX_CONFIG_DIR", config_dir.path())
        .env("TX_DATA_DIR", data_dir.path())
        .env("TX_CACHE_DIR", cache_dir.path())
        .env("TX_SKIP_INDEX", "1")
        .arg("resume")
        .arg("missing")
        .assert()
        .failure()
        .stderr(contains("session 'missing' not found"));

    Ok(())
}

/// Exercises the error-chain printing when parsing config fails.
#[test]
fn config_parse_error_prints_error_chain() -> Result<(), Box<dyn std::error::Error>> {
    let _guard = ENV_LOCK.lock().unwrap();
    let temp = TempDir::new()?;
    let config_dir = temp.child("bad-config");
    config_dir.create_dir_all()?;
    config_dir.child("config.toml").write_str("provider =")?;

    let data_dir = temp.child("data-root");
    data_dir.create_dir_all()?;
    let cache_dir = temp.child("cache-root");
    cache_dir.create_dir_all()?;

    #[allow(deprecated)]
    Command::cargo_bin("tx-dev")?
        .env("TX_CONFIG_DIR", config_dir.path())
        .env("TX_DATA_DIR", data_dir.path())
        .env("TX_CACHE_DIR", cache_dir.path())
        .env("TX_SKIP_INDEX", "1")
        .arg("config")
        .arg("lint")
        .assert()
        .failure()
        .stderr(contains("caused by:"));

    Ok(())
}

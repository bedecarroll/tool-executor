use assert_cmd::Command;
use assert_fs::TempDir;
use assert_fs::prelude::*;
use predicates::str::contains;

/// Exercises the error-printing path in src/bin/tx.rs (caused by missing session).
#[test]
fn resume_missing_session_prints_error_chain() -> Result<(), Box<dyn std::error::Error>> {
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

    Command::cargo_bin("tx")?
        .env("TX_CONFIG_DIR", config_dir.path())
        .env("TX_DATA_DIR", data_dir.path())
        .env("TX_CACHE_DIR", cache_dir.path())
        .arg("resume")
        .arg("missing")
        .assert()
        .failure()
        .stderr(contains("tx: session 'missing' not found"));

    Ok(())
}

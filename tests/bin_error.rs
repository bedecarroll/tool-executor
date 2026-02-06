#![cfg(unix)]

use assert_cmd::Command;
use assert_fs::TempDir;
use assert_fs::prelude::*;
use predicates::str::contains;
use tool_executor::test_support::ENV_LOCK;
use tool_executor::{
    db::Database,
    session::{MessageRecord, SessionIngest, SessionSummary},
};

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

/// Provider mismatch failures should map to exit code 2.
#[test]
fn provider_mismatch_exits_with_code_2() -> Result<(), Box<dyn std::error::Error>> {
    let _guard = ENV_LOCK.lock().unwrap();
    let temp = TempDir::new()?;
    let config_dir = temp.child("config-root");
    config_dir.create_dir_all()?;
    let sessions_dir = config_dir.child("sessions");
    sessions_dir.create_dir_all()?;
    config_dir.child("config.toml").write_str(&format!(
        r#"
provider = "demo"

[providers.demo]
bin = "echo"
session_roots = ["{}"]

[profiles.default]
provider = "demo"

[profiles.alt]
provider = "alt"
"#,
        sessions_dir.path().display()
    ))?;

    let data_dir = temp.child("data-root");
    data_dir.create_dir_all()?;
    let cache_dir = temp.child("cache-root");
    cache_dir.create_dir_all()?;
    let db_path = data_dir.child("tx.sqlite3");
    let mut db = Database::open(db_path.path())?;

    let session_path = sessions_dir.child("sess-1.jsonl");
    session_path.write_str(
        "{\"type\":\"event_msg\",\"payload\":{\"type\":\"user_message\",\"message\":\"hello\"}}\n",
    )?;

    let summary = SessionSummary {
        id: "sess-1".into(),
        provider: "demo".into(),
        wrapper: None,
        model: None,
        label: Some("Demo".into()),
        path: session_path.path().to_path_buf(),
        uuid: Some("uuid-1".into()),
        first_prompt: Some("Hello".into()),
        actionable: true,
        created_at: Some(0),
        started_at: Some(0),
        last_active: Some(0),
        size: 1,
        mtime: 0,
    };
    let mut message = MessageRecord::new(summary.id.clone(), 0, "user", "Hello", None, Some(0));
    message.is_first = true;
    db.upsert_session(&SessionIngest::new(summary, vec![message]))?;

    #[allow(deprecated)]
    Command::cargo_bin("tx-dev")?
        .env("TX_CONFIG_DIR", config_dir.path())
        .env("TX_DATA_DIR", data_dir.path())
        .env("TX_CACHE_DIR", cache_dir.path())
        .env("TX_SKIP_INDEX", "1")
        .arg("resume")
        .arg("uuid-1")
        .arg("--profile")
        .arg("alt")
        .assert()
        .code(2)
        .stderr(contains("provider mismatch"));

    Ok(())
}

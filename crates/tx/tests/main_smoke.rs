use assert_cmd::Command;
use assert_fs::TempDir;
use assert_fs::prelude::*;
use std::error::Error;
use std::fs;
use tool_executor::{
    db::Database,
    session::{MessageRecord, SessionIngest, SessionSummary},
    test_support::toml_path,
};

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

    #[allow(deprecated)]
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

#[test]
fn main_maps_provider_mismatch_to_exit_code_2() -> Result<(), Box<dyn Error>> {
    let temp = TempDir::new()?;
    let config_dir = temp.child("config");
    config_dir.create_dir_all()?;
    let sessions_dir = config_dir.child("sessions");
    sessions_dir.create_dir_all()?;

    fs::write(
        config_dir.child("config.toml"),
        format!(
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
            toml_path(sessions_dir.path())
        ),
    )?;

    let data_dir = temp.child("data");
    data_dir.create_dir_all()?;
    let cache_dir = temp.child("cache");
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
    Command::cargo_bin("tx")?
        .env("TX_CONFIG_DIR", config_dir.path())
        .env("TX_DATA_DIR", data_dir.path())
        .env("TX_CACHE_DIR", cache_dir.path())
        .env("TX_SKIP_INDEX", "1")
        .arg("resume")
        .arg("uuid-1")
        .arg("--profile")
        .arg("alt")
        .assert()
        .code(2);

    Ok(())
}

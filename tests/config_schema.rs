use assert_cmd::Command;
use assert_fs::TempDir;
use assert_fs::prelude::*;
use color_eyre::Result;
use predicates::str::is_empty;
use serde_json::Value;

fn schema_command(temp: &TempDir) -> Result<Command> {
    let mut cmd = Command::cargo_bin("tx")?;
    let config_dir = temp.child("config");
    config_dir.create_dir_all()?;

    let data_dir = temp.child("data");
    data_dir.create_dir_all()?;
    let cache_dir = temp.child("cache");
    cache_dir.create_dir_all()?;
    let home_dir = temp.child("home");
    home_dir.create_dir_all()?;
    let codex_home = temp.child("codex-home");
    codex_home.create_dir_all()?;

    cmd.env("TX_CONFIG_DIR", config_dir.path());
    cmd.env("TX_DATA_DIR", data_dir.path());
    cmd.env("TX_CACHE_DIR", cache_dir.path());
    cmd.env("XDG_DATA_HOME", data_dir.path());
    cmd.env("XDG_CACHE_HOME", cache_dir.path());
    cmd.env("HOME", home_dir.path());
    cmd.env("USERPROFILE", home_dir.path());
    cmd.env("CODEX_HOME", codex_home.path());
    cmd.env("TX_SKIP_INDEX", "1");

    Ok(cmd)
}

#[test]
fn config_schema_outputs_json_object() -> Result<()> {
    let temp = TempDir::new()?;
    let mut cmd = schema_command(&temp)?;
    cmd.args(["config", "schema"]);

    let assert = cmd.assert().success().stderr(is_empty());
    let stdout = String::from_utf8(assert.get_output().stdout.clone())?;
    let schema: Value = serde_json::from_str(&stdout)?;

    let root = schema.as_object().expect("root schema to be an object");
    assert!(root.contains_key("$schema"), "schema draft url missing");
    assert!(
        root.get("properties").and_then(Value::as_object).is_some(),
        "expected root schema to define properties"
    );

    temp.close()?;
    Ok(())
}

#[test]
fn config_schema_pretty_outputs_json_object() -> Result<()> {
    let temp = TempDir::new()?;
    let mut cmd = schema_command(&temp)?;
    cmd.args(["config", "schema", "--pretty"]);

    let assert = cmd.assert().success().stderr(is_empty());
    let stdout = String::from_utf8(assert.get_output().stdout.clone())?;
    let schema: Value = serde_json::from_str(&stdout)?;

    assert!(stdout.contains('\n'));
    assert!(schema.is_object());

    temp.close()?;
    Ok(())
}

use assert_cmd::Command;
use color_eyre::Result;
use predicates::str::is_empty;
use serde_json::Value;
use std::fs;
use std::path::PathBuf;
use tool_executor::config;

#[test]
fn config_schema_outputs_json_object() -> Result<()> {
    let mut cmd = Command::cargo_bin("tx")?;
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

    Ok(())
}

#[test]
fn config_schema_docs_asset_matches_cli_output() -> Result<()> {
    let mut cmd = Command::cargo_bin("tx")?;
    cmd.args(["config", "schema", "--pretty"]);

    let assert = cmd.assert().success().stderr(is_empty());
    let cli_stdout = String::from_utf8(assert.get_output().stdout.clone())?;

    let asset_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("docs")
        .join("src")
        .join("assets")
        .join("config-schema.json");
    let asset_contents = fs::read_to_string(&asset_path)?;

    assert_eq!(
        asset_contents.trim_end(),
        cli_stdout.trim_end(),
        "docs asset at {} is out of date",
        asset_path.display()
    );

    Ok(())
}

#[test]
fn config_schema_exposes_stdin_mode_enum() -> Result<()> {
    let schema_json = config::schema(false)?;
    let schema: Value = serde_json::from_str(&schema_json)?;

    let definitions = schema
        .get("definitions")
        .and_then(Value::as_object)
        .expect("schema definitions to exist");
    let raw_provider = definitions
        .get("RawProvider")
        .and_then(Value::as_object)
        .expect("RawProvider definition to exist");
    let properties = raw_provider
        .get("properties")
        .and_then(Value::as_object)
        .expect("RawProvider properties to exist");
    let stdin_mode = properties
        .get("stdin_mode")
        .and_then(Value::as_object)
        .expect("stdin_mode property to exist");
    let reference = stdin_mode
        .get("$ref")
        .and_then(Value::as_str)
        .expect("stdin_mode to reference RawStdinMode definition");
    assert_eq!(reference, "#/definitions/RawStdinMode");

    let mode_definition = definitions
        .get("RawStdinMode")
        .and_then(Value::as_object)
        .expect("RawStdinMode definition to exist");
    let enum_values = mode_definition
        .get("enum")
        .and_then(Value::as_array)
        .expect("RawStdinMode to define enumerated values");

    let captured: Vec<_> = enum_values
        .iter()
        .map(|value| value.as_str().expect("enum value to be a string"))
        .collect();

    assert_eq!(
        captured,
        ["pipe", "capture_arg"],
        "stdin_mode enum must list canonical values"
    );

    Ok(())
}

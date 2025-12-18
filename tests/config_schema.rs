use assert_fs::TempDir;
use assert_fs::prelude::*;
use color_eyre::Result;
use predicates::str::is_empty;
use serde_json::Value;
use std::fs;
use std::path::PathBuf;
use tool_executor::config;

#[test]
fn config_schema_outputs_json_object() -> Result<()> {
    let temp = TempDir::new()?;
    let config_dir = temp.child("config");
    let data_dir = temp.child("data");
    let cache_dir = temp.child("cache");
    config_dir.create_dir_all()?;
    data_dir.create_dir_all()?;
    cache_dir.create_dir_all()?;

    let mut cmd = assert_cmd::cargo::cargo_bin_cmd!("tx");
    cmd.args(["config", "schema"])
        .env("TX_CONFIG_DIR", config_dir.path())
        .env("TX_DATA_DIR", data_dir.path())
        .env("TX_CACHE_DIR", cache_dir.path());

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
    let temp = TempDir::new()?;
    let config_dir = temp.child("config");
    let data_dir = temp.child("data");
    let cache_dir = temp.child("cache");
    config_dir.create_dir_all()?;
    data_dir.create_dir_all()?;
    cache_dir.create_dir_all()?;

    let mut cmd = assert_cmd::cargo::cargo_bin_cmd!("tx");
    cmd.args(["config", "schema", "--pretty"])
        .env("TX_CONFIG_DIR", config_dir.path())
        .env("TX_DATA_DIR", data_dir.path())
        .env("TX_CACHE_DIR", cache_dir.path());

    let assert = cmd.assert().success().stderr(is_empty());
    let cli_stdout = String::from_utf8(assert.get_output().stdout.clone())?;
    let cli_normalized = normalize_newlines(&cli_stdout);

    let asset_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("docs")
        .join("src")
        .join("assets")
        .join("config-schema.json");
    let asset_contents = fs::read_to_string(&asset_path)?;
    let asset_normalized = normalize_newlines(&asset_contents);

    assert_eq!(
        asset_normalized.trim_end(),
        cli_normalized.trim_end(),
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
        .get("$defs")
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
    assert_eq!(reference, "#/$defs/RawStdinMode");

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

#[test]
fn config_schema_includes_profile_prompt_assembler() -> Result<()> {
    let schema_json = config::schema(false)?;
    let schema: Value = serde_json::from_str(&schema_json)?;

    let definitions = schema
        .get("$defs")
        .and_then(Value::as_object)
        .expect("schema definitions to exist");
    let raw_profile = definitions
        .get("RawProfile")
        .and_then(Value::as_object)
        .expect("RawProfile definition to exist");
    let properties = raw_profile
        .get("properties")
        .and_then(Value::as_object)
        .expect("RawProfile properties to exist");
    let prompt_assembler = properties
        .get("prompt_assembler")
        .and_then(Value::as_object)
        .expect("prompt_assembler property to exist");

    let ty = prompt_assembler
        .get("type")
        .and_then(Value::as_array)
        .expect("prompt_assembler type to be an array");
    let types: Vec<_> = ty
        .iter()
        .map(|value| value.as_str().expect("type value to be a string"))
        .collect();

    assert!(
        types.contains(&"string"),
        "prompt_assembler should accept strings"
    );
    assert!(
        types.contains(&"null"),
        "prompt_assembler should accept null"
    );

    let prompt_args = properties
        .get("prompt_assembler_args")
        .and_then(Value::as_object)
        .expect("prompt_assembler_args property to exist");
    assert_eq!(
        prompt_args
            .get("type")
            .and_then(Value::as_str)
            .expect("prompt_assembler_args type to exist"),
        "array"
    );
    let items = prompt_args
        .get("items")
        .and_then(Value::as_object)
        .expect("prompt_assembler_args items to exist");
    assert_eq!(
        items
            .get("type")
            .and_then(Value::as_str)
            .expect("prompt_assembler_args item type to exist"),
        "string"
    );

    Ok(())
}

fn normalize_newlines(input: &str) -> String {
    input.replace("\r\n", "\n")
}

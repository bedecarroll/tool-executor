use assert_cmd::Command;
use color_eyre::Result;
use predicates::str::is_empty;
use serde_json::Value;

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

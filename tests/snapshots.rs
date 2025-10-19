use assert_cmd::Command;
use insta::assert_snapshot;

#[test]
fn greet_command_snapshot() -> color_eyre::Result<()> {
    let mut cmd = Command::cargo_bin("tx")?;
    let assert = cmd.args(["greet", "--name", "snapshot"]).assert().success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone())?;
    assert_snapshot!("greet_command_snapshot", stdout);
    Ok(())
}

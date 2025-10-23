use assert_cmd::Command;
use assert_fs::TempDir;
use assert_fs::prelude::*;
use predicates::str::contains;
use serde_json::Value;
use serde_json::json;

fn base_command(temp: &TempDir) -> Command {
    let mut cmd = Command::cargo_bin("tx").expect("binary exists");
    let config_dir = temp.child("config-root");
    config_dir.create_dir_all().unwrap();
    cmd.env("TX_CONFIG_DIR", config_dir.path());

    let data_dir = temp.child("data-root");
    data_dir.create_dir_all().unwrap();
    cmd.env("XDG_DATA_HOME", data_dir.path());
    cmd.env("TX_DATA_DIR", data_dir.path());

    let cache_dir = temp.child("cache-root");
    cache_dir.create_dir_all().unwrap();
    cmd.env("XDG_CACHE_HOME", cache_dir.path());
    cmd.env("TX_CACHE_DIR", cache_dir.path());

    let home_dir = temp.child("home");
    home_dir.create_dir_all().unwrap();
    cmd.env("HOME", home_dir.path());
    cmd.env("USERPROFILE", home_dir.path());

    let codex_home = temp.child("codex-home");
    codex_home.create_dir_all().unwrap();
    cmd.env("CODEX_HOME", codex_home.path());
    cmd
}

#[test]
fn search_lists_empty_when_no_data() -> color_eyre::Result<()> {
    let temp = TempDir::new()?;
    let mut cmd = base_command(&temp);
    let output = cmd.arg("search").output()?;
    assert!(output.status.success());
    let parsed: Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(parsed, json!([]));
    temp.close()?;
    Ok(())
}

#[test]
fn search_reports_empty_when_no_matches() -> color_eyre::Result<()> {
    let temp = TempDir::new()?;
    let mut cmd = base_command(&temp);
    let output = cmd.args(["search", "missing"]).output()?;
    assert!(output.status.success());
    let parsed: Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(parsed, json!([]));
    temp.close()?;
    Ok(())
}

#[test]
fn launch_dry_run_prints_pipeline() -> color_eyre::Result<()> {
    let temp = TempDir::new()?;
    let config_dir = temp.child("config-root");
    config_dir.create_dir_all()?;
    let config_path = config_dir.child("config.toml");
    let mut root = toml::map::Map::new();
    let mut providers = toml::map::Map::new();
    let mut echo = toml::map::Map::new();
    echo.insert("bin".into(), toml::Value::String("echo".into()));
    echo.insert(
        "flags".into(),
        toml::Value::Array(Vec::<toml::Value>::new()),
    );
    echo.insert("env".into(), toml::Value::Array(Vec::<toml::Value>::new()));
    providers.insert("echo".into(), toml::Value::Table(echo));
    root.insert("providers".into(), toml::Value::Table(providers));
    let config_contents = toml::to_string(&toml::Value::Table(root))?;
    std::fs::write(config_path.path(), config_contents)?;
    let written = std::fs::read_to_string(config_path.path())?;
    toml::from_str::<toml::Value>(&written).expect("valid launch config");

    let mut cmd = base_command(&temp);
    cmd.arg("launch")
        .arg("echo")
        .arg("--dry-run")
        .assert()
        .success()
        .stdout(contains("echo"));

    temp.close()?;
    Ok(())
}

#[test]
fn config_dump_outputs_merged_toml() -> color_eyre::Result<()> {
    let temp = TempDir::new()?;
    let config_dir = temp.child("config-root");
    config_dir.create_dir_all()?;
    let config_path = config_dir.child("config.toml");
    std::fs::write(config_path.path(), "provider = \"echo\"\n")?;
    let written = std::fs::read_to_string(config_path.path())?;
    toml::from_str::<toml::Value>(&written).expect("valid defaults config");
    let conf_d = config_dir.child("conf.d");
    conf_d.create_dir_all()?;
    std::fs::write(
        conf_d.child("00-extra.toml").path(),
        "search_mode = \"full_text\"\n",
    )?;

    let mut cmd = base_command(&temp);
    cmd.env("TX_CONFIG_DIR", config_dir.path())
        .arg("config")
        .arg("dump")
        .assert()
        .success()
        .stdout(contains("provider = \"echo\""))
        .stdout(contains("search_mode = \"full_text\""));

    temp.close()?;
    Ok(())
}

#[test]
fn config_lint_reports_errors_for_bad_config() -> color_eyre::Result<()> {
    let temp = TempDir::new()?;
    let config_dir = temp.child("config-root");
    config_dir.create_dir_all()?;
    std::fs::write(
        config_dir.child("config.toml").path(),
        "[providers.bad]\nflags = []\n",
    )?;

    let mut cmd = base_command(&temp);
    cmd.args(["config", "lint"])
        .assert()
        .failure()
        .stderr(contains("missing required field 'bin'"));

    temp.close()?;
    Ok(())
}

#[test]
fn config_default_outputs_bundled_template() -> color_eyre::Result<()> {
    let temp = TempDir::new()?;
    let mut cmd = base_command(&temp);
    cmd.args(["config", "default"])
        .assert()
        .success()
        .stdout(contains("provider = \"codex\""))
        .stdout(contains("Session logs are discovered automatically"));
    temp.close()?;
    Ok(())
}

#[test]
fn search_help_omits_json_and_emit_flags() -> color_eyre::Result<()> {
    let temp = TempDir::new()?;
    let mut cmd = base_command(&temp);
    let output = cmd.args(["search", "--help"]).output()?;
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout)?;
    assert!(!stdout.contains("--json"));
    assert!(!stdout.contains("--emit-command"));
    assert!(stdout.contains("--full-text"));
    assert!(stdout.contains("--role"));
    temp.close()?;
    Ok(())
}

#[test]
fn launch_emit_command_prints_pipeline() -> color_eyre::Result<()> {
    let temp = TempDir::new()?;
    let config_dir = temp.child("config-root");
    config_dir.create_dir_all()?;
    let config_path = config_dir.child("config.toml");
    let mut root = toml::map::Map::new();
    let mut providers = toml::map::Map::new();
    let mut echo = toml::map::Map::new();
    echo.insert("bin".into(), toml::Value::String("echo".into()));
    echo.insert(
        "flags".into(),
        toml::Value::Array(Vec::<toml::Value>::new()),
    );
    echo.insert("env".into(), toml::Value::Array(Vec::<toml::Value>::new()));
    providers.insert("echo".into(), toml::Value::Table(echo));
    root.insert("providers".into(), toml::Value::Table(providers));
    let config_contents = toml::to_string(&toml::Value::Table(root))?;
    std::fs::write(config_path.path(), config_contents)?;

    let mut cmd = base_command(&temp);
    cmd.args(["launch", "echo", "--emit-command"])
        .assert()
        .success()
        .stdout(contains("echo"));

    temp.close()?;
    Ok(())
}

#[test]
fn search_role_requires_term() -> color_eyre::Result<()> {
    let temp = TempDir::new()?;
    let mut cmd = base_command(&temp);
    cmd.args(["search", "--role", "user"])
        .assert()
        .failure()
        .stderr(contains("--role requires --full-text"));
    temp.close()?;
    Ok(())
}

#[test]
fn search_role_requires_full_text() -> color_eyre::Result<()> {
    let temp = TempDir::new()?;
    let mut cmd = base_command(&temp);
    cmd.args(["search", "context", "--role", "user"])
        .assert()
        .failure()
        .stderr(contains("--role requires --full-text"));
    temp.close()?;
    Ok(())
}

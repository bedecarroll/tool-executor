use assert_cmd::Command;
use assert_fs::TempDir;
use assert_fs::prelude::*;
use predicates::str::contains;

fn base_command(temp: &TempDir) -> Command {
    let mut cmd = Command::cargo_bin("tx").expect("binary exists");
    let config_dir = temp.child("config-root");
    config_dir.create_dir_all().unwrap();
    cmd.env("TX_CONFIG_DIR", config_dir.path());

    let data_dir = temp.child("data-root");
    data_dir.create_dir_all().unwrap();
    cmd.env("XDG_DATA_HOME", data_dir.path());

    let cache_dir = temp.child("cache-root");
    cache_dir.create_dir_all().unwrap();
    cmd.env("XDG_CACHE_HOME", cache_dir.path());
    cmd
}

#[test]
fn sessions_reports_empty_when_no_data() -> color_eyre::Result<()> {
    let temp = TempDir::new()?;
    let mut cmd = base_command(&temp);
    cmd.arg("sessions")
        .assert()
        .success()
        .stdout(contains("No sessions found."));
    temp.close()?;
    Ok(())
}

#[test]
fn search_reports_empty_when_no_matches() -> color_eyre::Result<()> {
    let temp = TempDir::new()?;
    let mut cmd = base_command(&temp);
    cmd.args(["search", "missing"])
        .assert()
        .success()
        .stdout(contains("No matches found."));
    temp.close()?;
    Ok(())
}

#[test]
fn launch_dry_run_prints_pipeline() -> color_eyre::Result<()> {
    let temp = TempDir::new()?;
    let config_dir = temp.child("config-root");
    config_dir.create_dir_all()?;
    let sessions_dir = temp.child("sessions");
    sessions_dir.create_dir_all()?;
    let config_contents = format!(
        "[providers.echo]\nbin = \"echo\"\nflags = []\nenv = []\nsession_roots = [\"{}\"]\n",
        sessions_dir.path().display()
    );
    let config_path = config_dir.child("config.toml");
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
    std::fs::write(config_path.path(), "[defaults]\nprovider = \"echo\"\n")?;
    let written = std::fs::read_to_string(config_path.path())?;
    toml::from_str::<toml::Value>(&written).expect("valid defaults config");
    let conf_d = config_dir.child("conf.d");
    conf_d.create_dir_all()?;
    std::fs::write(
        conf_d.child("00-extra.toml").path(),
        "[defaults]\nactionable_only = false\n",
    )?;

    let mut cmd = base_command(&temp);
    cmd.env("TX_CONFIG_DIR", config_dir.path())
        .arg("config")
        .arg("dump")
        .assert()
        .success()
        .stdout(contains("[defaults]"));

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

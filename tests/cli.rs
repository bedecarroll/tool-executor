use assert_cmd::Command;
use assert_fs::TempDir;
use assert_fs::prelude::*;
use predicates::prelude::PredicateBooleanExt;
use predicates::str::contains;
use serde_json::Value;
use serde_json::json;
use time::OffsetDateTime;
use tool_executor::db::Database;
use tool_executor::session::{MessageRecord, SessionIngest, SessionSummary};
use tool_executor::test_support::toml_path;

#[allow(deprecated)]
fn base_command(temp: &TempDir) -> Command {
    let mut cmd = Command::cargo_bin("tx-dev").expect("tx-dev binary available");
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
    let output = cmd.args(["search", "--full-text", "Search"]).output()?;
    if !output.status.success() {
        eprintln!("stdout: {}", String::from_utf8_lossy(&output.stdout));
        eprintln!("stderr: {}", String::from_utf8_lossy(&output.stderr));
    }
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
        .stdout(contains("stdin_to = \"codex:{prompt}\""))
        .stdout(contains("Session logs are discovered automatically").not());
    temp.close()?;
    Ok(())
}

#[test]
fn config_default_raw_outputs_template_without_substitution() -> color_eyre::Result<()> {
    let temp = TempDir::new()?;
    let mut cmd = base_command(&temp);
    cmd.args(["config", "default", "--raw"])
        .assert()
        .success()
        .stdout(contains("provider = \"codex\""))
        .stdout(contains("stdin_to = \"codex:{prompt}\""))
        .stdout(contains("Session logs are discovered automatically").not());
    temp.close()?;
    Ok(())
}

#[test]
fn config_list_displays_sections() -> color_eyre::Result<()> {
    let temp = TempDir::new()?;
    let mut cmd = base_command(&temp);
    cmd.args(["config", "list"])
        .assert()
        .success()
        .stdout(contains("Providers:"))
        .stdout(contains("Profiles:"));
    temp.close()?;
    Ok(())
}

#[test]
fn config_list_prints_wrappers_and_profiles() -> color_eyre::Result<()> {
    let temp = TempDir::new()?;
    let config_dir = temp.child("config-root");
    config_dir.create_dir_all()?;
    let sessions_dir = config_dir.child("sessions");
    sessions_dir.create_dir_all()?;
    let config_toml = format!(
        r#"
provider = "demo"

[providers.demo]
bin = "echo"
session_roots = ["{sessions}"]

[wrappers.wrap]
shell = true
cmd = "echo {{CMD}}"

[snippets.pre]
setup = "echo setup"

[snippets.post]
finish = "echo finish"

[profiles.sample]
provider = "demo"
pre = ["setup"]
post = ["finish"]
wrap = "wrap"
"#,
        sessions = toml_path(sessions_dir.path())
    );
    std::fs::write(config_dir.child("config.toml").path(), config_toml)?;

    let mut cmd = base_command(&temp);
    cmd.env("TX_CONFIG_DIR", config_dir.path())
        .args(["config", "list"])
        .assert()
        .success()
        .stdout(contains("demo (bin: echo"))
        .stdout(contains("wrap (shell)"))
        .stdout(contains("sample (provider: demo"));

    temp.close()?;
    Ok(())
}

#[test]
fn self_update_help_is_available() -> color_eyre::Result<()> {
    let temp = TempDir::new()?;
    let mut cmd = base_command(&temp);
    cmd.args(["self-update", "--help"])
        .assert()
        .success()
        .stdout(contains("Usage: tx self-update"))
        .stdout(contains("--version <TAG>"));
    temp.close()?;
    Ok(())
}

#[test]
fn stats_codex_outputs_summary() -> color_eyre::Result<()> {
    let temp = TempDir::new()?;
    let mut cmd = base_command(&temp);
    cmd.args(["stats", "codex"])
        .assert()
        .success()
        .stdout(contains("Codex Stats"))
        .stdout(contains("Sessions:"));
    temp.close()?;
    Ok(())
}

#[test]
fn export_errors_when_session_missing() -> color_eyre::Result<()> {
    let temp = TempDir::new()?;
    let mut cmd = base_command(&temp);
    cmd.args(["export", "missing-session"])
        .assert()
        .failure()
        .stderr(contains("not found"));
    temp.close()?;
    Ok(())
}

#[test]
fn doctor_reports_success() -> color_eyre::Result<()> {
    let temp = TempDir::new()?;
    let config_dir = temp.child("config-root");
    config_dir.create_dir_all()?;
    let sessions_dir = config_dir.child("sessions");
    sessions_dir.create_dir_all()?;
    let config_toml = format!(
        r#"
provider = "demo"

[providers.demo]
bin = "echo"
session_roots = ["{sessions}"]
"#,
        sessions = toml_path(sessions_dir.path())
    );
    std::fs::write(config_dir.child("config.toml").path(), config_toml)?;

    let mut cmd = base_command(&temp);
    cmd.env("TX_CONFIG_DIR", config_dir.path())
        .arg("doctor")
        .assert()
        .success();

    temp.close()?;
    Ok(())
}

#[test]
fn doctor_notes_removed_preview_filter() -> color_eyre::Result<()> {
    let temp = TempDir::new()?;
    let config_dir = temp.child("config-root");
    config_dir.create_dir_all()?;
    let sessions_dir = config_dir.child("sessions");
    sessions_dir.create_dir_all()?;
    let config_toml = format!(
        r#"
preview_filter = "glow"
provider = "demo"

[providers.demo]
bin = "echo"
session_roots = ["{sessions}"]
"#,
        sessions = toml_path(sessions_dir.path())
    );
    std::fs::write(config_dir.child("config.toml").path(), config_toml)?;

    let mut cmd = base_command(&temp);
    cmd.arg("doctor")
        .assert()
        .success()
        .stdout(contains("Ignoring configuration key `preview_filter`"));

    temp.close()?;
    Ok(())
}

#[test]
fn search_returns_results_with_role_filter() -> color_eyre::Result<()> {
    let temp = TempDir::new()?;
    let data_dir = temp.child("data-root");
    data_dir.create_dir_all()?;
    let db_path = data_dir.child("tx.sqlite3");
    let mut db = Database::open(db_path.path())?;
    let now = OffsetDateTime::now_utc().unix_timestamp();
    let summary = SessionSummary {
        id: "sess-1".into(),
        provider: "codex".into(),
        wrapper: None,
        model: None,
        label: Some("Label".into()),
        path: temp.child("sess.jsonl").path().to_path_buf(),
        uuid: Some("uuid-1".into()),
        first_prompt: Some("search term".into()),
        actionable: true,
        created_at: Some(now),
        started_at: Some(now),
        last_active: Some(now),
        size: 1,
        mtime: now,
    };
    let mut message = MessageRecord::new(
        summary.id.clone(),
        0,
        "user",
        "search term",
        None,
        Some(now),
    );
    message.is_first = true;
    let ingest = SessionIngest::new(summary, vec![message]);
    db.upsert_session(&ingest)?;
    drop(db);

    let mut cmd = base_command(&temp);
    let output = cmd
        .env("TX_SKIP_INDEX", "1")
        .args(["search", "--full-text", "--role", "user", "search"])
        .output()?;
    if !output.status.success() {
        eprintln!("stdout: {}", String::from_utf8_lossy(&output.stdout));
        eprintln!("stderr: {}", String::from_utf8_lossy(&output.stderr));
    }
    assert!(output.status.success());
    let parsed: Value = serde_json::from_slice(&output.stdout)?;
    if parsed.as_array().map_or(0, Vec::len) != 1 {
        eprintln!("search stdout: {}", String::from_utf8_lossy(&output.stdout));
    }
    assert_eq!(parsed.as_array().unwrap().len(), 1);
    temp.close()?;
    Ok(())
}

#[test]
fn config_where_reports_directories() -> color_eyre::Result<()> {
    let temp = TempDir::new()?;
    let config_dir = temp.child("config-root");
    config_dir.create_dir_all()?;
    let mut cmd = base_command(&temp);
    cmd.env("TX_CONFIG_DIR", config_dir.path())
        .args(["config", "where"])
        .assert()
        .success()
        .stdout(contains("Configuration directory:"))
        .stdout(contains(config_dir.path().to_string_lossy().as_ref()))
        .stdout(contains("Sources (in load order):"));
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

#[test]
fn resume_accepts_uuid_identifier() -> color_eyre::Result<()> {
    let temp = TempDir::new()?;
    let uuid = "019a1e58-daad-7740-9a01-7a9527114dd9";

    let codex_home = temp.child("codex-home");
    codex_home.create_dir_all()?;
    let session_dir = codex_home.child("session");
    session_dir.create_dir_all()?;
    let payload = format!(
        "{{\"type\":\"event_msg\",\"payload\":{{\"type\":\"user_message\",\"id\":\"{uuid}\",\
         \"text\":\"Ping\"}}}}\n\
         {{\"type\":\"message\",\"payload\":{{\"role\":\"assistant\",\"content\":[{{\"text\":\
         \"Pong\"}}]}}}}\n"
    );
    session_dir.child("resume.jsonl").write_str(&payload)?;

    let mut cmd = base_command(&temp);
    cmd.arg("resume")
        .arg(uuid)
        .arg("--emit-command")
        .assert()
        .success()
        .stdout(contains(uuid));

    temp.close()?;
    Ok(())
}

#[test]
fn resume_last_launches_most_recent_actionable_session() -> color_eyre::Result<()> {
    let temp = TempDir::new()?;
    let config_dir = temp.child("config-root");
    config_dir.create_dir_all()?;
    let codex_home = temp.child("codex-home");
    codex_home.create_dir_all()?;
    let sessions_dir = codex_home.child("session");
    sessions_dir.create_dir_all()?;
    let config_toml = r#"
provider = "codex"

[providers.codex]
bin = "echo"

[wrappers.print]
shell = true
cmd = "echo {{session.id}}"
"#;
    std::fs::write(config_dir.child("config.toml").path(), config_toml)?;

    sessions_dir.child("older.jsonl").write_str(
        r#"{"timestamp":"2025-12-17T12:00:00Z","type":"event_msg","payload":{"type":"user_message","message":"Earlier"}}"#,
    )?;
    sessions_dir.child("non-actionable.jsonl").write_str(
        r#"{"timestamp":"2025-12-18T06:00:00Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"text","text":"assistant only"}]}}"#,
    )?;
    sessions_dir.child("latest.jsonl").write_str(
        r#"{"timestamp":"2025-12-18T05:00:00Z","type":"event_msg","payload":{"type":"user_message","message":"Newest actionable"}}"#,
    )?;

    let mut cmd = base_command(&temp);
    let output = cmd
        .arg("resume")
        .arg("last")
        .arg("--emit-command")
        .arg("--wrap")
        .arg("print")
        .env("CODEX_HOME", codex_home.path())
        .env("TX_CONFIG_DIR", config_dir.path())
        .output()?;
    if !output.status.success() {
        eprintln!("stdout: {}", String::from_utf8_lossy(&output.stdout));
        eprintln!("stderr: {}", String::from_utf8_lossy(&output.stderr));
    }
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout)?;
    assert!(
        stdout.contains("codex/latest.jsonl"),
        "expected latest actionable session id in stdout, got: {stdout}"
    );

    temp.close()?;
    Ok(())
}

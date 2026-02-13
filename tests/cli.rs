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

fn write_codex_session_with_uuid(
    temp: &TempDir,
    file_name: &str,
    uuid: &str,
) -> color_eyre::Result<()> {
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
    session_dir.child(file_name).write_str(&payload)?;
    Ok(())
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
fn db_reset_requires_confirmation() -> color_eyre::Result<()> {
    let temp = TempDir::new()?;
    let mut cmd = base_command(&temp);
    cmd.args(["db", "reset"])
        .assert()
        .failure()
        .stderr(contains("--yes"));

    temp.close()?;
    Ok(())
}

#[test]
fn db_reset_removes_database_files() -> color_eyre::Result<()> {
    let temp = TempDir::new()?;
    let mut cmd = base_command(&temp);
    let data_dir = temp.child("data-root");

    data_dir.child("tx.sqlite3").write_str("stub")?;
    data_dir.child("tx.sqlite3-wal").write_str("wal")?;
    data_dir.child("tx.sqlite3-shm").write_str("shm")?;

    cmd.args(["db", "reset", "--yes"])
        .assert()
        .success()
        .stdout(contains("Deleted database"));

    assert!(!data_dir.child("tx.sqlite3").path().exists());
    assert!(!data_dir.child("tx.sqlite3-wal").path().exists());
    assert!(!data_dir.child("tx.sqlite3-shm").path().exists());

    temp.close()?;
    Ok(())
}

#[test]
fn db_reset_reports_missing_database() -> color_eyre::Result<()> {
    let temp = TempDir::new()?;
    let mut cmd = base_command(&temp);

    cmd.args(["db", "reset", "--yes"])
        .assert()
        .success()
        .stdout(contains("Database not found"));

    temp.close()?;
    Ok(())
}

#[test]
fn db_reset_quiet_suppresses_output() -> color_eyre::Result<()> {
    let temp = TempDir::new()?;
    let mut cmd = base_command(&temp);
    let data_dir = temp.child("data-root");

    data_dir.child("tx.sqlite3").write_str("stub")?;
    data_dir.child("tx.sqlite3-wal").write_str("wal")?;
    data_dir.child("tx.sqlite3-shm").write_str("shm")?;

    cmd.args(["-q", "db", "reset", "--yes"])
        .assert()
        .success()
        .stdout("");

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
fn search_without_term_honors_since_filter() -> color_eyre::Result<()> {
    let temp = TempDir::new()?;
    let data_dir = temp.child("data-root");
    data_dir.create_dir_all()?;
    let db_path = data_dir.child("tx.sqlite3");
    let mut db = Database::open(db_path.path())?;
    let now = OffsetDateTime::now_utc().unix_timestamp();
    let path = temp.child("old.jsonl").path().to_path_buf();
    let summary = SessionSummary {
        id: "sess-old".into(),
        provider: "codex".into(),
        wrapper: None,
        model: None,
        label: Some("Old".into()),
        path,
        uuid: Some("uuid-old".into()),
        first_prompt: Some("old prompt".into()),
        actionable: true,
        created_at: Some(now - 10_000),
        started_at: Some(now - 10_000),
        last_active: Some(now - 10_000),
        size: 1,
        mtime: now - 10_000,
    };
    let mut message = MessageRecord::new(
        summary.id.clone(),
        0,
        "user",
        "old prompt",
        None,
        Some(now - 10_000),
    );
    message.is_first = true;
    db.upsert_session(&SessionIngest::new(summary, vec![message]))?;
    drop(db);

    let mut cmd = base_command(&temp);
    let output = cmd
        .env("TX_SKIP_INDEX", "1")
        .args(["search", "--since", "1s"])
        .output()?;
    assert!(output.status.success());
    let parsed: Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(parsed, json!([]));
    temp.close()?;
    Ok(())
}

#[test]
fn search_without_term_lists_sessions() -> color_eyre::Result<()> {
    let temp = TempDir::new()?;
    let data_dir = temp.child("data-root");
    data_dir.create_dir_all()?;
    let db_path = data_dir.child("tx.sqlite3");
    let mut db = Database::open(db_path.path())?;
    let now = OffsetDateTime::now_utc().unix_timestamp();
    let path = temp.child("recent.jsonl").path().to_path_buf();
    let summary = SessionSummary {
        id: "sess-recent".into(),
        provider: "codex".into(),
        wrapper: None,
        model: None,
        label: Some("Recent".into()),
        path,
        uuid: Some("uuid-recent".into()),
        first_prompt: Some("recent prompt".into()),
        actionable: true,
        created_at: Some(now - 10),
        started_at: Some(now - 10),
        last_active: Some(now - 5),
        size: 1,
        mtime: now - 5,
    };
    let mut message = MessageRecord::new(
        summary.id.clone(),
        0,
        "user",
        "recent prompt",
        None,
        Some(now - 10),
    );
    message.is_first = true;
    db.upsert_session(&SessionIngest::new(summary, vec![message]))?;
    drop(db);

    let mut cmd = base_command(&temp);
    let output = cmd
        .env("TX_SKIP_INDEX", "1")
        .args(["search", "--limit", "5"])
        .output()?;
    assert!(output.status.success());
    let parsed: Value = serde_json::from_slice(&output.stdout)?;
    let list = parsed.as_array().expect("json array");
    assert_eq!(list.len(), 1);
    assert_eq!(
        list[0].get("id").and_then(Value::as_str),
        Some("sess-recent")
    );
    temp.close()?;
    Ok(())
}

#[test]
fn db_reset_fails_with_invalid_config() -> color_eyre::Result<()> {
    let temp = TempDir::new()?;
    let config_dir = temp.child("config-root");
    config_dir.create_dir_all()?;
    config_dir.child("config.toml").write_str("provider =")?;

    let mut cmd = base_command(&temp);
    cmd.env("TX_CONFIG_DIR", config_dir.path())
        .args(["db", "reset", "--yes"])
        .assert()
        .failure()
        .stderr(contains("parse").or(contains("expected")));

    temp.close()?;
    Ok(())
}

#[test]
fn db_reset_surfaces_remove_errors() -> color_eyre::Result<()> {
    let temp = TempDir::new()?;
    let data_dir = temp.child("data-root");
    data_dir.create_dir_all()?;
    data_dir.child("tx.sqlite3").create_dir_all()?;

    let mut cmd = base_command(&temp);
    cmd.args(["db", "reset", "--yes"])
        .assert()
        .failure()
        .stderr(contains("failed to remove"));

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
fn rag_index_requires_openai_api_key() -> color_eyre::Result<()> {
    let temp = TempDir::new()?;
    let mut cmd = base_command(&temp);
    cmd.env_remove("OPENAI_API_KEY")
        .env("TX_SKIP_INDEX", "1")
        .args(["rag", "index", "--batch-size", "1"])
        .assert()
        .failure()
        .stderr(contains("OPENAI_API_KEY"));
    temp.close()?;
    Ok(())
}

#[test]
fn rag_search_requires_openai_api_key() -> color_eyre::Result<()> {
    let temp = TempDir::new()?;
    let mut cmd = base_command(&temp);
    cmd.env_remove("OPENAI_API_KEY")
        .env("TX_SKIP_INDEX", "1")
        .args(["rag", "search", "--query", "find retries", "--k", "5"])
        .assert()
        .failure()
        .stderr(contains("OPENAI_API_KEY"));
    temp.close()?;
    Ok(())
}

#[test]
fn resume_accepts_uuid_identifier() -> color_eyre::Result<()> {
    let temp = TempDir::new()?;
    let uuid = "019a1e58-daad-7740-9a01-7a9527114dd9";
    write_codex_session_with_uuid(&temp, "resume.jsonl", uuid)?;

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
fn resume_emit_json_requires_dry_run_or_emit_command() -> color_eyre::Result<()> {
    let temp = TempDir::new()?;
    let uuid = "019a1e58-daad-7740-9a01-7a9527114dd9";
    write_codex_session_with_uuid(&temp, "resume-emit-json-error.jsonl", uuid)?;

    let mut cmd = base_command(&temp);
    cmd.arg("resume")
        .arg(uuid)
        .arg("--emit-json")
        .assert()
        .failure()
        .stderr(contains("--emit-json requires --dry-run or --emit-command"));

    temp.close()?;
    Ok(())
}

#[test]
fn resume_dry_run_emit_json_outputs_payload() -> color_eyre::Result<()> {
    let temp = TempDir::new()?;
    let uuid = "019a1e58-daad-7740-9a01-7a9527114dd9";
    write_codex_session_with_uuid(&temp, "resume-emit-json-ok.jsonl", uuid)?;

    let mut cmd = base_command(&temp);
    let output = cmd
        .arg("resume")
        .arg(uuid)
        .arg("--dry-run")
        .arg("--emit-json")
        .output()?;
    assert!(output.status.success());
    let parsed: Value = serde_json::from_slice(&output.stdout)?;
    assert!(parsed.get("command").is_some());
    assert!(parsed.get("env").is_some());

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

#[test]
fn resume_executes_pipeline_shell_wrapper_success() -> color_eyre::Result<()> {
    let temp = TempDir::new()?;
    let uuid = "019a1e58-daad-7740-9a01-7a9527114dd9";
    write_codex_session_with_uuid(&temp, "resume-run-success.jsonl", uuid)?;

    let config_dir = temp.child("config-root");
    config_dir.create_dir_all()?;
    let config_toml = r#"
provider = "codex"

[providers.codex]
bin = "echo"
flags = []

[wrappers.ok]
shell = true
cmd = "true"
"#;
    std::fs::write(config_dir.child("config.toml").path(), config_toml)?;

    let mut cmd = base_command(&temp);
    cmd.env("TX_CONFIG_DIR", config_dir.path())
        .arg("resume")
        .arg(uuid)
        .arg("--wrap")
        .arg("ok")
        .assert()
        .success();

    temp.close()?;
    Ok(())
}

#[test]
fn resume_executes_pipeline_shell_wrapper_failure() -> color_eyre::Result<()> {
    let temp = TempDir::new()?;
    let uuid = "019a1e58-daad-7740-9a01-7a9527114dd9";
    write_codex_session_with_uuid(&temp, "resume-run-fail.jsonl", uuid)?;

    let config_dir = temp.child("config-root");
    config_dir.create_dir_all()?;
    let config_toml = r#"
provider = "codex"

[providers.codex]
bin = "echo"
flags = []

[wrappers.fail]
shell = true
cmd = "false"
"#;
    std::fs::write(config_dir.child("config.toml").path(), config_toml)?;

    let mut cmd = base_command(&temp);
    cmd.env("TX_CONFIG_DIR", config_dir.path())
        .arg("resume")
        .arg(uuid)
        .arg("--wrap")
        .arg("fail")
        .assert()
        .failure()
        .stderr(contains("command exited with status"));

    temp.close()?;
    Ok(())
}

#[test]
fn resume_rejects_unknown_profile() -> color_eyre::Result<()> {
    let temp = TempDir::new()?;
    let uuid = "019a1e58-daad-7740-9a01-7a9527114dd9";
    write_codex_session_with_uuid(&temp, "resume-unknown-profile.jsonl", uuid)?;

    let config_dir = temp.child("config-root");
    config_dir.create_dir_all()?;
    let config_toml = r#"
provider = "codex"

[providers.codex]
bin = "echo"
flags = []
"#;
    std::fs::write(config_dir.child("config.toml").path(), config_toml)?;

    let mut cmd = base_command(&temp);
    cmd.env("TX_CONFIG_DIR", config_dir.path())
        .arg("resume")
        .arg(uuid)
        .arg("--profile")
        .arg("missing")
        .assert()
        .failure()
        .stderr(contains("profile 'missing' not found"));

    temp.close()?;
    Ok(())
}

#[test]
fn resume_last_errors_when_no_actionable_sessions_exist() -> color_eyre::Result<()> {
    let temp = TempDir::new()?;
    let config_dir = temp.child("config-root");
    config_dir.create_dir_all()?;
    let codex_home = temp.child("codex-home");
    codex_home.create_dir_all()?;
    let sessions_dir = codex_home.child("session");
    sessions_dir.create_dir_all()?;
    std::fs::write(
        config_dir.child("config.toml").path(),
        "provider = \"codex\"\n[providers.codex]\nbin = \"echo\"\n",
    )?;

    sessions_dir.child("assistant-only.jsonl").write_str(
        r#"{"timestamp":"2025-12-18T06:00:00Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"text","text":"assistant only"}]}}"#,
    )?;

    let mut cmd = base_command(&temp);
    cmd.env("CODEX_HOME", codex_home.path())
        .env("TX_CONFIG_DIR", config_dir.path())
        .arg("resume")
        .arg("last")
        .assert()
        .failure()
        .stderr(contains("no previous sessions available to resume"));

    temp.close()?;
    Ok(())
}

#[test]
fn resume_executes_pipeline_exec_invocation_success() -> color_eyre::Result<()> {
    let temp = TempDir::new()?;
    let uuid = "019a1e58-daad-7740-9a01-7a9527114dd9";
    write_codex_session_with_uuid(&temp, "resume-run-exec-success.jsonl", uuid)?;

    let config_dir = temp.child("config-root");
    config_dir.create_dir_all()?;
    let config_toml = r#"
provider = "codex"

[providers.codex]
bin = "echo"
flags = []
"#;
    std::fs::write(config_dir.child("config.toml").path(), config_toml)?;

    let mut cmd = base_command(&temp);
    cmd.env("TX_CONFIG_DIR", config_dir.path())
        .arg("resume")
        .arg(uuid)
        .assert()
        .success();

    temp.close()?;
    Ok(())
}

#[test]
fn resume_executes_pipeline_exec_invocation_failure() -> color_eyre::Result<()> {
    let temp = TempDir::new()?;
    let uuid = "019a1e58-daad-7740-9a01-7a9527114dd9";
    write_codex_session_with_uuid(&temp, "resume-run-exec-fail.jsonl", uuid)?;

    let config_dir = temp.child("config-root");
    config_dir.create_dir_all()?;
    let config_toml = r#"
provider = "codex"

[providers.codex]
bin = "false"
flags = []
"#;
    std::fs::write(config_dir.child("config.toml").path(), config_toml)?;

    let mut cmd = base_command(&temp);
    cmd.env("TX_CONFIG_DIR", config_dir.path())
        .arg("resume")
        .arg(uuid)
        .assert()
        .failure()
        .stderr(contains("command exited with status"));

    temp.close()?;
    Ok(())
}

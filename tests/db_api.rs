use assert_fs::TempDir;
use assert_fs::prelude::*;
use rusqlite::Connection;
use time::OffsetDateTime;
use tool_executor::db::Database;
use tool_executor::session::{MessageRecord, SessionIngest, SessionSummary, TokenUsageRecord};

fn make_summary(
    id: &str,
    uuid: &str,
    path: std::path::PathBuf,
    actionable: bool,
    now: i64,
) -> SessionSummary {
    SessionSummary {
        id: id.to_string(),
        provider: "codex".to_string(),
        wrapper: None,
        model: None,
        label: Some(id.to_string()),
        path,
        uuid: Some(uuid.to_string()),
        first_prompt: Some(format!("{id} prompt")),
        actionable,
        created_at: Some(now),
        started_at: Some(now),
        last_active: Some(now),
        size: 1,
        mtime: now,
    }
}

#[test]
fn database_public_api_roundtrip() -> color_eyre::Result<()> {
    let temp = TempDir::new()?;
    let db_path = temp.child("tx.sqlite3");
    let mut db = Database::open(db_path.path())?;
    let now = OffsetDateTime::now_utc().unix_timestamp();

    let session1_path = temp.child("session-1.jsonl");
    session1_path.touch()?;
    let summary1 = make_summary(
        "sess-1",
        "uuid-1",
        session1_path.path().to_path_buf(),
        true,
        now - 5,
    );
    let mut msg1 = MessageRecord::new(
        summary1.id.clone(),
        0,
        "user",
        "session one prompt",
        None,
        Some(now - 5),
    );
    msg1.is_first = true;
    let usage1 = TokenUsageRecord {
        session_id: summary1.id.clone(),
        timestamp: now - 5,
        input_tokens: 100,
        cached_input_tokens: 10,
        output_tokens: 50,
        reasoning_output_tokens: 0,
        total_tokens: 150,
        model: Some("gpt-5.2".to_string()),
        rate_limits: None,
    };
    db.upsert_session(
        &SessionIngest::new(summary1.clone(), vec![msg1]).with_token_usage(vec![usage1]),
    )?;

    let session2_path = temp.child("session-2.jsonl");
    session2_path.touch()?;
    let summary2 = make_summary(
        "sess-2",
        "uuid-2",
        session2_path.path().to_path_buf(),
        false,
        now - 100,
    );
    let mut msg2 = MessageRecord::new(
        summary2.id.clone(),
        0,
        "user",
        "session two prompt",
        None,
        Some(now - 100),
    );
    msg2.is_first = true;
    let usage2 = TokenUsageRecord {
        session_id: summary2.id.clone(),
        timestamp: now - 100,
        input_tokens: 10,
        cached_input_tokens: 0,
        output_tokens: 5,
        reasoning_output_tokens: 0,
        total_tokens: 15,
        model: Some("gpt-5.2".to_string()),
        rate_limits: None,
    };
    db.upsert_session(
        &SessionIngest::new(summary2.clone(), vec![msg2]).with_token_usage(vec![usage2]),
    )?;

    assert_eq!(db.count_sessions()?, 2);
    assert!(
        db.existing_by_path(summary1.path.to_string_lossy().as_ref())?
            .is_some()
    );
    assert_eq!(db.provider_for("sess-1")?.as_deref(), Some("codex"));
    assert_eq!(db.sessions_for_provider("codex")?.len(), 2);
    assert_eq!(db.token_usage_for_provider("codex")?.len(), 2);
    assert_eq!(db.user_message_timestamps("codex")?.len(), 2);

    let by_uuid = db
        .session_summary_for_identifier("uuid-1")?
        .expect("summary by uuid");
    assert_eq!(by_uuid.id, "sess-1");

    let transcript = db.fetch_transcript("uuid-1")?.expect("transcript by uuid");
    assert_eq!(transcript.session.id, "sess-1");
    assert_eq!(transcript.messages.len(), 1);

    let actionable = db.list_sessions(Some("codex"), true, None, Some(10))?;
    assert_eq!(actionable.len(), 1);
    assert_eq!(actionable[0].id, "sess-1");

    let first_prompt_hits = db.search_first_prompt("prompt", Some("codex"), false)?;
    assert!(!first_prompt_hits.is_empty());

    let full_text_hits = db.search_full_text("prompt", Some("codex"), false)?;
    assert!(!full_text_hits.is_empty());

    let latest = db
        .latest_actionable_session()?
        .expect("latest actionable session");
    assert_eq!(latest.id, "sess-1");

    db.delete_session("sess-1")?;
    assert_eq!(db.count_sessions()?, 1);

    temp.close()?;
    Ok(())
}

#[test]
fn database_open_reports_error_for_directory_path() -> color_eyre::Result<()> {
    let temp = TempDir::new()?;
    let Err(err) = Database::open(temp.path()) else {
        panic!("opening a directory should fail");
    };
    assert!(format!("{err:#}").contains("failed to open database"));
    temp.close()?;
    Ok(())
}

#[test]
fn database_methods_surface_query_errors_when_schema_is_missing() -> color_eyre::Result<()> {
    let temp = TempDir::new()?;
    let db_path = temp.child("broken.sqlite3");
    let db_path_buf = db_path.path().to_path_buf();

    let db = Database::open(&db_path_buf)?;
    drop(db);

    let conn = Connection::open(&db_path_buf)?;
    conn.execute("DROP TABLE sessions", [])?;
    drop(conn);

    let db = Database::open(&db_path_buf)?;
    assert!(db.existing_by_path("/tmp/demo.jsonl").is_err());
    assert!(db.delete_session("sess-1").is_err());
    assert!(db.count_sessions().is_err());
    assert!(db.session_summary("sess-1").is_err());
    assert!(db.session_summary_for_identifier("uuid-1").is_err());
    assert!(db.latest_actionable_session().is_err());
    assert!(db.provider_for("sess-1").is_err());

    temp.close()?;
    Ok(())
}

use assert_fs::TempDir;
use assert_fs::prelude::*;
use time::OffsetDateTime;
use tool_executor::commands::stats;
use tool_executor::db::Database;
use tool_executor::session::{MessageRecord, SessionIngest, SessionSummary, TokenUsageRecord};

#[test]
fn codex_stats_renders_with_priced_and_unpriced_usage() -> color_eyre::Result<()> {
    let temp = TempDir::new()?;
    let db_path = temp.child("tx.sqlite3");
    let mut db = Database::open(db_path.path())?;
    let now = OffsetDateTime::now_utc().unix_timestamp();

    let summary = SessionSummary {
        id: "codex/demo".into(),
        provider: "codex".into(),
        wrapper: None,
        model: Some("gpt-5.2".into()),
        label: Some("Demo".into()),
        path: temp.child("session.jsonl").path().to_path_buf(),
        uuid: Some("demo-uuid".into()),
        first_prompt: Some("Hello".into()),
        actionable: true,
        created_at: Some(now - 120),
        started_at: Some(now - 120),
        last_active: Some(now),
        size: 1,
        mtime: now,
    };
    let mut message = MessageRecord::new(
        summary.id.clone(),
        0,
        "user",
        "Hello",
        None,
        Some(now - 120),
    );
    message.is_first = true;

    let usage_priced = TokenUsageRecord {
        session_id: summary.id.clone(),
        timestamp: now - 60,
        input_tokens: 1000,
        cached_input_tokens: 100,
        output_tokens: 500,
        reasoning_output_tokens: 0,
        total_tokens: 1500,
        model: Some("gpt-5.2".into()),
        rate_limits: Some(
            r#"{"primary":{"used_percent":20,"window_minutes":60,"resets_at":1700000000000},"credits":{"has_credits":true,"unlimited":false,"balance":7},"plan_type":"pro"}"#.into(),
        ),
    };
    let usage_unpriced = TokenUsageRecord {
        session_id: summary.id.clone(),
        timestamp: now - 30,
        input_tokens: 10,
        cached_input_tokens: 0,
        output_tokens: 5,
        reasoning_output_tokens: 0,
        total_tokens: 15,
        model: Some("unknown-model".into()),
        rate_limits: None,
    };
    let ingest = SessionIngest::new(summary, vec![message])
        .with_token_usage(vec![usage_priced, usage_unpriced]);
    db.upsert_session(&ingest)?;

    stats::codex(&db, db_path.path())?;
    drop(db);
    temp.close()?;
    Ok(())
}

#[test]
fn codex_stats_handles_invalid_timestamps_and_multiple_priced_models() -> color_eyre::Result<()> {
    let temp = TempDir::new()?;
    let db_path = temp.child("tx-invalid.sqlite3");
    let mut db = Database::open(db_path.path())?;
    let now = OffsetDateTime::now_utc().unix_timestamp();

    let summary = SessionSummary {
        id: "codex/invalid".into(),
        provider: "codex".into(),
        wrapper: None,
        model: Some("gpt-5".into()),
        label: Some("Invalid".into()),
        path: temp.child("invalid-session.jsonl").path().to_path_buf(),
        uuid: Some("invalid-uuid".into()),
        first_prompt: Some("Hello".into()),
        actionable: true,
        created_at: Some(i64::MAX),
        started_at: Some(i64::MAX),
        last_active: Some(i64::MAX),
        size: 1,
        mtime: now,
    };

    let mut message = MessageRecord::new(summary.id.clone(), 0, "user", "Hello", None, Some(now));
    message.is_first = true;

    let usage_primary = TokenUsageRecord {
        session_id: summary.id.clone(),
        timestamp: now,
        input_tokens: 250,
        cached_input_tokens: 25,
        output_tokens: 125,
        reasoning_output_tokens: 0,
        total_tokens: 375,
        model: Some("gpt-5".into()),
        rate_limits: None,
    };
    let usage_secondary = TokenUsageRecord {
        session_id: summary.id.clone(),
        timestamp: i64::MAX,
        input_tokens: 100,
        cached_input_tokens: 10,
        output_tokens: 40,
        reasoning_output_tokens: 0,
        total_tokens: 140,
        model: Some("gpt-5.2".into()),
        rate_limits: None,
    };

    db.upsert_session(
        &SessionIngest::new(summary, vec![message])
            .with_token_usage(vec![usage_primary, usage_secondary]),
    )?;

    stats::codex(&db, db_path.path())?;
    drop(db);
    temp.close()?;
    Ok(())
}

use time::OffsetDateTime;
use tool_executor::session::{
    MessageRecord, SessionSummary, Transcript, fallback_session_uuid, session_uuid_from_value,
};

#[test]
fn session_summary_has_path_matches() {
    let summary = SessionSummary {
        id: "demo".into(),
        provider: "codex".into(),
        wrapper: None,
        model: None,
        label: None,
        path: std::path::PathBuf::from("/tmp/demo.jsonl"),
        uuid: None,
        first_prompt: None,
        actionable: true,
        created_at: None,
        started_at: None,
        last_active: None,
        size: 1,
        mtime: 1,
    };
    assert!(summary.has_path("/tmp/demo.jsonl"));
    assert!(!summary.has_path("/tmp/other.jsonl"));
}

#[test]
fn transcript_markdown_captures_user_and_assistant_roles() {
    let now = OffsetDateTime::now_utc().unix_timestamp();
    let mut user = MessageRecord::new("demo", 0, "user", "First", None, Some(now));
    user.is_first = true;
    let assistant = MessageRecord::new("demo", 1, "assistant", "Second", None, Some(now));

    let transcript = Transcript {
        session: SessionSummary {
            id: "demo".into(),
            provider: "codex".into(),
            wrapper: None,
            model: None,
            label: None,
            path: std::path::PathBuf::from("/tmp/demo.jsonl"),
            uuid: None,
            first_prompt: None,
            actionable: true,
            created_at: None,
            started_at: None,
            last_active: Some(now),
            size: 1,
            mtime: 1,
        },
        messages: vec![user, assistant],
    };

    let lines = transcript.markdown_lines(None);
    assert!(lines.iter().any(|line| line.contains("— User")));
    assert!(lines.iter().any(|line| line.contains("— Assistant")));
}

#[test]
fn session_summary_is_stale_when_size_or_mtime_changes() {
    let summary = SessionSummary {
        id: "demo".into(),
        provider: "codex".into(),
        wrapper: None,
        model: None,
        label: None,
        path: std::path::PathBuf::from("/tmp/demo.jsonl"),
        uuid: None,
        first_prompt: None,
        actionable: true,
        created_at: None,
        started_at: None,
        last_active: None,
        size: 10,
        mtime: 20,
    };

    assert!(!summary.is_stale(10, 20));
    assert!(summary.is_stale(11, 20));
    assert!(summary.is_stale(10, 21));
}

#[test]
fn session_uuid_helpers_cover_payload_session_and_path_fallback() {
    let payload_value: serde_json::Value = serde_json::json!({
        "payload": { "id": "from-payload" }
    });
    assert_eq!(
        session_uuid_from_value(&payload_value).as_deref(),
        Some("from-payload")
    );

    let session_value: serde_json::Value = serde_json::json!({
        "session": { "id": "from-session" }
    });
    assert_eq!(
        session_uuid_from_value(&session_value).as_deref(),
        Some("from-session")
    );

    let fallback = fallback_session_uuid(std::path::Path::new(
        "/tmp/rollout-something-uuid-123.jsonl",
    ));
    assert_eq!(fallback.as_deref(), Some("123"));
}

#[test]
fn session_uuid_helpers_cover_root_path_and_markdown_invalid_timestamp() {
    assert_eq!(fallback_session_uuid(std::path::Path::new("/")), None);

    let transcript = Transcript {
        session: SessionSummary {
            id: "demo".into(),
            provider: "codex".into(),
            wrapper: None,
            model: None,
            label: None,
            path: std::path::PathBuf::from("/tmp/demo.jsonl"),
            uuid: None,
            first_prompt: None,
            actionable: true,
            created_at: None,
            started_at: None,
            last_active: None,
            size: 1,
            mtime: 1,
        },
        messages: vec![MessageRecord::new(
            "demo",
            0,
            "assistant",
            "hi",
            None,
            Some(i64::MAX),
        )],
    };
    let lines = transcript.markdown_lines(None);
    assert!(lines.iter().any(|line| line.contains("## - — Assistant")));
}

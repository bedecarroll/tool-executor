use time::OffsetDateTime;
use tool_executor::session::{MessageRecord, SessionSummary, Transcript};

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

use std::path::{Path, PathBuf};

use serde_json::Value;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use time::macros::format_description;

#[derive(Debug, Clone)]
pub struct SessionSummary {
    pub id: String,
    pub provider: String,
    pub wrapper: Option<String>,
    pub model: Option<String>,
    pub label: Option<String>,
    pub path: PathBuf,
    pub uuid: Option<String>,
    pub first_prompt: Option<String>,
    pub actionable: bool,
    pub created_at: Option<i64>,
    pub started_at: Option<i64>,
    pub last_active: Option<i64>,
    pub size: i64,
    pub mtime: i64,
}

#[derive(Debug, Clone)]
pub struct SessionIngest {
    pub summary: SessionSummary,
    pub messages: Vec<MessageRecord>,
}

#[derive(Debug, Clone)]
pub struct MessageRecord {
    pub session_id: String,
    pub index: i64,
    pub role: String,
    pub content: String,
    pub source: Option<String>,
    pub timestamp: Option<i64>,
    pub is_first: bool,
}

#[derive(Debug, Clone)]
pub struct SessionQuery {
    pub id: String,
    pub provider: String,
    pub wrapper: Option<String>,
    pub label: Option<String>,
    pub first_prompt: Option<String>,
    pub actionable: bool,
    pub last_active: Option<i64>,
}

impl SessionSummary {
    #[must_use]
    pub fn is_stale(&self, size: i64, mtime: i64) -> bool {
        self.size != size || self.mtime != mtime
    }

    pub fn has_path<P: AsRef<Path>>(&self, path: P) -> bool {
        self.path == path.as_ref()
    }
}

#[derive(Debug, Clone)]
pub struct SearchHit {
    pub session_id: String,
    pub provider: String,
    pub wrapper: Option<String>,
    pub label: Option<String>,
    pub role: Option<String>,
    pub snippet: Option<String>,
    pub last_active: Option<i64>,
    pub actionable: bool,
}

#[derive(Debug, Clone)]
pub struct Transcript {
    pub session: SessionSummary,
    pub messages: Vec<MessageRecord>,
}

impl Transcript {
    #[must_use]
    pub fn markdown_lines(&self, limit: Option<usize>) -> Vec<String> {
        let mut lines = Vec::new();
        let header_id = self.session.uuid.as_deref().unwrap_or(&self.session.id);
        lines.push(format!("# Codex Session {header_id}"));
        if let Some(wrapper) = self.session.wrapper.as_deref() {
            lines.push(format!("**Wrapper**: `{wrapper}`"));
        }
        lines.push(String::new());

        let mut emitted = 0usize;
        let mut total_renderable = 0usize;

        for message in &self.messages {
            let role_lower = message.role.to_ascii_lowercase();
            if role_lower != "user" && role_lower != "assistant" {
                continue;
            }

            let text = message.content.trim_start();
            let trimmed = text.trim();
            if trimmed.is_empty()
                || trimmed.starts_with("<user_instructions>")
                || trimmed.starts_with("<environment_context>")
            {
                continue;
            }

            total_renderable += 1;

            if limit.is_some_and(|max| emitted >= max) {
                continue;
            }

            let role_title = if role_lower == "user" {
                "User"
            } else {
                "Assistant"
            };

            let timestamp = message
                .timestamp
                .and_then(|ts| OffsetDateTime::from_unix_timestamp(ts).ok())
                .and_then(|dt| {
                    dt.format(&Rfc3339)
                        .or_else(|_| dt.format(&format_description!("%Y-%m-%d %H:%M:%S")))
                        .ok()
                })
                .unwrap_or_else(|| "-".to_string());

            lines.push(format!("## {timestamp} — {role_title}"));
            lines.push(String::new());
            lines.extend(text.lines().map(str::to_string));
            lines.push(String::new());

            emitted += 1;
        }

        if let Some(limit) = limit
            && total_renderable > limit
        {
            lines.push(format!(
                "*… and {} more messages*",
                total_renderable - limit
            ));
            lines.push(String::new());
        }

        lines
    }
}

impl SessionIngest {
    #[must_use]
    pub fn new(summary: SessionSummary, messages: Vec<MessageRecord>) -> Self {
        Self { summary, messages }
    }
}

impl MessageRecord {
    pub fn new(
        session_id: impl Into<String>,
        index: i64,
        role: impl Into<String>,
        content: impl Into<String>,
        source: Option<String>,
        timestamp: Option<i64>,
    ) -> Self {
        Self {
            session_id: session_id.into(),
            index,
            role: role.into(),
            content: content.into(),
            source,
            timestamp,
            is_first: false,
        }
    }
}

/// Attempt to extract a session UUID from a parsed Codex log entry.
#[must_use]
pub fn session_uuid_from_value(value: &Value) -> Option<String> {
    let map = value.as_object()?;

    if let Some(payload) = map.get("payload")
        && let Some(uuid) = payload
            .get("id")
            .and_then(Value::as_str)
            .or_else(|| payload.get("session_id").and_then(Value::as_str))
    {
        return Some(uuid.to_string());
    }

    if let Some(session) = map.get("session")
        && let Some(uuid) = session.get("id").and_then(Value::as_str)
    {
        return Some(uuid.to_string());
    }

    map.get("id").and_then(Value::as_str).map(str::to_string)
}

/// Derive a fallback session UUID from the log file path when the payload does not expose one.
#[must_use]
pub fn fallback_session_uuid(path: &Path) -> Option<String> {
    let file_name = path.file_name()?.to_str()?;
    let trimmed = file_name.strip_suffix(".jsonl").unwrap_or(file_name);

    if let Some(stripped) = trimmed.strip_prefix("rollout-") {
        if let Some((_, suffix)) = stripped.rsplit_once('-') {
            return Some(suffix.to_string());
        }
        return Some(stripped.to_string());
    }

    Some(trimmed.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_summary() -> SessionSummary {
        SessionSummary {
            id: "sample".into(),
            provider: "codex".into(),
            wrapper: None,
            model: None,
            label: Some("demo".into()),
            path: PathBuf::from("/tmp/sample.jsonl"),
            uuid: Some("abc123".into()),
            first_prompt: Some("Hello world".into()),
            actionable: true,
            created_at: Some(1),
            started_at: Some(2),
            last_active: Some(3),
            size: 1024,
            mtime: 4,
        }
    }

    #[test]
    fn transcript_markdown_limits_output() {
        let mut messages = Vec::new();
        for (idx, (role, content)) in [
            ("user", "First message"),
            ("assistant", "Response"),
            ("system", "Skip me"),
            (
                "assistant",
                "<user_instructions>ignored</user_instructions>",
            ),
        ]
        .into_iter()
        .enumerate()
        {
            let mut record = MessageRecord::new(
                "sample",
                i64::try_from(idx).expect("message index within i64 range"),
                role,
                content,
                None,
                Some(5),
            );
            if idx == 0 {
                record.is_first = true;
            }
            messages.push(record);
        }
        let transcript = Transcript {
            session: sample_summary(),
            messages,
        };

        let lines = transcript.markdown_lines(Some(1));
        assert!(lines.iter().any(|line| line.contains("First message")));
        assert!(
            lines
                .iter()
                .any(|line| line.contains("… and 1 more messages"))
        );
        assert!(lines.iter().all(|line| !line.contains("Skip me")));
    }

    #[test]
    fn transcript_markdown_includes_assistant_roles_without_limit() {
        let mut messages = Vec::new();
        for (idx, (role, content)) in [("user", "Hello"), ("assistant", "Hi there")]
            .into_iter()
            .enumerate()
        {
            let mut record = MessageRecord::new(
                "sample",
                i64::try_from(idx).expect("message index within i64 range"),
                role,
                content,
                None,
                Some(5),
            );
            if idx == 0 {
                record.is_first = true;
            }
            messages.push(record);
        }
        let transcript = Transcript {
            session: sample_summary(),
            messages,
        };

        let lines = transcript.markdown_lines(None);
        assert!(lines.iter().any(|line| line.contains("— User")));
        assert!(lines.iter().any(|line| line.contains("— Assistant")));
    }

    #[test]
    fn session_summary_has_path_matches_exact_path() {
        let summary = sample_summary();
        assert!(summary.has_path("/tmp/sample.jsonl"));
        assert!(!summary.has_path("/tmp/other.jsonl"));
    }

    #[test]
    fn session_uuid_from_value_falls_back_to_session_key() {
        let value: Value =
            serde_json::json!({"session": {"id": "session-1"}, "payload": {"type": "noop"}});
        assert_eq!(session_uuid_from_value(&value), Some("session-1".into()));
    }

    #[test]
    fn session_uuid_from_value_uses_payload_id() {
        let value: Value = serde_json::json!({"payload": {"id": "payload-42"}});
        assert_eq!(session_uuid_from_value(&value), Some("payload-42".into()));
    }

    #[test]
    fn fallback_session_uuid_extracts_rollout_suffix() {
        let path = Path::new("/tmp/rollout-2024-10-26-abcdef.jsonl");
        assert_eq!(fallback_session_uuid(path), Some("abcdef".to_string()));
    }

    #[test]
    fn fallback_session_uuid_handles_rollout_without_suffix() {
        let path = Path::new("/tmp/rollout-log.jsonl");
        assert_eq!(fallback_session_uuid(path), Some("log".to_string()));
    }

    #[test]
    fn session_summary_staleness_detects_changes() {
        let summary = sample_summary();
        assert!(summary.is_stale(2048, 8));
        assert!(!summary.is_stale(1024, 4));
    }
}

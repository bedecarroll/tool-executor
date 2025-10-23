use std::path::{Path, PathBuf};

use serde_json::Value;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use time::macros::format_description;

#[derive(Debug, Clone)]
pub struct SessionSummary {
    pub id: String,
    pub provider: String,
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

use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct SessionSummary {
    pub id: String,
    pub provider: String,
    pub label: Option<String>,
    pub path: PathBuf,
    pub first_prompt: Option<String>,
    pub actionable: bool,
    pub created_at: Option<i64>,
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
    pub snippet: Option<String>,
    pub last_active: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct Transcript {
    pub session: SessionSummary,
    pub messages: Vec<MessageRecord>,
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
    ) -> Self {
        Self {
            session_id: session_id.into(),
            index,
            role: role.into(),
            content: content.into(),
        }
    }
}

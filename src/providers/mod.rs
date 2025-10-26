pub mod codex;

use crate::session::SessionSummary;
use color_eyre::Result;

#[derive(Debug, Clone)]
pub struct ResumePlan {
    pub args: Vec<String>,
    pub resume_token: Option<String>,
}

/// Derive provider-specific resume arguments for a stored session.
///
/// # Errors
///
/// Returns an error if provider-specific helpers fail to inspect the session.
pub fn resume_info(summary: &SessionSummary) -> Result<Option<ResumePlan>> {
    match summary.provider.as_str() {
        "codex" => codex::resume_info(summary),
        _ => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resume_info_returns_none_for_other_providers() -> Result<()> {
        let summary = SessionSummary {
            id: "sess".into(),
            provider: "other".into(),
            label: None,
            path: "/tmp/other.jsonl".into(),
            uuid: None,
            first_prompt: None,
            actionable: false,
            created_at: None,
            started_at: None,
            last_active: None,
            size: 0,
            mtime: 0,
        };
        assert!(resume_info(&summary)?.is_none());
        Ok(())
    }
}

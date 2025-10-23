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

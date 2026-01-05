use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use color_eyre::Result;
use serde_json::Value;

use crate::providers::ResumePlan;
use crate::session::{SessionSummary, fallback_session_uuid, session_uuid_from_value};

/// Build a Codex resume plan from a stored session summary.
///
/// # Errors
///
/// Returns an error if the session log cannot be opened or parsed.
pub fn resume_info(summary: &SessionSummary) -> Result<Option<ResumePlan>> {
    let uuid = if let Some(uuid) = summary.uuid.clone() {
        Some(uuid)
    } else {
        extract_session_uuid(&summary.path)?
    };
    let Some(uuid) = uuid else {
        return Ok(None);
    };

    let mut args = Vec::new();
    if let Some(model) = summary.model.as_deref() {
        args.push("-m".to_string());
        args.push(model.to_string());
    }
    args.push("resume".to_string());
    args.push(uuid.clone());

    Ok(Some(ResumePlan {
        args,
        resume_token: Some(uuid),
    }))
}

fn extract_session_uuid(path: &Path) -> Result<Option<String>> {
    match File::open(path) {
        Ok(file) => {
            let reader = BufReader::new(file);
            for line_result in reader.lines().take(256) {
                let line = line_result?;
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                if let Ok(value) = serde_json::from_str::<Value>(trimmed)
                    && let Some(uuid) = session_uuid_from_value(&value)
                {
                    return Ok(Some(uuid));
                }
            }
            Ok(fallback_session_uuid(path))
        }
        Err(_) => Ok(fallback_session_uuid(path)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::SessionSummary;
    use std::io::Write;
    use std::path::PathBuf;
    use tempfile::NamedTempFile;

    fn sample_summary() -> SessionSummary {
        SessionSummary {
            id: "sess-1".into(),
            provider: "codex".into(),
            wrapper: None,
            model: None,
            label: Some("demo".into()),
            path: PathBuf::from("/tmp/session.jsonl"),
            uuid: None,
            first_prompt: Some("Hello".into()),
            actionable: true,
            created_at: Some(1),
            started_at: Some(1),
            last_active: Some(1),
            size: 1,
            mtime: 1,
        }
    }

    #[test]
    fn extracts_uuid_from_payload() -> Result<()> {
        let mut file = NamedTempFile::new()?;
        file.write_all(b"{\"payload\": {\"id\": \"abc123\"}, \"type\": \"event_msg\"}\n")?;
        let path = file.into_temp_path();
        let uuid = extract_session_uuid(&path)?.expect("uuid");
        assert_eq!(uuid, "abc123");
        Ok(())
    }

    #[test]
    fn falls_back_to_rollout_suffix() {
        let path = Path::new("/tmp/rollout-2024-07-30T02-42-13-7f6c3c.jsonl");
        let uuid = fallback_session_uuid(path).expect("uuid");
        assert_eq!(uuid, "7f6c3c");
    }

    #[test]
    fn falls_back_to_filename_without_extension() {
        let path = Path::new("/tmp/abcd-ef.jsonl");
        let uuid = fallback_session_uuid(path).expect("uuid");
        assert_eq!(uuid, "abcd-ef");
    }

    #[test]
    fn resume_info_uses_existing_uuid() -> Result<()> {
        let mut summary = sample_summary();
        summary.uuid = Some("known-uuid".into());
        let plan = resume_info(&summary)?.expect("resume plan");
        assert_eq!(plan.args, vec!["resume".to_string(), "known-uuid".into()]);
        assert_eq!(plan.resume_token.as_deref(), Some("known-uuid"));
        Ok(())
    }

    #[test]
    fn resume_info_includes_model_when_available() -> Result<()> {
        let mut summary = sample_summary();
        summary.uuid = Some("known-uuid".into());
        summary.model = Some("o3-mini".into());
        let plan = resume_info(&summary)?.expect("resume plan");
        assert_eq!(
            plan.args,
            vec![
                "-m".to_string(),
                "o3-mini".to_string(),
                "resume".to_string(),
                "known-uuid".to_string()
            ]
        );
        assert_eq!(plan.resume_token.as_deref(), Some("known-uuid"));
        Ok(())
    }

    #[test]
    fn resume_info_falls_back_when_log_missing() -> Result<()> {
        let mut summary = sample_summary();
        summary.path = PathBuf::from("/tmp/fallback-log.jsonl");
        summary.uuid = None;
        let plan = resume_info(&summary)?.expect("resume plan");
        assert_eq!(plan.args, vec!["resume".to_string(), "fallback-log".into()]);
        assert_eq!(plan.resume_token.as_deref(), Some("fallback-log"));
        Ok(())
    }

    #[test]
    fn resume_info_returns_none_when_no_identifier() -> Result<()> {
        let mut summary = sample_summary();
        summary.path = PathBuf::new();
        summary.uuid = None;
        let plan = resume_info(&summary)?;
        assert!(plan.is_none());
        Ok(())
    }
}

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

    Ok(Some(ResumePlan {
        args: vec!["resume".to_string(), uuid.clone()],
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
    use std::io::Write;
    use tempfile::NamedTempFile;

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
}

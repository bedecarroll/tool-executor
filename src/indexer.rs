use std::collections::HashSet;
use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use color_eyre::Result;
use color_eyre::eyre::{self, Context, eyre};
use itertools::Itertools;
use serde_json::Value;
use walkdir::WalkDir;

use crate::config::model::{Config, ProviderConfig};
use crate::db::Database;
use crate::session::{MessageRecord, SessionIngest, SessionSummary};

#[derive(Debug, Default)]
pub struct IndexReport {
    pub scanned: usize,
    pub updated: usize,
    pub skipped: usize,
    pub removed: usize,
    pub errors: Vec<IndexError>,
}

#[derive(Debug)]
pub struct IndexError {
    pub path: PathBuf,
    pub error: eyre::Report,
}

pub struct Indexer<'a> {
    db: &'a mut Database,
    config: &'a Config,
}

impl<'a> Indexer<'a> {
    pub fn new(db: &'a mut Database, config: &'a Config) -> Self {
        Self { db, config }
    }

    /// Re-scan configured session roots and update the index.
    ///
    /// # Errors
    ///
    /// Returns an error if walking the filesystem or updating the database fails.
    pub fn run(&mut self) -> Result<IndexReport> {
        let mut report = IndexReport::default();

        for provider in self.config.providers.values() {
            let mut seen = HashSet::new();
            for root in &provider.session_roots {
                if !root.exists() {
                    tracing::debug!(provider = %provider.name, root = %root.display(), "session root missing");
                    continue;
                }
                if root.is_file() {
                    if is_jsonl(root) {
                        match self.process_file(provider, root) {
                            Ok(Some(id)) => {
                                seen.insert(id);
                                report.updated += 1;
                                report.scanned += 1;
                            }
                            Ok(None) => {
                                report.skipped += 1;
                            }
                            Err(err) => {
                                report.errors.push(IndexError {
                                    path: root.clone(),
                                    error: err,
                                });
                            }
                        }
                    }
                    continue;
                }

                for entry in WalkDir::new(root)
                    .follow_links(true)
                    .into_iter()
                    .filter_map(std::result::Result::ok)
                    .filter(|e| e.file_type().is_file())
                {
                    let path = entry.path();
                    if !is_jsonl(path) {
                        continue;
                    }

                    report.scanned += 1;
                    match self.process_file(provider, path) {
                        Ok(Some(id)) => {
                            seen.insert(id);
                            report.updated += 1;
                        }
                        Ok(None) => {
                            report.skipped += 1;
                        }
                        Err(err) => {
                            report.errors.push(IndexError {
                                path: path.to_path_buf(),
                                error: err,
                            });
                        }
                    }
                }
            }

            // remove stale sessions for provider
            let existing = self.db.sessions_for_provider(&provider.name)?;
            for session in existing {
                if !seen.contains(&session.id) && !session.path.exists() {
                    self.db.delete_session(&session.id)?;
                    report.removed += 1;
                }
            }
        }

        Ok(report)
    }

    fn process_file(&mut self, provider: &ProviderConfig, path: &Path) -> Result<Option<String>> {
        let metadata = fs::metadata(path)
            .with_context(|| format!("failed to read metadata for {}", path.display()))?;
        let size = i64::try_from(metadata.len()).unwrap_or(i64::MAX);
        let mtime = system_time_to_unix(metadata.modified().ok()).unwrap_or_else(current_unix_time);
        let created_at = system_time_to_unix(metadata.created().ok());

        let canonical_path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        let path_str = canonical_path.to_string_lossy().to_string();

        if let Some(existing) = self.db.existing_by_path(&path_str)?
            && !existing.is_stale(size, mtime)
        {
            return Ok(None);
        }

        let ingest = Self::build_ingest(provider, &canonical_path, size, mtime, created_at)
            .with_context(|| format!("failed to ingest session from {}", path.display()))?;
        let id = ingest.summary.id.clone();
        self.db.upsert_session(&ingest)?;
        Ok(Some(id))
    }

    fn build_ingest(
        provider: &ProviderConfig,
        path: &Path,
        size: i64,
        mtime: i64,
        created_at: Option<i64>,
    ) -> Result<SessionIngest> {
        let (session_id, relative) = compute_session_id(provider, path);
        let label = path
            .file_stem()
            .and_then(OsStr::to_str)
            .map(str::to_string)
            .or_else(|| Some(relative.clone()));

        let file = File::open(path)?;
        let reader = BufReader::new(file);
        let mut messages = Vec::new();
        let mut first_prompt: Option<String> = None;

        for line in reader.lines() {
            let line = line?;
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }

            let value: Value = match serde_json::from_str(trimmed) {
                Ok(v) => v,
                Err(err) => {
                    tracing::debug!(path = %path.display(), "skipping invalid json line: {err}");
                    continue;
                }
            };

            for (role, content) in extract_messages(&value) {
                if let Some(clean) = clean_text(&content) {
                    let index = i64::try_from(messages.len()).unwrap_or(i64::MAX);
                    if first_prompt.is_none() && role.eq_ignore_ascii_case("user") {
                        first_prompt = Some(clean.clone());
                    }
                    messages.push(MessageRecord::new(&session_id, index, role, clean));
                }
            }
        }

        if messages.is_empty() {
            return Err(eyre!("no messages discovered in session"));
        }

        let actionable = first_prompt.is_some();
        let summary = SessionSummary {
            id: session_id,
            provider: provider.name.clone(),
            label,
            path: path.to_path_buf(),
            first_prompt,
            actionable,
            created_at,
            last_active: Some(mtime),
            size,
            mtime,
        };

        Ok(SessionIngest::new(summary, messages))
    }
}

fn is_jsonl(path: &Path) -> bool {
    path.extension()
        .and_then(OsStr::to_str)
        .is_some_and(|ext| ext.eq_ignore_ascii_case("jsonl"))
}

fn system_time_to_unix(time: Option<SystemTime>) -> Option<i64> {
    time.and_then(|time| {
        time.duration_since(SystemTime::UNIX_EPOCH)
            .map(|duration| i64::try_from(duration.as_secs()).unwrap_or(i64::MAX))
            .ok()
    })
}

fn current_unix_time() -> i64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|duration| i64::try_from(duration.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

fn compute_session_id(provider: &ProviderConfig, path: &Path) -> (String, String) {
    let mut relative = None;
    for root in &provider.session_roots {
        if let Ok(stripped) = path.strip_prefix(root) {
            relative = Some(stripped.to_path_buf());
            break;
        }
        if let Ok(canon_root) = root.canonicalize()
            && let Ok(stripped) = path.strip_prefix(&canon_root)
        {
            relative = Some(stripped.to_path_buf());
            break;
        }
    }

    let relative = relative.unwrap_or_else(|| {
        path.file_name()
            .map_or_else(|| path.to_path_buf(), PathBuf::from)
    });

    let normalized = relative
        .components()
        .map(|comp| comp.as_os_str().to_string_lossy())
        .join("/");
    let id = format!("{}/{}", provider.name, normalized);
    (id, normalized)
}

fn extract_messages(value: &Value) -> Vec<(String, String)> {
    let mut messages = Vec::new();

    if let Some(obj) = value.as_object() {
        if let Some(typ) = obj.get("type").and_then(Value::as_str) {
            match typ {
                "event_msg" => {
                    if let Some(payload) = obj.get("payload")
                        && payload
                            .get("type")
                            .and_then(Value::as_str)
                            .is_some_and(|ty| ty == "user_message")
                        && let Some(text) = extract_text(payload)
                    {
                        messages.push(("user".to_string(), text));
                    }
                }
                "response_item" | "message" => {
                    let container = obj.get("payload").unwrap_or(value);
                    if let Some(role) = container.get("role").and_then(Value::as_str)
                        && let Some(text) = extract_text(container)
                    {
                        messages.push((role.to_string(), text));
                    }
                }
                _ => {}
            }
        }

        if let Some(role) = obj.get("role").and_then(Value::as_str)
            && let Some(text) = extract_text(value)
        {
            messages.push((role.to_string(), text));
        }
    }

    if messages.is_empty()
        && let Some(role) = value.get("role").and_then(Value::as_str)
        && let Some(text) = extract_text(value)
    {
        messages.push((role.to_string(), text));
    }

    messages
}

fn extract_text(container: &Value) -> Option<String> {
    if let Some(content) = container.get("content")
        && let Some(items) = content.as_array()
    {
        let mut parts = Vec::new();
        for item in items {
            if let Some(text) = item.get("text").and_then(Value::as_str) {
                parts.push(text.to_string());
            } else if let Some(message) = item.get("message").and_then(Value::as_str) {
                parts.push(message.to_string());
            } else if let Some(text) = item.get("content").and_then(extract_text) {
                parts.push(text);
            }
        }
        if !parts.is_empty() {
            return Some(parts.join(""));
        }
    }

    if let Some(text) = container.get("payload").and_then(extract_text) {
        return Some(text);
    }

    if let Some(message) = container.get("message").and_then(Value::as_str) {
        return Some(message.to_string());
    }
    if let Some(text) = container.get("text").and_then(Value::as_str) {
        return Some(text.to_string());
    }
    None
}

const IGNORED_TAG_PREFIXES: [&str; 3] = [
    "<user_instructions>",
    "</user_instructions>",
    "<environment_context>",
];

fn clean_text(input: &str) -> Option<String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }

    if IGNORED_TAG_PREFIXES
        .iter()
        .any(|tag| trimmed.starts_with(tag))
    {
        return None;
    }

    Some(trimmed.to_string())
}

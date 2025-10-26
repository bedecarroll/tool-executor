use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use color_eyre::Result;
use color_eyre::eyre::{self, Context, eyre};
use itertools::Itertools;
use serde_json::Value;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use walkdir::WalkDir;

use crate::config::model::{Config, ProviderConfig};
use crate::db::Database;
use crate::session::{
    MessageRecord, SessionIngest, SessionSummary, fallback_session_uuid, session_uuid_from_value,
};

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

        let mut state = Self::collect_ingest_state(&session_id, path)?;

        if state.messages.is_empty() {
            Self::handle_empty_transcript(
                &mut state.messages,
                &mut state.first_prompt,
                state.fallback_preview,
                state.instructions_preview,
                state.saw_instruction_block,
                state.saw_any_record,
                &session_id,
            )?;
        }

        if let Some(first) = state.messages.first_mut() {
            first.is_first = true;
        }

        let actionable = state
            .messages
            .iter()
            .any(|message| message.role.eq_ignore_ascii_case("user"));
        if state.first_prompt.is_none() {
            state.first_prompt = state
                .messages
                .first()
                .map(|message| message.content.clone());
        }
        let session_uuid = state.session_uuid.or_else(|| fallback_session_uuid(path));
        let started_at = state.earliest_timestamp.or(created_at);
        let last_active = state.latest_timestamp.unwrap_or(mtime);
        let summary = SessionSummary {
            id: session_id,
            provider: provider.name.clone(),
            label,
            path: path.to_path_buf(),
            uuid: session_uuid,
            first_prompt: state.first_prompt,
            actionable,
            created_at,
            started_at,
            last_active: Some(last_active),
            size,
            mtime,
        };

        Ok(SessionIngest::new(summary, state.messages))
    }

    fn collect_ingest_state(session_id: &str, path: &Path) -> Result<IngestState> {
        let file = File::open(path)?;
        let reader = BufReader::new(file);
        let mut state = IngestState::default();
        let mut seen_messages: HashMap<(String, String, Option<i64>), usize> = HashMap::new();

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
            state.saw_any_record = true;

            if state.session_uuid.is_none() {
                state.session_uuid = session_uuid_from_value(&value);
            }

            if state.instructions_preview.is_none()
                && let Some(instructions) = value
                    .get("payload")
                    .and_then(|payload| payload.get("instructions"))
                    .and_then(Value::as_str)
            {
                state.instructions_preview = summarize_instructions(instructions);
            }

            let source = value
                .get("type")
                .and_then(Value::as_str)
                .map(str::to_string);
            let timestamp = parse_timestamp(&value);
            if let Some(ts) = timestamp {
                state.earliest_timestamp = Some(
                    state
                        .earliest_timestamp
                        .map_or(ts, |current| current.min(ts)),
                );
                state.latest_timestamp =
                    Some(state.latest_timestamp.map_or(ts, |current| current.max(ts)));
            }

            for (role, content) in extract_messages(&value) {
                let trimmed_content = content.trim();
                if state.fallback_preview.is_none()
                    && let Some(line) = trimmed_content
                        .lines()
                        .map(str::trim)
                        .find(|line| !line.is_empty() && !line.starts_with('<'))
                {
                    state.fallback_preview =
                        summarize_instructions(line).or_else(|| Some(line.to_string()));
                }

                if trimmed_content.starts_with("<user_instructions>")
                    || trimmed_content.starts_with("</user_instructions>")
                {
                    state.saw_instruction_block = true;
                }

                if let Some(clean) = clean_text(&content) {
                    let normalized_role = role.to_ascii_lowercase();
                    let key = (normalized_role.clone(), clean.clone(), timestamp);
                    if let Some(existing_idx) = seen_messages.get(&key) {
                        if let Some(existing) = state.messages.get_mut(*existing_idx) {
                            update_existing_source(existing, source.as_ref());
                        }
                        continue;
                    }

                    let is_user = normalized_role == "user";
                    let index = i64::try_from(state.messages.len()).unwrap_or(i64::MAX);
                    if state.first_prompt.is_none() && is_user {
                        state.first_prompt = Some(clean.clone());
                    }
                    state.messages.push(MessageRecord::new(
                        session_id,
                        index,
                        role,
                        clean,
                        source.clone(),
                        timestamp,
                    ));
                    seen_messages.insert(key, state.messages.len() - 1);
                }
            }
        }

        Ok(state)
    }

    fn handle_empty_transcript(
        messages: &mut Vec<MessageRecord>,
        first_prompt: &mut Option<String>,
        fallback_preview: Option<String>,
        instructions_preview: Option<String>,
        saw_instruction_block: bool,
        saw_any_record: bool,
        session_id: &str,
    ) -> Result<()> {
        let mut preview = fallback_preview.or(instructions_preview).or_else(|| {
            if saw_instruction_block {
                Some("Session bootstrapped (instructions only)".to_string())
            } else {
                None
            }
        });

        if preview.is_none() && saw_any_record {
            preview = Some("Session created (no transcript yet)".to_string());
        }

        if let Some(summary) = &mut preview
            && summary.len() > 240
        {
            summary.truncate(240);
        }

        let preview = preview.ok_or_else(|| eyre!("no messages discovered in session"))?;
        if first_prompt.is_none() {
            *first_prompt = Some(preview.clone());
        }
        messages.push(MessageRecord::new(
            session_id, 0, "system", preview, None, None,
        ));
        Ok(())
    }
}

#[derive(Default)]
struct IngestState {
    messages: Vec<MessageRecord>,
    first_prompt: Option<String>,
    fallback_preview: Option<String>,
    instructions_preview: Option<String>,
    saw_instruction_block: bool,
    saw_any_record: bool,
    session_uuid: Option<String>,
    earliest_timestamp: Option<i64>,
    latest_timestamp: Option<i64>,
}

fn update_existing_source(existing: &mut MessageRecord, source: Option<&String>) {
    if let Some(value) = source
        && (existing.source.is_none() || value == "response_item")
    {
        existing.source = Some(value.clone());
    }
}

fn parse_timestamp(value: &Value) -> Option<i64> {
    let timestamp = value.get("timestamp").and_then(Value::as_str)?;
    OffsetDateTime::parse(timestamp, &Rfc3339)
        .ok()
        .map(OffsetDateTime::unix_timestamp)
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

    let relative = match relative {
        Some(rel) if !rel.as_os_str().is_empty() => rel,
        _ => path
            .file_name()
            .map_or_else(|| path.to_path_buf(), PathBuf::from),
    };

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

fn summarize_instructions(raw: &str) -> Option<String> {
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed.starts_with('<') {
            continue;
        }
        if trimmed.starts_with('#') {
            let summary = trimmed.trim_start_matches('#').trim();
            if !summary.is_empty() {
                return Some(summary.to_string());
            }
            continue;
        }
        return Some(trimmed.to_string());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::model::{Defaults, FeatureConfig, SearchMode, SnippetConfig};
    use assert_fs::TempDir;
    use assert_fs::prelude::*;
    use color_eyre::Result;
    use indexmap::IndexMap;
    use serde_json::json;
    use std::convert::TryFrom;

    fn provider_with_root(root: &Path) -> ProviderConfig {
        ProviderConfig {
            name: "codex".to_string(),
            bin: "codex".to_string(),
            flags: Vec::new(),
            env: Vec::new(),
            session_roots: vec![root.to_path_buf()],
            stdin: None,
        }
    }

    fn config_from_provider(provider: ProviderConfig) -> Config {
        let mut providers = IndexMap::new();
        providers.insert("codex".into(), provider);
        Config {
            defaults: Defaults {
                provider: Some("codex".into()),
                profile: None,
                search_mode: SearchMode::FirstPrompt,
                preview_filter: None,
            },
            providers,
            snippets: SnippetConfig {
                pre: IndexMap::new(),
                post: IndexMap::new(),
            },
            wrappers: IndexMap::new(),
            profiles: IndexMap::new(),
            features: FeatureConfig {
                prompt_assembler: None,
            },
        }
    }

    #[test]
    fn instructions_only_sessions_get_placeholder() -> Result<()> {
        let temp = TempDir::new()?;
        let session_file = temp.child("session.jsonl");
        session_file.write_str(concat!(
            "{\"type\":\"session_meta\",\"payload\":{\"instructions\":\"# General guidance\\n\\nKeep things simple.\\n\"}}\n",
            "{\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"user\",\"content\":[{\"type\":\"input_text\",\"text\":\"<user_instructions>\\n\\n# General guidance\\n\\nKeep things simple.\\n</user_instructions>\"}]}}\n",
        ))?;

        let metadata = std::fs::metadata(session_file.path())?;
        let size = i64::try_from(metadata.len()).unwrap_or(i64::MAX);
        let now = current_unix_time();
        let provider = provider_with_root(temp.path());

        let ingest = Indexer::build_ingest(&provider, session_file.path(), size, now, Some(now))?;

        assert_eq!(ingest.messages.len(), 1);
        assert_eq!(ingest.messages[0].role, "system");
        assert!(
            ingest.messages[0].content.contains("General guidance"),
            "placeholder should summarize instructions"
        );
        assert!(ingest.messages[0].is_first);
        assert_eq!(ingest.messages[0].source, None);
        assert_eq!(ingest.summary.uuid.as_deref(), Some("session"));
        assert!(!ingest.summary.actionable);
        assert_eq!(
            ingest.summary.first_prompt.as_deref(),
            Some("General guidance")
        );
        assert_eq!(ingest.summary.started_at, Some(now));
        assert_eq!(ingest.summary.last_active, Some(now));

        Ok(())
    }

    #[test]
    fn user_and_assistant_messages_are_ingested() -> Result<()> {
        let temp = TempDir::new()?;
        let session_file = temp.child("conversation.jsonl");
        session_file.write_str(concat!(
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"user_message\",\"message\":\"Hello world\"}}\n",
            "{\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"Hi there\"}]}}\n",
        ))?;

        let metadata = std::fs::metadata(session_file.path())?;
        let size = i64::try_from(metadata.len()).unwrap_or(i64::MAX);
        let now = current_unix_time();
        let provider = provider_with_root(temp.path());

        let ingest = Indexer::build_ingest(&provider, session_file.path(), size, now, Some(now))?;

        assert_eq!(ingest.messages.len(), 2);
        assert_eq!(ingest.messages[0].role, "user");
        assert_eq!(ingest.messages[0].content, "Hello world");
        assert!(ingest.messages[0].is_first);
        assert_eq!(ingest.messages[0].source.as_deref(), Some("event_msg"));
        assert_eq!(ingest.messages[1].role, "assistant");
        assert!(!ingest.messages[1].is_first);
        assert_eq!(ingest.messages[1].source.as_deref(), Some("response_item"));
        assert!(ingest.summary.actionable);
        assert_eq!(ingest.summary.first_prompt.as_deref(), Some("Hello world"));
        assert_eq!(ingest.summary.uuid.as_deref(), Some("conversation"));
        assert_eq!(ingest.summary.started_at, Some(now));
        assert_eq!(ingest.summary.last_active, Some(now));

        Ok(())
    }

    #[test]
    fn indexer_reports_errors_for_unexpected_payloads() -> Result<()> {
        let temp = TempDir::new()?;
        let sessions_dir = temp.child("sessions");
        sessions_dir.create_dir_all()?;

        let good = sessions_dir.child("good.jsonl");
        good.write_str(
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"user_message\",\"message\":\"Hello\"}}\n",
        )?;

        let bad = sessions_dir.child("bad.jsonl");
        bad.write_str("{not-json}\n")?;

        let mut providers = IndexMap::new();
        providers.insert("codex".into(), provider_with_root(sessions_dir.path()));

        let config = Config {
            defaults: Defaults {
                provider: Some("codex".into()),
                profile: None,
                search_mode: SearchMode::FirstPrompt,
                preview_filter: None,
            },
            providers,
            snippets: SnippetConfig {
                pre: IndexMap::new(),
                post: IndexMap::new(),
            },
            wrappers: IndexMap::new(),
            profiles: IndexMap::new(),
            features: FeatureConfig {
                prompt_assembler: None,
            },
        };

        let db_path = temp.child("tx.sqlite3");
        let mut db = Database::open(db_path.path())?;

        let mut indexer = Indexer::new(&mut db, &config);
        let report = indexer.run()?;
        assert_eq!(report.updated, 1, "expected one session to be ingested");
        assert_eq!(report.errors.len(), 1, "expected one error for bad payload");
        let error_path = &report.errors[0].path;
        assert!(
            error_path.ends_with("bad.jsonl"),
            "unexpected error path: {error_path:?}"
        );
        Ok(())
    }

    #[test]
    fn indexer_processes_single_file_roots() -> Result<()> {
        let temp = TempDir::new()?;
        let session_file = temp.child("session.jsonl");
        session_file.write_str(
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"user_message\",\"message\":\"Inline root\"}}\n",
        )?;

        let provider = provider_with_root(session_file.path());
        let config = config_from_provider(provider);
        let db_path = temp.child("tx.sqlite3");
        let mut db = Database::open(db_path.path())?;

        let mut indexer = Indexer::new(&mut db, &config);
        let report = indexer.run()?;
        assert_eq!(report.scanned, 1, "single file root should be scanned once");
        assert_eq!(report.updated, 1, "session should be ingested");
        Ok(())
    }

    #[test]
    fn indexer_ignores_missing_roots() -> Result<()> {
        let temp = TempDir::new()?;
        let missing = temp.child("missing-root");
        let provider = provider_with_root(missing.path());
        let config = config_from_provider(provider);
        let db_path = temp.child("tx.sqlite3");
        let mut db = Database::open(db_path.path())?;

        let mut indexer = Indexer::new(&mut db, &config);
        let report = indexer.run()?;
        assert_eq!(report.scanned, 0);
        assert_eq!(report.updated, 0);
        assert_eq!(report.removed, 0);
        Ok(())
    }

    #[test]
    fn indexer_skips_non_jsonl_file_roots() -> Result<()> {
        let temp = TempDir::new()?;
        let notes = temp.child("notes.txt");
        notes.write_str("not jsonl")?;

        let provider = provider_with_root(notes.path());
        let config = config_from_provider(provider);
        let db_path = temp.child("tx.sqlite3");
        let mut db = Database::open(db_path.path())?;

        let mut indexer = Indexer::new(&mut db, &config);
        let report = indexer.run()?;
        assert_eq!(report.scanned, 0);
        assert_eq!(report.updated, 0);
        assert!(report.errors.is_empty());
        Ok(())
    }

    #[test]
    fn indexer_removes_sessions_missing_on_disk() -> Result<()> {
        let temp = TempDir::new()?;
        let sessions_dir = temp.child("sessions");
        sessions_dir.create_dir_all()?;

        let mut providers = IndexMap::new();
        providers.insert("codex".into(), provider_with_root(sessions_dir.path()));

        let config = Config {
            defaults: Defaults {
                provider: Some("codex".into()),
                profile: None,
                search_mode: SearchMode::FirstPrompt,
                preview_filter: None,
            },
            providers,
            snippets: SnippetConfig {
                pre: IndexMap::new(),
                post: IndexMap::new(),
            },
            wrappers: IndexMap::new(),
            profiles: IndexMap::new(),
            features: FeatureConfig {
                prompt_assembler: None,
            },
        };

        let db_path = temp.child("tx.sqlite3");
        let mut db = Database::open(db_path.path())?;

        let missing_path = sessions_dir.child("missing.jsonl");
        let summary = SessionSummary {
            id: "codex/missing.jsonl".into(),
            provider: "codex".into(),
            label: Some("orphaned".into()),
            path: missing_path.path().to_path_buf(),
            uuid: Some("missing".into()),
            first_prompt: Some("hello".into()),
            actionable: true,
            created_at: Some(0),
            started_at: Some(0),
            last_active: Some(0),
            size: 1,
            mtime: 1,
        };
        let ingest = SessionIngest::new(
            summary,
            vec![MessageRecord::new(
                "codex/missing.jsonl",
                0,
                "system",
                "hello",
                None,
                None,
            )],
        );
        db.upsert_session(&ingest)?;

        let mut indexer = Indexer::new(&mut db, &config);
        let report = indexer.run()?;
        assert_eq!(report.removed, 1, "expected missing session to be pruned");
        assert_eq!(report.updated, 0, "no new sessions expected");
        assert_eq!(db.count_sessions()?, 0, "database should be empty");
        Ok(())
    }

    #[test]
    fn indexer_skips_unchanged_sessions_on_subsequent_runs() -> Result<()> {
        let temp = TempDir::new()?;
        let sessions_dir = temp.child("sessions");
        sessions_dir.create_dir_all()?;

        let session_file = sessions_dir.child("session.jsonl");
        session_file.write_str(
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"user_message\",\"message\":\"Ping\"}}\n",
        )?;

        let mut providers = IndexMap::new();
        providers.insert("codex".into(), provider_with_root(sessions_dir.path()));

        let config = Config {
            defaults: Defaults {
                provider: Some("codex".into()),
                profile: None,
                search_mode: SearchMode::FirstPrompt,
                preview_filter: None,
            },
            providers,
            snippets: SnippetConfig {
                pre: IndexMap::new(),
                post: IndexMap::new(),
            },
            wrappers: IndexMap::new(),
            profiles: IndexMap::new(),
            features: FeatureConfig {
                prompt_assembler: None,
            },
        };

        let db_path = temp.child("tx.sqlite3");
        let mut db = Database::open(db_path.path())?;

        {
            let mut indexer = Indexer::new(&mut db, &config);
            let report = indexer.run()?;
            assert_eq!(report.updated, 1, "initial run should ingest session");
            assert_eq!(report.skipped, 0);
        }

        let mut indexer = Indexer::new(&mut db, &config);
        let report = indexer.run()?;
        assert_eq!(report.updated, 0, "second run should not rewrite session");
        assert_eq!(report.skipped, 1, "unchanged session should be skipped");
        Ok(())
    }

    #[test]
    fn indexer_handles_missing_roots_and_single_file_providers() -> Result<()> {
        let temp = TempDir::new()?;
        let missing = temp.child("missing");
        let single = temp.child("single.jsonl");
        single.write_str(
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"user_message\",\"message\":\"Ping\"}}\n",
        )?;

        let mut providers = IndexMap::new();
        providers.insert(
            "codex".into(),
            ProviderConfig {
                name: "codex".into(),
                bin: "echo".into(),
                flags: Vec::new(),
                env: Vec::new(),
                session_roots: vec![missing.path().to_path_buf(), single.path().to_path_buf()],
                stdin: None,
            },
        );

        let config = Config {
            defaults: Defaults {
                provider: Some("codex".into()),
                profile: None,
                search_mode: SearchMode::FirstPrompt,
                preview_filter: None,
            },
            providers,
            snippets: SnippetConfig {
                pre: IndexMap::new(),
                post: IndexMap::new(),
            },
            wrappers: IndexMap::new(),
            profiles: IndexMap::new(),
            features: FeatureConfig {
                prompt_assembler: None,
            },
        };

        let db_path = temp.child("tx.sqlite3");
        let mut db = Database::open(db_path.path())?;
        let mut indexer = Indexer::new(&mut db, &config);
        let report = indexer.run()?;
        assert_eq!(report.scanned, 1);
        assert_eq!(report.updated, 1);
        Ok(())
    }

    #[test]
    fn indexer_removes_stale_sessions_when_files_deleted() -> Result<()> {
        let temp = TempDir::new()?;
        let sessions_dir = temp.child("sessions");
        sessions_dir.create_dir_all()?;
        let session_file = sessions_dir.child("obsolete.jsonl");
        session_file.write_str(
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"user_message\",\"message\":\"Hello\"}}\n",
        )?;

        let mut providers = IndexMap::new();
        providers.insert("codex".into(), provider_with_root(sessions_dir.path()));

        let config = Config {
            defaults: Defaults {
                provider: Some("codex".into()),
                profile: None,
                search_mode: SearchMode::FirstPrompt,
                preview_filter: None,
            },
            providers,
            snippets: SnippetConfig {
                pre: IndexMap::new(),
                post: IndexMap::new(),
            },
            wrappers: IndexMap::new(),
            profiles: IndexMap::new(),
            features: FeatureConfig {
                prompt_assembler: None,
            },
        };

        let db_path = temp.child("tx.sqlite3");
        let mut db = Database::open(db_path.path())?;

        {
            let mut indexer = Indexer::new(&mut db, &config);
            let report = indexer.run()?;
            assert_eq!(report.updated, 1);
            assert_eq!(db.count_sessions()?, 1);
        }

        fs::remove_file(session_file.path())?;

        let mut indexer = Indexer::new(&mut db, &config);
        let report = indexer.run()?;
        assert_eq!(report.removed, 1, "stale session should be removed");
        assert_eq!(db.count_sessions()?, 0);
        Ok(())
    }

    #[test]
    fn compute_session_id_normalizes_paths() -> Result<()> {
        let temp = TempDir::new()?;
        let sessions_dir = temp.child("sessions");
        let nested = sessions_dir.child("nested");
        nested.create_dir_all()?;
        let file = nested.child("conversation.jsonl");
        file.write_str("{}\n")?;
        let provider = provider_with_root(sessions_dir.path());
        let (id, relative) = compute_session_id(&provider, file.path());
        assert_eq!(id, format!("{}/nested/conversation.jsonl", provider.name));
        assert_eq!(relative, "nested/conversation.jsonl");
        Ok(())
    }

    #[test]
    fn compute_session_id_falls_back_to_filename() -> Result<()> {
        let temp = TempDir::new()?;
        let sessions_dir = temp.child("sessions");
        sessions_dir.create_dir_all()?;
        let file = temp.child("orphan.jsonl");
        file.write_str("{}\n")?;
        let provider = provider_with_root(sessions_dir.path());
        let (id, relative) = compute_session_id(&provider, file.path());
        assert_eq!(id, "codex/orphan.jsonl");
        assert_eq!(relative, "orphan.jsonl");
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn compute_session_id_handles_canonical_roots() -> Result<()> {
        use std::os::unix::fs as unix_fs;

        let temp = TempDir::new()?;
        let real_dir = temp.child("real");
        real_dir.create_dir_all()?;
        let file = real_dir.child("symlinked.jsonl");
        file.write_str("{}\n")?;

        let link = temp.child("link");
        unix_fs::symlink(real_dir.path(), link.path())?;
        let provider = provider_with_root(link.path());
        let (id, relative) = compute_session_id(&provider, file.path());
        assert_eq!(id, format!("{}/symlinked.jsonl", provider.name));
        assert_eq!(relative, "symlinked.jsonl");
        Ok(())
    }

    #[test]
    fn update_existing_source_prefers_response_item() {
        let mut record = MessageRecord::new(
            "sess",
            0,
            "assistant",
            "reply",
            Some("event_msg".into()),
            None,
        );
        update_existing_source(&mut record, Some(&"response_item".to_string()));
        assert_eq!(record.source.as_deref(), Some("response_item"));

        update_existing_source(&mut record, Some(&"other".to_string()));
        assert_eq!(record.source.as_deref(), Some("response_item"));

        let mut missing = MessageRecord::new("sess", 1, "assistant", "text", None, None);
        update_existing_source(&mut missing, Some(&"event_msg".to_string()));
        assert_eq!(missing.source.as_deref(), Some("event_msg"));
    }

    #[test]
    fn extract_text_handles_nested_payloads() {
        let nested = json!({
            "payload": {
                "content": [
                    {"type": "text", "text": "Hello"},
                    {"type": "text", "text": " world"}
                ]
            }
        });
        assert_eq!(extract_text(&nested), Some("Hello world".into()));

        let message = json!({
            "message": "fallback"
        });
        assert_eq!(extract_text(&message), Some("fallback".into()));
    }

    #[test]
    fn extract_messages_covers_event_and_response() {
        let event = json!({
            "type": "event_msg",
            "payload": {
                "type": "user_message",
                "message": "Hello"
            }
        });
        let mut results = extract_messages(&event);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, "user");

        let response = json!({
            "type": "response_item",
            "payload": {
                "role": "assistant",
                "content": [{"type": "text", "text": "Hi"}]
            }
        });
        results = extract_messages(&response);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, "assistant");

        let direct = json!({
            "role": "system",
            "content": [{"type": "text", "text": "System"}]
        });
        results = extract_messages(&direct);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, "system");
    }
}

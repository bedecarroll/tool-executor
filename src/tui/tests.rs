#![allow(
    unused_mut,
    clippy::implicit_clone,
    clippy::redundant_clone,
    clippy::map_unwrap_or
)]
use super::*;
use assert_fs::TempDir;
#[cfg(unix)]
use assert_fs::fixture::PathChild;
#[cfg(unix)]
use assert_fs::prelude::*;
use color_eyre::Result;
use color_eyre::eyre::eyre;
#[cfg(unix)]
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use indexmap::IndexMap;
#[cfg(unix)]
use ratatui::Terminal;
#[cfg(unix)]
use ratatui::backend::TestBackend;
#[cfg(unix)]
use ratatui::buffer::Buffer;
#[cfg(unix)]
use std::fs;
#[cfg(unix)]
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::sync::{LazyLock, Mutex, MutexGuard};
#[cfg(unix)]
use std::time::Duration as StdDuration;
use time::{Duration, OffsetDateTime};

use crate::config::AppDirectories;
use crate::config::Config;
use crate::config::model::{
    Defaults, FeatureConfig, ProfileConfig, ProviderConfig, SearchMode, SnippetConfig,
    StdinMapping, StdinMode, WrapperConfig, WrapperMode,
};
#[cfg(unix)]
use crate::config::model::{PromptAssemblerConfig, Snippet};
use crate::db::Database;
#[cfg(unix)]
use crate::pipeline::Invocation;
#[cfg(unix)]
use crate::prompts::PromptAssembler;
use crate::session::{MessageRecord, SessionIngest, SessionSummary, Transcript};

#[cfg(unix)]
static PATH_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

#[cfg(unix)]
struct PathGuard {
    original: Option<String>,
    _lock: MutexGuard<'static, ()>,
}

#[cfg(unix)]
impl PathGuard {
    fn push(temp: &TempDir) -> Self {
        let lock = PATH_LOCK.lock().unwrap();
        let original = env::var("PATH").ok();
        let mut paths = vec![temp.path().to_path_buf()];
        if let Some(value) = &original {
            paths.extend(env::split_paths(value).collect::<Vec<_>>());
        }
        let joined = env::join_paths(paths).expect("join paths");
        unsafe {
            env::set_var("PATH", joined);
        }
        Self {
            original,
            _lock: lock,
        }
    }
}

#[cfg(unix)]
impl Drop for PathGuard {
    fn drop(&mut self) {
        if let Some(value) = &self.original {
            unsafe {
                env::set_var("PATH", value);
            }
        } else {
            unsafe {
                env::remove_var("PATH");
            }
        }
    }
}

fn provider_config(root: &Path) -> ProviderConfig {
    ProviderConfig {
        name: "codex".into(),
        bin: "echo".into(),
        flags: vec!["hello".into()],
        env: Vec::new(),
        session_roots: vec![root.join("sessions")],
        stdin: None,
    }
}

fn build_config(root: &Path) -> Config {
    let mut providers = IndexMap::new();
    providers.insert("codex".into(), provider_config(root));

    let snippets = SnippetConfig {
        pre: IndexMap::new(),
        post: IndexMap::new(),
    };

    let mut profiles = IndexMap::new();
    profiles.insert(
        "default".into(),
        ProfileConfig {
            name: "default".into(),
            provider: "codex".into(),
            description: Some("Default profile".into()),
            pre: Vec::new(),
            post: Vec::new(),
            wrap: None,
            prompt_assembler: None,
            prompt_assembler_args: Vec::new(),
        },
    );

    Config {
        defaults: Defaults {
            provider: Some("codex".into()),
            profile: Some("default".into()),
            search_mode: SearchMode::FirstPrompt,
            terminal_title: None,
        },
        providers,
        snippets,
        wrappers: IndexMap::new(),
        profiles,
        features: FeatureConfig {
            prompt_assembler: None,
        },
    }
}

fn build_directories(temp: &TempDir) -> AppDirectories {
    AppDirectories {
        config_dir: temp.path().join("config"),
        data_dir: temp.path().join("data"),
        cache_dir: temp.path().join("cache"),
    }
}

fn insert_session(db: &mut Database, path: &Path, id: &str) -> Result<SessionSummary> {
    let summary = SessionSummary {
        id: id.into(),
        provider: "codex".into(),
        wrapper: None,
        model: None,
        label: Some(format!("Session {id}")),
        path: path.to_path_buf(),
        uuid: Some(format!("uuid-{id}")),
        first_prompt: Some("Hello there".into()),
        actionable: true,
        created_at: Some(OffsetDateTime::now_utc().unix_timestamp()),
        started_at: Some(OffsetDateTime::now_utc().unix_timestamp()),
        last_active: Some(OffsetDateTime::now_utc().unix_timestamp()),
        size: 15,
        mtime: 20,
    };
    let mut message = MessageRecord::new(
        summary.id.clone(),
        0,
        "user",
        "Hello there",
        Some(format!("event_msg_{id}")),
        Some(summary.last_active.unwrap()),
    );
    message.is_first = true;
    let ingest = SessionIngest::new(summary.clone(), vec![message]);
    db.upsert_session(&ingest)?;
    Ok(summary)
}

fn insert_session_without_uuid(db: &mut Database, path: &Path, id: &str) -> Result<SessionSummary> {
    let summary = SessionSummary {
        id: id.into(),
        provider: "codex".into(),
        wrapper: None,
        model: None,
        label: Some(format!("Session {id}")),
        path: path.to_path_buf(),
        uuid: None,
        first_prompt: Some("Hello there".into()),
        actionable: true,
        created_at: Some(OffsetDateTime::now_utc().unix_timestamp()),
        started_at: Some(OffsetDateTime::now_utc().unix_timestamp()),
        last_active: Some(OffsetDateTime::now_utc().unix_timestamp()),
        size: 15,
        mtime: 20,
    };
    let mut message = MessageRecord::new(
        summary.id.clone(),
        0,
        "user",
        "Hello there",
        Some(format!("event_msg_{id}")),
        Some(summary.last_active.unwrap()),
    );
    message.is_first = true;
    let ingest = SessionIngest::new(summary.clone(), vec![message]);
    db.upsert_session(&ingest)?;
    Ok(summary)
}

#[cfg(unix)]
#[test]
fn app_state_navigation_and_plan() -> Result<()> {
    let temp = TempDir::new()?;
    let mut config = build_config(temp.path());
    let directories = build_directories(&temp);
    directories.ensure_all()?;

    let mut db = Database::open(&directories.data_dir.join("tx.sqlite3"))?;
    let session_dir = config
        .providers
        .get("codex")
        .unwrap()
        .session_roots
        .first()
        .unwrap()
        .clone();
    fs::create_dir_all(&session_dir)?;
    let session_path = session_dir.join("demo.jsonl");
    fs::File::create(&session_path)?.write_all(b"{}")?;
    insert_session(&mut db, &session_path, "sess-1")?;

    let mut ctx = UiContext {
        config: &config,
        directories: &directories,
        db: &mut db,
        prompt: None,
    };

    let mut state = AppState::new(&mut ctx)?;
    assert!(!state.entries.is_empty());

    // Navigate list and type filter characters.
    state.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
    state.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE))?;
    state.handle_key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE))?;
    state.handle_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE))?;
    assert_eq!(state.filter, "/d");
    assert!(state.message.is_none());
    state.handle_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE))?;
    state.handle_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE))?;
    assert!(state.filter.is_empty());

    // Cycle provider filters.
    state.handle_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL))?;
    state.handle_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL))?;
    state.handle_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL))?;

    // Build a plan for the selected session.
    let plan = state.build_plan()?.expect("plan generated");
    match plan.invocation {
        Invocation::Shell { ref command } => assert!(command.contains("echo")),
        Invocation::Exec { .. } => panic!("expected shell invocation"),
    }

    // Render the UI.
    let backend = TestBackend::new(60, 20);
    let mut terminal = Terminal::new(backend)?;
    terminal.draw(|frame| draw(frame, &mut state))?;
    Ok(())
}

#[test]
fn session_entry_helpers_and_matching() {
    let entry = SessionEntry {
        id: "sess".into(),
        provider: "codex".into(),
        wrapper: None,
        label: Some("Demo".into()),
        first_prompt: Some("Hello prompt".into()),
        actionable: true,
        last_active: Some(0),
        snippet: Some("Snippet line".into()),
        snippet_role: Some("user".into()),
    };
    assert!(entry.matches("demo"));
    assert!(entry.matches("codex"));
    assert!(entry.matches("hello"));
    assert!(entry.matches("snippet"));
    assert!(entry.matches("#sess"));
    assert_eq!(entry.display_label(), "Demo");
    assert_eq!(entry.snippet_line().as_deref(), Some("Hello prompt"));
    assert_eq!(entry.short_session_tag(), "#sess");
}

#[test]
fn profile_entry_matching() {
    let entry = ProfileEntry {
        display: "Default".into(),
        provider: "codex".into(),
        pre: vec!["pre".into()],
        post: vec!["post".into()],
        wrap: None,
        description: Some("helpful".into()),
        tags: vec!["team".into()],
        inline_pre: Vec::new(),
        stdin_supported: true,
        prompt_assembler: None,
        prompt_assembler_args: Vec::new(),
        prompt_available: true,
        kind: ProfileKind::Config {
            name: "default".into(),
        },
        preview_lines: Vec::new(),
    };
    assert!(entry.matches("help"));
    assert!(entry.matches("team"));
}

#[test]
fn formatting_helpers_cover_paths() {
    assert_eq!(truncate_len("hello", 10), "hello");
    assert_eq!(truncate_len("longword", 4), "long");
    assert_eq!(normalize_whitespace(" test\nvalue "), "test value");
    assert_eq!(pad_relative_time("3m"), "3m      ");
    assert_eq!(
        format_rollout_label("rollout-2024-10-26T02-42-13-abcd"),
        Some("2024-10-26 02:42:13 • abcd".into())
    );
    assert_eq!(rollout_suffix("custom-xyz"), Some("custom-xyz".into()));
    assert_eq!(
        meaningful_excerpt("   \n# Title\nContent line."),
        Some("# Title Content line.".into())
    );

    let now = OffsetDateTime::from_unix_timestamp(1_000_000).unwrap();
    let past = now - Duration::minutes(5);
    assert_eq!(
        format_relative_time(Some(past.unix_timestamp()), now),
        Some("5m ago".into())
    );
    assert_eq!(format_relative_time(None, now), None);
    assert_eq!(
        format_relative_time(Some((now + Duration::seconds(30)).unix_timestamp()), now),
        Some("in <1m".into())
    );
    assert_eq!(
        format_relative_time(Some((now - Duration::seconds(20)).unix_timestamp()), now),
        Some("just now".into())
    );
    assert_eq!(
        format_relative_time(Some((now + Duration::hours(2)).unix_timestamp()), now),
        Some("in 2h".into())
    );
    let days_ago = now - Duration::days(3);
    assert!(
        format_relative_time(Some(days_ago.unix_timestamp()), now)
            .unwrap()
            .contains(' ')
    );
    let last_year = now.replace_year(now.year() - 1).unwrap();
    assert!(
        format_relative_time(Some(last_year.unix_timestamp()), now)
            .unwrap()
            .contains('-')
    );
    assert!(format_dual_time(Some(now.unix_timestamp())).is_some());
    assert_eq!(truncate("hello", 10), "hello");
    assert_eq!(truncate("truncate", 4), "tru…");
}

#[test]
fn fake_events_read_errors_when_empty() {
    let mut events = FakeEvents::new(Vec::new());
    let err = events
        .read()
        .expect_err("empty fake event queue should error");
    assert!(err.to_string().contains("no more events"));
}

#[test]
fn session_entry_matching_display_and_snippet_fallbacks() {
    let entry = SessionEntry {
        id: "root/rollout-2024-10-26T02-42-13-ABCD".into(),
        provider: "codex".into(),
        wrapper: Some("ShellWrap".into()),
        label: None,
        first_prompt: None,
        actionable: true,
        last_active: Some(0),
        snippet: Some("Snippet body".into()),
        snippet_role: Some("Assistant".into()),
    };

    assert!(entry.matches("root/rollout"));
    assert!(entry.matches("shellwrap"));
    assert!(entry.matches("02:42:13"));
    assert_eq!(entry.display_label(), "2024-10-26 02:42:13 • abcd");
    assert_eq!(
        entry.snippet_line().as_deref(),
        Some("[assistant] Snippet body")
    );
    assert!(entry.short_session_tag().starts_with("#rollout-2024"));

    let entry_without_role = SessionEntry {
        snippet_role: None,
        ..entry.clone()
    };
    assert_eq!(
        entry_without_role.snippet_line().as_deref(),
        Some("Snippet body")
    );

    let entry_without_snippet = SessionEntry {
        snippet: None,
        snippet_role: None,
        ..entry
    };
    assert_eq!(entry_without_snippet.snippet_line(), None);
}

#[test]
fn session_entry_label_and_tag_fallback_paths() {
    let labeled = SessionEntry {
        id: "session-1".into(),
        provider: "codex".into(),
        wrapper: None,
        label: Some("rollout-2024-10-26T02-42-13-CUSTOM".into()),
        first_prompt: Some("Prompt".into()),
        actionable: true,
        last_active: Some(0),
        snippet: None,
        snippet_role: None,
    };
    assert_eq!(labeled.display_label(), "2024-10-26 02:42:13 • custom");
    assert_eq!(labeled.snippet_line().as_deref(), Some("Prompt"));

    let path_fallback = SessionEntry {
        label: None,
        id: "parent/SessionLabel".into(),
        ..labeled.clone()
    };
    assert_eq!(path_fallback.display_label(), "SessionLabel");

    let dashed = SessionEntry {
        label: None,
        id: "parent/---".into(),
        ..labeled
    };
    assert_eq!(dashed.short_session_tag(), "#---");
    assert_eq!(rollout_suffix("---"), None);
}

#[test]
fn format_relative_time_covers_future_minutes_and_days() {
    let now = OffsetDateTime::from_unix_timestamp(1_000_000).expect("valid timestamp");
    assert_eq!(
        format_relative_time(Some((now + Duration::minutes(5)).unix_timestamp()), now),
        Some("in 5m".into())
    );
    assert!(
        format_relative_time(Some((now + Duration::days(2)).unix_timestamp()), now)
            .expect("future day should render")
            .starts_with("on ")
    );
}

#[test]
fn markdown_lines_to_text_returns_none_for_empty_input() {
    let lines: Vec<String> = Vec::new();
    assert_eq!(markdown_lines_to_text(&lines), None);
}

#[test]
fn session_preview_from_result_error_with_wrapper() {
    let mut session = make_session_entry("sess-error-wrapper");
    session.wrapper = Some("shellwrap".into());
    let preview = AppState::session_preview_from_result(&session, Err(eyre!("boom")));

    assert_eq!(preview.lines[0], "**Wrapper**: `shellwrap`");
    assert_eq!(preview.lines[1], "");
    assert!(preview.lines[2].contains("boom"));
}

#[cfg(unix)]
#[test]
fn state_filtering_and_provider_cycle() -> Result<()> {
    let temp = TempDir::new()?;
    let mut config = build_config(temp.path());
    let directories = build_directories(&temp);
    directories.ensure_all()?;
    let mut db = Database::open(&directories.data_dir.join("tx.sqlite3"))?;
    let session_dir = config
        .providers
        .get("codex")
        .unwrap()
        .session_roots
        .first()
        .unwrap()
        .clone();
    fs::create_dir_all(&session_dir)?;
    let session_path = session_dir.join("filter.jsonl");
    fs::File::create(&session_path)?.write_all(b"{\"event\":\"filter\"}\n")?;
    insert_session(&mut db, &session_path, "sess-1")?;

    let mut ctx = UiContext {
        config: &config,
        directories: &directories,
        db: &mut db,
        prompt: None,
    };

    let mut state = AppState::new(&mut ctx)?;
    state.message = Some("filter mode".into());
    state.message = None;
    state.set_temporary_status_message("temp".into(), StdDuration::from_secs(0));
    state.expire_status_message();
    let _ = state.status_message();

    state.handle_key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE))?;
    state.handle_key(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE))?;
    assert_eq!(state.filter, "/h");
    assert!(state.message.is_none());

    state.filter = "Hello".into();
    state.full_text = true;
    state.refresh_entries()?;
    state.move_selection(5);
    state.move_selection(-5);
    state.provider_filter = Some("alt".into());
    state.refresh_entries()?;
    state.provider_filter = None;
    state.filter.clear();
    state.refresh_entries()?;

    state.full_text = false;
    state.filter = "Demo".into();
    state.refresh_entries()?;
    state.filter.clear();
    state.refresh_entries()?;

    state.cycle_provider_filter()?;
    state.cycle_provider_filter()?;

    state.list_state.select(Some(0));
    state.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE))?;
    state.reindex()?;

    if let Some((idx, Entry::Profile(_))) = state
        .entries
        .iter()
        .enumerate()
        .find(|(_, entry)| matches!(entry, Entry::Profile(_)))
    {
        state.index = idx;
        state.list_state.select(Some(idx));
        let plan = state.build_plan()?.expect("profile plan");
        if let Invocation::Shell { command } = plan.invocation {
            assert!(command.contains("echo"));
        }
    }
    Ok(())
}

fn make_session_entry(id: &str) -> SessionEntry {
    SessionEntry {
        id: id.into(),
        provider: "codex".into(),
        wrapper: None,
        label: Some("Demo".into()),
        first_prompt: Some("Hello".into()),
        actionable: true,
        last_active: Some(123),
        snippet: None,
        snippet_role: None,
    }
}

fn make_transcript(id: &str) -> Transcript {
    let summary = SessionSummary {
        id: id.into(),
        provider: "codex".into(),
        wrapper: None,
        model: None,
        label: Some("Demo".into()),
        path: PathBuf::from("/tmp/demo.jsonl"),
        uuid: Some("uuid-demo".into()),
        first_prompt: Some("Hello".into()),
        actionable: true,
        created_at: Some(1),
        started_at: Some(1),
        last_active: Some(2),
        size: 42,
        mtime: 2,
    };
    let message = MessageRecord::new(
        id,
        0,
        "user",
        "Hello world",
        Some("event_msg".into()),
        Some(2),
    );
    Transcript {
        session: summary,
        messages: vec![message],
    }
}

#[test]
fn session_preview_from_result_success() {
    let session = make_session_entry("sess-success");
    let transcript = make_transcript("sess-success");
    let preview = AppState::session_preview_from_result(&session, Ok(Some(transcript)));
    assert!(preview.lines.join("\n").contains("Hello world"));
    assert!(preview.title.is_some());
    let styled = preview
        .styled
        .as_ref()
        .expect("expected styled markdown output");
    assert!(
        styled
            .lines
            .iter()
            .any(|line| line.style != Style::default()
                || line.spans.iter().any(|span| span.style != Style::default()))
    );
}

#[test]
fn session_preview_from_result_missing_transcript() {
    let session = make_session_entry("sess-missing");
    let preview = AppState::session_preview_from_result(&session, Ok(None));
    assert_eq!(preview.lines, vec![String::from("No transcript available")]);
    assert_eq!(preview.title.as_deref(), Some("sess-missing"));
}

#[test]
fn session_preview_from_result_error() {
    let session = make_session_entry("sess-error");
    let preview = AppState::session_preview_from_result(&session, Err(eyre!("boom")));
    assert!(preview.lines[0].contains("boom"));
}

#[test]
fn session_preview_from_result_includes_wrapper_metadata() {
    let mut session = make_session_entry("sess-wrapper");
    session.wrapper = Some("shellwrap".into());
    let mut transcript = make_transcript("sess-wrapper");
    transcript.session.wrapper = Some("shellwrap".into());

    let preview = AppState::session_preview_from_result(&session, Ok(Some(transcript)));
    let joined = preview.lines.join("\n");
    assert!(
        joined.contains("**Wrapper**: `shellwrap`"),
        "expected wrapper metadata in preview:\n{joined}"
    );
}

#[test]
fn session_preview_from_result_missing_transcript_with_wrapper() {
    let mut session = make_session_entry("sess-missing-wrapper");
    session.wrapper = Some("shellwrap".into());

    let preview = AppState::session_preview_from_result(&session, Ok(None));
    assert_eq!(
        preview.lines,
        vec![
            String::from("**Wrapper**: `shellwrap`"),
            String::new(),
            String::from("No transcript available")
        ]
    );
}

#[test]
#[cfg(unix)]
fn handle_key_navigation_and_provider_cycle() -> Result<()> {
    let temp = TempDir::new()?;
    let mut config = build_config(temp.path());
    let directories = build_directories(&temp);
    directories.ensure_all()?;
    let mut db = Database::open(&directories.data_dir.join("tx.sqlite3"))?;
    let session_dir = config
        .providers
        .get("codex")
        .unwrap()
        .session_roots
        .first()
        .unwrap()
        .to_path_buf();
    fs::create_dir_all(&session_dir)?;

    for idx in 0..12 {
        let filename = format!("session-{idx}.jsonl");
        let path = session_dir.join(&filename);
        fs::File::create(&path)?.write_all(b"{\"event\":\"demo\"}\n")?;
        insert_session(&mut db, &path, &format!("sess-{idx}"))?;
    }

    let mut ctx = UiContext {
        config: &config,
        directories: &directories,
        db: &mut db,
        prompt: None,
    };

    let mut state = AppState::new(&mut ctx)?;
    assert!(state.entries.len() >= 12);

    state.filter = "hello".into();
    state.message = Some(MESSAGE_FILTER_MODE.to_string());
    state.handle_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE))?;
    assert_eq!(state.filter, "hell");
    state.handle_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE))?;
    state.handle_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::SHIFT))?;
    assert_eq!(state.filter, "he");
    state.handle_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE))?;
    state.handle_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE))?;
    assert!(state.filter.is_empty());
    state.message = None;

    state.handle_key(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE))?;
    assert!(state.index >= 10);
    let after_page_down = state.index;
    state.handle_key(KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE))?;
    assert!(state.index <= after_page_down);
    state.handle_key(KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE))?;
    assert_eq!(state.index, 0);

    state.handle_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL))?;
    assert_eq!(state.provider_filter.as_deref(), Some("codex"));
    assert!(state.overlay_message.is_some());

    state.handle_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL))?;
    assert!(state.provider_filter.is_none());

    state.handle_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL))?;
    assert_eq!(state.provider_filter.as_deref(), Some("codex"));

    state.handle_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL))?;
    assert!(state.full_text);
    assert!(matches!(state.status_message(), Some(message) if message.contains("full-text")));

    Ok(())
}

#[cfg(unix)]
#[test]
fn filter_typing_supports_slash_and_uppercase() -> Result<()> {
    let temp = TempDir::new()?;
    let mut config = build_config(temp.path());
    let directories = build_directories(&temp);
    directories.ensure_all()?;
    let mut db = Database::open(&directories.data_dir.join("tx.sqlite3"))?;

    let mut ctx = UiContext {
        config: &config,
        directories: &directories,
        db: &mut db,
        prompt: None,
    };

    let mut state = AppState::new(&mut ctx)?;
    assert!(state.filter.is_empty());
    assert!(state.message.is_none());

    state.handle_key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE))?;
    assert_eq!(state.filter, "/");
    assert!(state.message.is_none());

    state.handle_key(KeyEvent::new(KeyCode::Char('R'), KeyModifiers::NONE))?;
    assert_eq!(state.filter, "/R");
    assert!(state.message.is_none());

    state.handle_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE))?;
    assert_eq!(state.filter, "/");
    state.handle_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE))?;
    assert!(state.filter.is_empty());
    assert!(state.message.is_none());
    Ok(())
}

#[cfg(unix)]
#[test]
fn preview_cache_invalidation_and_profile_plans() -> Result<()> {
    let temp = TempDir::new()?;
    let mut config = build_config(temp.path());
    let pre_snippet_name = "prepare";
    config.snippets.pre.insert(
        pre_snippet_name.into(),
        Snippet {
            name: pre_snippet_name.into(),
            command: "echo pre".into(),
        },
    );
    config.wrappers.insert(
        "shellwrap".into(),
        WrapperConfig {
            name: "shellwrap".into(),
            mode: WrapperMode::Shell {
                command: "echo '{{CMD}} --wrapped'".into(),
            },
        },
    );
    if let Some(default_profile) = config.profiles.get_mut("default") {
        default_profile.wrap = Some("shellwrap".into());
        default_profile.pre = vec![pre_snippet_name.into()];
    }

    let directories = build_directories(&temp);
    directories.ensure_all()?;
    let mut db = Database::open(&directories.data_dir.join("tx.sqlite3"))?;

    let mut ctx = UiContext {
        config: &config,
        directories: &directories,
        db: &mut db,
        prompt: None,
    };

    let mut state = AppState::new(&mut ctx)?;
    state.entries = vec![Entry::Empty(EmptyEntry {
        title: "Nothing here".into(),
        preview: vec!["No entries found".into()],
        status: Some("refresh to load".into()),
    })];
    state.index = 0;
    state.list_state.select(Some(0));

    let first_preview = state.preview();
    assert_eq!(first_preview.lines, vec!["No entries found".to_string()]);

    if let Some(cached) = state.preview_cache.get_mut("empty:Nothing here") {
        cached.lines = vec!["stale".into()];
        cached.updated_at = cached
            .updated_at
            .checked_sub(StdDuration::from_secs(10))
            .expect("cached preview timestamp should allow subtraction");
    }

    let refreshed_preview = state.preview();
    assert_eq!(
        refreshed_preview.lines,
        vec!["No entries found".to_string()]
    );

    let profile_entry = state
        .profiles
        .iter()
        .find(|entry| entry.display == "default")
        .cloned()
        .expect("default profile");
    let wrapped_plan = state.plan_for_profile(&profile_entry)?.expect("plan");
    assert_eq!(wrapped_plan.wrapper.as_deref(), Some("shellwrap"));
    if let Invocation::Shell { command } = &wrapped_plan.invocation {
        assert!(command.contains("--wrapped"));
    } else {
        panic!("expected shell invocation");
    }

    let provider_entry = state
        .profiles
        .iter()
        .find(|entry| matches!(entry.kind, ProfileKind::Provider))
        .cloned()
        .expect("provider entry");
    let provider_plan = state.plan_for_profile(&provider_entry)?.expect("plan");
    assert!(provider_plan.wrapper.is_none());

    Ok(())
}

#[test]
fn empty_entry_for_state_variants() -> Result<()> {
    let temp = TempDir::new()?;
    let mut config = build_config(temp.path());
    let directories = build_directories(&temp);
    directories.ensure_all()?;

    {
        let mut db = Database::open(&directories.data_dir.join("tx.sqlite3"))?;
        let mut ctx = UiContext {
            config: &config,
            directories: &directories,
            db: &mut db,
            prompt: None,
        };
        let entry = EmptyEntry::for_state(&ctx);
        assert_eq!(entry.title, "New session");
        assert!(
            entry
                .status
                .as_deref()
                .is_some_and(|status| status.contains("No sessions yet"))
        );
    }

    let mut config_without = config.clone();
    config_without.providers.clear();
    {
        let mut db = Database::open(&directories.data_dir.join("tx.sqlite3"))?;
        let mut ctx = UiContext {
            config: &config_without,
            directories: &directories,
            db: &mut db,
            prompt: None,
        };
        let entry = EmptyEntry::for_state(&ctx);
        assert_eq!(entry.title, "New session (configure tx)");
        assert!(
            entry
                .preview
                .iter()
                .any(|line| line.contains("Add a TOML file"))
        );
    }
    Ok(())
}

#[cfg(unix)]
#[test]
fn handle_key_normal_enter_executes_plan() -> Result<()> {
    let temp = TempDir::new()?;
    let mut config = build_config(temp.path());
    let directories = build_directories(&temp);
    directories.ensure_all()?;
    let mut db = Database::open(&directories.data_dir.join("tx.sqlite3"))?;
    let session_dir = config
        .providers
        .get("codex")
        .unwrap()
        .session_roots
        .first()
        .unwrap()
        .to_path_buf();
    fs::create_dir_all(&session_dir)?;
    let session_path = session_dir.join("enter.jsonl");
    fs::File::create(&session_path)?.write_all(b"{\"event\":\"enter\"}\n")?;
    insert_session(&mut db, &session_path, "sess-enter")?;
    let mut ctx = UiContext {
        config: &config,
        directories: &directories,
        db: &mut db,
        prompt: None,
    };
    let mut state = AppState::new(&mut ctx)?;
    if let Some((idx, Entry::Session(_))) = state
        .entries
        .iter()
        .enumerate()
        .find(|(_, entry)| matches!(entry, Entry::Session(_)))
    {
        state.index = idx;
        state.list_state.select(Some(idx));
        state.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
        assert!(matches!(state.outcome, Some(Outcome::Execute(_))));
    } else {
        panic!("expected session entry");
    }
    Ok(())
}

#[cfg(unix)]
#[test]
fn handle_key_normal_ctrl_tab_emits_plan() -> Result<()> {
    let temp = TempDir::new()?;
    let mut config = build_config(temp.path());
    let directories = build_directories(&temp);
    directories.ensure_all()?;
    let mut db = Database::open(&directories.data_dir.join("tx.sqlite3"))?;
    let session_dir = config
        .providers
        .get("codex")
        .unwrap()
        .session_roots
        .first()
        .unwrap()
        .to_path_buf();
    fs::create_dir_all(&session_dir)?;
    let session_path = session_dir.join("ctrl-tab.jsonl");
    fs::File::create(&session_path)?.write_all(b"{\"event\":\"ctrl_tab\"}\n")?;
    insert_session(&mut db, &session_path, "sess-ctrl-tab")?;
    let mut ctx = UiContext {
        config: &config,
        directories: &directories,
        db: &mut db,
        prompt: None,
    };
    let mut state = AppState::new(&mut ctx)?;
    if let Some((idx, Entry::Session(_))) = state
        .entries
        .iter()
        .enumerate()
        .find(|(_, entry)| matches!(entry, Entry::Session(_)))
    {
        state.index = idx;
        state.list_state.select(Some(idx));
        state.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::CONTROL))?;
        assert!(matches!(state.outcome, Some(Outcome::Emit(_))));
    } else {
        panic!("expected session entry");
    }
    Ok(())
}

#[test]
fn build_plan_empty_entry_sets_message() -> Result<()> {
    let temp = TempDir::new()?;
    let mut config = build_config(temp.path());
    let directories = build_directories(&temp);
    directories.ensure_all()?;
    let mut db = Database::open(&directories.data_dir.join("tx.sqlite3"))?;
    let mut ctx = UiContext {
        config: &config,
        directories: &directories,
        db: &mut db,
        prompt: None,
    };
    let mut state = AppState::new(&mut ctx)?;
    state.entries = vec![Entry::Empty(EmptyEntry {
        title: "Nothing".into(),
        preview: vec![],
        status: Some("Select a session".into()),
    })];
    state.index = 0;
    state.list_state.select(Some(0));
    assert!(state.build_plan()?.is_none());
    assert_eq!(state.message.as_deref(), Some("Select a session"));
    Ok(())
}

#[test]
fn build_plan_and_preview_handle_no_selection() -> Result<()> {
    let temp = TempDir::new()?;
    let config = build_config(temp.path());
    let directories = build_directories(&temp);
    directories.ensure_all()?;
    let mut db = Database::open(&directories.data_dir.join("tx.sqlite3"))?;
    let mut ctx = UiContext {
        config: &config,
        directories: &directories,
        db: &mut db,
        prompt: None,
    };
    let mut state = AppState::new(&mut ctx)?;
    state.entries.clear();
    state.index = 0;
    state.list_state.select(None);

    assert!(state.build_plan()?.is_none());

    let preview = state.preview();
    assert_eq!(preview.lines, vec!["No selection".to_string()]);
    assert_eq!(preview.title.as_deref(), Some("Preview"));
    Ok(())
}

#[test]
fn selection_fallbacks_show_status_when_no_session_selected() -> Result<()> {
    let temp = TempDir::new()?;
    let config = build_config(temp.path());
    let directories = build_directories(&temp);
    directories.ensure_all()?;
    let mut db = Database::open(&directories.data_dir.join("tx.sqlite3"))?;
    let mut ctx = UiContext {
        config: &config,
        directories: &directories,
        db: &mut db,
        prompt: None,
    };
    let mut state = AppState::new(&mut ctx)?;

    let profile = state
        .profiles
        .iter()
        .find(|entry| matches!(entry.kind, ProfileKind::Config { .. }))
        .cloned()
        .expect("config profile should exist");
    state.entries = vec![Entry::Profile(profile)];
    state.index = 0;
    state.list_state.select(Some(0));

    assert!(state.selected_session().is_none());
    state.trigger_print_session_id()?;
    assert_eq!(
        state.status_message().as_deref(),
        Some("Select a session to print its ID.")
    );

    state.trigger_export_markdown()?;
    assert_eq!(
        state.status_message().as_deref(),
        Some("Select a session to export its transcript.")
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn handle_key_normal_covers_backspace_toggle_and_default_branch() -> Result<()> {
    let temp = TempDir::new()?;
    let config = build_config(temp.path());
    let directories = build_directories(&temp);
    directories.ensure_all()?;
    let mut db = Database::open(&directories.data_dir.join("tx.sqlite3"))?;
    let mut ctx = UiContext {
        config: &config,
        directories: &directories,
        db: &mut db,
        prompt: None,
    };
    let mut state = AppState::new(&mut ctx)?;

    state.filter = "ab".into();
    assert!(!state.handle_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE))?);
    assert_eq!(state.filter, "a");

    state.full_text = true;
    assert!(!state.handle_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL))?);
    assert!(!state.full_text);
    assert_eq!(state.status_message().as_deref(), Some("search: prompt"));

    let filter_before = state.filter.clone();
    assert!(!state.handle_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::ALT))?);
    assert_eq!(state.filter, filter_before);

    state.entries.clear();
    state.move_selection(1);
    assert_eq!(state.index, 0);
    assert_eq!(state.list_state.selected(), None);
    Ok(())
}

#[cfg(unix)]
#[test]
fn backspace_with_filter_refreshes_entries_and_clears_preview_cache() -> Result<()> {
    let temp = TempDir::new()?;
    let mut config = build_config(temp.path());
    let directories = build_directories(&temp);
    directories.ensure_all()?;
    let mut db = Database::open(&directories.data_dir.join("tx.sqlite3"))?;
    let session_dir = config
        .providers
        .get("codex")
        .expect("provider")
        .session_roots
        .first()
        .expect("session root")
        .clone();
    fs::create_dir_all(&session_dir)?;
    let session_path = session_dir.join("backspace-refresh.jsonl");
    fs::File::create(&session_path)?.write_all(b"{\"event\":\"backspace\"}\n")?;
    insert_session(&mut db, &session_path, "sess-backspace")?;

    let mut ctx = UiContext {
        config: &config,
        directories: &directories,
        db: &mut db,
        prompt: None,
    };
    let mut state = AppState::new(&mut ctx)?;

    let _ = state.preview();
    assert!(!state.preview_cache.is_empty());
    state.filter = "x".into();
    state.handle_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE))?;
    assert!(state.filter.is_empty());
    assert!(!state.handle_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE))?);
    assert!(state.preview_cache.is_empty());
    Ok(())
}

#[test]
fn refresh_entries_clamps_selection_index() -> Result<()> {
    let temp = TempDir::new()?;
    let config = build_config(temp.path());
    let directories = build_directories(&temp);
    directories.ensure_all()?;
    let mut db = Database::open(&directories.data_dir.join("tx.sqlite3"))?;
    let mut ctx = UiContext {
        config: &config,
        directories: &directories,
        db: &mut db,
        prompt: None,
    };
    let mut state = AppState::new(&mut ctx)?;
    state.index = usize::MAX;

    state.refresh_entries()?;
    assert_eq!(state.index, state.entries.len().saturating_sub(1));
    Ok(())
}

#[cfg(unix)]
#[test]
fn search_full_text_dedupes_hits_and_backfills_snippet() -> Result<()> {
    let temp = TempDir::new()?;
    let config = build_config(temp.path());
    let directories = build_directories(&temp);
    directories.ensure_all()?;
    let mut db = Database::open(&directories.data_dir.join("tx.sqlite3"))?;

    let session_dir = config
        .providers
        .get("codex")
        .expect("provider")
        .session_roots
        .first()
        .expect("session root")
        .clone();
    fs::create_dir_all(&session_dir)?;
    let session_path = session_dir.join("fts-dedupe.jsonl");
    fs::File::create(&session_path)?.write_all(b"{\"event\":\"fts\"}\n")?;
    let mut summary = insert_session(&mut db, &session_path, "sess-fts-dedupe")?;
    summary.first_prompt = None;
    let now = OffsetDateTime::now_utc().unix_timestamp();
    let messages = vec![
        MessageRecord::new(
            summary.id.clone(),
            0,
            "assistant",
            "branchterm one",
            Some("event_msg_a".into()),
            Some(now),
        ),
        MessageRecord::new(
            summary.id.clone(),
            1,
            "user",
            "branchterm two",
            Some("event_msg_b".into()),
            Some(now + 1),
        ),
    ];
    db.upsert_session(&SessionIngest::new(summary, messages))?;

    let mut ctx = UiContext {
        config: &config,
        directories: &directories,
        db: &mut db,
        prompt: None,
    };
    let state = AppState::new(&mut ctx)?;
    let sessions = state.search_full_text_sessions("branchterm")?;

    assert_eq!(sessions.len(), 1);
    let session = sessions.first().expect("single deduped session");
    assert!(session.first_prompt.is_none());
    assert!(
        session
            .snippet
            .as_deref()
            .is_some_and(|snippet| snippet.contains("branchterm"))
    );
    assert!(session.snippet_role.is_some());
    Ok(())
}

#[cfg(unix)]
#[test]
fn preview_renders_session_with_filter_and_cache() -> Result<()> {
    let temp = TempDir::new()?;
    let mut config = build_config(temp.path());
    let directories = build_directories(&temp);
    directories.ensure_all()?;
    let mut db = Database::open(&directories.data_dir.join("tx.sqlite3"))?;
    let session_dir = config
        .providers
        .get("codex")
        .unwrap()
        .session_roots
        .first()
        .unwrap()
        .to_path_buf();
    fs::create_dir_all(&session_dir)?;
    let session_path = session_dir.join("preview.jsonl");
    fs::File::create(&session_path)?.write_all(b"{\"event\":\"preview\"}\n")?;
    insert_session(&mut db, &session_path, "sess-preview")?;

    let mut ctx = UiContext {
        config: &config,
        directories: &directories,
        db: &mut db,
        prompt: None,
    };
    let mut state = AppState::new(&mut ctx)?;
    let first = state.preview();
    assert!(!first.lines.is_empty());
    assert!(first.title.is_some());

    if let Some(preview) = state.preview_cache.values_mut().next() {
        preview.updated_at = Instant::now();
    }
    let cached = state.preview();
    assert!(!cached.lines.is_empty());
    Ok(())
}

#[cfg(unix)]
#[test]
fn preview_renders_markdown_without_filter() -> Result<()> {
    let temp = TempDir::new()?;
    let mut config = build_config(temp.path());
    let directories = build_directories(&temp);
    directories.ensure_all()?;
    let mut db = Database::open(&directories.data_dir.join("tx.sqlite3"))?;
    let session_dir = config
        .providers
        .get("codex")
        .unwrap()
        .session_roots
        .first()
        .unwrap()
        .to_path_buf();
    fs::create_dir_all(&session_dir)?;
    let session_path = session_dir.join("markdown.jsonl");
    fs::File::create(&session_path)?.write_all(b"{\"event\":\"preview\"}\n")?;
    insert_session(&mut db, &session_path, "sess-markdown")?;

    let mut ctx = UiContext {
        config: &config,
        directories: &directories,
        db: &mut db,
        prompt: None,
    };
    let mut state = AppState::new(&mut ctx)?;
    if let Some((idx, _)) = state
        .entries
        .iter()
        .enumerate()
        .find(|(_, entry)| matches!(entry, Entry::Session(_)))
    {
        state.index = idx;
        state.list_state.select(Some(idx));
    }
    let preview = state.preview();
    let joined = preview.lines.join("\n");
    assert!(
        joined.contains("**Provider**") || joined.contains("# Codex Session"),
        "expected markdown markers in preview lines: {joined:?}"
    );
    let styled = preview
        .styled
        .as_ref()
        .expect("expected styled markdown output without external helpers");
    let first_line = styled
        .lines
        .first()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .unwrap_or_default();
    assert!(
        !first_line.contains("**"),
        "expected markdown renderer to strip formatting markers, saw: {first_line:?}"
    );
    Ok(())
}

#[test]
fn dispatch_outcome_handles_emit_and_execute() -> Result<()> {
    let plan = PipelinePlan {
        pipeline: "echo hi".into(),
        display: "echo hi".into(),
        friendly_display: "echo hi".into(),
        env: Vec::new(),
        invocation: Invocation::Shell {
            command: "echo hi".into(),
        },
        provider: "codex".into(),
        terminal_title: "codex".into(),
        pre_snippets: Vec::new(),
        post_snippets: Vec::new(),
        wrapper: None,
        needs_stdin_prompt: false,
        uses_capture_arg: false,
        capture_has_pre_commands: false,
        stdin_prompt_label: None,
        cwd: std::env::current_dir()?,
        prompt_assembler: None,
    };

    dispatch_outcome(Some(Outcome::Emit(plan.clone())))?;

    let mut exec_plan = plan.clone();
    exec_plan.invocation = Invocation::Exec {
        argv: vec!["/bin/sh".into(), "-c".into(), "true".into()],
    };
    dispatch_outcome(Some(Outcome::Execute(exec_plan)))?;
    dispatch_outcome(Some(Outcome::PrintSessionId("session-123".into())))?;
    dispatch_outcome(Some(Outcome::ExportMarkdown(vec![
        "# heading".into(),
        "body".into(),
    ])))?;
    dispatch_outcome(None)?;
    Ok(())
}

#[cfg(unix)]
#[test]
fn reindex_updates_message_and_refreshes_entries() -> Result<()> {
    let temp = TempDir::new()?;
    let mut config = build_config(temp.path());
    let directories = build_directories(&temp);
    directories.ensure_all()?;

    let sessions_dir = config
        .providers
        .get("codex")
        .expect("provider present")
        .session_roots
        .first()
        .expect("session root")
        .clone();
    fs::create_dir_all(&sessions_dir)?;
    let session_path = sessions_dir.join("reindex.jsonl");
    fs::File::create(&session_path)?.write_all(b"{\"event\":\"index\"}\n")?;

    let mut db = Database::open(&directories.data_dir.join("tx.sqlite3"))?;
    insert_session(&mut db, &session_path, "sess-reindex")?;

    let mut ctx = UiContext {
        config: &config,
        directories: &directories,
        db: &mut db,
        prompt: None,
    };
    let mut state = AppState::new(&mut ctx)?;
    state.reindex()?;
    assert!(
        state
            .message
            .as_ref()
            .is_some_and(|msg| msg.contains("Indexed")),
        "expected reindex to emit status message"
    );
    Ok(())
}

#[test]
fn run_with_terminal_emits_plan() -> Result<()> {
    let temp = TempDir::new()?;
    let mut config = build_config(temp.path());
    let directories = build_directories(&temp);
    directories.ensure_all()?;
    let mut db = Database::open(&directories.data_dir.join("tx.sqlite3"))?;
    let mut ctx = UiContext {
        config: &config,
        directories: &directories,
        db: &mut db,
        prompt: None,
    };
    let mut terminal = Terminal::new(TestBackend::new(80, 24))?;
    let mut events = FakeEvents::new(vec![Event::Key(KeyEvent::new(
        KeyCode::Tab,
        KeyModifiers::NONE,
    ))]);
    let outcome = run_with_terminal(&mut ctx, &mut terminal, &mut events)?;
    match outcome {
        Some(Outcome::Emit(plan)) => assert!(!plan.display.is_empty()),
        other => panic!("expected emit outcome, got {other:?}"),
    }
    Ok(())
}

#[cfg(unix)]
#[test]
fn status_message_prefers_overlay_then_filter() -> Result<()> {
    let temp = TempDir::new()?;
    let mut config = build_config(temp.path());
    let directories = build_directories(&temp);
    directories.ensure_all()?;
    let mut db = Database::open(&directories.data_dir.join("tx.sqlite3"))?;
    let session_dir = config
        .providers
        .get("codex")
        .unwrap()
        .session_roots
        .first()
        .unwrap()
        .to_path_buf();
    fs::create_dir_all(&session_dir)?;
    let session_path = session_dir.join("overlay.jsonl");
    fs::File::create(&session_path)?.write_all(b"{\"event\":\"overlay\"}\n")?;
    insert_session(&mut db, &session_path, "sess-overlay")?;

    let mut ctx = UiContext {
        config: &config,
        directories: &directories,
        db: &mut db,
        prompt: None,
    };

    let mut state = AppState::new(&mut ctx)?;
    state.set_temporary_status_message("overlay".into(), StdDuration::from_secs(60));
    assert_eq!(state.status_message().as_deref(), Some("overlay"));

    let backend = TestBackend::new(40, 6);
    let mut terminal = Terminal::new(backend)?;
    terminal.draw(|frame| {
        let area = frame.area();
        draw_status(frame, area, &state);
    })?;

    state.overlay_message = Some((
        "expired".into(),
        Instant::now()
            .checked_sub(StdDuration::from_secs(1))
            .expect("now should be later than the duration"),
    ));
    state.expire_status_message();
    state.filter = "filter text".into();
    assert_eq!(state.status_message().as_deref(), Some("filter text"));

    state.filter.clear();
    state.message = Some("custom status".into());
    let backend = TestBackend::new(40, 6);
    let mut terminal = Terminal::new(backend)?;
    terminal.draw(|frame| {
        let area = frame.area();
        draw_status(frame, area, &state);
    })?;

    Ok(())
}

#[test]
fn cycle_provider_filter_wraps_and_resets() -> Result<()> {
    let temp = TempDir::new()?;
    let mut config = build_config(temp.path());
    config.providers.insert(
        "alpha".into(),
        ProviderConfig {
            name: "alpha".into(),
            bin: "echo".into(),
            flags: Vec::new(),
            env: Vec::new(),
            session_roots: vec![temp.child("alpha-sessions").path().to_path_buf()],
            stdin: None,
        },
    );

    let directories = build_directories(&temp);
    directories.ensure_all()?;
    let mut db = Database::open(&directories.data_dir.join("tx.sqlite3"))?;

    let mut ctx = UiContext {
        config: &config,
        directories: &directories,
        db: &mut db,
        prompt: None,
    };

    let mut state = AppState::new(&mut ctx)?;
    assert_eq!(
        state.provider_order,
        vec!["alpha".to_string(), "codex".to_string()]
    );
    assert!(state.provider_filter.is_none());

    state.cycle_provider_filter()?;
    assert_eq!(state.provider_filter.as_deref(), Some("alpha"));
    assert_eq!(state.status_message().as_deref(), Some("provider: alpha"));
    assert_eq!(state.status_banner(), "provider: alpha");

    state.cycle_provider_filter()?;
    assert_eq!(state.provider_filter.as_deref(), Some("codex"));
    assert_eq!(state.status_message().as_deref(), Some("provider: codex"));

    state.cycle_provider_filter()?;
    assert!(state.provider_filter.is_none());
    assert_eq!(state.status_message().as_deref(), Some("provider: all"));

    state.provider_filter = Some("ghost".into());
    state.cycle_provider_filter()?;
    assert_eq!(state.provider_filter.as_deref(), Some("alpha"));

    state.provider_order.clear();
    state.cycle_provider_filter()?;
    assert!(state.provider_filter.is_none());
    assert_eq!(state.status_message().as_deref(), Some("provider: all"));
    Ok(())
}

#[test]
fn status_message_handles_all_sources() -> Result<()> {
    let temp = TempDir::new()?;
    let mut config = build_config(temp.path());
    let directories = build_directories(&temp);
    directories.ensure_all()?;
    let mut db = Database::open(&directories.data_dir.join("tx.sqlite3"))?;

    let mut ctx = UiContext {
        config: &config,
        directories: &directories,
        db: &mut db,
        prompt: None,
    };

    let mut state = AppState::new(&mut ctx)?;

    state.set_temporary_status_message("overlay".into(), StdDuration::from_secs(60));
    assert_eq!(state.status_message().as_deref(), Some("overlay"));

    state.overlay_message = Some((
        "expired".into(),
        Instant::now()
            .checked_sub(StdDuration::from_secs(1))
            .expect("now should be later than the duration"),
    ));
    state.filter = "typing".into();
    assert_eq!(state.status_message().as_deref(), Some("typing"));
    assert_eq!(state.status_banner(), "typing");

    state.filter.clear();
    assert_eq!(state.status_message(), None);

    state.entries = vec![Entry::Empty(EmptyEntry {
        title: "empty".into(),
        preview: Vec::new(),
        status: Some("entry-status".into()),
    })];
    state.index = 0;
    assert_eq!(state.status_message().as_deref(), Some("entry-status"));

    state.entries = vec![Entry::Empty(EmptyEntry {
        title: "empty".into(),
        preview: Vec::new(),
        status: None,
    })];
    state.message = Some("manual message".into());
    assert_eq!(state.status_message().as_deref(), Some("manual message"));

    state.message = Some(MESSAGE_FILTER_MODE.to_string());
    state.filter = "residual".into();
    assert_eq!(state.status_message().as_deref(), Some("residual"));

    state.filter.clear();
    state.message = None;
    state.entries.clear();
    assert_eq!(state.status_message(), None);
    Ok(())
}

#[cfg(unix)]
#[test]
fn draw_preview_uses_cached_styled_content() -> Result<()> {
    let temp = TempDir::new()?;
    let mut config = build_config(temp.path());
    let directories = build_directories(&temp);
    directories.ensure_all()?;
    let mut db = Database::open(&directories.data_dir.join("tx.sqlite3"))?;
    let session_dir = config
        .providers
        .get("codex")
        .unwrap()
        .session_roots
        .first()
        .unwrap()
        .to_path_buf();
    fs::create_dir_all(&session_dir)?;
    let session_path = session_dir.join("preview.jsonl");
    fs::File::create(&session_path)?.write_all(b"{\"event\":\"preview\"}\n")?;
    insert_session(&mut db, &session_path, "sess-preview")?;

    let mut ctx = UiContext {
        config: &config,
        directories: &directories,
        db: &mut db,
        prompt: None,
    };

    let mut state = AppState::new(&mut ctx)?;
    let key = match &state.entries[state.index] {
        Entry::Session(session) => format!("session:{}", session.id),
        Entry::Profile(profile) => format!("profile:{}", profile.display),
        Entry::Empty(empty) => format!("empty:{}", empty.title),
    };
    state.preview_cache.insert(
        key,
        Preview {
            lines: vec!["cached".into()],
            styled: Some(Text::styled("cached", Style::default().fg(Color::Yellow))),
            title: Some("Cached Preview".into()),
            timestamp: Some("2025-10-26".into()),
            updated_at: Instant::now(),
        },
    );

    let backend = TestBackend::new(60, 10);
    let mut terminal = Terminal::new(backend)?;
    terminal.draw(|frame| {
        let area = frame.area();
        draw_preview(frame, area, &mut state);
    })?;

    if let Some(preview) = state.preview_cache.values_mut().next() {
        preview.updated_at = Instant::now()
            .checked_sub(StdDuration::from_secs(10))
            .expect("now should be later than the duration");
    }
    let backend = TestBackend::new(60, 10);
    let mut terminal = Terminal::new(backend)?;
    terminal.draw(|frame| {
        let area = frame.area();
        draw_preview(frame, area, &mut state);
    })?;
    Ok(())
}

#[cfg(unix)]
#[test]
fn draw_preview_renders_plain_lines_when_styled_is_missing() -> Result<()> {
    let temp = TempDir::new()?;
    let config = build_config(temp.path());
    let directories = build_directories(&temp);
    directories.ensure_all()?;
    let mut db = Database::open(&directories.data_dir.join("tx.sqlite3"))?;
    let mut ctx = UiContext {
        config: &config,
        directories: &directories,
        db: &mut db,
        prompt: None,
    };
    let mut state = AppState::new(&mut ctx)?;

    let key = match &state.entries[state.index] {
        Entry::Session(session) => format!("session:{}", session.id),
        Entry::Profile(profile) => format!("profile:{}", profile.display),
        Entry::Empty(empty) => format!("empty:{}", empty.title),
    };
    state.preview_cache.insert(
        key,
        Preview {
            lines: vec!["plain preview".into(), "no markdown styling".into()],
            styled: None,
            title: None,
            timestamp: None,
            updated_at: Instant::now(),
        },
    );

    let backend = TestBackend::new(60, 10);
    let mut terminal = Terminal::new(backend)?;
    terminal.draw(|frame| {
        let area = frame.area();
        draw_preview(frame, area, &mut state);
    })?;
    Ok(())
}

#[cfg(unix)]
#[test]
fn run_app_exits_on_escape_event() -> Result<()> {
    let temp = TempDir::new()?;
    let mut config = build_config(temp.path());
    let directories = build_directories(&temp);
    directories.ensure_all()?;
    let mut db = Database::open(&directories.data_dir.join("tx.sqlite3"))?;

    let mut ctx = UiContext {
        config: &config,
        directories: &directories,
        db: &mut db,
        prompt: None,
    };

    let mut events = FakeEvents::new(vec![Event::Key(KeyEvent::new(
        KeyCode::Esc,
        KeyModifiers::NONE,
    ))]);
    let backend = TestBackend::new(40, 10);
    let mut terminal = Terminal::new(backend)?;
    let result = run_app(&mut ctx, &mut terminal, &mut events)?;
    assert!(result.is_none());
    Ok(())
}

#[cfg(unix)]
#[test]
fn run_app_emits_plan_on_tab_event() -> Result<()> {
    let temp = TempDir::new()?;
    let mut config = build_config(temp.path());
    let directories = build_directories(&temp);
    directories.ensure_all()?;
    let mut db = Database::open(&directories.data_dir.join("tx.sqlite3"))?;
    let session_dir = config
        .providers
        .get("codex")
        .unwrap()
        .session_roots
        .first()
        .unwrap()
        .to_path_buf();
    fs::create_dir_all(&session_dir)?;
    let session_path = session_dir.join("planned.jsonl");
    fs::File::create(&session_path)?.write_all(b"{\"event\":\"plan\"}\n")?;
    insert_session(&mut db, &session_path, "sess-plan")?;

    let mut ctx = UiContext {
        config: &config,
        directories: &directories,
        db: &mut db,
        prompt: None,
    };

    let mut events = FakeEvents::new(vec![Event::Key(KeyEvent::new(
        KeyCode::Tab,
        KeyModifiers::NONE,
    ))]);
    let backend = TestBackend::new(60, 12);
    let mut terminal = Terminal::new(backend)?;
    let result = run_app(&mut ctx, &mut terminal, &mut events)?;
    assert!(matches!(result, Some(Outcome::Emit(_))));
    Ok(())
}

#[cfg(unix)]
#[test]
fn run_app_prints_session_id_on_ctrl_y() -> Result<()> {
    let temp = TempDir::new()?;
    let mut config = build_config(temp.path());
    let directories = build_directories(&temp);
    directories.ensure_all()?;
    let mut db = Database::open(&directories.data_dir.join("tx.sqlite3"))?;
    let session_dir = config
        .providers
        .get("codex")
        .unwrap()
        .session_roots
        .first()
        .unwrap()
        .to_path_buf();
    fs::create_dir_all(&session_dir)?;
    let session_path = session_dir.join("ctrl-i.jsonl");
    fs::File::create(&session_path)?.write_all(b"{\"event\":\"plan\"}\n")?;
    insert_session(&mut db, &session_path, "sess-ctrl-i")?;

    let mut ctx = UiContext {
        config: &config,
        directories: &directories,
        db: &mut db,
        prompt: None,
    };

    let mut state = AppState::new(&mut ctx)?;
    if let Some((idx, _)) = state
        .entries
        .iter()
        .enumerate()
        .find(|(_, entry)| matches!(entry, Entry::Session(_)))
    {
        state.index = idx;
        state.list_state.select(Some(idx));
    }
    state.handle_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::CONTROL))?;
    match state.outcome {
        Some(Outcome::PrintSessionId(session_id)) => {
            assert_eq!(session_id, "uuid-sess-ctrl-i");
        }
        other => panic!("expected PrintSessionId outcome, got {other:?}"),
    }
    Ok(())
}

#[cfg(unix)]
#[test]
fn run_app_prints_session_id_on_ctrl_y_without_uuid() -> Result<()> {
    let temp = TempDir::new()?;
    let mut config = build_config(temp.path());
    let directories = build_directories(&temp);
    directories.ensure_all()?;
    let mut db = Database::open(&directories.data_dir.join("tx.sqlite3"))?;
    let session_dir = config
        .providers
        .get("codex")
        .unwrap()
        .session_roots
        .first()
        .unwrap()
        .to_path_buf();
    fs::create_dir_all(&session_dir)?;
    let session_path = session_dir.join("ctrl-y.jsonl");
    fs::File::create(&session_path)?.write_all(b"{\"event\":\"plan\"}\n")?;
    insert_session_without_uuid(&mut db, &session_path, "sess-no-uuid")?;

    let mut ctx = UiContext {
        config: &config,
        directories: &directories,
        db: &mut db,
        prompt: None,
    };

    let mut state = AppState::new(&mut ctx)?;
    if let Some((idx, _)) = state
        .entries
        .iter()
        .enumerate()
        .find(|(_, entry)| matches!(entry, Entry::Session(_)))
    {
        state.index = idx;
        state.list_state.select(Some(idx));
    }
    state.handle_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::CONTROL))?;
    match state.outcome {
        Some(Outcome::PrintSessionId(session_id)) => {
            assert_eq!(session_id, "sess-no-uuid");
        }
        other => panic!("expected PrintSessionId outcome, got {other:?}"),
    }
    Ok(())
}

#[cfg(unix)]
#[test]
fn run_app_prints_session_id_on_ctrl_y_session_missing() -> Result<()> {
    let temp = TempDir::new()?;
    let mut config = build_config(temp.path());
    let directories = build_directories(&temp);
    directories.ensure_all()?;
    let mut db = Database::open(&directories.data_dir.join("tx.sqlite3"))?;
    let session_dir = config
        .providers
        .get("codex")
        .unwrap()
        .session_roots
        .first()
        .unwrap()
        .to_path_buf();
    fs::create_dir_all(&session_dir)?;
    let session_path = session_dir.join("ctrl-y-missing.jsonl");
    fs::File::create(&session_path)?.write_all(b"{\"event\":\"plan\"}\n")?;
    insert_session(&mut db, &session_path, "sess-missing")?;

    let mut ctx = UiContext {
        config: &config,
        directories: &directories,
        db: &mut db,
        prompt: None,
    };

    let mut state = AppState::new(&mut ctx)?;
    if let Some((idx, _)) = state
        .entries
        .iter()
        .enumerate()
        .find(|(_, entry)| matches!(entry, Entry::Session(_)))
    {
        state.index = idx;
        state.list_state.select(Some(idx));
    }
    state.ctx.db.delete_session("sess-missing")?;
    state.handle_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::CONTROL))?;
    assert!(state.outcome.is_none());
    assert_eq!(
        state.status_message().as_deref(),
        Some("Session not found; try refreshing.")
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn run_app_exports_transcript_missing_session() -> Result<()> {
    let temp = TempDir::new()?;
    let mut config = build_config(temp.path());
    let directories = build_directories(&temp);
    directories.ensure_all()?;
    let mut db = Database::open(&directories.data_dir.join("tx.sqlite3"))?;
    let session_dir = config
        .providers
        .get("codex")
        .unwrap()
        .session_roots
        .first()
        .unwrap()
        .to_path_buf();
    fs::create_dir_all(&session_dir)?;
    let session_path = session_dir.join("ctrl-e-missing.jsonl");
    fs::File::create(&session_path)?.write_all(b"{\"event\":\"export\"}\n")?;
    insert_session(&mut db, &session_path, "sess-export-missing")?;

    let mut ctx = UiContext {
        config: &config,
        directories: &directories,
        db: &mut db,
        prompt: None,
    };

    let mut state = AppState::new(&mut ctx)?;
    if let Some((idx, _)) = state
        .entries
        .iter()
        .enumerate()
        .find(|(_, entry)| matches!(entry, Entry::Session(_)))
    {
        state.index = idx;
        state.list_state.select(Some(idx));
    }
    state.ctx.db.delete_session("sess-export-missing")?;
    state.handle_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL))?;
    assert!(state.outcome.is_none());
    assert_eq!(
        state.status_message().as_deref(),
        Some("Transcript not available for export.")
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn run_app_exports_transcript_on_ctrl_e() -> Result<()> {
    let temp = TempDir::new()?;
    let mut config = build_config(temp.path());
    let directories = build_directories(&temp);
    directories.ensure_all()?;
    let mut db = Database::open(&directories.data_dir.join("tx.sqlite3"))?;
    let session_dir = config
        .providers
        .get("codex")
        .unwrap()
        .session_roots
        .first()
        .unwrap()
        .to_path_buf();
    fs::create_dir_all(&session_dir)?;
    let session_path = session_dir.join("ctrl-e.jsonl");
    fs::File::create(&session_path)?.write_all(b"{\"event\":\"export\"}\n")?;
    insert_session(&mut db, &session_path, "sess-ctrl-e")?;

    let mut ctx = UiContext {
        config: &config,
        directories: &directories,
        db: &mut db,
        prompt: None,
    };

    let mut state = AppState::new(&mut ctx)?;
    if let Some((idx, _)) = state
        .entries
        .iter()
        .enumerate()
        .find(|(_, entry)| matches!(entry, Entry::Session(_)))
    {
        state.index = idx;
        state.list_state.select(Some(idx));
    }
    state.handle_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL))?;
    match state.outcome {
        Some(Outcome::ExportMarkdown(lines)) => {
            assert!(
                lines.iter().any(|line| line.contains("Hello there")),
                "export should include transcript content"
            );
            assert_eq!(
                lines.first().map(String::as_str),
                Some("# Codex Session uuid-sess-ctrl-e")
            );
        }
        other => panic!("expected ExportMarkdown outcome, got {other:?}"),
    }
    Ok(())
}

#[cfg(unix)]
#[test]
#[allow(clippy::too_many_lines)]
fn load_profiles_handles_prompt_statuses() -> Result<()> {
    let temp = TempDir::new()?;
    let mut config = build_config(temp.path());
    config.profiles.insert(
        "ALPHA".into(),
        ProfileConfig {
            name: "ALPHA".into(),
            provider: "codex".into(),
            description: None,
            pre: Vec::new(),
            post: Vec::new(),
            wrap: None,
            prompt_assembler: None,
            prompt_assembler_args: Vec::new(),
        },
    );
    config.profiles.insert(
        "alpha".into(),
        ProfileConfig {
            name: "alpha".into(),
            provider: "codex".into(),
            description: None,
            pre: Vec::new(),
            post: Vec::new(),
            wrap: None,
            prompt_assembler: None,
            prompt_assembler_args: Vec::new(),
        },
    );

    let alpha_root = temp.path().join("alpha_sessions");
    fs::create_dir_all(&alpha_root)?;
    config.providers.insert(
        "alpha".into(),
        ProviderConfig {
            name: "alpha".into(),
            bin: "echo".into(),
            flags: vec!["--alpha".into()],
            env: Vec::new(),
            session_roots: vec![alpha_root],
            stdin: None,
        },
    );

    let directories = build_directories(&temp);
    directories.ensure_all()?;
    let mut db = Database::open(&directories.data_dir.join("tx.sqlite3"))?;
    let session_dir = config
        .providers
        .get("codex")
        .unwrap()
        .session_roots
        .first()
        .unwrap()
        .to_path_buf();
    fs::create_dir_all(&session_dir)?;
    let session_path = session_dir.join("profile.jsonl");
    fs::File::create(&session_path)?.write_all(b"{\"event\":\"profile\"}\n")?;
    insert_session(&mut db, &session_path, "sess-profile")?;

    let script = temp.child("pa");
    script.write_str(
        "#!/bin/sh\nif [ \"$1\" = \"list\" ] && [ \"$2\" = \"--json\" ]; then\n  echo '[{\"name\":\"virtual\",\"description\":\"Virtual entry\",\"stdin_supported\":true}]'\nelif [ \"$1\" = \"show\" ] && [ \"$2\" = \"--json\" ]; then\n  echo '{\"profile\":{\"content\":\"Virtual content\"}}'\nelse\n  exit 1\nfi\n",
    )?;
    #[cfg(unix)]
    {
        let perms = fs::Permissions::from_mode(0o755);
        fs::set_permissions(script.path(), perms.clone())?;
    }

    let _guard = PathGuard::push(&temp);
    let mut prompt_assembler = PromptAssembler::new(PromptAssemblerConfig {
        namespace: "tests".into(),
    });

    let mut ctx = UiContext {
        config: &config,
        directories: &directories,
        db: &mut db,
        prompt: Some(&mut prompt_assembler),
    };

    let mut state = AppState::new(&mut ctx)?;
    let alpha_count = state
        .profiles
        .iter()
        .filter(|entry| entry.display.eq_ignore_ascii_case("alpha"))
        .count();
    assert_eq!(alpha_count, 1);
    assert!(
        state
            .profiles
            .iter()
            .any(|entry| matches!(entry.kind, ProfileKind::Virtual))
    );

    script.write_str("#!/bin/sh\nexit 1\n")?;
    #[cfg(unix)]
    {
        let perms = fs::Permissions::from_mode(0o755);
        fs::set_permissions(script.path(), perms)?;
    }

    state.load_profiles();
    assert!(
        state
            .message
            .as_deref()
            .is_some_and(|msg| msg.contains("prompt assembler unavailable"))
    );

    Ok(())
}

#[cfg(unix)]
#[test]
#[allow(clippy::too_many_lines)]
fn load_profiles_covers_conflicts_and_missing_prompt_message() -> Result<()> {
    let temp = TempDir::new()?;
    let mut config = build_config(temp.path());
    config.features.prompt_assembler = Some(PromptAssemblerConfig {
        namespace: "tests".into(),
    });
    config.profiles.insert(
        "ALPHA".into(),
        ProfileConfig {
            name: "ALPHA".into(),
            provider: "codex".into(),
            description: None,
            pre: Vec::new(),
            post: Vec::new(),
            wrap: None,
            prompt_assembler: None,
            prompt_assembler_args: Vec::new(),
        },
    );
    config.profiles.insert(
        "alpha".into(),
        ProfileConfig {
            name: "alpha".into(),
            provider: "codex".into(),
            description: None,
            pre: Vec::new(),
            post: Vec::new(),
            wrap: None,
            prompt_assembler: None,
            prompt_assembler_args: Vec::new(),
        },
    );
    config.profiles.insert(
        "tests/demo".into(),
        ProfileConfig {
            name: "tests/demo".into(),
            provider: "codex".into(),
            description: Some("conflicting key".into()),
            pre: Vec::new(),
            post: Vec::new(),
            wrap: None,
            prompt_assembler: None,
            prompt_assembler_args: Vec::new(),
        },
    );
    config.profiles.insert(
        "missing-prompt".into(),
        ProfileConfig {
            name: "missing-prompt".into(),
            provider: "codex".into(),
            description: None,
            pre: Vec::new(),
            post: Vec::new(),
            wrap: None,
            prompt_assembler: Some("does-not-exist".into()),
            prompt_assembler_args: Vec::new(),
        },
    );

    config.providers.insert(
        "alpha".into(),
        ProviderConfig {
            name: "alpha".into(),
            bin: "echo".into(),
            flags: vec!["--alpha".into()],
            env: Vec::new(),
            session_roots: vec![temp.path().join("alpha-sessions")],
            stdin: None,
        },
    );

    let script = temp.child("pa");
    script.write_str(
        "#!/bin/sh\nif [ \"$1\" = \"list\" ] && [ \"$2\" = \"--json\" ]; then\n  echo '[{\"name\":\"demo\"}]'\nelif [ \"$1\" = \"show\" ] && [ \"$2\" = \"--json\" ] && [ \"$3\" = \"demo\" ]; then\n  echo '{\"profile\":{\"content\":\"Virtual content\"}}'\nelse\n  exit 1\nfi\n",
    )?;
    let perms = fs::Permissions::from_mode(0o755);
    fs::set_permissions(script.path(), perms)?;

    let _guard = PathGuard::push(&temp);
    let mut prompt_assembler = PromptAssembler::new(PromptAssemblerConfig {
        namespace: "tests".into(),
    });
    let directories = build_directories(&temp);
    directories.ensure_all()?;
    let mut db = Database::open(&directories.data_dir.join("tx.sqlite3"))?;
    let mut ctx = UiContext {
        config: &config,
        directories: &directories,
        db: &mut db,
        prompt: Some(&mut prompt_assembler),
    };

    let state = AppState::new(&mut ctx)?;
    assert!(state.message.as_deref().is_some_and(|message| {
        message.contains("prompt assembler prompt 'does-not-exist' referenced by profile")
    }));
    let alpha_entries = state
        .profiles
        .iter()
        .filter(|entry| entry.display.eq_ignore_ascii_case("alpha"))
        .count();
    assert_eq!(alpha_entries, 1);
    assert!(
        !state.profiles.iter().any(
            |entry| matches!(entry.kind, ProfileKind::Virtual) && entry.display == "tests/demo"
        )
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn load_profiles_sets_message_when_virtual_profiles_have_no_provider() -> Result<()> {
    let temp = TempDir::new()?;
    let mut config = build_config(temp.path());
    config.providers.clear();
    config.defaults.provider = None;
    config.features.prompt_assembler = Some(PromptAssemblerConfig {
        namespace: "tests".into(),
    });

    let script = temp.child("pa");
    script.write_str(
        "#!/bin/sh\nif [ \"$1\" = \"list\" ] && [ \"$2\" = \"--json\" ]; then\n  echo '[{\"name\":\"demo\"}]'\nelif [ \"$1\" = \"show\" ] && [ \"$2\" = \"--json\" ] && [ \"$3\" = \"demo\" ]; then\n  echo '{\"profile\":{\"content\":\"Virtual content\"}}'\nelse\n  exit 1\nfi\n",
    )?;
    let perms = fs::Permissions::from_mode(0o755);
    fs::set_permissions(script.path(), perms)?;

    let _guard = PathGuard::push(&temp);
    let mut prompt_assembler = PromptAssembler::new(PromptAssemblerConfig {
        namespace: "tests".into(),
    });
    let directories = build_directories(&temp);
    directories.ensure_all()?;
    let mut db = Database::open(&directories.data_dir.join("tx.sqlite3"))?;
    let mut ctx = UiContext {
        config: &config,
        directories: &directories,
        db: &mut db,
        prompt: Some(&mut prompt_assembler),
    };

    let state = AppState::new(&mut ctx)?;
    assert_eq!(
        state.message.as_deref(),
        Some("prompt profiles unavailable: no providers configured")
    );
    Ok(())
}

#[test]
fn refresh_entries_preserves_config_profile_order() -> Result<()> {
    let temp = TempDir::new()?;
    let mut config = build_config(temp.path());
    config.profiles.clear();
    config.defaults.profile = Some("gamma".into());

    let make_profile = |name: &str| ProfileConfig {
        name: name.into(),
        provider: "codex".into(),
        description: None,
        pre: Vec::new(),
        post: Vec::new(),
        wrap: None,
        prompt_assembler: None,
        prompt_assembler_args: Vec::new(),
    };

    config
        .profiles
        .insert("gamma".into(), make_profile("gamma"));
    config
        .profiles
        .insert("alpha".into(), make_profile("alpha"));
    config.profiles.insert("beta".into(), make_profile("beta"));

    let directories = build_directories(&temp);
    directories.ensure_all()?;
    let mut db = Database::open(&directories.data_dir.join("tx.sqlite3"))?;

    let mut ctx = UiContext {
        config: &config,
        directories: &directories,
        db: &mut db,
        prompt: None,
    };

    let state = AppState::new(&mut ctx)?;
    let profile_names: Vec<String> = state
        .entries
        .iter()
        .filter_map(|entry| match entry {
            Entry::Profile(profile) => Some(profile.display.clone()),
            _ => None,
        })
        .collect();

    assert_eq!(
        profile_names,
        vec![
            "gamma".to_string(),
            "alpha".to_string(),
            "beta".to_string(),
            "codex".to_string(),
        ]
    );

    Ok(())
}

#[test]
fn preview_handles_missing_and_profile_entries() -> Result<()> {
    let temp = TempDir::new()?;
    let mut config = build_config(temp.path());
    let directories = build_directories(&temp);
    directories.ensure_all()?;
    let mut db = Database::open(&directories.data_dir.join("tx.sqlite3"))?;
    let session_dir = config
        .providers
        .get("codex")
        .unwrap()
        .session_roots
        .first()
        .unwrap()
        .to_path_buf();
    fs::create_dir_all(&session_dir)?;
    let session_path = session_dir.join("preview.jsonl");
    fs::File::create(&session_path)?.write_all(b"{\"event\":\"preview\"}\n")?;
    insert_session(&mut db, &session_path, "sess-preview")?;

    let mut ctx = UiContext {
        config: &config,
        directories: &directories,
        db: &mut db,
        prompt: None,
    };

    let mut state = AppState::new(&mut ctx)?;
    state.preview_cache.clear();
    state.entries = vec![Entry::Session(SessionEntry {
        id: "missing".into(),
        provider: "codex".into(),
        wrapper: None,
        label: Some("missing".into()),
        first_prompt: None,
        actionable: true,
        last_active: Some(0),
        snippet: None,
        snippet_role: None,
    })];
    state.index = 0;
    let missing_preview = state.preview();
    assert_eq!(
        missing_preview.lines,
        vec![String::from("No transcript available")]
    );

    state.preview_cache.clear();
    if let Some(profile) = state
        .profiles
        .iter()
        .find(|entry| matches!(entry.kind, ProfileKind::Config { .. }))
        .cloned()
    {
        let mut profile_entry = profile;
        profile_entry.tags = vec!["alpha".into(), "beta".into()];
        state.entries = vec![Entry::Profile(profile_entry)];
        let profile_preview = state.preview();
        assert!(
            profile_preview
                .lines
                .iter()
                .any(|line| line.contains("Tags: alpha, beta"))
        );
    }

    state.entries = vec![Entry::Empty(EmptyEntry {
        title: "Empty".into(),
        preview: vec![String::from("Nothing to show")],
        status: None,
    })];
    let empty_preview = state.preview();
    assert_eq!(empty_preview.lines, vec![String::from("Nothing to show")]);

    Ok(())
}

#[test]
fn preview_prefers_virtual_profile_contents() -> Result<()> {
    let temp = TempDir::new()?;
    let mut config = build_config(temp.path());
    let directories = build_directories(&temp);
    directories.ensure_all()?;
    let mut db = Database::open(&directories.data_dir.join("tx.sqlite3"))?;

    let mut ctx = UiContext {
        config: &config,
        directories: &directories,
        db: &mut db,
        prompt: None,
    };

    let mut state = AppState::new(&mut ctx)?;
    state.preview_cache.clear();
    state.entries = vec![Entry::Profile(ProfileEntry {
        display: "pa/demo".into(),
        provider: "codex".into(),
        pre: Vec::new(),
        post: Vec::new(),
        wrap: None,
        description: Some("Original description".into()),
        tags: vec!["team".into()],
        inline_pre: Vec::new(),
        stdin_supported: true,
        prompt_assembler: Some("demo".into()),
        prompt_assembler_args: Vec::new(),
        prompt_available: true,
        kind: ProfileKind::Virtual,
        preview_lines: vec!["Instruction 1".into(), "Instruction 2".into()],
    })];
    state.index = 0;
    state.list_state.select(Some(0));

    let preview = state.preview();
    assert_eq!(
        preview.lines,
        vec![
            "**Provider**: `codex`".to_string(),
            "**Description**: Original description".to_string(),
            "**Tags**: `team`".to_string(),
            String::new(),
            "```markdown".to_string(),
            "Instruction 1".to_string(),
            "Instruction 2".to_string(),
            "```".to_string()
        ]
    );

    Ok(())
}

#[test]
fn provider_profile_preview_formats_command() -> Result<()> {
    let temp = TempDir::new()?;
    let mut config = build_config(temp.path());
    let directories = build_directories(&temp);
    directories.ensure_all()?;
    let mut db = Database::open(&directories.data_dir.join("tx.sqlite3"))?;

    let mut ctx = UiContext {
        config: &config,
        directories: &directories,
        db: &mut db,
        prompt: None,
    };

    let mut state = AppState::new(&mut ctx)?;
    let provider_entry = state
        .profiles
        .iter()
        .find(|entry| matches!(entry.kind, ProfileKind::Provider))
        .cloned()
        .expect("provider profile");

    let description = provider_entry
        .description
        .clone()
        .expect("provider description");
    let command_line = provider_entry
        .preview_lines
        .first()
        .cloned()
        .expect("preview line");

    state.entries = vec![Entry::Profile(provider_entry.clone())];
    state.index = 0;
    state.list_state.select(Some(0));
    state.preview_cache.clear();

    let preview = state.preview();
    assert_eq!(
        preview.lines,
        vec![
            format!("**Provider**: `{}`", provider_entry.provider),
            format!("**Description**: {description}"),
            "**Tags**: _None_".to_string(),
            String::new(),
            "```markdown".to_string(),
            command_line,
            "```".to_string()
        ]
    );

    Ok(())
}

#[test]
fn build_markdown_profile_preview_handles_multiline_and_empty_description() {
    let multiline = ProfileEntry {
        display: "demo".into(),
        provider: "codex".into(),
        pre: Vec::new(),
        post: Vec::new(),
        wrap: None,
        description: Some("First line\nSecond line".into()),
        tags: Vec::new(),
        inline_pre: Vec::new(),
        stdin_supported: false,
        prompt_assembler: None,
        prompt_assembler_args: Vec::new(),
        prompt_available: true,
        kind: ProfileKind::Virtual,
        preview_lines: Vec::new(),
    };
    let multiline_lines = build_markdown_profile_preview(&multiline);
    assert_eq!(multiline_lines[1], "**Description**: First line");
    assert!(multiline_lines.iter().any(|line| line == "Second line"));
    assert!(
        multiline_lines
            .iter()
            .any(|line| line == "_No prompt output_")
    );

    let empty_description = ProfileEntry {
        description: Some("   ".into()),
        ..multiline
    };
    let empty_lines = build_markdown_profile_preview(&empty_description);
    assert!(
        empty_lines
            .iter()
            .any(|line| line == "**Description**: _None_")
    );
}

#[cfg(unix)]
#[test]
fn plan_for_session_builds_resume_plan() -> Result<()> {
    let temp = TempDir::new()?;
    let mut config = build_config(temp.path());
    let directories = build_directories(&temp);
    directories.ensure_all()?;
    let mut db = Database::open(&directories.data_dir.join("tx.sqlite3"))?;
    let session_dir = config
        .providers
        .get("codex")
        .unwrap()
        .session_roots
        .first()
        .unwrap()
        .to_path_buf();
    fs::create_dir_all(&session_dir)?;
    let session_path = session_dir.join("resume.jsonl");
    fs::File::create(&session_path)?.write_all(b"{\"event\":\"resume\"}\n")?;
    insert_session(&mut db, &session_path, "sess-resume")?;

    let mut ctx = UiContext {
        config: &config,
        directories: &directories,
        db: &mut db,
        prompt: None,
    };
    let mut state = AppState::new(&mut ctx)?;
    let session_entry = state
        .entries
        .iter()
        .find_map(|entry| match entry {
            Entry::Session(session) => Some(session.clone()),
            _ => None,
        })
        .expect("session entry");
    let plan = state.plan_for_session(&session_entry)?;
    assert!(plan.is_some());
    Ok(())
}

#[cfg(unix)]
#[test]
fn plan_for_session_uses_current_dir_when_session_path_has_no_parent() -> Result<()> {
    let temp = TempDir::new()?;
    let config = build_config(temp.path());
    let directories = build_directories(&temp);
    directories.ensure_all()?;
    let mut db = Database::open(&directories.data_dir.join("tx.sqlite3"))?;
    insert_session(&mut db, Path::new("/"), "sess-root-path")?;

    let mut ctx = UiContext {
        config: &config,
        directories: &directories,
        db: &mut db,
        prompt: None,
    };
    let mut state = AppState::new(&mut ctx)?;
    let session_entry = state
        .entries
        .iter()
        .find_map(|entry| match entry {
            Entry::Session(session) if session.id == "sess-root-path" => Some(session.clone()),
            _ => None,
        })
        .expect("session entry");
    let plan = state
        .plan_for_session(&session_entry)?
        .expect("resume plan");
    assert!(plan.pipeline.contains("resume"));
    Ok(())
}

#[test]
fn plan_for_profile_marks_stdin_prompt() -> Result<()> {
    let temp = TempDir::new()?;
    let mut config = build_config(temp.path());
    let directories = build_directories(&temp);
    directories.ensure_all()?;
    let mut db = Database::open(&directories.data_dir.join("tx.sqlite3"))?;
    let mut ctx = UiContext {
        config: &config,
        directories: &directories,
        db: &mut db,
        prompt: None,
    };
    let mut state = AppState::new(&mut ctx)?;
    if let Some(profile) = state
        .profiles
        .iter()
        .find(|entry| matches!(entry.kind, ProfileKind::Config { .. }))
        .cloned()
    {
        let mut profile_entry = profile;
        profile_entry.stdin_supported = true;
        let plan = state.plan_for_profile(&profile_entry)?.expect("plan");
        assert!(plan.needs_stdin_prompt);
    }
    Ok(())
}

#[cfg(unix)]
#[test]
fn plan_for_virtual_profile_uses_virtual_request_fields() -> Result<()> {
    let temp = TempDir::new()?;
    let mut config = build_config(temp.path());
    config.snippets.pre.insert(
        "prepare".into(),
        Snippet {
            name: "prepare".into(),
            command: "printf PRE".into(),
        },
    );
    config.snippets.post.insert(
        "finish".into(),
        Snippet {
            name: "finish".into(),
            command: "cat".into(),
        },
    );

    let directories = build_directories(&temp);
    directories.ensure_all()?;
    let mut db = Database::open(&directories.data_dir.join("tx.sqlite3"))?;

    let mut ctx = UiContext {
        config: &config,
        directories: &directories,
        db: &mut db,
        prompt: None,
    };
    let mut state = AppState::new(&mut ctx)?;
    let profile = ProfileEntry {
        display: "virtual/demo".into(),
        provider: "codex".into(),
        pre: vec!["prepare".into()],
        post: vec!["finish".into()],
        wrap: None,
        description: Some("Virtual profile".into()),
        tags: vec!["demo".into()],
        inline_pre: Vec::new(),
        stdin_supported: false,
        prompt_assembler: None,
        prompt_assembler_args: Vec::new(),
        prompt_available: true,
        kind: ProfileKind::Virtual,
        preview_lines: vec!["Use this prompt".into()],
    };

    let plan = state.plan_for_profile(&profile)?.expect("plan");
    assert!(plan.pipeline.contains("printf PRE"));
    assert!(plan.pipeline.contains("echo hello"));
    assert!(plan.pipeline.contains("cat"));
    Ok(())
}

#[cfg(unix)]
#[test]
fn plan_for_profile_injects_prompt_assembler() -> Result<()> {
    let temp = TempDir::new()?;
    let mut config = build_config(temp.path());
    let directories = build_directories(&temp);
    directories.ensure_all()?;
    let mut db = Database::open(&directories.data_dir.join("tx.sqlite3"))?;

    let pa_script = temp.child("pa");
    pa_script.write_str(
        "#!/bin/sh\nif [ \"$1\" = \"list\" ] && [ \"$2\" = \"--json\" ]; then\n  echo '[{\"name\":\"demo\",\"description\":\"Demo prompt\",\"stdin_supported\":true}]'\nelif [ \"$1\" = \"show\" ] && [ \"$2\" = \"--json\" ]; then\n  if [ \"$3\" = \"demo\" ]; then\n    echo '{\"profile\":{\"content\":\"Instruction\"}}'\n  else\n    exit 1\n  fi\nelse\n  exit 1\nfi\n",
    )?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o755);
        std::fs::set_permissions(pa_script.path(), perms)?;
    }

    let _path_guard = PathGuard::push(&temp);

    config.features.prompt_assembler = Some(PromptAssemblerConfig {
        namespace: "pa".into(),
    });
    let mut prompt_assembler =
        PromptAssembler::new(config.features.prompt_assembler.clone().unwrap());

    let provider = config
        .providers
        .get_mut("codex")
        .expect("codex provider to exist");
    provider.stdin = Some(StdinMapping {
        args: vec!["{prompt}".into()],
        mode: StdinMode::CaptureArg,
    });

    let wrapper = WrapperConfig {
        name: "troubleshooting".into(),
        mode: WrapperMode::Shell {
            command: "tmux new-session -s demo '{{CMD}}'".into(),
        },
    };
    config
        .wrappers
        .insert(wrapper.name.clone(), wrapper.clone());

    config.profiles.insert(
        "troubleshooting".into(),
        ProfileConfig {
            name: "troubleshooting".into(),
            provider: "codex".into(),
            description: Some("Troubleshooting run".into()),
            pre: Vec::new(),
            post: Vec::new(),
            wrap: Some(wrapper.name.clone()),
            prompt_assembler: Some("demo".into()),
            prompt_assembler_args: vec!["--limit".into(), "5".into()],
        },
    );

    let mut ctx = UiContext {
        config: &config,
        directories: &directories,
        db: &mut db,
        prompt: Some(&mut prompt_assembler),
    };
    let mut state = AppState::new(&mut ctx)?;

    let profile = state
        .profiles
        .iter()
        .find(|entry| entry.display == "troubleshooting")
        .cloned()
        .expect("profile to be present");
    assert!(profile.inline_pre.is_empty());
    assert_eq!(
        profile.prompt_assembler_args,
        vec!["--limit".to_string(), "5".to_string()]
    );

    let plan = state.plan_for_profile(&profile)?.expect("plan");
    let prompt = plan
        .prompt_assembler
        .as_ref()
        .expect("plan should include prompt invocation");
    assert_eq!(prompt.name, "demo");
    assert_eq!(prompt.args, vec!["--limit".to_string(), "5".to_string()]);
    assert!(
        !plan.pipeline.contains("internal prompt-assembler"),
        "pipeline should not embed prompt helper: {}",
        plan.pipeline
    );
    assert!(
        plan.pipeline.contains("internal capture-arg"),
        "pipeline should capture prompt: {}",
        plan.pipeline
    );
    assert_eq!(plan.wrapper.as_deref(), Some(wrapper.name.as_str()));
    Ok(())
}

#[test]
fn plan_for_profile_errors_when_prompt_missing() -> Result<()> {
    let temp = TempDir::new()?;
    let mut config = build_config(temp.path());
    config.profiles.insert(
        "missing".into(),
        ProfileConfig {
            name: "missing".into(),
            provider: "codex".into(),
            description: None,
            pre: Vec::new(),
            post: Vec::new(),
            wrap: None,
            prompt_assembler: Some("absent".into()),
            prompt_assembler_args: Vec::new(),
        },
    );

    let directories = build_directories(&temp);
    directories.ensure_all()?;
    let mut db = Database::open(&directories.data_dir.join("tx.sqlite3"))?;

    let mut ctx = UiContext {
        config: &config,
        directories: &directories,
        db: &mut db,
        prompt: None,
    };
    let mut state = AppState::new(&mut ctx)?;

    let profile = state
        .profiles
        .iter()
        .find(|entry| entry.display == "missing")
        .cloned()
        .expect("profile to exist");
    let err = state.plan_for_profile(&profile).unwrap_err();
    assert!(err.to_string().contains("unknown prompt"));
    Ok(())
}

#[cfg(unix)]
#[test]
fn typing_uppercase_r_in_normal_mode_updates_filter() -> Result<()> {
    let temp = TempDir::new()?;
    let mut config = build_config(temp.path());
    let directories = build_directories(&temp);
    directories.ensure_all()?;
    let mut db = Database::open(&directories.data_dir.join("tx.sqlite3"))?;
    let mut ctx = UiContext {
        config: &config,
        directories: &directories,
        db: &mut db,
        prompt: None,
    };
    let mut state = AppState::new(&mut ctx)?;
    assert!(state.filter.is_empty());
    state.handle_key(KeyEvent::new(KeyCode::Char('R'), KeyModifiers::NONE))?;
    assert_eq!(state.filter, "R");
    assert!(state.message.is_none());
    Ok(())
}

#[cfg(unix)]
#[test]
fn tui_draw_snapshots() -> Result<()> {
    let temp = TempDir::new()?;
    let mut config = build_config(temp.path());
    let directories = build_directories(&temp);
    directories.ensure_all()?;
    let mut db = Database::open(&directories.data_dir.join("tx.sqlite3"))?;

    let mut ctx = UiContext {
        config: &config,
        directories: &directories,
        db: &mut db,
        prompt: None,
    };

    let mut state = AppState::new(&mut ctx)?;
    state.entries = vec![
        Entry::Empty(EmptyEntry {
            title: "No Sessions".into(),
            preview: vec!["Use tx search to populate sessions.".into()],
            status: Some("Run tx index to refresh listings.".into()),
        }),
        Entry::Profile(ProfileEntry {
            display: "Wrapped Profile".into(),
            provider: "codex".into(),
            pre: vec!["echo prepare".into()],
            post: vec!["echo cleanup".into()],
            wrap: Some("shellwrap".into()),
            description: Some("Run codex with helpers".into()),
            tags: vec!["team".into(), "demo".into()],
            inline_pre: Vec::new(),
            stdin_supported: true,
            prompt_assembler: Some("demo".into()),
            prompt_assembler_args: Vec::new(),
            prompt_available: true,
            kind: ProfileKind::Config {
                name: "wrapped".into(),
            },
            preview_lines: Vec::new(),
        }),
    ];
    state.index = 0;
    state.list_state.select(Some(0));
    state.preview_cache.clear();

    let empty_snapshot = render_to_string(&mut state, 60, 16)?;
    insta::assert_snapshot!("tui_empty_entry_render", empty_snapshot);

    state.index = 1;
    state.list_state.select(Some(1));
    state.preview_cache.clear();
    let profile_snapshot = render_to_string(&mut state, 60, 16)?;
    insta::assert_snapshot!("tui_profile_entry_render", profile_snapshot);

    state.overlay_message = Some((
        "provider: codex".into(),
        Instant::now() + StdDuration::from_secs(60),
    ));
    let overlay_snapshot = render_to_string(&mut state, 60, 10)?;
    insta::assert_snapshot!("tui_status_overlay_render", overlay_snapshot);

    state.overlay_message = None;
    state.filter = "codex".into();
    state.message = Some("doctor: missing CODEX_TOKEN".into());
    let filter_snapshot = render_to_string(&mut state, 60, 10)?;
    insta::assert_snapshot!("tui_filter_status_render", filter_snapshot);
    Ok(())
}

#[cfg(unix)]
#[test]
fn tui_draw_search_results_snapshot() -> Result<()> {
    let temp = TempDir::new()?;
    let mut config = build_config(temp.path());
    let directories = build_directories(&temp);
    directories.ensure_all()?;
    let mut db = Database::open(&directories.data_dir.join("tx.sqlite3"))?;

    let mut ctx = UiContext {
        config: &config,
        directories: &directories,
        db: &mut db,
        prompt: None,
    };

    let mut state = AppState::new(&mut ctx)?;
    state.entries.clear();
    state.preview_cache.clear();
    let snapshot = render_search_results_snapshot(&mut state)?;
    insta::assert_snapshot!("tui_search_results_render", snapshot);
    Ok(())
}

#[cfg(unix)]
fn render_search_results_snapshot(state: &mut AppState<'_>) -> Result<String> {
    state.full_text = true;
    state.filter = "refactor".into();
    state.message = Some("search: 2 matches".into());
    state.entries = vec![
        Entry::Session(SessionEntry {
            id: "codex/refactor-feature".into(),
            provider: "codex".into(),
            wrapper: Some("shellwrap".into()),
            label: Some("Refactor Feature".into()),
            first_prompt: Some("refactor feature layout".into()),
            actionable: true,
            last_active: Some(1_697_000_000),
            snippet: Some("Assistant suggested extracting helpers".into()),
            snippet_role: Some("assistant".into()),
        }),
        Entry::Session(SessionEntry {
            id: "codex/refactor-tests".into(),
            provider: "codex".into(),
            wrapper: None,
            label: Some("Refactor Tests".into()),
            first_prompt: Some("improve test names".into()),
            actionable: true,
            last_active: Some(1_697_050_000),
            snippet: Some("User requested clearer snapshot titles".into()),
            snippet_role: Some("user".into()),
        }),
    ];
    state.index = 0;
    state.list_state.select(Some(0));
    state.preview_cache.clear();

    let preview_lines = vec![
        "Session: Refactor Feature".into(),
        String::new(),
        "Assistant suggested extracting helpers".into(),
    ];
    state.preview_cache.insert(
        "session:codex/refactor-feature".into(),
        Preview {
            lines: preview_lines.clone(),
            styled: markdown_lines_to_text(&preview_lines),
            title: Some("codex/refactor-feature".into()),
            timestamp: Some("2023-10-22 19:00 UTC".into()),
            updated_at: Instant::now(),
        },
    );

    render_to_string(state, 60, 16)
}

#[cfg(unix)]
fn render_to_string(state: &mut AppState<'_>, width: u16, height: u16) -> Result<String> {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend)?;
    terminal.draw(|frame| draw(frame, state))?;
    let buffer = terminal.backend_mut().buffer().clone();
    Ok(buffer_to_string(&buffer))
}

#[cfg(unix)]
fn buffer_to_string(buffer: &Buffer) -> String {
    let area = buffer.area();
    let mut lines = Vec::new();
    for y in 0..area.height {
        let mut line = String::new();
        for x in 0..area.width {
            let symbol = buffer
                .cell((x, y))
                .map_or(" ", ratatui::buffer::Cell::symbol);
            if symbol.is_empty() {
                line.push(' ');
            } else {
                line.push_str(symbol);
            }
        }
        while line.ends_with(' ') {
            line.pop();
        }
        lines.push(line);
    }
    lines.join("\n")
}

#[test]
fn buffer_to_string_strips_trailing_whitespace() {
    let mut buffer = Buffer::empty(Rect::new(0, 0, 3, 1));
    // Set an empty symbol to exercise the space fallback path.
    buffer[(0, 0)].set_symbol("");

    let rendered = buffer_to_string(&buffer);
    assert_eq!(rendered, "");
}

#[test]
fn empty_entry_placeholder_when_no_providers() -> Result<()> {
    let temp = TempDir::new()?;
    let mut config = build_config(temp.path());
    config.providers.clear();
    config.profiles.clear();
    config.defaults.provider = None;
    config.defaults.profile = None;
    let directories = build_directories(&temp);
    directories.ensure_all()?;
    let mut db = Database::open(&directories.data_dir.join("tx.sqlite3"))?;

    let mut ctx = UiContext {
        config: &config,
        directories: &directories,
        db: &mut db,
        prompt: None,
    };

    let placeholder = EmptyEntry::for_state(&ctx);
    let state = AppState::new(&mut ctx)?;
    assert!(matches!(state.entries.first(), Some(Entry::Empty(_))));
    assert!(placeholder.status.is_some());
    Ok(())
}

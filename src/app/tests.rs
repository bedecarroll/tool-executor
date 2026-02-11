use super::*;
use crate::cli::{
    ConfigCommand, ConfigDefaultCommand, ExportCommand, ResumeCommand, SearchCommand,
};
use crate::config::model::{
    Config, ConfigDiagnostic, Defaults, DiagnosticLevel, EnvVar, FeatureConfig, ProfileConfig,
    ProviderConfig, SearchMode, Snippet, SnippetConfig, WrapperConfig, WrapperMode,
};
use crate::config::{AppDirectories, ConfigSource, ConfigSourceKind, LoadedConfig};
use crate::db::Database;
use crate::indexer::IndexError;
use crate::pipeline::{Invocation, PipelinePlan};
use crate::session::{MessageRecord, SearchHit, SessionIngest, SessionSummary, Transcript};
use crate::test_support::{ENV_LOCK, EnvOverride, toml_path};
use assert_fs::TempDir;
use assert_fs::prelude::*;
use clap::Parser;
use color_eyre::eyre::eyre;
use indexmap::IndexMap;
use std::collections::HashMap;
#[cfg(unix)]
use std::env;
use std::fs;
use std::io::{Cursor, Write};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use time::OffsetDateTime;
use toml::Value;

fn sample_summary() -> SessionSummary {
    SessionSummary {
        id: "id".into(),
        provider: "codex".into(),
        wrapper: Some("shellwrap".into()),
        model: None,
        label: Some("demo".into()),
        path: PathBuf::from("/tmp/file.jsonl"),
        uuid: Some("abc".into()),
        first_prompt: Some("Hello".into()),
        actionable: true,
        created_at: Some(1),
        started_at: Some(2),
        last_active: Some(3),
        size: 4,
        mtime: 5,
    }
}

#[test]
fn export_markdown_emits_transcript_lines() {
    let summary = sample_summary();
    let mut message = MessageRecord::new(
        summary.id.clone(),
        0,
        "user",
        "Hello markdown",
        None,
        Some(0),
    );
    message.is_first = true;
    let transcript = Transcript {
        session: summary,
        messages: vec![message],
    };
    export_markdown(&transcript);
}

#[test]
fn parse_vars_splits_key_value_pairs() -> Result<()> {
    let vars = vec!["FOO=bar".into(), "BAZ=qux=quux".into()];
    let parsed = parse_vars(&vars)?;
    assert_eq!(parsed.get("FOO"), Some(&"bar".to_string()));
    assert_eq!(parsed.get("BAZ"), Some(&"qux=quux".to_string()));
    Ok(())
}

#[test]
fn parse_vars_rejects_missing_equals() {
    let err = parse_vars(&["invalid".into()]).unwrap_err();
    assert!(err.to_string().contains("expected KEY=VALUE"));
}

#[test]
fn resume_capture_prompt_detects_profile_and_cli_pre_snippets() {
    assert!(should_capture_prompt_for_resume(None, true, &[]));
    assert!(should_capture_prompt_for_resume(
        None,
        false,
        &["pre".into()],
    ));
    assert!(should_capture_prompt_for_resume(
        Some(&PromptInvocation {
            name: "demo".into(),
            args: Vec::new(),
        }),
        false,
        &[],
    ));
    assert!(!should_capture_prompt_for_resume(None, false, &[]));
}

#[test]
fn prompt_for_stdin_with_reader_appends_newline() -> Result<()> {
    let mut reader = Cursor::new("value");
    let captured = prompt_for_stdin_with_reader(None, &mut reader)?;
    assert_eq!(captured, "value\n");
    Ok(())
}

#[test]
fn prompt_for_stdin_with_reader_normalizes_crlf() -> Result<()> {
    let mut reader = Cursor::new("value\r\n");
    let captured = prompt_for_stdin_with_reader(Some("Label"), &mut reader)?;
    assert_eq!(captured, "value\n");
    Ok(())
}

#[test]
fn summary_to_json_includes_snippet() {
    let summary = sample_summary();
    let value = summary_to_json(&summary, Some("snippet"), Some("user"));
    assert_eq!(value["id"], "id");
    assert_eq!(value["snippet"], "snippet");
    assert_eq!(value["snippet_role"], "user");
    assert_eq!(value["wrapper"], "shellwrap");
}

#[test]
fn collate_search_results_applies_filters_and_limit() -> Result<()> {
    let hits = vec![
        SearchHit {
            session_id: "keep-1".into(),
            provider: "codex".into(),
            wrapper: None,
            label: None,
            role: Some("user".into()),
            snippet: Some("hello".into()),
            last_active: Some(120),
            actionable: true,
        },
        SearchHit {
            session_id: "skip-role".into(),
            provider: "codex".into(),
            wrapper: None,
            label: None,
            role: Some("assistant".into()),
            snippet: None,
            last_active: Some(140),
            actionable: true,
        },
        SearchHit {
            session_id: "stale".into(),
            provider: "codex".into(),
            wrapper: None,
            label: None,
            role: Some("user".into()),
            snippet: None,
            last_active: Some(10),
            actionable: true,
        },
        SearchHit {
            session_id: "keep-2".into(),
            provider: "codex".into(),
            wrapper: None,
            label: None,
            role: Some("USER".into()),
            snippet: None,
            last_active: Some(200),
            actionable: true,
        },
    ];

    let mut summaries = HashMap::new();
    let mut summary_one = sample_summary();
    summary_one.id = "keep-1".into();
    summary_one.last_active = Some(120);
    summaries.insert(summary_one.id.clone(), summary_one);

    let mut summary_two = sample_summary();
    summary_two.id = "skip-role".into();
    summary_two.last_active = Some(140);
    summaries.insert(summary_two.id.clone(), summary_two);

    let mut summary_three = sample_summary();
    summary_three.id = "stale".into();
    summary_three.last_active = Some(10);
    summaries.insert(summary_three.id.clone(), summary_three);

    let mut summary_four = sample_summary();
    summary_four.id = "keep-2".into();
    summary_four.last_active = Some(200);
    summaries.insert(summary_four.id.clone(), summary_four);

    let mut lookup =
        |id: &str| -> Result<Option<SessionSummary>> { Ok(summaries.get(id).cloned()) };

    let detailed =
        App::collate_search_results(hits, Some(100), Some("user"), Some(1), &mut lookup)?;
    assert_eq!(detailed.len(), 1);
    assert_eq!(detailed[0].0.session_id, "keep-1");
    Ok(())
}

#[test]
fn collate_search_results_skips_missing_summary() -> Result<()> {
    let hits = vec![SearchHit {
        session_id: "missing".into(),
        provider: "codex".into(),
        wrapper: None,
        label: None,
        role: Some("user".into()),
        snippet: None,
        last_active: Some(50),
        actionable: true,
    }];

    let mut lookup = |_id: &str| -> Result<Option<SessionSummary>> { Ok(None) };
    let detailed = App::collate_search_results(hits, None, None, None, &mut lookup)?;
    assert!(detailed.is_empty());
    Ok(())
}

#[test]
fn log_index_report_emits_warnings_for_errors() {
    let mut report = IndexReport {
        scanned: 1,
        updated: 0,
        skipped: 0,
        removed: 0,
        errors: vec![IndexError {
            path: PathBuf::from("missing.jsonl"),
            error: eyre!("boom"),
        }],
    };
    log_index_report(&report);
    report.errors.clear();
    log_index_report(&report);
}

#[test]
fn config_lint_returns_error_for_error_diagnostics() -> Result<()> {
    let temp = TempDir::new()?;
    let directories = setup_directories(&temp)?;
    let (sessions_dir, _) = create_session_artifacts(&temp)?;
    let config = fixture_config(&sessions_dir);
    let diagnostics = vec![ConfigDiagnostic {
        level: DiagnosticLevel::Error,
        message: "broken config".into(),
    }];
    let sources = vec![ConfigSource {
        path: directories.config_dir.join("config.toml"),
        kind: ConfigSourceKind::Main,
    }];
    let merged = Value::Table(toml::map::Map::new());
    let db = Database::open(&directories.data_dir.join("tx.sqlite3"))?;
    let cli = Cli::parse_from(["tx"]);
    let app = App {
        cli: &cli,
        loaded: LoadedConfig {
            config,
            diagnostics,
            sources,
            merged,
            directories: directories.clone(),
        },
        db,
        prompt: None,
    };

    let err = app
        .config_lint()
        .expect_err("should surface configuration errors");
    assert!(err.to_string().contains("configuration contains errors"));
    Ok(())
}

#[test]
fn emit_command_covers_plain_and_json_modes() -> Result<()> {
    let cwd = std::env::current_dir().expect("current dir");
    let plan = PipelinePlan {
        pipeline: "echo hi".into(),
        display: "echo hi".into(),
        friendly_display: "friendly hi".into(),
        env: vec![("KEY".into(), "VALUE".into())],
        invocation: Invocation::Shell {
            command: "true".into(),
        },
        provider: "echo".into(),
        terminal_title: "echo".into(),
        pre_snippets: Vec::new(),
        post_snippets: Vec::new(),
        wrapper: None,
        needs_stdin_prompt: false,
        uses_capture_arg: false,
        capture_has_pre_commands: false,
        stdin_prompt_label: None,
        cwd,
        prompt_assembler: None,
    };

    emit_command(
        &plan,
        EmitMode::Plain {
            newline: false,
            friendly: true,
        },
    )?;
    emit_command(&plan, EmitMode::Json)?;
    Ok(())
}

#[cfg(unix)]
#[test]
fn execute_plan_exec_invocation_sets_capture_env() -> Result<()> {
    let _guard = ENV_LOCK.lock().unwrap();
    let temp = TempDir::new()?;
    let log_path = temp.child("capture.log");
    let script = temp.child("capture.sh");
    script.write_str("#!/bin/sh\nprintf '%s' \"$TX_CAPTURE_STDIN_DATA\" > \"$1\"\n")?;
    let perms = std::fs::Permissions::from_mode(0o755);
    std::fs::set_permissions(script.path(), perms)?;

    let plan = PipelinePlan {
        pipeline: "internal capture-arg".into(),
        display: "log".into(),
        friendly_display: "log".into(),
        env: Vec::new(),
        invocation: Invocation::Exec {
            argv: vec![
                script.path().display().to_string(),
                log_path.path().display().to_string(),
            ],
        },
        provider: "demo".into(),
        terminal_title: "demo".into(),
        pre_snippets: Vec::new(),
        post_snippets: Vec::new(),
        wrapper: None,
        needs_stdin_prompt: false,
        uses_capture_arg: true,
        capture_has_pre_commands: false,
        stdin_prompt_label: None,
        cwd: temp.path().to_path_buf(),
        prompt_assembler: None,
    };

    execute_plan_with_prompt(&plan, true, Some("payload".into()), |_| Ok(None))?;

    let captured = std::fs::read_to_string(log_path.path())?;
    assert_eq!(captured, "payload");
    Ok(())
}

#[cfg(unix)]
#[test]
fn default_shell_prefers_environment_or_falls_back() {
    let _guard = ENV_LOCK.lock().unwrap();
    unsafe {
        std::env::set_var("SHELL", "/bin/sh");
    }

    let shell = default_shell();
    assert_eq!(shell.flag, "-c");
    assert!(!shell.path.is_empty());
}

fn setup_directories(temp: &TempDir) -> Result<AppDirectories> {
    let directories = AppDirectories {
        config_dir: temp.path().join("config"),
        data_dir: temp.path().join("data"),
        cache_dir: temp.path().join("cache"),
    };
    directories.ensure_all()?;
    Ok(directories)
}

fn create_session_artifacts(temp: &TempDir) -> Result<(PathBuf, PathBuf)> {
    let sessions_dir = temp.path().join("sessions");
    fs::create_dir_all(&sessions_dir)?;
    let session_path = sessions_dir.join("session.jsonl");
    fs::File::create(&session_path)?.write_all(b"{\"event\":\"test\"}\n")?;
    Ok((sessions_dir, session_path))
}

#[allow(clippy::too_many_lines)]
fn fixture_config(sessions_dir: &Path) -> Config {
    let mut providers = IndexMap::new();
    providers.insert(
        "codex".into(),
        ProviderConfig {
            name: "codex".into(),
            bin: "echo".into(),
            flags: vec!["--from-config".into()],
            env: vec![EnvVar {
                key: "TEST_PRESENT".into(),
                value_template: "${env:TEST_PRESENT}".into(),
            }],
            session_roots: vec![sessions_dir.to_path_buf()],
            stdin: None,
        },
    );
    providers.insert(
        "alt".into(),
        ProviderConfig {
            name: "alt".into(),
            bin: "echo".into(),
            flags: Vec::new(),
            env: vec![EnvVar {
                key: "TEST_MISSING".into(),
                value_template: "${env:TEST_MISSING}".into(),
            }],
            session_roots: vec![sessions_dir.to_path_buf()],
            stdin: None,
        },
    );

    let mut pre_snippets = IndexMap::new();
    pre_snippets.insert(
        "pre".into(),
        Snippet {
            name: "pre".into(),
            command: "echo pre".into(),
        },
    );

    let snippets = SnippetConfig {
        pre: pre_snippets,
        post: IndexMap::new(),
    };

    let mut profiles = IndexMap::new();
    profiles.insert(
        "default".into(),
        ProfileConfig {
            name: "default".into(),
            provider: "codex".into(),
            description: Some("Primary profile".into()),
            pre: vec!["pre".into()],
            post: Vec::new(),
            wrap: Some("wrap".into()),
            prompt_assembler: None,
            prompt_assembler_args: Vec::new(),
        },
    );
    profiles.insert(
        "mismatch".into(),
        ProfileConfig {
            name: "mismatch".into(),
            provider: "alt".into(),
            description: None,
            pre: Vec::new(),
            post: Vec::new(),
            wrap: None,
            prompt_assembler: None,
            prompt_assembler_args: Vec::new(),
        },
    );

    let mut wrappers = IndexMap::new();
    wrappers.insert(
        "wrap".into(),
        WrapperConfig {
            name: "wrap".into(),
            mode: WrapperMode::Shell {
                command: "echo {{CMD}}".into(),
            },
        },
    );
    wrappers.insert(
        "execwrap".into(),
        WrapperConfig {
            name: "execwrap".into(),
            mode: WrapperMode::Exec {
                argv: vec!["exec-binary".into()],
            },
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
        wrappers,
        profiles,
        features: FeatureConfig {
            prompt_assembler: None,
        },
    }
}

fn fixture_sources(directories: &AppDirectories) -> Vec<ConfigSource> {
    vec![
        ConfigSource {
            kind: ConfigSourceKind::Main,
            path: directories.config_dir.join("config.toml"),
        },
        ConfigSource {
            kind: ConfigSourceKind::DropIn,
            path: directories.config_dir.join("conf.d/10-extra.toml"),
        },
        ConfigSource {
            kind: ConfigSourceKind::Project,
            path: directories.config_dir.join("..").join("project.toml"),
        },
        ConfigSource {
            kind: ConfigSourceKind::ProjectDropIn,
            path: directories
                .config_dir
                .join("..")
                .join("project.d")
                .join("00-extra.toml"),
        },
    ]
}

fn seed_database(
    directories: &AppDirectories,
    session_path: PathBuf,
) -> Result<(Database, SessionSummary)> {
    let db_path = directories.data_dir.join("tx.sqlite3");
    let mut db = Database::open(&db_path)?;
    let now = OffsetDateTime::now_utc().unix_timestamp();
    let summary = SessionSummary {
        id: "sess-1".into(),
        provider: "codex".into(),
        wrapper: Some("shellwrap".into()),
        model: None,
        label: Some("Demo Session".into()),
        path: session_path,
        uuid: Some("uuid-1".into()),
        first_prompt: Some("Hello world".into()),
        actionable: true,
        created_at: Some(now),
        started_at: Some(now),
        last_active: Some(now),
        size: 42,
        mtime: now,
    };
    let mut message = MessageRecord::new(
        summary.id.clone(),
        0,
        "user",
        "Hello world",
        Some("event_msg".into()),
        Some(now),
    );
    message.is_first = true;
    db.upsert_session(&SessionIngest::new(summary.clone(), vec![message]))?;
    Ok((db, summary))
}

fn configure_provider_env() {
    unsafe {
        std::env::set_var("TEST_PRESENT", "1");
        std::env::remove_var("TEST_MISSING");
    }
}

#[cfg(unix)]
fn install_pa_script(temp: &TempDir, script: &str) -> Result<PathBuf> {
    let pa = temp.child("pa");
    pa.write_str(script)?;
    let perms = fs::Permissions::from_mode(0o755);
    fs::set_permissions(pa.path(), perms)?;
    Ok(pa.path().to_path_buf())
}

fn build_app_fixture(
    diagnostics: Vec<ConfigDiagnostic>,
) -> Result<(TempDir, App<'static>, SessionSummary)> {
    let temp = TempDir::new()?;
    let directories = setup_directories(&temp)?;
    let (sessions_dir, session_path) = create_session_artifacts(&temp)?;
    let config = fixture_config(&sessions_dir);
    let sources = fixture_sources(&directories);
    let (db, summary) = seed_database(&directories, session_path)?;

    let mut merged = toml::map::Map::new();
    merged.insert("provider".into(), Value::String("codex".into()));
    let loaded = LoadedConfig {
        config: config.clone(),
        merged: Value::Table(merged),
        directories: directories.clone(),
        sources,
        diagnostics,
    };

    configure_provider_env();

    let cli = Box::leak(Box::new(Cli {
        config_dir: Some(directories.config_dir.clone()),
        verbose: 0,
        quiet: false,
        command: None,
    }));

    let app = App {
        cli,
        loaded,
        db,
        prompt: None,
    };

    Ok((temp, app, summary))
}

#[test]
fn app_search_and_export_paths() -> Result<()> {
    let (_temp, app, summary) = build_app_fixture(Vec::new())?;
    let mut search_cmd = SearchCommand {
        term: None,
        full_text: false,
        provider: None,
        since: None,
        role: None,
        limit: None,
    };
    app.search(&search_cmd)?;

    search_cmd.term = Some("Hello".into());
    app.search(&search_cmd)?;

    search_cmd.full_text = true;
    search_cmd.role = Some("user".into());
    search_cmd.since = Some(60);
    app.search(&search_cmd)?;

    let export_cmd = ExportCommand {
        session_id: summary.id.clone(),
    };
    app.export(&export_cmd)?;
    Ok(())
}

#[test]
fn app_search_rejects_role_without_full_text() -> Result<()> {
    let (_temp, app, _summary) = build_app_fixture(Vec::new())?;
    let cmd = SearchCommand {
        term: Some("hello".into()),
        full_text: false,
        provider: None,
        since: None,
        role: Some("user".into()),
        limit: None,
    };
    let err = app.search(&cmd).expect_err("role requires full-text");
    assert!(err.to_string().contains("--role requires --full-text"));
    Ok(())
}

#[test]
fn app_resume_accepts_plain_session_id() -> Result<()> {
    let (_temp, mut app, summary) = build_app_fixture(Vec::new())?;
    let cmd = ResumeCommand {
        session_id: summary.id.clone(),
        profile: Some("default".into()),
        pre_snippets: Vec::new(),
        post_snippets: Vec::new(),
        wrap: None,
        emit_command: true,
        emit_json: false,
        vars: Vec::new(),
        dry_run: false,
        provider_args: Vec::new(),
    };
    app.resume(&cmd)?;
    Ok(())
}

#[cfg(unix)]
#[test]
fn resolve_resume_profile_builds_prompt_invocation_and_validates_prompt() -> Result<()> {
    let _env = ENV_LOCK.lock().unwrap();
    let (temp, mut app, _summary) = build_app_fixture(Vec::new())?;
    let pa_path = install_pa_script(
        &temp,
        "#!/bin/sh\nif [ \"$1\" = \"list\" ] && [ \"$2\" = \"--json\" ]; then\n  printf '%s' '[{\"name\":\"demo/prompt\",\"stdin_supported\":true}]'\n  exit 0\nfi\nif [ \"$1\" = \"show\" ] && [ \"$2\" = \"--json\" ]; then\n  printf '%s' '{\"profile\":{\"content\":\"Demo\"}}'\n  exit 0\nfi\nexit 1\n",
    )?;
    let _pa_guard = EnvOverride::set_path("TX_TEST_PA_BIN", &pa_path);

    app.loaded.config.features.prompt_assembler = Some(PromptAssemblerConfig {
        namespace: "tests".into(),
    });
    let profile = app
        .loaded
        .config
        .profiles
        .get_mut("default")
        .expect("default profile");
    profile.prompt_assembler = Some("demo/prompt".into());
    profile.prompt_assembler_args = vec!["one".into(), "two".into()];

    let (prompt_invocation, has_pre_snippets) =
        app.resolve_resume_profile(Some("default"), "codex")?;

    assert!(has_pre_snippets);
    let invocation = prompt_invocation.expect("prompt invocation");
    assert_eq!(invocation.name, "demo/prompt");
    assert_eq!(invocation.args, vec!["one".to_string(), "two".to_string()]);
    assert!(app.prompt.is_some());
    Ok(())
}

#[test]
fn resume_uses_current_dir_when_session_path_has_no_parent() -> Result<()> {
    let (_temp, mut app, mut summary) = build_app_fixture(Vec::new())?;
    summary.path = PathBuf::from("/");
    app.db
        .upsert_session(&SessionIngest::new(summary.clone(), Vec::new()))?;

    let cmd = ResumeCommand {
        session_id: summary.id,
        profile: Some("default".into()),
        pre_snippets: Vec::new(),
        post_snippets: Vec::new(),
        wrap: None,
        emit_command: true,
        emit_json: false,
        vars: Vec::new(),
        dry_run: false,
        provider_args: Vec::new(),
    };

    app.resume(&cmd)?;
    Ok(())
}

#[test]
fn resume_executes_pipeline_when_not_emitting() -> Result<()> {
    let (_temp, mut app, summary) = build_app_fixture(Vec::new())?;
    let cmd = ResumeCommand {
        session_id: summary.id,
        profile: None,
        pre_snippets: Vec::new(),
        post_snippets: Vec::new(),
        wrap: None,
        emit_command: false,
        emit_json: false,
        vars: Vec::new(),
        dry_run: false,
        provider_args: Vec::new(),
    };

    app.resume(&cmd)?;
    Ok(())
}

#[test]
fn app_export_errors_when_session_missing_direct() -> Result<()> {
    let (_temp, app, _summary) = build_app_fixture(Vec::new())?;
    let cmd = ExportCommand {
        session_id: "missing-session".into(),
    };
    let err = app.export(&cmd).expect_err("missing session should error");
    assert!(err.to_string().contains("not found"));
    Ok(())
}

#[test]
fn app_config_lint_runs() -> Result<()> {
    let (_temp, app, _summary) = build_app_fixture(Vec::new())?;
    app.config(&ConfigCommand::Lint)?;
    Ok(())
}

#[test]
fn collate_search_results_returns_hit_details() -> Result<()> {
    let hit = SearchHit {
        session_id: "sess-123".into(),
        provider: "codex".into(),
        wrapper: Some("shellwrap".into()),
        label: Some("Sample".into()),
        role: Some("user".into()),
        snippet: Some("Hello world".into()),
        last_active: Some(42),
        actionable: true,
    };
    let mut second_hit = hit.clone();
    second_hit.session_id = "sess-456".into();

    let summary = sample_summary();
    let mut other_summary = summary.clone();
    other_summary.id = second_hit.session_id.clone();

    let mut lookup_count = 0;
    let detailed = App::collate_search_results(
        vec![hit.clone(), second_hit.clone()],
        None,
        None,
        Some(1),
        |id| {
            lookup_count += 1;
            if id == hit.session_id.as_str() {
                Ok(Some(summary.clone()))
            } else if id == second_hit.session_id.as_str() {
                Ok(Some(other_summary.clone()))
            } else {
                Ok(None)
            }
        },
    )?;
    assert_eq!(lookup_count, 2);
    assert_eq!(detailed.len(), 1);
    let (returned_hit, returned_summary) = &detailed[0];
    assert_eq!(returned_hit.session_id, hit.session_id);
    assert_eq!(returned_summary.id, summary.id);
    Ok(())
}

#[test]
fn app_resume_and_config_commands() -> Result<()> {
    let (_temp, mut app, summary) = build_app_fixture(Vec::new())?;
    let mut resume_cmd = ResumeCommand {
        session_id: summary.uuid.clone().unwrap(),
        profile: Some("default".into()),
        pre_snippets: vec!["pre".into()],
        post_snippets: Vec::new(),
        wrap: None,
        emit_command: true,
        emit_json: true,
        vars: vec!["KEY=value".into()],
        dry_run: false,
        provider_args: vec!["--flag".into()],
    };
    app.resume(&resume_cmd)?;

    resume_cmd.profile = Some("mismatch".into());
    let err = app.resume(&resume_cmd).unwrap_err();
    assert!(err.to_string().contains("provider mismatch"));

    app.config(&ConfigCommand::List)?;
    app.config(&ConfigCommand::Dump)?;
    app.config(&ConfigCommand::Where)?;
    app.config(&ConfigCommand::Default(ConfigDefaultCommand { raw: false }))?;
    Ok(())
}

#[test]
fn ensure_prompt_available_errors_when_disabled() -> Result<()> {
    let (_temp, mut app, _summary) = build_app_fixture(Vec::new())?;
    app.loaded.config.features.prompt_assembler = None;

    let err = app
        .ensure_prompt_available("demo")
        .expect_err("prompt lookup should fail when assembler disabled");
    assert!(
        err.to_string().contains("prompt-assembler is disabled"),
        "unexpected error: {err:?}"
    );
    Ok(())
}

#[test]
fn ensure_prompt_available_errors_when_existing_prompt_feature_is_disabled() -> Result<()> {
    let (_temp, mut app, _summary) = build_app_fixture(Vec::new())?;
    app.prompt = Some(PromptAssembler::new(PromptAssemblerConfig {
        namespace: "tests".into(),
    }));
    app.loaded.config.features.prompt_assembler = None;

    let err = app
        .ensure_prompt_available("demo/prompt")
        .expect_err("prompt lookup should fail when assembler disabled");
    assert!(err.to_string().contains("prompt-assembler is disabled"));
    Ok(())
}

#[cfg(unix)]
#[test]
fn ensure_prompt_available_errors_when_prompt_name_missing() -> Result<()> {
    let _env = ENV_LOCK.lock().unwrap();
    let (temp, mut app, _summary) = build_app_fixture(Vec::new())?;
    let pa_path = install_pa_script(
        &temp,
        "#!/bin/sh\nif [ \"$1\" = \"list\" ] && [ \"$2\" = \"--json\" ]; then\n  printf '%s' '[{\"name\":\"other/prompt\",\"stdin_supported\":true}]'\n  exit 0\nfi\nif [ \"$1\" = \"show\" ] && [ \"$2\" = \"--json\" ]; then\n  printf '%s' '{\"profile\":{\"content\":\"Other\"}}'\n  exit 0\nfi\nexit 1\n",
    )?;
    let _pa_guard = EnvOverride::set_path("TX_TEST_PA_BIN", &pa_path);
    app.loaded.config.features.prompt_assembler = Some(PromptAssemblerConfig {
        namespace: "tests".into(),
    });

    let err = app
        .ensure_prompt_available("demo/prompt")
        .expect_err("missing prompt should error");
    assert!(
        err.to_string()
            .contains("prompt assembler prompt 'demo/prompt' not found")
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn ensure_prompt_available_surfaces_unavailable_prompt_assembler() -> Result<()> {
    let _env = ENV_LOCK.lock().unwrap();
    let (temp, mut app, _summary) = build_app_fixture(Vec::new())?;
    let pa_path = install_pa_script(&temp, "#!/bin/sh\nexit 1\n")?;
    let _pa_guard = EnvOverride::set_path("TX_TEST_PA_BIN", &pa_path);
    app.loaded.config.features.prompt_assembler = Some(PromptAssemblerConfig {
        namespace: "tests".into(),
    });

    let err = app
        .ensure_prompt_available("demo/prompt")
        .expect_err("unavailable prompt assembler should fail");
    assert!(err.to_string().contains("prompt assembler unavailable"));
    Ok(())
}

#[test]
fn app_resume_requires_emit_json_companion_flag() -> Result<()> {
    let (_temp, mut app, summary) = build_app_fixture(Vec::new())?;
    let cmd = ResumeCommand {
        session_id: summary.uuid.clone().unwrap(),
        profile: Some("default".into()),
        pre_snippets: Vec::new(),
        post_snippets: Vec::new(),
        wrap: None,
        emit_command: false,
        emit_json: true,
        vars: Vec::new(),
        dry_run: false,
        provider_args: Vec::new(),
    };
    let err = app
        .resume(&cmd)
        .expect_err("emit-json should require dry-run or emit-command");
    assert!(
        err.to_string()
            .contains("--emit-json requires --dry-run or --emit-command")
    );
    Ok(())
}

#[test]
fn app_config_default_supports_raw_mode() -> Result<()> {
    let (_temp, app, _) = build_app_fixture(Vec::new())?;
    app.config(&ConfigCommand::Default(ConfigDefaultCommand { raw: true }))?;
    Ok(())
}

#[cfg(unix)]
#[test]
fn execute_plan_shell_succeeds() -> Result<()> {
    let _env = ENV_LOCK.lock().unwrap();
    let original_shell = std::env::var("SHELL").ok();
    unsafe {
        std::env::set_var("SHELL", "/bin/sh");
    }

    let cwd = std::env::current_dir()?;
    let plan = PipelinePlan {
        pipeline: "true".into(),
        display: "true".into(),
        friendly_display: "true".into(),
        env: Vec::new(),
        invocation: Invocation::Shell {
            command: "true".into(),
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
        cwd,
        prompt_assembler: None,
    };

    execute_plan(&plan)?;

    if let Some(shell) = original_shell {
        unsafe {
            std::env::set_var("SHELL", shell);
        }
    } else {
        unsafe {
            std::env::remove_var("SHELL");
        }
    }

    Ok(())
}

#[cfg(unix)]
#[test]
fn execute_plan_shell_propagates_failure() -> Result<()> {
    let _env = ENV_LOCK.lock().unwrap();
    let original_shell = std::env::var("SHELL").ok();
    unsafe {
        std::env::set_var("SHELL", "/bin/sh");
    }

    let cwd = std::env::current_dir()?;
    let plan = PipelinePlan {
        pipeline: "false".into(),
        display: "false".into(),
        friendly_display: "false".into(),
        env: Vec::new(),
        invocation: Invocation::Shell {
            command: "false".into(),
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
        cwd,
        prompt_assembler: None,
    };

    let err = execute_plan(&plan).unwrap_err();
    assert!(
        err.to_string().contains("command exited with status"),
        "unexpected error: {err:?}"
    );

    if let Some(shell) = original_shell {
        unsafe {
            std::env::set_var("SHELL", shell);
        }
    } else {
        unsafe {
            std::env::remove_var("SHELL");
        }
    }

    Ok(())
}

#[test]
fn execute_plan_shell_captures_prompt_input() -> Result<()> {
    let _env = ENV_LOCK.lock().unwrap();
    let temp = TempDir::new()?;
    let output = temp.child("captured.txt");
    let script = temp.child(if cfg!(windows) {
        "capture.cmd"
    } else {
        "capture.sh"
    });
    if cfg!(windows) {
        script.write_str(
            "@echo off\r\nsetlocal EnableExtensions EnableDelayedExpansion\r\n<nul set /p=\"%TX_CAPTURE_STDIN_DATA%\" > \"%~1\"\r\nexit /B 0\r\n",
        )?;
    } else {
        script.write_str("#!/bin/sh\nprintf '%s' \"$TX_CAPTURE_STDIN_DATA\" > \"$1\"\n")?;
        #[cfg(unix)]
        {
            let perms = fs::Permissions::from_mode(0o755);
            fs::set_permissions(script.path(), perms)?;
        }
    }

    let command = format!("{} {}", script.path().display(), output.path().display());

    let plan = PipelinePlan {
        pipeline: command.clone(),
        display: "capture".into(),
        friendly_display: "capture".into(),
        env: Vec::new(),
        invocation: Invocation::Shell { command },
        provider: "codex".into(),
        terminal_title: "codex".into(),
        pre_snippets: Vec::new(),
        post_snippets: Vec::new(),
        wrapper: None,
        needs_stdin_prompt: true,
        uses_capture_arg: false,
        capture_has_pre_commands: false,
        stdin_prompt_label: Some("Prompt".into()),
        cwd: temp.path().to_path_buf(),
        prompt_assembler: None,
    };

    execute_plan_with_prompt(&plan, true, None, |_| Ok(Some("payload".into())))?;
    assert_eq!(std::fs::read_to_string(output.path())?, "payload");
    Ok(())
}

#[cfg(unix)]
#[test]
fn execute_plan_uses_prompt_assembler_output() -> Result<()> {
    let _env = ENV_LOCK.lock().unwrap();
    let temp = TempDir::new()?;

    let pa_dir = temp.child("bin");
    pa_dir.create_dir_all()?;
    let pa = pa_dir.child("pa");
    pa.write_str(
        "#!/bin/sh\nif [ \"$1\" = \"show\" ] && [ \"${2:-}\" = \"--json\" ]; then\n  shift 2\n  printf '%s' '{\"profile\":{\"content\":\"Demo {0}\\n\"}}'\n  exit 0\nfi\nshift\nprintf 'Demo %s\\n' \"${1:-}\"\n",
    )?;
    let perms = fs::Permissions::from_mode(0o755);
    fs::set_permissions(pa.path(), perms.clone())?;

    let capture = temp.child("capture.sh");
    capture.write_str("#!/bin/sh\nprintf '%s' \"$TX_CAPTURE_STDIN_DATA\" > \"$1\"\n")?;
    fs::set_permissions(capture.path(), perms)?;

    let output = temp.child("prompt.txt");
    let command = format!("{} {}", capture.path().display(), output.path().display());

    let original_path = std::env::var("PATH").ok();
    let new_path = if let Some(ref existing) = original_path {
        format!("{}:{}", pa_dir.path().display(), existing)
    } else {
        pa_dir.path().display().to_string()
    };
    unsafe {
        std::env::set_var("PATH", &new_path);
    }

    let plan = PipelinePlan {
        pipeline: command.clone(),
        display: "capture".into(),
        friendly_display: "capture".into(),
        env: Vec::new(),
        invocation: Invocation::Shell { command },
        provider: "codex".into(),
        terminal_title: "codex".into(),
        pre_snippets: Vec::new(),
        post_snippets: Vec::new(),
        wrapper: None,
        needs_stdin_prompt: false,
        uses_capture_arg: false,
        capture_has_pre_commands: false,
        stdin_prompt_label: None,
        cwd: temp.path().to_path_buf(),
        prompt_assembler: Some(PromptInvocation {
            name: "demo".into(),
            args: vec!["value".into()],
        }),
    };

    execute_plan(&plan)?;
    unsafe {
        if let Some(value) = original_path {
            std::env::set_var("PATH", value);
        } else {
            std::env::remove_var("PATH");
        }
    }

    let contents = std::fs::read_to_string(output.path())?;
    assert_eq!(contents.trim_end(), "Demo value");
    Ok(())
}

#[test]
fn execute_plan_emits_capture_warning_for_internal_pipeline() -> Result<()> {
    #[cfg(windows)]
    let original_shell = std::env::var("SHELL").ok();
    #[cfg(windows)]
    unsafe {
        if std::env::var_os("SHELL").is_none()
            && let Ok(comspec) = std::env::var("COMSPEC")
        {
            std::env::set_var("SHELL", comspec);
        }
    }

    let command = if cfg!(windows) {
        "exit 0".to_string()
    } else {
        "true".to_string()
    };
    let plan = PipelinePlan {
        pipeline: "internal capture-arg".into(),
        display: "capture".into(),
        friendly_display: "capture".into(),
        env: Vec::new(),
        invocation: Invocation::Shell { command },
        provider: "codex".into(),
        terminal_title: "codex".into(),
        pre_snippets: Vec::new(),
        post_snippets: Vec::new(),
        wrapper: None,
        needs_stdin_prompt: false,
        uses_capture_arg: true,
        capture_has_pre_commands: false,
        stdin_prompt_label: None,
        cwd: std::env::current_dir()?,
        prompt_assembler: None,
    };

    execute_plan_with_prompt(&plan, true, None, |_| Ok(None))?;

    #[cfg(windows)]
    {
        if let Some(shell) = original_shell {
            unsafe {
                std::env::set_var("SHELL", shell);
            }
        } else {
            unsafe {
                std::env::remove_var("SHELL");
            }
        }
    }
    Ok(())
}

#[test]
fn should_warn_capture_respects_pre_pipeline() -> Result<()> {
    let cwd = std::env::current_dir()?;
    let base = PipelinePlan {
        pipeline: "internal capture-arg".into(),
        display: "capture".into(),
        friendly_display: "capture".into(),
        env: Vec::new(),
        invocation: Invocation::Shell {
            command: "true".into(),
        },
        provider: "codex".into(),
        terminal_title: "codex".into(),
        pre_snippets: Vec::new(),
        post_snippets: Vec::new(),
        wrapper: None,
        needs_stdin_prompt: false,
        uses_capture_arg: true,
        capture_has_pre_commands: false,
        stdin_prompt_label: None,
        cwd: cwd.clone(),
        prompt_assembler: None,
    };

    assert!(should_warn_capture(&base, None, true));
    assert!(!should_warn_capture(&base, Some("payload"), true));
    assert!(!should_warn_capture(&base, None, false));

    let mut with_pre = base;
    with_pre.pipeline = "internal capture-arg --pre 'echo hi'".into();
    with_pre.capture_has_pre_commands = true;
    assert!(!should_warn_capture(&with_pre, None, true));
    Ok(())
}

#[cfg(unix)]
#[test]
fn execute_plan_exec_succeeds_and_failures() -> Result<()> {
    let cwd = std::env::current_dir()?;
    let mut success_plan = PipelinePlan {
        pipeline: "exec success".into(),
        display: "exec success".into(),
        friendly_display: "exec success".into(),
        env: Vec::new(),
        invocation: Invocation::Exec {
            argv: vec!["/bin/sh".into(), "-c".into(), "exit 0".into()],
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
        cwd: cwd.clone(),
        prompt_assembler: None,
    };

    execute_plan(&success_plan)?;

    success_plan.invocation = Invocation::Exec {
        argv: vec!["/bin/sh".into(), "-c".into(), "exit 5".into()],
    };
    let err = execute_plan(&success_plan).unwrap_err();
    assert!(
        err.to_string().contains("command exited with status"),
        "unexpected error: {err:?}"
    );

    Ok(())
}

#[test]
fn app_doctor_and_config_lint() -> Result<()> {
    let (_temp, app, _summary) = build_app_fixture(Vec::new())?;
    app.doctor()?;
    app.config(&ConfigCommand::Lint)?;

    let diag = ConfigDiagnostic {
        level: DiagnosticLevel::Error,
        message: "bad configuration".into(),
    };
    let (_temp, app_with_error, _) = build_app_fixture(vec![diag])?;
    let err = app_with_error.config(&ConfigCommand::Lint).unwrap_err();
    assert!(err.to_string().contains("configuration contains errors"));
    Ok(())
}

#[test]
fn emit_command_covers_all_modes() -> Result<()> {
    let cwd = std::env::current_dir()?;
    let plan = PipelinePlan {
        pipeline: "echo hi".into(),
        display: "echo hi".into(),
        friendly_display: "friendly hi".into(),
        env: vec![("KEY".into(), "VALUE".into())],
        invocation: Invocation::Shell {
            command: "true".into(),
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
        cwd,
        prompt_assembler: None,
    };

    emit_command(&plan, EmitMode::Json)?;
    emit_command(
        &plan,
        EmitMode::Plain {
            newline: false,
            friendly: false,
        },
    )?;
    emit_command(
        &plan,
        EmitMode::Plain {
            newline: true,
            friendly: true,
        },
    )?;
    Ok(())
}

#[test]
fn config_lint_emits_warnings_without_error() -> Result<()> {
    let warning = ConfigDiagnostic {
        level: DiagnosticLevel::Warning,
        message: "deprecated option".into(),
    };
    let (_temp, app, _) = build_app_fixture(vec![warning])?;
    app.config(&ConfigCommand::Lint)?;
    Ok(())
}

#[test]
fn search_requires_full_text_for_role() -> Result<()> {
    let (_temp, app, _summary) = build_app_fixture(Vec::new())?;
    let cmd = SearchCommand {
        term: Some("hello".into()),
        full_text: false,
        provider: None,
        since: None,
        role: Some("user".into()),
        limit: None,
    };
    let err = app
        .search(&cmd)
        .expect_err("role filter without full-text should error");
    assert!(
        err.to_string().contains("--role requires --full-text"),
        "unexpected error: {err:?}"
    );
    Ok(())
}

#[test]
fn app_run_ui_starts_event_loop() -> Result<()> {
    let (_temp, mut app, _) = build_app_fixture(Vec::new())?;
    app.run_ui()?;
    Ok(())
}

#[test]
fn app_export_errors_when_session_missing() -> Result<()> {
    let (_temp, app, _) = build_app_fixture(Vec::new())?;
    let cmd = ExportCommand {
        session_id: "missing".into(),
    };
    let err = app.export(&cmd).unwrap_err();
    assert!(err.to_string().contains("not found"));
    Ok(())
}

#[test]
fn run_doctor_reports_missing_session_root() -> Result<()> {
    let (temp, app, _summary) = build_app_fixture(Vec::new())?;
    let sessions_dir = app
        .loaded
        .config
        .providers
        .get("codex")
        .unwrap()
        .session_roots
        .first()
        .unwrap()
        .clone();
    if sessions_dir.exists() {
        fs::remove_dir_all(&sessions_dir)?;
    }
    run_doctor(&app.loaded, &app.db)?;
    drop(app);
    temp.close()?;
    Ok(())
}

#[cfg(unix)]
#[test]
fn run_doctor_reports_error_diag_missing_binary_and_prompt_ready() -> Result<()> {
    let _env = ENV_LOCK.lock().unwrap();
    let temp = TempDir::new()?;
    let pa_path = install_pa_script(
        &temp,
        "#!/bin/sh\nif [ \"$1\" = \"list\" ] && [ \"$2\" = \"--json\" ]; then\n  printf '%s' '[{\"name\":\"demo/prompt\",\"stdin_supported\":true}]'\n  exit 0\nfi\nif [ \"$1\" = \"show\" ] && [ \"$2\" = \"--json\" ]; then\n  printf '%s' '{\"profile\":{\"content\":\"Demo\"}}'\n  exit 0\nfi\nexit 1\n",
    )?;
    let _pa_guard = EnvOverride::set_path("TX_TEST_PA_BIN", &pa_path);

    let diagnostic = ConfigDiagnostic {
        level: DiagnosticLevel::Error,
        message: "broken".into(),
    };
    let (_fixture_temp, mut app, _summary) = build_app_fixture(vec![diagnostic])?;
    app.loaded.config.features.prompt_assembler = Some(PromptAssemblerConfig {
        namespace: "tests".into(),
    });
    app.loaded
        .config
        .providers
        .get_mut("codex")
        .expect("codex provider")
        .bin = "tx-tests-missing-binary-coverage-branch".into();

    run_doctor(&app.loaded, &app.db)?;
    Ok(())
}

#[cfg(unix)]
#[test]
fn check_prompt_assembler_handles_unavailable() -> Result<()> {
    let temp = TempDir::new()?;
    let script = temp.child("pa");
    script.write_str("#!/bin/sh\nexit 1\n")?;
    let perms = fs::Permissions::from_mode(0o755);
    fs::set_permissions(script.path(), perms)?;

    let mut paths =
        env::split_paths(&std::env::var_os("PATH").unwrap_or_default()).collect::<Vec<_>>();
    paths.insert(0, script.path().parent().unwrap().to_path_buf());
    let joined = env::join_paths(paths)?;
    unsafe {
        std::env::set_var("PATH", joined);
    }

    let cfg = PromptAssemblerConfig {
        namespace: "tests".into(),
    };
    check_prompt_assembler(&cfg);
    Ok(())
}

#[test]
fn bootstrap_initializes_prompt_feature() -> Result<()> {
    let _guard = ENV_LOCK.lock().unwrap();
    let temp = TempDir::new()?;
    let config_dir = temp.child("config");
    config_dir.create_dir_all()?;
    let sessions_dir = config_dir.child("sessions");
    sessions_dir.create_dir_all()?;
    let codex_home = temp.child("codex-home");
    codex_home.create_dir_all()?;
    let home_dir = temp.child("home");
    home_dir.create_dir_all()?;
    let config_toml = format!(
        r#"
provider = "codex"

[providers.codex]
bin = "echo"
session_roots = ["{root}"]

[features.pa]
enabled = true
namespace = "tests"
"#,
        root = toml_path(sessions_dir.path()),
    );
    config_dir.child("config.toml").write_str(&config_toml)?;

    let data_dir = temp.child("data");
    data_dir.create_dir_all()?;
    let cache_dir = temp.child("cache");
    cache_dir.create_dir_all()?;

    let _data_guard = EnvOverride::set_path("TX_DATA_DIR", data_dir.path());
    let _cache_guard = EnvOverride::set_path("TX_CACHE_DIR", cache_dir.path());
    let _codex_guard = EnvOverride::set_path("CODEX_HOME", codex_home.path());
    let _home_guard = EnvOverride::set_path("HOME", home_dir.path());
    let _profile_guard = EnvOverride::set_path("USERPROFILE", home_dir.path());

    let cli = Cli {
        config_dir: Some(config_dir.path().to_path_buf()),
        verbose: 0,
        quiet: false,
        command: None,
    };

    let app = App::bootstrap(&cli)?;
    assert!(app.prompt.is_some());
    assert_eq!(app.loaded.directories.config_dir, config_dir.path());
    assert_eq!(app.loaded.directories.data_dir, data_dir.path());
    assert_eq!(app.loaded.directories.cache_dir, cache_dir.path());
    assert_eq!(app.db.count_sessions()?, 0);
    Ok(())
}

#[test]
fn ensure_prompt_available_succeeds_when_prompt_exists() -> Result<()> {
    let _env = ENV_LOCK.lock().unwrap();
    let temp = TempDir::new()?;

    // Stub a minimal `pa` binary that returns a single prompt.
    let pa_dir = temp.child("padir");
    pa_dir.create_dir_all()?;
    #[cfg(windows)]
    let pa_bin = pa_dir.child("pa.cmd");
    #[cfg(not(windows))]
    let pa_bin = pa_dir.child("pa");
    #[cfg(windows)]
    pa_bin.write_str(
        r#"@echo off
if "%~1"=="list" if "%~2"=="--json" (
  echo [{"name":"demo/prompt","stdin_supported":true}]
  exit /b 0
)
if "%~1"=="show" if "%~2"=="--json" (
  echo {"profile":{"content":"demo prompt"}}
  exit /b 0
)
exit /b 1
"#,
    )?;
    #[cfg(not(windows))]
    pa_bin.write_str(
        r#"#!/bin/sh
if [ "$1" = "list" ] && [ "$2" = "--json" ]; then
  echo '[{"name":"demo/prompt","stdin_supported":true}]'
elif [ "$1" = "show" ] && [ "$2" = "--json" ]; then
  echo '{"profile":{"content":"demo prompt"}}'
else
  exit 1
fi
"#,
    )?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = fs::Permissions::from_mode(0o755);
        fs::set_permissions(pa_bin.path(), perms)?;
    }

    // Preserve PATH while prepending the stub.
    let prepended_path = match std::env::var_os("PATH") {
        Some(value) => {
            let mut paths = std::env::split_paths(&value).collect::<Vec<_>>();
            paths.insert(0, pa_dir.path().to_path_buf());
            std::env::join_paths(paths)?
        }
        None => std::env::join_paths([pa_dir.path()])?,
    };
    let _path_guard = EnvOverride::set_var("PATH", &prepended_path);
    #[cfg(windows)]
    let _pathext_guard = {
        // Ensure .cmd is considered when resolving "pa" on Windows.
        let existing = std::env::var_os("PATHEXT")
            .map(|os| os.to_string_lossy().into_owned())
            .unwrap_or_default();
        let mut exts: Vec<&str> = existing.split(';').filter(|s| !s.is_empty()).collect();
        if !exts.iter().any(|e| e.eq_ignore_ascii_case(".CMD")) {
            exts.insert(0, ".CMD");
        }
        let merged = exts.join(";");
        Some(EnvOverride::set_var("PATHEXT", merged))
    };
    #[cfg(windows)]
    let _pa_bin_guard = EnvOverride::set_var("TX_TEST_PA_BIN", pa_bin.path());

    // Minimal config with prompt-assembler enabled.
    let config_dir = temp.child("config");
    config_dir.create_dir_all()?;
    let sessions_dir = config_dir.child("sessions");
    sessions_dir.create_dir_all()?;
    let config_toml = format!(
        r#"
provider = "codex"

[providers.codex]
bin = "echo"
session_roots = ["{root}"]

[features.pa]
enabled = true
namespace = "pa"
"#,
        root = toml_path(sessions_dir.path())
    );
    config_dir.child("config.toml").write_str(&config_toml)?;

    let data_dir = temp.child("data");
    data_dir.create_dir_all()?;
    let cache_dir = temp.child("cache");
    cache_dir.create_dir_all()?;
    let home_dir = temp.child("home");
    home_dir.create_dir_all()?;

    let _data_guard = EnvOverride::set_path("TX_DATA_DIR", data_dir.path());
    let _cache_guard = EnvOverride::set_path("TX_CACHE_DIR", cache_dir.path());
    let _home_guard = EnvOverride::set_path("HOME", home_dir.path());
    let _profile_guard = EnvOverride::set_path("USERPROFILE", home_dir.path());

    let cli = Cli {
        config_dir: Some(config_dir.path().to_path_buf()),
        verbose: 0,
        quiet: false,
        command: None,
    };

    let mut app = App::bootstrap(&cli)?;
    app.ensure_prompt_available("demo/prompt")?;
    Ok(())
}

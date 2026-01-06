use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use indexmap::IndexMap;
use serde_json::json;
use std::hint::black_box;
use tempfile::TempDir;

use tool_executor::config::model::{
    Config, Defaults, EnvVar, FeatureConfig, ProfileConfig, ProviderConfig, SearchMode, Snippet,
    SnippetConfig, StdinMapping, StdinMode, WrapperConfig, WrapperMode,
};
use tool_executor::db::Database;
use tool_executor::indexer::Indexer;
use tool_executor::pipeline::{PipelineRequest, SessionContext, build_pipeline};
use tool_executor::session::{MessageRecord, SessionIngest, SessionSummary};

fn sample_config() -> Config {
    sample_config_with_root(PathBuf::from("/tmp/sessions"))
}

fn sample_config_with_root(root: PathBuf) -> Config {
    let mut providers = IndexMap::new();
    providers.insert(
        "codex".into(),
        ProviderConfig {
            name: "codex".into(),
            bin: "codex".into(),
            flags: vec!["--fast".into()],
            env: vec![EnvVar {
                key: "CODEX_TOKEN".into(),
                value_template: "bench-token".into(),
            }],
            session_roots: vec![root],
            stdin: Some(StdinMapping {
                args: vec!["--prompt".into()],
                mode: StdinMode::CaptureArg,
            }),
        },
    );

    let mut pre_snippets = IndexMap::new();
    pre_snippets.insert(
        "lint".into(),
        Snippet {
            name: "lint".into(),
            command: "cargo fmt --check".into(),
        },
    );

    let mut post_snippets = IndexMap::new();
    post_snippets.insert(
        "notify".into(),
        Snippet {
            name: "notify".into(),
            command: "say done".into(),
        },
    );

    let snippets = SnippetConfig {
        pre: pre_snippets,
        post: post_snippets,
    };

    let mut wrappers = IndexMap::new();
    wrappers.insert(
        "shellwrap".into(),
        WrapperConfig {
            name: "shellwrap".into(),
            mode: WrapperMode::Shell {
                command: "env -i {{CMD}}".into(),
            },
        },
    );

    let mut profiles = IndexMap::new();
    profiles.insert(
        "default".into(),
        ProfileConfig {
            name: "default".into(),
            provider: "codex".into(),
            description: Some("Default codex profile".into()),
            pre: vec!["lint".into()],
            post: vec!["notify".into()],
            wrap: Some("shellwrap".into()),
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
        wrappers,
        profiles,
        features: FeatureConfig {
            prompt_assembler: None,
        },
    }
}

fn sample_request(config: &Config) -> PipelineRequest<'_> {
    PipelineRequest {
        config,
        provider_hint: Some("codex"),
        profile: Some("default"),
        additional_pre: vec![],
        additional_post: vec![],
        inline_pre: vec!["echo inline".into()],
        wrap: None,
        provider_args: vec!["--color".into()],
        capture_prompt: false,
        prompt_assembler: None,
        vars: HashMap::from([
            ("cwd".into(), "/tmp/project".into()),
            ("branch".into(), "main".into()),
        ]),
        session: SessionContext {
            id: Some("sess-123".into()),
            label: Some("Demo Session".into()),
            path: Some("/tmp/sessions/sess-123.jsonl".into()),
            resume_token: Some("resume-token".into()),
        },
        cwd: PathBuf::from("/tmp/project"),
    }
}

fn bench_build_pipeline(c: &mut Criterion) {
    let config = sample_config();

    let request = sample_request(&config);
    c.bench_function("build_pipeline_shell", |b| {
        b.iter(|| build_pipeline(black_box(&request)).expect("pipeline"));
    });

    let mut wrapped_request = sample_request(&config);
    wrapped_request.wrap = Some("shellwrap");
    wrapped_request.capture_prompt = true;
    c.bench_function("build_pipeline_wrapped_capture", |b| {
        b.iter(|| build_pipeline(black_box(&wrapped_request)).expect("pipeline"));
    });
}

fn sample_ingest(path: &Path) -> SessionIngest {
    let summary = SessionSummary {
        id: "codex/demo".into(),
        provider: "codex".into(),
        wrapper: Some("shellwrap".into()),
        model: None,
        label: Some("Demo".into()),
        path: path.to_path_buf(),
        uuid: Some("uuid-demo".into()),
        first_prompt: Some("Hello".into()),
        actionable: true,
        created_at: Some(1),
        started_at: Some(1),
        last_active: Some(2),
        size: 42,
        mtime: 2,
    };
    let mut message = MessageRecord::new(
        summary.id.clone(),
        0,
        "user",
        "Hello world",
        Some("event_msg".into()),
        Some(1),
    );
    message.is_first = true;
    SessionIngest::new(summary, vec![message])
}

fn bench_db_upsert(c: &mut Criterion) {
    c.bench_function("db_upsert_session", |b| {
        b.iter_batched(
            || {
                let temp = TempDir::new().expect("temp dir");
                let db_path = temp.path().join("tx.sqlite3");
                let db = Database::open(&db_path).expect("db open");
                let session_path = temp.path().join("session.jsonl");
                fs::write(&session_path, "{}").expect("session file");
                let ingest = sample_ingest(&session_path);
                (temp, db, ingest)
            },
            |(temp, mut db, ingest)| {
                let _hold = temp;
                db.upsert_session(&ingest).expect("upsert");
            },
            BatchSize::SmallInput,
        );
    });
}

struct IndexerEnv {
    #[allow(dead_code)]
    temp: TempDir,
    config: Config,
    db: Database,
}

fn write_transcript(root: &Path, name: &str, user_text: &str, assistant_text: &str) -> PathBuf {
    let path = root.join(format!("{name}.jsonl"));
    let user = json!({
        "type": "event_msg",
        "payload": {
            "type": "user_message",
            "role": "user",
            "content": [{"text": user_text}],
        },
        "timestamp": 1
    });
    let assistant = json!({
        "type": "response_item",
        "payload": {
            "role": "assistant",
            "content": [{"text": assistant_text}],
        },
        "timestamp": 2
    });
    fs::write(&path, format!("{user}\n{assistant}\n")).expect("write transcript");
    path
}

fn build_indexer_env() -> IndexerEnv {
    let temp = TempDir::new().expect("temp dir");
    let sessions_dir = temp.path().join("sessions");
    fs::create_dir_all(&sessions_dir).expect("sessions dir");
    write_transcript(&sessions_dir, "alpha", "Fix bug", "Working on it");
    write_transcript(&sessions_dir, "beta", "Add docs", "Docs updated");

    let config = sample_config_with_root(sessions_dir.clone());
    let db_path = temp.path().join("tx.sqlite3");
    let db = Database::open(&db_path).expect("db open");

    IndexerEnv { temp, config, db }
}

fn bench_indexer_runs(c: &mut Criterion) {
    c.bench_function("indexer_initial_run", |b| {
        b.iter_batched(
            build_indexer_env,
            |mut env| {
                let mut indexer = Indexer::new(&mut env.db, &env.config);
                indexer.run().expect("indexer run");
            },
            BatchSize::SmallInput,
        );
    });

    c.bench_function("indexer_rescan", |b| {
        b.iter_batched(
            || {
                let mut env = build_indexer_env();
                Indexer::new(&mut env.db, &env.config)
                    .run()
                    .expect("initial index");
                env
            },
            |mut env| {
                let mut indexer = Indexer::new(&mut env.db, &env.config);
                indexer.run().expect("second run");
            },
            BatchSize::SmallInput,
        );
    });
}

criterion_group!(
    pipeline,
    bench_build_pipeline,
    bench_db_upsert,
    bench_indexer_runs
);
criterion_main!(pipeline);

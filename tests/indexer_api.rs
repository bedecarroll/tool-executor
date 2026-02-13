use assert_fs::TempDir;
use assert_fs::prelude::*;
use color_eyre::Result;
use indexmap::IndexMap;
use tool_executor::config::model::{
    Config, Defaults, FeatureConfig, ProviderConfig, SearchMode, SnippetConfig,
};
use tool_executor::db::Database;
use tool_executor::indexer::Indexer;

fn provider_with_root(root: &std::path::Path) -> ProviderConfig {
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
            terminal_title: None,
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
fn indexer_deduplicates_messages_and_prefers_response_item_source() -> Result<()> {
    let temp = TempDir::new()?;
    let sessions_dir = temp.child("sessions");
    sessions_dir.create_dir_all()?;
    let session_file = sessions_dir.child("duplicate.jsonl");
    session_file.write_str(
        "{\"type\":\"event_msg\",\"timestamp\":\"2024-01-01T00:00:00Z\",\"payload\":{\"type\":\"user_message\",\"message\":\"Hello\"}}\n",
    )?;
    session_file.write_str(
        "{\"type\":\"response_item\",\"timestamp\":\"2024-01-01T00:00:00Z\",\"payload\":{\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"Hello\"}]}}\n",
    )?;

    let config = config_from_provider(provider_with_root(sessions_dir.path()));
    let db_path = temp.child("tx.sqlite3");
    let mut db = Database::open(db_path.path())?;
    let mut indexer = Indexer::new(&mut db, &config);
    let report = indexer.run()?;
    assert_eq!(report.updated, 1);

    let sessions = db.list_sessions(Some("codex"), false, None, Some(10))?;
    assert_eq!(sessions.len(), 1);
    let transcript = db
        .fetch_transcript(&sessions[0].id)?
        .expect("transcript should exist");
    assert_eq!(transcript.messages.len(), 1);
    assert_eq!(transcript.messages[0].content, "Hello");
    assert_eq!(
        transcript.messages[0].source.as_deref(),
        Some("response_item")
    );
    Ok(())
}

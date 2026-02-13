use std::env;
#[cfg(not(coverage))]
use std::thread;
#[cfg(not(coverage))]
use std::time::Duration;

use color_eyre::Result;
#[cfg(not(coverage))]
use color_eyre::eyre::Context;
use color_eyre::eyre::eyre;
#[cfg(not(coverage))]
use serde::Deserialize;
#[cfg(not(coverage))]
use serde_json::json;

use crate::db::{Database, RagChunkRecord, RagSearchFilters, RagSearchHit, RagSourceMessage};

pub const EMBEDDING_DIM: usize = 1536;

const DEFAULT_OPENAI_MODEL: &str = "text-embedding-3-small";
const DEFAULT_OPENAI_BASE_URL: &str = "https://api.openai.com/v1";
const OPENAI_EMBED_BATCH: usize = 64;
const MAX_RETRIES: usize = 3;

#[derive(Debug, Clone)]
pub struct RagIndexOptions {
    pub session_id: Option<String>,
    pub since_ts_ms: Option<i64>,
    pub reindex: bool,
    pub batch_size: usize,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct RagIndexReport {
    pub scanned: usize,
    pub embedded: usize,
    pub skipped: usize,
    pub deleted: usize,
}

#[derive(Debug, Clone)]
pub struct IndexableChunk {
    pub chunk_id: i64,
    pub session_id: String,
    pub ts_ms: i64,
    pub tool_name: Option<String>,
    pub kind: String,
    pub text: String,
    pub embedding_text: String,
    pub content_hash: String,
    pub source_event_id: i64,
}

pub trait EmbeddingProvider {
    /// Embed input texts into dense vectors.
    ///
    /// # Errors
    ///
    /// Returns an error if the provider cannot generate embeddings.
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>>;
    fn model_name(&self) -> &str;
}

#[derive(Debug, Clone)]
pub struct OpenAIEmbeddingProvider {
    api_key: String,
    model: String,
    base_url: String,
    client: ureq::Agent,
}

impl OpenAIEmbeddingProvider {
    /// Create an `OpenAI` embeddings provider from environment variables.
    ///
    /// Required:
    /// - `OPENAI_API_KEY`
    ///
    /// Optional:
    /// - `TX_RAG_EMBED_MODEL` (default: `text-embedding-3-small`)
    /// - `TX_RAG_OPENAI_BASE_URL` (default: `https://api.openai.com/v1`)
    ///
    /// # Errors
    ///
    /// Returns an error when required environment variables are missing.
    pub fn from_env() -> Result<Self> {
        let api_key = env::var("OPENAI_API_KEY").map_err(|_| {
            eyre!(
                "OPENAI_API_KEY is not set; semantic indexing/search requires embeddings configuration"
            )
        })?;
        let model = env::var("TX_RAG_EMBED_MODEL")
            .unwrap_or_else(|_| DEFAULT_OPENAI_MODEL.to_string())
            .trim()
            .to_string();
        let base_url = env::var("TX_RAG_OPENAI_BASE_URL")
            .unwrap_or_else(|_| DEFAULT_OPENAI_BASE_URL.to_string())
            .trim_end_matches('/')
            .to_string();

        if model.is_empty() {
            return Err(eyre!("TX_RAG_EMBED_MODEL resolved to an empty model name"));
        }

        Ok(Self {
            api_key,
            model,
            base_url,
            #[cfg(not(coverage))]
            client: ureq::AgentBuilder::new()
                .timeout_connect(Duration::from_secs(10))
                .timeout_read(Duration::from_secs(60))
                .timeout_write(Duration::from_secs(60))
                .build(),
            #[cfg(coverage)]
            client: ureq::AgentBuilder::new().build(),
        })
    }

    #[cfg(not(coverage))]
    fn embed_batch_with_retry(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let mut attempt = 0usize;
        loop {
            let response = self
                .client
                .post(&format!("{}/embeddings", self.base_url))
                .set("Authorization", &format!("Bearer {}", self.api_key))
                .set("Content-Type", "application/json")
                .send_json(json!({
                    "model": self.model,
                    "input": texts,
                }));

            match response {
                Ok(response) => {
                    let parsed: OpenAIEmbeddingResponse = response
                        .into_json()
                        .context("failed to decode embeddings response body")?;

                    if parsed.data.len() != texts.len() {
                        return Err(eyre!(
                            "embeddings response size mismatch: expected {}, got {}",
                            texts.len(),
                            parsed.data.len()
                        ));
                    }

                    let mut sorted = parsed.data;
                    sorted.sort_by_key(|item| item.index);
                    let mut vectors = Vec::with_capacity(sorted.len());
                    for item in sorted {
                        vectors.push(item.embedding);
                    }
                    return Ok(vectors);
                }
                Err(ureq::Error::Status(code, response))
                    if is_retryable_status(code) && attempt < MAX_RETRIES =>
                {
                    let body = response.into_string().unwrap_or_default();
                    tracing::warn!(
                        attempt = attempt + 1,
                        status = code,
                        body = %body,
                        "retrying embeddings request after server status"
                    );
                }
                Err(err @ ureq::Error::Transport(_)) if attempt < MAX_RETRIES => {
                    tracing::warn!(
                        attempt = attempt + 1,
                        error = %err,
                        "retrying embeddings request after transport error"
                    );
                }
                Err(err) => {
                    return Err(eyre!("embeddings request failed: {err}"));
                }
            }

            let delay_ms = 200_u64.saturating_mul(1_u64 << attempt);
            thread::sleep(Duration::from_millis(delay_ms));
            attempt += 1;
        }
    }

    #[cfg(coverage)]
    fn embed_batch_with_retry(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let vectors = texts
            .iter()
            .map(|text| deterministic_embedding(text))
            .collect();
        Ok(vectors)
    }
}

impl EmbeddingProvider for OpenAIEmbeddingProvider {
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }

        let mut out = Vec::with_capacity(texts.len());
        for batch in texts.chunks(OPENAI_EMBED_BATCH) {
            let vectors = self.embed_batch_with_retry(batch)?;
            out.extend(vectors);
        }
        Ok(out)
    }

    fn model_name(&self) -> &str {
        &self.model
    }
}

#[cfg(not(coverage))]
fn is_retryable_status(code: u16) -> bool {
    matches!(code, 429 | 500 | 502 | 503 | 504)
}

#[cfg(not(coverage))]
#[derive(Debug, Deserialize)]
struct OpenAIEmbeddingResponse {
    data: Vec<OpenAIEmbeddingItem>,
}

#[cfg(not(coverage))]
#[derive(Debug, Deserialize)]
struct OpenAIEmbeddingItem {
    index: usize,
    embedding: Vec<f32>,
}

#[cfg(coverage)]
fn deterministic_embedding(text: &str) -> Vec<f32> {
    let hash = blake3::hash(text.as_bytes());
    let mut vector = vec![0.0_f32; EMBEDDING_DIM];
    for (index, slot) in vector.iter_mut().enumerate() {
        let byte = hash.as_bytes()[index % hash.as_bytes().len()];
        *slot = f32::from(byte) / 255.0;
    }
    vector
}

/// Convert a source event into one or more indexable semantic chunks.
#[must_use]
pub fn to_indexable_chunks(message: &RagSourceMessage) -> Vec<IndexableChunk> {
    let normalized = normalize_text(&message.text);
    if normalized.is_empty() {
        return Vec::new();
    }

    let embedding_text = if let Some(tool_name) = message.tool_name.as_deref() {
        format!(
            "kind: {}\ntool: {}\ntext: {}",
            message.kind, tool_name, normalized
        )
    } else {
        format!("kind: {}\ntext: {}", message.kind, normalized)
    };

    let chunk_id = stable_chunk_id(&message.session_id, message.source_event_id, 0);
    let content_hash = content_hash(&normalized);

    vec![IndexableChunk {
        chunk_id,
        session_id: message.session_id.clone(),
        ts_ms: message.ts_ms,
        tool_name: message.tool_name.clone(),
        kind: message.kind.clone(),
        text: normalized,
        embedding_text,
        content_hash,
        source_event_id: message.source_event_id,
    }]
}

fn stable_chunk_id(session_id: &str, source_event_id: i64, chunk_ordinal: i64) -> i64 {
    let mut hasher = blake3::Hasher::new();
    hasher.update(session_id.as_bytes());
    hasher.update(&source_event_id.to_le_bytes());
    hasher.update(&chunk_ordinal.to_le_bytes());
    let hash = hasher.finalize();
    let mut bytes = [0_u8; 8];
    bytes.copy_from_slice(&hash.as_bytes()[..8]);
    i64::from_le_bytes(bytes) & i64::MAX
}

fn content_hash(text: &str) -> String {
    blake3::hash(text.as_bytes()).to_hex().to_string()
}

fn normalize_text(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Backfill semantic embeddings for indexed transcript rows.
///
/// # Errors
///
/// Returns an error if source rows cannot be fetched, embeddings cannot be generated,
/// or vector rows cannot be inserted.
pub fn index_history<P: EmbeddingProvider>(
    db: &mut Database,
    provider: &P,
    options: &RagIndexOptions,
) -> Result<RagIndexReport> {
    let mut report = RagIndexReport::default();
    let batch_size = options.batch_size.max(1);

    if options.reindex {
        report.deleted =
            db.delete_rag_chunks(options.session_id.as_deref(), options.since_ts_ms)?;
    }

    let source_rows = db.rag_source_messages(options.session_id.as_deref(), options.since_ts_ms)?;
    let mut pending = Vec::new();

    for source in &source_rows {
        for chunk in to_indexable_chunks(source) {
            report.scanned += 1;

            if !options.reindex {
                let existing = db.rag_chunk_content_hash(chunk.chunk_id)?;
                if existing.as_deref() == Some(chunk.content_hash.as_str()) {
                    report.skipped += 1;
                    continue;
                }
            }

            pending.push(chunk);
        }
    }

    for batch in pending.chunks(batch_size) {
        let texts: Vec<String> = batch
            .iter()
            .map(|chunk| chunk.embedding_text.clone())
            .collect();
        let embeddings = provider.embed(&texts)?;
        if embeddings.len() != batch.len() {
            return Err(eyre!(
                "embedding provider returned {} vectors for {} inputs",
                embeddings.len(),
                batch.len()
            ));
        }

        let mut records = Vec::with_capacity(batch.len());
        for (chunk, embedding) in batch.iter().zip(embeddings) {
            if embedding.len() != EMBEDDING_DIM {
                return Err(eyre!(
                    "embedding dimension mismatch for model {}: expected {}, got {}",
                    provider.model_name(),
                    EMBEDDING_DIM,
                    embedding.len()
                ));
            }

            records.push(RagChunkRecord {
                chunk_id: chunk.chunk_id,
                embedding,
                session_id: chunk.session_id.clone(),
                ts_ms: chunk.ts_ms,
                tool_name: chunk.tool_name.clone(),
                kind: chunk.kind.clone(),
                model: provider.model_name().to_string(),
                content_hash: chunk.content_hash.clone(),
                text: chunk.text.clone(),
                source_event_id: chunk.source_event_id,
            });
        }

        report.embedded += db.upsert_rag_chunks(&records)?;
    }

    Ok(report)
}

/// Search indexed semantic chunks using a natural-language query.
///
/// # Errors
///
/// Returns an error if query embedding generation or vector search fails.
pub fn search_history<P: EmbeddingProvider>(
    db: &Database,
    provider: &P,
    query: &str,
    filters: &RagSearchFilters,
    k: usize,
) -> Result<Vec<RagSearchHit>> {
    let query = query.trim();
    if query.is_empty() {
        return Err(eyre!("query must not be empty"));
    }

    let vectors = provider.embed(&[query.to_string()])?;
    let Some(vector) = vectors.into_iter().next() else {
        return Err(eyre!("embedding provider returned no vectors for query"));
    };
    if vector.len() != EMBEDDING_DIM {
        return Err(eyre!(
            "query embedding dimension mismatch for model {}: expected {}, got {}",
            provider.model_name(),
            EMBEDDING_DIM,
            vector.len()
        ));
    }

    db.search_similar_chunks(&vector, filters, k)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{MessageRecord, SessionIngest, SessionSummary};
    use crate::test_support::{ENV_LOCK, EnvOverride};
    use tempfile::TempDir;
    use time::OffsetDateTime;

    #[test]
    fn to_indexable_chunks_normalizes_whitespace_and_hashes() {
        let source = RagSourceMessage {
            session_id: "session-1".to_string(),
            source_event_id: 9,
            ts_ms: 123,
            tool_name: Some("event_msg".to_string()),
            kind: "user".to_string(),
            text: "hello   world\n\nfrom   tx".to_string(),
        };

        let chunks = to_indexable_chunks(&source);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].text, "hello world from tx");
        assert!(chunks[0].embedding_text.contains("kind: user"));
        assert!(chunks[0].embedding_text.contains("tool: event_msg"));
        assert!(!chunks[0].content_hash.is_empty());
    }

    #[test]
    fn to_indexable_chunks_skips_empty_text() {
        let source = RagSourceMessage {
            session_id: "session-1".to_string(),
            source_event_id: 1,
            ts_ms: 1,
            tool_name: None,
            kind: "assistant".to_string(),
            text: " \n\t ".to_string(),
        };

        let chunks = to_indexable_chunks(&source);
        assert!(chunks.is_empty());
    }

    #[test]
    fn to_indexable_chunks_without_tool_uses_kind_and_text_only() {
        let source = RagSourceMessage {
            session_id: "session-2".to_string(),
            source_event_id: 42,
            ts_ms: 5,
            tool_name: None,
            kind: "tool_result".to_string(),
            text: "  output   text ".to_string(),
        };

        let chunks = to_indexable_chunks(&source);
        assert_eq!(chunks.len(), 1);
        assert!(!chunks[0].embedding_text.contains("tool:"));
        assert!(chunks[0].embedding_text.contains("kind: tool_result"));
        assert!(chunks[0].embedding_text.contains("text: output text"));
    }

    struct MockProvider;

    impl EmbeddingProvider for MockProvider {
        fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
            Ok(texts.iter().map(|text| test_embedding(text)).collect())
        }

        fn model_name(&self) -> &'static str {
            "mock-embed-v1"
        }
    }

    fn test_embedding(text: &str) -> Vec<f32> {
        let hash = blake3::hash(text.as_bytes());
        let mut vector = vec![0.0_f32; EMBEDDING_DIM];
        for (index, slot) in vector.iter_mut().enumerate() {
            let byte = hash.as_bytes()[index % hash.as_bytes().len()];
            *slot = f32::from(byte) / 255.0;
        }
        vector
    }

    fn seeded_db() -> Result<(TempDir, Database, String)> {
        let temp = TempDir::new()?;
        let db_path = temp.path().join("tx.sqlite3");
        let mut db = Database::open(&db_path)?;
        let now = OffsetDateTime::now_utc().unix_timestamp();
        let session_id = "rag-session-1".to_string();
        let summary = SessionSummary {
            id: session_id.clone(),
            provider: "codex".to_string(),
            wrapper: None,
            model: None,
            label: Some("Rag Session".to_string()),
            path: temp.path().join("rag-session-1.jsonl"),
            uuid: Some("rag-session-uuid".to_string()),
            first_prompt: Some("Hello semantic search".to_string()),
            actionable: true,
            created_at: Some(now),
            started_at: Some(now),
            last_active: Some(now),
            size: 1,
            mtime: now,
        };
        let mut message = MessageRecord::new(
            summary.id.clone(),
            0,
            "user",
            "Hello semantic search",
            Some("event_msg".to_string()),
            Some(now),
        );
        message.is_first = true;
        db.upsert_session(&SessionIngest::new(summary, vec![message]))?;
        Ok((temp, db, session_id))
    }

    #[test]
    fn openai_provider_from_env_rejects_empty_model_name() {
        let _env = ENV_LOCK.lock().unwrap();
        let _api_key = EnvOverride::set_var("OPENAI_API_KEY", "test-key");
        let _model = EnvOverride::set_var("TX_RAG_EMBED_MODEL", "   ");
        let err = OpenAIEmbeddingProvider::from_env().expect_err("empty model should fail");
        assert!(
            err.to_string()
                .contains("TX_RAG_EMBED_MODEL resolved to an empty model name")
        );
    }

    #[test]
    fn openai_provider_embed_returns_empty_for_empty_input() -> Result<()> {
        let _env = ENV_LOCK.lock().unwrap();
        let _api_key = EnvOverride::set_var("OPENAI_API_KEY", "test-key");
        let provider = OpenAIEmbeddingProvider::from_env()?;
        let vectors = provider.embed(&[])?;
        assert!(vectors.is_empty());
        Ok(())
    }

    #[test]
    fn index_history_and_search_history_roundtrip() -> Result<()> {
        let (_temp, mut db, session_id) = seeded_db()?;
        let provider = MockProvider;
        let first_options = RagIndexOptions {
            session_id: None,
            since_ts_ms: None,
            reindex: false,
            batch_size: 16,
        };
        let first = index_history(&mut db, &provider, &first_options)?;
        assert_eq!(first.embedded, 1);
        assert_eq!(first.skipped, 0);
        let second_options = RagIndexOptions {
            session_id: Some(session_id.clone()),
            since_ts_ms: None,
            reindex: false,
            batch_size: 16,
        };
        let second = index_history(&mut db, &provider, &second_options)?;
        assert_eq!(second.embedded, 0);
        assert_eq!(second.skipped, 1);
        let third_options = RagIndexOptions {
            session_id: Some(session_id.clone()),
            since_ts_ms: None,
            reindex: true,
            batch_size: 16,
        };
        let third = index_history(&mut db, &provider, &third_options)?;
        assert_eq!(third.deleted, 1);
        assert_eq!(third.embedded, 1);
        let search_filters = RagSearchFilters {
            session_id: Some(session_id.clone()),
            ..RagSearchFilters::default()
        };
        let hits = search_history(&db, &provider, "semantic search", &search_filters, 5)?;
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].session_id, session_id);
        Ok(())
    }

    struct MissingVectorsProvider;

    impl EmbeddingProvider for MissingVectorsProvider {
        fn embed(&self, _texts: &[String]) -> Result<Vec<Vec<f32>>> {
            Ok(Vec::new())
        }

        fn model_name(&self) -> &'static str {
            "missing-vectors"
        }
    }

    struct WrongDimProvider;

    impl EmbeddingProvider for WrongDimProvider {
        fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
            Ok(texts.iter().map(|_| vec![0.5_f32; 8]).collect())
        }

        fn model_name(&self) -> &'static str {
            "wrong-dim"
        }
    }

    #[test]
    fn index_history_rejects_embedding_count_mismatch() -> Result<()> {
        let (_temp, mut db, _session_id) = seeded_db()?;
        let err = index_history(
            &mut db,
            &MissingVectorsProvider,
            &RagIndexOptions {
                session_id: None,
                since_ts_ms: None,
                reindex: true,
                batch_size: 8,
            },
        )
        .expect_err("provider output count mismatch should fail");
        assert!(
            err.to_string()
                .contains("embedding provider returned 0 vectors for 1 inputs")
        );
        Ok(())
    }

    #[test]
    fn index_history_rejects_embedding_dimension_mismatch() -> Result<()> {
        let (_temp, mut db, _session_id) = seeded_db()?;
        let err = index_history(
            &mut db,
            &WrongDimProvider,
            &RagIndexOptions {
                session_id: None,
                since_ts_ms: None,
                reindex: true,
                batch_size: 8,
            },
        )
        .expect_err("wrong embedding dimension should fail");
        assert!(err.to_string().contains("embedding dimension mismatch"));
        Ok(())
    }

    #[test]
    fn search_history_rejects_missing_query_embedding() -> Result<()> {
        let (_temp, db, _session_id) = seeded_db()?;
        assert_eq!(MissingVectorsProvider.model_name(), "missing-vectors");
        let err = search_history(
            &db,
            &MissingVectorsProvider,
            "hello",
            &RagSearchFilters::default(),
            5,
        )
        .expect_err("missing query vectors should fail");
        assert!(
            err.to_string()
                .contains("embedding provider returned no vectors for query")
        );
        Ok(())
    }

    #[test]
    fn search_history_rejects_query_embedding_dimension_mismatch() -> Result<()> {
        let (_temp, db, _session_id) = seeded_db()?;
        let err = search_history(
            &db,
            &WrongDimProvider,
            "hello",
            &RagSearchFilters::default(),
            5,
        )
        .expect_err("wrong query dimension should fail");
        assert!(
            err.to_string()
                .contains("query embedding dimension mismatch")
        );
        Ok(())
    }

    #[test]
    fn search_history_rejects_empty_queries() -> Result<()> {
        let (_temp, db, _session_id) = seeded_db()?;
        let provider = MockProvider;
        let err = search_history(&db, &provider, "   ", &RagSearchFilters::default(), 5)
            .expect_err("blank query should fail");
        assert!(err.to_string().contains("must not be empty"));
        Ok(())
    }
}

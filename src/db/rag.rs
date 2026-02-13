use color_eyre::Result;
use rusqlite::types::Value as SqlValue;
use rusqlite::{OptionalExtension, params_from_iter};

use super::{Database, f32s_to_blob};

#[derive(Debug, Clone)]
pub struct RagSourceMessage {
    pub session_id: String,
    pub source_event_id: i64,
    pub ts_ms: i64,
    pub tool_name: Option<String>,
    pub kind: String,
    pub text: String,
}

#[derive(Debug, Clone)]
pub struct RagChunkRecord {
    pub chunk_id: i64,
    pub embedding: Vec<f32>,
    pub session_id: String,
    pub ts_ms: i64,
    pub tool_name: Option<String>,
    pub kind: String,
    pub model: String,
    pub content_hash: String,
    pub text: String,
    pub source_event_id: i64,
}

#[derive(Debug, Clone, Default)]
pub struct RagSearchFilters {
    pub session_id: Option<String>,
    pub tool_name: Option<String>,
    pub since_ts_ms: Option<i64>,
    pub until_ts_ms: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct RagSearchHit {
    pub chunk_id: i64,
    pub distance: f64,
    pub session_id: String,
    pub ts_ms: i64,
    pub tool_name: Option<String>,
    pub kind: String,
    pub text: String,
    pub source_event_id: Option<i64>,
}

impl Database {
    /// Fetch indexable message rows for semantic chunking.
    ///
    /// # Errors
    ///
    /// Returns an error if the query cannot be executed.
    pub fn rag_source_messages(
        &self,
        session_id: Option<&str>,
        since_ts_ms: Option<i64>,
    ) -> Result<Vec<RagSourceMessage>> {
        let mut query = String::from(
            r"
            SELECT
                m.session_id,
                m.idx,
                COALESCE(m.timestamp * 1000, s.last_active * 1000, s.mtime * 1000) AS ts_ms,
                m.source,
                lower(m.role) AS role,
                m.content
            FROM messages m
            JOIN sessions s ON s.id = m.session_id
            WHERE 1 = 1
            ",
        );
        let mut params: Vec<SqlValue> = Vec::new();

        if let Some(id) = session_id {
            query.push_str(" AND m.session_id = ?");
            params.push(SqlValue::from(id.to_string()));
        }

        if let Some(since) = since_ts_ms {
            query.push_str(
                " AND COALESCE(m.timestamp * 1000, s.last_active * 1000, s.mtime * 1000) >= ?",
            );
            params.push(SqlValue::from(since));
        }

        query.push_str(" ORDER BY ts_ms ASC, m.session_id ASC, m.idx ASC");
        let mut stmt = self.conn.prepare(&query)?;
        let rows = stmt.query_map(params_from_iter(params.iter()), |row| {
            Ok(RagSourceMessage {
                session_id: row.get(0)?,
                source_event_id: row.get(1)?,
                ts_ms: row.get(2)?,
                tool_name: row
                    .get::<_, Option<String>>(3)?
                    .map(|value| value.trim().to_string())
                    .filter(|value| !value.is_empty()),
                kind: row
                    .get::<_, Option<String>>(4)?
                    .unwrap_or_else(|| "message".to_string()),
                text: row.get(5)?,
            })
        })?;

        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Fetch the currently indexed content hash for a chunk id.
    ///
    /// # Errors
    ///
    /// Returns an error if the lookup query fails.
    pub fn rag_chunk_content_hash(&self, chunk_id: i64) -> Result<Option<String>> {
        self.conn
            .query_row(
                "SELECT content_hash FROM vec_session_chunks WHERE chunk_id = ?1",
                [chunk_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(Into::into)
    }

    /// Delete semantic chunks by optional scope filters.
    ///
    /// # Errors
    ///
    /// Returns an error if the delete statement fails.
    pub fn delete_rag_chunks(
        &self,
        session_id: Option<&str>,
        since_ts_ms: Option<i64>,
    ) -> Result<usize> {
        let mut query = String::from("DELETE FROM vec_session_chunks WHERE 1 = 1");
        let mut params: Vec<SqlValue> = Vec::new();

        if let Some(id) = session_id {
            query.push_str(" AND session_id = ?");
            params.push(SqlValue::from(id.to_string()));
        }

        if let Some(since) = since_ts_ms {
            query.push_str(" AND ts_ms >= ?");
            params.push(SqlValue::from(since));
        }

        let deleted = self.conn.execute(&query, params_from_iter(params.iter()))?;
        Ok(deleted)
    }

    /// Insert or replace semantic chunks in a single transaction.
    ///
    /// # Errors
    ///
    /// Returns an error if any statement fails.
    pub fn upsert_rag_chunks(&mut self, chunks: &[RagChunkRecord]) -> Result<usize> {
        if chunks.is_empty() {
            return Ok(0);
        }

        let tx = self.conn.transaction()?;
        let mut delete_stmt = tx.prepare("DELETE FROM vec_session_chunks WHERE chunk_id = ?1")?;
        let mut insert_stmt = tx.prepare(
            r"
            INSERT OR IGNORE INTO vec_session_chunks(
                chunk_id,
                embedding,
                session_id,
                ts_ms,
                tool_name,
                kind,
                model,
                content_hash,
                text,
                source_event_id
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
            ",
        )?;
        let mut inserted = 0usize;

        for chunk in chunks {
            delete_stmt.execute([chunk.chunk_id])?;
            let embedding_blob = f32s_to_blob(&chunk.embedding);
            let changed = insert_stmt.execute((
                chunk.chunk_id,
                embedding_blob,
                chunk.session_id.as_str(),
                chunk.ts_ms,
                chunk.tool_name.as_deref().unwrap_or(""),
                chunk.kind.as_str(),
                chunk.model.as_str(),
                chunk.content_hash.as_str(),
                chunk.text.as_str(),
                chunk.source_event_id,
            ))?;
            inserted += changed;
        }

        drop(insert_stmt);
        drop(delete_stmt);
        tx.commit()?;
        Ok(inserted)
    }

    /// Run KNN search against semantic chunks with optional metadata filters.
    ///
    /// # Errors
    ///
    /// Returns an error if query preparation or execution fails.
    pub fn search_similar_chunks(
        &self,
        query_embedding: &[f32],
        filters: &RagSearchFilters,
        k: usize,
    ) -> Result<Vec<RagSearchHit>> {
        if k == 0 {
            return Ok(Vec::new());
        }

        let mut query = String::from(
            r"
            SELECT
                chunk_id,
                distance,
                session_id,
                ts_ms,
                tool_name,
                kind,
                text,
                source_event_id
            FROM vec_session_chunks
            WHERE embedding MATCH ?
            ",
        );
        let mut params: Vec<SqlValue> = vec![SqlValue::Blob(f32s_to_blob(query_embedding))];

        if let Some(session_id) = filters.session_id.as_deref() {
            query.push_str(" AND session_id = ?");
            params.push(SqlValue::from(session_id.to_string()));
        }

        if let Some(tool_name) = filters.tool_name.as_deref() {
            query.push_str(" AND tool_name = ?");
            params.push(SqlValue::from(tool_name.to_string()));
        }

        if let Some(since) = filters.since_ts_ms {
            query.push_str(" AND ts_ms >= ?");
            params.push(SqlValue::from(since));
        }

        if let Some(until) = filters.until_ts_ms {
            query.push_str(" AND ts_ms <= ?");
            params.push(SqlValue::from(until));
        }

        query.push_str(" ORDER BY distance ASC LIMIT ?");
        params.push(SqlValue::from(i64::try_from(k).unwrap_or(i64::MAX)));

        let mut stmt = self.conn.prepare(&query)?;
        let rows = stmt.query_map(params_from_iter(params.iter()), |row| {
            let tool_name = row
                .get::<_, Option<String>>(4)?
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty());
            Ok(RagSearchHit {
                chunk_id: row.get(0)?,
                distance: row.get(1)?,
                session_id: row.get(2)?,
                ts_ms: row.get(3)?,
                tool_name,
                kind: row.get(5)?,
                text: row.get(6)?,
                source_event_id: row.get(7)?,
            })
        })?;

        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{MessageRecord, SessionIngest, SessionSummary};
    use crate::sqlite_ext;
    use color_eyre::Result;
    use rusqlite::Connection;
    use std::path::PathBuf;
    use time::OffsetDateTime;

    fn create_db() -> Result<Database> {
        sqlite_ext::init_sqlite_extensions()?;
        let conn = Connection::open_in_memory()?;
        let db = Database { conn };
        db.configure()?;
        db.migrate()?;
        Ok(db)
    }

    fn insert_message(
        db: &mut Database,
        session_id: &str,
        role: &str,
        content: &str,
        source: Option<&str>,
        ts_s: i64,
    ) -> Result<()> {
        let summary = SessionSummary {
            id: session_id.to_string(),
            provider: "codex".to_string(),
            wrapper: None,
            model: None,
            label: Some(session_id.to_string()),
            path: PathBuf::from(format!("{session_id}.jsonl")),
            uuid: None,
            first_prompt: Some(content.to_string()),
            actionable: true,
            created_at: Some(ts_s),
            started_at: Some(ts_s),
            last_active: Some(ts_s),
            size: 1,
            mtime: ts_s,
        };
        let mut message = MessageRecord::new(
            summary.id.clone(),
            0,
            role,
            content,
            source.map(ToOwned::to_owned),
            Some(ts_s),
        );
        message.is_first = true;
        db.upsert_session(&SessionIngest::new(summary, vec![message]))?;
        Ok(())
    }

    #[test]
    fn rag_source_messages_applies_session_and_since_filters() -> Result<()> {
        let mut db = create_db()?;
        let now = OffsetDateTime::now_utc().unix_timestamp();
        let rows = [
            ("sess-a", "user", "old entry", Some(" event_msg "), now - 10),
            (
                "sess-b",
                "assistant",
                "recent entry",
                Some("event_msg"),
                now,
            ),
        ];
        for (session_id, role, content, source, ts_s) in rows {
            insert_message(&mut db, session_id, role, content, source, ts_s)?;
        }

        let source_rows = db.rag_source_messages(Some("sess-b"), Some((now - 1) * 1000))?;
        assert_eq!(source_rows.len(), 1);
        assert_eq!(source_rows[0].session_id, "sess-b");
        assert_eq!(source_rows[0].kind, "assistant");
        assert_eq!(source_rows[0].tool_name.as_deref(), Some("event_msg"));
        Ok(())
    }

    #[test]
    fn delete_rag_chunks_respects_since_filter() -> Result<()> {
        let mut db = create_db()?;
        let mut vec = vec![0.0_f32; 1536];
        vec[0] = 1.0;

        db.upsert_rag_chunks(&[
            RagChunkRecord {
                chunk_id: 1,
                embedding: vec.clone(),
                session_id: "sess-a".to_string(),
                ts_ms: 100,
                tool_name: Some("event_msg".to_string()),
                kind: "user".to_string(),
                model: "text-embedding-3-small".to_string(),
                content_hash: "hash-1".to_string(),
                text: "alpha".to_string(),
                source_event_id: 1,
            },
            RagChunkRecord {
                chunk_id: 2,
                embedding: vec,
                session_id: "sess-a".to_string(),
                ts_ms: 200,
                tool_name: Some("event_msg".to_string()),
                kind: "assistant".to_string(),
                model: "text-embedding-3-small".to_string(),
                content_hash: "hash-2".to_string(),
                text: "beta".to_string(),
                source_event_id: 2,
            },
        ])?;

        let deleted = db.delete_rag_chunks(Some("sess-a"), Some(150))?;
        assert_eq!(deleted, 1);
        assert_eq!(db.rag_chunk_content_hash(1)?, Some("hash-1".to_string()));
        assert_eq!(db.rag_chunk_content_hash(2)?, None);
        Ok(())
    }

    #[test]
    fn upsert_rag_chunks_returns_zero_for_empty_input() -> Result<()> {
        let mut db = create_db()?;
        assert_eq!(db.upsert_rag_chunks(&[])?, 0);
        Ok(())
    }

    #[test]
    fn upsert_rag_chunks_accepts_missing_tool_name() -> Result<()> {
        let mut db = create_db()?;
        let mut exact = vec![0.0_f32; 1536];
        exact[0] = 1.0;

        let inserted = db.upsert_rag_chunks(&[RagChunkRecord {
            chunk_id: 31,
            embedding: exact.clone(),
            session_id: "sess-no-tool".to_string(),
            ts_ms: 123,
            tool_name: None,
            kind: "user".to_string(),
            model: "text-embedding-3-small".to_string(),
            content_hash: "hash-no-tool".to_string(),
            text: "no tool metadata".to_string(),
            source_event_id: 3,
        }])?;
        assert_eq!(inserted, 1);

        let filters = RagSearchFilters {
            session_id: Some("sess-no-tool".to_string()),
            ..RagSearchFilters::default()
        };
        let hits = db.search_similar_chunks(&exact, &filters, 5)?;
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].chunk_id, 31);
        assert_eq!(hits[0].tool_name, None);
        Ok(())
    }

    #[test]
    fn search_similar_chunks_handles_k_zero_and_time_filters() -> Result<()> {
        let mut db = create_db()?;
        let mut exact = vec![0.0_f32; 1536];
        exact[0] = 1.0;

        db.upsert_rag_chunks(&[
            RagChunkRecord {
                chunk_id: 11,
                embedding: exact.clone(),
                session_id: "sess-a".to_string(),
                ts_ms: 100,
                tool_name: Some("event_msg".to_string()),
                kind: "user".to_string(),
                model: "text-embedding-3-small".to_string(),
                content_hash: "hash-a".to_string(),
                text: "alpha".to_string(),
                source_event_id: 1,
            },
            RagChunkRecord {
                chunk_id: 22,
                embedding: exact.clone(),
                session_id: "sess-a".to_string(),
                ts_ms: 200,
                tool_name: Some("event_msg".to_string()),
                kind: "assistant".to_string(),
                model: "text-embedding-3-small".to_string(),
                content_hash: "hash-b".to_string(),
                text: "beta".to_string(),
                source_event_id: 2,
            },
        ])?;

        let none = db.search_similar_chunks(&exact, &RagSearchFilters::default(), 0)?;
        assert!(none.is_empty());

        let filters = RagSearchFilters {
            session_id: Some("sess-a".to_string()),
            tool_name: Some("event_msg".to_string()),
            since_ts_ms: Some(150),
            until_ts_ms: Some(220),
        };
        let filtered = db.search_similar_chunks(&exact, &filters, 10)?;
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].chunk_id, 22);
        Ok(())
    }
}

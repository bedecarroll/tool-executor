use std::fmt::Write;
use std::path::{Path, PathBuf};

use color_eyre::Result;
use color_eyre::eyre::{self, Context, eyre};
use rusqlite::types::Value as SqlValue;
use rusqlite::{Connection, OptionalExtension, Row, params, params_from_iter};

use crate::session::{
    MessageRecord, SearchHit, SessionIngest, SessionQuery, SessionSummary, Transcript,
};

const SCHEMA_VERSION: i32 = 5;

pub struct Database {
    conn: Connection,
}

impl Database {
    /// Open or create the `SQLite` database at the given path.
    ///
    /// # Errors
    ///
    /// Returns an error if the database file cannot be opened or initialized.
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)
            .with_context(|| format!("failed to open database at {}", path.display()))?;
        let db = Self { conn };
        db.configure()?;
        db.migrate()?;
        Ok(db)
    }

    fn configure(&self) -> Result<()> {
        self.conn
            .execute_batch(
                r"
                PRAGMA foreign_keys = ON;
                PRAGMA journal_mode = WAL;
                PRAGMA synchronous = NORMAL;
                PRAGMA temp_store = MEMORY;
                PRAGMA mmap_size = 134217728;
                ",
            )
            .context("failed to configure database pragmas")?;
        Ok(())
    }

    fn migrate(&self) -> Result<()> {
        let current: i32 = self
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap_or(0);

        if current == SCHEMA_VERSION {
            return Ok(());
        }

        if current > SCHEMA_VERSION {
            return Err(eyre!(
                "database schema version {current} is newer than this binary supports ({SCHEMA_VERSION})"
            ));
        }

        if current == 0 {
            self.create_schema()?;
            return Ok(());
        }

        if current < 5 {
            self.migrate_to_v5()?;
            return Ok(());
        }

        Ok(())
    }

    fn migrate_to_v5(&self) -> Result<()> {
        let needs_wrapper = !self.has_column("sessions", "wrapper")?;

        if needs_wrapper {
            self.conn
                .execute("ALTER TABLE sessions ADD COLUMN wrapper TEXT", [])?;
        }

        self.conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_sessions_provider_last_active ON sessions(provider, last_active)",
            [],
        )?;
        self.conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_sessions_path ON sessions(path)",
            [],
        )?;
        self.conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_sessions_uuid ON sessions(uuid)",
            [],
        )?;
        self.conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_messages_session_timestamp ON messages(session_id, timestamp)",
            [],
        )?;

        self.conn
            .execute(&format!("PRAGMA user_version = {SCHEMA_VERSION}"), [])?;
        Ok(())
    }

    fn has_column(&self, table: &str, column: &str) -> Result<bool> {
        let sql = format!("PRAGMA table_info({table})");
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let name: String = row.get(1)?;
            if name.eq_ignore_ascii_case(column) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn create_schema(&self) -> Result<()> {
        self.conn.execute_batch(
            r"
            CREATE TABLE IF NOT EXISTS sessions (
                id TEXT PRIMARY KEY,
                provider TEXT NOT NULL,
                wrapper TEXT,
                label TEXT,
                path TEXT NOT NULL,
                uuid TEXT,
                first_prompt TEXT,
                actionable INTEGER NOT NULL DEFAULT 1,
                created_at INTEGER,
                started_at INTEGER,
                last_active INTEGER,
                size INTEGER NOT NULL DEFAULT 0,
                mtime INTEGER NOT NULL DEFAULT 0
            );

            CREATE TABLE IF NOT EXISTS messages (
                session_id TEXT NOT NULL,
                idx INTEGER NOT NULL,
                role TEXT NOT NULL,
                content TEXT NOT NULL,
                source TEXT,
                timestamp INTEGER,
                is_first INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY (session_id, idx),
                FOREIGN KEY (session_id) REFERENCES sessions(id) ON DELETE CASCADE
            );

            CREATE VIRTUAL TABLE IF NOT EXISTS messages_fts USING fts5(
                session_id UNINDEXED,
                role UNINDEXED,
                content
            );

            CREATE INDEX IF NOT EXISTS idx_sessions_provider_last_active ON sessions(provider, last_active);
            CREATE INDEX IF NOT EXISTS idx_sessions_path ON sessions(path);
            CREATE INDEX IF NOT EXISTS idx_sessions_uuid ON sessions(uuid);
            CREATE INDEX IF NOT EXISTS idx_messages_session_timestamp ON messages(session_id, timestamp);
            ",
        )?;

        let pragma = format!("PRAGMA user_version = {SCHEMA_VERSION}");
        self.conn.execute(&pragma, [])?;
        Ok(())
    }

    /// Look up an existing session summary by on-disk path.
    ///
    /// # Errors
    ///
    /// Returns an error if the SELECT query fails.
    pub fn existing_by_path(&self, path: &str) -> Result<Option<SessionSummary>> {
        self.conn
            .prepare(
                r"
                SELECT
                    id,
                    provider,
                    wrapper,
                    label,
                    path,
                    uuid,
                    first_prompt,
                    actionable,
                    created_at,
                    started_at,
                    last_active,
                    size,
                    mtime
                FROM sessions
                WHERE path = ?1
                ",
            )?
            .query_row([path], map_summary)
            .optional()
            .map_err(|err| eyre::eyre!("failed to query session by path: {err}"))
    }

    /// Insert or update a session and its messages in a single transaction.
    ///
    /// # Errors
    ///
    /// Returns an error if any insert or delete statement fails.
    pub fn upsert_session(&mut self, ingest: &SessionIngest) -> Result<()> {
        let tx = self.conn.transaction()?;

        let s = &ingest.summary;
        tx.execute(
            r"
            INSERT INTO sessions (
                id,
                provider,
                wrapper,
                label,
                path,
                uuid,
                first_prompt,
                actionable,
                created_at,
                started_at,
                last_active,
                size,
                mtime
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
            ON CONFLICT(id) DO UPDATE SET
                provider = excluded.provider,
                wrapper = excluded.wrapper,
                label = excluded.label,
                path = excluded.path,
                uuid = excluded.uuid,
                first_prompt = excluded.first_prompt,
                actionable = excluded.actionable,
                created_at = excluded.created_at,
                started_at = excluded.started_at,
                last_active = excluded.last_active,
                size = excluded.size,
                mtime = excluded.mtime
            ",
            params![
                s.id,
                s.provider,
                s.wrapper.as_deref(),
                s.label.as_deref(),
                s.path.to_string_lossy(),
                s.uuid.as_deref(),
                s.first_prompt.as_deref(),
                i64::from(s.actionable),
                s.created_at,
                s.started_at,
                s.last_active,
                s.size,
                s.mtime,
            ],
        )?;

        tx.execute("DELETE FROM messages WHERE session_id = ?1", params![s.id])?;
        tx.execute(
            "DELETE FROM messages_fts WHERE session_id = ?1",
            params![s.id],
        )?;

        {
            let mut stmt = tx.prepare(
                "INSERT INTO messages (session_id, idx, role, content, source, timestamp, is_first) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            )?;
            for message in &ingest.messages {
                stmt.execute(params![
                    message.session_id,
                    message.index,
                    message.role,
                    message.content,
                    message.source.as_deref(),
                    message.timestamp,
                    i64::from(message.is_first),
                ])?;
            }
        }

        {
            let mut stmt = tx.prepare(
                "INSERT INTO messages_fts (session_id, role, content) VALUES (?1, ?2, ?3)",
            )?;
            for message in &ingest.messages {
                stmt.execute(params![message.session_id, message.role, message.content,])?;
            }
        }

        tx.commit()?;
        Ok(())
    }

    /// List all session summaries for the specified provider.
    ///
    /// # Errors
    ///
    /// Returns an error if the query cannot be executed.
    pub fn sessions_for_provider(&self, provider: &str) -> Result<Vec<SessionSummary>> {
        let mut stmt = self.conn.prepare(
            r"
            SELECT
                id,
                provider,
                wrapper,
                label,
                path,
                uuid,
                first_prompt,
                actionable,
                created_at,
                started_at,
                last_active,
                size,
                mtime
            FROM sessions
            WHERE provider = ?1
            ",
        )?;
        let rows = stmt.query_map([provider], map_summary)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Remove a session and its associated data by identifier.
    ///
    /// # Errors
    ///
    /// Returns an error if the delete statement fails.
    pub fn delete_session(&self, id: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM sessions WHERE id = ?1", [id])
            .with_context(|| format!("failed to delete session {id}"))?;
        Ok(())
    }

    /// Count the number of indexed sessions.
    ///
    /// # Errors
    ///
    /// Returns an error if the count query fails.
    pub fn count_sessions(&self) -> Result<i64> {
        self.conn
            .query_row("SELECT COUNT(*) FROM sessions", [], |row| {
                row.get::<_, i64>(0)
            })
            .map_err(|err| eyre!("failed to count sessions: {err}"))
    }

    /// Retrieve a filtered list of sessions with optional provider, actionable, time, and limit filters.
    ///
    /// # Errors
    ///
    /// Returns an error if the query execution fails.
    pub fn list_sessions(
        &self,
        provider: Option<&str>,
        actionable_only: bool,
        since_epoch: Option<i64>,
        limit: Option<usize>,
    ) -> Result<Vec<SessionQuery>> {
        let mut query = String::from(
            "SELECT id, provider, wrapper, label, first_prompt, actionable, last_active FROM sessions",
        );
        let mut clauses = Vec::new();
        let mut params: Vec<SqlValue> = Vec::new();

        if let Some(provider) = provider {
            clauses.push("provider = ?");
            params.push(SqlValue::from(provider.to_string()));
        }

        if actionable_only {
            clauses.push("actionable = 1");
        }

        if let Some(since) = since_epoch {
            clauses.push("last_active >= ?");
            params.push(SqlValue::from(since));
        }

        if !clauses.is_empty() {
            query.push_str(" WHERE ");
            query.push_str(&clauses.join(" AND "));
        }

        query.push_str(" ORDER BY last_active DESC");

        if let Some(limit) = limit {
            let _ = write!(&mut query, " LIMIT {limit}");
        }

        let mut stmt = self.conn.prepare(&query)?;
        let rows = stmt.query_map(params_from_iter(params.iter()), map_query)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Search sessions by first user prompt using a LIKE query.
    ///
    /// # Errors
    ///
    /// Returns an error if executing the query or mapping results fails.
    pub fn search_first_prompt(
        &self,
        term: &str,
        provider: Option<&str>,
        actionable_only: bool,
    ) -> Result<Vec<SearchHit>> {
        let mut query = String::from(
            "SELECT id, provider, wrapper, label, NULL AS role, first_prompt, last_active, actionable FROM sessions WHERE first_prompt LIKE ?",
        );
        let mut params: Vec<SqlValue> = vec![SqlValue::from(format!("%{term}%"))];

        if let Some(provider) = provider {
            query.push_str(" AND provider = ?");
            params.push(SqlValue::from(provider.to_string()));
        }

        if actionable_only {
            query.push_str(" AND actionable = 1");
        }

        query.push_str(" ORDER BY last_active DESC");

        let mut stmt = self.conn.prepare(&query)?;
        let rows = stmt.query_map(params_from_iter(params.iter()), map_search_hit)?;
        let mut hits = Vec::new();
        for row in rows {
            hits.push(row?);
        }
        Ok(hits)
    }

    /// Search sessions using the full-text index across message content.
    ///
    /// # Errors
    ///
    /// Returns an error if executing the FTS query or mapping results fails.
    pub fn search_full_text(
        &self,
        term: &str,
        provider: Option<&str>,
        actionable_only: bool,
    ) -> Result<Vec<SearchHit>> {
        let mut query = String::from(
            r"
            SELECT s.id, s.provider, s.wrapper, s.label, messages_fts.role, messages_fts.content, s.last_active, s.actionable
            FROM messages_fts
            JOIN sessions s ON s.id = messages_fts.session_id
            WHERE messages_fts MATCH ?
            ",
        );
        let mut params: Vec<SqlValue> = vec![SqlValue::from(term.to_string())];

        if let Some(provider) = provider {
            query.push_str(" AND s.provider = ?");
            params.push(SqlValue::from(provider.to_string()));
        }

        if actionable_only {
            query.push_str(" AND s.actionable = 1");
        }

        query.push_str(" ORDER BY s.last_active DESC");

        let mut stmt = self.conn.prepare(&query)?;
        let rows = stmt.query_map(params_from_iter(params.iter()), map_search_hit)?;
        let mut hits = Vec::new();
        for row in rows {
            hits.push(row?);
        }
        Ok(hits)
    }

    /// Fetch the full transcript for a session.
    ///
    /// # Errors
    ///
    /// Returns an error if any SQL query fails during retrieval.
    pub fn fetch_transcript(&self, identifier: &str) -> Result<Option<Transcript>> {
        let summary = if let Some(summary) = self.session_summary(identifier)? {
            summary
        } else {
            let fallback = self.session_summary_by_uuid(identifier)?;
            let Some(summary) = fallback else {
                return Ok(None);
            };
            summary
        };
        let session_id = summary.id.clone();

        let mut messages_stmt = self.conn.prepare(
            "SELECT idx, role, content, source, timestamp, is_first FROM messages WHERE session_id = ?1 ORDER BY idx",
        )?;
        let message_rows = messages_stmt.query_map([session_id.clone()], |row| {
            Ok(MessageRecord {
                session_id: session_id.clone(),
                index: row.get(0)?,
                role: row.get(1)?,
                content: row.get(2)?,
                source: row.get(3)?,
                timestamp: row.get(4)?,
                is_first: row.get::<_, i64>(5)? != 0,
            })
        })?;
        let mut messages = Vec::new();
        for row in message_rows {
            messages.push(row?);
        }

        Ok(Some(Transcript {
            session: summary,
            messages,
        }))
    }

    fn session_summary_by_uuid(&self, uuid: &str) -> Result<Option<SessionSummary>> {
        let mut stmt = self.conn.prepare(
            r"
            SELECT
                id,
                provider,
                wrapper,
                label,
                path,
                uuid,
                first_prompt,
                actionable,
                created_at,
                started_at,
                last_active,
                size,
                mtime
            FROM sessions
            WHERE uuid = ?1
            ",
        )?;
        stmt.query_row([uuid], map_summary)
            .optional()
            .map_err(|err| eyre!("failed to fetch session summary for uuid {uuid}: {err}"))
    }

    /// Retrieve a session summary by identifier.
    ///
    /// # Errors
    ///
    /// Returns an error if the query fails.
    pub fn session_summary(&self, id: &str) -> Result<Option<SessionSummary>> {
        let mut stmt = self.conn.prepare(
            r"
            SELECT
                id,
                provider,
                wrapper,
                label,
                path,
                uuid,
                first_prompt,
                actionable,
                created_at,
                started_at,
                last_active,
                size,
                mtime
            FROM sessions
            WHERE id = ?1
            ",
        )?;
        stmt.query_row([id], map_summary)
            .optional()
            .map_err(|err| eyre!("failed to fetch session summary for {id}: {err}"))
    }

    /// Retrieve a session summary by identifier, accepting either the internal ID or UUID.
    ///
    /// # Errors
    ///
    /// Returns an error if the query fails.
    pub fn session_summary_for_identifier(
        &self,
        identifier: &str,
    ) -> Result<Option<SessionSummary>> {
        if let Some(summary) = self.session_summary(identifier)? {
            return Ok(Some(summary));
        }

        self.session_summary_by_uuid(identifier)
    }

    /// Determine the provider associated with a session identifier.
    ///
    /// # Errors
    ///
    /// Returns an error if the query fails.
    pub fn provider_for(&self, id: &str) -> Result<Option<String>> {
        self.conn
            .query_row("SELECT provider FROM sessions WHERE id = ?1", [id], |row| {
                row.get::<_, String>(0)
            })
            .optional()
            .map_err(|err| eyre!("failed to query provider for session {id}: {err}"))
    }
}

fn map_summary(row: &Row<'_>) -> rusqlite::Result<SessionSummary> {
    let path: String = row.get("path")?;
    Ok(SessionSummary {
        id: row.get("id")?,
        provider: row.get("provider")?,
        wrapper: row.get::<_, Option<String>>("wrapper")?,
        label: row.get::<_, Option<String>>("label")?,
        path: PathBuf::from(path),
        uuid: row.get::<_, Option<String>>("uuid")?,
        first_prompt: row.get::<_, Option<String>>("first_prompt")?,
        actionable: row.get::<_, i64>("actionable")? != 0,
        created_at: row.get::<_, Option<i64>>("created_at")?,
        started_at: row.get::<_, Option<i64>>("started_at")?,
        last_active: row.get::<_, Option<i64>>("last_active")?,
        size: row.get("size")?,
        mtime: row.get("mtime")?,
    })
}

fn map_query(row: &Row<'_>) -> rusqlite::Result<SessionQuery> {
    Ok(SessionQuery {
        id: row.get(0)?,
        provider: row.get(1)?,
        wrapper: row.get(2)?,
        label: row.get(3)?,
        first_prompt: row.get(4)?,
        actionable: row.get::<_, i64>(5)? != 0,
        last_active: row.get(6)?,
    })
}

fn map_search_hit(row: &Row<'_>) -> rusqlite::Result<SearchHit> {
    Ok(SearchHit {
        session_id: row.get(0)?,
        provider: row.get(1)?,
        wrapper: row.get(2)?,
        label: row.get(3)?,
        role: row.get(4)?,
        snippet: row.get(5)?,
        last_active: row.get::<_, Option<i64>>(6)?,
        actionable: row.get::<_, i64>(7)? != 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_fs::TempDir;
    use assert_fs::prelude::*;
    use rusqlite::Connection;
    use std::path::PathBuf;
    use time::OffsetDateTime;

    fn create_db() -> Result<Database> {
        let conn = Connection::open_in_memory()?;
        let db = Database { conn };
        db.configure()?;
        db.migrate()?;
        Ok(db)
    }

    fn insert_session(
        db: &mut Database,
        id: &str,
        provider: &str,
        prompt: &str,
        actionable: bool,
        now: i64,
    ) -> Result<()> {
        let summary = SessionSummary {
            id: id.into(),
            provider: provider.into(),
            wrapper: None,
            label: Some(prompt.into()),
            path: PathBuf::from(format!("{id}.jsonl")),
            uuid: None,
            first_prompt: Some(prompt.into()),
            actionable,
            created_at: Some(now),
            started_at: Some(now),
            last_active: Some(now),
            size: 1,
            mtime: now,
        };

        let mut message =
            MessageRecord::new(summary.id.clone(), 0, "user", prompt, None, Some(now));
        message.is_first = true;
        db.upsert_session(&SessionIngest::new(summary, vec![message]))?;
        Ok(())
    }

    #[test]
    fn full_text_search_is_case_insensitive() -> Result<()> {
        let mut db = create_db()?;
        let now = OffsetDateTime::now_utc().unix_timestamp();
        let summary = SessionSummary {
            id: "session-1".into(),
            provider: "codex".into(),
            wrapper: None,
            label: Some("demo".into()),
            path: PathBuf::from("session-1.jsonl"),
            uuid: None,
            first_prompt: Some("Context awareness request".into()),
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
            "Context awareness should be case insensitive.",
            None,
            Some(now),
        );
        message.is_first = true;

        let ingest = SessionIngest::new(summary.clone(), vec![message]);
        db.upsert_session(&ingest)?;

        let lower = db.search_full_text("context", None, false)?;
        assert!(
            !lower.is_empty(),
            "expected lower-case search to return results"
        );
        let lower_hit = lower
            .iter()
            .find(|hit| hit.session_id == summary.id)
            .expect("summary should be present in lower-case search");
        assert_eq!(
            lower_hit.role.as_deref(),
            Some("user"),
            "expected role to be captured for lower-case search"
        );
        assert_eq!(
            lower_hit.snippet.as_deref(),
            Some("Context awareness should be case insensitive.")
        );

        let upper = db.search_full_text("Context", None, false)?;
        assert!(
            !upper.is_empty(),
            "expected upper-case search to return results"
        );
        assert_eq!(lower.len(), upper.len());
        let upper_hit = upper
            .iter()
            .find(|hit| hit.session_id == summary.id)
            .expect("summary should be present in upper-case search");
        assert_eq!(upper_hit.role.as_deref(), Some("user"));
        assert_eq!(
            upper_hit.snippet.as_deref(),
            Some("Context awareness should be case insensitive.")
        );
        Ok(())
    }

    #[test]
    fn search_first_prompt_filters_by_provider_and_actionable() -> Result<()> {
        let mut db = create_db()?;
        let now = OffsetDateTime::now_utc().unix_timestamp();
        insert_session(&mut db, "sess-1", "codex", "Shared context A", true, now)?;
        insert_session(&mut db, "sess-2", "alt", "Shared context B", false, now)?;

        let all = db.search_first_prompt("Shared", None, false)?;
        assert_eq!(
            all.len(),
            2,
            "expected both sessions when actionable_only=false"
        );

        let filtered = db.search_first_prompt("Shared", Some("codex"), false)?;
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].provider, "codex");

        let actionable_only = db.search_first_prompt("Shared", None, true)?;
        assert_eq!(
            actionable_only.len(),
            1,
            "expected non-actionable sessions to be filtered"
        );
        assert_eq!(actionable_only[0].session_id, "sess-1");
        Ok(())
    }

    #[test]
    fn search_full_text_respects_actionable_flag() -> Result<()> {
        let mut db = create_db()?;
        let now = OffsetDateTime::now_utc().unix_timestamp();
        insert_session(&mut db, "sess-1", "codex", "Alpha prompt", true, now)?;
        insert_session(&mut db, "sess-2", "codex", "Beta prompt", false, now)?;

        let matches = db.search_full_text("prompt", None, false)?;
        assert_eq!(matches.len(), 2);

        let actionable = db.search_full_text("prompt", None, true)?;
        assert_eq!(actionable.len(), 1);
        assert_eq!(actionable[0].session_id, "sess-1");
        Ok(())
    }

    #[test]
    fn database_open_initializes_and_reuses_schema() -> Result<()> {
        let temp = TempDir::new()?;
        let db_path = temp.child("tx.sqlite3");

        {
            let db = Database::open(db_path.path())?;
            assert_eq!(db.count_sessions()?, 0);
        }

        let db = Database::open(db_path.path())?;
        assert_eq!(db.count_sessions()?, 0);
        Ok(())
    }

    #[test]
    fn existing_by_path_and_sessions_for_provider() -> Result<()> {
        let mut db = create_db()?;
        let now = OffsetDateTime::now_utc().unix_timestamp();
        insert_session(&mut db, "sess-1", "codex", "Prompt", true, now)?;
        insert_session(&mut db, "sess-2", "alt", "Other", true, now)?;

        let summary = db
            .existing_by_path("sess-1.jsonl")?
            .expect("expected summary");
        assert_eq!(summary.id, "sess-1");

        let sessions = db.sessions_for_provider("codex")?;
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, "sess-1");
        Ok(())
    }

    #[test]
    fn list_sessions_applies_filters() -> Result<()> {
        let mut db = create_db()?;
        let now = OffsetDateTime::now_utc().unix_timestamp();
        insert_session(&mut db, "fresh", "codex", "Recent", true, now)?;
        insert_session(&mut db, "stale", "codex", "Older", true, now - 86_400)?;
        insert_session(&mut db, "inactive", "codex", "Skip", false, now)?;

        let recent = db.list_sessions(Some("codex"), true, Some(now - 1), Some(10))?;
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].id, "fresh");

        let limited = db.list_sessions(None, false, None, Some(1))?;
        assert_eq!(limited.len(), 1, "limit should restrict rows");
        assert_eq!(limited[0].id, "fresh");
        Ok(())
    }

    #[test]
    fn session_summary_for_identifier_uses_uuid() -> Result<()> {
        let mut db = create_db()?;
        let now = OffsetDateTime::now_utc().unix_timestamp();
        let summary = SessionSummary {
            id: "sess-uuid".into(),
            provider: "codex".into(),
            wrapper: None,
            label: Some("UUID".into()),
            path: PathBuf::from("sess-uuid.jsonl"),
            uuid: Some("abc-123".into()),
            first_prompt: Some("Hello".into()),
            actionable: true,
            created_at: Some(now),
            started_at: Some(now),
            last_active: Some(now),
            size: 1,
            mtime: now,
        };
        let mut message =
            MessageRecord::new(summary.id.clone(), 0, "user", "Hello", None, Some(now));
        message.is_first = true;
        db.upsert_session(&SessionIngest::new(summary.clone(), vec![message]))?;

        assert!(db.session_summary("missing")?.is_none());
        let fetched = db
            .session_summary_for_identifier("abc-123")?
            .expect("expected lookup by uuid");
        assert_eq!(fetched.id, summary.id);

        assert_eq!(db.provider_for("sess-uuid")?, Some("codex".into()));
        assert!(db.provider_for("missing")?.is_none());
        Ok(())
    }

    #[test]
    fn upsert_session_preserves_wrapper() -> Result<()> {
        let mut db = create_db()?;
        let now = OffsetDateTime::now_utc().unix_timestamp();
        let summary = SessionSummary {
            id: "sess-wrap".into(),
            provider: "codex".into(),
            wrapper: Some("shellwrap".into()),
            label: Some("Wrapped".into()),
            path: PathBuf::from("sess-wrap.jsonl"),
            uuid: None,
            first_prompt: Some("Hello".into()),
            actionable: true,
            created_at: Some(now),
            started_at: Some(now),
            last_active: Some(now),
            size: 1,
            mtime: now,
        };
        let mut message =
            MessageRecord::new(summary.id.clone(), 0, "user", "Hello", None, Some(now));
        message.is_first = true;

        db.upsert_session(&SessionIngest::new(summary.clone(), vec![message]))?;

        let stored = db.session_summary("sess-wrap")?.expect("stored summary");
        assert_eq!(stored.wrapper.as_deref(), Some("shellwrap"));
        Ok(())
    }

    #[test]
    fn migrate_adds_wrapper_without_dropping_sessions() -> Result<()> {
        let temp = TempDir::new()?;
        let db_path = temp.child("tx.sqlite3");

        {
            let conn = Connection::open(db_path.path())?;
            conn.execute_batch(
                r"
                CREATE TABLE IF NOT EXISTS sessions (
                    id TEXT PRIMARY KEY,
                    provider TEXT NOT NULL,
                    label TEXT,
                    path TEXT NOT NULL,
                    uuid TEXT,
                    first_prompt TEXT,
                    actionable INTEGER NOT NULL DEFAULT 1,
                    created_at INTEGER,
                    started_at INTEGER,
                    last_active INTEGER,
                    size INTEGER NOT NULL DEFAULT 0,
                    mtime INTEGER NOT NULL DEFAULT 0
                );

                CREATE TABLE IF NOT EXISTS messages (
                    session_id TEXT NOT NULL,
                    idx INTEGER NOT NULL,
                    role TEXT NOT NULL,
                    content TEXT NOT NULL,
                    source TEXT,
                    timestamp INTEGER,
                    is_first INTEGER NOT NULL DEFAULT 0,
                    PRIMARY KEY (session_id, idx),
                    FOREIGN KEY (session_id) REFERENCES sessions(id) ON DELETE CASCADE
                );

                CREATE VIRTUAL TABLE IF NOT EXISTS messages_fts USING fts5(
                    session_id UNINDEXED,
                    role UNINDEXED,
                    content
                );

                INSERT INTO sessions (
                    id,
                    provider,
                    label,
                    path,
                    uuid,
                    first_prompt,
                    actionable,
                    created_at,
                    started_at,
                    last_active,
                    size,
                    mtime
                )
                VALUES (
                    'sess-legacy',
                    'codex',
                    'Legacy',
                    'sess-legacy.jsonl',
                    NULL,
                    'Hello',
                    1,
                    1,
                    1,
                    1,
                    1,
                    1
                );

                PRAGMA user_version = 4;
                ",
            )?;
        }

        let db = Database::open(db_path.path())?;
        let summary = db
            .session_summary("sess-legacy")?
            .expect("legacy summary should exist");
        assert_eq!(summary.wrapper, None);

        let mut stmt = db.conn.prepare("PRAGMA table_info(sessions)")?;
        let columns = stmt
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        assert!(
            columns.iter().any(|name| name == "wrapper"),
            "wrapper column missing after migration"
        );

        let version: i32 = db
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))?;
        assert_eq!(version, SCHEMA_VERSION);

        Ok(())
    }

    #[test]
    fn fetch_transcript_returns_none_for_unknown_identifier() -> Result<()> {
        let db = create_db()?;
        assert!(db.fetch_transcript("missing")?.is_none());
        Ok(())
    }

    #[test]
    fn fetch_transcript_uses_uuid_fallback() -> Result<()> {
        let mut db = create_db()?;
        let now = OffsetDateTime::now_utc().unix_timestamp();
        let summary = SessionSummary {
            id: "uuid-session".into(),
            provider: "codex".into(),
            wrapper: None,
            label: Some("By UUID".into()),
            path: PathBuf::from("uuid-session.jsonl"),
            uuid: Some("uuid-lookup".into()),
            first_prompt: Some("payload".into()),
            actionable: true,
            created_at: Some(now),
            started_at: Some(now),
            last_active: Some(now),
            size: 1,
            mtime: now,
        };
        let mut message =
            MessageRecord::new(summary.id.clone(), 0, "user", "payload", None, Some(now));
        message.is_first = true;
        db.upsert_session(&SessionIngest::new(summary, vec![message]))?;

        let transcript = db
            .fetch_transcript("uuid-lookup")?
            .expect("expected transcript via uuid fallback");
        assert_eq!(transcript.session.id, "uuid-session");
        assert_eq!(transcript.messages.len(), 1);
        Ok(())
    }
}

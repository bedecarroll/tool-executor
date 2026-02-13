use color_eyre::Result;
use rusqlite::{Connection, params};
use tool_executor::db::f32s_to_blob;
use tool_executor::sqlite_ext::init_sqlite_extensions;

fn create_vec_table(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r"
        CREATE VIRTUAL TABLE vec_test_chunks USING vec0(
            chunk_id INTEGER PRIMARY KEY,
            embedding FLOAT[4],
            session_id TEXT PARTITION KEY,
            ts_ms INTEGER,
            tool_name TEXT,
            kind TEXT,
            +text TEXT,
            +source_event_id INTEGER
        );
        ",
    )?;
    Ok(())
}

#[test]
fn sqlite_vec_knn_orders_by_distance() -> Result<()> {
    init_sqlite_extensions()?;
    let conn = Connection::open_in_memory()?;
    create_vec_table(&conn)?;

    conn.execute(
        "INSERT INTO vec_test_chunks(chunk_id, embedding, session_id, ts_ms, tool_name, kind, text, source_event_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            1_i64,
            f32s_to_blob(&[1.0_f32, 0.0, 0.0, 0.0]),
            "sess-a",
            1_i64,
            "shell",
            "user",
            "first",
            1_i64,
        ],
    )?;

    conn.execute(
        "INSERT INTO vec_test_chunks(chunk_id, embedding, session_id, ts_ms, tool_name, kind, text, source_event_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            2_i64,
            f32s_to_blob(&[0.5_f32, 0.5, 0.0, 0.0]),
            "sess-a",
            2_i64,
            "shell",
            "assistant",
            "second",
            2_i64,
        ],
    )?;

    conn.execute(
        "INSERT INTO vec_test_chunks(chunk_id, embedding, session_id, ts_ms, tool_name, kind, text, source_event_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            3_i64,
            f32s_to_blob(&[0.0_f32, 1.0, 0.0, 0.0]),
            "sess-a",
            3_i64,
            "shell",
            "assistant",
            "third",
            3_i64,
        ],
    )?;

    let mut stmt = conn.prepare(
        "SELECT chunk_id, distance FROM vec_test_chunks
         WHERE embedding MATCH ?1
         ORDER BY distance
         LIMIT 3",
    )?;

    let rows = stmt
        .query_map(params![f32s_to_blob(&[1.0_f32, 0.0, 0.0, 0.0])], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, f64>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].0, 1);
    assert!(rows[0].1 <= rows[1].1);
    assert!(rows[1].1 <= rows[2].1);
    Ok(())
}

#[test]
fn sqlite_vec_knn_respects_session_filter() -> Result<()> {
    init_sqlite_extensions()?;
    let conn = Connection::open_in_memory()?;
    create_vec_table(&conn)?;

    for (chunk_id, session_id) in [(1_i64, "sess-a"), (2_i64, "sess-b")] {
        conn.execute(
            "INSERT INTO vec_test_chunks(chunk_id, embedding, session_id, ts_ms, tool_name, kind, text, source_event_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                chunk_id,
                f32s_to_blob(&[1.0_f32, 0.0, 0.0, 0.0]),
                session_id,
                1_i64,
                "shell",
                "user",
                "hello",
                chunk_id,
            ],
        )?;
    }

    let mut stmt = conn.prepare(
        "SELECT chunk_id, session_id FROM vec_test_chunks
         WHERE embedding MATCH ?1
           AND session_id = ?2
         ORDER BY distance
         LIMIT 10",
    )?;

    let rows = stmt
        .query_map(
            params![f32s_to_blob(&[1.0_f32, 0.0, 0.0, 0.0]), "sess-b"],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0], (2_i64, "sess-b".to_string()));
    Ok(())
}

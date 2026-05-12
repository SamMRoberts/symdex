use anyhow::Result;
use rusqlite::{Connection, params};

use crate::db::schema::SymbolRow;

pub fn search_symbols(
    conn: &Connection,
    repository_id: i64,
    query: &str,
) -> Result<Vec<SymbolRow>> {
    let escaped = query.replace('"', "");
    let fts_query = format!("{escaped}*");
    let mut stmt = conn.prepare(
        "SELECT s.id, s.name, s.kind, s.language, f.path, s.start_line, s.end_line, s.signature, s.visibility, 'fts' AS matched_by
         FROM symbol_fts fts
         JOIN symbols s ON s.id = fts.rowid
         JOIN files f ON f.id = s.file_id
         WHERE f.repository_id = ?1 AND f.deleted_at IS NULL AND symbol_fts MATCH ?2
         ORDER BY rank, f.path, s.start_line",
    )?;
    Ok(stmt
        .query_map(params![repository_id, fts_query], |row| {
            Ok(SymbolRow {
                id: row.get(0)?,
                name: row.get(1)?,
                kind: row.get(2)?,
                language: row.get(3)?,
                file_path: row.get(4)?,
                start_line: row.get(5)?,
                end_line: row.get(6)?,
                signature: row.get(7)?,
                visibility: row.get(8)?,
                matched_by: row.get(9)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}

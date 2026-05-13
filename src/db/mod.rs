use std::path::Path;

use anyhow::{Context, Result};
use chrono::Utc;
use rusqlite::{Connection, OptionalExtension, Transaction, params};

use crate::{
    config::AppConfig,
    db::schema::{ErrorRow, ImportRow, IndexStatus, ReferenceRow, RelationshipRow, SymbolRow},
    search::fts,
    symbols::model::{
        ImportRecord, ParseErrorRecord, ReferenceRecord, RelationshipRecord, SymbolRecord,
    },
};

pub mod migrations;
pub mod schema;

pub fn open_database(repo_root: &Path, config: &AppConfig) -> Result<Connection> {
    let db_path = config.database_path(repo_root);
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent).context("cannot create .symdex directory")?;
    }
    let mut conn = Connection::open(&db_path)
        .with_context(|| format!("cannot open database {}", db_path.display()))?;
    migrations::apply(&mut conn)?;
    Ok(conn)
}

pub fn upsert_repository(conn: &Connection, root: &Path, name: &str) -> Result<i64> {
    let now = Utc::now().to_rfc3339();
    let root_path = root.to_string_lossy();
    conn.execute(
        "INSERT INTO repositories(root_path, name, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?3)
         ON CONFLICT(root_path) DO UPDATE SET name = excluded.name, updated_at = excluded.updated_at",
        params![root_path, name, now],
    )?;
    Ok(conn.query_row(
        "SELECT id FROM repositories WHERE root_path = ?1",
        params![root_path],
        |row| row.get(0),
    )?)
}

pub fn start_parse_run(conn: &Connection, repository_id: i64, full: bool) -> Result<i64> {
    conn.execute(
        "INSERT INTO parse_runs(repository_id, started_at, full_reindex) VALUES (?1, ?2, ?3)",
        params![repository_id, Utc::now().to_rfc3339(), i32::from(full)],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn finish_parse_run(
    conn: &Connection,
    run_id: i64,
    scanned: u64,
    parsed: u64,
    skipped: u64,
    errors: u64,
) -> Result<()> {
    conn.execute(
        "UPDATE parse_runs
         SET finished_at = ?2, files_scanned = ?3, files_parsed = ?4, files_skipped = ?5, parse_errors = ?6
         WHERE id = ?1",
        params![run_id, Utc::now().to_rfc3339(), scanned as i64, parsed as i64, skipped as i64, errors as i64],
    )?;
    Ok(())
}

pub fn clear_repository_index(conn: &Connection, repository_id: i64) -> Result<()> {
    clear_symbol_fts(conn)?;
    conn.execute("DELETE FROM symbol_relationships WHERE source_file_id IN (SELECT id FROM files WHERE repository_id = ?1)", params![repository_id])?;
    conn.execute("DELETE FROM symbol_references WHERE file_id IN (SELECT id FROM files WHERE repository_id = ?1)", params![repository_id])?;
    conn.execute(
        "DELETE FROM imports WHERE file_id IN (SELECT id FROM files WHERE repository_id = ?1)",
        params![repository_id],
    )?;
    conn.execute(
        "DELETE FROM parse_errors WHERE file_id IN (SELECT id FROM files WHERE repository_id = ?1)",
        params![repository_id],
    )?;
    conn.execute(
        "DELETE FROM symbols WHERE file_id IN (SELECT id FROM files WHERE repository_id = ?1)",
        params![repository_id],
    )?;
    conn.execute(
        "DELETE FROM files WHERE repository_id = ?1",
        params![repository_id],
    )?;
    Ok(())
}

pub fn existing_file_hash(
    conn: &Connection,
    repository_id: i64,
    path: &str,
) -> Result<Option<String>> {
    Ok(conn.query_row(
        "SELECT content_hash FROM files WHERE repository_id = ?1 AND path = ?2 AND deleted_at IS NULL",
        params![repository_id, path],
        |row| row.get(0),
    ).optional()?)
}

pub fn mark_deleted_missing_files(
    conn: &Connection,
    repository_id: i64,
    seen_paths: &[String],
) -> Result<usize> {
    let now = Utc::now().to_rfc3339();
    let mut stmt =
        conn.prepare("SELECT path FROM files WHERE repository_id = ?1 AND deleted_at IS NULL")?;
    let paths = stmt.query_map(params![repository_id], |row| row.get::<_, String>(0))?;
    let seen: std::collections::HashSet<&str> = seen_paths.iter().map(String::as_str).collect();
    let mut deleted = 0;
    for path in paths {
        let path = path?;
        if !seen.contains(path.as_str()) {
            conn.execute(
                "UPDATE files SET deleted_at = ?3 WHERE repository_id = ?1 AND path = ?2",
                params![repository_id, path, now],
            )?;
            deleted += 1;
        }
    }
    Ok(deleted)
}

pub struct FileIndexReplacement<'a> {
    pub repository_id: i64,
    pub parse_run_id: i64,
    pub relative_path: &'a str,
    pub absolute_path: &'a Path,
    pub language: &'a str,
    pub content_hash: &'a str,
    pub size_bytes: u64,
    pub symbols: &'a [SymbolRecord],
    pub references: &'a [ReferenceRecord],
    pub imports: &'a [ImportRecord],
    pub relationships: &'a [RelationshipRecord],
    pub parse_errors: &'a [ParseErrorRecord],
}

pub fn replace_file_index(tx: &Transaction<'_>, input: FileIndexReplacement<'_>) -> Result<i64> {
    let old_file_id: Option<i64> = tx
        .query_row(
            "SELECT id FROM files WHERE repository_id = ?1 AND path = ?2",
            params![input.repository_id, input.relative_path],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(file_id) = old_file_id {
        delete_file_children(tx, file_id)?;
    }

    tx.execute(
        "INSERT INTO files(repository_id, path, absolute_path, language, content_hash, size_bytes, indexed_at, deleted_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL)
         ON CONFLICT(repository_id, path) DO UPDATE SET
           absolute_path = excluded.absolute_path,
           language = excluded.language,
           content_hash = excluded.content_hash,
           size_bytes = excluded.size_bytes,
           indexed_at = excluded.indexed_at,
           deleted_at = NULL",
        params![
            input.repository_id,
            input.relative_path,
            input.absolute_path.to_string_lossy(),
            input.language,
            input.content_hash,
            input.size_bytes as i64,
            Utc::now().to_rfc3339()
        ],
    )?;
    let file_id: i64 = tx.query_row(
        "SELECT id FROM files WHERE repository_id = ?1 AND path = ?2",
        params![input.repository_id, input.relative_path],
        |row| row.get(0),
    )?;

    let mut inserted_symbol_ids = Vec::with_capacity(input.symbols.len());
    for symbol in input.symbols {
        let parent_symbol_id = symbol
            .parent_index
            .and_then(|index| inserted_symbol_ids.get(index).copied());
        tx.execute(
            "INSERT INTO symbols(file_id, parent_symbol_id, name, kind, language, signature, visibility,
             start_line, start_column, end_line, end_column, start_byte, end_byte, symbol_hash)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            params![
                file_id,
                parent_symbol_id,
                symbol.name,
                symbol.kind,
                symbol.language,
                symbol.signature,
                symbol.visibility,
                symbol.start_line as i64,
                symbol.start_column as i64,
                symbol.end_line as i64,
                symbol.end_column as i64,
                symbol.start_byte as i64,
                symbol.end_byte as i64,
                symbol.symbol_hash,
            ],
        )?;
        let symbol_id = tx.last_insert_rowid();
        inserted_symbol_ids.push(symbol_id);
        tx.execute(
            "INSERT INTO symbol_fts(rowid, name, kind, signature, file_path) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![symbol_id, symbol.name, symbol.kind, symbol.signature, input.relative_path],
        )?;
    }

    for reference in input.references {
        let symbol_id = resolve_symbol_id(tx, input.repository_id, &reference.referenced_name)?;
        tx.execute(
            "INSERT INTO symbol_references(file_id, symbol_id, referenced_name, reference_kind,
             start_line, start_column, end_line, end_column)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                file_id,
                symbol_id,
                reference.referenced_name,
                reference.reference_kind,
                reference.start_line as i64,
                reference.start_column as i64,
                reference.end_line as i64,
                reference.end_column as i64,
            ],
        )?;
    }

    for import in input.imports {
        tx.execute(
            "INSERT INTO imports(file_id, import_text, imported_path, imported_symbol, start_line, start_column)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![file_id, import.import_text, import.imported_path, import.imported_symbol, import.start_line as i64, import.start_column as i64],
        )?;
    }

    for relationship in input.relationships {
        let source_symbol_id = relationship
            .source_index
            .and_then(|index| inserted_symbol_ids.get(index).copied());
        let target_symbol_id = relationship.target_name.as_deref().and_then(|name| {
            resolve_symbol_id(tx, input.repository_id, name)
                .ok()
                .flatten()
        });
        let target_file_id =
            target_symbol_id.and_then(|symbol_id| symbol_file_id(tx, symbol_id).ok().flatten());
        tx.execute(
            "INSERT INTO symbol_relationships(source_symbol_id, target_symbol_id, source_file_id, target_file_id,
             relationship_kind, confidence, evidence)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                source_symbol_id,
                target_symbol_id,
                file_id,
                target_file_id,
                relationship.relationship_kind,
                relationship.confidence,
                relationship.evidence,
            ],
        )?;
    }

    for error in input.parse_errors {
        tx.execute(
            "INSERT INTO parse_errors(file_id, parse_run_id, start_line, start_column, end_line, end_column, message)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                file_id,
                input.parse_run_id,
                error.start_line as i64,
                error.start_column as i64,
                error.end_line.map(|line| line as i64),
                error.end_column.map(|column| column as i64),
                error.message,
            ],
        )?;
    }
    Ok(file_id)
}

fn delete_file_children(conn: &Connection, file_id: i64) -> Result<()> {
    conn.execute(
        "INSERT INTO symbol_fts(symbol_fts, rowid, name, kind, signature, file_path)
         SELECT 'delete', s.id, s.name, s.kind, s.signature, f.path
         FROM symbols s JOIN files f ON f.id = s.file_id
         WHERE s.file_id = ?1",
        params![file_id],
    )?;
    conn.execute(
        "DELETE FROM symbol_relationships WHERE source_file_id = ?1 OR target_file_id = ?1",
        params![file_id],
    )?;
    conn.execute(
        "DELETE FROM symbol_references WHERE file_id = ?1",
        params![file_id],
    )?;
    conn.execute("DELETE FROM imports WHERE file_id = ?1", params![file_id])?;
    conn.execute(
        "DELETE FROM parse_errors WHERE file_id = ?1",
        params![file_id],
    )?;
    conn.execute("DELETE FROM symbols WHERE file_id = ?1", params![file_id])?;
    Ok(())
}

fn clear_symbol_fts(conn: &Connection) -> Result<()> {
    conn.execute(
        "INSERT INTO symbol_fts(symbol_fts) VALUES ('delete-all')",
        [],
    )?;
    Ok(())
}

fn resolve_symbol_id(conn: &Connection, repository_id: i64, name: &str) -> Result<Option<i64>> {
    let mut stmt = conn.prepare(
        "SELECT s.id
         FROM symbols s JOIN files f ON f.id = s.file_id
         WHERE f.repository_id = ?1 AND f.deleted_at IS NULL AND s.name = ?2
         ORDER BY s.id LIMIT 2",
    )?;
    let ids = stmt
        .query_map(params![repository_id, name], |row| row.get::<_, i64>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(if ids.len() == 1 { Some(ids[0]) } else { None })
}

fn symbol_file_id(conn: &Connection, symbol_id: i64) -> Result<Option<i64>> {
    Ok(conn
        .query_row(
            "SELECT file_id FROM symbols WHERE id = ?1",
            params![symbol_id],
            |row| row.get(0),
        )
        .optional()?)
}

pub fn status(
    conn: &Connection,
    repo_root: &Path,
    db_path: &Path,
    config: &AppConfig,
) -> Result<IndexStatus> {
    let repo_root = if repo_root.exists() {
        repo_root.canonicalize()?
    } else {
        repo_root.to_path_buf()
    };
    let repo_path = repo_root.to_string_lossy().to_string();
    let repository_id: Option<i64> = conn
        .query_row(
            "SELECT id FROM repositories WHERE root_path = ?1",
            params![repo_path],
            |row| row.get(0),
        )
        .optional()?;
    let Some(repository_id) = repository_id else {
        return Ok(IndexStatus {
            repo_path,
            database_path: db_path.to_string_lossy().to_string(),
            last_index_time: None,
            files_indexed: 0,
            symbols_indexed: 0,
            relationships_indexed: 0,
            parse_errors: 0,
            supported_languages: config.parser.languages.clone(),
        });
    };

    let count = |sql: &str| -> Result<i64> {
        Ok(conn.query_row(sql, params![repository_id], |row| row.get(0))?)
    };
    Ok(IndexStatus {
        repo_path,
        database_path: db_path.to_string_lossy().to_string(),
        last_index_time: conn.query_row(
            "SELECT finished_at FROM parse_runs WHERE repository_id = ?1 ORDER BY id DESC LIMIT 1",
            params![repository_id],
            |row| row.get(0),
        ).optional()?,
        files_indexed: count("SELECT COUNT(*) FROM files WHERE repository_id = ?1 AND deleted_at IS NULL")?,
        symbols_indexed: count("SELECT COUNT(*) FROM symbols s JOIN files f ON f.id = s.file_id WHERE f.repository_id = ?1 AND f.deleted_at IS NULL")?,
        relationships_indexed: count("SELECT COUNT(*) FROM symbol_relationships r JOIN files f ON f.id = r.source_file_id WHERE f.repository_id = ?1 AND f.deleted_at IS NULL")?,
        parse_errors: count("SELECT COUNT(*) FROM parse_errors e JOIN files f ON f.id = e.file_id WHERE f.repository_id = ?1 AND f.deleted_at IS NULL")?,
        supported_languages: config.parser.languages.clone(),
    })
}

pub fn find_symbols(conn: &Connection, repository_id: i64, name: &str) -> Result<Vec<SymbolRow>> {
    find_symbols_sql(conn, repository_id, name)
}

pub fn find_symbols_with_search(
    conn: &Connection,
    repository_id: i64,
    name: &str,
    enable_fts: bool,
) -> Result<Vec<SymbolRow>> {
    let mut rows = find_symbols_sql(conn, repository_id, name)?;
    if !enable_fts {
        return Ok(rows);
    }

    let mut seen = rows
        .iter()
        .map(|row| row.id)
        .collect::<std::collections::HashSet<_>>();
    for row in fts::search_symbols(conn, repository_id, name)? {
        if seen.insert(row.id) {
            rows.push(row);
        }
    }
    Ok(rows)
}

fn find_symbols_sql(conn: &Connection, repository_id: i64, name: &str) -> Result<Vec<SymbolRow>> {
    let prefix = format!("{name}%");
    let mut stmt = conn.prepare(
        "SELECT s.id, s.name, s.kind, s.language, f.path, s.start_line, s.end_line, s.signature, s.visibility,
         CASE WHEN s.name = ?2 THEN 'exact' ELSE 'prefix' END AS matched_by
         FROM symbols s JOIN files f ON f.id = s.file_id
         WHERE f.repository_id = ?1 AND f.deleted_at IS NULL AND (s.name = ?2 OR s.name LIKE ?3)
         ORDER BY matched_by, f.path, s.start_line",
    )?;
    collect_symbols(stmt.query_map(params![repository_id, name, prefix], symbol_row)?)
}

pub fn symbols_in_file(
    conn: &Connection,
    repository_id: i64,
    file: &str,
) -> Result<Vec<SymbolRow>> {
    let like = format!("%{file}");
    let mut stmt = conn.prepare(
        "SELECT s.id, s.name, s.kind, s.language, f.path, s.start_line, s.end_line, s.signature, s.visibility, 'file' AS matched_by
         FROM symbols s JOIN files f ON f.id = s.file_id
         WHERE f.repository_id = ?1 AND f.deleted_at IS NULL AND (f.path = ?2 OR f.path LIKE ?3)
         ORDER BY f.path, s.start_line",
    )?;
    collect_symbols(stmt.query_map(params![repository_id, file, like], symbol_row)?)
}

fn collect_symbols(
    rows: impl Iterator<Item = rusqlite::Result<SymbolRow>>,
) -> Result<Vec<SymbolRow>> {
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(Into::into)
}

fn symbol_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SymbolRow> {
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
}

pub fn repository_id(conn: &Connection, repo_root: &Path) -> Result<Option<i64>> {
    let repo_root = if repo_root.exists() {
        repo_root.canonicalize()?
    } else {
        repo_root.to_path_buf()
    };
    Ok(conn
        .query_row(
            "SELECT id FROM repositories WHERE root_path = ?1",
            params![repo_root.to_string_lossy()],
            |row| row.get(0),
        )
        .optional()?)
}

pub fn references(conn: &Connection, repository_id: i64, name: &str) -> Result<Vec<ReferenceRow>> {
    let mut stmt = conn.prepare(
        "SELECT r.referenced_name, r.reference_kind, f.path, r.start_line, r.start_column, s.name
         FROM symbol_references r
         JOIN files f ON f.id = r.file_id
         LEFT JOIN symbols s ON s.id = r.symbol_id
         WHERE f.repository_id = ?1 AND f.deleted_at IS NULL AND r.referenced_name = ?2
         ORDER BY f.path, r.start_line",
    )?;
    Ok(stmt
        .query_map(params![repository_id, name], |row| {
            Ok(ReferenceRow {
                referenced_name: row.get(0)?,
                reference_kind: row.get(1)?,
                file_path: row.get(2)?,
                start_line: row.get(3)?,
                start_column: row.get(4)?,
                symbol_name: row.get(5)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}

pub fn imports(conn: &Connection, repository_id: i64, file: &str) -> Result<Vec<ImportRow>> {
    let like = format!("%{file}");
    let mut stmt = conn.prepare(
        "SELECT f.path, i.import_text, i.imported_path, i.imported_symbol, i.start_line, i.start_column
         FROM imports i JOIN files f ON f.id = i.file_id
         WHERE f.repository_id = ?1 AND f.deleted_at IS NULL AND (f.path = ?2 OR f.path LIKE ?3)
         ORDER BY f.path, i.start_line",
    )?;
    Ok(stmt
        .query_map(params![repository_id, file, like], |row| {
            Ok(ImportRow {
                file_path: row.get(0)?,
                import_text: row.get(1)?,
                imported_path: row.get(2)?,
                imported_symbol: row.get(3)?,
                start_line: row.get(4)?,
                start_column: row.get(5)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}

pub fn parse_errors(
    conn: &Connection,
    repository_id: i64,
    file: Option<&str>,
) -> Result<Vec<ErrorRow>> {
    let like = file.map(|value| format!("%{value}"));
    let mut stmt = conn.prepare(
        "SELECT f.path, e.start_line, e.start_column, e.message
         FROM parse_errors e JOIN files f ON f.id = e.file_id
         WHERE f.repository_id = ?1 AND f.deleted_at IS NULL AND (?2 IS NULL OR f.path = ?2 OR f.path LIKE ?3)
         ORDER BY f.path, e.start_line",
    )?;
    Ok(stmt
        .query_map(params![repository_id, file, like], |row| {
            Ok(ErrorRow {
                file_path: row.get(0)?,
                start_line: row.get(1)?,
                start_column: row.get(2)?,
                message: row.get(3)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}

pub fn files_with_errors(conn: &Connection, repository_id: i64) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT f.path FROM parse_errors e JOIN files f ON f.id = e.file_id
         WHERE f.repository_id = ?1 AND f.deleted_at IS NULL ORDER BY f.path",
    )?;
    Ok(stmt
        .query_map(params![repository_id], |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}

pub fn relationships(
    conn: &Connection,
    repository_id: i64,
    name: &str,
    direction: RelationshipDirection,
) -> Result<Vec<RelationshipRow>> {
    let predicate = match direction {
        RelationshipDirection::Callers => "(target.name = ?2 OR r.evidence LIKE '%' || ?2 || '%')",
        RelationshipDirection::Callees => "source.name = ?2",
    };
    let sql = format!(
        "SELECT source.name, target.name, source_file.path, target_file.path, r.relationship_kind, r.confidence, r.evidence
         FROM symbol_relationships r
         LEFT JOIN symbols source ON source.id = r.source_symbol_id
         LEFT JOIN symbols target ON target.id = r.target_symbol_id
         JOIN files source_file ON source_file.id = r.source_file_id
         LEFT JOIN files target_file ON target_file.id = r.target_file_id
         WHERE source_file.repository_id = ?1 AND source_file.deleted_at IS NULL AND r.relationship_kind = 'calls' AND {predicate}
         ORDER BY source_file.path, source.start_line"
    );
    let mut stmt = conn.prepare(&sql)?;
    Ok(stmt
        .query_map(params![repository_id, name], |row| {
            Ok(RelationshipRow {
                source_name: row.get(0)?,
                target_name: row.get(1)?,
                source_file: row.get(2)?,
                target_file: row.get(3)?,
                relationship_kind: row.get(4)?,
                confidence: row.get(5)?,
                evidence: row.get(6)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}

#[derive(Debug, Clone, Copy)]
pub enum RelationshipDirection {
    Callers,
    Callees,
}

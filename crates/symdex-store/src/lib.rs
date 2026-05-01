//! Persistence boundary for SQLite and Qdrant adapters.

mod qdrant;

use std::env;
use std::fmt::{Display, Formatter};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

pub use qdrant::{
    PointPayload, QdrantClient, RetrievedPoint, ScoredPoint, VectorPoint, qdrant_collection_name,
    qdrant_point_id,
};

#[cfg(test)]
pub(crate) use qdrant::{
    CreateCollectionRequest, DeletePointsRequest, Distance, MatchValue, QueryPointsRequest,
    RepositoryFilter, RepositoryFilterCondition, ScrollPointsRequest, UpsertPointsRequest,
    VectorParams, validate_collection_name,
};

pub const MAX_CALL_PATH_DEPTH: usize = 8;

pub fn clamp_call_path_depth(depth: usize) -> usize {
    depth.clamp(1, MAX_CALL_PATH_DEPTH)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreConfig {
    pub sqlite_path: PathBuf,
    pub qdrant_url: String,
}

impl StoreConfig {
    pub fn from_env() -> Self {
        Self {
            sqlite_path: env_path("SYMDEX_DB_PATH", "symdex_DB_PATH")
                .unwrap_or_else(|| PathBuf::from(".symdex/symdex.sqlite")),
            qdrant_url: env_value("SYMDEX_QDRANT_URL", "symdex_QDRANT_URL")
                .unwrap_or_else(|| "http://localhost:6333".to_owned()),
        }
    }
}

pub fn sqlite_parent(config: &StoreConfig) -> Option<PathBuf> {
    config.sqlite_path.parent().map(PathBuf::from)
}

#[derive(Debug)]
pub struct SqliteStore {
    connection: Connection,
}

impl SqliteStore {
    pub fn open(config: &StoreConfig) -> Result<Self> {
        if let Some(parent) = sqlite_parent(config) {
            std::fs::create_dir_all(&parent).map_err(StoreError::Io)?;
        }
        let connection = Connection::open(&config.sqlite_path).map_err(StoreError::Sqlite)?;
        connection
            .execute_batch("PRAGMA foreign_keys = ON;")
            .map_err(StoreError::Sqlite)?;
        Ok(Self { connection })
    }

    pub fn migrate(&self) -> Result<()> {
        self.connection
            .execute_batch(SCHEMA)
            .map_err(StoreError::Sqlite)?;
        self.ensure_provenance_columns()?;
        Ok(())
    }

    fn ensure_provenance_columns(&self) -> Result<()> {
        for column in PROVENANCE_COLUMNS {
            self.ensure_column(column)?;
        }
        Ok(())
    }

    fn ensure_column(&self, column: &ProvenanceColumn) -> Result<()> {
        if self.column_exists(column.table, column.name)? {
            return Ok(());
        }
        self.connection
            .execute(column.alter_sql, [])
            .map_err(StoreError::Sqlite)?;
        Ok(())
    }

    fn column_exists(&self, table: &str, column: &str) -> Result<bool> {
        let mut statement = self
            .connection
            .prepare("SELECT name FROM pragma_table_info(?1)")
            .map_err(StoreError::Sqlite)?;
        let rows = statement
            .query_map(params![table], |row| row.get::<_, String>(0))
            .map_err(StoreError::Sqlite)?;
        for row in rows {
            if row.map_err(StoreError::Sqlite)? == column {
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub fn upsert_repository(&self, repository: &RepositoryRecord) -> Result<()> {
        let now = timestamp();
        self.connection
            .execute(
                "INSERT INTO repositories (id, root_path, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?3)
                 ON CONFLICT(id) DO UPDATE SET
                   root_path = excluded.root_path,
                   updated_at = excluded.updated_at",
                params![repository.id, repository.root_path, now],
            )
            .map_err(StoreError::Sqlite)?;
        Ok(())
    }

    pub fn file_unchanged(
        &self,
        repository_id: &str,
        path: &str,
        content_hash: &str,
        parser_version: &str,
    ) -> Result<bool> {
        let stored: Option<(String, Option<String>)> = self
            .connection
            .query_row(
                "SELECT content_hash, parser_version FROM files WHERE repository_id = ?1 AND path = ?2",
                params![repository_id, path],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(StoreError::Sqlite)?;
        Ok(stored
            .as_ref()
            .is_some_and(|(stored_hash, stored_parser_version)| {
                stored_hash == content_hash
                    && stored_parser_version.as_deref() == Some(parser_version)
            }))
    }

    pub fn replace_file_facts(
        &mut self,
        file: &FileRecord,
        symbols: &[SymbolRecord],
        chunks: &[ChunkRecord],
        calls: &[CallRecord],
    ) -> Result<()> {
        let transaction = self.connection.transaction().map_err(StoreError::Sqlite)?;
        transaction
            .execute(
                "INSERT INTO files (
                   id, repository_id, path, language, content_hash, indexed_at,
                   index_run_id, parser_version
                 )
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                  ON CONFLICT(repository_id, path) DO UPDATE SET
                    id = excluded.id,
                    language = excluded.language,
                    content_hash = excluded.content_hash,
                    indexed_at = excluded.indexed_at,
                    index_run_id = excluded.index_run_id,
                    parser_version = excluded.parser_version",
                params![
                    file.id,
                    file.repository_id,
                    file.path,
                    file.language,
                    file.content_hash,
                    timestamp(),
                    file.index_run_id,
                    file.parser_version
                ],
            )
            .map_err(StoreError::Sqlite)?;
        transaction
            .execute(
                "DELETE FROM calls
                 WHERE caller_symbol_id IN (SELECT id FROM symbols WHERE file_id = ?1)",
                params![file.id],
            )
            .map_err(StoreError::Sqlite)?;
        transaction
            .execute("DELETE FROM chunks WHERE file_id = ?1", params![file.id])
            .map_err(StoreError::Sqlite)?;
        transaction
            .execute("DELETE FROM symbols WHERE file_id = ?1", params![file.id])
            .map_err(StoreError::Sqlite)?;

        {
            let mut statement = transaction
                .prepare(
                    "INSERT INTO symbols (
                        id, file_id, parent_symbol_id, name, qualified_name, kind, signature,
                        start_line, end_line, start_byte, end_byte, index_run_id, parser_version
                      )
                      VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                )
                .map_err(StoreError::Sqlite)?;
            for symbol in symbols {
                statement
                    .execute(params![
                        symbol.id,
                        symbol.file_id,
                        symbol.parent_symbol_id,
                        symbol.name,
                        symbol.qualified_name,
                        symbol.kind,
                        symbol.signature,
                        symbol.start_line as i64,
                        symbol.end_line as i64,
                        symbol.start_byte as i64,
                        symbol.end_byte as i64,
                        symbol.index_run_id,
                        symbol.parser_version,
                    ])
                    .map_err(StoreError::Sqlite)?;
            }
        }

        {
            let mut statement = transaction
                .prepare(
                     "INSERT INTO chunks (
                        id, file_id, symbol_id, kind, text_hash,
                        start_line, end_line, start_byte, end_byte,
                        qdrant_point_id, excluded_reason, index_run_id, parser_version,
                        embedding_model, embedding_dimension, embedded_at
                      )
                      VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
                )
                .map_err(StoreError::Sqlite)?;
            for chunk in chunks {
                statement
                    .execute(params![
                        chunk.id,
                        chunk.file_id,
                        chunk.symbol_id,
                        chunk.kind,
                        chunk.text_hash,
                        chunk.start_line as i64,
                        chunk.end_line as i64,
                        chunk.start_byte as i64,
                        chunk.end_byte as i64,
                        chunk.qdrant_point_id,
                        chunk.excluded_reason,
                        chunk.index_run_id,
                        chunk.parser_version,
                        chunk.embedding_model,
                        chunk.embedding_dimension.map(|dimension| dimension as i64),
                        chunk.embedded_at,
                    ])
                    .map_err(StoreError::Sqlite)?;
            }
        }

        {
            let mut statement = transaction
                .prepare(
                    "INSERT INTO calls (
                        id, caller_symbol_id, callee_text, callee_symbol_id,
                        call_line, confidence, resolution_status, index_run_id, parser_version
                      )
                      VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                      ON CONFLICT(id) DO UPDATE SET
                        caller_symbol_id = excluded.caller_symbol_id,
                        callee_text = excluded.callee_text,
                        callee_symbol_id = excluded.callee_symbol_id,
                        call_line = excluded.call_line,
                        confidence = excluded.confidence,
                        resolution_status = excluded.resolution_status,
                        index_run_id = excluded.index_run_id,
                        parser_version = excluded.parser_version",
                )
                .map_err(StoreError::Sqlite)?;
            for call in calls {
                statement
                    .execute(params![
                        call.id,
                        call.caller_symbol_id,
                        call.callee_text,
                        call.callee_symbol_id,
                        call.call_line as i64,
                        call.confidence as f64,
                        call.resolution_status,
                        call.index_run_id,
                        call.parser_version,
                    ])
                    .map_err(StoreError::Sqlite)?;
            }
        }

        transaction.commit().map_err(StoreError::Sqlite)?;
        Ok(())
    }

    pub fn remove_missing_files(
        &mut self,
        repository_id: &str,
        active_paths: &[String],
    ) -> Result<usize> {
        let existing = self.file_paths(repository_id)?;
        let active: std::collections::BTreeSet<&str> =
            active_paths.iter().map(String::as_str).collect();
        let missing: Vec<String> = existing
            .into_iter()
            .filter(|path| !active.contains(path.as_str()))
            .collect();
        if missing.is_empty() {
            return Ok(0);
        }

        let transaction = self.connection.transaction().map_err(StoreError::Sqlite)?;
        for path in &missing {
            let file_id: Option<String> = transaction
                .query_row(
                    "SELECT id FROM files WHERE repository_id = ?1 AND path = ?2",
                    params![repository_id, path],
                    |row| row.get(0),
                )
                .optional()
                .map_err(StoreError::Sqlite)?;
            if let Some(file_id) = file_id {
                transaction
                    .execute(
                        "DELETE FROM calls
                         WHERE caller_symbol_id IN (SELECT id FROM symbols WHERE file_id = ?1)",
                        params![file_id],
                    )
                    .map_err(StoreError::Sqlite)?;
                transaction
                    .execute("DELETE FROM chunks WHERE file_id = ?1", params![file_id])
                    .map_err(StoreError::Sqlite)?;
            }
            transaction
                .execute(
                    "DELETE FROM files WHERE repository_id = ?1 AND path = ?2",
                    params![repository_id, path],
                )
                .map_err(StoreError::Sqlite)?;
        }
        transaction.commit().map_err(StoreError::Sqlite)?;
        Ok(missing.len())
    }

    pub fn qdrant_point_ids_for_paths(
        &self,
        repository_id: &str,
        paths: &[String],
    ) -> Result<Vec<String>> {
        if paths.is_empty() {
            return Ok(Vec::new());
        }
        let mut point_ids = std::collections::BTreeSet::new();
        let mut statement = self
            .connection
            .prepare(
                "SELECT chunks.qdrant_point_id
                 FROM chunks
                 JOIN files ON chunks.file_id = files.id
                 WHERE files.repository_id = ?1
                   AND files.path = ?2
                   AND chunks.qdrant_point_id IS NOT NULL
                 ORDER BY chunks.qdrant_point_id",
            )
            .map_err(StoreError::Sqlite)?;
        for path in paths {
            let rows = statement
                .query_map(params![repository_id, path], |row| row.get::<_, String>(0))
                .map_err(StoreError::Sqlite)?;
            for row in rows {
                point_ids.insert(row.map_err(StoreError::Sqlite)?);
            }
        }
        Ok(point_ids.into_iter().collect())
    }

    pub fn qdrant_point_ids_for_missing_files(
        &self,
        repository_id: &str,
        active_paths: &[String],
    ) -> Result<Vec<String>> {
        let existing = self.file_paths(repository_id)?;
        let active: std::collections::BTreeSet<&str> =
            active_paths.iter().map(String::as_str).collect();
        let missing: Vec<String> = existing
            .into_iter()
            .filter(|path| !active.contains(path.as_str()))
            .collect();
        self.qdrant_point_ids_for_paths(repository_id, &missing)
    }

    pub fn qdrant_expected_points(&self, repository_id: &str) -> Result<Vec<QdrantExpectedPoint>> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT chunks.qdrant_point_id, chunks.id, files.path, chunks.start_line,
                        chunks.end_line, chunks.text_hash, chunks.embedding_model,
                        chunks.embedding_dimension
                 FROM chunks
                 JOIN files ON chunks.file_id = files.id
                 WHERE files.repository_id = ?1
                   AND chunks.qdrant_point_id IS NOT NULL
                 ORDER BY files.path, chunks.start_line, chunks.id",
            )
            .map_err(StoreError::Sqlite)?;
        let rows = statement
            .query_map(params![repository_id], |row| {
                Ok(QdrantExpectedPoint {
                    qdrant_point_id: row.get(0)?,
                    chunk_id: row.get(1)?,
                    path: row.get(2)?,
                    start_line: row.get::<_, i64>(3)? as usize,
                    end_line: row.get::<_, i64>(4)? as usize,
                    text_hash: row.get(5)?,
                    embedding_model: row.get(6)?,
                    embedding_dimension: row
                        .get::<_, Option<i64>>(7)?
                        .map(|dimension| dimension as usize),
                })
            })
            .map_err(StoreError::Sqlite)?;
        collect_rows(rows)
    }

    pub fn repository_status(&self, repository_id: &str) -> Result<RepositoryStatus> {
        let files_indexed: usize = self
            .connection
            .query_row(
                "SELECT COUNT(*) FROM files WHERE repository_id = ?1",
                params![repository_id],
                |row| row.get::<_, i64>(0),
            )
            .map_err(StoreError::Sqlite)?
            .try_into()
            .map_err(|_| StoreError::UnexpectedResponse("negative file count".to_owned()))?;
        let chunks_indexed: usize = self
            .connection
            .query_row(
                "SELECT COUNT(*)
                 FROM chunks
                 JOIN files ON chunks.file_id = files.id
                 WHERE files.repository_id = ?1",
                params![repository_id],
                |row| row.get::<_, i64>(0),
            )
            .map_err(StoreError::Sqlite)?
            .try_into()
            .map_err(|_| StoreError::UnexpectedResponse("negative chunk count".to_owned()))?;
        let last_indexed_at: Option<String> = self
            .connection
            .query_row(
                "SELECT MAX(indexed_at) FROM files WHERE repository_id = ?1",
                params![repository_id],
                |row| row.get(0),
            )
            .map_err(StoreError::Sqlite)?;
        let embedding = self.latest_embedding_run(repository_id)?;
        Ok(RepositoryStatus {
            repository_id: repository_id.to_owned(),
            files_indexed,
            chunks_indexed,
            symbols_indexed: self.count_joined(repository_id, "symbols")?,
            calls_indexed: self.count_calls(repository_id)?,
            last_indexed_at,
            embedding_model: embedding.as_ref().map(|run| run.embedding_model.clone()),
            embedding_dimension: embedding.and_then(|run| run.embedding_dimension),
        })
    }

    pub fn ensure_embedding_compatible(
        &self,
        repository_id: &str,
        embedding_model: &str,
        embedding_dimension: usize,
    ) -> Result<()> {
        let Some(previous) = self.latest_embedding_run_for_model(repository_id, embedding_model)?
        else {
            return Ok(());
        };
        if let Some(previous_dimension) = previous.embedding_dimension
            && previous_dimension != embedding_dimension
        {
            return Err(StoreError::EmbeddingDimensionChanged {
                repository_id: repository_id.to_owned(),
                embedding_model: embedding_model.to_owned(),
                previous_dimension,
                current_dimension: embedding_dimension,
            });
        }
        Ok(())
    }

    pub fn start_index_run(&self, run: &IndexRunRecord) -> Result<()> {
        let now = timestamp();
        self.connection
            .execute(
                "INSERT INTO index_runs (
                   id, repository_id, started_at, finished_at, status, embedding_model,
                   embedding_dimension, files_seen, files_indexed, chunks_embedded,
                   error_summary, parser_version, indexer_version, run_kind
                  )
                  VALUES (?1, ?2, ?3, NULL, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
                  ON CONFLICT(id) DO UPDATE SET
                    started_at = excluded.started_at,
                    finished_at = NULL,
                    status = excluded.status,
                    embedding_model = excluded.embedding_model,
                    embedding_dimension = excluded.embedding_dimension,
                    files_seen = excluded.files_seen,
                    files_indexed = excluded.files_indexed,
                    chunks_embedded = excluded.chunks_embedded,
                    error_summary = excluded.error_summary,
                    parser_version = excluded.parser_version,
                    indexer_version = excluded.indexer_version,
                    run_kind = excluded.run_kind",
                params![
                    run.id,
                    run.repository_id,
                    now,
                    run.status,
                    run.embedding_model,
                    run.embedding_dimension.map(|dimension| dimension as i64),
                    run.files_seen as i64,
                    run.files_indexed as i64,
                    run.chunks_embedded as i64,
                    run.error_summary,
                    run.parser_version,
                    run.indexer_version,
                    run.run_kind,
                ],
            )
            .map_err(StoreError::Sqlite)?;
        Ok(())
    }

    pub fn finish_index_run(&self, run: &IndexRunRecord) -> Result<()> {
        let now = timestamp();
        self.connection
            .execute(
                "INSERT INTO index_runs (
                   id, repository_id, started_at, finished_at, status, embedding_model,
                   embedding_dimension, files_seen, files_indexed, chunks_embedded,
                   error_summary, parser_version, indexer_version, run_kind
                  )
                  VALUES (?1, ?2, ?3, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
                  ON CONFLICT(id) DO UPDATE SET
                    finished_at = excluded.finished_at,
                    status = excluded.status,
                    embedding_model = excluded.embedding_model,
                    embedding_dimension = excluded.embedding_dimension,
                    files_seen = excluded.files_seen,
                    files_indexed = excluded.files_indexed,
                    chunks_embedded = excluded.chunks_embedded,
                    error_summary = excluded.error_summary,
                    parser_version = excluded.parser_version,
                    indexer_version = excluded.indexer_version,
                    run_kind = excluded.run_kind",
                params![
                    run.id,
                    run.repository_id,
                    now,
                    run.status,
                    run.embedding_model,
                    run.embedding_dimension.map(|dimension| dimension as i64),
                    run.files_seen as i64,
                    run.files_indexed as i64,
                    run.chunks_embedded as i64,
                    run.error_summary,
                    run.parser_version,
                    run.indexer_version,
                    run.run_kind,
                ],
            )
            .map_err(StoreError::Sqlite)?;
        Ok(())
    }

    pub fn record_index_run(&self, run: &IndexRunRecord) -> Result<()> {
        self.finish_index_run(run)
    }

    pub fn record_chunk_embedding_provenance(
        &self,
        chunk_ids: &[String],
        embedding_model: &str,
        embedding_dimension: usize,
    ) -> Result<()> {
        let embedded_at = timestamp();
        let mut statement = self
            .connection
            .prepare(
                "UPDATE chunks
                 SET embedding_model = ?2,
                     embedding_dimension = ?3,
                     embedded_at = ?4
                 WHERE id = ?1",
            )
            .map_err(StoreError::Sqlite)?;
        for chunk_id in chunk_ids {
            statement
                .execute(params![
                    chunk_id,
                    embedding_model,
                    embedding_dimension as i64,
                    embedded_at
                ])
                .map_err(StoreError::Sqlite)?;
        }
        Ok(())
    }

    pub fn new_index_run_id(repository_id: &str, run_kind: &str) -> String {
        format!("{repository_id}-{run_kind}-{}", timestamp_nanos())
    }

    pub fn find_symbols(&self, repository_id: &str, query: &str) -> Result<Vec<SymbolSearchRow>> {
        let like = format!("%{query}%");
        let mut statement = self
            .connection
            .prepare(
                "SELECT symbols.id, symbols.name, symbols.qualified_name, symbols.kind,
                        files.path, symbols.start_line, symbols.end_line,
                        files.content_hash, symbols.index_run_id, symbols.parser_version,
                        files.indexed_at
                 FROM symbols
                 JOIN files ON symbols.file_id = files.id
                 WHERE files.repository_id = ?1
                   AND (symbols.name = ?2 OR symbols.qualified_name = ?2
                        OR symbols.name LIKE ?3 OR symbols.qualified_name LIKE ?3)
                 ORDER BY
                   CASE
                     WHEN symbols.qualified_name = ?2 THEN 0
                     WHEN symbols.name = ?2 THEN 1
                     ELSE 2
                   END,
                   files.path,
                   symbols.start_line
                 LIMIT 25",
            )
            .map_err(StoreError::Sqlite)?;
        let rows = statement
            .query_map(params![repository_id, query, like], |row| {
                Ok(SymbolSearchRow {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    qualified_name: row.get(2)?,
                    kind: row.get(3)?,
                    path: row.get(4)?,
                    start_line: row.get::<_, i64>(5)? as usize,
                    end_line: row.get::<_, i64>(6)? as usize,
                    provenance: EvidenceProvenance {
                        content_hash: row.get(7)?,
                        index_run_id: row.get(8)?,
                        parser_version: row.get(9)?,
                        indexed_at: row.get(10)?,
                        embedding_model: None,
                        embedding_dimension: None,
                        embedded_at: None,
                    },
                })
            })
            .map_err(StoreError::Sqlite)?;
        collect_rows(rows)
    }

    pub fn file_provenance(
        &self,
        repository_id: &str,
        path: &str,
    ) -> Result<Option<EvidenceProvenance>> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT content_hash, index_run_id, parser_version, indexed_at
                   FROM files
                  WHERE repository_id = ?1 AND path = ?2
                  LIMIT 1",
            )
            .map_err(StoreError::Sqlite)?;
        let mut rows = statement
            .query_map(params![repository_id, path], |row| {
                Ok(EvidenceProvenance {
                    content_hash: row.get(0)?,
                    index_run_id: row.get(1)?,
                    parser_version: row.get(2)?,
                    indexed_at: row.get(3)?,
                    embedding_model: None,
                    embedding_dimension: None,
                    embedded_at: None,
                })
            })
            .map_err(StoreError::Sqlite)?;
        rows.next().transpose().map_err(StoreError::Sqlite)
    }

    pub fn symbols_at_location(
        &self,
        repository_id: &str,
        path: &str,
        line: usize,
    ) -> Result<Vec<SymbolSearchRow>> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT symbols.id, symbols.name, symbols.qualified_name, symbols.kind,
                        files.path, symbols.start_line, symbols.end_line,
                        files.content_hash, symbols.index_run_id, symbols.parser_version,
                        files.indexed_at
                   FROM symbols
                   JOIN files ON symbols.file_id = files.id
                  WHERE files.repository_id = ?1
                    AND files.path = ?2
                    AND symbols.start_line <= ?3
                    AND symbols.end_line >= ?3
                  ORDER BY (symbols.end_line - symbols.start_line), symbols.start_line, symbols.qualified_name
                  LIMIT 10",
            )
            .map_err(StoreError::Sqlite)?;
        let rows = statement
            .query_map(params![repository_id, path, line as i64], |row| {
                Ok(SymbolSearchRow {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    qualified_name: row.get(2)?,
                    kind: row.get(3)?,
                    path: row.get(4)?,
                    start_line: row.get::<_, i64>(5)? as usize,
                    end_line: row.get::<_, i64>(6)? as usize,
                    provenance: EvidenceProvenance {
                        content_hash: row.get(7)?,
                        index_run_id: row.get(8)?,
                        parser_version: row.get(9)?,
                        indexed_at: row.get(10)?,
                        embedding_model: None,
                        embedding_dimension: None,
                        embedded_at: None,
                    },
                })
            })
            .map_err(StoreError::Sqlite)?;
        collect_rows(rows)
    }

    pub fn calls_at_location(
        &self,
        repository_id: &str,
        path: &str,
        line: usize,
    ) -> Result<Vec<CallPathEdge>> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT calls.id, calls.callee_text, calls.call_line, calls.confidence,
                        calls.resolution_status,
                        caller.id, caller.name, caller.qualified_name, caller.kind,
                        caller_files.path, caller.start_line, caller.end_line,
                        callee.id, callee.name, callee.qualified_name, callee.kind,
                        callee_files.path, callee.start_line, callee.end_line,
                        caller_files.content_hash, calls.index_run_id, calls.parser_version,
                        caller_files.indexed_at
                   FROM calls
                   JOIN symbols caller ON calls.caller_symbol_id = caller.id
                   JOIN files caller_files ON caller.file_id = caller_files.id
                   LEFT JOIN symbols callee ON calls.callee_symbol_id = callee.id
                   LEFT JOIN files callee_files ON callee.file_id = callee_files.id
                  WHERE caller_files.repository_id = ?1
                    AND caller_files.path = ?2
                    AND calls.call_line = ?3
                  ORDER BY caller.qualified_name, calls.callee_text, calls.id
                  LIMIT 25",
            )
            .map_err(StoreError::Sqlite)?;
        let rows = statement
            .query_map(params![repository_id, path, line as i64], call_path_edge)
            .map_err(StoreError::Sqlite)?;
        collect_rows(rows)
    }

    pub fn callers(&self, repository_id: &str, symbol_query: &str) -> Result<Vec<CallSearchRow>> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT calls.callee_text, calls.call_line, calls.confidence, calls.resolution_status,
                        caller.id, caller.name, caller.qualified_name, caller.kind,
                        files.path, caller.start_line, caller.end_line,
                        files.content_hash, calls.index_run_id, calls.parser_version,
                        files.indexed_at
                  FROM calls
                  JOIN symbols target ON calls.callee_symbol_id = target.id
                  JOIN symbols caller ON calls.caller_symbol_id = caller.id
                 JOIN files ON caller.file_id = files.id
                 WHERE files.repository_id = ?1
                   AND (target.id = ?2 OR target.name = ?2 OR target.qualified_name = ?2)
                 ORDER BY files.path, calls.call_line
                 LIMIT 50",
            )
            .map_err(StoreError::Sqlite)?;
        let rows = statement
            .query_map(params![repository_id, symbol_query], call_search_row)
            .map_err(StoreError::Sqlite)?;
        collect_rows(rows)
    }

    pub fn callees(&self, repository_id: &str, symbol_query: &str) -> Result<Vec<CallSearchRow>> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT calls.callee_text, calls.call_line, calls.confidence, calls.resolution_status,
                        callee.id, callee.name, callee.qualified_name, callee.kind,
                        files.path, callee.start_line, callee.end_line,
                        files.content_hash, calls.index_run_id, calls.parser_version,
                        files.indexed_at
                  FROM calls
                  JOIN symbols caller ON calls.caller_symbol_id = caller.id
                  LEFT JOIN symbols callee ON calls.callee_symbol_id = callee.id
                 JOIN files ON caller.file_id = files.id
                 WHERE files.repository_id = ?1
                   AND (caller.id = ?2 OR caller.name = ?2 OR caller.qualified_name = ?2)
                 ORDER BY calls.call_line, calls.callee_text
                 LIMIT 50",
            )
            .map_err(StoreError::Sqlite)?;
        let rows = statement
            .query_map(params![repository_id, symbol_query], call_search_row)
            .map_err(StoreError::Sqlite)?;
        collect_rows(rows)
    }

    pub fn call_paths(
        &self,
        repository_id: &str,
        source_query: &str,
        target_query: &str,
        max_depth: usize,
    ) -> Result<Vec<CallPath>> {
        let max_depth = clamp_call_path_depth(max_depth);
        let sources = self.resolve_symbol_refs(repository_id, source_query)?;
        let targets = self.resolve_symbol_refs(repository_id, target_query)?;
        let target_ids = targets
            .iter()
            .map(|symbol| symbol.id.clone())
            .collect::<std::collections::BTreeSet<_>>();
        let edges = self.call_path_edges(repository_id)?;
        let mut edges_by_caller = std::collections::BTreeMap::<String, Vec<CallPathEdge>>::new();
        for edge in edges {
            edges_by_caller
                .entry(edge.caller_symbol_id.clone())
                .or_default()
                .push(edge);
        }

        let mut paths = Vec::new();
        for source in sources {
            let mut visited = std::collections::BTreeSet::from([source.id.clone()]);
            let mut stack = Vec::new();
            trace_call_paths(
                &source.id,
                max_depth,
                &TraceContext {
                    target_query,
                    target_ids: &target_ids,
                    edges_by_caller: &edges_by_caller,
                },
                &mut visited,
                &mut stack,
                &mut paths,
            );
            if paths.len() >= 50 {
                break;
            }
        }
        paths.truncate(50);
        Ok(paths)
    }

    pub fn transitive_call_paths_to(
        &self,
        repository_id: &str,
        target_query: &str,
        max_depth: usize,
    ) -> Result<Vec<CallPath>> {
        let max_depth = clamp_call_path_depth(max_depth);
        let targets = self.resolve_symbol_refs(repository_id, target_query)?;
        let target_ids = targets
            .iter()
            .map(|symbol| symbol.id.clone())
            .collect::<std::collections::BTreeSet<_>>();
        let sources = self.all_symbol_refs(repository_id)?;
        let edges = self.call_path_edges(repository_id)?;
        let mut edges_by_caller = std::collections::BTreeMap::<String, Vec<CallPathEdge>>::new();
        for edge in edges {
            edges_by_caller
                .entry(edge.caller_symbol_id.clone())
                .or_default()
                .push(edge);
        }

        let mut paths = Vec::new();
        for source in sources {
            if target_ids.contains(&source.id) {
                continue;
            }
            let mut visited = std::collections::BTreeSet::from([source.id.clone()]);
            let mut stack = Vec::new();
            let mut source_paths = Vec::new();
            trace_call_paths(
                &source.id,
                max_depth,
                &TraceContext {
                    target_query,
                    target_ids: &target_ids,
                    edges_by_caller: &edges_by_caller,
                },
                &mut visited,
                &mut stack,
                &mut source_paths,
            );
            paths.extend(source_paths.into_iter().filter(|path| path.hops > 1));
            if paths.len() >= 50 {
                break;
            }
        }
        paths.truncate(50);
        Ok(paths)
    }

    pub fn transitive_call_paths_from(
        &self,
        repository_id: &str,
        source_query: &str,
        max_depth: usize,
    ) -> Result<Vec<CallPath>> {
        let max_depth = clamp_call_path_depth(max_depth);
        let sources = self.resolve_symbol_refs(repository_id, source_query)?;
        let edges = self.call_path_edges(repository_id)?;
        let mut edges_by_caller = std::collections::BTreeMap::<String, Vec<CallPathEdge>>::new();
        for edge in edges {
            edges_by_caller
                .entry(edge.caller_symbol_id.clone())
                .or_default()
                .push(edge);
        }

        let mut paths = Vec::new();
        for source in sources {
            let mut visited = std::collections::BTreeSet::from([source.id.clone()]);
            let mut stack = Vec::new();
            trace_reachable_call_paths(
                &source.id,
                max_depth,
                &edges_by_caller,
                &mut visited,
                &mut stack,
                &mut paths,
            );
            if paths.len() >= 50 {
                break;
            }
        }
        paths.truncate(50);
        Ok(paths)
    }

    fn resolve_symbol_refs(
        &self,
        repository_id: &str,
        symbol_query: &str,
    ) -> Result<Vec<SymbolRef>> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT symbols.id
                  FROM symbols
                  JOIN files ON symbols.file_id = files.id
                 WHERE files.repository_id = ?1
                   AND (symbols.id = ?2 OR symbols.name = ?2 OR symbols.qualified_name = ?2)
                 ORDER BY symbols.qualified_name, files.path, symbols.start_line
                 LIMIT 25",
            )
            .map_err(StoreError::Sqlite)?;
        let rows = statement
            .query_map(params![repository_id, symbol_query], |row| {
                Ok(SymbolRef { id: row.get(0)? })
            })
            .map_err(StoreError::Sqlite)?;
        collect_rows(rows)
    }

    fn all_symbol_refs(&self, repository_id: &str) -> Result<Vec<SymbolRef>> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT symbols.id
                  FROM symbols
                  JOIN files ON symbols.file_id = files.id
                 WHERE files.repository_id = ?1
                 ORDER BY symbols.qualified_name, files.path, symbols.start_line
                 LIMIT 500",
            )
            .map_err(StoreError::Sqlite)?;
        let rows = statement
            .query_map(params![repository_id], |row| {
                Ok(SymbolRef { id: row.get(0)? })
            })
            .map_err(StoreError::Sqlite)?;
        collect_rows(rows)
    }

    fn call_path_edges(&self, repository_id: &str) -> Result<Vec<CallPathEdge>> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT calls.id, calls.callee_text, calls.call_line, calls.confidence,
                        calls.resolution_status,
                        caller.id, caller.name, caller.qualified_name, caller.kind,
                        caller_file.path, caller.start_line, caller.end_line,
                        callee.id, callee.name, callee.qualified_name, callee.kind,
                        callee_file.path, callee.start_line, callee.end_line,
                        caller_file.content_hash, calls.index_run_id, calls.parser_version,
                        caller_file.indexed_at
                   FROM calls
                   JOIN symbols caller ON calls.caller_symbol_id = caller.id
                   JOIN files caller_file ON caller.file_id = caller_file.id
                   LEFT JOIN symbols callee ON calls.callee_symbol_id = callee.id
                   LEFT JOIN files callee_file ON callee.file_id = callee_file.id
                  WHERE caller_file.repository_id = ?1
                  ORDER BY caller.qualified_name, caller_file.path, calls.call_line,
                           calls.callee_text, calls.id",
            )
            .map_err(StoreError::Sqlite)?;
        let rows = statement
            .query_map(params![repository_id], call_path_edge)
            .map_err(StoreError::Sqlite)?;
        collect_rows(rows)
    }

    pub fn context_pack(
        &self,
        repository_id: &str,
        symbol_query: &str,
        limit: usize,
    ) -> Result<ContextPack> {
        let limit = limit.clamp(1, 25);
        let mut focus_symbols = self.find_symbols(repository_id, symbol_query)?;
        focus_symbols.truncate(limit);
        let mut direct_callers = self.callers(repository_id, symbol_query)?;
        direct_callers.truncate(limit);
        let mut direct_callees = self.callees(repository_id, symbol_query)?;
        direct_callees.truncate(limit);

        let mut files = std::collections::BTreeSet::new();
        for symbol in &focus_symbols {
            files.insert(symbol.path.clone());
        }
        for row in direct_callers.iter().chain(direct_callees.iter()) {
            if let Some(path) = &row.path {
                files.insert(path.clone());
            }
        }

        Ok(ContextPack {
            format: "symdex.context_pack.v1".to_owned(),
            repository_id: repository_id.to_owned(),
            query: symbol_query.to_owned(),
            focus_symbols,
            direct_callers,
            direct_callees,
            files: files.into_iter().collect(),
            limits: ContextPackLimits {
                max_symbols: limit,
                max_callers: limit,
                max_callees: limit,
            },
            notes: vec![
                "metadata_only_no_source_text".to_owned(),
                "direct_relationships_only".to_owned(),
            ],
        })
    }

    pub fn storage_explorer_summary(
        &self,
        repository_id: &str,
        embedding_model: &str,
    ) -> Result<StorageExplorerSummary> {
        let status = self.repository_status(repository_id)?;
        let sqlite = SqliteStorageSummary {
            repositories: self.count_repositories(repository_id)?,
            files: status.files_indexed,
            chunks: status.chunks_indexed,
            symbols: status.symbols_indexed,
            calls: status.calls_indexed,
            index_runs: self.count_index_runs(repository_id)?,
        };
        let chunk_projection = self.chunk_projection(repository_id)?;
        let latest_embedding = self.latest_embedding_run(repository_id)?;
        let projected_model = latest_embedding
            .as_ref()
            .map(|run| run.embedding_model.as_str())
            .unwrap_or(embedding_model);
        let qdrant = QdrantStorageProjection {
            collection_name: qdrant_collection_name(repository_id, projected_model),
            embedding_model: projected_model.to_owned(),
            embedding_dimension: latest_embedding
                .as_ref()
                .and_then(|run| run.embedding_dimension),
            embeddable_chunks: chunk_projection.embeddable_chunks,
            vector_backed_chunks: chunk_projection.vector_backed_chunks,
            excluded_chunks: chunk_projection.excluded_chunks,
            missing_vector_chunks: chunk_projection.missing_vector_chunks,
        };
        let warnings = storage_warnings(&sqlite, &qdrant);
        Ok(StorageExplorerSummary {
            repository_id: repository_id.to_owned(),
            sqlite,
            qdrant,
            warnings,
        })
    }

    pub fn index_coverage_summary(&self, repository_id: &str) -> Result<IndexCoverageSummary> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT
                   files.id,
                   files.path,
                   files.language,
                   (SELECT COUNT(*) FROM chunks WHERE chunks.file_id = files.id),
                   (SELECT COUNT(*) FROM symbols WHERE symbols.file_id = files.id),
                   (SELECT COUNT(*)
                    FROM calls
                    JOIN symbols caller ON calls.caller_symbol_id = caller.id
                    WHERE caller.file_id = files.id),
                   (SELECT COUNT(*)
                    FROM chunks
                    WHERE chunks.file_id = files.id
                      AND chunks.excluded_reason IS NULL),
                   (SELECT COUNT(*)
                    FROM chunks
                    WHERE chunks.file_id = files.id
                      AND chunks.qdrant_point_id IS NOT NULL),
                   (SELECT COUNT(*)
                    FROM chunks
                    WHERE chunks.file_id = files.id
                      AND chunks.excluded_reason IS NOT NULL)
                 FROM files
                 WHERE files.repository_id = ?1
                 ORDER BY files.path
                 LIMIT 200",
            )
            .map_err(StoreError::Sqlite)?;
        let rows = statement
            .query_map(params![repository_id], |row| {
                let chunks = row.get::<_, i64>(3)? as usize;
                let embeddable_chunks = row.get::<_, i64>(6)? as usize;
                let vector_backed_chunks = row.get::<_, i64>(7)? as usize;
                let excluded_chunks = row.get::<_, i64>(8)? as usize;
                Ok(FileCoverageBaseRow {
                    file_id: row.get(0)?,
                    path: row.get(1)?,
                    language: row.get(2)?,
                    chunks,
                    symbols: row.get::<_, i64>(4)? as usize,
                    calls: row.get::<_, i64>(5)? as usize,
                    embeddable_chunks,
                    vector_backed_chunks,
                    excluded_chunks,
                    status: file_coverage_status(
                        chunks,
                        embeddable_chunks,
                        vector_backed_chunks,
                        excluded_chunks,
                    ),
                })
            })
            .map_err(StoreError::Sqlite)?;
        let files = collect_rows(rows)?
            .into_iter()
            .map(|row| {
                let detail = self.file_detail_summary(&row.file_id)?;
                Ok(FileCoverageRow {
                    path: row.path,
                    language: row.language,
                    chunks: row.chunks,
                    symbols: row.symbols,
                    calls: row.calls,
                    embeddable_chunks: row.embeddable_chunks,
                    vector_backed_chunks: row.vector_backed_chunks,
                    excluded_chunks: row.excluded_chunks,
                    status: row.status,
                    detail,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(IndexCoverageSummary {
            repository_id: repository_id.to_owned(),
            files,
        })
    }

    pub fn symbol_outline_summary(&self, repository_id: &str) -> Result<SymbolOutlineSummary> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT symbols.id, symbols.parent_symbol_id, symbols.kind,
                        symbols.qualified_name, symbols.name, symbols.start_line,
                        symbols.end_line, files.path
                 FROM symbols
                 JOIN files ON symbols.file_id = files.id
                 WHERE files.repository_id = ?1
                 ORDER BY files.path, symbols.start_line, symbols.qualified_name
                 LIMIT 300",
            )
            .map_err(StoreError::Sqlite)?;
        let rows = statement
            .query_map(params![repository_id], |row| {
                Ok(SymbolOutlineBaseRow {
                    id: row.get(0)?,
                    parent_symbol_id: row.get(1)?,
                    kind: row.get(2)?,
                    qualified_name: row.get(3)?,
                    name: row.get(4)?,
                    start_line: row.get::<_, i64>(5)? as usize,
                    end_line: row.get::<_, i64>(6)? as usize,
                    path: row.get(7)?,
                })
            })
            .map_err(StoreError::Sqlite)?;
        let bases = collect_rows(rows)?;
        Ok(SymbolOutlineSummary {
            repository_id: repository_id.to_owned(),
            symbols: symbol_outline_rows(bases),
        })
    }

    pub fn call_resolution_summary(&self, repository_id: &str) -> Result<CallResolutionSummary> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT files.path, caller.qualified_name, calls.callee_text, calls.call_line,
                        calls.confidence, calls.resolution_status
                 FROM calls
                 JOIN symbols caller ON calls.caller_symbol_id = caller.id
                 JOIN files ON caller.file_id = files.id
                 WHERE files.repository_id = ?1
                 ORDER BY calls.resolution_status, calls.confidence ASC, files.path,
                          calls.call_line, calls.callee_text
                 LIMIT 500",
            )
            .map_err(StoreError::Sqlite)?;
        let rows = statement
            .query_map(params![repository_id], |row| {
                let confidence: f64 = row.get(4)?;
                Ok(CallResolutionEdgeRow {
                    path: row.get(0)?,
                    caller_symbol: row.get(1)?,
                    callee_text: row.get(2)?,
                    call_line: row.get::<_, i64>(3)? as usize,
                    confidence,
                    resolution_status: row.get(5)?,
                    confidence_bucket: confidence_bucket(confidence),
                })
            })
            .map_err(StoreError::Sqlite)?;
        Ok(CallResolutionSummary {
            repository_id: repository_id.to_owned(),
            buckets: call_resolution_buckets(collect_rows(rows)?),
        })
    }

    pub fn embedding_coverage_summary(
        &self,
        repository_id: &str,
        configured_embedding_model: &str,
    ) -> Result<EmbeddingCoverageSummary> {
        let status = self.repository_status(repository_id)?;
        let chunk_projection = self.chunk_projection(repository_id)?;
        let latest_embedding = self.latest_embedding_run(repository_id)?;
        let projected_model = latest_embedding
            .as_ref()
            .map(|run| run.embedding_model.as_str())
            .unwrap_or(configured_embedding_model);
        let embedding_dimension = latest_embedding
            .as_ref()
            .and_then(|run| run.embedding_dimension);
        let latest_chunks_embedded = latest_embedding.as_ref().map(|run| run.chunks_embedded);
        let exclusion_reasons = self.embedding_exclusion_reasons(repository_id)?;

        let mut health = embedding_coverage_health(
            &status,
            &chunk_projection,
            latest_embedding.as_ref(),
            configured_embedding_model,
        );
        if health.is_empty() {
            health.push(StorageHealthRow {
                status: StorageHealthStatus::Ok,
                label: "embedding_coverage_ok".to_owned(),
                detail: "All embeddable SQLite chunks have recorded vector point metadata."
                    .to_owned(),
            });
        }

        Ok(EmbeddingCoverageSummary {
            repository_id: repository_id.to_owned(),
            collection_name: qdrant_collection_name(repository_id, projected_model),
            configured_embedding_model: configured_embedding_model.to_owned(),
            embedding_model: projected_model.to_owned(),
            embedding_dimension,
            total_chunks: status.chunks_indexed,
            embeddable_chunks: chunk_projection.embeddable_chunks,
            vector_backed_chunks: chunk_projection.vector_backed_chunks,
            missing_vector_chunks: chunk_projection.missing_vector_chunks,
            excluded_chunks: chunk_projection.excluded_chunks,
            latest_chunks_embedded,
            exclusion_reasons,
            health,
        })
    }

    pub fn index_runs_timeline_summary(
        &self,
        repository_id: &str,
    ) -> Result<IndexRunsTimelineSummary> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT id, started_at, finished_at, status, embedding_model,
                        embedding_dimension, files_seen, files_indexed,
                        chunks_embedded, error_summary
                 FROM index_runs
                 WHERE repository_id = ?1
                 ORDER BY started_at DESC, finished_at DESC, id DESC
                 LIMIT 50",
            )
            .map_err(StoreError::Sqlite)?;
        let rows = statement
            .query_map(params![repository_id], |row| {
                Ok(IndexRunTimelineRow {
                    id: row.get(0)?,
                    started_at: row.get(1)?,
                    finished_at: row.get(2)?,
                    status: row.get(3)?,
                    embedding_model: row.get(4)?,
                    embedding_dimension: row
                        .get::<_, Option<i64>>(5)?
                        .map(|dimension| dimension as usize),
                    files_seen: row.get::<_, i64>(6)? as usize,
                    files_indexed: row.get::<_, i64>(7)? as usize,
                    chunks_embedded: row.get::<_, i64>(8)? as usize,
                    error_summary: row.get(9)?,
                })
            })
            .map_err(StoreError::Sqlite)?;
        Ok(IndexRunsTimelineSummary {
            repository_id: repository_id.to_owned(),
            runs: collect_rows(rows)?,
        })
    }

    pub fn semantic_neighborhood_summary(
        &self,
        repository_id: &str,
        configured_embedding_model: &str,
    ) -> Result<SemanticNeighborhoodSummary> {
        let latest_embedding = self.latest_embedding_run(repository_id)?;
        let projected_model = latest_embedding
            .as_ref()
            .map(|run| run.embedding_model.as_str())
            .unwrap_or(configured_embedding_model);
        let mut statement = self
            .connection
            .prepare(
                "SELECT chunks.qdrant_point_id, files.path, chunks.start_line, chunks.end_line,
                        symbols.qualified_name, chunks.kind, files.language, chunks.text_hash
                 FROM chunks
                 JOIN files ON chunks.file_id = files.id
                 LEFT JOIN symbols ON chunks.symbol_id = symbols.id
                 WHERE files.repository_id = ?1
                   AND chunks.qdrant_point_id IS NOT NULL
                 ORDER BY files.path, chunks.start_line, chunks.end_line, chunks.id
                 LIMIT 100",
            )
            .map_err(StoreError::Sqlite)?;
        let rows = statement
            .query_map(params![repository_id], |row| {
                Ok(SemanticNeighborhoodRow {
                    qdrant_point_id: row.get(0)?,
                    path: row.get(1)?,
                    start_line: row.get::<_, i64>(2)? as usize,
                    end_line: row.get::<_, i64>(3)? as usize,
                    symbol_name: row.get(4)?,
                    chunk_kind: row.get(5)?,
                    language: row.get(6)?,
                    score: None,
                    text_hash: row.get(7)?,
                })
            })
            .map_err(StoreError::Sqlite)?;
        let rows = collect_rows(rows)?;
        let health = semantic_neighborhood_health(&rows);
        Ok(SemanticNeighborhoodSummary {
            repository_id: repository_id.to_owned(),
            collection_name: qdrant_collection_name(repository_id, projected_model),
            embedding_model: projected_model.to_owned(),
            rows,
            health,
        })
    }

    pub fn cross_store_health_summary(
        &self,
        repository_id: &str,
        configured_embedding_model: &str,
    ) -> Result<CrossStoreHealthSummary> {
        let projection = self.chunk_projection(repository_id)?;
        let latest_embedding = self.latest_embedding_run(repository_id)?;
        let projected_model = latest_embedding
            .as_ref()
            .map(|run| run.embedding_model.as_str())
            .unwrap_or(configured_embedding_model);
        let mut rows = Vec::new();

        if projection.embeddable_chunks > 0 && latest_embedding.is_none() {
            rows.push(StorageHealthRow {
                status: StorageHealthStatus::Error,
                label: "missing_collection".to_owned(),
                detail: "Embeddable chunks exist, but no successful semantic index run has recorded collection metadata.".to_owned(),
            });
        } else if projection.embeddable_chunks > 0 && projection.vector_backed_chunks == 0 {
            rows.push(StorageHealthRow {
                status: StorageHealthStatus::Error,
                label: "missing_collection".to_owned(),
                detail: "Embeddable chunks exist, but no chunks have recorded Qdrant point IDs."
                    .to_owned(),
            });
        }
        if projection.missing_vector_chunks > 0 {
            rows.push(StorageHealthRow {
                status: StorageHealthStatus::Warning,
                label: "missing_vectors".to_owned(),
                detail: format!(
                    "{} embeddable chunks are missing recorded Qdrant point IDs.",
                    projection.missing_vector_chunks
                ),
            });
        }
        if projection.excluded_chunks > 0 {
            rows.push(StorageHealthRow {
                status: StorageHealthStatus::Warning,
                label: "excluded_chunks".to_owned(),
                detail: format!(
                    "{} chunks are intentionally excluded from semantic embedding.",
                    projection.excluded_chunks
                ),
            });
        }
        if let Some(latest) = &latest_embedding
            && latest.embedding_model != configured_embedding_model
        {
            rows.push(StorageHealthRow {
                status: StorageHealthStatus::Error,
                label: "model_drift".to_owned(),
                detail: format!(
                    "Configured model {configured_embedding_model} differs from latest indexed model {}.",
                    latest.embedding_model
                ),
            });
        }
        let dimensions = self.successful_embedding_dimensions(repository_id, projected_model)?;
        if dimensions.len() > 1 {
            rows.push(StorageHealthRow {
                status: StorageHealthStatus::Error,
                label: "dimension_drift".to_owned(),
                detail: format!(
                    "Successful runs for model {projected_model} recorded multiple dimensions: {}.",
                    dimensions
                        .iter()
                        .map(usize::to_string)
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            });
        }
        if rows.is_empty() {
            rows.push(StorageHealthRow {
                status: StorageHealthStatus::Ok,
                label: "cross_store_ok".to_owned(),
                detail:
                    "SQLite chunk metadata and recorded Qdrant projection metadata are aligned."
                        .to_owned(),
            });
        }

        Ok(CrossStoreHealthSummary {
            repository_id: repository_id.to_owned(),
            collection_name: qdrant_collection_name(repository_id, projected_model),
            rows,
        })
    }

    pub fn indexed_file_freshness_snapshots(
        &self,
        repository_id: &str,
    ) -> Result<Vec<FileFreshnessSnapshot>> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT path, content_hash, indexed_at, index_run_id, parser_version
                 FROM files
                 WHERE repository_id = ?1
                 ORDER BY path",
            )
            .map_err(StoreError::Sqlite)?;
        let rows = statement
            .query_map(params![repository_id], |row| {
                Ok(FileFreshnessSnapshot {
                    path: row.get(0)?,
                    content_hash: row.get(1)?,
                    indexed_at: row.get(2)?,
                    index_run_id: row.get(3)?,
                    parser_version: row.get(4)?,
                })
            })
            .map_err(StoreError::Sqlite)?;
        collect_rows(rows)
    }

    fn file_paths(&self, repository_id: &str) -> Result<Vec<String>> {
        let mut statement = self
            .connection
            .prepare("SELECT path FROM files WHERE repository_id = ?1")
            .map_err(StoreError::Sqlite)?;
        let rows = statement
            .query_map(params![repository_id], |row| row.get(0))
            .map_err(StoreError::Sqlite)?;
        let mut paths = Vec::new();
        for row in rows {
            paths.push(row.map_err(StoreError::Sqlite)?);
        }
        Ok(paths)
    }

    fn count_joined(&self, repository_id: &str, table: &str) -> Result<usize> {
        let sql = format!(
            "SELECT COUNT(*)
             FROM {table}
             JOIN files ON {table}.file_id = files.id
             WHERE files.repository_id = ?1"
        );
        let count: i64 = self
            .connection
            .query_row(&sql, params![repository_id], |row| row.get(0))
            .map_err(StoreError::Sqlite)?;
        count
            .try_into()
            .map_err(|_| StoreError::UnexpectedResponse(format!("negative {table} count")))
    }

    fn count_calls(&self, repository_id: &str) -> Result<usize> {
        let count: i64 = self
            .connection
            .query_row(
                "SELECT COUNT(*)
                 FROM calls
                 JOIN symbols ON calls.caller_symbol_id = symbols.id
                 JOIN files ON symbols.file_id = files.id
                 WHERE files.repository_id = ?1",
                params![repository_id],
                |row| row.get(0),
            )
            .map_err(StoreError::Sqlite)?;
        count
            .try_into()
            .map_err(|_| StoreError::UnexpectedResponse("negative call count".to_owned()))
    }

    fn count_repositories(&self, repository_id: &str) -> Result<usize> {
        self.count_scalar(
            "SELECT COUNT(*) FROM repositories WHERE id = ?1",
            repository_id,
            "repository",
        )
    }

    fn count_index_runs(&self, repository_id: &str) -> Result<usize> {
        self.count_scalar(
            "SELECT COUNT(*) FROM index_runs WHERE repository_id = ?1",
            repository_id,
            "index run",
        )
    }

    fn count_scalar(&self, sql: &str, repository_id: &str, label: &str) -> Result<usize> {
        let count: i64 = self
            .connection
            .query_row(sql, params![repository_id], |row| row.get(0))
            .map_err(StoreError::Sqlite)?;
        count
            .try_into()
            .map_err(|_| StoreError::UnexpectedResponse(format!("negative {label} count")))
    }

    fn chunk_projection(&self, repository_id: &str) -> Result<ChunkProjectionCounts> {
        self.connection
            .query_row(
                "SELECT
                   COALESCE(SUM(CASE WHEN chunks.excluded_reason IS NULL THEN 1 ELSE 0 END), 0),
                   COALESCE(SUM(CASE WHEN chunks.qdrant_point_id IS NOT NULL THEN 1 ELSE 0 END), 0),
                   COALESCE(SUM(CASE WHEN chunks.excluded_reason IS NOT NULL THEN 1 ELSE 0 END), 0),
                   COALESCE(SUM(CASE
                     WHEN chunks.excluded_reason IS NULL AND chunks.qdrant_point_id IS NULL
                     THEN 1 ELSE 0 END), 0)
                 FROM chunks
                 JOIN files ON chunks.file_id = files.id
                 WHERE files.repository_id = ?1",
                params![repository_id],
                |row| {
                    Ok(ChunkProjectionCounts {
                        embeddable_chunks: row.get::<_, i64>(0)? as usize,
                        vector_backed_chunks: row.get::<_, i64>(1)? as usize,
                        excluded_chunks: row.get::<_, i64>(2)? as usize,
                        missing_vector_chunks: row.get::<_, i64>(3)? as usize,
                    })
                },
            )
            .map_err(StoreError::Sqlite)
    }

    fn file_detail_summary(&self, file_id: &str) -> Result<FileDetailSummary> {
        Ok(FileDetailSummary {
            chunks: self.file_chunk_details(file_id)?,
            symbols: self.file_symbol_details(file_id)?,
            calls: self.file_call_details(file_id)?,
        })
    }

    fn file_chunk_details(&self, file_id: &str) -> Result<Vec<FileChunkDetailRow>> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT chunks.kind, symbols.qualified_name, chunks.start_line,
                        chunks.end_line, chunks.qdrant_point_id, chunks.excluded_reason
                 FROM chunks
                 LEFT JOIN symbols ON chunks.symbol_id = symbols.id
                 WHERE chunks.file_id = ?1
                 ORDER BY chunks.start_line, chunks.kind
                 LIMIT 6",
            )
            .map_err(StoreError::Sqlite)?;
        let rows = statement
            .query_map(params![file_id], |row| {
                let qdrant_point_id: Option<String> = row.get(4)?;
                let excluded_reason: Option<String> = row.get(5)?;
                Ok(FileChunkDetailRow {
                    kind: row.get(0)?,
                    symbol: row.get(1)?,
                    start_line: row.get::<_, i64>(2)? as usize,
                    end_line: row.get::<_, i64>(3)? as usize,
                    vector_status: chunk_vector_status(&qdrant_point_id, &excluded_reason),
                    excluded_reason,
                })
            })
            .map_err(StoreError::Sqlite)?;
        collect_rows(rows)
    }

    fn file_symbol_details(&self, file_id: &str) -> Result<Vec<FileSymbolDetailRow>> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT kind, qualified_name, parent_symbol_id, start_line, end_line
                 FROM symbols
                 WHERE file_id = ?1
                 ORDER BY start_line, qualified_name
                 LIMIT 6",
            )
            .map_err(StoreError::Sqlite)?;
        let rows = statement
            .query_map(params![file_id], |row| {
                Ok(FileSymbolDetailRow {
                    kind: row.get(0)?,
                    qualified_name: row.get(1)?,
                    parent_symbol_id: row.get(2)?,
                    start_line: row.get::<_, i64>(3)? as usize,
                    end_line: row.get::<_, i64>(4)? as usize,
                })
            })
            .map_err(StoreError::Sqlite)?;
        collect_rows(rows)
    }

    fn file_call_details(&self, file_id: &str) -> Result<Vec<FileCallDetailRow>> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT caller.qualified_name, calls.callee_text, calls.call_line,
                        calls.confidence, calls.resolution_status
                 FROM calls
                 JOIN symbols caller ON calls.caller_symbol_id = caller.id
                 WHERE caller.file_id = ?1
                 ORDER BY calls.call_line, calls.callee_text
                 LIMIT 6",
            )
            .map_err(StoreError::Sqlite)?;
        let rows = statement
            .query_map(params![file_id], |row| {
                Ok(FileCallDetailRow {
                    caller_symbol: row.get(0)?,
                    callee_text: row.get(1)?,
                    call_line: row.get::<_, i64>(2)? as usize,
                    confidence: row.get(3)?,
                    resolution_status: row.get(4)?,
                })
            })
            .map_err(StoreError::Sqlite)?;
        collect_rows(rows)
    }

    fn embedding_exclusion_reasons(
        &self,
        repository_id: &str,
    ) -> Result<Vec<EmbeddingExclusionRow>> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT chunks.excluded_reason, COUNT(*)
                 FROM chunks
                 JOIN files ON chunks.file_id = files.id
                 WHERE files.repository_id = ?1
                   AND chunks.excluded_reason IS NOT NULL
                 GROUP BY chunks.excluded_reason
                 ORDER BY COUNT(*) DESC, chunks.excluded_reason
                 LIMIT 12",
            )
            .map_err(StoreError::Sqlite)?;
        let rows = statement
            .query_map(params![repository_id], |row| {
                Ok(EmbeddingExclusionRow {
                    reason: row.get(0)?,
                    chunks: row.get::<_, i64>(1)? as usize,
                })
            })
            .map_err(StoreError::Sqlite)?;
        collect_rows(rows)
    }

    fn successful_embedding_dimensions(
        &self,
        repository_id: &str,
        embedding_model: &str,
    ) -> Result<Vec<usize>> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT DISTINCT embedding_dimension
                 FROM index_runs
                 WHERE repository_id = ?1
                   AND embedding_model = ?2
                   AND status = 'success'
                   AND embedding_dimension IS NOT NULL
                 ORDER BY embedding_dimension",
            )
            .map_err(StoreError::Sqlite)?;
        let rows = statement
            .query_map(params![repository_id, embedding_model], |row| {
                Ok(row.get::<_, i64>(0)? as usize)
            })
            .map_err(StoreError::Sqlite)?;
        collect_rows(rows)
    }

    fn latest_embedding_run(&self, repository_id: &str) -> Result<Option<EmbeddingIndexMetadata>> {
        self.connection
            .query_row(
                "SELECT embedding_model, embedding_dimension, chunks_embedded
                 FROM index_runs
                 WHERE repository_id = ?1
                   AND status = 'success'
                   AND embedding_dimension IS NOT NULL
                 ORDER BY finished_at DESC, started_at DESC
                 LIMIT 1",
                params![repository_id],
                embedding_index_metadata,
            )
            .optional()
            .map_err(StoreError::Sqlite)
    }

    fn latest_embedding_run_for_model(
        &self,
        repository_id: &str,
        embedding_model: &str,
    ) -> Result<Option<EmbeddingIndexMetadata>> {
        self.connection
            .query_row(
                "SELECT embedding_model, embedding_dimension, chunks_embedded
                 FROM index_runs
                 WHERE repository_id = ?1
                   AND embedding_model = ?2
                   AND status = 'success'
                   AND embedding_dimension IS NOT NULL
                 ORDER BY finished_at DESC, started_at DESC
                 LIMIT 1",
                params![repository_id, embedding_model],
                embedding_index_metadata,
            )
            .optional()
            .map_err(StoreError::Sqlite)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepositoryRecord {
    pub id: String,
    pub root_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileRecord {
    pub id: String,
    pub repository_id: String,
    pub path: String,
    pub language: String,
    pub content_hash: String,
    pub index_run_id: String,
    pub parser_version: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChunkRecord {
    pub id: String,
    pub file_id: String,
    pub symbol_id: Option<String>,
    pub kind: String,
    pub text_hash: String,
    pub start_line: usize,
    pub end_line: usize,
    pub start_byte: usize,
    pub end_byte: usize,
    pub qdrant_point_id: Option<String>,
    pub excluded_reason: Option<String>,
    pub index_run_id: String,
    pub parser_version: String,
    pub embedding_model: Option<String>,
    pub embedding_dimension: Option<usize>,
    pub embedded_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SymbolRecord {
    pub id: String,
    pub file_id: String,
    pub parent_symbol_id: Option<String>,
    pub name: String,
    pub qualified_name: String,
    pub kind: String,
    pub signature: Option<String>,
    pub start_line: usize,
    pub end_line: usize,
    pub start_byte: usize,
    pub end_byte: usize,
    pub index_run_id: String,
    pub parser_version: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CallRecord {
    pub id: String,
    pub caller_symbol_id: String,
    pub callee_text: String,
    pub callee_symbol_id: Option<String>,
    pub call_line: usize,
    pub confidence: f32,
    pub resolution_status: String,
    pub index_run_id: String,
    pub parser_version: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepositoryStatus {
    pub repository_id: String,
    pub files_indexed: usize,
    pub chunks_indexed: usize,
    pub symbols_indexed: usize,
    pub calls_indexed: usize,
    pub last_indexed_at: Option<String>,
    pub embedding_model: Option<String>,
    pub embedding_dimension: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexRunRecord {
    pub id: String,
    pub repository_id: String,
    pub status: String,
    pub embedding_model: String,
    pub embedding_dimension: Option<usize>,
    pub files_seen: usize,
    pub files_indexed: usize,
    pub chunks_embedded: usize,
    pub error_summary: Option<String>,
    pub parser_version: String,
    pub indexer_version: String,
    pub run_kind: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbeddingIndexMetadata {
    pub embedding_model: String,
    pub embedding_dimension: Option<usize>,
    pub chunks_embedded: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageExplorerSummary {
    pub repository_id: String,
    pub sqlite: SqliteStorageSummary,
    pub qdrant: QdrantStorageProjection,
    pub warnings: Vec<StorageHealthRow>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SqliteStorageSummary {
    pub repositories: usize,
    pub files: usize,
    pub chunks: usize,
    pub symbols: usize,
    pub calls: usize,
    pub index_runs: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QdrantStorageProjection {
    pub collection_name: String,
    pub embedding_model: String,
    pub embedding_dimension: Option<usize>,
    pub embeddable_chunks: usize,
    pub vector_backed_chunks: usize,
    pub excluded_chunks: usize,
    pub missing_vector_chunks: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QdrantExpectedPoint {
    pub qdrant_point_id: String,
    pub chunk_id: String,
    pub path: String,
    pub start_line: usize,
    pub end_line: usize,
    pub text_hash: String,
    pub embedding_model: Option<String>,
    pub embedding_dimension: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageHealthRow {
    pub status: StorageHealthStatus,
    pub label: String,
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorageHealthStatus {
    Ok,
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq)]
pub struct IndexCoverageSummary {
    pub repository_id: String,
    pub files: Vec<FileCoverageRow>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FileCoverageRow {
    pub path: String,
    pub language: String,
    pub chunks: usize,
    pub symbols: usize,
    pub calls: usize,
    pub embeddable_chunks: usize,
    pub vector_backed_chunks: usize,
    pub excluded_chunks: usize,
    pub status: FileCoverageStatus,
    pub detail: FileDetailSummary,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileCoverageStatus {
    Covered,
    MetadataOnly,
    Excluded,
    MissingVector,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FileDetailSummary {
    pub chunks: Vec<FileChunkDetailRow>,
    pub symbols: Vec<FileSymbolDetailRow>,
    pub calls: Vec<FileCallDetailRow>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileChunkDetailRow {
    pub kind: String,
    pub symbol: Option<String>,
    pub start_line: usize,
    pub end_line: usize,
    pub vector_status: ChunkVectorStatus,
    pub excluded_reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChunkVectorStatus {
    VectorBacked,
    MissingVector,
    Excluded,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileSymbolDetailRow {
    pub kind: String,
    pub qualified_name: String,
    pub parent_symbol_id: Option<String>,
    pub start_line: usize,
    pub end_line: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FileCallDetailRow {
    pub caller_symbol: String,
    pub callee_text: String,
    pub call_line: usize,
    pub confidence: f64,
    pub resolution_status: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolOutlineSummary {
    pub repository_id: String,
    pub symbols: Vec<SymbolOutlineRow>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolOutlineRow {
    pub id: String,
    pub parent_symbol_id: Option<String>,
    pub depth: usize,
    pub child_count: usize,
    pub kind: String,
    pub qualified_name: String,
    pub name: String,
    pub path: String,
    pub start_line: usize,
    pub end_line: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CallResolutionSummary {
    pub repository_id: String,
    pub buckets: Vec<CallResolutionBucket>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CallResolutionBucket {
    pub resolution_status: String,
    pub confidence_bucket: ConfidenceBucket,
    pub call_count: usize,
    pub average_confidence: f64,
    pub rows: Vec<CallResolutionEdgeRow>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CallResolutionEdgeRow {
    pub path: String,
    pub caller_symbol: String,
    pub callee_text: String,
    pub call_line: usize,
    pub confidence: f64,
    pub resolution_status: String,
    pub confidence_bucket: ConfidenceBucket,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbeddingCoverageSummary {
    pub repository_id: String,
    pub collection_name: String,
    pub configured_embedding_model: String,
    pub embedding_model: String,
    pub embedding_dimension: Option<usize>,
    pub total_chunks: usize,
    pub embeddable_chunks: usize,
    pub vector_backed_chunks: usize,
    pub missing_vector_chunks: usize,
    pub excluded_chunks: usize,
    pub latest_chunks_embedded: Option<usize>,
    pub exclusion_reasons: Vec<EmbeddingExclusionRow>,
    pub health: Vec<StorageHealthRow>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbeddingExclusionRow {
    pub reason: String,
    pub chunks: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexRunsTimelineSummary {
    pub repository_id: String,
    pub runs: Vec<IndexRunTimelineRow>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexRunTimelineRow {
    pub id: String,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub status: String,
    pub embedding_model: String,
    pub embedding_dimension: Option<usize>,
    pub files_seen: usize,
    pub files_indexed: usize,
    pub chunks_embedded: usize,
    pub error_summary: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SemanticNeighborhoodSummary {
    pub repository_id: String,
    pub collection_name: String,
    pub embedding_model: String,
    pub rows: Vec<SemanticNeighborhoodRow>,
    pub health: Vec<StorageHealthRow>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SemanticNeighborhoodRow {
    pub qdrant_point_id: String,
    pub path: String,
    pub start_line: usize,
    pub end_line: usize,
    pub symbol_name: Option<String>,
    pub chunk_kind: String,
    pub language: String,
    pub score: Option<f64>,
    pub text_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrossStoreHealthSummary {
    pub repository_id: String,
    pub collection_name: String,
    pub rows: Vec<StorageHealthRow>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ConfidenceBucket {
    High,
    Medium,
    Low,
}

impl ConfidenceBucket {
    pub fn label(self) -> &'static str {
        match self {
            Self::High => "high",
            Self::Medium => "medium",
            Self::Low => "low",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SymbolSearchRow {
    pub id: String,
    pub name: String,
    pub qualified_name: String,
    pub kind: String,
    pub path: String,
    pub start_line: usize,
    pub end_line: usize,
    pub provenance: EvidenceProvenance,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CallSearchRow {
    pub callee_text: String,
    pub call_line: usize,
    pub confidence: f64,
    pub resolution_status: String,
    pub symbol_id: Option<String>,
    pub symbol_name: Option<String>,
    pub symbol_qualified_name: Option<String>,
    pub symbol_kind: Option<String>,
    pub path: Option<String>,
    pub start_line: Option<usize>,
    pub end_line: Option<usize>,
    pub provenance: EvidenceProvenance,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CallPath {
    pub hops: usize,
    pub min_confidence: f64,
    pub terminal_resolution_status: String,
    pub edges: Vec<CallPathEdge>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CallPathEdge {
    pub call_id: String,
    pub caller_symbol_id: String,
    pub caller_symbol_name: String,
    pub caller_symbol_qualified_name: String,
    pub caller_symbol_kind: String,
    pub caller_path: String,
    pub caller_start_line: usize,
    pub caller_end_line: usize,
    pub callee_text: String,
    pub callee_symbol_id: Option<String>,
    pub callee_symbol_name: Option<String>,
    pub callee_symbol_qualified_name: Option<String>,
    pub callee_symbol_kind: Option<String>,
    pub callee_path: Option<String>,
    pub callee_start_line: Option<usize>,
    pub callee_end_line: Option<usize>,
    pub call_line: usize,
    pub confidence: f64,
    pub resolution_status: String,
    pub provenance: EvidenceProvenance,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ContextPack {
    pub format: String,
    pub repository_id: String,
    pub query: String,
    pub focus_symbols: Vec<SymbolSearchRow>,
    pub direct_callers: Vec<CallSearchRow>,
    pub direct_callees: Vec<CallSearchRow>,
    pub files: Vec<String>,
    pub limits: ContextPackLimits,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ContextPackLimits {
    pub max_symbols: usize,
    pub max_callers: usize,
    pub max_callees: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EvidenceProvenance {
    pub content_hash: Option<String>,
    pub index_run_id: Option<String>,
    pub parser_version: Option<String>,
    pub indexed_at: Option<String>,
    pub embedding_model: Option<String>,
    pub embedding_dimension: Option<usize>,
    pub embedded_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileFreshnessSnapshot {
    pub path: String,
    pub content_hash: String,
    pub indexed_at: String,
    pub index_run_id: Option<String>,
    pub parser_version: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum EvidenceFreshness {
    Fresh,
    Stale,
    Deleted,
    Missing,
    Unknown,
}

impl EvidenceFreshness {
    pub fn label(self) -> &'static str {
        match self {
            Self::Fresh => "fresh",
            Self::Stale => "stale",
            Self::Deleted => "deleted",
            Self::Missing => "missing",
            Self::Unknown => "unknown",
        }
    }
}

pub fn freshness_for_hash(
    indexed_content_hash: Option<&str>,
    current_content_hash: Option<&str>,
) -> EvidenceFreshness {
    match (indexed_content_hash, current_content_hash) {
        (Some(indexed), Some(current)) if indexed == current => EvidenceFreshness::Fresh,
        (Some(_), Some(_)) => EvidenceFreshness::Stale,
        (Some(_), None) => EvidenceFreshness::Deleted,
        (None, Some(_)) => EvidenceFreshness::Missing,
        (None, None) => EvidenceFreshness::Unknown,
    }
}

fn env_path(upper: &str, legacy: &str) -> Option<PathBuf> {
    env_value(upper, legacy).map(PathBuf::from)
}

fn env_value(upper: &str, legacy: &str) -> Option<String> {
    env::var(upper).ok().or_else(|| env::var(legacy).ok())
}

#[derive(Debug)]
pub enum StoreError {
    Io(std::io::Error),
    Sqlite(rusqlite::Error),
    HttpClient(reqwest::Error),
    HttpRequest(reqwest::Error),
    HttpStatus(reqwest::Error),
    Decode(reqwest::Error),
    InvalidCollectionName(String),
    InvalidPointId(String),
    InvalidVectorSize(usize),
    InvalidLimit(usize),
    InconsistentVectorDimensions,
    EmbeddingDimensionChanged {
        repository_id: String,
        embedding_model: String,
        previous_dimension: usize,
        current_dimension: usize,
    },
    UnexpectedResponse(String),
}

impl Display for StoreError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "filesystem error: {error}"),
            Self::Sqlite(error) => write!(f, "SQLite error: {error}"),
            Self::HttpClient(error) => write!(f, "failed to create HTTP client: {error}"),
            Self::HttpRequest(error) => write!(f, "Qdrant request failed: {error}"),
            Self::HttpStatus(error) => write!(f, "Qdrant returned an error status: {error}"),
            Self::Decode(error) => write!(f, "failed to decode Qdrant response: {error}"),
            Self::InvalidCollectionName(name) => {
                write!(f, "invalid Qdrant collection name `{name}`")
            }
            Self::InvalidPointId(id) => write!(f, "invalid Qdrant point id source `{id}`"),
            Self::InvalidVectorSize(size) => write!(f, "invalid Qdrant vector size `{size}`"),
            Self::InvalidLimit(limit) => write!(f, "invalid Qdrant query limit `{limit}`"),
            Self::InconsistentVectorDimensions => {
                write!(f, "Qdrant points have inconsistent vector dimensions")
            }
            Self::EmbeddingDimensionChanged {
                repository_id,
                embedding_model,
                previous_dimension,
                current_dimension,
            } => write!(
                f,
                "embedding dimension changed for repository `{repository_id}` and model `{embedding_model}`: previous={previous_dimension} current={current_dimension}; reset the collection or use a new model name before reindexing"
            ),
            Self::UnexpectedResponse(message) => write!(f, "unexpected Qdrant response: {message}"),
        }
    }
}

impl std::error::Error for StoreError {}

pub type Result<T> = std::result::Result<T, StoreError>;

fn collect_rows<T>(
    rows: rusqlite::MappedRows<'_, impl FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<T>>,
) -> Result<Vec<T>> {
    let mut values = Vec::new();
    for row in rows {
        values.push(row.map_err(StoreError::Sqlite)?);
    }
    Ok(values)
}

fn call_search_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<CallSearchRow> {
    Ok(CallSearchRow {
        callee_text: row.get(0)?,
        call_line: row.get::<_, i64>(1)? as usize,
        confidence: row.get(2)?,
        resolution_status: row.get(3)?,
        symbol_id: row.get(4)?,
        symbol_name: row.get(5)?,
        symbol_qualified_name: row.get(6)?,
        symbol_kind: row.get(7)?,
        path: row.get(8)?,
        start_line: row.get::<_, Option<i64>>(9)?.map(|line| line as usize),
        end_line: row.get::<_, Option<i64>>(10)?.map(|line| line as usize),
        provenance: EvidenceProvenance {
            content_hash: row.get(11)?,
            index_run_id: row.get(12)?,
            parser_version: row.get(13)?,
            indexed_at: row.get(14)?,
            embedding_model: None,
            embedding_dimension: None,
            embedded_at: None,
        },
    })
}

fn call_path_edge(row: &rusqlite::Row<'_>) -> rusqlite::Result<CallPathEdge> {
    Ok(CallPathEdge {
        call_id: row.get(0)?,
        callee_text: row.get(1)?,
        call_line: row.get::<_, i64>(2)? as usize,
        confidence: row.get(3)?,
        resolution_status: row.get(4)?,
        caller_symbol_id: row.get(5)?,
        caller_symbol_name: row.get(6)?,
        caller_symbol_qualified_name: row.get(7)?,
        caller_symbol_kind: row.get(8)?,
        caller_path: row.get(9)?,
        caller_start_line: row.get::<_, i64>(10)? as usize,
        caller_end_line: row.get::<_, i64>(11)? as usize,
        callee_symbol_id: row.get(12)?,
        callee_symbol_name: row.get(13)?,
        callee_symbol_qualified_name: row.get(14)?,
        callee_symbol_kind: row.get(15)?,
        callee_path: row.get(16)?,
        callee_start_line: row.get::<_, Option<i64>>(17)?.map(|line| line as usize),
        callee_end_line: row.get::<_, Option<i64>>(18)?.map(|line| line as usize),
        provenance: EvidenceProvenance {
            content_hash: row.get(19)?,
            index_run_id: row.get(20)?,
            parser_version: row.get(21)?,
            indexed_at: row.get(22)?,
            embedding_model: None,
            embedding_dimension: None,
            embedded_at: None,
        },
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SymbolRef {
    id: String,
}

struct TraceContext<'a> {
    target_query: &'a str,
    target_ids: &'a std::collections::BTreeSet<String>,
    edges_by_caller: &'a std::collections::BTreeMap<String, Vec<CallPathEdge>>,
}

fn trace_call_paths(
    current_symbol_id: &str,
    remaining_depth: usize,
    context: &TraceContext<'_>,
    visited: &mut std::collections::BTreeSet<String>,
    stack: &mut Vec<CallPathEdge>,
    paths: &mut Vec<CallPath>,
) {
    if remaining_depth == 0 || paths.len() >= 50 {
        return;
    }
    let Some(edges) = context.edges_by_caller.get(current_symbol_id) else {
        return;
    };
    for edge in edges {
        stack.push(edge.clone());
        if call_edge_reaches_target(edge, context.target_query, context.target_ids) {
            paths.push(call_path_from_edges(stack));
            stack.pop();
            if paths.len() >= 50 {
                return;
            }
            continue;
        }
        if let Some(next_symbol_id) = &edge.callee_symbol_id
            && visited.insert(next_symbol_id.clone())
        {
            trace_call_paths(
                next_symbol_id,
                remaining_depth.saturating_sub(1),
                context,
                visited,
                stack,
                paths,
            );
            visited.remove(next_symbol_id);
        }
        stack.pop();
    }
}

fn trace_reachable_call_paths(
    current_symbol_id: &str,
    remaining_depth: usize,
    edges_by_caller: &std::collections::BTreeMap<String, Vec<CallPathEdge>>,
    visited: &mut std::collections::BTreeSet<String>,
    stack: &mut Vec<CallPathEdge>,
    paths: &mut Vec<CallPath>,
) {
    if remaining_depth == 0 || paths.len() >= 50 {
        return;
    }
    let Some(edges) = edges_by_caller.get(current_symbol_id) else {
        return;
    };
    for edge in edges {
        stack.push(edge.clone());
        // Impact already reports direct callees separately, so this traversal
        // only materializes bounded transitive paths.
        if stack.len() > 1 {
            paths.push(call_path_from_edges(stack));
            if paths.len() >= 50 {
                stack.pop();
                return;
            }
        }
        if let Some(next_symbol_id) = &edge.callee_symbol_id
            && visited.insert(next_symbol_id.clone())
        {
            trace_reachable_call_paths(
                next_symbol_id,
                remaining_depth.saturating_sub(1),
                edges_by_caller,
                visited,
                stack,
                paths,
            );
            visited.remove(next_symbol_id);
        }
        stack.pop();
    }
}

fn call_edge_reaches_target(
    edge: &CallPathEdge,
    target_query: &str,
    target_ids: &std::collections::BTreeSet<String>,
) -> bool {
    edge.callee_symbol_id
        .as_ref()
        .is_some_and(|symbol_id| target_ids.contains(symbol_id))
        || edge.callee_text == target_query
        || edge
            .callee_symbol_name
            .as_ref()
            .is_some_and(|name| name == target_query)
        || edge
            .callee_symbol_qualified_name
            .as_ref()
            .is_some_and(|name| name == target_query)
}

fn call_path_from_edges(edges: &[CallPathEdge]) -> CallPath {
    CallPath {
        hops: edges.len(),
        min_confidence: edges
            .iter()
            .map(|edge| edge.confidence)
            .fold(1.0_f64, f64::min),
        terminal_resolution_status: edges
            .last()
            .map(|edge| edge.resolution_status.clone())
            .unwrap_or_else(|| "unresolved".to_owned()),
        edges: edges.to_vec(),
    }
}

fn embedding_index_metadata(row: &rusqlite::Row<'_>) -> rusqlite::Result<EmbeddingIndexMetadata> {
    Ok(EmbeddingIndexMetadata {
        embedding_model: row.get(0)?,
        embedding_dimension: row
            .get::<_, Option<i64>>(1)?
            .map(|dimension| dimension as usize),
        chunks_embedded: row.get::<_, i64>(2)? as usize,
    })
}

#[derive(Debug, Clone, Copy)]
struct ChunkProjectionCounts {
    embeddable_chunks: usize,
    vector_backed_chunks: usize,
    excluded_chunks: usize,
    missing_vector_chunks: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FileCoverageBaseRow {
    file_id: String,
    path: String,
    language: String,
    chunks: usize,
    symbols: usize,
    calls: usize,
    embeddable_chunks: usize,
    vector_backed_chunks: usize,
    excluded_chunks: usize,
    status: FileCoverageStatus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SymbolOutlineBaseRow {
    id: String,
    parent_symbol_id: Option<String>,
    kind: String,
    qualified_name: String,
    name: String,
    start_line: usize,
    end_line: usize,
    path: String,
}

fn storage_warnings(
    sqlite: &SqliteStorageSummary,
    qdrant: &QdrantStorageProjection,
) -> Vec<StorageHealthRow> {
    let mut rows = Vec::new();
    if sqlite.repositories == 0 {
        rows.push(StorageHealthRow {
            status: StorageHealthStatus::Error,
            label: "repository_missing".to_owned(),
            detail: "SQLite has no repository row for the selected root.".to_owned(),
        });
    }
    if sqlite.index_runs == 0 {
        rows.push(StorageHealthRow {
            status: StorageHealthStatus::Warning,
            label: "no_index_runs".to_owned(),
            detail: "No index run metadata has been recorded yet.".to_owned(),
        });
    }
    if qdrant.missing_vector_chunks > 0 {
        rows.push(StorageHealthRow {
            status: StorageHealthStatus::Warning,
            label: "missing_vectors".to_owned(),
            detail: format!(
                "{} embeddable chunks do not have Qdrant point IDs.",
                qdrant.missing_vector_chunks
            ),
        });
    }
    if qdrant.excluded_chunks > 0 {
        rows.push(StorageHealthRow {
            status: StorageHealthStatus::Warning,
            label: "excluded_chunks".to_owned(),
            detail: format!(
                "{} chunks are intentionally metadata-only.",
                qdrant.excluded_chunks
            ),
        });
    }
    if rows.is_empty() {
        rows.push(StorageHealthRow {
            status: StorageHealthStatus::Ok,
            label: "coverage_ok".to_owned(),
            detail: "SQLite metadata and vector-backed chunk counts are aligned.".to_owned(),
        });
    }
    rows
}

fn embedding_coverage_health(
    status: &RepositoryStatus,
    projection: &ChunkProjectionCounts,
    latest_embedding: Option<&EmbeddingIndexMetadata>,
    configured_embedding_model: &str,
) -> Vec<StorageHealthRow> {
    let mut rows = Vec::new();
    if projection.missing_vector_chunks > 0 {
        rows.push(StorageHealthRow {
            status: StorageHealthStatus::Warning,
            label: "missing_vectors".to_owned(),
            detail: format!(
                "{} embeddable chunks have no recorded Qdrant point ID.",
                projection.missing_vector_chunks
            ),
        });
    }
    if projection.excluded_chunks > 0 {
        rows.push(StorageHealthRow {
            status: StorageHealthStatus::Warning,
            label: "excluded_chunks".to_owned(),
            detail: format!(
                "{} chunks are intentionally excluded from embeddings.",
                projection.excluded_chunks
            ),
        });
    }
    if projection.embeddable_chunks > 0 && latest_embedding.is_none() {
        rows.push(StorageHealthRow {
            status: StorageHealthStatus::Warning,
            label: "no_embedding_run".to_owned(),
            detail: "No successful semantic index run has recorded model/dimension metadata."
                .to_owned(),
        });
    }
    if let Some(latest) = latest_embedding {
        if latest.embedding_model != configured_embedding_model {
            rows.push(StorageHealthRow {
                status: StorageHealthStatus::Error,
                label: "model_drift".to_owned(),
                detail: format!(
                    "Configured model {configured_embedding_model} differs from latest indexed model {}.",
                    latest.embedding_model
                ),
            });
        }
        if latest.embedding_dimension.is_none() {
            rows.push(StorageHealthRow {
                status: StorageHealthStatus::Error,
                label: "dimension_missing".to_owned(),
                detail: "Latest successful semantic run did not record a vector dimension."
                    .to_owned(),
            });
        }
        if latest.chunks_embedded != projection.vector_backed_chunks {
            rows.push(StorageHealthRow {
                status: StorageHealthStatus::Warning,
                label: "run_count_mismatch".to_owned(),
                detail: format!(
                    "Latest run embedded {} chunks, while SQLite records {} vector-backed chunks.",
                    latest.chunks_embedded, projection.vector_backed_chunks
                ),
            });
        }
    }
    if status.chunks_indexed == 0 {
        rows.push(StorageHealthRow {
            status: StorageHealthStatus::Warning,
            label: "no_chunks".to_owned(),
            detail: "No SQLite chunks are indexed for this repository.".to_owned(),
        });
    }
    rows
}

fn semantic_neighborhood_health(rows: &[SemanticNeighborhoodRow]) -> Vec<StorageHealthRow> {
    if rows.is_empty() {
        return vec![StorageHealthRow {
            status: StorageHealthStatus::Warning,
            label: "no_vector_payloads".to_owned(),
            detail: "No vector-backed chunk metadata is recorded for semantic inspection."
                .to_owned(),
        }];
    }
    vec![StorageHealthRow {
        status: StorageHealthStatus::Ok,
        label: "metadata_only".to_owned(),
        detail: format!(
            "{} Qdrant payload metadata rows are available without source text.",
            rows.len()
        ),
    }]
}

fn file_coverage_status(
    chunks: usize,
    embeddable_chunks: usize,
    vector_backed_chunks: usize,
    excluded_chunks: usize,
) -> FileCoverageStatus {
    if chunks > 0 && excluded_chunks == chunks {
        FileCoverageStatus::Excluded
    } else if embeddable_chunks > vector_backed_chunks {
        FileCoverageStatus::MissingVector
    } else if embeddable_chunks > 0 && embeddable_chunks == vector_backed_chunks {
        FileCoverageStatus::Covered
    } else {
        FileCoverageStatus::MetadataOnly
    }
}

fn chunk_vector_status(
    qdrant_point_id: &Option<String>,
    excluded_reason: &Option<String>,
) -> ChunkVectorStatus {
    if excluded_reason.is_some() {
        ChunkVectorStatus::Excluded
    } else if qdrant_point_id.is_some() {
        ChunkVectorStatus::VectorBacked
    } else {
        ChunkVectorStatus::MissingVector
    }
}

fn symbol_outline_rows(bases: Vec<SymbolOutlineBaseRow>) -> Vec<SymbolOutlineRow> {
    use std::collections::BTreeMap;

    let parents: BTreeMap<String, Option<String>> = bases
        .iter()
        .map(|row| (row.id.clone(), row.parent_symbol_id.clone()))
        .collect();
    let mut child_counts: BTreeMap<String, usize> = BTreeMap::new();
    for parent_id in bases.iter().filter_map(|row| row.parent_symbol_id.as_ref()) {
        *child_counts.entry(parent_id.clone()).or_default() += 1;
    }

    bases
        .into_iter()
        .map(|row| {
            let depth = symbol_depth(&row.parent_symbol_id, &parents);
            let child_count = child_counts.get(&row.id).copied().unwrap_or(0);
            SymbolOutlineRow {
                id: row.id,
                parent_symbol_id: row.parent_symbol_id,
                depth,
                child_count,
                kind: row.kind,
                qualified_name: row.qualified_name,
                name: row.name,
                path: row.path,
                start_line: row.start_line,
                end_line: row.end_line,
            }
        })
        .collect()
}

fn symbol_depth(
    parent_symbol_id: &Option<String>,
    parents: &std::collections::BTreeMap<String, Option<String>>,
) -> usize {
    let mut depth = 0;
    let mut current = parent_symbol_id.as_ref();
    while let Some(symbol_id) = current {
        depth += 1;
        if depth >= 16 {
            break;
        }
        current = parents.get(symbol_id).and_then(Option::as_ref);
    }
    depth
}

fn confidence_bucket(confidence: f64) -> ConfidenceBucket {
    if confidence >= 0.85 {
        ConfidenceBucket::High
    } else if confidence >= 0.5 {
        ConfidenceBucket::Medium
    } else {
        ConfidenceBucket::Low
    }
}

fn call_resolution_buckets(rows: Vec<CallResolutionEdgeRow>) -> Vec<CallResolutionBucket> {
    use std::collections::BTreeMap;

    let mut grouped: BTreeMap<(String, ConfidenceBucket), Vec<CallResolutionEdgeRow>> =
        BTreeMap::new();
    for row in rows {
        grouped
            .entry((row.resolution_status.clone(), row.confidence_bucket))
            .or_default()
            .push(row);
    }

    grouped
        .into_iter()
        .map(|((resolution_status, confidence_bucket), rows)| {
            let call_count = rows.len();
            let average_confidence = if call_count == 0 {
                0.0
            } else {
                rows.iter().map(|row| row.confidence).sum::<f64>() / call_count as f64
            };
            CallResolutionBucket {
                resolution_status,
                confidence_bucket,
                call_count,
                average_confidence,
                rows,
            }
        })
        .collect()
}

struct ProvenanceColumn {
    table: &'static str,
    name: &'static str,
    alter_sql: &'static str,
}

const PROVENANCE_COLUMNS: &[ProvenanceColumn] = &[
    ProvenanceColumn {
        table: "index_runs",
        name: "parser_version",
        alter_sql: "ALTER TABLE index_runs ADD COLUMN parser_version TEXT",
    },
    ProvenanceColumn {
        table: "index_runs",
        name: "indexer_version",
        alter_sql: "ALTER TABLE index_runs ADD COLUMN indexer_version TEXT",
    },
    ProvenanceColumn {
        table: "index_runs",
        name: "run_kind",
        alter_sql: "ALTER TABLE index_runs ADD COLUMN run_kind TEXT NOT NULL DEFAULT 'manual'",
    },
    ProvenanceColumn {
        table: "files",
        name: "index_run_id",
        alter_sql: "ALTER TABLE files ADD COLUMN index_run_id TEXT",
    },
    ProvenanceColumn {
        table: "files",
        name: "parser_version",
        alter_sql: "ALTER TABLE files ADD COLUMN parser_version TEXT",
    },
    ProvenanceColumn {
        table: "symbols",
        name: "index_run_id",
        alter_sql: "ALTER TABLE symbols ADD COLUMN index_run_id TEXT",
    },
    ProvenanceColumn {
        table: "symbols",
        name: "parser_version",
        alter_sql: "ALTER TABLE symbols ADD COLUMN parser_version TEXT",
    },
    ProvenanceColumn {
        table: "chunks",
        name: "index_run_id",
        alter_sql: "ALTER TABLE chunks ADD COLUMN index_run_id TEXT",
    },
    ProvenanceColumn {
        table: "chunks",
        name: "parser_version",
        alter_sql: "ALTER TABLE chunks ADD COLUMN parser_version TEXT",
    },
    ProvenanceColumn {
        table: "chunks",
        name: "embedding_model",
        alter_sql: "ALTER TABLE chunks ADD COLUMN embedding_model TEXT",
    },
    ProvenanceColumn {
        table: "chunks",
        name: "embedding_dimension",
        alter_sql: "ALTER TABLE chunks ADD COLUMN embedding_dimension INTEGER",
    },
    ProvenanceColumn {
        table: "chunks",
        name: "embedded_at",
        alter_sql: "ALTER TABLE chunks ADD COLUMN embedded_at TEXT",
    },
    ProvenanceColumn {
        table: "calls",
        name: "index_run_id",
        alter_sql: "ALTER TABLE calls ADD COLUMN index_run_id TEXT",
    },
    ProvenanceColumn {
        table: "calls",
        name: "parser_version",
        alter_sql: "ALTER TABLE calls ADD COLUMN parser_version TEXT",
    },
];

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS repositories (
  id TEXT PRIMARY KEY,
  root_path TEXT NOT NULL UNIQUE,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS index_runs (
  id TEXT PRIMARY KEY,
  repository_id TEXT NOT NULL,
  started_at TEXT NOT NULL,
  finished_at TEXT,
  status TEXT NOT NULL,
  embedding_model TEXT NOT NULL,
  embedding_dimension INTEGER,
  files_seen INTEGER DEFAULT 0,
  files_indexed INTEGER DEFAULT 0,
  chunks_embedded INTEGER DEFAULT 0,
  error_summary TEXT,
  parser_version TEXT,
  indexer_version TEXT,
  run_kind TEXT NOT NULL DEFAULT 'manual'
);

CREATE TABLE IF NOT EXISTS files (
  id TEXT PRIMARY KEY,
  repository_id TEXT NOT NULL,
  path TEXT NOT NULL,
  language TEXT NOT NULL,
  content_hash TEXT NOT NULL,
  indexed_at TEXT NOT NULL,
  index_run_id TEXT,
  parser_version TEXT,
  UNIQUE(repository_id, path),
  FOREIGN KEY(repository_id) REFERENCES repositories(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS symbols (
  id TEXT PRIMARY KEY,
  file_id TEXT NOT NULL,
  parent_symbol_id TEXT,
  name TEXT NOT NULL,
  qualified_name TEXT,
  kind TEXT NOT NULL,
  signature TEXT,
  start_line INTEGER NOT NULL,
  end_line INTEGER NOT NULL,
  start_byte INTEGER NOT NULL,
  end_byte INTEGER NOT NULL,
  index_run_id TEXT,
  parser_version TEXT,
  FOREIGN KEY(file_id) REFERENCES files(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS chunks (
  id TEXT PRIMARY KEY,
  file_id TEXT NOT NULL,
  symbol_id TEXT,
  kind TEXT NOT NULL,
  text_hash TEXT NOT NULL,
  start_line INTEGER NOT NULL,
  end_line INTEGER NOT NULL,
  start_byte INTEGER NOT NULL,
  end_byte INTEGER NOT NULL,
  qdrant_point_id TEXT,
  excluded_reason TEXT,
  index_run_id TEXT,
  parser_version TEXT,
  embedding_model TEXT,
  embedding_dimension INTEGER,
  embedded_at TEXT,
  FOREIGN KEY(file_id) REFERENCES files(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS calls (
  id TEXT PRIMARY KEY,
  caller_symbol_id TEXT NOT NULL,
  callee_text TEXT NOT NULL,
  callee_symbol_id TEXT,
  call_line INTEGER NOT NULL,
  confidence REAL NOT NULL,
  resolution_status TEXT NOT NULL,
  index_run_id TEXT,
  parser_version TEXT
);

CREATE INDEX IF NOT EXISTS idx_files_repository_path ON files(repository_id, path);
CREATE INDEX IF NOT EXISTS idx_chunks_file_id ON chunks(file_id);
CREATE INDEX IF NOT EXISTS idx_symbols_file_id ON symbols(file_id);
CREATE INDEX IF NOT EXISTS idx_symbols_name ON symbols(name);
CREATE INDEX IF NOT EXISTS idx_symbols_qualified_name ON symbols(qualified_name);
CREATE INDEX IF NOT EXISTS idx_calls_caller_symbol_id ON calls(caller_symbol_id);
CREATE INDEX IF NOT EXISTS idx_calls_callee_symbol_id ON calls(callee_symbol_id);
CREATE INDEX IF NOT EXISTS idx_index_runs_repository_status ON index_runs(repository_id, status, finished_at);
CREATE INDEX IF NOT EXISTS idx_index_runs_repository_model_status ON index_runs(repository_id, embedding_model, status, finished_at);
"#;

pub fn current_timestamp() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    seconds.to_string()
}

fn timestamp() -> String {
    current_timestamp()
}

fn timestamp_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    use rusqlite::params;

    use crate::{
        CallRecord, ChunkRecord, ChunkVectorStatus, ConfidenceBucket, CreateCollectionRequest,
        DeletePointsRequest, Distance, FileCoverageStatus, FileRecord, MatchValue, PointPayload,
        QdrantClient, QueryPointsRequest, RepositoryFilter, RepositoryFilterCondition,
        RepositoryRecord, ScrollPointsRequest, SqliteStore, StorageHealthStatus, StoreConfig,
        StoreError, SymbolRecord, UpsertPointsRequest, VectorParams, VectorPoint,
        qdrant_collection_name, qdrant_point_id, validate_collection_name,
    };

    #[test]
    fn collection_name_is_deterministic_and_safe() {
        assert_eq!(
            qdrant_collection_name("Repo-ID_123", "nomic-embed-text:latest"),
            "symdex_repo_id_123_nomic_embed_text_latest"
        );
    }

    #[test]
    fn validates_collection_names() {
        assert!(validate_collection_name("symdex_repo_model").is_ok());
        assert!(validate_collection_name("").is_err());
        assert!(validate_collection_name("../bad").is_err());
    }

    #[test]
    fn create_collection_request_uses_cosine_vectors() {
        let request = CreateCollectionRequest {
            vectors: VectorParams {
                size: 768,
                distance: Distance::Cosine,
            },
        };

        let json = serde_json::to_value(request).expect("request should serialize");
        assert_eq!(json["vectors"]["size"], 768);
        assert_eq!(json["vectors"]["distance"], "Cosine");
    }

    #[test]
    fn qdrant_point_id_formats_stable_hash_as_uuid() {
        assert_eq!(
            qdrant_point_id("0123456789abcdeffedcba9876543210").expect("point id should format"),
            "01234567-89ab-cdef-fedc-ba9876543210"
        );
        assert!(qdrant_point_id("not-hex").is_err());
    }

    #[test]
    fn freshness_for_hash_labels_evidence_states() {
        assert_eq!(
            crate::freshness_for_hash(Some("same"), Some("same")),
            crate::EvidenceFreshness::Fresh
        );
        assert_eq!(
            crate::freshness_for_hash(Some("old"), Some("new")),
            crate::EvidenceFreshness::Stale
        );
        assert_eq!(
            crate::freshness_for_hash(Some("old"), None),
            crate::EvidenceFreshness::Deleted
        );
        assert_eq!(
            crate::freshness_for_hash(None, Some("new")),
            crate::EvidenceFreshness::Missing
        );
        assert_eq!(
            crate::freshness_for_hash(None, None),
            crate::EvidenceFreshness::Unknown
        );
    }

    #[test]
    fn upsert_points_request_uses_payload_without_source_text() {
        let point = VectorPoint {
            id: "01234567-89ab-cdef-fedc-ba9876543210".to_owned(),
            vector: vec![0.1, 0.2],
            payload: sample_payload(),
        };
        let points = vec![point];
        let request = UpsertPointsRequest { points: &points };

        let json = serde_json::to_value(request).expect("request should serialize");

        assert_eq!(
            json["points"][0]["id"],
            "01234567-89ab-cdef-fedc-ba9876543210"
        );
        assert_eq!(
            json["points"][0]["vector"].as_array().expect("vector")[0]
                .as_f64()
                .expect("number") as f32,
            0.1
        );
        assert_eq!(json["points"][0]["payload"]["path"], "src/lib.rs");
        assert!(json["points"][0]["payload"].get("source_text").is_none());
    }

    #[test]
    fn query_points_request_asks_for_payload_not_vectors() {
        let request = QueryPointsRequest {
            query: vec![0.1, 0.2],
            limit: 5,
            with_payload: true,
            with_vector: false,
        };

        let json = serde_json::to_value(request).expect("request should serialize");

        assert_eq!(
            json["query"].as_array().expect("query")[1]
                .as_f64()
                .expect("number") as f32,
            0.2
        );
        assert_eq!(json["limit"], 5);
        assert_eq!(json["with_payload"], true);
        assert_eq!(json["with_vector"], false);
    }

    #[test]
    fn delete_points_request_uses_point_ids_without_source_text() {
        let point_ids = vec!["01234567-89ab-cdef-fedc-ba9876543210".to_owned()];
        let request = DeletePointsRequest { points: &point_ids };

        let json = serde_json::to_value(request).expect("request should serialize");

        assert_eq!(json["points"][0], "01234567-89ab-cdef-fedc-ba9876543210");
        assert!(json.get("source_text").is_none());
    }

    #[test]
    fn scroll_points_request_filters_by_repository_without_source_text() {
        let request = ScrollPointsRequest {
            filter: RepositoryFilter {
                must: vec![RepositoryFilterCondition {
                    key: "repository_id",
                    value_match: MatchValue { value: "repo" },
                }],
            },
            limit: 256,
            with_payload: true,
            with_vector: false,
            offset: None,
        };

        let json = serde_json::to_value(request).expect("request should serialize");

        assert_eq!(json["filter"]["must"][0]["key"], "repository_id");
        assert_eq!(json["filter"]["must"][0]["match"]["value"], "repo");
        assert_eq!(json["with_payload"], true);
        assert_eq!(json["with_vector"], false);
        assert!(json.get("source_text").is_none());
    }

    #[test]
    fn sqlite_migrates_and_persists_file_chunks() {
        let db = TestDb::new("persist");
        let mut store = SqliteStore::open(&db.config()).expect("store should open");
        store.migrate().expect("migration should run");
        store
            .upsert_repository(&RepositoryRecord {
                id: "repo".to_owned(),
                root_path: "/tmp/repo".to_owned(),
            })
            .expect("repository should persist");

        store
            .replace_file_facts(&sample_file("hash-1"), &[], &[sample_chunk("chunk-1")], &[])
            .expect("file chunks should persist");

        assert!(
            store
                .file_unchanged("repo", "src/lib.rs", "hash-1", "parser")
                .expect("unchanged check should run")
        );
        assert!(
            !store
                .file_unchanged("repo", "src/lib.rs", "hash-1", "next-parser")
                .expect("parser-version check should run")
        );
        let status = store.repository_status("repo").expect("status should load");
        assert_eq!(status.files_indexed, 1);
        assert_eq!(status.chunks_indexed, 1);
        assert!(status.last_indexed_at.is_some());
    }

    #[test]
    fn sqlite_migration_creates_query_indexes() {
        let db = TestDb::new("indexes");
        let store = SqliteStore::open(&db.config()).expect("store should open");
        store.migrate().expect("migration should run");

        let indexes = sqlite_index_names(&store);

        for expected in [
            "idx_files_repository_path",
            "idx_chunks_file_id",
            "idx_symbols_file_id",
            "idx_symbols_name",
            "idx_symbols_qualified_name",
            "idx_calls_caller_symbol_id",
            "idx_calls_callee_symbol_id",
            "idx_index_runs_repository_status",
            "idx_index_runs_repository_model_status",
        ] {
            assert!(
                indexes.iter().any(|index| index == expected),
                "missing SQLite index {expected}; found {indexes:?}"
            );
        }
    }

    #[test]
    fn sqlite_migration_creates_provenance_columns() {
        let db = TestDb::new("provenance-columns");
        let store = SqliteStore::open(&db.config()).expect("store should open");
        store.migrate().expect("migration should run");

        for (table, column) in [
            ("index_runs", "parser_version"),
            ("index_runs", "indexer_version"),
            ("index_runs", "run_kind"),
            ("files", "index_run_id"),
            ("files", "parser_version"),
            ("symbols", "index_run_id"),
            ("symbols", "parser_version"),
            ("chunks", "index_run_id"),
            ("chunks", "parser_version"),
            ("chunks", "embedding_model"),
            ("chunks", "embedding_dimension"),
            ("chunks", "embedded_at"),
            ("calls", "index_run_id"),
            ("calls", "parser_version"),
        ] {
            assert!(
                store
                    .column_exists(table, column)
                    .expect("column check should run"),
                "missing {table}.{column}"
            );
        }
    }

    #[test]
    fn sqlite_replaces_chunks_and_removes_deleted_files() {
        let db = TestDb::new("cleanup");
        let mut store = SqliteStore::open(&db.config()).expect("store should open");
        store.migrate().expect("migration should run");
        store
            .upsert_repository(&RepositoryRecord {
                id: "repo".to_owned(),
                root_path: "/tmp/repo".to_owned(),
            })
            .expect("repository should persist");
        store
            .replace_file_facts(&sample_file("hash-1"), &[], &[sample_chunk("chunk-1")], &[])
            .expect("initial chunks should persist");
        store
            .replace_file_facts(
                &sample_file("hash-2"),
                &[],
                &[sample_chunk("chunk-2"), sample_chunk("chunk-3")],
                &[],
            )
            .expect("replacement chunks should persist");

        let status = store.repository_status("repo").expect("status should load");
        assert_eq!(status.files_indexed, 1);
        assert_eq!(status.chunks_indexed, 2);
        assert!(
            !store
                .file_unchanged("repo", "src/lib.rs", "hash-1", "parser")
                .expect("unchanged check should run")
        );

        let removed = store
            .remove_missing_files("repo", &[])
            .expect("cleanup should run");
        assert_eq!(removed, 1);
        let status = store.repository_status("repo").expect("status should load");
        assert_eq!(status.files_indexed, 0);
        assert_eq!(status.chunks_indexed, 0);
    }

    #[test]
    fn sqlite_collects_qdrant_point_ids_before_replacement_and_deletion() {
        let db = TestDb::new("qdrant-point-cleanup");
        let mut store = SqliteStore::open(&db.config()).expect("store should open");
        store.migrate().expect("migration should run");
        store
            .upsert_repository(&RepositoryRecord {
                id: "repo".to_owned(),
                root_path: "/tmp/repo".to_owned(),
            })
            .expect("repository should persist");

        let mut other_file = sample_file("hash-other");
        other_file.id = "other-file".to_owned();
        other_file.path = "src/other.rs".to_owned();
        let mut other_chunk = sample_chunk("other-chunk");
        other_chunk.file_id = "other-file".to_owned();
        other_chunk.qdrant_point_id = Some("11111111-1111-1111-1111-111111111111".to_owned());

        store
            .replace_file_facts(&sample_file("hash-1"), &[], &[sample_chunk("chunk-1")], &[])
            .expect("file chunks should persist");
        store
            .replace_file_facts(&other_file, &[], &[other_chunk], &[])
            .expect("other file chunks should persist");

        let replaced = store
            .qdrant_point_ids_for_paths("repo", &["src/lib.rs".to_owned()])
            .expect("point ids should load");
        assert_eq!(replaced, vec!["01234567-89ab-cdef-fedc-ba9876543210"]);

        let missing = store
            .qdrant_point_ids_for_missing_files("repo", &["src/lib.rs".to_owned()])
            .expect("missing point ids should load");
        assert_eq!(missing, vec!["11111111-1111-1111-1111-111111111111"]);
    }

    #[test]
    fn sqlite_builds_qdrant_expected_point_manifest() {
        let db = TestDb::new("qdrant-expected-points");
        let mut store = SqliteStore::open(&db.config()).expect("store should open");
        store.migrate().expect("migration should run");
        store
            .upsert_repository(&RepositoryRecord {
                id: "repo".to_owned(),
                root_path: "/tmp/repo".to_owned(),
            })
            .expect("repository should persist");

        let mut vector = sample_chunk("chunk-vector");
        vector.embedding_model = Some("nomic-embed-text".to_owned());
        vector.embedding_dimension = Some(768);
        store
            .replace_file_facts(
                &sample_file("hash-1"),
                &[],
                &[vector, missing_vector_chunk("chunk-missing")],
                &[],
            )
            .expect("facts should persist");

        let manifest = store
            .qdrant_expected_points("repo")
            .expect("expected points should load");

        assert_eq!(manifest.len(), 1);
        assert_eq!(manifest[0].chunk_id, "chunk-vector");
        assert_eq!(manifest[0].path, "src/lib.rs");
        assert_eq!(
            manifest[0].embedding_model.as_deref(),
            Some("nomic-embed-text")
        );
        assert_eq!(manifest[0].embedding_dimension, Some(768));
    }

    #[test]
    fn sqlite_persists_index_provenance_metadata() {
        let db = TestDb::new("provenance-values");
        let mut store = SqliteStore::open(&db.config()).expect("store should open");
        store.migrate().expect("migration should run");
        store
            .upsert_repository(&RepositoryRecord {
                id: "repo".to_owned(),
                root_path: "/tmp/repo".to_owned(),
            })
            .expect("repository should persist");

        let symbols = vec![sample_symbol("symbol", "caller", "caller")];
        let calls = vec![CallRecord {
            id: "call-1".to_owned(),
            caller_symbol_id: "symbol".to_owned(),
            callee_text: "helper".to_owned(),
            callee_symbol_id: None,
            call_line: 4,
            confidence: 0.25,
            resolution_status: "unresolved".to_owned(),
            index_run_id: "run".to_owned(),
            parser_version: "parser".to_owned(),
        }];
        store
            .replace_file_facts(
                &sample_file("hash-1"),
                &symbols,
                &[sample_chunk("chunk-1")],
                &calls,
            )
            .expect("facts should persist");
        store
            .record_chunk_embedding_provenance(&["chunk-1".to_owned()], "nomic-embed-text", 768)
            .expect("chunk provenance should update");
        store
            .record_index_run(&sample_index_run("nomic-embed-text", 768))
            .expect("index run should persist");

        let file_provenance: (String, String) = store
            .connection
            .query_row(
                "SELECT index_run_id, parser_version FROM files WHERE id = 'file'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("file provenance should load");
        assert_eq!(file_provenance, ("run".to_owned(), "parser".to_owned()));

        let chunk_provenance: (String, String, String, i64, Option<String>) = store
            .connection
            .query_row(
                "SELECT index_run_id, parser_version, embedding_model,
                        embedding_dimension, embedded_at
                 FROM chunks
                 WHERE id = 'chunk-1'",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .expect("chunk provenance should load");
        assert_eq!(chunk_provenance.0, "run");
        assert_eq!(chunk_provenance.1, "parser");
        assert_eq!(chunk_provenance.2, "nomic-embed-text");
        assert_eq!(chunk_provenance.3, 768);
        assert!(chunk_provenance.4.is_some());

        let symbol_provenance: (String, String) = store
            .connection
            .query_row(
                "SELECT index_run_id, parser_version FROM symbols WHERE id = 'symbol'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("symbol provenance should load");
        assert_eq!(symbol_provenance, ("run".to_owned(), "parser".to_owned()));

        let call_provenance: (String, String) = store
            .connection
            .query_row(
                "SELECT index_run_id, parser_version FROM calls WHERE id = 'call-1'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("call provenance should load");
        assert_eq!(call_provenance, ("run".to_owned(), "parser".to_owned()));

        let run_provenance: (String, String, String) = store
            .connection
            .query_row(
                "SELECT parser_version, indexer_version, run_kind FROM index_runs LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("run provenance should load");
        assert_eq!(
            run_provenance,
            (
                "parser".to_owned(),
                "indexer".to_owned(),
                "semantic".to_owned()
            )
        );
    }

    #[test]
    fn sqlite_persists_symbols_and_calls_for_queries() {
        let db = TestDb::new("symbols-calls");
        let mut store = SqliteStore::open(&db.config()).expect("store should open");
        store.migrate().expect("migration should run");
        store
            .upsert_repository(&RepositoryRecord {
                id: "repo".to_owned(),
                root_path: "/tmp/repo".to_owned(),
            })
            .expect("repository should persist");

        let symbols = vec![
            sample_symbol("caller-symbol", "caller", "caller"),
            sample_symbol("callee-symbol", "helper", "helper"),
        ];
        let calls = vec![CallRecord {
            id: "call-1".to_owned(),
            caller_symbol_id: "caller-symbol".to_owned(),
            callee_text: "helper".to_owned(),
            callee_symbol_id: Some("callee-symbol".to_owned()),
            call_line: 4,
            confidence: 1.0,
            resolution_status: "resolved_exact".to_owned(),
            index_run_id: "run".to_owned(),
            parser_version: "parser".to_owned(),
        }];
        store
            .replace_file_facts(
                &sample_file("hash-1"),
                &symbols,
                &[sample_chunk("chunk-1")],
                &calls,
            )
            .expect("facts should persist");

        let status = store.repository_status("repo").expect("status should load");
        assert_eq!(status.symbols_indexed, 2);
        assert_eq!(status.calls_indexed, 1);

        let found = store.find_symbols("repo", "helper").expect("symbol search");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].qualified_name, "helper");
        assert_eq!(found[0].provenance.content_hash.as_deref(), Some("hash-1"));
        assert_eq!(found[0].provenance.index_run_id.as_deref(), Some("run"));

        let callers = store.callers("repo", "helper").expect("callers query");
        assert_eq!(callers.len(), 1);
        assert_eq!(callers[0].symbol_qualified_name.as_deref(), Some("caller"));
        assert_eq!(
            callers[0].provenance.parser_version.as_deref(),
            Some("parser")
        );

        let callees = store.callees("repo", "caller").expect("callees query");
        assert_eq!(callees.len(), 1);
        assert_eq!(callees[0].symbol_qualified_name.as_deref(), Some("helper"));
    }

    #[test]
    fn sqlite_traces_bounded_call_paths_deterministically() {
        let db = TestDb::new("call-paths");
        let mut store = SqliteStore::open(&db.config()).expect("store should open");
        store.migrate().expect("migration should run");
        store
            .upsert_repository(&RepositoryRecord {
                id: "repo".to_owned(),
                root_path: "/tmp/repo".to_owned(),
            })
            .expect("repository should persist");

        let symbols = vec![
            sample_symbol("a-symbol", "a", "crate::a"),
            sample_symbol("b-symbol", "b", "crate::b"),
            sample_symbol("c-symbol", "c", "crate::c"),
        ];
        let calls = vec![
            sample_call("call-a-b", "a-symbol", "b", Some("b-symbol"), 10),
            sample_call("call-b-c", "b-symbol", "c", Some("c-symbol"), 20),
            sample_call("call-a-c", "a-symbol", "c", Some("c-symbol"), 30),
            sample_call("call-c-a", "c-symbol", "a", Some("a-symbol"), 40),
            unresolved_call("call-a-missing", "a-symbol", "missing", 50),
        ];
        store
            .replace_file_facts(&sample_file("hash-1"), &symbols, &[], &calls)
            .expect("calls should persist");

        let shallow = store
            .call_paths("repo", "crate::a", "crate::c", 1)
            .expect("shallow paths should trace");
        assert_eq!(shallow.len(), 1);
        assert_eq!(shallow[0].hops, 1);
        assert_eq!(shallow[0].edges[0].call_id, "call-a-c");

        let paths = store
            .call_paths("repo", "crate::a", "crate::c", 2)
            .expect("paths should trace");
        assert_eq!(paths.len(), 2);
        assert_eq!(paths[0].hops, 2);
        assert_eq!(paths[0].edges[0].call_id, "call-a-b");
        assert_eq!(paths[0].edges[1].call_id, "call-b-c");
        assert_eq!(paths[1].hops, 1);
        assert_eq!(paths[1].edges[0].call_id, "call-a-c");

        let cyclic = store
            .call_paths("repo", "crate::a", "crate::c", 8)
            .expect("cycle-safe paths should trace");
        assert_eq!(cyclic.len(), 2);
    }

    #[test]
    fn sqlite_traces_unresolved_terminal_call_path_by_callee_text() {
        let db = TestDb::new("call-paths-unresolved");
        let mut store = SqliteStore::open(&db.config()).expect("store should open");
        store.migrate().expect("migration should run");
        store
            .upsert_repository(&RepositoryRecord {
                id: "repo".to_owned(),
                root_path: "/tmp/repo".to_owned(),
            })
            .expect("repository should persist");

        let symbols = vec![sample_symbol("a-symbol", "a", "crate::a")];
        let calls = vec![unresolved_call("call-a-missing", "a-symbol", "missing", 10)];
        store
            .replace_file_facts(&sample_file("hash-1"), &symbols, &[], &calls)
            .expect("calls should persist");

        let paths = store
            .call_paths("repo", "crate::a", "missing", 2)
            .expect("unresolved terminal path should trace");

        assert_eq!(paths.len(), 1);
        assert_eq!(paths[0].hops, 1);
        assert_eq!(paths[0].terminal_resolution_status, "unresolved");
        assert_eq!(paths[0].edges[0].callee_symbol_id, None);
        assert_eq!(paths[0].edges[0].callee_text, "missing");
    }

    #[test]
    fn sqlite_traces_ambiguous_terminal_call_path_by_callee_text() {
        let db = TestDb::new("call-paths-ambiguous");
        let mut store = SqliteStore::open(&db.config()).expect("store should open");
        store.migrate().expect("migration should run");
        store
            .upsert_repository(&RepositoryRecord {
                id: "repo".to_owned(),
                root_path: "/tmp/repo".to_owned(),
            })
            .expect("repository should persist");

        let symbols = vec![sample_symbol("a-symbol", "a", "crate::a")];
        let mut ambiguous = unresolved_call("call-a-helper", "a-symbol", "helper", 10);
        ambiguous.resolution_status = "ambiguous".to_owned();
        ambiguous.confidence = 0.5;
        store
            .replace_file_facts(&sample_file("hash-1"), &symbols, &[], &[ambiguous])
            .expect("calls should persist");

        let paths = store
            .call_paths("repo", "crate::a", "helper", 2)
            .expect("ambiguous terminal path should trace");

        assert_eq!(paths.len(), 1);
        assert_eq!(paths[0].hops, 1);
        assert_eq!(paths[0].terminal_resolution_status, "ambiguous");
        assert_eq!(paths[0].edges[0].callee_symbol_id, None);
        assert_eq!(paths[0].edges[0].callee_text, "helper");
    }

    #[test]
    fn sqlite_reports_transitive_impact_paths_deterministically() {
        let db = TestDb::new("transitive-impact-paths");
        let mut store = SqliteStore::open(&db.config()).expect("store should open");
        store.migrate().expect("migration should run");
        store
            .upsert_repository(&RepositoryRecord {
                id: "repo".to_owned(),
                root_path: "/tmp/repo".to_owned(),
            })
            .expect("repository should persist");

        let symbols = vec![
            sample_symbol("a-symbol", "a", "crate::a"),
            sample_symbol("b-symbol", "b", "crate::b"),
            sample_symbol("c-symbol", "c", "crate::c"),
            sample_symbol("d-symbol", "d", "crate::d"),
        ];
        let calls = vec![
            sample_call("call-a-b", "a-symbol", "b", Some("b-symbol"), 10),
            sample_call("call-b-c", "b-symbol", "c", Some("c-symbol"), 20),
            sample_call("call-c-d", "c-symbol", "d", Some("d-symbol"), 30),
        ];
        store
            .replace_file_facts(&sample_file("hash-1"), &symbols, &[], &calls)
            .expect("calls should persist");

        let callers = store
            .transitive_call_paths_to("repo", "crate::c", 3)
            .expect("transitive callers should trace");
        assert_eq!(callers.len(), 1);
        assert_eq!(callers[0].hops, 2);
        assert_eq!(callers[0].edges[0].call_id, "call-a-b");
        assert_eq!(callers[0].edges[1].call_id, "call-b-c");

        let callees = store
            .transitive_call_paths_from("repo", "crate::a", 3)
            .expect("transitive callees should trace");
        assert_eq!(callees.len(), 2);
        assert_eq!(callees[0].hops, 2);
        assert_eq!(callees[0].edges[1].call_id, "call-b-c");
        assert_eq!(callees[1].hops, 3);
        assert_eq!(callees[1].edges[2].call_id, "call-c-d");
    }

    #[test]
    fn sqlite_builds_compact_context_pack() {
        let db = TestDb::new("context-pack");
        let mut store = SqliteStore::open(&db.config()).expect("store should open");
        store.migrate().expect("migration should run");
        store
            .upsert_repository(&RepositoryRecord {
                id: "repo".to_owned(),
                root_path: "/tmp/repo".to_owned(),
            })
            .expect("repository should persist");

        let symbols = vec![
            sample_symbol("caller-symbol", "caller", "caller"),
            sample_symbol("callee-symbol", "helper", "helper"),
        ];
        let calls = vec![CallRecord {
            id: "call-1".to_owned(),
            caller_symbol_id: "caller-symbol".to_owned(),
            callee_text: "helper".to_owned(),
            callee_symbol_id: Some("callee-symbol".to_owned()),
            call_line: 4,
            confidence: 1.0,
            resolution_status: "resolved_exact".to_owned(),
            index_run_id: "run".to_owned(),
            parser_version: "parser".to_owned(),
        }];
        store
            .replace_file_facts(
                &sample_file("hash-1"),
                &symbols,
                &[sample_chunk("chunk-1")],
                &calls,
            )
            .expect("facts should persist");

        let pack = store
            .context_pack("repo", "helper", 5)
            .expect("context pack should build");

        assert_eq!(pack.format, "symdex.context_pack.v1");
        assert_eq!(pack.focus_symbols.len(), 1);
        assert_eq!(pack.direct_callers.len(), 1);
        assert!(pack.direct_callees.is_empty());
        assert_eq!(pack.files, vec!["src/lib.rs"]);
        let json = serde_json::to_value(&pack).expect("pack should serialize");
        assert_eq!(json["notes"][0], "metadata_only_no_source_text");
        assert!(json.get("source_text").is_none());
    }

    #[test]
    fn sqlite_replaces_calls_without_unique_conflict() {
        let db = TestDb::new("replace-calls");
        let mut store = SqliteStore::open(&db.config()).expect("store should open");
        store.migrate().expect("migration should run");
        store
            .upsert_repository(&RepositoryRecord {
                id: "repo".to_owned(),
                root_path: "/tmp/repo".to_owned(),
            })
            .expect("repository should persist");

        let symbols = vec![
            sample_symbol("caller-symbol", "caller", "caller"),
            sample_symbol("callee-symbol", "helper", "helper"),
        ];
        store
            .replace_file_facts(
                &sample_file("hash-1"),
                &symbols,
                &[sample_chunk("chunk-1")],
                &[CallRecord {
                    id: "call-1".to_owned(),
                    caller_symbol_id: "caller-symbol".to_owned(),
                    callee_text: "helper".to_owned(),
                    callee_symbol_id: Some("callee-symbol".to_owned()),
                    call_line: 4,
                    confidence: 1.0,
                    resolution_status: "resolved_exact".to_owned(),
                    index_run_id: "run".to_owned(),
                    parser_version: "parser".to_owned(),
                }],
            )
            .expect("initial calls should persist");

        store
            .replace_file_facts(
                &sample_file("hash-2"),
                &symbols,
                &[sample_chunk("chunk-2")],
                &[CallRecord {
                    id: "call-1".to_owned(),
                    caller_symbol_id: "caller-symbol".to_owned(),
                    callee_text: "helper_updated".to_owned(),
                    callee_symbol_id: Some("callee-symbol".to_owned()),
                    call_line: 5,
                    confidence: 0.9,
                    resolution_status: "resolved_local_candidate".to_owned(),
                    index_run_id: "run".to_owned(),
                    parser_version: "parser".to_owned(),
                }],
            )
            .expect("replacement calls should persist");

        let status = store.repository_status("repo").expect("status should load");
        assert_eq!(status.calls_indexed, 1);

        let callees = store.callees("repo", "caller").expect("callees query");
        assert_eq!(callees.len(), 1);
        assert_eq!(callees[0].callee_text, "helper_updated");
        assert_eq!(callees[0].call_line, 5);
    }

    #[test]
    fn sqlite_records_embedding_runs_and_rejects_dimension_changes() {
        let db = TestDb::new("embedding-runs");
        let store = SqliteStore::open(&db.config()).expect("store should open");
        store.migrate().expect("migration should run");
        store
            .upsert_repository(&RepositoryRecord {
                id: "repo".to_owned(),
                root_path: "/tmp/repo".to_owned(),
            })
            .expect("repository should persist");

        store
            .record_index_run(&sample_index_run("nomic-embed-text", 768))
            .expect("index run should persist");

        let status = store.repository_status("repo").expect("status should load");
        assert_eq!(status.embedding_model.as_deref(), Some("nomic-embed-text"));
        assert_eq!(status.embedding_dimension, Some(768));
        store
            .ensure_embedding_compatible("repo", "nomic-embed-text", 768)
            .expect("same dimension should be compatible");
        let error = store
            .ensure_embedding_compatible("repo", "nomic-embed-text", 1024)
            .expect_err("changed dimension should be rejected");
        assert!(matches!(
            error,
            StoreError::EmbeddingDimensionChanged {
                previous_dimension: 768,
                current_dimension: 1024,
                ..
            }
        ));
        store
            .ensure_embedding_compatible("repo", "different-model", 1024)
            .expect("different model writes to a different collection");
    }

    #[test]
    fn sqlite_builds_storage_explorer_summary_without_source_text() {
        let db = TestDb::new("storage-explorer");
        let mut store = SqliteStore::open(&db.config()).expect("store should open");
        store.migrate().expect("migration should run");
        store
            .upsert_repository(&RepositoryRecord {
                id: "repo".to_owned(),
                root_path: "/tmp/repo".to_owned(),
            })
            .expect("repository should persist");

        let mut missing_vector = sample_chunk("chunk-missing-vector");
        missing_vector.qdrant_point_id = None;
        let mut excluded = sample_chunk("chunk-excluded");
        excluded.qdrant_point_id = None;
        excluded.excluded_reason = Some("secret_detected".to_owned());
        store
            .replace_file_facts(
                &sample_file("hash-1"),
                &[sample_symbol("symbol", "add", "crate::add")],
                &[sample_chunk("chunk-vector"), missing_vector, excluded],
                &[],
            )
            .expect("facts should persist");
        store
            .record_index_run(&sample_index_run("nomic-embed-text", 768))
            .expect("index run should persist");

        let summary = store
            .storage_explorer_summary("repo", "nomic-embed-text")
            .expect("storage summary should build");

        assert_eq!(summary.sqlite.repositories, 1);
        assert_eq!(summary.sqlite.files, 1);
        assert_eq!(summary.sqlite.chunks, 3);
        assert_eq!(summary.sqlite.symbols, 1);
        assert_eq!(summary.sqlite.index_runs, 1);
        assert_eq!(summary.qdrant.embedding_model, "nomic-embed-text");
        assert_eq!(summary.qdrant.embedding_dimension, Some(768));
        assert_eq!(summary.qdrant.embeddable_chunks, 2);
        assert_eq!(summary.qdrant.vector_backed_chunks, 1);
        assert_eq!(summary.qdrant.excluded_chunks, 1);
        assert_eq!(summary.qdrant.missing_vector_chunks, 1);
        assert!(summary.qdrant.collection_name.starts_with("symdex_repo_"));
        assert!(summary.warnings.iter().any(
            |row| row.status == StorageHealthStatus::Warning && row.label == "missing_vectors"
        ));
        let debug = format!("{summary:?}");
        assert!(!debug.contains("source_text"));
    }

    #[test]
    fn sqlite_builds_embedding_coverage_summary() {
        let db = TestDb::new("embedding-coverage");
        let mut store = SqliteStore::open(&db.config()).expect("store should open");
        store.migrate().expect("migration should run");
        store
            .upsert_repository(&RepositoryRecord {
                id: "repo".to_owned(),
                root_path: "/tmp/repo".to_owned(),
            })
            .expect("repository should persist");

        store
            .replace_file_facts(
                &sample_file("hash-1"),
                &[sample_symbol("symbol", "add", "crate::add")],
                &[
                    sample_chunk("chunk-vector"),
                    missing_vector_chunk("chunk-missing"),
                    excluded_chunk("chunk-secret", "file"),
                ],
                &[],
            )
            .expect("facts should persist");
        store
            .record_index_run(&sample_index_run("nomic-embed-text", 768))
            .expect("index run should persist");

        let summary = store
            .embedding_coverage_summary("repo", "nomic-embed-text")
            .expect("embedding coverage should load");

        assert_eq!(summary.repository_id, "repo");
        assert_eq!(summary.configured_embedding_model, "nomic-embed-text");
        assert_eq!(summary.embedding_model, "nomic-embed-text");
        assert_eq!(summary.embedding_dimension, Some(768));
        assert_eq!(summary.total_chunks, 3);
        assert_eq!(summary.embeddable_chunks, 2);
        assert_eq!(summary.vector_backed_chunks, 1);
        assert_eq!(summary.missing_vector_chunks, 1);
        assert_eq!(summary.excluded_chunks, 1);
        assert_eq!(summary.latest_chunks_embedded, Some(1));
        assert_eq!(summary.exclusion_reasons.len(), 1);
        assert_eq!(summary.exclusion_reasons[0].reason, "secret_detected");
        assert_eq!(summary.exclusion_reasons[0].chunks, 1);
        assert!(summary.collection_name.starts_with("symdex_repo_"));
        assert!(summary.health.iter().any(
            |row| row.status == StorageHealthStatus::Warning && row.label == "missing_vectors"
        ));
        let debug = format!("{summary:?}");
        assert!(!debug.contains("source_text"));
    }

    #[test]
    fn sqlite_builds_index_runs_timeline_summary() {
        let db = TestDb::new("index-runs-timeline");
        let store = SqliteStore::open(&db.config()).expect("store should open");
        store.migrate().expect("migration should run");
        store
            .upsert_repository(&RepositoryRecord {
                id: "repo".to_owned(),
                root_path: "/tmp/repo".to_owned(),
            })
            .expect("repository should persist");
        insert_index_run_fixture(
            &store,
            IndexRunFixture {
                id: "run-old",
                started_at: "2026-01-01T00:00:00Z",
                finished_at: Some("2026-01-01T00:00:10Z"),
                status: "success",
                model: "nomic-embed-text",
                dimension: Some(768),
                files_seen: 4,
                files_indexed: 3,
                chunks_embedded: 7,
                error_summary: None,
            },
        );
        insert_index_run_fixture(
            &store,
            IndexRunFixture {
                id: "run-new",
                started_at: "2026-01-02T00:00:00Z",
                finished_at: Some("2026-01-02T00:00:04Z"),
                status: "failed",
                model: "nomic-embed-text",
                dimension: Some(768),
                files_seen: 5,
                files_indexed: 2,
                chunks_embedded: 1,
                error_summary: Some("qdrant unavailable"),
            },
        );

        let summary = store
            .index_runs_timeline_summary("repo")
            .expect("timeline summary should load");

        assert_eq!(summary.repository_id, "repo");
        assert_eq!(summary.runs.len(), 2);
        assert_eq!(summary.runs[0].id, "run-new");
        assert_eq!(summary.runs[0].status, "failed");
        assert_eq!(summary.runs[0].files_seen, 5);
        assert_eq!(summary.runs[0].files_indexed, 2);
        assert_eq!(summary.runs[0].chunks_embedded, 1);
        assert_eq!(
            summary.runs[0].error_summary.as_deref(),
            Some("qdrant unavailable")
        );
        assert_eq!(summary.runs[1].id, "run-old");
        let debug = format!("{summary:?}");
        assert!(!debug.contains("source_text"));
    }

    #[test]
    fn sqlite_tracks_started_and_finished_index_runs() {
        let db = TestDb::new("index-run-lifecycle");
        let store = SqliteStore::open(&db.config()).expect("store should open");
        store.migrate().expect("migration should run");
        store
            .upsert_repository(&RepositoryRecord {
                id: "repo".to_owned(),
                root_path: "/tmp/repo".to_owned(),
            })
            .expect("repository should persist");

        let mut run = sample_index_run("nomic-embed-text", 768);
        run.id = "run-lifecycle".to_owned();
        run.status = "running".to_owned();
        run.files_seen = 0;
        run.files_indexed = 0;
        run.chunks_embedded = 0;
        store
            .start_index_run(&run)
            .expect("started run should persist");

        let started: (String, Option<String>, i64, i64) = store
            .connection
            .query_row(
                "SELECT status, finished_at, files_seen, chunks_embedded
                 FROM index_runs
                 WHERE id = 'run-lifecycle'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .expect("started run should load");
        assert_eq!(started, ("running".to_owned(), None, 0, 0));

        run.status = "partial".to_owned();
        run.files_seen = 4;
        run.files_indexed = 3;
        run.error_summary = Some("qdrant unavailable".to_owned());
        store
            .finish_index_run(&run)
            .expect("finished run should persist");

        let finished: (String, Option<String>, i64, i64, Option<String>) = store
            .connection
            .query_row(
                "SELECT status, finished_at, files_seen, files_indexed, error_summary
                 FROM index_runs
                 WHERE id = 'run-lifecycle'",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .expect("finished run should load");
        assert_eq!(finished.0, "partial");
        assert!(finished.1.is_some());
        assert_eq!(finished.2, 4);
        assert_eq!(finished.3, 3);
        assert_eq!(finished.4.as_deref(), Some("qdrant unavailable"));
    }

    #[test]
    fn sqlite_builds_semantic_neighborhood_summary_without_source_text() {
        let db = TestDb::new("semantic-neighborhood");
        let mut store = SqliteStore::open(&db.config()).expect("store should open");
        store.migrate().expect("migration should run");
        store
            .upsert_repository(&RepositoryRecord {
                id: "repo".to_owned(),
                root_path: "/tmp/repo".to_owned(),
            })
            .expect("repository should persist");

        let mut vector = sample_chunk("chunk-vector");
        vector.text_hash = "hash-vector".to_owned();
        let mut missing = missing_vector_chunk("chunk-missing");
        missing.text_hash = "hash-missing".to_owned();
        store
            .replace_file_facts(
                &sample_file("hash-1"),
                &[sample_symbol("symbol", "add", "crate::add")],
                &[vector, missing],
                &[],
            )
            .expect("facts should persist");
        store
            .record_index_run(&sample_index_run("nomic-embed-text", 768))
            .expect("index run should persist");

        let summary = store
            .semantic_neighborhood_summary("repo", "nomic-embed-text")
            .expect("semantic neighborhood should load");

        assert_eq!(summary.repository_id, "repo");
        assert_eq!(summary.embedding_model, "nomic-embed-text");
        assert_eq!(summary.rows.len(), 1);
        assert_eq!(summary.rows[0].path, "src/lib.rs");
        assert_eq!(summary.rows[0].symbol_name.as_deref(), Some("crate::add"));
        assert_eq!(summary.rows[0].chunk_kind, "function");
        assert_eq!(summary.rows[0].language, "rust");
        assert_eq!(summary.rows[0].score, None);
        assert_eq!(summary.rows[0].text_hash, "hash-vector");
        assert!(
            summary
                .health
                .iter()
                .any(|row| row.status == StorageHealthStatus::Ok && row.label == "metadata_only")
        );
        let debug = format!("{summary:?}");
        assert!(!debug.contains("source_text"));
    }

    #[test]
    fn sqlite_builds_cross_store_health_warnings() {
        let db = TestDb::new("cross-store-health");
        let mut store = SqliteStore::open(&db.config()).expect("store should open");
        store.migrate().expect("migration should run");
        store
            .upsert_repository(&RepositoryRecord {
                id: "repo".to_owned(),
                root_path: "/tmp/repo".to_owned(),
            })
            .expect("repository should persist");

        store
            .replace_file_facts(
                &sample_file("hash-1"),
                &[sample_symbol("symbol", "add", "crate::add")],
                &[
                    sample_chunk("chunk-vector"),
                    missing_vector_chunk("chunk-missing"),
                    excluded_chunk("chunk-secret", "file"),
                ],
                &[],
            )
            .expect("facts should persist");
        insert_index_run_fixture(
            &store,
            IndexRunFixture {
                id: "run-drift-768",
                started_at: "2026-01-01T00:00:00Z",
                finished_at: Some("2026-01-01T00:00:10Z"),
                status: "success",
                model: "different-model",
                dimension: Some(768),
                files_seen: 1,
                files_indexed: 1,
                chunks_embedded: 1,
                error_summary: None,
            },
        );
        insert_index_run_fixture(
            &store,
            IndexRunFixture {
                id: "run-drift-1024",
                started_at: "2026-01-02T00:00:00Z",
                finished_at: Some("2026-01-02T00:00:10Z"),
                status: "success",
                model: "different-model",
                dimension: Some(1024),
                files_seen: 1,
                files_indexed: 1,
                chunks_embedded: 1,
                error_summary: None,
            },
        );

        let summary = store
            .cross_store_health_summary("repo", "nomic-embed-text")
            .expect("health summary should load");

        assert_eq!(summary.repository_id, "repo");
        assert!(summary.rows.iter().any(
            |row| row.status == StorageHealthStatus::Warning && row.label == "missing_vectors"
        ));
        assert!(summary.rows.iter().any(
            |row| row.status == StorageHealthStatus::Warning && row.label == "excluded_chunks"
        ));
        assert!(
            summary
                .rows
                .iter()
                .any(|row| row.status == StorageHealthStatus::Error && row.label == "model_drift")
        );
        assert!(
            summary
                .rows
                .iter()
                .any(|row| row.status == StorageHealthStatus::Error
                    && row.label == "dimension_drift")
        );
        let debug = format!("{summary:?}");
        assert!(!debug.contains("source_text"));
    }

    #[test]
    fn sqlite_flags_missing_collection_health() {
        let db = TestDb::new("missing-collection-health");
        let mut store = SqliteStore::open(&db.config()).expect("store should open");
        store.migrate().expect("migration should run");
        store
            .upsert_repository(&RepositoryRecord {
                id: "repo".to_owned(),
                root_path: "/tmp/repo".to_owned(),
            })
            .expect("repository should persist");
        store
            .replace_file_facts(
                &sample_file("hash-1"),
                &[],
                &[missing_vector_chunk("chunk-missing")],
                &[],
            )
            .expect("facts should persist");

        let summary = store
            .cross_store_health_summary("repo", "nomic-embed-text")
            .expect("health summary should load");

        assert!(summary.rows.iter().any(
            |row| row.status == StorageHealthStatus::Error && row.label == "missing_collection"
        ));
    }

    #[test]
    fn sqlite_builds_file_index_coverage_summary() {
        let db = TestDb::new("index-coverage");
        let mut store = SqliteStore::open(&db.config()).expect("store should open");
        store.migrate().expect("migration should run");
        store
            .upsert_repository(&RepositoryRecord {
                id: "repo".to_owned(),
                root_path: "/tmp/repo".to_owned(),
            })
            .expect("repository should persist");

        let symbols = vec![
            sample_symbol("caller-symbol", "caller", "crate::caller"),
            sample_symbol("callee-symbol", "helper", "crate::helper"),
        ];
        store
            .replace_file_facts(
                &sample_file("hash-1"),
                &symbols,
                &[
                    sample_chunk("chunk-vector"),
                    missing_vector_chunk("chunk-missing"),
                ],
                &[CallRecord {
                    id: "call-1".to_owned(),
                    caller_symbol_id: "caller-symbol".to_owned(),
                    callee_text: "helper".to_owned(),
                    callee_symbol_id: Some("callee-symbol".to_owned()),
                    call_line: 4,
                    confidence: 1.0,
                    resolution_status: "resolved_exact".to_owned(),
                    index_run_id: "run".to_owned(),
                    parser_version: "parser".to_owned(),
                }],
            )
            .expect("first file facts should persist");

        let second_file = FileRecord {
            id: "file-secret".to_owned(),
            repository_id: "repo".to_owned(),
            path: "src/secret.rs".to_owned(),
            language: "rust".to_owned(),
            content_hash: "hash-secret".to_owned(),
            index_run_id: "run".to_owned(),
            parser_version: "parser".to_owned(),
        };
        store
            .replace_file_facts(
                &second_file,
                &[],
                &[excluded_chunk("secret-chunk", "file-secret")],
                &[],
            )
            .expect("second file facts should persist");

        let coverage = store
            .index_coverage_summary("repo")
            .expect("coverage should load");

        assert_eq!(coverage.repository_id, "repo");
        assert_eq!(coverage.files.len(), 2);
        let lib = coverage
            .files
            .iter()
            .find(|file| file.path == "src/lib.rs")
            .expect("lib coverage row");
        assert_eq!(lib.language, "rust");
        assert_eq!(lib.chunks, 2);
        assert_eq!(lib.symbols, 2);
        assert_eq!(lib.calls, 1);
        assert_eq!(lib.embeddable_chunks, 2);
        assert_eq!(lib.vector_backed_chunks, 1);
        assert_eq!(lib.excluded_chunks, 0);
        assert_eq!(lib.status, FileCoverageStatus::MissingVector);
        assert_eq!(lib.detail.chunks.len(), 2);
        assert!(
            lib.detail
                .chunks
                .iter()
                .any(|chunk| chunk.vector_status == ChunkVectorStatus::MissingVector)
        );
        assert_eq!(lib.detail.symbols.len(), 2);
        assert_eq!(lib.detail.calls.len(), 1);
        assert_eq!(lib.detail.calls[0].callee_text, "helper");
        assert_eq!(lib.detail.calls[0].resolution_status, "resolved_exact");

        let secret = coverage
            .files
            .iter()
            .find(|file| file.path == "src/secret.rs")
            .expect("secret coverage row");
        assert_eq!(secret.chunks, 1);
        assert_eq!(secret.embeddable_chunks, 0);
        assert_eq!(secret.excluded_chunks, 1);
        assert_eq!(secret.status, FileCoverageStatus::Excluded);
        assert_eq!(
            secret.detail.chunks[0].vector_status,
            ChunkVectorStatus::Excluded
        );
        assert_eq!(
            secret.detail.chunks[0].excluded_reason.as_deref(),
            Some("secret_detected")
        );
    }

    #[test]
    fn sqlite_builds_symbol_outline_with_parent_depths() {
        let db = TestDb::new("symbol-outline");
        let mut store = SqliteStore::open(&db.config()).expect("store should open");
        store.migrate().expect("migration should run");
        store
            .upsert_repository(&RepositoryRecord {
                id: "repo".to_owned(),
                root_path: "/tmp/repo".to_owned(),
            })
            .expect("repository should persist");

        let mut parent = sample_symbol("parent-symbol", "Service", "crate::Service");
        parent.kind = "struct".to_owned();
        let mut child = sample_symbol("child-symbol", "run", "crate::Service::run");
        child.parent_symbol_id = Some("parent-symbol".to_owned());
        child.start_line = 5;
        child.end_line = 8;
        let mut grandchild = sample_symbol("grandchild-symbol", "inner", "crate::Service::inner");
        grandchild.parent_symbol_id = Some("child-symbol".to_owned());
        grandchild.start_line = 6;
        grandchild.end_line = 7;

        store
            .replace_file_facts(
                &sample_file("hash-1"),
                &[parent, child, grandchild],
                &[],
                &[],
            )
            .expect("symbols should persist");

        let outline = store
            .symbol_outline_summary("repo")
            .expect("outline should load");

        assert_eq!(outline.repository_id, "repo");
        assert_eq!(outline.symbols.len(), 3);
        let parent = outline
            .symbols
            .iter()
            .find(|symbol| symbol.id == "parent-symbol")
            .expect("parent symbol row");
        assert_eq!(parent.depth, 0);
        assert_eq!(parent.child_count, 1);
        assert_eq!(parent.kind, "struct");

        let child = outline
            .symbols
            .iter()
            .find(|symbol| symbol.id == "child-symbol")
            .expect("child symbol row");
        assert_eq!(child.parent_symbol_id.as_deref(), Some("parent-symbol"));
        assert_eq!(child.depth, 1);
        assert_eq!(child.child_count, 1);

        let grandchild = outline
            .symbols
            .iter()
            .find(|symbol| symbol.id == "grandchild-symbol")
            .expect("grandchild symbol row");
        assert_eq!(grandchild.depth, 2);
    }

    #[test]
    fn sqlite_builds_call_resolution_summary_by_status_and_confidence() {
        let db = TestDb::new("call-resolution");
        let mut store = SqliteStore::open(&db.config()).expect("store should open");
        store.migrate().expect("migration should run");
        store
            .upsert_repository(&RepositoryRecord {
                id: "repo".to_owned(),
                root_path: "/tmp/repo".to_owned(),
            })
            .expect("repository should persist");

        let symbols = vec![
            sample_symbol("caller-symbol", "caller", "crate::caller"),
            sample_symbol("callee-symbol", "helper", "crate::helper"),
        ];
        let calls = vec![
            CallRecord {
                id: "call-high".to_owned(),
                caller_symbol_id: "caller-symbol".to_owned(),
                callee_text: "helper".to_owned(),
                callee_symbol_id: Some("callee-symbol".to_owned()),
                call_line: 4,
                confidence: 1.0,
                resolution_status: "resolved_exact".to_owned(),
                index_run_id: "run".to_owned(),
                parser_version: "parser".to_owned(),
            },
            CallRecord {
                id: "call-low".to_owned(),
                caller_symbol_id: "caller-symbol".to_owned(),
                callee_text: "missing".to_owned(),
                callee_symbol_id: None,
                call_line: 7,
                confidence: 0.25,
                resolution_status: "unresolved".to_owned(),
                index_run_id: "run".to_owned(),
                parser_version: "parser".to_owned(),
            },
        ];
        store
            .replace_file_facts(&sample_file("hash-1"), &symbols, &[], &calls)
            .expect("calls should persist");

        let summary = store
            .call_resolution_summary("repo")
            .expect("call resolution summary should load");

        assert_eq!(summary.repository_id, "repo");
        assert_eq!(summary.buckets.len(), 2);
        let resolved = summary
            .buckets
            .iter()
            .find(|bucket| bucket.resolution_status == "resolved_exact")
            .expect("resolved bucket");
        assert_eq!(resolved.confidence_bucket, ConfidenceBucket::High);
        assert_eq!(resolved.call_count, 1);
        assert_eq!(resolved.rows[0].caller_symbol, "crate::caller");
        assert_eq!(resolved.rows[0].callee_text, "helper");

        let unresolved = summary
            .buckets
            .iter()
            .find(|bucket| bucket.resolution_status == "unresolved")
            .expect("unresolved bucket");
        assert_eq!(unresolved.confidence_bucket, ConfidenceBucket::Low);
        assert_eq!(unresolved.call_count, 1);
        assert_eq!(unresolved.rows[0].call_line, 7);
    }

    #[test]
    fn live_qdrant_health_is_opt_in() {
        if std::env::var("SYMDEX_TEST_QDRANT").ok().as_deref() != Some("1") {
            return;
        }

        let client = QdrantClient::new(&StoreConfig::from_env()).expect("client should build");
        client.health_check().expect("qdrant should be reachable");
    }

    fn sample_payload() -> PointPayload {
        PointPayload {
            repository_id: "repo".to_owned(),
            file_id: "file".to_owned(),
            chunk_id: "chunk".to_owned(),
            symbol_id: Some("symbol".to_owned()),
            symbol_name: Some("add".to_owned()),
            path: "src/lib.rs".to_owned(),
            language: "rust".to_owned(),
            chunk_kind: "function".to_owned(),
            start_line: 1,
            end_line: 3,
            text_hash: "hash".to_owned(),
            parser_version: Some("parser".to_owned()),
            content_hash: Some("content-hash".to_owned()),
            index_run_id: Some("run".to_owned()),
            embedding_model: Some("nomic-embed-text".to_owned()),
            embedding_dimension: Some(768),
            indexed_at: Some("123".to_owned()),
        }
    }

    fn sample_file(content_hash: &str) -> FileRecord {
        FileRecord {
            id: "file".to_owned(),
            repository_id: "repo".to_owned(),
            path: "src/lib.rs".to_owned(),
            language: "rust".to_owned(),
            content_hash: content_hash.to_owned(),
            index_run_id: "run".to_owned(),
            parser_version: "parser".to_owned(),
        }
    }

    fn sample_chunk(id: &str) -> ChunkRecord {
        ChunkRecord {
            id: id.to_owned(),
            file_id: "file".to_owned(),
            symbol_id: Some("symbol".to_owned()),
            kind: "function".to_owned(),
            text_hash: format!("text-{id}"),
            start_line: 1,
            end_line: 3,
            start_byte: 0,
            end_byte: 32,
            qdrant_point_id: Some("01234567-89ab-cdef-fedc-ba9876543210".to_owned()),
            excluded_reason: None,
            index_run_id: "run".to_owned(),
            parser_version: "parser".to_owned(),
            embedding_model: None,
            embedding_dimension: None,
            embedded_at: None,
        }
    }

    fn missing_vector_chunk(id: &str) -> ChunkRecord {
        let mut chunk = sample_chunk(id);
        chunk.qdrant_point_id = None;
        chunk
    }

    fn excluded_chunk(id: &str, file_id: &str) -> ChunkRecord {
        ChunkRecord {
            id: id.to_owned(),
            file_id: file_id.to_owned(),
            symbol_id: None,
            kind: "function".to_owned(),
            text_hash: format!("text-{id}"),
            start_line: 1,
            end_line: 3,
            start_byte: 0,
            end_byte: 32,
            qdrant_point_id: None,
            excluded_reason: Some("secret_detected".to_owned()),
            index_run_id: "run".to_owned(),
            parser_version: "parser".to_owned(),
            embedding_model: None,
            embedding_dimension: None,
            embedded_at: None,
        }
    }

    fn sample_symbol(id: &str, name: &str, qualified_name: &str) -> SymbolRecord {
        SymbolRecord {
            id: id.to_owned(),
            file_id: "file".to_owned(),
            parent_symbol_id: None,
            name: name.to_owned(),
            qualified_name: qualified_name.to_owned(),
            kind: "function".to_owned(),
            signature: Some(format!("fn {name}()")),
            start_line: 1,
            end_line: 3,
            start_byte: 0,
            end_byte: 32,
            index_run_id: "run".to_owned(),
            parser_version: "parser".to_owned(),
        }
    }

    fn sample_call(
        id: &str,
        caller_symbol_id: &str,
        callee_text: &str,
        callee_symbol_id: Option<&str>,
        call_line: usize,
    ) -> CallRecord {
        CallRecord {
            id: id.to_owned(),
            caller_symbol_id: caller_symbol_id.to_owned(),
            callee_text: callee_text.to_owned(),
            callee_symbol_id: callee_symbol_id.map(str::to_owned),
            call_line,
            confidence: 1.0,
            resolution_status: "resolved_exact".to_owned(),
            index_run_id: "run".to_owned(),
            parser_version: "parser".to_owned(),
        }
    }

    fn unresolved_call(
        id: &str,
        caller_symbol_id: &str,
        callee_text: &str,
        call_line: usize,
    ) -> CallRecord {
        let mut call = sample_call(id, caller_symbol_id, callee_text, None, call_line);
        call.confidence = 0.25;
        call.resolution_status = "unresolved".to_owned();
        call
    }

    fn sample_index_run(model: &str, dimension: usize) -> crate::IndexRunRecord {
        crate::IndexRunRecord {
            id: format!("run-{model}-{dimension}"),
            repository_id: "repo".to_owned(),
            status: "success".to_owned(),
            embedding_model: model.to_owned(),
            embedding_dimension: Some(dimension),
            files_seen: 1,
            files_indexed: 1,
            chunks_embedded: 1,
            error_summary: None,
            parser_version: "parser".to_owned(),
            indexer_version: "indexer".to_owned(),
            run_kind: "semantic".to_owned(),
        }
    }

    struct IndexRunFixture {
        id: &'static str,
        started_at: &'static str,
        finished_at: Option<&'static str>,
        status: &'static str,
        model: &'static str,
        dimension: Option<usize>,
        files_seen: usize,
        files_indexed: usize,
        chunks_embedded: usize,
        error_summary: Option<&'static str>,
    }

    fn insert_index_run_fixture(store: &SqliteStore, fixture: IndexRunFixture) {
        store
            .connection
            .execute(
                "INSERT INTO index_runs (
                   id, repository_id, started_at, finished_at, status, embedding_model,
                   embedding_dimension, files_seen, files_indexed, chunks_embedded,
                   error_summary
                 )
                 VALUES (?1, 'repo', ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    fixture.id,
                    fixture.started_at,
                    fixture.finished_at,
                    fixture.status,
                    fixture.model,
                    fixture.dimension.map(|dimension| dimension as i64),
                    fixture.files_seen as i64,
                    fixture.files_indexed as i64,
                    fixture.chunks_embedded as i64,
                    fixture.error_summary,
                ],
            )
            .expect("index run fixture should insert");
    }

    fn sqlite_index_names(store: &SqliteStore) -> Vec<String> {
        let mut statement = store
            .connection
            .prepare(
                "SELECT name FROM sqlite_master
                 WHERE type = 'index'
                   AND name NOT LIKE 'sqlite_autoindex%'
                 ORDER BY name",
            )
            .expect("index query should prepare");
        let rows = statement
            .query_map([], |row| row.get::<_, String>(0))
            .expect("index query should run");
        rows.map(|row| row.expect("index row should decode"))
            .collect()
    }

    struct TestDb {
        dir: PathBuf,
    }

    impl TestDb {
        fn new(name: &str) -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock should be after epoch")
                .as_nanos();
            let dir = std::env::temp_dir().join(format!(
                "symdex-store-test-{name}-{}-{nonce}",
                std::process::id()
            ));
            fs::create_dir_all(&dir).expect("test db dir should exist");
            Self { dir }
        }

        fn config(&self) -> StoreConfig {
            StoreConfig {
                sqlite_path: self.dir.join("symdex.sqlite"),
                qdrant_url: "http://localhost:6333".to_owned(),
            }
        }
    }

    impl Drop for TestDb {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(Path::new(&self.dir));
        }
    }
}

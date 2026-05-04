use serde::{Deserialize, Serialize};
use std::time::Duration;
use zerocopy::IntoBytes;

use crate::{Result, StoreConfig, StoreError, collect_rows};

#[derive(Debug, Clone)]
pub struct SqliteVectorStore {
    sqlite_path: std::path::PathBuf,
}

impl SqliteVectorStore {
    pub fn new(config: &StoreConfig) -> Result<Self> {
        symdex_sqlite_vec::register_sqlite_vec().map_err(StoreError::SqliteVecRegistration)?;
        Ok(Self {
            sqlite_path: config.sqlite_path.clone(),
        })
    }

    pub fn health_check(&self) -> Result<String> {
        let connection = self.connection()?;
        connection
            .query_row("SELECT vec_version()", [], |row| row.get(0))
            .map_err(StoreError::Sqlite)
    }

    pub fn table_exists(&self, table_name: &str) -> Result<bool> {
        validate_vector_table_name(table_name)?;
        let connection = self.connection()?;
        connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
                rusqlite::params![table_name],
                |row| row.get::<_, i64>(0),
            )
            .map(|exists| exists != 0)
            .map_err(StoreError::Sqlite)
    }

    pub fn ensure_table(&self, table_name: &str, vector_size: usize) -> Result<bool> {
        validate_vector_table_name(table_name)?;
        if vector_size == 0 {
            return Err(StoreError::InvalidVectorSize(vector_size));
        }
        let connection = self.connection()?;
        ensure_vector_points_table(&connection)?;
        if self.table_exists(table_name)? {
            return Ok(false);
        }

        connection
            .execute(
                &format!(
                    "CREATE VIRTUAL TABLE IF NOT EXISTS {table_name} USING vec0(embedding float[{vector_size}] distance_metric=cosine)"
                ),
                [],
            )
            .map_err(StoreError::Sqlite)?;
        Ok(true)
    }

    pub fn upsert_points(&self, table_name: &str, points: &[VectorPoint]) -> Result<()> {
        validate_vector_table_name(table_name)?;
        if points.is_empty() {
            return Ok(());
        }
        let dimension = points[0].vector.len();
        if dimension == 0 {
            return Err(StoreError::InvalidVectorSize(dimension));
        }
        if points.iter().any(|point| point.vector.len() != dimension) {
            return Err(StoreError::InconsistentVectorDimensions);
        }

        let mut connection = self.connection()?;
        ensure_vector_points_table(&connection)?;
        self.ensure_table(table_name, dimension)?;
        let transaction = connection.transaction().map_err(StoreError::Sqlite)?;
        for point in points {
            let rowid = vector_rowid(&point.id)?;
            transaction
                .execute(
                    &format!("DELETE FROM {table_name} WHERE rowid = ?1"),
                    [rowid],
                )
                .map_err(StoreError::Sqlite)?;
            transaction
                .execute(
                    &format!("INSERT INTO {table_name}(rowid, embedding) VALUES (?1, ?2)"),
                    rusqlite::params![rowid, point.vector.as_bytes()],
                )
                .map_err(StoreError::Sqlite)?;
            transaction
                .execute(
                    "INSERT INTO vector_points (
                       vector_store, vector_table, vector_rowid, vector_point_id,
                       repository_id, file_id, chunk_id, symbol_id, symbol_name,
                       path, language, chunk_kind, start_line, end_line, text_hash,
                       parser_version, content_hash, index_run_id, embedding_model,
                       embedding_dimension, indexed_at
                     )
                     VALUES ('sqlite_vec', ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20)
                     ON CONFLICT(vector_store, vector_table, vector_point_id)
                     DO UPDATE SET
                       vector_rowid = excluded.vector_rowid,
                       repository_id = excluded.repository_id,
                       file_id = excluded.file_id,
                       chunk_id = excluded.chunk_id,
                       symbol_id = excluded.symbol_id,
                       symbol_name = excluded.symbol_name,
                       path = excluded.path,
                       language = excluded.language,
                       chunk_kind = excluded.chunk_kind,
                       start_line = excluded.start_line,
                       end_line = excluded.end_line,
                       text_hash = excluded.text_hash,
                       parser_version = excluded.parser_version,
                       content_hash = excluded.content_hash,
                       index_run_id = excluded.index_run_id,
                       embedding_model = excluded.embedding_model,
                       embedding_dimension = excluded.embedding_dimension,
                       indexed_at = excluded.indexed_at",
                    rusqlite::params![
                        table_name,
                        rowid,
                        point.id,
                        point.payload.repository_id,
                        point.payload.file_id,
                        point.payload.chunk_id,
                        point.payload.symbol_id,
                        point.payload.symbol_name,
                        point.payload.path,
                        point.payload.language,
                        point.payload.chunk_kind,
                        point.payload.start_line as i64,
                        point.payload.end_line as i64,
                        point.payload.text_hash,
                        point.payload.parser_version,
                        point.payload.content_hash,
                        point.payload.index_run_id,
                        point.payload.embedding_model,
                        point.payload.embedding_dimension.map(|dimension| dimension as i64),
                        point.payload.indexed_at,
                    ],
                )
                .map_err(StoreError::Sqlite)?;
        }
        transaction.commit().map_err(StoreError::Sqlite)?;
        Ok(())
    }

    pub fn delete_points(&self, table_name: &str, point_ids: &[String]) -> Result<()> {
        validate_vector_table_name(table_name)?;
        if point_ids.is_empty() {
            return Ok(());
        }

        let mut connection = self.connection()?;
        ensure_vector_points_table(&connection)?;
        let transaction = connection.transaction().map_err(StoreError::Sqlite)?;
        for point_id in point_ids {
            let rowid = vector_rowid(point_id)?;
            transaction
                .execute(
                    &format!("DELETE FROM {table_name} WHERE rowid = ?1"),
                    [rowid],
                )
                .map_err(StoreError::Sqlite)?;
            transaction
                .execute(
                    "DELETE FROM vector_points
                     WHERE vector_store = 'sqlite_vec'
                       AND vector_table = ?1
                       AND vector_point_id = ?2",
                    rusqlite::params![table_name, point_id],
                )
                .map_err(StoreError::Sqlite)?;
        }
        transaction.commit().map_err(StoreError::Sqlite)?;
        Ok(())
    }

    pub fn scroll_points_for_repository(
        &self,
        table_name: &str,
        repository_id: &str,
    ) -> Result<Vec<RetrievedPoint>> {
        validate_vector_table_name(table_name)?;
        let connection = self.connection()?;
        ensure_vector_points_table(&connection)?;
        let mut statement = connection
            .prepare(
                "SELECT vector_point_id, repository_id, file_id, chunk_id, symbol_id,
                        symbol_name, path, language, chunk_kind, start_line, end_line,
                        text_hash, parser_version, content_hash, index_run_id,
                        embedding_model, embedding_dimension, indexed_at
                 FROM vector_points
                 WHERE vector_store = 'sqlite_vec'
                   AND vector_table = ?1
                   AND repository_id = ?2
                 ORDER BY vector_point_id",
            )
            .map_err(StoreError::Sqlite)?;
        let rows = statement
            .query_map(rusqlite::params![table_name, repository_id], |row| {
                Ok(RetrievedPoint {
                    id: row.get(0)?,
                    payload: payload_from_row(row, 1)?,
                })
            })
            .map_err(StoreError::Sqlite)?;
        collect_rows(rows)
    }

    pub fn query_points(
        &self,
        table_name: &str,
        vector: Vec<f32>,
        limit: usize,
    ) -> Result<Vec<ScoredPoint>> {
        validate_vector_table_name(table_name)?;
        if vector.is_empty() {
            return Err(StoreError::InvalidVectorSize(0));
        }
        if limit == 0 {
            return Err(StoreError::InvalidLimit(limit));
        }

        let connection = self.connection()?;
        ensure_vector_points_table(&connection)?;
        if !self.table_exists(table_name)? {
            return Ok(Vec::new());
        }
        let sql = format!(
            "WITH matches AS (
               SELECT rowid, distance
               FROM {table_name}
               WHERE embedding MATCH ?1 AND k = ?2
             )
             SELECT vector_points.vector_point_id, matches.distance,
                    vector_points.repository_id, vector_points.file_id,
                    vector_points.chunk_id, vector_points.symbol_id,
                    vector_points.symbol_name, vector_points.path,
                    vector_points.language, vector_points.chunk_kind,
                    vector_points.start_line, vector_points.end_line,
                    vector_points.text_hash, vector_points.parser_version,
                    vector_points.content_hash, vector_points.index_run_id,
                    vector_points.embedding_model, vector_points.embedding_dimension,
                    vector_points.indexed_at
             FROM matches
             JOIN vector_points
               ON vector_points.vector_store = 'sqlite_vec'
              AND vector_points.vector_table = ?3
              AND vector_points.vector_rowid = matches.rowid
             ORDER BY matches.distance ASC"
        );
        let mut statement = connection.prepare(&sql).map_err(StoreError::Sqlite)?;
        let rows = statement
            .query_map(
                rusqlite::params![vector.as_bytes(), limit as i64, table_name],
                |row| {
                    let distance = row.get::<_, f64>(1)?;
                    Ok(ScoredPoint {
                        id: row.get(0)?,
                        score: 1.0 - distance,
                        payload: payload_from_row(row, 2)?,
                    })
                },
            )
            .map_err(StoreError::Sqlite)?;
        collect_rows(rows)
    }

    pub fn query_points_for_ref(
        &self,
        table_name: &str,
        repository_id: &str,
        repository_ref_id: &str,
        vector: Vec<f32>,
        limit: usize,
    ) -> Result<Vec<ScoredPoint>> {
        validate_vector_table_name(table_name)?;
        if vector.is_empty() {
            return Err(StoreError::InvalidVectorSize(0));
        }
        if limit == 0 {
            return Err(StoreError::InvalidLimit(limit));
        }

        let connection = self.connection()?;
        ensure_vector_points_table(&connection)?;
        if !self.table_exists(table_name)? {
            return Ok(Vec::new());
        }
        let sql = format!(
            "WITH eligible AS (
               SELECT vector_points.vector_rowid
               FROM vector_points
               JOIN ref_files ON ref_files.file_id = vector_points.file_id
               WHERE vector_points.vector_store = 'sqlite_vec'
                 AND vector_points.vector_table = ?3
                 AND vector_points.repository_id = ?4
                 AND ref_files.repository_ref_id = ?5
             ),
             matches AS (
               SELECT rowid, distance
               FROM {table_name}
               WHERE embedding MATCH ?1
                 AND k = ?2
                 AND rowid IN (SELECT vector_rowid FROM eligible)
             )
             SELECT vector_points.vector_point_id, matches.distance,
                    vector_points.repository_id, vector_points.file_id,
                    vector_points.chunk_id, vector_points.symbol_id,
                    vector_points.symbol_name, vector_points.path,
                    vector_points.language, vector_points.chunk_kind,
                    vector_points.start_line, vector_points.end_line,
                    vector_points.text_hash, vector_points.parser_version,
                    vector_points.content_hash, vector_points.index_run_id,
                    vector_points.embedding_model, vector_points.embedding_dimension,
                    vector_points.indexed_at
             FROM matches
             JOIN vector_points
               ON vector_points.vector_store = 'sqlite_vec'
              AND vector_points.vector_table = ?3
              AND vector_points.vector_rowid = matches.rowid
             ORDER BY matches.distance ASC"
        );
        let mut statement = connection.prepare(&sql).map_err(StoreError::Sqlite)?;
        let rows = statement
            .query_map(
                rusqlite::params![
                    vector.as_bytes(),
                    limit as i64,
                    table_name,
                    repository_id,
                    repository_ref_id,
                ],
                |row| {
                    let distance = row.get::<_, f64>(1)?;
                    Ok(ScoredPoint {
                        id: row.get(0)?,
                        score: 1.0 - distance,
                        payload: payload_from_row(row, 2)?,
                    })
                },
            )
            .map_err(StoreError::Sqlite)?;
        collect_rows(rows)
    }

    fn connection(&self) -> Result<rusqlite::Connection> {
        if let Some(parent) = self.sqlite_path.parent() {
            std::fs::create_dir_all(parent).map_err(StoreError::Io)?;
        }
        let connection =
            rusqlite::Connection::open(&self.sqlite_path).map_err(StoreError::Sqlite)?;
        connection
            .busy_timeout(Duration::from_secs(30))
            .map_err(StoreError::Sqlite)?;
        connection
            .execute_batch(
                "PRAGMA journal_mode = WAL;
                 PRAGMA foreign_keys = ON;",
            )
            .map_err(StoreError::Sqlite)?;
        Ok(connection)
    }
}

pub fn vector_table_name(repository_id: &str, embedding_model: &str) -> String {
    format!(
        "symdex_{}_{}",
        slug_component(repository_id),
        slug_component(embedding_model)
    )
}

pub fn vector_point_id(stable_hash: &str) -> Result<String> {
    let hex: String = stable_hash
        .chars()
        .filter(|character| character.is_ascii_hexdigit())
        .map(|character| character.to_ascii_lowercase())
        .collect();
    if hex.len() != 32 {
        return Err(StoreError::InvalidPointId(stable_hash.to_owned()));
    }
    Ok(format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    ))
}

pub fn vector_rowid(point_id: &str) -> Result<i64> {
    if point_id.trim().is_empty() {
        return Err(StoreError::InvalidPointId(point_id.to_owned()));
    }
    let mut hex: String = point_id
        .chars()
        .filter(|character| character.is_ascii_hexdigit())
        .take(16)
        .collect();
    if hex.len() != 16 {
        hex = symdex_core::stable_id(&["vector-rowid", point_id])
            .chars()
            .take(16)
            .collect();
    }
    let value = u64::from_str_radix(&hex, 16)
        .map_err(|_| StoreError::InvalidPointId(point_id.to_owned()))?;
    Ok((value & i64::MAX as u64).max(1) as i64)
}

fn slug_component(input: &str) -> String {
    let mut slug = String::new();
    for character in input.chars() {
        if character.is_ascii_alphanumeric() {
            slug.push(character.to_ascii_lowercase());
        } else if !slug.ends_with('_') {
            slug.push('_');
        }
    }
    if slug.is_empty() {
        "unknown".to_owned()
    } else {
        slug
    }
}

pub(crate) fn validate_vector_table_name(table_name: &str) -> Result<()> {
    if table_name.is_empty()
        || !table_name
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '_')
    {
        return Err(StoreError::InvalidVectorTableName(table_name.to_owned()));
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct VectorPoint {
    pub id: String,
    pub vector: Vec<f32>,
    pub payload: PointPayload,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PointPayload {
    pub repository_id: String,
    pub file_id: String,
    pub chunk_id: String,
    pub symbol_id: Option<String>,
    pub symbol_name: Option<String>,
    pub path: String,
    pub language: String,
    pub chunk_kind: String,
    pub start_line: usize,
    pub end_line: usize,
    pub text_hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parser_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index_run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embedding_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embedding_dimension: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub indexed_at: Option<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct ScoredPoint {
    pub id: String,
    pub score: f64,
    pub payload: PointPayload,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct RetrievedPoint {
    pub id: String,
    pub payload: PointPayload,
}

fn ensure_vector_points_table(connection: &rusqlite::Connection) -> Result<()> {
    connection
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS vector_points (
               vector_store TEXT NOT NULL,
               vector_table TEXT NOT NULL,
               vector_rowid INTEGER NOT NULL,
               vector_point_id TEXT NOT NULL,
               repository_id TEXT NOT NULL,
               file_id TEXT NOT NULL,
               chunk_id TEXT NOT NULL,
               symbol_id TEXT,
               symbol_name TEXT,
               path TEXT NOT NULL,
               language TEXT NOT NULL,
               chunk_kind TEXT NOT NULL,
               start_line INTEGER NOT NULL,
               end_line INTEGER NOT NULL,
               text_hash TEXT NOT NULL,
               parser_version TEXT,
               content_hash TEXT,
               index_run_id TEXT,
               embedding_model TEXT,
               embedding_dimension INTEGER,
               indexed_at TEXT,
               PRIMARY KEY(vector_store, vector_table, vector_point_id),
               UNIQUE(vector_store, vector_table, vector_rowid)
             );
             CREATE INDEX IF NOT EXISTS idx_vector_points_repository_table
               ON vector_points(repository_id, vector_store, vector_table);
             CREATE INDEX IF NOT EXISTS idx_vector_points_chunk
               ON vector_points(chunk_id);",
        )
        .map_err(StoreError::Sqlite)
}

fn payload_from_row(row: &rusqlite::Row<'_>, offset: usize) -> rusqlite::Result<PointPayload> {
    Ok(PointPayload {
        repository_id: row.get(offset)?,
        file_id: row.get(offset + 1)?,
        chunk_id: row.get(offset + 2)?,
        symbol_id: row.get(offset + 3)?,
        symbol_name: row.get(offset + 4)?,
        path: row.get(offset + 5)?,
        language: row.get(offset + 6)?,
        chunk_kind: row.get(offset + 7)?,
        start_line: row.get::<_, i64>(offset + 8)? as usize,
        end_line: row.get::<_, i64>(offset + 9)? as usize,
        text_hash: row.get(offset + 10)?,
        parser_version: row.get(offset + 11)?,
        content_hash: row.get(offset + 12)?,
        index_run_id: row.get(offset + 13)?,
        embedding_model: row.get(offset + 14)?,
        embedding_dimension: row
            .get::<_, Option<i64>>(offset + 15)?
            .map(|dimension| dimension as usize),
        indexed_at: row.get(offset + 16)?,
    })
}

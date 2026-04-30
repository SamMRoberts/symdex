//! Persistence boundary for SQLite and Qdrant adapters.

use std::env;
use std::fmt::{Display, Formatter};
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

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
        Ok(())
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
    ) -> Result<bool> {
        let stored_hash: Option<String> = self
            .connection
            .query_row(
                "SELECT content_hash FROM files WHERE repository_id = ?1 AND path = ?2",
                params![repository_id, path],
                |row| row.get(0),
            )
            .optional()
            .map_err(StoreError::Sqlite)?;
        Ok(stored_hash.as_deref() == Some(content_hash))
    }

    pub fn replace_file_chunks(&mut self, file: &FileRecord, chunks: &[ChunkRecord]) -> Result<()> {
        let transaction = self.connection.transaction().map_err(StoreError::Sqlite)?;
        transaction
            .execute(
                "INSERT INTO files (id, repository_id, path, language, content_hash, indexed_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(repository_id, path) DO UPDATE SET
                   id = excluded.id,
                   language = excluded.language,
                   content_hash = excluded.content_hash,
                   indexed_at = excluded.indexed_at",
                params![
                    file.id,
                    file.repository_id,
                    file.path,
                    file.language,
                    file.content_hash,
                    timestamp()
                ],
            )
            .map_err(StoreError::Sqlite)?;
        transaction
            .execute("DELETE FROM chunks WHERE file_id = ?1", params![file.id])
            .map_err(StoreError::Sqlite)?;

        {
            let mut statement = transaction
                .prepare(
                    "INSERT INTO chunks (
                       id, file_id, symbol_id, kind, text_hash,
                       start_line, end_line, start_byte, end_byte,
                       qdrant_point_id, excluded_reason
                     )
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
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
        Ok(RepositoryStatus {
            repository_id: repository_id.to_owned(),
            files_indexed,
            chunks_indexed,
            last_indexed_at,
        })
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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepositoryStatus {
    pub repository_id: String,
    pub files_indexed: usize,
    pub chunks_indexed: usize,
    pub last_indexed_at: Option<String>,
}

fn env_path(upper: &str, legacy: &str) -> Option<PathBuf> {
    env_value(upper, legacy).map(PathBuf::from)
}

fn env_value(upper: &str, legacy: &str) -> Option<String> {
    env::var(upper).ok().or_else(|| env::var(legacy).ok())
}

#[derive(Debug, Clone)]
pub struct QdrantClient {
    base_url: String,
    http: reqwest::blocking::Client,
}

impl QdrantClient {
    pub fn new(config: &StoreConfig) -> Result<Self> {
        Self::with_timeout(config, Duration::from_secs(30))
    }

    pub fn with_timeout(config: &StoreConfig, timeout: Duration) -> Result<Self> {
        let http = reqwest::blocking::Client::builder()
            .timeout(timeout)
            .build()
            .map_err(StoreError::HttpClient)?;
        Ok(Self {
            base_url: config.qdrant_url.trim_end_matches('/').to_owned(),
            http,
        })
    }

    pub fn health_check(&self) -> Result<()> {
        self.http
            .get(self.endpoint("/collections"))
            .send()
            .map_err(StoreError::HttpRequest)?
            .error_for_status()
            .map_err(StoreError::HttpStatus)?;
        Ok(())
    }

    pub fn collection_exists(&self, collection_name: &str) -> Result<bool> {
        validate_collection_name(collection_name)?;
        let response = self
            .http
            .get(self.endpoint(&format!("/collections/{collection_name}")))
            .send()
            .map_err(StoreError::HttpRequest)?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(false);
        }
        response
            .error_for_status()
            .map_err(StoreError::HttpStatus)?;
        Ok(true)
    }

    pub fn ensure_collection(&self, collection_name: &str, vector_size: usize) -> Result<bool> {
        validate_collection_name(collection_name)?;
        if vector_size == 0 {
            return Err(StoreError::InvalidVectorSize(vector_size));
        }
        if self.collection_exists(collection_name)? {
            return Ok(false);
        }

        let request = CreateCollectionRequest {
            vectors: VectorParams {
                size: vector_size,
                distance: Distance::Cosine,
            },
        };
        let response: QdrantResponse<bool> = self
            .http
            .put(self.endpoint(&format!("/collections/{collection_name}")))
            .json(&request)
            .send()
            .map_err(StoreError::HttpRequest)?
            .error_for_status()
            .map_err(StoreError::HttpStatus)?
            .json()
            .map_err(StoreError::Decode)?;

        if response.status != "ok" || !response.result {
            return Err(StoreError::UnexpectedResponse(format!(
                "status={} result={}",
                response.status, response.result
            )));
        }
        Ok(true)
    }

    pub fn upsert_points(&self, collection_name: &str, points: &[VectorPoint]) -> Result<()> {
        validate_collection_name(collection_name)?;
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

        let request = UpsertPointsRequest { points };
        let response: QdrantResponse<OperationResult> = self
            .http
            .put(self.endpoint(&format!("/collections/{collection_name}/points?wait=true")))
            .json(&request)
            .send()
            .map_err(StoreError::HttpRequest)?
            .error_for_status()
            .map_err(StoreError::HttpStatus)?
            .json()
            .map_err(StoreError::Decode)?;

        if response.status != "ok" {
            return Err(StoreError::UnexpectedResponse(format!(
                "status={} operation_status={}",
                response.status, response.result.status
            )));
        }
        Ok(())
    }

    pub fn query_points(
        &self,
        collection_name: &str,
        vector: Vec<f32>,
        limit: usize,
    ) -> Result<Vec<ScoredPoint>> {
        validate_collection_name(collection_name)?;
        if vector.is_empty() {
            return Err(StoreError::InvalidVectorSize(0));
        }
        if limit == 0 {
            return Err(StoreError::InvalidLimit(limit));
        }

        let request = QueryPointsRequest {
            query: vector,
            limit,
            with_payload: true,
            with_vector: false,
        };
        let response: QdrantResponse<QueryPointsResult> = self
            .http
            .post(self.endpoint(&format!("/collections/{collection_name}/points/query")))
            .json(&request)
            .send()
            .map_err(StoreError::HttpRequest)?
            .error_for_status()
            .map_err(StoreError::HttpStatus)?
            .json()
            .map_err(StoreError::Decode)?;

        if response.status != "ok" {
            return Err(StoreError::UnexpectedResponse(format!(
                "status={}",
                response.status
            )));
        }
        Ok(response.result.points)
    }

    fn endpoint(&self, path: &str) -> String {
        format!("{}/{}", self.base_url, path.trim_start_matches('/'))
    }
}

pub fn qdrant_collection_name(repository_id: &str, embedding_model: &str) -> String {
    format!(
        "symdex_{}_{}",
        slug_component(repository_id),
        slug_component(embedding_model)
    )
}

pub fn qdrant_point_id(stable_hash: &str) -> Result<String> {
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

fn validate_collection_name(collection_name: &str) -> Result<()> {
    if collection_name.is_empty()
        || !collection_name.chars().all(|character| {
            character.is_ascii_alphanumeric() || character == '_' || character == '-'
        })
    {
        return Err(StoreError::InvalidCollectionName(
            collection_name.to_owned(),
        ));
    }
    Ok(())
}

#[derive(Debug, Serialize)]
struct CreateCollectionRequest {
    vectors: VectorParams,
}

#[derive(Debug, Serialize)]
struct VectorParams {
    size: usize,
    distance: Distance,
}

#[derive(Debug, Serialize)]
enum Distance {
    Cosine,
}

#[derive(Debug, Deserialize)]
struct QdrantResponse<T> {
    status: String,
    result: T,
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
}

#[derive(Debug, Serialize)]
struct UpsertPointsRequest<'a> {
    points: &'a [VectorPoint],
}

#[derive(Debug, Deserialize)]
struct OperationResult {
    status: String,
}

#[derive(Debug, Serialize)]
struct QueryPointsRequest {
    query: Vec<f32>,
    limit: usize,
    with_payload: bool,
    with_vector: bool,
}

#[derive(Debug, Deserialize)]
struct QueryPointsResult {
    points: Vec<ScoredPoint>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct ScoredPoint {
    pub id: serde_json::Value,
    pub score: f64,
    pub payload: PointPayload,
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
            Self::UnexpectedResponse(message) => write!(f, "unexpected Qdrant response: {message}"),
        }
    }
}

impl std::error::Error for StoreError {}

pub type Result<T> = std::result::Result<T, StoreError>;

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
  error_summary TEXT
);

CREATE TABLE IF NOT EXISTS files (
  id TEXT PRIMARY KEY,
  repository_id TEXT NOT NULL,
  path TEXT NOT NULL,
  language TEXT NOT NULL,
  content_hash TEXT NOT NULL,
  indexed_at TEXT NOT NULL,
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
  FOREIGN KEY(file_id) REFERENCES files(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS calls (
  id TEXT PRIMARY KEY,
  caller_symbol_id TEXT NOT NULL,
  callee_text TEXT NOT NULL,
  callee_symbol_id TEXT,
  call_line INTEGER NOT NULL,
  confidence REAL NOT NULL,
  resolution_status TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_files_repository_path ON files(repository_id, path);
CREATE INDEX IF NOT EXISTS idx_chunks_file_id ON chunks(file_id);
"#;

fn timestamp() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    seconds.to_string()
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    use crate::{
        ChunkRecord, CreateCollectionRequest, Distance, FileRecord, PointPayload, QdrantClient,
        QueryPointsRequest, RepositoryRecord, SqliteStore, StoreConfig, UpsertPointsRequest,
        VectorParams, VectorPoint, qdrant_collection_name, qdrant_point_id,
        validate_collection_name,
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
            .replace_file_chunks(&sample_file("hash-1"), &[sample_chunk("chunk-1")])
            .expect("file chunks should persist");

        assert!(
            store
                .file_unchanged("repo", "src/lib.rs", "hash-1")
                .expect("unchanged check should run")
        );
        let status = store.repository_status("repo").expect("status should load");
        assert_eq!(status.files_indexed, 1);
        assert_eq!(status.chunks_indexed, 1);
        assert!(status.last_indexed_at.is_some());
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
            .replace_file_chunks(&sample_file("hash-1"), &[sample_chunk("chunk-1")])
            .expect("initial chunks should persist");
        store
            .replace_file_chunks(
                &sample_file("hash-2"),
                &[sample_chunk("chunk-2"), sample_chunk("chunk-3")],
            )
            .expect("replacement chunks should persist");

        let status = store.repository_status("repo").expect("status should load");
        assert_eq!(status.files_indexed, 1);
        assert_eq!(status.chunks_indexed, 2);
        assert!(
            !store
                .file_unchanged("repo", "src/lib.rs", "hash-1")
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
        }
    }

    fn sample_file(content_hash: &str) -> FileRecord {
        FileRecord {
            id: "file".to_owned(),
            repository_id: "repo".to_owned(),
            path: "src/lib.rs".to_owned(),
            language: "rust".to_owned(),
            content_hash: content_hash.to_owned(),
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
        }
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

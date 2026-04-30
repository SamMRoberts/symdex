//! Persistence boundary for SQLite and Qdrant adapters.

use std::env;
use std::fmt::{Display, Formatter};
use std::path::PathBuf;
use std::time::Duration;

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

#[cfg(test)]
mod tests {
    use crate::{
        CreateCollectionRequest, Distance, PointPayload, QdrantClient, QueryPointsRequest,
        StoreConfig, UpsertPointsRequest, VectorParams, VectorPoint, qdrant_collection_name,
        qdrant_point_id, validate_collection_name,
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
}

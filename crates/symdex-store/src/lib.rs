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

#[derive(Debug)]
pub enum StoreError {
    HttpClient(reqwest::Error),
    HttpRequest(reqwest::Error),
    HttpStatus(reqwest::Error),
    Decode(reqwest::Error),
    InvalidCollectionName(String),
    InvalidVectorSize(usize),
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
            Self::InvalidVectorSize(size) => write!(f, "invalid Qdrant vector size `{size}`"),
            Self::UnexpectedResponse(message) => write!(f, "unexpected Qdrant response: {message}"),
        }
    }
}

impl std::error::Error for StoreError {}

pub type Result<T> = std::result::Result<T, StoreError>;

#[cfg(test)]
mod tests {
    use crate::{
        CreateCollectionRequest, Distance, QdrantClient, StoreConfig, VectorParams,
        qdrant_collection_name, validate_collection_name,
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
    fn live_qdrant_health_is_opt_in() {
        if std::env::var("SYMDEX_TEST_QDRANT").ok().as_deref() != Some("1") {
            return;
        }

        let client = QdrantClient::new(&StoreConfig::from_env()).expect("client should build");
        client.health_check().expect("qdrant should be reachable");
    }
}

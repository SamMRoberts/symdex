use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::{Result, StoreConfig, StoreError};

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

    pub fn delete_points(&self, collection_name: &str, point_ids: &[String]) -> Result<()> {
        validate_collection_name(collection_name)?;
        if point_ids.is_empty() {
            return Ok(());
        }

        let request = DeletePointsRequest { points: point_ids };
        let response: QdrantResponse<OperationResult> = self
            .http
            .post(self.endpoint(&format!(
                "/collections/{collection_name}/points/delete?wait=true"
            )))
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

    pub fn scroll_points_for_repository(
        &self,
        collection_name: &str,
        repository_id: &str,
    ) -> Result<Vec<RetrievedPoint>> {
        validate_collection_name(collection_name)?;
        let mut points = Vec::new();
        let mut offset = None;
        loop {
            let request = ScrollPointsRequest {
                filter: RepositoryFilter {
                    must: vec![RepositoryFilterCondition {
                        key: "repository_id",
                        value_match: MatchValue {
                            value: repository_id,
                        },
                    }],
                },
                limit: 256,
                with_payload: true,
                with_vector: false,
                offset,
            };
            let response: QdrantResponse<ScrollPointsResult> = self
                .http
                .post(self.endpoint(&format!("/collections/{collection_name}/points/scroll")))
                .json(&request)
                .send()
                .map_err(StoreError::HttpRequest)?
                .error_for_status()
                .map_err(StoreError::HttpStatus)?
                .json()
                .map_err(StoreError::Decode)?;

            if response.status != "ok" {
                return Err(StoreError::UnexpectedResponse(format!(
                    "status={} points={}",
                    response.status,
                    response.result.points.len()
                )));
            }
            points.extend(response.result.points);
            offset = response.result.next_page_offset;
            if offset.is_none() {
                break;
            }
        }
        Ok(points)
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

pub(crate) fn validate_collection_name(collection_name: &str) -> Result<()> {
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
pub(crate) struct CreateCollectionRequest {
    pub(crate) vectors: VectorParams,
}

#[derive(Debug, Serialize)]
pub(crate) struct VectorParams {
    pub(crate) size: usize,
    pub(crate) distance: Distance,
}

#[derive(Debug, Serialize)]
pub(crate) enum Distance {
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

#[derive(Debug, Serialize)]
pub(crate) struct UpsertPointsRequest<'a> {
    pub(crate) points: &'a [VectorPoint],
}

#[derive(Debug, Serialize)]
pub(crate) struct DeletePointsRequest<'a> {
    pub(crate) points: &'a [String],
}

#[derive(Debug, Deserialize)]
struct OperationResult {
    status: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct ScrollPointsRequest<'a> {
    pub(crate) filter: RepositoryFilter<'a>,
    pub(crate) limit: usize,
    pub(crate) with_payload: bool,
    pub(crate) with_vector: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) offset: Option<serde_json::Value>,
}

#[derive(Debug, Serialize)]
pub(crate) struct RepositoryFilter<'a> {
    pub(crate) must: Vec<RepositoryFilterCondition<'a>>,
}

#[derive(Debug, Serialize)]
pub(crate) struct RepositoryFilterCondition<'a> {
    pub(crate) key: &'a str,
    #[serde(rename = "match")]
    pub(crate) value_match: MatchValue<'a>,
}

#[derive(Debug, Serialize)]
pub(crate) struct MatchValue<'a> {
    pub(crate) value: &'a str,
}

#[derive(Debug, Deserialize)]
struct ScrollPointsResult {
    points: Vec<RetrievedPoint>,
    next_page_offset: Option<serde_json::Value>,
}

#[derive(Debug, Serialize)]
pub(crate) struct QueryPointsRequest {
    pub(crate) query: Vec<f32>,
    pub(crate) limit: usize,
    pub(crate) with_payload: bool,
    pub(crate) with_vector: bool,
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

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct RetrievedPoint {
    pub id: serde_json::Value,
    pub payload: PointPayload,
}

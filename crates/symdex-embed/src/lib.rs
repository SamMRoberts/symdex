//! Local embedding adapter boundary for Ollama.

use std::env;
use std::fmt::{Display, Formatter};
use std::time::Duration;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbedConfig {
    pub ollama_url: String,
    pub model: String,
    pub truncate: bool,
}

impl EmbedConfig {
    pub fn from_env() -> Self {
        Self {
            ollama_url: env_value("SYMDEX_OLLAMA_URL", "symdex_OLLAMA_URL")
                .unwrap_or_else(|| "http://localhost:11434".to_owned()),
            model: env_value("SYMDEX_EMBED_MODEL", "symdex_EMBED_MODEL")
                .unwrap_or_else(|| "nomic-embed-text".to_owned()),
            truncate: env_value("SYMDEX_EMBED_TRUNCATE", "symdex_EMBED_TRUNCATE")
                .as_deref()
                .map(env_bool)
                .unwrap_or(true),
        }
    }
}

#[derive(Debug, Clone)]
pub struct OllamaClient {
    config: EmbedConfig,
    http: reqwest::blocking::Client,
}

impl OllamaClient {
    pub fn new(config: EmbedConfig) -> Result<Self> {
        let http = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(EmbedError::HttpClient)?;
        Ok(Self { config, http })
    }

    pub fn config(&self) -> &EmbedConfig {
        &self.config
    }

    pub fn list_models(&self) -> Result<Vec<ModelInfo>> {
        let response: TagsResponse = self
            .http
            .get(self.endpoint("/api/tags"))
            .send()
            .map_err(EmbedError::HttpRequest)?
            .error_for_status()
            .map_err(EmbedError::HttpStatus)?
            .json()
            .map_err(EmbedError::Decode)?;
        Ok(response.models)
    }

    pub fn model_available(&self) -> Result<bool> {
        Ok(model_available_in(&self.list_models()?, &self.config.model))
    }

    pub fn embed_batch(&self, inputs: &[String]) -> Result<EmbeddingBatch> {
        if inputs.is_empty() {
            return Ok(EmbeddingBatch {
                model: self.config.model.clone(),
                embeddings: Vec::new(),
                prompt_eval_count: None,
            });
        }

        let request = EmbedRequest {
            model: self.config.model.clone(),
            input: inputs,
            truncate: self.config.truncate,
        };
        let response: EmbedResponse = self
            .http
            .post(self.endpoint("/api/embed"))
            .json(&request)
            .send()
            .map_err(EmbedError::HttpRequest)?
            .error_for_status()
            .map_err(EmbedError::HttpStatus)?
            .json()
            .map_err(EmbedError::Decode)?;

        embedding_batch_from_response(response, inputs.len())
    }

    pub fn probe_dimension(&self) -> Result<usize> {
        let batch = self.embed_batch(&["symdex dimension probe".to_owned()])?;
        batch
            .dimension()
            .ok_or(EmbedError::MissingEmbeddingDimension)
    }

    fn endpoint(&self, path: &str) -> String {
        format!(
            "{}/{}",
            self.config.ollama_url.trim_end_matches('/'),
            path.trim_start_matches('/')
        )
    }
}

fn model_available_in(models: &[ModelInfo], configured: &str) -> bool {
    let configured_latest = format!("{configured}:latest");
    models.iter().any(|model| {
        model.name == configured
            || model.model == configured
            || model.name == configured_latest
            || model.model == configured_latest
    })
}

fn embedding_batch_from_response(
    response: EmbedResponse,
    expected_count: usize,
) -> Result<EmbeddingBatch> {
    if response.embeddings.len() != expected_count {
        return Err(EmbedError::EmbeddingCount {
            expected: expected_count,
            actual: response.embeddings.len(),
        });
    }

    let dimension = response.embeddings.first().map(Vec::len).unwrap_or(0);
    if response
        .embeddings
        .iter()
        .any(|embedding| embedding.len() != dimension)
    {
        return Err(EmbedError::InconsistentDimensions);
    }

    Ok(EmbeddingBatch {
        model: response.model,
        embeddings: response.embeddings,
        prompt_eval_count: response.prompt_eval_count,
    })
}

#[derive(Debug, Clone, PartialEq)]
pub struct EmbeddingBatch {
    pub model: String,
    pub embeddings: Vec<Vec<f32>>,
    pub prompt_eval_count: Option<u64>,
}

impl EmbeddingBatch {
    pub fn dimension(&self) -> Option<usize> {
        self.embeddings.first().map(Vec::len)
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct ModelInfo {
    pub name: String,
    #[serde(default)]
    pub model: String,
}

#[derive(Debug)]
pub enum EmbedError {
    HttpClient(reqwest::Error),
    HttpRequest(reqwest::Error),
    HttpStatus(reqwest::Error),
    Decode(reqwest::Error),
    EmbeddingCount { expected: usize, actual: usize },
    InconsistentDimensions,
    MissingEmbeddingDimension,
}

impl Display for EmbedError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::HttpClient(error) => write!(f, "failed to create HTTP client: {error}"),
            Self::HttpRequest(error) => write!(f, "Ollama request failed: {error}"),
            Self::HttpStatus(error) => write!(f, "Ollama returned an error status: {error}"),
            Self::Decode(error) => write!(f, "failed to decode Ollama response: {error}"),
            Self::EmbeddingCount { expected, actual } => write!(
                f,
                "Ollama returned {actual} embeddings for {expected} inputs"
            ),
            Self::InconsistentDimensions => {
                write!(f, "Ollama returned embeddings with inconsistent dimensions")
            }
            Self::MissingEmbeddingDimension => write!(f, "Ollama returned no embedding dimension"),
        }
    }
}

impl std::error::Error for EmbedError {}

pub type Result<T> = std::result::Result<T, EmbedError>;

#[derive(Debug, Serialize)]
struct EmbedRequest<'a> {
    model: String,
    input: &'a [String],
    truncate: bool,
}

#[derive(Debug, Deserialize)]
struct EmbedResponse {
    model: String,
    embeddings: Vec<Vec<f32>>,
    prompt_eval_count: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct TagsResponse {
    models: Vec<ModelInfo>,
}

fn env_value(upper: &str, legacy: &str) -> Option<String> {
    env::var(upper).ok().or_else(|| env::var(legacy).ok())
}

fn env_bool(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

#[cfg(test)]
mod tests {
    use crate::{
        EmbedConfig, EmbedRequest, EmbedResponse, ModelInfo, OllamaClient,
        embedding_batch_from_response, env_bool, model_available_in,
    };

    #[test]
    fn model_available_matches_plain_and_latest_names() {
        let models = vec![ModelInfo {
            name: "nomic-embed-text:latest".to_owned(),
            model: "nomic-embed-text:latest".to_owned(),
        }];

        assert!(model_available_in(&models, "nomic-embed-text"));
    }

    #[test]
    fn embed_request_uses_current_api_shape_with_truncation_enabled() {
        let inputs = vec!["first".to_owned(), "second".to_owned()];
        let request = EmbedRequest {
            model: "nomic-embed-text".to_owned(),
            input: &inputs,
            truncate: true,
        };

        let json = serde_json::to_value(request).expect("request should serialize");

        assert_eq!(json["model"], "nomic-embed-text");
        assert_eq!(json["input"][0], "first");
        assert_eq!(json["truncate"], true);
    }

    #[test]
    fn embed_truncate_env_accepts_explicit_truthy_values() {
        assert!(env_bool("1"));
        assert!(env_bool("true"));
        assert!(env_bool("yes"));
        assert!(env_bool("on"));
        assert!(!env_bool("0"));
        assert!(!env_bool("false"));
    }

    #[test]
    fn embed_response_reports_dimension() {
        let response = EmbedResponse {
            model: "nomic-embed-text".to_owned(),
            embeddings: vec![vec![0.1, 0.2, 0.3], vec![0.4, 0.5, 0.6]],
            prompt_eval_count: Some(12),
        };

        let batch = embedding_batch_from_response(response, 2).expect("response should validate");
        assert_eq!(batch.model, "nomic-embed-text");
        assert_eq!(batch.dimension(), Some(3));
        assert_eq!(batch.embeddings.len(), 2);
        assert_eq!(batch.prompt_eval_count, Some(12));
    }

    #[test]
    fn embed_batch_rejects_mismatched_counts() {
        let response = EmbedResponse {
            model: "nomic-embed-text".to_owned(),
            embeddings: vec![vec![0.1, 0.2, 0.3]],
            prompt_eval_count: None,
        };

        let error = embedding_batch_from_response(response, 2)
            .expect_err("mismatched embeddings should fail");

        assert!(
            error
                .to_string()
                .contains("returned 1 embeddings for 2 inputs")
        );
    }

    #[test]
    fn live_ollama_probe_is_opt_in() {
        if std::env::var("SYMDEX_TEST_OLLAMA").ok().as_deref() != Some("1") {
            return;
        }

        let client = OllamaClient::new(EmbedConfig::from_env()).expect("client should build");
        assert!(client.model_available().expect("model check should work"));
        assert!(
            client
                .probe_dimension()
                .expect("dimension probe should work")
                > 0
        );
    }
}

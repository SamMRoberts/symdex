//! Local embedding adapter boundary for Ollama.

use std::env;
use std::fmt::{Display, Formatter};
use std::time::Duration;

use serde::{Deserialize, Serialize};

pub const DEFAULT_OLLAMA_URL: &str = "http://localhost:11434";
pub const DEFAULT_FAST_EMBED_MODEL: &str = "nomic-embed-text";
pub const DEFAULT_QUALITY_EMBED_MODEL: &str = "nomic-embed-text-v2-moe";
pub const DEFAULT_EMBED_TRUNCATE: bool = true;
pub const DEFAULT_EMBED_BATCH_SIZE: usize = 16;
pub const DEFAULT_QUALITY_EMBED_BATCH_SIZE: usize = 16;
pub const DEFAULT_QUALITY_EMBED_WORKERS: usize = 1;
pub const DEFAULT_EMBED_MAX_CHUNK_BYTES: usize = 32 * 1024;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EmbedConfigValues<'a> {
    pub ollama_url: Option<&'a str>,
    pub model: Option<&'a str>,
    pub truncate: Option<&'a str>,
    pub batch_size: Option<&'a str>,
    pub max_chunk_bytes: Option<&'a str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbedConfig {
    pub ollama_url: String,
    pub model: String,
    pub truncate: bool,
    pub batch_size: usize,
    pub max_chunk_bytes: usize,
}

impl EmbedConfig {
    pub fn from_env() -> Self {
        let ollama_url = env_value("SYMDEX_OLLAMA_URL", "symdex_OLLAMA_URL");
        let model = env_value("SYMDEX_EMBED_MODEL", "symdex_EMBED_MODEL");
        let truncate = env_value("SYMDEX_EMBED_TRUNCATE", "symdex_EMBED_TRUNCATE");
        let batch_size = env_value("SYMDEX_EMBED_BATCH_SIZE", "symdex_EMBED_BATCH_SIZE");
        let max_chunk_bytes = env_value(
            "SYMDEX_EMBED_MAX_CHUNK_BYTES",
            "symdex_EMBED_MAX_CHUNK_BYTES",
        );

        Self::from_values(EmbedConfigValues {
            ollama_url: ollama_url.as_deref(),
            model: model.as_deref(),
            truncate: truncate.as_deref(),
            batch_size: batch_size.as_deref(),
            max_chunk_bytes: max_chunk_bytes.as_deref(),
        })
    }

    pub fn from_values(values: EmbedConfigValues<'_>) -> Self {
        Self {
            ollama_url: values.ollama_url.unwrap_or(DEFAULT_OLLAMA_URL).to_owned(),
            model: values.model.unwrap_or(DEFAULT_FAST_EMBED_MODEL).to_owned(),
            truncate: values
                .truncate
                .map(env_bool)
                .unwrap_or(DEFAULT_EMBED_TRUNCATE),
            batch_size: values
                .batch_size
                .and_then(env_usize)
                .unwrap_or(DEFAULT_EMBED_BATCH_SIZE),
            max_chunk_bytes: values
                .max_chunk_bytes
                .and_then(env_usize)
                .unwrap_or(DEFAULT_EMBED_MAX_CHUNK_BYTES),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayeredEmbedConfig {
    pub ollama_url: String,
    pub fast_model: String,
    pub quality_model: String,
    pub quality_enabled: bool,
    pub truncate: bool,
    pub batch_size: usize,
    pub quality_batch_size: usize,
    pub quality_workers: usize,
    pub max_chunk_bytes: usize,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LayeredEmbedConfigValues<'a> {
    pub ollama_url: Option<&'a str>,
    pub legacy_model: Option<&'a str>,
    pub fast_model: Option<&'a str>,
    pub quality_model: Option<&'a str>,
    pub quality_enabled: Option<&'a str>,
    pub truncate: Option<&'a str>,
    pub batch_size: Option<&'a str>,
    pub quality_batch_size: Option<&'a str>,
    pub quality_workers: Option<&'a str>,
    pub max_chunk_bytes: Option<&'a str>,
}

impl LayeredEmbedConfig {
    pub fn from_env() -> Self {
        let ollama_url = env_value("SYMDEX_OLLAMA_URL", "symdex_OLLAMA_URL");
        let legacy_model = env_value("SYMDEX_EMBED_MODEL", "symdex_EMBED_MODEL");
        let fast_model = env_value("SYMDEX_FAST_EMBED_MODEL", "symdex_FAST_EMBED_MODEL");
        let quality_model = env_value("SYMDEX_QUALITY_EMBED_MODEL", "symdex_QUALITY_EMBED_MODEL");
        let quality_enabled = env_value("SYMDEX_QUALITY_INDEX", "symdex_QUALITY_INDEX");
        let truncate = env_value("SYMDEX_EMBED_TRUNCATE", "symdex_EMBED_TRUNCATE");
        let batch_size = env_value("SYMDEX_EMBED_BATCH_SIZE", "symdex_EMBED_BATCH_SIZE");
        let quality_batch_size =
            env_value("SYMDEX_QUALITY_BATCH_SIZE", "symdex_QUALITY_BATCH_SIZE");
        let quality_workers = env_value("SYMDEX_QUALITY_WORKERS", "symdex_QUALITY_WORKERS");
        let max_chunk_bytes = env_value(
            "SYMDEX_EMBED_MAX_CHUNK_BYTES",
            "symdex_EMBED_MAX_CHUNK_BYTES",
        );

        Self::from_values(LayeredEmbedConfigValues {
            ollama_url: ollama_url.as_deref(),
            legacy_model: legacy_model.as_deref(),
            fast_model: fast_model.as_deref(),
            quality_model: quality_model.as_deref(),
            quality_enabled: quality_enabled.as_deref(),
            truncate: truncate.as_deref(),
            batch_size: batch_size.as_deref(),
            quality_batch_size: quality_batch_size.as_deref(),
            quality_workers: quality_workers.as_deref(),
            max_chunk_bytes: max_chunk_bytes.as_deref(),
        })
    }

    pub fn from_values(values: LayeredEmbedConfigValues<'_>) -> Self {
        let fast_model = values
            .fast_model
            .or(values.legacy_model)
            .unwrap_or(DEFAULT_FAST_EMBED_MODEL)
            .to_owned();

        Self {
            ollama_url: values.ollama_url.unwrap_or(DEFAULT_OLLAMA_URL).to_owned(),
            fast_model,
            quality_model: values
                .quality_model
                .unwrap_or(DEFAULT_QUALITY_EMBED_MODEL)
                .to_owned(),
            quality_enabled: values.quality_enabled.map(env_bool).unwrap_or(true),
            truncate: values
                .truncate
                .map(env_bool)
                .unwrap_or(DEFAULT_EMBED_TRUNCATE),
            batch_size: values
                .batch_size
                .and_then(env_usize)
                .unwrap_or(DEFAULT_EMBED_BATCH_SIZE),
            quality_batch_size: values
                .quality_batch_size
                .and_then(env_usize)
                .unwrap_or(DEFAULT_QUALITY_EMBED_BATCH_SIZE),
            quality_workers: values
                .quality_workers
                .and_then(env_usize)
                .unwrap_or(DEFAULT_QUALITY_EMBED_WORKERS),
            max_chunk_bytes: values
                .max_chunk_bytes
                .and_then(env_usize)
                .unwrap_or(DEFAULT_EMBED_MAX_CHUNK_BYTES),
        }
    }

    pub fn fast_embed_config(&self) -> EmbedConfig {
        EmbedConfig {
            ollama_url: self.ollama_url.clone(),
            model: self.fast_model.clone(),
            truncate: self.truncate,
            batch_size: self.batch_size,
            max_chunk_bytes: self.max_chunk_bytes,
        }
    }

    pub fn quality_embed_config(&self) -> EmbedConfig {
        EmbedConfig {
            ollama_url: self.ollama_url.clone(),
            model: self.quality_model.clone(),
            truncate: self.truncate,
            batch_size: self.quality_batch_size,
            max_chunk_bytes: self.max_chunk_bytes,
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

        let mut model = None;
        let mut embeddings = Vec::with_capacity(inputs.len());
        let mut prompt_eval_count = None;
        for batch in inputs.chunks(self.config.batch_size.max(1)) {
            let response = self.embed_batch_request(batch)?;
            let batch = embedding_batch_from_response(response, batch.len())?;
            if model.is_none() {
                model = Some(batch.model.clone());
            }
            prompt_eval_count = merge_prompt_eval_count(prompt_eval_count, batch.prompt_eval_count);
            embeddings.extend(batch.embeddings);
        }

        embedding_batch_from_parts(
            model.unwrap_or_else(|| self.config.model.clone()),
            embeddings,
            prompt_eval_count,
            inputs.len(),
        )
    }

    fn embed_batch_request(&self, inputs: &[String]) -> Result<EmbedResponse> {
        let request = EmbedRequest {
            model: self.config.model.clone(),
            input: inputs,
            truncate: self.config.truncate,
        };
        self.http
            .post(self.endpoint("/api/embed"))
            .json(&request)
            .send()
            .map_err(EmbedError::HttpRequest)?
            .error_for_status()
            .map_err(EmbedError::HttpStatus)?
            .json()
            .map_err(EmbedError::Decode)
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
    embedding_batch_from_parts(
        response.model,
        response.embeddings,
        response.prompt_eval_count,
        expected_count,
    )
}

fn embedding_batch_from_parts(
    model: String,
    embeddings: Vec<Vec<f32>>,
    prompt_eval_count: Option<u64>,
    expected_count: usize,
) -> Result<EmbeddingBatch> {
    if embeddings.len() != expected_count {
        return Err(EmbedError::EmbeddingCount {
            expected: expected_count,
            actual: embeddings.len(),
        });
    }

    let dimension = embeddings.first().map(Vec::len).unwrap_or(0);
    if embeddings
        .iter()
        .any(|embedding| embedding.len() != dimension)
    {
        return Err(EmbedError::InconsistentDimensions);
    }

    Ok(EmbeddingBatch {
        model,
        embeddings,
        prompt_eval_count,
    })
}

fn merge_prompt_eval_count(left: Option<u64>, right: Option<u64>) -> Option<u64> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left + right),
        (Some(left), None) => Some(left),
        (None, Some(right)) => Some(right),
        (None, None) => None,
    }
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

fn env_usize(value: &str) -> Option<usize> {
    value
        .trim()
        .parse::<usize>()
        .ok()
        .filter(|value| *value > 0)
}

#[cfg(test)]
mod tests {
    use crate::{
        DEFAULT_EMBED_BATCH_SIZE, DEFAULT_EMBED_MAX_CHUNK_BYTES, DEFAULT_EMBED_TRUNCATE,
        DEFAULT_FAST_EMBED_MODEL, DEFAULT_OLLAMA_URL, DEFAULT_QUALITY_EMBED_BATCH_SIZE,
        DEFAULT_QUALITY_EMBED_MODEL, DEFAULT_QUALITY_EMBED_WORKERS, EmbedConfig, EmbedConfigValues,
        EmbedRequest, EmbedResponse, LayeredEmbedConfig, LayeredEmbedConfigValues, ModelInfo,
        OllamaClient, embedding_batch_from_parts, embedding_batch_from_response, env_bool,
        env_usize, model_available_in,
    };

    #[test]
    fn embed_config_from_values_uses_existing_defaults() {
        let config = EmbedConfig::from_values(EmbedConfigValues::default());

        assert_eq!(config.ollama_url, DEFAULT_OLLAMA_URL);
        assert_eq!(config.model, DEFAULT_FAST_EMBED_MODEL);
        assert_eq!(config.truncate, DEFAULT_EMBED_TRUNCATE);
        assert_eq!(config.batch_size, DEFAULT_EMBED_BATCH_SIZE);
        assert_eq!(config.max_chunk_bytes, DEFAULT_EMBED_MAX_CHUNK_BYTES);
    }

    #[test]
    fn embed_config_from_values_accepts_existing_overrides() {
        let config = EmbedConfig::from_values(EmbedConfigValues {
            ollama_url: Some("http://127.0.0.1:11434"),
            model: Some("custom-fast"),
            truncate: Some("false"),
            batch_size: Some("4"),
            max_chunk_bytes: Some("1024"),
        });

        assert_eq!(config.ollama_url, "http://127.0.0.1:11434");
        assert_eq!(config.model, "custom-fast");
        assert!(!config.truncate);
        assert_eq!(config.batch_size, 4);
        assert_eq!(config.max_chunk_bytes, 1024);
    }

    #[test]
    fn layered_embed_config_uses_layer_defaults() {
        let config = LayeredEmbedConfig::from_values(LayeredEmbedConfigValues::default());

        assert_eq!(config.ollama_url, DEFAULT_OLLAMA_URL);
        assert_eq!(config.fast_model, DEFAULT_FAST_EMBED_MODEL);
        assert_eq!(config.quality_model, DEFAULT_QUALITY_EMBED_MODEL);
        assert!(config.quality_enabled);
        assert_eq!(config.truncate, DEFAULT_EMBED_TRUNCATE);
        assert_eq!(config.batch_size, DEFAULT_EMBED_BATCH_SIZE);
        assert_eq!(config.quality_batch_size, DEFAULT_QUALITY_EMBED_BATCH_SIZE);
        assert_eq!(config.quality_workers, DEFAULT_QUALITY_EMBED_WORKERS);
        assert_eq!(config.max_chunk_bytes, DEFAULT_EMBED_MAX_CHUNK_BYTES);
    }

    #[test]
    fn layered_embed_config_prefers_fast_model_over_legacy_model() {
        let config = LayeredEmbedConfig::from_values(LayeredEmbedConfigValues {
            legacy_model: Some("legacy-fast"),
            fast_model: Some("layer-fast"),
            ..LayeredEmbedConfigValues::default()
        });

        assert_eq!(config.fast_model, "layer-fast");
        assert_eq!(config.quality_model, DEFAULT_QUALITY_EMBED_MODEL);
    }

    #[test]
    fn layered_embed_config_uses_legacy_model_only_for_fast_fallback() {
        let config = LayeredEmbedConfig::from_values(LayeredEmbedConfigValues {
            legacy_model: Some("legacy-fast"),
            ..LayeredEmbedConfigValues::default()
        });

        assert_eq!(config.fast_model, "legacy-fast");
        assert_eq!(config.quality_model, DEFAULT_QUALITY_EMBED_MODEL);
    }

    #[test]
    fn layered_embed_config_accepts_quality_model_override() {
        let config = LayeredEmbedConfig::from_values(LayeredEmbedConfigValues {
            legacy_model: Some("legacy-fast"),
            quality_model: Some("quality-custom"),
            ..LayeredEmbedConfigValues::default()
        });

        assert_eq!(config.fast_model, "legacy-fast");
        assert_eq!(config.quality_model, "quality-custom");
    }

    #[test]
    fn layered_embed_config_parses_quality_controls() {
        let config = LayeredEmbedConfig::from_values(LayeredEmbedConfigValues {
            quality_enabled: Some("0"),
            quality_batch_size: Some("8"),
            quality_workers: Some("2"),
            ..LayeredEmbedConfigValues::default()
        });

        assert!(!config.quality_enabled);
        assert_eq!(config.quality_batch_size, 8);
        assert_eq!(config.quality_workers, 2);
    }

    #[test]
    fn layered_embed_config_falls_back_for_invalid_quality_numeric_values() {
        let config = LayeredEmbedConfig::from_values(LayeredEmbedConfigValues {
            quality_batch_size: Some("0"),
            quality_workers: Some("nope"),
            ..LayeredEmbedConfigValues::default()
        });

        assert_eq!(config.quality_batch_size, DEFAULT_QUALITY_EMBED_BATCH_SIZE);
        assert_eq!(config.quality_workers, DEFAULT_QUALITY_EMBED_WORKERS);
    }

    #[test]
    fn layered_embed_config_derives_fast_and_quality_embed_configs() {
        let config = LayeredEmbedConfig::from_values(LayeredEmbedConfigValues {
            ollama_url: Some("http://127.0.0.1:11434"),
            fast_model: Some("layer-fast"),
            quality_model: Some("layer-quality"),
            truncate: Some("false"),
            batch_size: Some("12"),
            quality_batch_size: Some("6"),
            max_chunk_bytes: Some("4096"),
            ..LayeredEmbedConfigValues::default()
        });

        let fast = config.fast_embed_config();
        let quality = config.quality_embed_config();

        assert_eq!(fast.ollama_url, "http://127.0.0.1:11434");
        assert_eq!(fast.model, "layer-fast");
        assert!(!fast.truncate);
        assert_eq!(fast.batch_size, 12);
        assert_eq!(fast.max_chunk_bytes, 4096);

        assert_eq!(quality.ollama_url, "http://127.0.0.1:11434");
        assert_eq!(quality.model, "layer-quality");
        assert!(!quality.truncate);
        assert_eq!(quality.batch_size, 6);
        assert_eq!(quality.max_chunk_bytes, 4096);
    }

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
    fn embed_batch_size_env_accepts_positive_integers() {
        assert_eq!(env_usize("1"), Some(1));
        assert_eq!(env_usize("16"), Some(16));
        assert_eq!(env_usize("0"), None);
        assert_eq!(env_usize("nope"), None);
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
    fn embed_batch_parts_preserve_count_order_and_prompt_total() {
        let batch = embedding_batch_from_parts(
            "nomic-embed-text".to_owned(),
            vec![vec![0.1, 0.2], vec![0.3, 0.4], vec![0.5, 0.6]],
            Some(9),
            3,
        )
        .expect("combined embeddings should validate");

        assert_eq!(batch.embeddings[0], vec![0.1, 0.2]);
        assert_eq!(batch.embeddings[2], vec![0.5, 0.6]);
        assert_eq!(batch.prompt_eval_count, Some(9));
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

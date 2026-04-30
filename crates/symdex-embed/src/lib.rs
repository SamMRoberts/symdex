//! Local embedding adapter boundary for Ollama.

use std::env;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbedConfig {
    pub ollama_url: String,
    pub model: String,
}

impl EmbedConfig {
    pub fn from_env() -> Self {
        Self {
            ollama_url: env_value("SYMDEX_OLLAMA_URL", "symdex_OLLAMA_URL")
                .unwrap_or_else(|| "http://localhost:11434".to_owned()),
            model: env_value("SYMDEX_EMBED_MODEL", "symdex_EMBED_MODEL")
                .unwrap_or_else(|| "nomic-embed-text".to_owned()),
        }
    }
}

fn env_value(upper: &str, legacy: &str) -> Option<String> {
    env::var(upper).ok().or_else(|| env::var(legacy).ok())
}

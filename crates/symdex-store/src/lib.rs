//! Persistence boundary for SQLite and Qdrant adapters.

use std::env;
use std::path::PathBuf;

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
                .unwrap_or_else(|| "http://localhost:6334".to_owned()),
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

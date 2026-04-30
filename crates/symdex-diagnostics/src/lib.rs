//! Local diagnostic checks shared by the CLI and TUI.

use std::env;
use std::path::{Path, PathBuf};
use std::time::Duration;

use symdex_embed::{EmbedConfig, OllamaClient};
use symdex_store::{QdrantClient, StoreConfig, sqlite_parent};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagnosticReport {
    pub workspace: String,
    pub sqlite_path: String,
    pub qdrant_url: String,
    pub ollama_url: String,
    pub embed_model: String,
    pub checks: Vec<DiagnosticCheck>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagnosticCheck {
    pub label: String,
    pub state: DiagnosticState,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosticState {
    Ok,
    Missing,
    Unreachable,
    Error,
    Skipped,
}

impl DiagnosticState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Missing => "missing",
            Self::Unreachable => "unreachable",
            Self::Error => "error",
            Self::Skipped => "skipped",
        }
    }
}

pub fn run_diagnostics() -> Result<DiagnosticReport, String> {
    let store = StoreConfig::from_env();
    let embed = EmbedConfig::from_env();
    let cwd = env::current_dir().map_err(|error| format!("read current directory: {error}"))?;

    let mut checks = Vec::new();
    checks.push(match sqlite_parent(&store) {
        Some(parent) => writable_dir_check("sqlite_parent", &parent),
        None => DiagnosticCheck {
            label: "sqlite_parent".to_owned(),
            state: DiagnosticState::Skipped,
            message: "database path has no parent".to_owned(),
        },
    });
    checks.extend(ollama_checks(&embed));
    checks.push(qdrant_check(&store));

    Ok(DiagnosticReport {
        workspace: cwd.display().to_string(),
        sqlite_path: store.sqlite_path.display().to_string(),
        qdrant_url: store.qdrant_url,
        ollama_url: embed.ollama_url,
        embed_model: embed.model,
        checks,
    })
}

fn writable_dir_check(label: &str, path: &Path) -> DiagnosticCheck {
    if path.exists() {
        if path.is_dir() {
            return DiagnosticCheck {
                label: label.to_owned(),
                state: DiagnosticState::Ok,
                message: path.display().to_string(),
            };
        }
        return DiagnosticCheck {
            label: label.to_owned(),
            state: DiagnosticState::Error,
            message: format!("not a directory ({})", path.display()),
        };
    }

    let display_path: PathBuf = path.to_path_buf();
    DiagnosticCheck {
        label: label.to_owned(),
        state: DiagnosticState::Missing,
        message: format!(
            "{} - run `symdex init` to create it",
            display_path.display()
        ),
    }
}

fn ollama_checks(config: &EmbedConfig) -> Vec<DiagnosticCheck> {
    let client = match OllamaClient::new(config.clone()) {
        Ok(client) => client,
        Err(error) => {
            return vec![DiagnosticCheck {
                label: "ollama_status".to_owned(),
                state: DiagnosticState::Error,
                message: error.to_string(),
            }];
        }
    };

    match client.model_available() {
        Ok(true) => {
            let mut checks = vec![DiagnosticCheck {
                label: "ollama_model".to_owned(),
                state: DiagnosticState::Ok,
                message: config.model.clone(),
            }];
            checks.push(match client.probe_dimension() {
                Ok(dimension) => DiagnosticCheck {
                    label: "embedding_dimension".to_owned(),
                    state: DiagnosticState::Ok,
                    message: dimension.to_string(),
                },
                Err(error) => DiagnosticCheck {
                    label: "embedding_dimension".to_owned(),
                    state: DiagnosticState::Unreachable,
                    message: error.to_string(),
                },
            });
            checks
        }
        Ok(false) => vec![DiagnosticCheck {
            label: "ollama_model".to_owned(),
            state: DiagnosticState::Missing,
            message: config.model.clone(),
        }],
        Err(error) => vec![DiagnosticCheck {
            label: "ollama_status".to_owned(),
            state: DiagnosticState::Unreachable,
            message: error.to_string(),
        }],
    }
}

fn qdrant_check(config: &StoreConfig) -> DiagnosticCheck {
    let client = match QdrantClient::with_timeout(config, Duration::from_secs(3)) {
        Ok(client) => client,
        Err(error) => {
            return DiagnosticCheck {
                label: "qdrant_status".to_owned(),
                state: DiagnosticState::Error,
                message: error.to_string(),
            };
        }
    };

    match client.health_check() {
        Ok(()) => DiagnosticCheck {
            label: "qdrant_status".to_owned(),
            state: DiagnosticState::Ok,
            message: String::new(),
        },
        Err(error) => DiagnosticCheck {
            label: "qdrant_status".to_owned(),
            state: DiagnosticState::Unreachable,
            message: error.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use crate::{DiagnosticState, writable_dir_check};

    #[test]
    fn writable_dir_check_reports_existing_directory() {
        let check = writable_dir_check("sqlite_parent", std::path::Path::new("."));

        assert_eq!(check.label, "sqlite_parent");
        assert_eq!(check.state, DiagnosticState::Ok);
    }

    #[test]
    fn writable_dir_check_reports_missing_directory() {
        let check = writable_dir_check(
            "sqlite_parent",
            std::path::Path::new("target/definitely-not-created-by-this-test"),
        );

        assert_eq!(check.state, DiagnosticState::Missing);
        assert!(check.message.contains("symdex init"));
    }
}

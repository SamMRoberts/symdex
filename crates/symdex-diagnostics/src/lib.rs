//! Local diagnostic checks shared by the CLI and TUI.

use std::env;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use symdex_core::{EVIDENCE_CONTRACT_SCHEMA, EVIDENCE_CONTRACT_VERSION};
use symdex_embed::{EmbedConfig, OllamaClient};
use symdex_query::FreshnessSummary;
use symdex_store::{EvidenceFreshness, SqliteVectorStore, StoreConfig, sqlite_parent};

const RUST_ANALYZER_ENABLE_ENV: &str = "SYMDEX_RUST_ANALYZER";
const RUST_ANALYZER_CMD_ENV: &str = "SYMDEX_RUST_ANALYZER_CMD";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagnosticReport {
    pub workspace: String,
    pub sqlite_path: String,
    pub vector_store: String,
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
    run_diagnostics_for_repo(None)
}

pub fn run_diagnostics_for_repo(repo: Option<&str>) -> Result<DiagnosticReport, String> {
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
    checks.push(sqlite_database_check(&store.sqlite_path));
    checks.extend(ollama_checks(&embed));
    checks.push(sqlite_vec_check(&store));
    checks.push(rust_analyzer_check(
        &RustAnalyzerDiagnosticsConfig::from_env(),
    ));
    checks.push(mcp_contract_check());
    checks.extend(cross_agent_repo_checks(repo));

    Ok(DiagnosticReport {
        workspace: cwd.display().to_string(),
        sqlite_path: store.sqlite_path.display().to_string(),
        vector_store: "sqlite_vec".to_owned(),
        ollama_url: embed.ollama_url,
        embed_model: embed.model,
        checks,
    })
}

fn sqlite_database_check(path: &Path) -> DiagnosticCheck {
    if path.exists() {
        if path.is_file() {
            return DiagnosticCheck {
                label: "sqlite_database".to_owned(),
                state: DiagnosticState::Ok,
                message: path.display().to_string(),
            };
        }
        return DiagnosticCheck {
            label: "sqlite_database".to_owned(),
            state: DiagnosticState::Error,
            message: format!("not a file ({})", path.display()),
        };
    }

    DiagnosticCheck {
        label: "sqlite_database".to_owned(),
        state: DiagnosticState::Missing,
        message: format!("{} - run `symdex init`", path.display()),
    }
}

fn mcp_contract_check() -> DiagnosticCheck {
    DiagnosticCheck {
        label: "mcp_evidence_contract".to_owned(),
        state: DiagnosticState::Ok,
        message: format!(
            "{EVIDENCE_CONTRACT_SCHEMA} version={EVIDENCE_CONTRACT_VERSION} local_only evidence_read_only watcher_start_explicit"
        ),
    }
}

fn cross_agent_repo_checks(repo: Option<&str>) -> Vec<DiagnosticCheck> {
    let Some(repo) = repo.map(str::trim).filter(|repo| !repo.is_empty()) else {
        return vec![
            DiagnosticCheck {
                label: "index_freshness".to_owned(),
                state: DiagnosticState::Skipped,
                message: "pass a repository path to check indexed evidence freshness".to_owned(),
            },
            DiagnosticCheck {
                label: "provenance_consistency".to_owned(),
                state: DiagnosticState::Skipped,
                message: "pass a repository path to check indexed evidence provenance".to_owned(),
            },
            DiagnosticCheck {
                label: "watcher_status".to_owned(),
                state: DiagnosticState::Skipped,
                message: "pass a repository path to check watcher status".to_owned(),
            },
        ];
    };

    match symdex_query::run_freshness_report(repo, None) {
        Ok(summary) => {
            let mut checks = vec![
                index_freshness_check(&summary),
                provenance_consistency_check(&summary),
            ];
            checks.push(watcher_status_check(repo));
            checks
        }
        Err(error) => {
            let mut checks = vec![
                DiagnosticCheck {
                    label: "index_freshness".to_owned(),
                    state: DiagnosticState::Error,
                    message: error.clone(),
                },
                DiagnosticCheck {
                    label: "provenance_consistency".to_owned(),
                    state: DiagnosticState::Error,
                    message: error,
                },
            ];
            checks.push(watcher_status_check(repo));
            checks
        }
    }
}

fn watcher_status_check(repo: &str) -> DiagnosticCheck {
    match symdex_watch::status(repo) {
        Ok(status) => {
            let state = match status.state.as_str() {
                "running" | "pending" | "indexing" | "starting" => DiagnosticState::Ok,
                "inactive" | "stopped" => DiagnosticState::Skipped,
                "stale" => DiagnosticState::Unreachable,
                "failed" => DiagnosticState::Error,
                _ => DiagnosticState::Skipped,
            };
            DiagnosticCheck {
                label: "watcher_status".to_owned(),
                state,
                message: format!(
                    "state={} owner={} clients={} client_kinds={} shutdown_after={} files_seen={} queued={} last={} error={}",
                    status.state,
                    status.owner_kind,
                    status.attached_clients,
                    if status.client_kinds.is_empty() {
                        "<none>".to_owned()
                    } else {
                        status.client_kinds.join(",")
                    },
                    status
                        .shutdown_after_seconds
                        .map(|seconds| seconds.to_string())
                        .unwrap_or_else(|| "<none>".to_owned()),
                    status.files_seen,
                    status.queued_events,
                    status.last_indexed_path.as_deref().unwrap_or("<none>"),
                    status.last_error.as_deref().unwrap_or("<none>")
                ),
            }
        }
        Err(error) => DiagnosticCheck {
            label: "watcher_status".to_owned(),
            state: DiagnosticState::Error,
            message: error,
        },
    }
}

fn index_freshness_check(summary: &FreshnessSummary) -> DiagnosticCheck {
    if summary.files.is_empty() {
        return DiagnosticCheck {
            label: "index_freshness".to_owned(),
            state: DiagnosticState::Missing,
            message: format!("{} has no indexed files", summary.repository_id),
        };
    }

    let fresh = summary.count(EvidenceFreshness::Fresh);
    let stale = summary.count(EvidenceFreshness::Stale);
    let deleted = summary.count(EvidenceFreshness::Deleted);
    let missing = summary.count(EvidenceFreshness::Missing);
    let unknown = summary.count(EvidenceFreshness::Unknown);
    let message = format!(
        "fresh={fresh} stale={stale} deleted={deleted} missing={missing} unknown={unknown}"
    );
    let state = if stale == 0 && deleted == 0 && missing == 0 && unknown == 0 {
        DiagnosticState::Ok
    } else {
        DiagnosticState::Error
    };

    DiagnosticCheck {
        label: "index_freshness".to_owned(),
        state,
        message,
    }
}

fn provenance_consistency_check(summary: &FreshnessSummary) -> DiagnosticCheck {
    let indexed_rows = summary
        .files
        .iter()
        .filter(|row| row.indexed_content_hash.is_some())
        .collect::<Vec<_>>();
    if indexed_rows.is_empty() {
        return DiagnosticCheck {
            label: "provenance_consistency".to_owned(),
            state: DiagnosticState::Missing,
            message: format!("{} has no indexed provenance rows", summary.repository_id),
        };
    }

    let incomplete = indexed_rows
        .iter()
        .filter(|row| {
            row.indexed_at.as_deref().unwrap_or_default().is_empty()
                || row.index_run_id.as_deref().unwrap_or_default().is_empty()
                || row.parser_version.as_deref().unwrap_or_default().is_empty()
        })
        .count();
    let state = if incomplete == 0 {
        DiagnosticState::Ok
    } else {
        DiagnosticState::Error
    };

    DiagnosticCheck {
        label: "provenance_consistency".to_owned(),
        state,
        message: format!(
            "indexed_rows={} incomplete_provenance={incomplete}",
            indexed_rows.len()
        ),
    }
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

fn sqlite_vec_check(config: &StoreConfig) -> DiagnosticCheck {
    let client = match SqliteVectorStore::new(config) {
        Ok(client) => client,
        Err(error) => {
            return DiagnosticCheck {
                label: "sqlite_vec_status".to_owned(),
                state: DiagnosticState::Error,
                message: error.to_string(),
            };
        }
    };

    match client.health_check() {
        Ok(version) => DiagnosticCheck {
            label: "sqlite_vec_status".to_owned(),
            state: DiagnosticState::Ok,
            message: version,
        },
        Err(error) => DiagnosticCheck {
            label: "sqlite_vec_status".to_owned(),
            state: DiagnosticState::Unreachable,
            message: error.to_string(),
        },
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RustAnalyzerDiagnosticsConfig {
    enabled: bool,
    command: String,
}

impl RustAnalyzerDiagnosticsConfig {
    fn from_env() -> Self {
        Self::from_values(
            env::var(RUST_ANALYZER_ENABLE_ENV).ok().as_deref(),
            env::var(RUST_ANALYZER_CMD_ENV).ok().as_deref(),
        )
    }

    fn from_values(enabled: Option<&str>, command: Option<&str>) -> Self {
        Self {
            enabled: enabled.is_some_and(env_flag_enabled),
            command: command
                .map(str::trim)
                .filter(|command| !command.is_empty())
                .unwrap_or("rust-analyzer")
                .to_owned(),
        }
    }
}

fn env_flag_enabled(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

fn rust_analyzer_check(config: &RustAnalyzerDiagnosticsConfig) -> DiagnosticCheck {
    if !config.enabled {
        return DiagnosticCheck {
            label: "rust_analyzer_enrichment".to_owned(),
            state: DiagnosticState::Skipped,
            message: format!(
                "set {RUST_ANALYZER_ENABLE_ENV}=1 to enable optional rust-analyzer readiness checks"
            ),
        };
    }

    match Command::new(&config.command).arg("--version").output() {
        Ok(output) if output.status.success() => DiagnosticCheck {
            label: "rust_analyzer_enrichment".to_owned(),
            state: DiagnosticState::Ok,
            message: rust_analyzer_version_message(&config.command, &output),
        },
        Ok(output) => DiagnosticCheck {
            label: "rust_analyzer_enrichment".to_owned(),
            state: DiagnosticState::Error,
            message: format!(
                "{} --version exited with {}",
                config.command,
                output
                    .status
                    .code()
                    .map_or_else(|| "signal".to_owned(), |code| format!("status {code}"))
            ),
        },
        Err(error) if error.kind() == io::ErrorKind::NotFound => DiagnosticCheck {
            label: "rust_analyzer_enrichment".to_owned(),
            state: DiagnosticState::Missing,
            message: format!(
                "{} not found; set {RUST_ANALYZER_CMD_ENV} to an installed rust-analyzer binary",
                config.command
            ),
        },
        Err(error) => DiagnosticCheck {
            label: "rust_analyzer_enrichment".to_owned(),
            state: DiagnosticState::Error,
            message: error.to_string(),
        },
    }
}

fn rust_analyzer_version_message(command: &str, output: &std::process::Output) -> String {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let version = if stdout.trim().is_empty() {
        stderr.trim()
    } else {
        stdout.trim()
    };
    if version.is_empty() {
        command.to_owned()
    } else {
        version.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use symdex_query::{FileFreshnessRow, FreshnessSummary};
    use symdex_store::EvidenceFreshness;

    use crate::{
        DiagnosticState, RustAnalyzerDiagnosticsConfig, env_flag_enabled, index_freshness_check,
        provenance_consistency_check, rust_analyzer_check, sqlite_database_check,
        writable_dir_check,
    };

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

    #[test]
    fn sqlite_database_check_reports_missing_file() {
        let check = sqlite_database_check(std::path::Path::new(
            "target/definitely-not-created-by-this-test/symdex.sqlite",
        ));

        assert_eq!(check.label, "sqlite_database");
        assert_eq!(check.state, DiagnosticState::Missing);
        assert!(check.message.contains("symdex init"));
    }

    #[test]
    fn index_freshness_check_flags_stale_or_missing_evidence() {
        let summary = freshness_summary(vec![
            freshness_row("src/fresh.rs", EvidenceFreshness::Fresh, true),
            freshness_row("src/stale.rs", EvidenceFreshness::Stale, true),
            freshness_row("src/new.rs", EvidenceFreshness::Missing, false),
        ]);

        let check = index_freshness_check(&summary);

        assert_eq!(check.state, DiagnosticState::Error);
        assert!(check.message.contains("fresh=1"));
        assert!(check.message.contains("stale=1"));
        assert!(check.message.contains("missing=1"));
    }

    #[test]
    fn provenance_consistency_check_flags_incomplete_indexed_rows() {
        let mut row = freshness_row("src/lib.rs", EvidenceFreshness::Fresh, true);
        row.parser_version = None;
        let summary = freshness_summary(vec![row]);

        let check = provenance_consistency_check(&summary);

        assert_eq!(check.state, DiagnosticState::Error);
        assert!(check.message.contains("incomplete_provenance=1"));
    }

    #[test]
    fn rust_analyzer_config_is_disabled_by_default_and_honors_command_override() {
        let default = RustAnalyzerDiagnosticsConfig::from_values(None, None);
        assert!(!default.enabled);
        assert_eq!(default.command, "rust-analyzer");

        let configured =
            RustAnalyzerDiagnosticsConfig::from_values(Some("yes"), Some("/bin/rust-analyzer"));
        assert!(configured.enabled);
        assert_eq!(configured.command, "/bin/rust-analyzer");
    }

    #[test]
    fn rust_analyzer_readiness_check_is_skipped_when_not_enabled() {
        let check = rust_analyzer_check(&RustAnalyzerDiagnosticsConfig {
            enabled: false,
            command: "definitely-not-run".to_owned(),
        });

        assert_eq!(check.label, "rust_analyzer_enrichment");
        assert_eq!(check.state, DiagnosticState::Skipped);
        assert!(check.message.contains("SYMDEX_RUST_ANALYZER"));
    }

    #[test]
    fn rust_analyzer_env_flag_accepts_explicit_truthy_values() {
        for value in ["1", "true", "TRUE", "yes", "on"] {
            assert!(env_flag_enabled(value), "{value} should enable the flag");
        }
        for value in ["", "0", "false", "off", "no"] {
            assert!(
                !env_flag_enabled(value),
                "{value} should not enable the flag"
            );
        }
    }

    fn freshness_summary(files: Vec<FileFreshnessRow>) -> FreshnessSummary {
        FreshnessSummary {
            repository_id: "repo".to_owned(),
            symbol_query: None,
            files,
            focus_symbols: Vec::new(),
            context_pack: None,
        }
    }

    fn freshness_row(path: &str, freshness: EvidenceFreshness, indexed: bool) -> FileFreshnessRow {
        FileFreshnessRow {
            path: path.to_owned(),
            freshness,
            indexed_content_hash: indexed.then(|| "indexed-hash".to_owned()),
            current_content_hash: Some("current-hash".to_owned()),
            indexed_at: indexed.then(|| "2026-05-01T00:00:00Z".to_owned()),
            index_run_id: indexed.then(|| "run-1".to_owned()),
            parser_version: indexed.then(|| "parser".to_owned()),
        }
    }
}

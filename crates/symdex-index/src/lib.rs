//! Indexing orchestration shared by the CLI and TUI.

use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs;
use std::io;
use std::process::Command;
use std::thread;
use std::time::Duration;

use symdex_core::{
    CallEdge, CodeChunk, DiscoveredTest, DiscoveryOptions, FileFacts, Language, ParseDiagnostic,
    RepoRoot, ResolutionStatus, Symbol, SymbolKind, discover_indexable_files, index_source_file,
};
use symdex_embed::{EmbedConfig, OllamaClient};
use symdex_store::{
    CallRecord, ChunkRecord, FileRecord, IndexRunRecord, PointPayload, QdrantClient,
    RepositoryRecord, SqliteStore, StoreConfig, SymbolRecord, TestRecord, VectorPoint,
    current_timestamp, qdrant_collection_name, qdrant_point_id,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexOptions {
    pub repo: String,
    pub offline: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContinuousIndexOptions {
    pub repo: String,
    pub offline: bool,
    pub poll_interval: Duration,
    pub debounce: Duration,
}

impl ContinuousIndexOptions {
    pub fn new(repo: impl Into<String>, offline: bool) -> Self {
        Self {
            repo: repo.into(),
            offline,
            poll_interval: Duration::from_millis(1_000),
            debounce: Duration::from_millis(250),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexProgress {
    pub phase: &'static str,
    pub completed: usize,
    pub total: usize,
    pub message: String,
}

impl IndexProgress {
    fn new(
        phase: &'static str,
        completed: usize,
        total: usize,
        message: impl Into<String>,
    ) -> Self {
        Self {
            phase,
            completed,
            total: total.max(1),
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct IndexSummary {
    pub repository_id: String,
    pub repository_root: String,
    pub files_seen: usize,
    pub files_skipped_unchanged: usize,
    pub files: Vec<FileIndexSummary>,
    pub chunks_seen: usize,
    pub chunks_excluded_from_embedding: usize,
    pub sqlite_files_indexed: usize,
    pub sqlite_chunks_indexed: usize,
    pub sqlite_symbols_indexed: usize,
    pub sqlite_calls_indexed: usize,
    pub sqlite_files_removed: usize,
    pub rust_analyzer: RustAnalyzerEnrichmentSummary,
    pub embedding: EmbeddingSummary,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileIndexSummary {
    pub path: String,
    pub language: String,
    pub content_hash: String,
    pub chunks: Vec<ChunkIndexSummary>,
    pub parse_diagnostics: Vec<ParseDiagnosticSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChunkIndexSummary {
    pub kind: String,
    pub start_line: usize,
    pub end_line: usize,
    pub symbol: Option<String>,
    pub excluded_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseDiagnosticSummary {
    pub start_line: usize,
    pub end_line: usize,
    pub start_byte: usize,
    pub end_byte: usize,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum EmbeddingSummary {
    SkippedOffline,
    SkippedNoChunks,
    Completed {
        model: String,
        dimension: usize,
        qdrant_collection: String,
        chunks_embedded: usize,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RustAnalyzerEnrichmentSummary {
    Disabled {
        enable_env: String,
    },
    NotReady {
        command: String,
        reason: String,
    },
    SkippedNoRustFiles {
        command: String,
        version: String,
    },
    Planned {
        command: String,
        version: String,
        eligible_files: usize,
        eligible_symbols: usize,
        eligible_calls: usize,
    },
}

const RUST_ANALYZER_ENABLE_ENV: &str = "SYMDEX_RUST_ANALYZER";
const RUST_ANALYZER_CMD_ENV: &str = "SYMDEX_RUST_ANALYZER_CMD";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchSnapshot {
    files: BTreeMap<String, String>,
}

impl WatchSnapshot {
    pub fn len(&self) -> usize {
        self.files.len()
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WatchChangeSet {
    pub created: Vec<String>,
    pub modified: Vec<String>,
    pub deleted: Vec<String>,
}

impl WatchChangeSet {
    pub fn is_empty(&self) -> bool {
        self.created.is_empty() && self.modified.is_empty() && self.deleted.is_empty()
    }

    pub fn event_count(&self) -> usize {
        self.created.len() + self.modified.len() + self.deleted.len()
    }

    pub fn paths(&self) -> Vec<&str> {
        self.created
            .iter()
            .chain(self.modified.iter())
            .chain(self.deleted.iter())
            .map(String::as_str)
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ContinuousIndexEvent {
    Started {
        repository_id: String,
        files_seen: usize,
    },
    Idle {
        files_seen: usize,
    },
    ChangesPending {
        changes: WatchChangeSet,
    },
    ChangesDetected {
        changes: WatchChangeSet,
    },
    BatchCompleted {
        changes: WatchChangeSet,
        summary: Box<IndexSummary>,
    },
    BatchFailed {
        changes: WatchChangeSet,
        error: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PersistenceSummary {
    files_indexed: usize,
    chunks_indexed: usize,
    symbols_indexed: usize,
    calls_indexed: usize,
    files_removed: usize,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct RunCounts {
    files_seen: usize,
    files_indexed: usize,
    chunks_embedded: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RunScope<'a> {
    index_run_id: &'a str,
    repository_id: &'a str,
    embedding_model: &'a str,
    run_kind: &'a str,
}

pub fn run_index(options: &IndexOptions) -> Result<IndexSummary, String> {
    run_index_with_progress(options, |_| {})
}

pub fn run_incremental_index(options: &IndexOptions) -> Result<IndexSummary, String> {
    run_index_internal(options, true, |_| {})
}

pub fn run_continuous_index(
    options: &ContinuousIndexOptions,
    mut on_event: impl FnMut(ContinuousIndexEvent),
) -> Result<(), String> {
    run_continuous_index_until(options, &mut on_event, || true)
}

pub fn run_continuous_index_until(
    options: &ContinuousIndexOptions,
    mut on_event: impl FnMut(ContinuousIndexEvent),
    mut should_continue: impl FnMut() -> bool,
) -> Result<(), String> {
    let root = RepoRoot::open(&options.repo).map_err(|error| error.to_string())?;
    let mut snapshot = watch_snapshot(&root)?;
    on_event(ContinuousIndexEvent::Started {
        repository_id: root.id().to_owned(),
        files_seen: snapshot.len(),
    });

    while should_continue() {
        thread::sleep(options.poll_interval);
        if !should_continue() {
            break;
        }
        let (next_snapshot, first_changes) = detect_watch_changes(&root, &snapshot)?;
        if first_changes.is_empty() {
            snapshot = next_snapshot;
            on_event(ContinuousIndexEvent::Idle {
                files_seen: snapshot.len(),
            });
            continue;
        }

        on_event(ContinuousIndexEvent::ChangesPending {
            changes: first_changes.clone(),
        });
        thread::sleep(options.debounce);
        if !should_continue() {
            break;
        }
        let (debounced_snapshot, changes) = detect_watch_changes(&root, &snapshot)?;
        let changes = if changes.is_empty() {
            first_changes
        } else {
            changes
        };
        on_event(ContinuousIndexEvent::ChangesDetected {
            changes: changes.clone(),
        });

        match run_incremental_index(&IndexOptions {
            repo: options.repo.clone(),
            offline: options.offline,
        }) {
            Ok(summary) => {
                snapshot = watch_snapshot(&root).unwrap_or(debounced_snapshot);
                on_event(ContinuousIndexEvent::BatchCompleted {
                    changes,
                    summary: Box::new(summary),
                });
            }
            Err(error) => {
                snapshot = debounced_snapshot;
                on_event(ContinuousIndexEvent::BatchFailed { changes, error });
            }
        }
    }
    Ok(())
}

pub fn watch_snapshot(root: &RepoRoot) -> Result<WatchSnapshot, String> {
    let files = discover_indexable_files(root, &DiscoveryOptions::default())
        .map_err(|error| error.to_string())?
        .into_iter()
        .map(|file| (file.facts.relative_path, file.facts.content_hash))
        .collect();
    Ok(WatchSnapshot { files })
}

pub fn detect_watch_changes(
    root: &RepoRoot,
    previous: &WatchSnapshot,
) -> Result<(WatchSnapshot, WatchChangeSet), String> {
    let next = watch_snapshot(root)?;
    Ok((next.clone(), diff_watch_snapshots(previous, &next)))
}

pub fn diff_watch_snapshots(previous: &WatchSnapshot, next: &WatchSnapshot) -> WatchChangeSet {
    let mut changes = WatchChangeSet::default();
    for (path, hash) in &next.files {
        match previous.files.get(path) {
            None => changes.created.push(path.clone()),
            Some(previous_hash) if previous_hash != hash => changes.modified.push(path.clone()),
            Some(_) => {}
        }
    }
    for path in previous.files.keys() {
        if !next.files.contains_key(path) {
            changes.deleted.push(path.clone());
        }
    }
    changes
}

pub fn run_index_with_progress(
    options: &IndexOptions,
    mut on_progress: impl FnMut(IndexProgress),
) -> Result<IndexSummary, String> {
    run_index_internal(options, options.offline, &mut on_progress)
}

fn run_index_internal(
    options: &IndexOptions,
    skip_unchanged: bool,
    mut on_progress: impl FnMut(IndexProgress),
) -> Result<IndexSummary, String> {
    on_progress(IndexProgress::new(
        "open",
        0,
        1,
        "Opening repository and local stores",
    ));
    let root = RepoRoot::open(&options.repo).map_err(|error| error.to_string())?;
    let store_config = StoreConfig::from_env();
    let mut sqlite = SqliteStore::open(&store_config).map_err(|error| error.to_string())?;
    sqlite.migrate().map_err(|error| error.to_string())?;
    sqlite
        .upsert_repository(&RepositoryRecord {
            id: root.id().to_owned(),
            root_path: root.path().display().to_string(),
        })
        .map_err(|error| error.to_string())?;
    on_progress(IndexProgress::new(
        "open",
        1,
        1,
        "Repository and SQLite store ready",
    ));

    let run_kind = if options.offline {
        "offline"
    } else {
        "semantic"
    };
    let index_run_id = SqliteStore::new_index_run_id(root.id(), run_kind);
    let embedding_model = if options.offline {
        "offline".to_owned()
    } else {
        EmbedConfig::from_env().model
    };
    let run_scope = RunScope {
        index_run_id: &index_run_id,
        repository_id: root.id(),
        embedding_model: &embedding_model,
        run_kind,
    };
    sqlite
        .start_index_run(&index_run_record(
            &run_scope,
            "running",
            None,
            RunCounts::default(),
            None,
            "pending",
        ))
        .map_err(|error| error.to_string())?;

    let mut collection = match collect_index_reports(
        &root,
        if skip_unchanged { Some(&sqlite) } else { None },
        &mut on_progress,
    ) {
        Ok(collection) => collection,
        Err(error) => {
            finish_failed_index_run(
                &sqlite,
                &run_scope,
                "unknown",
                RunCounts::default(),
                "failed",
                &error,
            )?;
            return Err(error);
        }
    };
    let persisted_rust_symbols = match sqlite.rust_symbols_for_repository(root.id()) {
        Ok(symbols) => symbols,
        Err(error) => {
            let error = error.to_string();
            finish_failed_index_run(
                &sqlite,
                &run_scope,
                &parser_version_summary(&collection),
                RunCounts {
                    files_seen: collection.files_seen,
                    files_indexed: 0,
                    chunks_embedded: 0,
                },
                "failed",
                &error,
            )?;
            return Err(error);
        }
    };
    resolve_cross_file_rust_calls(&mut collection, &persisted_rust_symbols);
    let files = file_summaries(&collection.reports);
    let rust_analyzer =
        rust_analyzer_enrichment_summary(&collection, &RustAnalyzerEnrichmentConfig::from_env());
    let chunks_seen = collection
        .reports
        .iter()
        .map(|report| report.chunks.len())
        .sum();
    let chunks_excluded_from_embedding = collection
        .reports
        .iter()
        .flat_map(|report| report.chunks.iter())
        .filter(|chunk| chunk.excluded_reason.is_some())
        .count();
    if !options.offline
        && let Err(error) = delete_stale_qdrant_points(
            &sqlite,
            &root,
            &store_config,
            &collection,
            &embedding_model,
            &mut on_progress,
        )
    {
        finish_failed_index_run(
            &sqlite,
            &run_scope,
            &parser_version_summary(&collection),
            RunCounts {
                files_seen: collection.files_seen,
                files_indexed: 0,
                chunks_embedded: 0,
            },
            "failed",
            &error,
        )?;
        return Err(error);
    }
    let persistence = match persist_structural_index(
        &mut sqlite,
        &root,
        &collection,
        &index_run_id,
        &mut on_progress,
    ) {
        Ok(persistence) => persistence,
        Err(error) => {
            finish_failed_index_run(
                &sqlite,
                &run_scope,
                &parser_version_summary(&collection),
                RunCounts {
                    files_seen: collection.files_seen,
                    files_indexed: 0,
                    chunks_embedded: 0,
                },
                "failed",
                &error,
            )?;
            return Err(error);
        }
    };

    let embedding = if options.offline {
        on_progress(IndexProgress::new(
            "embedding",
            1,
            1,
            "Embedding skipped for offline indexing",
        ));
        sqlite
            .finish_index_run(&index_run_record(
                &run_scope,
                "success",
                None,
                RunCounts {
                    files_seen: collection.files_seen,
                    files_indexed: persistence.files_indexed,
                    chunks_embedded: 0,
                },
                None,
                &parser_version_summary(&collection),
            ))
            .map_err(|error| error.to_string())?;
        EmbeddingSummary::SkippedOffline
    } else {
        match persist_semantic_index(
            &sqlite,
            &root,
            &store_config,
            &collection,
            &index_run_id,
            &mut on_progress,
        ) {
            Ok(embedding) => {
                let (status, dimension, chunks_embedded) = match &embedding {
                    EmbeddingSummary::Completed {
                        dimension,
                        chunks_embedded,
                        ..
                    } => ("success", Some(*dimension), *chunks_embedded),
                    EmbeddingSummary::SkippedNoChunks => ("skipped", None, 0),
                    EmbeddingSummary::SkippedOffline => ("success", None, 0),
                };
                sqlite
                    .finish_index_run(&index_run_record(
                        &run_scope,
                        status,
                        dimension,
                        RunCounts {
                            files_seen: collection.files_seen,
                            files_indexed: persistence.files_indexed,
                            chunks_embedded,
                        },
                        None,
                        &parser_version_summary(&collection),
                    ))
                    .map_err(|error| error.to_string())?;
                embedding
            }
            Err(error) => {
                finish_failed_index_run(
                    &sqlite,
                    &run_scope,
                    &parser_version_summary(&collection),
                    RunCounts {
                        files_seen: collection.files_seen,
                        files_indexed: persistence.files_indexed,
                        chunks_embedded: 0,
                    },
                    "partial",
                    &error,
                )?;
                return Err(error);
            }
        }
    };

    Ok(IndexSummary {
        repository_id: root.id().to_owned(),
        repository_root: root.path().display().to_string(),
        files_seen: collection.files_seen,
        files_skipped_unchanged: collection.files_skipped_unchanged,
        files,
        chunks_seen,
        chunks_excluded_from_embedding,
        sqlite_files_indexed: persistence.files_indexed,
        sqlite_chunks_indexed: persistence.chunks_indexed,
        sqlite_symbols_indexed: persistence.symbols_indexed,
        sqlite_calls_indexed: persistence.calls_indexed,
        sqlite_files_removed: persistence.files_removed,
        rust_analyzer,
        embedding,
    })
}

fn collect_index_reports(
    root: &RepoRoot,
    sqlite: Option<&SqliteStore>,
    on_progress: &mut impl FnMut(IndexProgress),
) -> Result<IndexCollection, String> {
    let files = discover_indexable_files(root, &DiscoveryOptions::default())
        .map_err(|error| error.to_string())?;
    on_progress(IndexProgress::new(
        "discover",
        0,
        files.len(),
        format!("Discovered {} indexable files", files.len()),
    ));

    let mut reports = Vec::new();
    let mut files_skipped_unchanged = 0usize;
    for (index, file) in files.iter().enumerate() {
        if let Some(sqlite) = sqlite
            && sqlite
                .file_unchanged(
                    root.id(),
                    &file.facts.relative_path,
                    &file.facts.content_hash,
                    file.facts.language.parser_version(),
                )
                .map_err(|error| error.to_string())?
        {
            files_skipped_unchanged += 1;
            on_progress(IndexProgress::new(
                "parse",
                index + 1,
                files.len(),
                format!("Skipped unchanged {}", file.facts.relative_path),
            ));
            continue;
        }

        let source = fs::read_to_string(&file.absolute_path)
            .map_err(|error| format!("read {}: {error}", file.absolute_path.display()))?;
        let file_index =
            index_source_file(&file.facts, &source).map_err(|error| error.to_string())?;
        let parse_diagnostic_count = file_index.parse_diagnostics.len();
        reports.push(IndexReport {
            file: file.facts.clone(),
            chunks: file_index.chunks,
            symbols: file_index.symbols,
            calls: file_index.calls,
            tests: file_index.tests,
            parse_diagnostics: file_index.parse_diagnostics,
            source,
        });
        let message = if parse_diagnostic_count == 0 {
            format!("Parsed {}", file.facts.relative_path)
        } else {
            format!(
                "Parsed {} with {parse_diagnostic_count} diagnostics",
                file.facts.relative_path
            )
        };
        on_progress(IndexProgress::new("parse", index + 1, files.len(), message));
    }
    Ok(IndexCollection {
        files_seen: files.len(),
        files_skipped_unchanged,
        parser_versions: files
            .iter()
            .map(|file| file.facts.language.parser_version().to_owned())
            .collect(),
        active_paths: files
            .iter()
            .map(|file| file.facts.relative_path.clone())
            .collect(),
        reports,
    })
}

fn file_summaries(reports: &[IndexReport]) -> Vec<FileIndexSummary> {
    reports
        .iter()
        .map(|report| FileIndexSummary {
            path: report.file.relative_path.clone(),
            language: report.file.language.as_str().to_owned(),
            content_hash: report.file.content_hash.clone(),
            chunks: report
                .chunks
                .iter()
                .map(|chunk| ChunkIndexSummary {
                    kind: chunk.kind.as_str().to_owned(),
                    start_line: chunk.line_range.start,
                    end_line: chunk.line_range.end,
                    symbol: chunk.symbol_name.clone(),
                    excluded_reason: chunk.excluded_reason.clone(),
                })
                .collect(),
            parse_diagnostics: report
                .parse_diagnostics
                .iter()
                .map(parse_diagnostic_summary)
                .collect(),
        })
        .collect()
}

fn parse_diagnostic_summary(diagnostic: &ParseDiagnostic) -> ParseDiagnosticSummary {
    ParseDiagnosticSummary {
        start_line: diagnostic.line_range.start,
        end_line: diagnostic.line_range.end,
        start_byte: diagnostic.byte_range.start,
        end_byte: diagnostic.byte_range.end,
        message: diagnostic.message.clone(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RustAnalyzerEnrichmentConfig {
    enabled: bool,
    command: String,
}

impl RustAnalyzerEnrichmentConfig {
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

#[derive(Debug, Clone, PartialEq, Eq)]
enum RustAnalyzerReadiness {
    Disabled,
    Ready { version: String },
    NotReady { reason: String },
}

fn env_flag_enabled(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

fn rust_analyzer_enrichment_summary(
    collection: &IndexCollection,
    config: &RustAnalyzerEnrichmentConfig,
) -> RustAnalyzerEnrichmentSummary {
    let readiness = rust_analyzer_readiness(config);
    plan_rust_analyzer_enrichment(collection, config, readiness)
}

fn rust_analyzer_readiness(config: &RustAnalyzerEnrichmentConfig) -> RustAnalyzerReadiness {
    if !config.enabled {
        return RustAnalyzerReadiness::Disabled;
    }

    match Command::new(&config.command).arg("--version").output() {
        Ok(output) if output.status.success() => RustAnalyzerReadiness::Ready {
            version: rust_analyzer_version_message(&config.command, &output),
        },
        Ok(output) => RustAnalyzerReadiness::NotReady {
            reason: format!(
                "{} --version exited with {}",
                config.command,
                output
                    .status
                    .code()
                    .map_or_else(|| "signal".to_owned(), |code| format!("status {code}"))
            ),
        },
        Err(error) if error.kind() == io::ErrorKind::NotFound => RustAnalyzerReadiness::NotReady {
            reason: format!(
                "{} not found; set {RUST_ANALYZER_CMD_ENV} to an installed rust-analyzer binary",
                config.command
            ),
        },
        Err(error) => RustAnalyzerReadiness::NotReady {
            reason: error.to_string(),
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

fn plan_rust_analyzer_enrichment(
    collection: &IndexCollection,
    config: &RustAnalyzerEnrichmentConfig,
    readiness: RustAnalyzerReadiness,
) -> RustAnalyzerEnrichmentSummary {
    match readiness {
        RustAnalyzerReadiness::Disabled => RustAnalyzerEnrichmentSummary::Disabled {
            enable_env: RUST_ANALYZER_ENABLE_ENV.to_owned(),
        },
        RustAnalyzerReadiness::NotReady { reason } => RustAnalyzerEnrichmentSummary::NotReady {
            command: config.command.clone(),
            reason,
        },
        RustAnalyzerReadiness::Ready { version } => {
            let rust_reports = collection
                .reports
                .iter()
                .filter(|report| report.file.language == Language::Rust)
                .collect::<Vec<_>>();
            if rust_reports.is_empty() {
                return RustAnalyzerEnrichmentSummary::SkippedNoRustFiles {
                    command: config.command.clone(),
                    version,
                };
            }

            RustAnalyzerEnrichmentSummary::Planned {
                command: config.command.clone(),
                version,
                eligible_files: rust_reports.len(),
                eligible_symbols: rust_reports.iter().map(|report| report.symbols.len()).sum(),
                eligible_calls: rust_reports.iter().map(|report| report.calls.len()).sum(),
            }
        }
    }
}

fn resolve_cross_file_rust_calls(
    collection: &mut IndexCollection,
    persisted_rust_symbols: &[SymbolRecord],
) {
    let replaced_file_ids = collection
        .reports
        .iter()
        .map(|report| report.file.id.as_str())
        .collect::<BTreeSet<_>>();
    let mut symbols = collection
        .reports
        .iter()
        .filter(|report| report.file.language == Language::Rust)
        .flat_map(|report| report.symbols.iter().map(ResolutionSymbol::from_symbol))
        .collect::<Vec<_>>();
    symbols.extend(
        persisted_rust_symbols
            .iter()
            .filter(|symbol| !replaced_file_ids.contains(symbol.file_id.as_str()))
            .map(ResolutionSymbol::from_record),
    );

    if symbols.is_empty() {
        return;
    }

    for report in collection
        .reports
        .iter_mut()
        .filter(|report| report.file.language == Language::Rust)
    {
        for call in &mut report.calls {
            if call.resolution_status != ResolutionStatus::Unresolved {
                continue;
            }
            let caller = symbols
                .iter()
                .find(|symbol| symbol.id == call.caller_symbol_id);
            if let Some((callee_symbol_id, resolution_status, confidence)) =
                resolve_cross_file_rust_callee(&call.callee_text, caller, &symbols)
            {
                call.callee_symbol_id = callee_symbol_id;
                call.resolution_status = resolution_status;
                call.confidence = confidence;
            }
        }
    }
}

fn resolve_cross_file_rust_callee(
    callee_text: &str,
    caller: Option<&ResolutionSymbol>,
    symbols: &[ResolutionSymbol],
) -> Option<(Option<String>, ResolutionStatus, f32)> {
    let candidates = cross_file_rust_callee_candidates(callee_text, caller);
    if candidates.is_empty() {
        return None;
    }

    let exact = symbols
        .iter()
        .filter(|symbol| candidates.contains(&symbol.qualified_name))
        .collect::<Vec<_>>();
    match exact.as_slice() {
        [symbol] => {
            return Some((
                Some(symbol.id.clone()),
                ResolutionStatus::ResolvedExact,
                1.0,
            ));
        }
        [] => {}
        _ => return Some((None, ResolutionStatus::Ambiguous, 0.2)),
    }

    if cross_file_requires_exact_rust_resolution(callee_text, caller) {
        return Some((None, ResolutionStatus::Unresolved, 0.25));
    }

    let suffix_matches = symbols
        .iter()
        .filter(|symbol| {
            candidates
                .iter()
                .any(|candidate| symbol.qualified_name.ends_with(candidate))
        })
        .collect::<Vec<_>>();
    match suffix_matches.as_slice() {
        [symbol] => Some((
            Some(symbol.id.clone()),
            ResolutionStatus::ResolvedLocalCandidate,
            0.65,
        )),
        [] => None,
        _ => Some((None, ResolutionStatus::Ambiguous, 0.2)),
    }
}

fn cross_file_rust_callee_candidates(
    callee_text: &str,
    caller: Option<&ResolutionSymbol>,
) -> BTreeSet<String> {
    let normalized = normalize_rust_module_path(callee_text);
    if normalized.is_empty() || normalized.ends_with('!') {
        return BTreeSet::new();
    }

    let module_scoped_candidate =
        caller.and_then(|caller| cross_file_module_scoped_path_candidate(callee_text, caller));
    let module_unqualified_candidate =
        caller.and_then(|caller| cross_file_module_unqualified_path_candidate(callee_text, caller));
    let module_candidates = caller
        .into_iter()
        .flat_map(|caller| cross_file_module_relative_candidates(callee_text, caller))
        .collect::<Vec<_>>();
    let suppress_plain_candidate = module_scoped_candidate.is_some()
        || module_unqualified_candidate.is_some()
        || (!module_candidates.is_empty() && is_cross_file_module_relative_path(callee_text));

    let mut candidates = BTreeSet::new();
    if !suppress_plain_candidate && normalized.contains("::") {
        candidates.insert(callee_text.to_owned());
        candidates.insert(normalized);
    }
    candidates.extend(module_candidates);
    if let Some(module_scoped_candidate) = module_scoped_candidate {
        candidates.insert(module_scoped_candidate);
    }
    if let Some(module_unqualified_candidate) = module_unqualified_candidate {
        candidates.insert(module_unqualified_candidate);
    }
    candidates
}

fn cross_file_requires_exact_rust_resolution(
    callee_text: &str,
    caller: Option<&ResolutionSymbol>,
) -> bool {
    let Some(caller) = caller else {
        return false;
    };
    !cross_file_module_relative_candidates(callee_text, caller).is_empty()
        || cross_file_module_scoped_path_candidate(callee_text, caller).is_some()
        || cross_file_module_unqualified_path_candidate(callee_text, caller).is_some()
}

fn is_cross_file_module_relative_path(callee_text: &str) -> bool {
    callee_text.starts_with("self::") || callee_text.starts_with("super::")
}

fn cross_file_module_relative_candidates(
    callee_text: &str,
    caller: &ResolutionSymbol,
) -> Vec<String> {
    let Some(module_path) = caller.module_path() else {
        return Vec::new();
    };
    let Some((prefix, tail)) = callee_text.split_once("::") else {
        return Vec::new();
    };
    match prefix {
        "self" => join_cross_file_module_path(&module_path, tail)
            .into_iter()
            .collect(),
        "super" => cross_file_module_super_path(&module_path)
            .and_then(|module| join_cross_file_module_path(&module, tail))
            .into_iter()
            .collect(),
        _ => Vec::new(),
    }
}

fn cross_file_module_scoped_path_candidate(
    callee_text: &str,
    caller: &ResolutionSymbol,
) -> Option<String> {
    let (head, _) = callee_text.split_once("::")?;
    if matches!(head, "crate" | "self" | "super" | "Self") {
        return None;
    }
    let module_path = caller.module_path()?;
    if module_path.is_empty() {
        return None;
    }
    join_cross_file_module_path(&module_path, callee_text)
}

fn cross_file_module_unqualified_path_candidate(
    callee_text: &str,
    caller: &ResolutionSymbol,
) -> Option<String> {
    if callee_text.contains("::") || callee_text.contains('.') || callee_text.ends_with('!') {
        return None;
    }
    let module_path = caller.module_path()?;
    if module_path.is_empty() {
        return None;
    }
    join_cross_file_module_path(&module_path, callee_text)
}

fn cross_file_module_super_path(module_path: &str) -> Option<String> {
    if module_path.is_empty() {
        return None;
    }
    module_path
        .rsplit_once("::")
        .map(|(parent, _)| parent.to_owned())
        .or_else(|| Some(String::new()))
}

fn join_cross_file_module_path(module_path: &str, tail: &str) -> Option<String> {
    let tail = normalize_rust_module_path(tail);
    if tail.is_empty() {
        return None;
    }
    if module_path.is_empty() {
        Some(tail)
    } else {
        Some(format!("{module_path}::{tail}"))
    }
}

fn normalize_rust_module_path(path: &str) -> String {
    let mut parts = path
        .split("::")
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    while matches!(parts.first(), Some(&"crate" | &"self" | &"super")) {
        parts.remove(0);
    }
    parts.join("::")
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ResolutionSymbol {
    id: String,
    file_id: String,
    qualified_name: String,
    is_method: bool,
}

impl ResolutionSymbol {
    fn from_symbol(symbol: &Symbol) -> Self {
        Self {
            id: symbol.id.clone(),
            file_id: symbol.file_id.clone(),
            qualified_name: symbol.qualified_name.clone(),
            is_method: symbol.kind == SymbolKind::Method,
        }
    }

    fn from_record(symbol: &SymbolRecord) -> Self {
        Self {
            id: symbol.id.clone(),
            file_id: symbol.file_id.clone(),
            qualified_name: symbol.qualified_name.clone(),
            is_method: symbol.kind == SymbolKind::Method.as_str(),
        }
    }

    fn module_path(&self) -> Option<String> {
        let (container, _) = self.qualified_name.rsplit_once("::")?;
        let module = if self.is_method {
            self.receiver_type()
                .and_then(|receiver| container.strip_suffix(receiver))
                .map(|module| module.trim_end_matches("::"))
                .unwrap_or("")
        } else {
            container
        };
        Some(module.to_owned())
    }

    fn receiver_type(&self) -> Option<&str> {
        if !self.is_method {
            return None;
        }
        self.qualified_name
            .rsplit_once("::")
            .map(|(receiver, _)| receiver)
    }
}

fn persist_structural_index(
    sqlite: &mut SqliteStore,
    root: &RepoRoot,
    collection: &IndexCollection,
    index_run_id: &str,
    on_progress: &mut impl FnMut(IndexProgress),
) -> Result<PersistenceSummary, String> {
    let mut chunks_indexed = 0usize;
    let mut symbols_indexed = 0usize;
    let mut calls_indexed = 0usize;
    for (index, report) in collection.reports.iter().enumerate() {
        let file = FileRecord {
            id: report.file.id.clone(),
            repository_id: root.id().to_owned(),
            path: report.file.relative_path.clone(),
            language: report.file.language.as_str().to_owned(),
            content_hash: report.file.content_hash.clone(),
            index_run_id: index_run_id.to_owned(),
            parser_version: report.file.language.parser_version().to_owned(),
        };
        let chunks = report
            .chunks
            .iter()
            .map(|chunk| chunk_record(chunk, index_run_id, report.file.language.parser_version()))
            .collect::<Result<Vec<_>, _>>()?;
        let symbols = report
            .symbols
            .iter()
            .map(|symbol| {
                symbol_record(symbol, index_run_id, report.file.language.parser_version())
            })
            .collect::<Vec<_>>();
        let calls = report
            .calls
            .iter()
            .map(|call| call_record(call, index_run_id, report.file.language.parser_version()))
            .collect::<Vec<_>>();
        let tests = report
            .tests
            .iter()
            .map(|test| {
                test_record(
                    root.id(),
                    test,
                    index_run_id,
                    report.file.language.parser_version(),
                )
            })
            .collect::<Vec<_>>();
        chunks_indexed += chunks.len();
        symbols_indexed += symbols.len();
        calls_indexed += calls.len();
        sqlite
            .replace_file_facts_with_tests(&file, &symbols, &chunks, &calls, &tests)
            .map_err(|error| error.to_string())?;
        on_progress(IndexProgress::new(
            "sqlite",
            index + 1,
            collection.reports.len(),
            format!("Persisted {}", report.file.relative_path),
        ));
    }

    let files_removed = sqlite
        .remove_missing_files(root.id(), &collection.active_paths)
        .map_err(|error| error.to_string())?;
    on_progress(IndexProgress::new(
        "sqlite",
        collection.reports.len(),
        collection.reports.len(),
        format!("Removed {files_removed} stale files"),
    ));
    Ok(PersistenceSummary {
        files_indexed: collection.reports.len(),
        chunks_indexed,
        symbols_indexed,
        calls_indexed,
        files_removed,
    })
}

fn persist_semantic_index(
    sqlite: &SqliteStore,
    root: &RepoRoot,
    store_config: &StoreConfig,
    collection: &IndexCollection,
    index_run_id: &str,
    on_progress: &mut impl FnMut(IndexProgress),
) -> Result<EmbeddingSummary, String> {
    let embed_config = EmbedConfig::from_env();
    let chunk_texts = chunk_texts(&collection.reports);
    if chunk_texts.is_empty() {
        on_progress(IndexProgress::new(
            "embedding",
            1,
            1,
            "Embedding skipped because no chunks changed",
        ));
        return Ok(EmbeddingSummary::SkippedNoChunks);
    }

    on_progress(IndexProgress::new(
        "embedding",
        1,
        5,
        format!("Preparing {} chunks for embedding", chunk_texts.len()),
    ));
    let embed_client =
        OllamaClient::new(embed_config.clone()).map_err(|error| error.to_string())?;
    if !embed_client
        .model_available()
        .map_err(|error| error.to_string())?
    {
        return Err(format!(
            "embedding model `{}` is not available",
            embed_config.model
        ));
    }
    on_progress(IndexProgress::new(
        "embedding",
        2,
        5,
        format!("Embedding {} chunks", chunk_texts.len()),
    ));

    let embeddings = embed_client
        .embed_batch(
            &chunk_texts
                .iter()
                .map(|chunk| chunk.text.clone())
                .collect::<Vec<_>>(),
        )
        .map_err(|error| error.to_string())?;
    let dimension = embeddings.dimension().unwrap_or(0);
    on_progress(IndexProgress::new(
        "embedding",
        3,
        5,
        format!("Embedding dimension {dimension}"),
    ));
    sqlite
        .ensure_embedding_compatible(root.id(), &embed_config.model, dimension)
        .map_err(|error| error.to_string())?;

    let qdrant = QdrantClient::new(store_config).map_err(|error| error.to_string())?;
    let qdrant_collection = qdrant_collection_name(root.id(), &embed_config.model);
    qdrant
        .ensure_collection(&qdrant_collection, dimension)
        .map_err(|error| error.to_string())?;
    on_progress(IndexProgress::new(
        "qdrant",
        4,
        5,
        format!("Upserting {} vector points", chunk_texts.len()),
    ));

    let points = chunk_texts
        .iter()
        .zip(embeddings.embeddings)
        .map(|(chunk, vector)| {
            vector_point(
                root.id(),
                chunk,
                vector,
                index_run_id,
                &embed_config.model,
                dimension,
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    qdrant
        .upsert_points(&qdrant_collection, &points)
        .map_err(|error| error.to_string())?;
    sqlite
        .record_chunk_embedding_provenance(
            &chunk_texts
                .iter()
                .map(|chunk| chunk.chunk.id.clone())
                .collect::<Vec<_>>(),
            &embed_config.model,
            dimension,
        )
        .map_err(|error| error.to_string())?;
    on_progress(IndexProgress::new(
        "qdrant",
        5,
        5,
        format!("Upserted {} vector points", points.len()),
    ));
    Ok(EmbeddingSummary::Completed {
        model: embed_config.model,
        dimension,
        qdrant_collection,
        chunks_embedded: points.len(),
    })
}

fn delete_stale_qdrant_points(
    sqlite: &SqliteStore,
    root: &RepoRoot,
    store_config: &StoreConfig,
    collection: &IndexCollection,
    embedding_model: &str,
    on_progress: &mut impl FnMut(IndexProgress),
) -> Result<usize, String> {
    let changed_paths = collection
        .reports
        .iter()
        .map(|report| report.file.relative_path.clone())
        .collect::<Vec<_>>();
    let mut point_ids = BTreeSet::new();
    point_ids.extend(
        sqlite
            .qdrant_point_ids_for_paths(root.id(), &changed_paths)
            .map_err(|error| error.to_string())?,
    );
    point_ids.extend(
        sqlite
            .qdrant_point_ids_for_missing_files(root.id(), &collection.active_paths)
            .map_err(|error| error.to_string())?,
    );

    if point_ids.is_empty() {
        on_progress(IndexProgress::new(
            "qdrant_cleanup",
            1,
            1,
            "No stale Qdrant points to delete",
        ));
        return Ok(0);
    }

    let qdrant = QdrantClient::new(store_config).map_err(|error| error.to_string())?;
    let qdrant_collection = qdrant_collection_name(root.id(), embedding_model);
    if !qdrant
        .collection_exists(&qdrant_collection)
        .map_err(|error| error.to_string())?
    {
        on_progress(IndexProgress::new(
            "qdrant_cleanup",
            1,
            1,
            format!("Skipped stale point deletion; collection {qdrant_collection} is missing"),
        ));
        return Ok(0);
    }

    let point_ids = point_ids.into_iter().collect::<Vec<_>>();
    on_progress(IndexProgress::new(
        "qdrant_cleanup",
        0,
        point_ids.len(),
        format!("Deleting {} stale Qdrant points", point_ids.len()),
    ));
    qdrant
        .delete_points(&qdrant_collection, &point_ids)
        .map_err(|error| error.to_string())?;
    on_progress(IndexProgress::new(
        "qdrant_cleanup",
        point_ids.len(),
        point_ids.len(),
        format!("Deleted {} stale Qdrant points", point_ids.len()),
    ));
    Ok(point_ids.len())
}

fn chunk_texts(reports: &[IndexReport]) -> Vec<ChunkText<'_>> {
    reports
        .iter()
        .flat_map(|report| {
            report
                .chunks
                .iter()
                .filter(|chunk| chunk.excluded_reason.is_none())
                .map(|chunk| ChunkText {
                    file: &report.file,
                    chunk,
                    text: report.source[chunk.byte_range.start..chunk.byte_range.end].to_owned(),
                })
        })
        .collect()
}

fn vector_point(
    repository_id: &str,
    chunk: &ChunkText<'_>,
    vector: Vec<f32>,
    index_run_id: &str,
    embedding_model: &str,
    embedding_dimension: usize,
) -> Result<VectorPoint, String> {
    Ok(VectorPoint {
        id: qdrant_point_id(&chunk.chunk.id).map_err(|error| error.to_string())?,
        vector,
        payload: PointPayload {
            repository_id: repository_id.to_owned(),
            file_id: chunk.file.id.clone(),
            chunk_id: chunk.chunk.id.clone(),
            symbol_id: chunk.chunk.symbol_id.clone(),
            symbol_name: chunk.chunk.symbol_name.clone(),
            path: chunk.chunk.relative_path.clone(),
            language: chunk.file.language.as_str().to_owned(),
            chunk_kind: chunk.chunk.kind.as_str().to_owned(),
            start_line: chunk.chunk.line_range.start,
            end_line: chunk.chunk.line_range.end,
            text_hash: chunk.chunk.text_hash.clone(),
            parser_version: Some(chunk.file.language.parser_version().to_owned()),
            content_hash: Some(chunk.file.content_hash.clone()),
            index_run_id: Some(index_run_id.to_owned()),
            embedding_model: Some(embedding_model.to_owned()),
            embedding_dimension: Some(embedding_dimension),
            indexed_at: Some(current_timestamp()),
        },
    })
}

fn chunk_record(
    chunk: &CodeChunk,
    index_run_id: &str,
    parser_version: &str,
) -> Result<ChunkRecord, String> {
    Ok(ChunkRecord {
        id: chunk.id.clone(),
        file_id: chunk.file_id.clone(),
        symbol_id: chunk.symbol_id.clone(),
        kind: chunk.kind.as_str().to_owned(),
        text_hash: chunk.text_hash.clone(),
        start_line: chunk.line_range.start,
        end_line: chunk.line_range.end,
        start_byte: chunk.byte_range.start,
        end_byte: chunk.byte_range.end,
        qdrant_point_id: if chunk.excluded_reason.is_none() {
            Some(qdrant_point_id(&chunk.id).map_err(|error| error.to_string())?)
        } else {
            None
        },
        excluded_reason: chunk.excluded_reason.clone(),
        index_run_id: index_run_id.to_owned(),
        parser_version: parser_version.to_owned(),
        embedding_model: None,
        embedding_dimension: None,
        embedded_at: None,
    })
}

fn symbol_record(symbol: &Symbol, index_run_id: &str, parser_version: &str) -> SymbolRecord {
    SymbolRecord {
        id: symbol.id.clone(),
        file_id: symbol.file_id.clone(),
        parent_symbol_id: symbol.parent_symbol_id.clone(),
        name: symbol.name.clone(),
        qualified_name: symbol.qualified_name.clone(),
        kind: symbol.kind.as_str().to_owned(),
        signature: symbol.signature.clone(),
        start_line: symbol.line_range.start,
        end_line: symbol.line_range.end,
        start_byte: symbol.byte_range.start,
        end_byte: symbol.byte_range.end,
        index_run_id: index_run_id.to_owned(),
        parser_version: parser_version.to_owned(),
    }
}

fn call_record(call: &CallEdge, index_run_id: &str, parser_version: &str) -> CallRecord {
    CallRecord {
        id: call.id.clone(),
        caller_symbol_id: call.caller_symbol_id.clone(),
        callee_text: call.callee_text.clone(),
        callee_symbol_id: call.callee_symbol_id.clone(),
        call_line: call.call_line,
        confidence: call.confidence,
        resolution_status: call.resolution_status.as_str().to_owned(),
        index_run_id: index_run_id.to_owned(),
        parser_version: parser_version.to_owned(),
    }
}

fn test_record(
    repository_id: &str,
    test: &DiscoveredTest,
    index_run_id: &str,
    parser_version: &str,
) -> TestRecord {
    TestRecord {
        id: test.id.clone(),
        repository_id: repository_id.to_owned(),
        file_id: test.file_id.clone(),
        symbol_id: test.symbol_id.clone(),
        name: test.name.clone(),
        qualified_name: test.qualified_name.clone(),
        framework: test.framework.clone(),
        language: test.language.as_str().to_owned(),
        path: test.relative_path.clone(),
        start_line: test.line_range.start,
        end_line: test.line_range.end,
        start_byte: test.byte_range.start,
        end_byte: test.byte_range.end,
        index_run_id: index_run_id.to_owned(),
        parser_version: parser_version.to_owned(),
    }
}

fn parser_version_summary(collection: &IndexCollection) -> String {
    if collection.parser_versions.is_empty() {
        return "none".to_owned();
    }
    collection
        .parser_versions
        .iter()
        .cloned()
        .collect::<Vec<_>>()
        .join(",")
}

fn index_run_record(
    scope: &RunScope<'_>,
    status: &str,
    embedding_dimension: Option<usize>,
    counts: RunCounts,
    error_summary: Option<String>,
    parser_version: &str,
) -> IndexRunRecord {
    IndexRunRecord {
        id: scope.index_run_id.to_owned(),
        repository_id: scope.repository_id.to_owned(),
        status: status.to_owned(),
        embedding_model: scope.embedding_model.to_owned(),
        embedding_dimension,
        files_seen: counts.files_seen,
        files_indexed: counts.files_indexed,
        chunks_embedded: counts.chunks_embedded,
        error_summary,
        parser_version: parser_version.to_owned(),
        indexer_version: env!("CARGO_PKG_VERSION").to_owned(),
        run_kind: scope.run_kind.to_owned(),
    }
}

fn finish_failed_index_run(
    sqlite: &SqliteStore,
    scope: &RunScope<'_>,
    parser_version: &str,
    counts: RunCounts,
    status: &str,
    error: &str,
) -> Result<(), String> {
    sqlite
        .finish_index_run(&index_run_record(
            scope,
            status,
            None,
            counts,
            Some(error_summary(error)),
            parser_version,
        ))
        .map_err(|record_error| {
            format!("{error}; additionally failed to record index run: {record_error}")
        })
}

fn error_summary(error: &str) -> String {
    let normalized = error.split_whitespace().collect::<Vec<_>>().join(" ");
    const MAX_ERROR_SUMMARY_CHARS: usize = 512;
    if normalized.chars().count() <= MAX_ERROR_SUMMARY_CHARS {
        return normalized;
    }

    normalized
        .chars()
        .take(MAX_ERROR_SUMMARY_CHARS)
        .collect::<String>()
}

struct IndexCollection {
    files_seen: usize,
    files_skipped_unchanged: usize,
    parser_versions: BTreeSet<String>,
    active_paths: Vec<String>,
    reports: Vec<IndexReport>,
}

struct IndexReport {
    file: FileFacts,
    chunks: Vec<CodeChunk>,
    symbols: Vec<Symbol>,
    calls: Vec<CallEdge>,
    tests: Vec<DiscoveredTest>,
    parse_diagnostics: Vec<ParseDiagnostic>,
    source: String,
}

struct ChunkText<'a> {
    file: &'a FileFacts,
    chunk: &'a CodeChunk,
    text: String,
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    use symdex_core::{
        ByteRange, CallEdge, ChunkKind, CodeChunk, FileFacts, Language, LineRange, RepoRoot,
        ResolutionStatus, Symbol, SymbolKind, content_hash, stable_id,
    };
    use symdex_store::SymbolRecord;

    use crate::{
        IndexCollection, IndexReport, RustAnalyzerEnrichmentConfig, RustAnalyzerEnrichmentSummary,
        RustAnalyzerReadiness, WatchSnapshot, chunk_record, chunk_texts, collect_index_reports,
        detect_watch_changes, diff_watch_snapshots, plan_rust_analyzer_enrichment,
        resolve_cross_file_rust_calls, watch_snapshot,
    };

    #[test]
    fn chunk_texts_skip_secret_excluded_chunks() {
        let file = sample_file();
        let source = "pub fn public() {}\npub fn secret() {}\n".to_owned();
        let public = sample_chunk("public", 0, 18, None);
        let secret = sample_chunk("secret", 18, source.len(), Some("likely_access_token"));
        let report = IndexReport {
            file: file.clone(),
            chunks: vec![public.clone(), secret.clone()],
            symbols: Vec::new(),
            calls: Vec::new(),
            tests: Vec::new(),
            parse_diagnostics: Vec::new(),
            source,
        };

        let reports = [report];
        let chunks = chunk_texts(&reports);

        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].chunk.id, public.id);
        let public_record =
            chunk_record(&public, "run", Language::Rust.parser_version()).expect("public record");
        let secret_record =
            chunk_record(&secret, "run", Language::Rust.parser_version()).expect("secret record");
        assert!(public_record.qdrant_point_id.is_some());
        assert!(secret_record.qdrant_point_id.is_none());
        assert_eq!(
            secret_record.excluded_reason.as_deref(),
            Some("likely_access_token")
        );
    }

    #[test]
    fn watch_snapshot_diff_coalesces_created_modified_and_deleted_paths() {
        let previous = WatchSnapshot {
            files: BTreeMap::from([
                ("src/deleted.rs".to_owned(), "old-delete".to_owned()),
                ("src/lib.rs".to_owned(), "old-lib".to_owned()),
                ("src/unchanged.rs".to_owned(), "same".to_owned()),
            ]),
        };
        let next = WatchSnapshot {
            files: BTreeMap::from([
                ("src/created.rs".to_owned(), "new-create".to_owned()),
                ("src/lib.rs".to_owned(), "new-lib".to_owned()),
                ("src/unchanged.rs".to_owned(), "same".to_owned()),
            ]),
        };

        let changes = diff_watch_snapshots(&previous, &next);

        assert_eq!(changes.created, vec!["src/created.rs"]);
        assert_eq!(changes.modified, vec!["src/lib.rs"]);
        assert_eq!(changes.deleted, vec!["src/deleted.rs"]);
        assert_eq!(changes.event_count(), 3);
        assert_eq!(
            changes.paths(),
            vec!["src/created.rs", "src/lib.rs", "src/deleted.rs"]
        );
    }

    #[test]
    fn detect_watch_changes_reports_created_and_modified_indexable_files() {
        let repo = TestRepo::new("watch-created-modified");
        repo.write("src/lib.rs", "pub fn old() {}\n");
        repo.write("web/app.js", "function app() {}\n");
        let root = RepoRoot::open(repo.path()).expect("repo root should open");
        let snapshot = watch_snapshot(&root).expect("snapshot should load");

        repo.write("src/lib.rs", "pub fn new_name() {}\n");
        repo.write("src/created.cs", "class Created { void Run() {} }\n");
        repo.write("web/app.js", "function changed() {}\n");
        repo.write("web/util.ts", "export function util(): void {}\n");
        let (_next, changes) =
            detect_watch_changes(&root, &snapshot).expect("changes should detect");

        assert_eq!(changes.created, vec!["src/created.cs", "web/util.ts"]);
        assert_eq!(changes.modified, vec!["src/lib.rs", "web/app.js"]);
        assert!(changes.deleted.is_empty());
    }

    #[test]
    fn detect_watch_changes_skips_ignored_unsupported_and_unchanged_files() {
        let repo = TestRepo::new("watch-ignored-unchanged");
        repo.write(".gitignore", "ignored.rs\nignored_dir/\n");
        repo.write("src/lib.rs", "pub fn lib() {}\n");
        let root = RepoRoot::open(repo.path()).expect("repo root should open");
        let snapshot = watch_snapshot(&root).expect("snapshot should load");

        repo.write("src/lib.rs", "pub fn lib() {}\n");
        repo.write("README.md", "# not indexed\n");
        repo.write("ignored.rs", "pub fn ignored() {}\n");
        repo.write("ignored_dir/new.rs", "pub fn ignored() {}\n");
        let (_next, changes) =
            detect_watch_changes(&root, &snapshot).expect("changes should detect");

        assert!(changes.is_empty());
    }

    #[test]
    fn index_collection_keeps_partial_parse_diagnostics() {
        let repo = TestRepo::new("partial-parse-diagnostics");
        repo.write("src/lib.rs", "pub fn ok() {}\npub fn broken( {}\n");
        let root = RepoRoot::open(repo.path()).expect("repo root should open");
        let collection = collect_index_reports(&root, None, &mut |_| {})
            .expect("syntax errors should not abort collection");

        assert_eq!(collection.reports.len(), 1);
        assert!(!collection.reports[0].chunks.is_empty());
        assert!(!collection.reports[0].parse_diagnostics.is_empty());
        let summaries = super::file_summaries(&collection.reports);
        assert!(!summaries[0].parse_diagnostics.is_empty());
    }

    #[test]
    fn resolves_cross_file_rust_module_calls_from_current_reports() {
        let repo = TestRepo::new("cross-file-rust-current-reports");
        repo.write(
            "src/lib.rs",
            "mod worker;\npub fn run() { crate::worker::helper(); }\n",
        );
        repo.write("src/worker.rs", "pub fn helper() {}\n");
        let root = RepoRoot::open(repo.path()).expect("repo root should open");
        let mut collection = collect_index_reports(&root, None, &mut |_| {})
            .expect("collection should parse both files");

        resolve_cross_file_rust_calls(&mut collection, &[]);

        let helper_symbol_id = collection
            .reports
            .iter()
            .flat_map(|report| &report.symbols)
            .find(|symbol| symbol.qualified_name == "worker::helper")
            .expect("helper symbol should be indexed")
            .id
            .clone();
        let call = collection
            .reports
            .iter()
            .flat_map(|report| &report.calls)
            .find(|call| call.callee_text == "crate::worker::helper")
            .expect("qualified module call should be captured");

        assert_eq!(
            call.callee_symbol_id.as_deref(),
            Some(helper_symbol_id.as_str())
        );
        assert_eq!(call.resolution_status, ResolutionStatus::ResolvedExact);
        assert_eq!(call.confidence, 1.0);
    }

    #[test]
    fn resolves_cross_file_rust_calls_against_persisted_unchanged_symbols() {
        let mut caller = sample_symbol("run");
        caller.id = stable_id(&["symbol", "run"]);
        let mut call = sample_unresolved_call("run", "crate::worker::helper");
        call.caller_symbol_id = caller.id.clone();
        let mut collection = collection_with_reports(vec![IndexReport {
            file: FileFacts {
                id: "file-lib".to_owned(),
                relative_path: "src/lib.rs".to_owned(),
                language: Language::Rust,
                content_hash: content_hash(b"lib"),
            },
            chunks: Vec::new(),
            symbols: vec![caller],
            calls: vec![call],
            tests: Vec::new(),
            parse_diagnostics: Vec::new(),
            source: String::new(),
        }]);
        let persisted_helper = sample_symbol_record("worker::helper", "file-worker");

        resolve_cross_file_rust_calls(&mut collection, std::slice::from_ref(&persisted_helper));

        let call = &collection.reports[0].calls[0];
        assert_eq!(
            call.callee_symbol_id.as_deref(),
            Some(persisted_helper.id.as_str())
        );
        assert_eq!(call.resolution_status, ResolutionStatus::ResolvedExact);
    }

    #[test]
    fn resolves_cross_file_unqualified_rust_calls_from_caller_module() {
        let mut outer_caller = sample_symbol("caller");
        outer_caller.id = stable_id(&["symbol", "outer::caller"]);
        outer_caller.qualified_name = "outer::caller".to_owned();
        let mut sibling_caller = sample_symbol("caller");
        sibling_caller.id = stable_id(&["symbol", "sibling::caller"]);
        sibling_caller.qualified_name = "sibling::caller".to_owned();
        let mut outer_call = sample_unresolved_call("outer::caller", "helper");
        outer_call.caller_symbol_id = outer_caller.id.clone();
        let mut sibling_call = sample_unresolved_call("sibling::caller", "helper");
        sibling_call.caller_symbol_id = sibling_caller.id.clone();
        let mut collection = collection_with_reports(vec![IndexReport {
            file: FileFacts {
                id: "file-callers".to_owned(),
                relative_path: "src/callers.rs".to_owned(),
                language: Language::Rust,
                content_hash: content_hash(b"callers"),
            },
            chunks: Vec::new(),
            symbols: vec![outer_caller, sibling_caller],
            calls: vec![outer_call, sibling_call],
            tests: Vec::new(),
            parse_diagnostics: Vec::new(),
            source: String::new(),
        }]);
        let outer_helper = sample_symbol_record("outer::helper", "file-outer-helper");
        let root_helper = sample_symbol_record("helper", "file-root-helper");

        resolve_cross_file_rust_calls(&mut collection, &[outer_helper.clone(), root_helper]);

        let outer_call = &collection.reports[0].calls[0];
        assert_eq!(
            outer_call.callee_symbol_id.as_deref(),
            Some(outer_helper.id.as_str())
        );
        assert_eq!(
            outer_call.resolution_status,
            ResolutionStatus::ResolvedExact
        );

        let sibling_call = &collection.reports[0].calls[1];
        assert!(sibling_call.callee_symbol_id.is_none());
        assert_eq!(sibling_call.resolution_status, ResolutionStatus::Unresolved);
    }

    #[test]
    fn resolves_cross_file_scoped_rust_calls_from_caller_module() {
        let mut caller = sample_symbol("caller");
        caller.id = stable_id(&["symbol", "outer::caller"]);
        caller.qualified_name = "outer::caller".to_owned();
        let mut call = sample_unresolved_call("outer::caller", "Worker::run");
        call.caller_symbol_id = caller.id.clone();
        let mut collection = collection_with_reports(vec![IndexReport {
            file: FileFacts {
                id: "file-caller".to_owned(),
                relative_path: "src/caller.rs".to_owned(),
                language: Language::Rust,
                content_hash: content_hash(b"caller"),
            },
            chunks: Vec::new(),
            symbols: vec![caller],
            calls: vec![call],
            tests: Vec::new(),
            parse_diagnostics: Vec::new(),
            source: String::new(),
        }]);
        let outer_run = sample_symbol_record("outer::Worker::run", "file-outer-worker");
        let root_run = sample_symbol_record("Worker::run", "file-root-worker");

        resolve_cross_file_rust_calls(&mut collection, &[outer_run.clone(), root_run]);

        let call = &collection.reports[0].calls[0];
        assert_eq!(
            call.callee_symbol_id.as_deref(),
            Some(outer_run.id.as_str())
        );
        assert_eq!(call.resolution_status, ResolutionStatus::ResolvedExact);
    }

    #[test]
    fn resolves_cross_file_super_module_calls_from_caller_scope() {
        let mut caller = sample_symbol("caller");
        caller.id = stable_id(&["symbol", "outer::inner::caller"]);
        caller.qualified_name = "outer::inner::caller".to_owned();
        let mut call = sample_unresolved_call("caller", "super::worker::helper");
        call.caller_symbol_id = caller.id.clone();
        let mut collection = collection_with_reports(vec![IndexReport {
            file: FileFacts {
                id: "file-inner".to_owned(),
                relative_path: "src/outer/inner.rs".to_owned(),
                language: Language::Rust,
                content_hash: content_hash(b"inner"),
            },
            chunks: Vec::new(),
            symbols: vec![caller],
            calls: vec![call],
            tests: Vec::new(),
            parse_diagnostics: Vec::new(),
            source: String::new(),
        }]);
        let parent_helper = sample_symbol_record("outer::worker::helper", "file-parent-worker");
        let root_helper = sample_symbol_record("worker::helper", "file-root-worker");

        resolve_cross_file_rust_calls(&mut collection, &[parent_helper.clone(), root_helper]);

        let call = &collection.reports[0].calls[0];
        assert_eq!(
            call.callee_symbol_id.as_deref(),
            Some(parent_helper.id.as_str())
        );
        assert_eq!(call.resolution_status, ResolutionStatus::ResolvedExact);
    }

    #[test]
    fn ignores_persisted_symbols_from_replaced_files_during_cross_file_resolution() {
        let caller = sample_symbol("run");
        let collection_file_id = caller.file_id.clone();
        let mut collection = collection_with_reports(vec![IndexReport {
            file: FileFacts {
                id: collection_file_id.clone(),
                relative_path: "src/lib.rs".to_owned(),
                language: Language::Rust,
                content_hash: content_hash(b"lib"),
            },
            chunks: Vec::new(),
            symbols: vec![caller],
            calls: vec![sample_unresolved_call("run", "crate::worker::helper")],
            tests: Vec::new(),
            parse_diagnostics: Vec::new(),
            source: String::new(),
        }]);
        let stale_symbol = sample_symbol_record("worker::helper", &collection_file_id);

        resolve_cross_file_rust_calls(&mut collection, &[stale_symbol]);

        let call = &collection.reports[0].calls[0];
        assert!(call.callee_symbol_id.is_none());
        assert_eq!(call.resolution_status, ResolutionStatus::Unresolved);
    }

    #[test]
    fn rust_analyzer_enrichment_plan_is_disabled_by_default() {
        let collection = collection_with_reports(Vec::new());
        let config = RustAnalyzerEnrichmentConfig::from_values(None, None);

        let summary =
            plan_rust_analyzer_enrichment(&collection, &config, RustAnalyzerReadiness::Disabled);

        assert_eq!(
            summary,
            RustAnalyzerEnrichmentSummary::Disabled {
                enable_env: "SYMDEX_RUST_ANALYZER".to_owned(),
            }
        );
    }

    #[test]
    fn rust_analyzer_enrichment_plan_reports_not_ready() {
        let collection = collection_with_reports(vec![sample_report(Language::Rust)]);
        let config = RustAnalyzerEnrichmentConfig::from_values(Some("1"), Some("custom-ra"));

        let summary = plan_rust_analyzer_enrichment(
            &collection,
            &config,
            RustAnalyzerReadiness::NotReady {
                reason: "missing binary".to_owned(),
            },
        );

        assert_eq!(
            summary,
            RustAnalyzerEnrichmentSummary::NotReady {
                command: "custom-ra".to_owned(),
                reason: "missing binary".to_owned(),
            }
        );
    }

    #[test]
    fn rust_analyzer_enrichment_plan_skips_when_no_rust_reports_exist() {
        let collection = collection_with_reports(vec![sample_report(Language::CSharp)]);
        let config = RustAnalyzerEnrichmentConfig::from_values(Some("true"), Some("ra"));

        let summary = plan_rust_analyzer_enrichment(
            &collection,
            &config,
            RustAnalyzerReadiness::Ready {
                version: "rust-analyzer test".to_owned(),
            },
        );

        assert_eq!(
            summary,
            RustAnalyzerEnrichmentSummary::SkippedNoRustFiles {
                command: "ra".to_owned(),
                version: "rust-analyzer test".to_owned(),
            }
        );
    }

    #[test]
    fn rust_analyzer_enrichment_plan_counts_eligible_rust_facts() {
        let mut rust = sample_report(Language::Rust);
        rust.symbols = vec![sample_symbol("one"), sample_symbol("two")];
        rust.calls = vec![sample_call("one", "two")];
        let collection = collection_with_reports(vec![rust, sample_report(Language::TypeScript)]);
        let config = RustAnalyzerEnrichmentConfig::from_values(Some("on"), Some("ra"));

        let summary = plan_rust_analyzer_enrichment(
            &collection,
            &config,
            RustAnalyzerReadiness::Ready {
                version: "rust-analyzer test".to_owned(),
            },
        );

        assert_eq!(
            summary,
            RustAnalyzerEnrichmentSummary::Planned {
                command: "ra".to_owned(),
                version: "rust-analyzer test".to_owned(),
                eligible_files: 1,
                eligible_symbols: 2,
                eligible_calls: 1,
            }
        );
    }

    fn sample_file() -> FileFacts {
        FileFacts {
            id: "file-1".to_owned(),
            relative_path: "src/lib.rs".to_owned(),
            language: Language::Rust,
            content_hash: content_hash(b"sample"),
        }
    }

    fn sample_report(language: Language) -> IndexReport {
        IndexReport {
            file: FileFacts {
                id: format!("file-{}", language.as_str()),
                relative_path: format!("src/sample.{}", language.as_str()),
                language,
                content_hash: content_hash(language.as_str().as_bytes()),
            },
            chunks: Vec::new(),
            symbols: Vec::new(),
            calls: Vec::new(),
            tests: Vec::new(),
            parse_diagnostics: Vec::new(),
            source: String::new(),
        }
    }

    fn collection_with_reports(reports: Vec<IndexReport>) -> IndexCollection {
        IndexCollection {
            files_seen: reports.len(),
            files_skipped_unchanged: 0,
            parser_versions: BTreeSet::new(),
            active_paths: reports
                .iter()
                .map(|report| report.file.relative_path.clone())
                .collect(),
            reports,
        }
    }

    fn sample_chunk(
        name: &str,
        start: usize,
        end: usize,
        excluded_reason: Option<&str>,
    ) -> CodeChunk {
        CodeChunk {
            id: stable_id(&["chunk", name]),
            file_id: "file-1".to_owned(),
            relative_path: "src/lib.rs".to_owned(),
            symbol_id: Some(stable_id(&["symbol", name])),
            symbol_name: Some(name.to_owned()),
            kind: ChunkKind::Function,
            byte_range: ByteRange::new(start, end),
            line_range: LineRange::new(1, 1),
            text_hash: content_hash(name.as_bytes()),
            excluded_reason: excluded_reason.map(str::to_owned),
        }
    }

    fn sample_symbol(name: &str) -> Symbol {
        Symbol {
            id: stable_id(&["symbol", name]),
            file_id: "file-rust".to_owned(),
            parent_symbol_id: None,
            name: name.to_owned(),
            qualified_name: name.to_owned(),
            kind: SymbolKind::Function,
            signature: Some(format!("fn {name}()")),
            byte_range: ByteRange::new(0, 1),
            line_range: LineRange::new(1, 1),
        }
    }

    fn sample_call(caller: &str, callee: &str) -> CallEdge {
        CallEdge {
            id: stable_id(&["call", caller, callee]),
            caller_symbol_id: stable_id(&["symbol", caller]),
            callee_text: callee.to_owned(),
            callee_symbol_id: Some(stable_id(&["symbol", callee])),
            call_line: 1,
            confidence: 1.0,
            resolution_status: ResolutionStatus::ResolvedExact,
        }
    }

    fn sample_unresolved_call(caller: &str, callee: &str) -> CallEdge {
        CallEdge {
            id: stable_id(&["call", caller, callee]),
            caller_symbol_id: stable_id(&["symbol", caller]),
            callee_text: callee.to_owned(),
            callee_symbol_id: None,
            call_line: 1,
            confidence: 0.25,
            resolution_status: ResolutionStatus::Unresolved,
        }
    }

    fn sample_symbol_record(qualified_name: &str, file_id: &str) -> SymbolRecord {
        let name = qualified_name.rsplit("::").next().unwrap_or(qualified_name);
        SymbolRecord {
            id: stable_id(&["persisted-symbol", qualified_name, file_id]),
            file_id: file_id.to_owned(),
            parent_symbol_id: None,
            name: name.to_owned(),
            qualified_name: qualified_name.to_owned(),
            kind: "function".to_owned(),
            signature: Some(format!("fn {name}()")),
            start_line: 1,
            end_line: 1,
            start_byte: 0,
            end_byte: 1,
            index_run_id: "run".to_owned(),
            parser_version: Language::Rust.parser_version().to_owned(),
        }
    }

    struct TestRepo {
        path: PathBuf,
    }

    impl TestRepo {
        fn new(name: &str) -> Self {
            let path = temp_path(name);
            fs::create_dir_all(&path).expect("repo should be created");
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }

        fn write(&self, relative: &str, contents: &str) {
            let path = self.path.join(relative);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).expect("parent directory should be created");
            }
            fs::write(path, contents).expect("test file should be written");
        }
    }

    impl Drop for TestRepo {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn temp_path(name: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should be after epoch")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "symdex-index-{name}-{}-{nonce}",
            std::process::id()
        ))
    }
}

//! Indexing orchestration shared by the CLI and TUI.

use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs;
use std::io;
use std::process::Command;
use std::thread;
use std::time::Duration;

use symdex_core::{
    CallEdge, CodeChunk, DiscoveredTest, DiscoveryOptions, FileFacts, Language, NormalizedRepoPath,
    ParseDiagnostic, RepoRoot, RepositoryRefSnapshot, ResolutionStatus, SemanticLayer, Symbol,
    SymbolKind, content_hash, discover_indexable_files, index_source_file,
};
use symdex_embed::{LayeredEmbedConfig, OllamaClient};
use symdex_store::{
    CallRecord, ChunkEmbeddingRecord, ChunkRecord, FastEmbeddingManifestRecord,
    FastSemanticGenerationInput, FileRecord, IndexRunRecord, PointPayload,
    QualityActivationSummary, QualityGenerationProgress, QualityJobCompletion, QualityJobSourceRow,
    QualityQueueSummary, RepositoryRecord, SqliteStore, SqliteVectorStore, StoreConfig,
    SymbolRecord, TestRecord, VectorPoint, current_timestamp, vector_point_id, vector_table_name,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexOptions {
    pub repo: String,
    pub offline: bool,
    pub scope: IndexScope,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexScope {
    Full,
    Incremental,
}

impl IndexScope {
    pub fn label(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Incremental => "incremental",
        }
    }

    fn skips_unchanged(self) -> bool {
        matches!(self, Self::Incremental)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContinuousIndexOptions {
    pub repo: String,
    pub offline: bool,
    pub quality_catch_up: bool,
    pub poll_interval: Duration,
    pub debounce: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QualityIndexOptions {
    pub repo: String,
}

impl ContinuousIndexOptions {
    pub fn new(repo: impl Into<String>, offline: bool) -> Self {
        Self {
            repo: repo.into(),
            offline,
            quality_catch_up: true,
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
        vector_table: String,
        chunks_embedded: usize,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QualityIndexSummary {
    pub repository_id: String,
    pub generation_id: String,
    pub quality_model: String,
    pub quality_dimension: Option<usize>,
    pub vector_table: String,
    pub claimed_jobs: usize,
    pub succeeded_jobs: usize,
    pub failed_jobs: usize,
    pub skipped_stale_jobs: usize,
    pub skipped_excluded_jobs: usize,
    pub remaining_pending_jobs: usize,
    pub quality_status: String,
    pub active_layer: String,
    pub activation_reason: String,
    pub progress: QualityGenerationProgress,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContinuousQualityState {
    pub repository_id: String,
    pub generation_id: String,
    pub active_layer: String,
    pub quality_status: String,
    pub activation_reason: Option<String>,
    pub embeddable_chunks: usize,
    pub quality_eligible_chunks: usize,
    pub quality_ineligible_chunks: usize,
    pub quality_embedded_chunks: usize,
    pub pending_jobs: usize,
    pub running_jobs: usize,
    pub succeeded_jobs: usize,
    pub failed_jobs: usize,
    pub skipped_stale_jobs: usize,
    pub skipped_excluded_jobs: usize,
}

impl ContinuousQualityState {
    fn has_work(&self) -> bool {
        self.pending_jobs > 0 || self.running_jobs > 0
    }
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
    QualityState {
        state: ContinuousQualityState,
    },
    QualityStarted {
        state: ContinuousQualityState,
    },
    QualityProgress {
        progress: IndexProgress,
    },
    QualityCompleted {
        summary: Box<QualityIndexSummary>,
    },
    QualityFailed {
        state: Option<ContinuousQualityState>,
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
    repository_ref_id: Option<&'a str>,
    embedding_model: &'a str,
    run_kind: &'a str,
}

pub fn run_index(options: &IndexOptions) -> Result<IndexSummary, String> {
    run_index_with_progress(options, |_| {})
}

pub fn run_quality_index(options: &QualityIndexOptions) -> Result<QualityIndexSummary, String> {
    run_quality_index_with_progress(options, |_| {})
}

pub fn run_quality_index_with_progress(
    options: &QualityIndexOptions,
    on_progress: impl FnMut(IndexProgress),
) -> Result<QualityIndexSummary, String> {
    run_quality_index_limited_with_progress(options, None, on_progress)
}

fn run_quality_index_limited_with_progress(
    options: &QualityIndexOptions,
    max_jobs: Option<usize>,
    mut on_progress: impl FnMut(IndexProgress),
) -> Result<QualityIndexSummary, String> {
    let root = RepoRoot::open(&options.repo).map_err(|error| error.to_string())?;
    let store_config = StoreConfig::from_env();
    let mut sqlite = SqliteStore::open(&store_config).map_err(|error| error.to_string())?;
    sqlite.migrate().map_err(|error| error.to_string())?;
    let repository = RepositoryRecord {
        id: root.id().to_owned(),
        root_path: root.path().display().to_string(),
    };
    sqlite
        .upsert_repository(&repository)
        .map_err(|error| error.to_string())?;
    let generation = sqlite
        .latest_semantic_generation(root.id())
        .map_err(|error| error.to_string())?
        .ok_or_else(|| {
            "no semantic generation has been recorded; run `symdex index` first".to_owned()
        })?;
    let layered_embed_config = LayeredEmbedConfig::from_env();
    if !layered_embed_config.quality_enabled {
        return Err("quality indexing is disabled by configuration".to_owned());
    }

    let quality_config = layered_embed_config.quality_embed_config();
    let quality_model = quality_config.model.clone();
    let quality_max_chunk_bytes = quality_config.max_chunk_bytes;
    let vector_table = vector_table_name(root.id(), &quality_model);
    let embed_client = OllamaClient::new(quality_config).map_err(|error| error.to_string())?;
    if !embed_client
        .model_available()
        .map_err(|error| error.to_string())?
    {
        let now = current_timestamp();
        let _ = sqlite.mark_quality_generation_blocked(&generation, &quality_model, &now);
        return Err(format!(
            "quality embedding model `{quality_model}` is not available"
        ));
    }

    let vector = SqliteVectorStore::new(&store_config).map_err(|error| error.to_string())?;
    let mut stats = QualityWorkerStats::default();
    let mut quality_dimension = generation.quality_dimension;
    let batch_size = layered_embed_config.quality_batch_size.max(1);

    loop {
        let remaining_limit = max_jobs.map(|limit| limit.saturating_sub(stats.claimed_jobs));
        if matches!(remaining_limit, Some(0)) {
            break;
        }
        let claim_limit = remaining_limit
            .map(|remaining| remaining.min(batch_size))
            .unwrap_or(batch_size)
            .max(1);
        let claimed_at = current_timestamp();
        let claimed = sqlite
            .claim_quality_embedding_jobs(root.id(), &generation.id, claim_limit, &claimed_at)
            .map_err(|error| error.to_string())?;
        if claimed.is_empty() {
            break;
        }
        stats.claimed_jobs += claimed.len();
        on_progress(IndexProgress::new(
            "quality_index",
            stats.claimed_jobs,
            stats.claimed_jobs + 1,
            format!("Claimed {} quality jobs", claimed.len()),
        ));

        let job_ids = claimed.iter().map(|job| job.id.clone()).collect::<Vec<_>>();
        let source_rows = sqlite
            .quality_job_source_rows(&job_ids)
            .map_err(|error| error.to_string())?;
        let mut source_rows = source_rows
            .into_iter()
            .map(|row| (row.job.id.clone(), row))
            .collect::<BTreeMap<_, _>>();
        for job in claimed {
            let Some(row) = source_rows.remove(&job.id) else {
                let completed_at = current_timestamp();
                sqlite
                    .complete_quality_embedding_job(
                        &job.id,
                        QualityJobCompletion::SkippedStale,
                        &completed_at,
                    )
                    .map_err(|error| error.to_string())?;
                stats.skipped_stale_jobs += 1;
                continue;
            };
            process_quality_job(
                &QualityWorkerContext {
                    root: &root,
                    vector: &vector,
                    embed_client: &embed_client,
                    latest_generation_id: &generation.id,
                    quality_model: &quality_model,
                    quality_max_chunk_bytes,
                    vector_table: &vector_table,
                },
                &mut sqlite,
                &mut quality_dimension,
                &mut stats,
                row,
            )?;
        }
    }

    let refreshed_at = current_timestamp();
    let activation = sqlite
        .refresh_quality_activation(root.id(), &generation.id, quality_dimension, &refreshed_at)
        .map_err(|error| error.to_string())?;

    on_progress(IndexProgress::new(
        "quality_index",
        stats.claimed_jobs,
        stats.claimed_jobs,
        quality_activation_progress_message(&stats, &activation),
    ));

    Ok(QualityIndexSummary {
        repository_id: root.id().to_owned(),
        generation_id: generation.id,
        quality_model,
        quality_dimension,
        vector_table,
        claimed_jobs: stats.claimed_jobs,
        succeeded_jobs: stats.succeeded_jobs,
        failed_jobs: stats.failed_jobs,
        skipped_stale_jobs: stats.skipped_stale_jobs,
        skipped_excluded_jobs: stats.skipped_excluded_jobs,
        remaining_pending_jobs: activation.progress.pending_jobs,
        quality_status: activation.quality_status.as_str().to_owned(),
        active_layer: activation.active_layer.as_str().to_owned(),
        activation_reason: activation.reason.as_str().to_owned(),
        progress: activation.progress,
    })
}

pub fn run_incremental_index(options: &IndexOptions) -> Result<IndexSummary, String> {
    run_index_internal(options, true, None, |_| {})
}

fn run_watch_incremental_index(options: &IndexOptions) -> Result<IndexSummary, String> {
    run_index_internal(options, true, Some("watch"), |_| {})
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
            run_continuous_quality_catch_up(options, &mut on_event, false);
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

        match run_watch_incremental_index(&IndexOptions {
            repo: options.repo.clone(),
            offline: options.offline,
            scope: IndexScope::Incremental,
        }) {
            Ok(summary) => {
                snapshot = watch_snapshot(&root).unwrap_or(debounced_snapshot);
                on_event(ContinuousIndexEvent::BatchCompleted {
                    changes,
                    summary: Box::new(summary),
                });
                run_continuous_quality_catch_up(options, &mut on_event, true);
            }
            Err(error) => {
                snapshot = debounced_snapshot;
                on_event(ContinuousIndexEvent::BatchFailed { changes, error });
            }
        }
    }
    Ok(())
}

fn run_continuous_quality_catch_up(
    options: &ContinuousIndexOptions,
    on_event: &mut impl FnMut(ContinuousIndexEvent),
    emit_state_without_work: bool,
) {
    let layered_config = LayeredEmbedConfig::from_env();
    if !should_run_continuous_quality_catch_up(options, &layered_config) {
        return;
    }

    let state = match continuous_quality_state(&options.repo, None) {
        Ok(Some(state)) => state,
        Ok(None) => return,
        Err(error) => {
            on_event(ContinuousIndexEvent::QualityFailed { state: None, error });
            return;
        }
    };
    if !state.has_work() && !emit_state_without_work {
        return;
    }
    on_event(ContinuousIndexEvent::QualityState {
        state: state.clone(),
    });
    if !state.has_work() {
        return;
    }

    on_event(ContinuousIndexEvent::QualityStarted { state });
    match run_quality_index_limited_with_progress(
        &QualityIndexOptions {
            repo: options.repo.clone(),
        },
        Some(layered_config.quality_batch_size.max(1)),
        |progress| on_event(ContinuousIndexEvent::QualityProgress { progress }),
    ) {
        Ok(summary) => on_event(ContinuousIndexEvent::QualityCompleted {
            summary: Box::new(summary),
        }),
        Err(error) => {
            let state = continuous_quality_state(&options.repo, Some("quality_worker_failed"))
                .ok()
                .flatten();
            on_event(ContinuousIndexEvent::QualityFailed { state, error });
        }
    }
}

fn should_run_continuous_quality_catch_up(
    options: &ContinuousIndexOptions,
    layered_config: &LayeredEmbedConfig,
) -> bool {
    options.quality_catch_up && !options.offline && layered_config.quality_enabled
}

fn continuous_quality_state(
    repo: &str,
    activation_reason: Option<&str>,
) -> Result<Option<ContinuousQualityState>, String> {
    let root = RepoRoot::open(repo).map_err(|error| error.to_string())?;
    let store_config = StoreConfig::from_env();
    let sqlite = SqliteStore::open(&store_config).map_err(|error| error.to_string())?;
    sqlite.migrate().map_err(|error| error.to_string())?;
    let Some(routing) = sqlite
        .semantic_routing_summary(root.id())
        .map_err(|error| error.to_string())?
    else {
        return Ok(None);
    };
    let progress = sqlite
        .quality_generation_progress(root.id(), &routing.generation_id)
        .map_err(|error| error.to_string())?;
    Ok(Some(ContinuousQualityState {
        repository_id: root.id().to_owned(),
        generation_id: routing.generation_id,
        active_layer: routing.active_layer.as_str().to_owned(),
        quality_status: routing.quality_status.as_str().to_owned(),
        activation_reason: activation_reason.map(str::to_owned),
        embeddable_chunks: progress.embeddable_chunks,
        quality_eligible_chunks: progress.quality_eligible_chunks,
        quality_ineligible_chunks: progress.quality_ineligible_chunks,
        quality_embedded_chunks: progress.quality_embedded_chunks,
        pending_jobs: progress.pending_jobs,
        running_jobs: progress.running_jobs,
        succeeded_jobs: progress.succeeded_jobs,
        failed_jobs: progress.failed_jobs,
        skipped_stale_jobs: progress.skipped_stale_jobs,
        skipped_excluded_jobs: progress.skipped_excluded_jobs,
    }))
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
    run_index_internal(
        options,
        options.scope.skips_unchanged(),
        None,
        &mut on_progress,
    )
}

fn run_index_internal(
    options: &IndexOptions,
    skip_unchanged: bool,
    run_kind_override: Option<&'static str>,
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
    let repository_ref = RepositoryRefSnapshot::detect(&root).map_err(|error| error.to_string())?;
    sqlite
        .sync_repository_ref(&repository_ref)
        .map_err(|error| error.to_string())?;
    on_progress(IndexProgress::new(
        "open",
        1,
        1,
        "Repository and SQLite store ready",
    ));

    let run_kind = index_run_kind(options.offline, run_kind_override);
    let index_run_id = SqliteStore::new_index_run_id(root.id(), run_kind);
    let layered_embed_config = LayeredEmbedConfig::from_env();
    let embed_config = layered_embed_config.fast_embed_config();
    let embedding_model = if options.offline {
        "offline".to_owned()
    } else {
        embed_config.model.clone()
    };
    let run_scope = RunScope {
        index_run_id: &index_run_id,
        repository_id: root.id(),
        repository_ref_id: Some(&repository_ref.id),
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
    apply_embedding_size_limits(&mut collection.reports, embed_config.max_chunk_bytes);
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
    let stale_vector_point_ids = if options.offline {
        BTreeSet::new()
    } else {
        match stale_vector_point_ids(&sqlite, &root, &collection) {
            Ok(point_ids) => point_ids,
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
        }
    };
    let prepared_semantic = if options.offline {
        None
    } else {
        match prepare_semantic_index(
            &sqlite,
            &root,
            &store_config,
            &collection,
            &layered_embed_config,
            &index_run_id,
            &mut on_progress,
        ) {
            Ok(prepared) => Some(prepared),
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
        }
    };
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
        match finalize_semantic_index(
            &mut sqlite,
            &root,
            &store_config,
            &layered_embed_config,
            prepared_semantic.expect("semantic indexing should be prepared when not offline"),
            &stale_vector_point_ids,
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

fn index_run_kind(offline: bool, override_kind: Option<&'static str>) -> &'static str {
    override_kind.unwrap_or(if offline { "offline" } else { "semantic" })
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
    let receiver_method_candidate =
        caller.and_then(|caller| cross_file_receiver_method_candidate(callee_text, caller));
    let module_candidates = caller
        .into_iter()
        .flat_map(|caller| cross_file_module_relative_candidates(callee_text, caller))
        .collect::<Vec<_>>();
    let suppress_plain_candidate = module_scoped_candidate.is_some()
        || module_unqualified_candidate.is_some()
        || receiver_method_candidate.is_some()
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
    if let Some(receiver_method_candidate) = receiver_method_candidate {
        candidates.insert(receiver_method_candidate);
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
        || cross_file_receiver_method_candidate(callee_text, caller).is_some()
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

fn cross_file_receiver_method_candidate(
    callee_text: &str,
    caller: &ResolutionSymbol,
) -> Option<String> {
    let receiver = caller.receiver_type()?;
    if let Some(method_name) = callee_text.strip_prefix("self.") {
        return Some(format!("{receiver}::{method_name}"));
    }
    if let Some(method_name) = callee_text.strip_prefix("Self::") {
        return Some(format!("{receiver}::{method_name}"));
    }
    None
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

#[derive(Debug, Clone)]
enum PreparedSemanticIndex {
    SkippedNoChunks,
    Completed(PreparedFastSemanticIndex),
}

#[derive(Debug, Clone)]
struct PreparedFastSemanticIndex {
    model: String,
    dimension: usize,
    vector_table: String,
    fast_embeddings: Vec<FastEmbeddingManifestRecord>,
    files_seen: usize,
}

impl PreparedFastSemanticIndex {
    fn point_ids(&self) -> BTreeSet<String> {
        self.fast_embeddings
            .iter()
            .map(|embedding| embedding.vector_point_id.clone())
            .collect()
    }
}

fn prepare_semantic_index(
    sqlite: &SqliteStore,
    root: &RepoRoot,
    store_config: &StoreConfig,
    collection: &IndexCollection,
    layered_embed_config: &LayeredEmbedConfig,
    index_run_id: &str,
    on_progress: &mut impl FnMut(IndexProgress),
) -> Result<PreparedSemanticIndex, String> {
    let embed_config = layered_embed_config.fast_embed_config();
    let chunk_texts = chunk_texts(&collection.reports);
    if chunk_texts.is_empty() {
        on_progress(IndexProgress::new(
            "embedding",
            1,
            1,
            "Embedding skipped because no chunks changed",
        ));
        return Ok(PreparedSemanticIndex::SkippedNoChunks);
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

    let vector = SqliteVectorStore::new(store_config).map_err(|error| error.to_string())?;
    let vector_table = vector_table_name(root.id(), &embed_config.model);
    vector
        .ensure_table(&vector_table, dimension)
        .map_err(|error| error.to_string())?;
    on_progress(IndexProgress::new(
        "vector",
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
    vector
        .upsert_points(&vector_table, &points)
        .map_err(|error| error.to_string())?;
    let fast_embeddings = chunk_texts
        .iter()
        .zip(points.iter())
        .map(|(chunk, point)| FastEmbeddingManifestRecord {
            file_id: chunk.file.id.clone(),
            chunk_id: chunk.chunk.id.clone(),
            content_hash: chunk.file.content_hash.clone(),
            text_hash: chunk.chunk.text_hash.clone(),
            vector_point_id: point.id.clone(),
        })
        .collect::<Vec<_>>();
    on_progress(IndexProgress::new(
        "vector",
        5,
        5,
        format!("Upserted {} vector points", points.len()),
    ));
    Ok(PreparedSemanticIndex::Completed(
        PreparedFastSemanticIndex {
            model: embed_config.model.clone(),
            dimension,
            vector_table,
            fast_embeddings,
            files_seen: collection.files_seen,
        },
    ))
}

fn finalize_semantic_index(
    sqlite: &mut SqliteStore,
    root: &RepoRoot,
    store_config: &StoreConfig,
    layered_embed_config: &LayeredEmbedConfig,
    prepared: PreparedSemanticIndex,
    stale_vector_point_ids: &BTreeSet<String>,
    on_progress: &mut impl FnMut(IndexProgress),
) -> Result<EmbeddingSummary, String> {
    let prepared = match prepared {
        PreparedSemanticIndex::SkippedNoChunks => {
            delete_stale_vector_points(
                root,
                store_config,
                &layered_embed_config.fast_embed_config().model,
                stale_vector_point_ids,
                &BTreeSet::new(),
                on_progress,
            )?;
            return Ok(EmbeddingSummary::SkippedNoChunks);
        }
        PreparedSemanticIndex::Completed(prepared) => prepared,
    };

    let recorded_at = current_timestamp();
    let generation = sqlite
        .record_fast_semantic_generation(FastSemanticGenerationInput {
            repository_id: root.id(),
            fast_model: &prepared.model,
            fast_dimension: prepared.dimension,
            vector_table: &prepared.vector_table,
            upserted_embeddings: &prepared.fast_embeddings,
            files_seen: prepared.files_seen,
            completed_at: &recorded_at,
        })
        .map_err(|error| error.to_string())?;
    queue_quality_jobs_after_fast_indexing(
        sqlite,
        root.id(),
        layered_embed_config,
        &generation,
        on_progress,
    )?;
    delete_stale_vector_points(
        root,
        store_config,
        &prepared.model,
        stale_vector_point_ids,
        &prepared.point_ids(),
        on_progress,
    )?;
    Ok(EmbeddingSummary::Completed {
        model: prepared.model,
        dimension: prepared.dimension,
        vector_table: prepared.vector_table,
        chunks_embedded: prepared.fast_embeddings.len(),
    })
}

fn queue_quality_jobs_after_fast_indexing(
    sqlite: &mut SqliteStore,
    repository_id: &str,
    layered_embed_config: &LayeredEmbedConfig,
    generation: &symdex_store::SemanticGenerationRecord,
    on_progress: &mut impl FnMut(IndexProgress),
) -> Result<(), String> {
    if !layered_embed_config.quality_enabled {
        on_progress(IndexProgress::new(
            "quality_queue",
            1,
            1,
            "Quality indexing disabled; no jobs queued",
        ));
        return Ok(());
    }

    let queued_at = current_timestamp();
    let quality_config = layered_embed_config.quality_embed_config();
    let quality_client = match OllamaClient::new(quality_config.clone()) {
        Ok(client) => client,
        Err(error) => {
            let summary = sqlite
                .mark_quality_generation_blocked(generation, &quality_config.model, &queued_at)
                .map_err(|error| error.to_string())?;
            on_progress(blocked_quality_queue_progress(&summary, &error.to_string()));
            return Ok(());
        }
    };

    match quality_client.model_available() {
        Ok(true) => {
            let carried = sqlite
                .carry_forward_quality_embeddings_for_fast_generation(
                    repository_id,
                    &generation.id,
                    &quality_config.model,
                )
                .map_err(|error| error.to_string())?;
            let jobs = sqlite
                .quality_embedding_jobs_for_fast_generation(
                    repository_id,
                    &generation.id,
                    &quality_config.model,
                    &queued_at,
                )
                .map_err(|error| error.to_string())?;
            if jobs.is_empty() {
                if carried.carried_embeddings > 0 {
                    let summary = sqlite
                        .queue_quality_embedding_jobs(
                            generation,
                            &quality_config.model,
                            &[],
                            &queued_at,
                        )
                        .map_err(|error| error.to_string())?;
                    let activation = sqlite
                        .refresh_quality_activation(
                            repository_id,
                            &generation.id,
                            carried.quality_dimension,
                            &queued_at,
                        )
                        .map_err(|error| error.to_string())?;
                    on_progress(IndexProgress::new(
                        "quality_queue",
                        1,
                        1,
                        format!(
                            "Reused {} quality embeddings for {}; queued {} jobs, marked {} old jobs stale; status={} active_layer={}",
                            carried.carried_embeddings,
                            summary.quality_model,
                            summary.queued_jobs,
                            summary.skipped_stale_jobs,
                            activation.quality_status.as_str(),
                            activation.active_layer.as_str()
                        ),
                    ));
                    return Ok(());
                }
                on_progress(IndexProgress::new(
                    "quality_queue",
                    1,
                    1,
                    "Quality queue skipped because no embeddable chunks are current",
                ));
                return Ok(());
            }
            let summary = sqlite
                .queue_quality_embedding_jobs(generation, &quality_config.model, &jobs, &queued_at)
                .map_err(|error| error.to_string())?;
            on_progress(IndexProgress::new(
                "quality_queue",
                1,
                1,
                format!(
                    "Queued {} quality jobs for {}; reused {} quality embeddings, marked {} old jobs stale",
                    summary.queued_jobs,
                    summary.quality_model,
                    carried.carried_embeddings,
                    summary.skipped_stale_jobs
                ),
            ));
            Ok(())
        }
        Ok(false) => {
            let summary = sqlite
                .mark_quality_generation_blocked(generation, &quality_config.model, &queued_at)
                .map_err(|error| error.to_string())?;
            on_progress(blocked_quality_queue_progress(
                &summary,
                "quality model is not available",
            ));
            Ok(())
        }
        Err(error) => {
            let summary = sqlite
                .mark_quality_generation_blocked(generation, &quality_config.model, &queued_at)
                .map_err(|error| error.to_string())?;
            on_progress(blocked_quality_queue_progress(&summary, &error.to_string()));
            Ok(())
        }
    }
}

fn blocked_quality_queue_progress(summary: &QualityQueueSummary, reason: &str) -> IndexProgress {
    IndexProgress::new(
        "quality_queue",
        1,
        1,
        format!(
            "Quality queue blocked for {}; marked {} old jobs stale ({})",
            summary.quality_model, summary.skipped_stale_jobs, reason
        ),
    )
}

#[derive(Debug, Default)]
struct QualityWorkerStats {
    claimed_jobs: usize,
    succeeded_jobs: usize,
    failed_jobs: usize,
    skipped_stale_jobs: usize,
    skipped_excluded_jobs: usize,
}

fn quality_activation_progress_message(
    stats: &QualityWorkerStats,
    activation: &QualityActivationSummary,
) -> String {
    format!(
        "Quality worker completed: {} succeeded, {} failed, {} stale, {} excluded; status={} active_layer={} reason={}",
        stats.succeeded_jobs,
        stats.failed_jobs,
        stats.skipped_stale_jobs,
        stats.skipped_excluded_jobs,
        activation.quality_status.as_str(),
        activation.active_layer.as_str(),
        activation.reason.as_str()
    )
}

struct QualityWorkerContext<'a> {
    root: &'a RepoRoot,
    vector: &'a SqliteVectorStore,
    embed_client: &'a OllamaClient,
    latest_generation_id: &'a str,
    quality_model: &'a str,
    quality_max_chunk_bytes: usize,
    vector_table: &'a str,
}

fn process_quality_job(
    context: &QualityWorkerContext<'_>,
    sqlite: &mut SqliteStore,
    quality_dimension: &mut Option<usize>,
    stats: &mut QualityWorkerStats,
    row: QualityJobSourceRow,
) -> Result<(), String> {
    let completed_at = current_timestamp();
    let prepared = match prepare_quality_job(
        context.root,
        context.latest_generation_id,
        context.quality_max_chunk_bytes,
        &row,
    ) {
        Ok(QualityJobPreparation::Ready(prepared)) => *prepared,
        Ok(QualityJobPreparation::Stale) => {
            sqlite
                .complete_quality_embedding_job(
                    &row.job.id,
                    QualityJobCompletion::SkippedStale,
                    &completed_at,
                )
                .map_err(|error| error.to_string())?;
            stats.skipped_stale_jobs += 1;
            return Ok(());
        }
        Ok(QualityJobPreparation::Excluded { reason }) => {
            sqlite
                .complete_quality_embedding_job(
                    &row.job.id,
                    QualityJobCompletion::SkippedExcluded { reason },
                    &completed_at,
                )
                .map_err(|error| error.to_string())?;
            stats.skipped_excluded_jobs += 1;
            return Ok(());
        }
        Err(error) => {
            sqlite
                .complete_quality_embedding_job(
                    &row.job.id,
                    QualityJobCompletion::Failed {
                        error_summary: error_summary(&error),
                    },
                    &completed_at,
                )
                .map_err(|store_error| store_error.to_string())?;
            stats.failed_jobs += 1;
            return Ok(());
        }
    };

    let embeddings = match context
        .embed_client
        .embed_batch(std::slice::from_ref(&prepared.text))
    {
        Ok(embeddings) => embeddings,
        Err(error) => {
            sqlite
                .complete_quality_embedding_job(
                    &prepared.row.job.id,
                    QualityJobCompletion::Failed {
                        error_summary: error_summary(&error.to_string()),
                    },
                    &completed_at,
                )
                .map_err(|store_error| store_error.to_string())?;
            stats.failed_jobs += 1;
            return Ok(());
        }
    };
    let dimension = embeddings.dimension().unwrap_or(0);
    if dimension == 0 {
        complete_failed_quality_job(
            sqlite,
            &prepared.row.job.id,
            "quality embedding returned an empty vector",
            &completed_at,
            stats,
        )?;
        return Ok(());
    }
    if let Some(previous_dimension) = *quality_dimension
        && previous_dimension != dimension
    {
        complete_failed_quality_job(
            sqlite,
            &prepared.row.job.id,
            &format!(
                "quality embedding dimension changed: previous={previous_dimension} current={dimension}"
            ),
            &completed_at,
            stats,
        )?;
        return Ok(());
    }
    if let Err(error) =
        sqlite.ensure_embedding_compatible(context.root.id(), context.quality_model, dimension)
    {
        complete_failed_quality_job(
            sqlite,
            &prepared.row.job.id,
            &error.to_string(),
            &completed_at,
            stats,
        )?;
        return Ok(());
    }
    if let Err(error) = context.vector.ensure_table(context.vector_table, dimension) {
        complete_failed_quality_job(
            sqlite,
            &prepared.row.job.id,
            &error.to_string(),
            &completed_at,
            stats,
        )?;
        return Ok(());
    }

    let Some(vector) = embeddings.embeddings.into_iter().next() else {
        complete_failed_quality_job(
            sqlite,
            &prepared.row.job.id,
            "quality embedding response did not include a vector",
            &completed_at,
            stats,
        )?;
        return Ok(());
    };
    let point = match quality_vector_point(
        context.root.id(),
        &prepared.row,
        vector,
        context.quality_model,
        dimension,
        &completed_at,
    ) {
        Ok(point) => point,
        Err(error) => {
            complete_failed_quality_job(
                sqlite,
                &prepared.row.job.id,
                &error,
                &completed_at,
                stats,
            )?;
            return Ok(());
        }
    };
    if let Err(error) = context
        .vector
        .upsert_points(context.vector_table, std::slice::from_ref(&point))
    {
        complete_failed_quality_job(
            sqlite,
            &prepared.row.job.id,
            &error.to_string(),
            &completed_at,
            stats,
        )?;
        return Ok(());
    }

    let embedding = quality_chunk_embedding_record(
        context.root.id(),
        &prepared.row,
        context.quality_model,
        dimension,
        context.vector_table,
        &point.id,
        &completed_at,
    );
    sqlite
        .complete_quality_embedding_job(
            &prepared.row.job.id,
            QualityJobCompletion::Succeeded {
                embedding: Box::new(embedding),
            },
            &completed_at,
        )
        .map_err(|error| error.to_string())?;
    *quality_dimension = Some(dimension);
    stats.succeeded_jobs += 1;
    Ok(())
}

fn complete_failed_quality_job(
    sqlite: &mut SqliteStore,
    job_id: &str,
    error: &str,
    completed_at: &str,
    stats: &mut QualityWorkerStats,
) -> Result<(), String> {
    sqlite
        .complete_quality_embedding_job(
            job_id,
            QualityJobCompletion::Failed {
                error_summary: error_summary(error),
            },
            completed_at,
        )
        .map_err(|store_error| store_error.to_string())?;
    stats.failed_jobs += 1;
    Ok(())
}

#[derive(Debug)]
struct PreparedQualityJob {
    row: QualityJobSourceRow,
    text: String,
}

#[derive(Debug)]
enum QualityJobPreparation {
    Ready(Box<PreparedQualityJob>),
    Stale,
    Excluded { reason: String },
}

fn prepare_quality_job(
    root: &RepoRoot,
    latest_generation_id: &str,
    max_chunk_bytes: usize,
    row: &QualityJobSourceRow,
) -> Result<QualityJobPreparation, String> {
    if row.job.generation_id != latest_generation_id {
        return Ok(QualityJobPreparation::Stale);
    }
    if row.current_file_id.as_deref() != Some(row.job.file_id.as_str())
        || row.current_content_hash.as_deref() != Some(row.job.content_hash.as_str())
        || row.current_text_hash.as_deref() != Some(row.job.text_hash.as_str())
    {
        return Ok(QualityJobPreparation::Stale);
    }
    if let Some(reason) = &row.excluded_reason {
        return Ok(QualityJobPreparation::Excluded {
            reason: reason.clone(),
        });
    }
    let (Some(start_byte), Some(end_byte)) = (row.start_byte, row.end_byte) else {
        return Ok(QualityJobPreparation::Stale);
    };
    if start_byte >= end_byte {
        return Ok(QualityJobPreparation::Stale);
    }
    if end_byte.saturating_sub(start_byte) > max_chunk_bytes {
        return Ok(QualityJobPreparation::Excluded {
            reason: "quality_chunk_too_large_for_embedding".to_owned(),
        });
    }
    let normalized = NormalizedRepoPath::new(&row.job.path).map_err(|error| error.to_string())?;
    let path = root.path().join(normalized.as_str());
    let source =
        fs::read_to_string(&path).map_err(|_| "source file is no longer readable".to_owned())?;
    let normalized_existing = root
        .normalize_existing_path(&path)
        .map_err(|error| error.to_string())?;
    if normalized_existing.as_str() != row.job.path {
        return Ok(QualityJobPreparation::Stale);
    }
    if content_hash(source.as_bytes()) != row.job.content_hash {
        return Ok(QualityJobPreparation::Stale);
    }
    let Some(text) = source.get(start_byte..end_byte).map(str::to_owned) else {
        return Ok(QualityJobPreparation::Stale);
    };
    if content_hash(text.as_bytes()) != row.job.text_hash {
        return Ok(QualityJobPreparation::Stale);
    }
    Ok(QualityJobPreparation::Ready(Box::new(PreparedQualityJob {
        row: row.clone(),
        text,
    })))
}

fn quality_vector_point(
    repository_id: &str,
    row: &QualityJobSourceRow,
    vector: Vec<f32>,
    embedding_model: &str,
    embedding_dimension: usize,
    indexed_at: &str,
) -> Result<VectorPoint, String> {
    Ok(VectorPoint {
        id: vector_point_id(&row.job.chunk_id).map_err(|error| error.to_string())?,
        vector,
        payload: PointPayload {
            repository_id: repository_id.to_owned(),
            file_id: row.job.file_id.clone(),
            chunk_id: row.job.chunk_id.clone(),
            symbol_id: row.symbol_id.clone(),
            symbol_name: row.symbol_name.clone(),
            path: row.job.path.clone(),
            language: row.language.clone().unwrap_or_default(),
            chunk_kind: row.chunk_kind.clone().unwrap_or_default(),
            start_line: row.start_line.unwrap_or_default(),
            end_line: row.end_line.unwrap_or_default(),
            text_hash: row.job.text_hash.clone(),
            parser_version: row.parser_version.clone(),
            content_hash: Some(row.job.content_hash.clone()),
            index_run_id: row.index_run_id.clone(),
            embedding_model: Some(embedding_model.to_owned()),
            embedding_dimension: Some(embedding_dimension),
            indexed_at: Some(indexed_at.to_owned()),
        },
    })
}

fn quality_chunk_embedding_record(
    repository_id: &str,
    row: &QualityJobSourceRow,
    embedding_model: &str,
    embedding_dimension: usize,
    vector_table: &str,
    vector_point_id: &str,
    embedded_at: &str,
) -> ChunkEmbeddingRecord {
    let semantic_layer = SemanticLayer::Quality.as_str();
    ChunkEmbeddingRecord {
        id: SqliteStore::chunk_embedding_id(
            repository_id,
            &row.job.generation_id,
            &row.job.chunk_id,
            semantic_layer,
            embedding_model,
            embedding_dimension,
        ),
        repository_id: repository_id.to_owned(),
        file_id: row.job.file_id.clone(),
        chunk_id: row.job.chunk_id.clone(),
        semantic_layer: semantic_layer.to_owned(),
        embedding_model: embedding_model.to_owned(),
        embedding_dimension,
        content_hash: row.job.content_hash.clone(),
        text_hash: row.job.text_hash.clone(),
        vector_table: vector_table.to_owned(),
        vector_point_id: vector_point_id.to_owned(),
        generation_id: row.job.generation_id.clone(),
        embedded_at: embedded_at.to_owned(),
        status: "current".to_owned(),
    }
}

fn stale_vector_point_ids(
    sqlite: &SqliteStore,
    root: &RepoRoot,
    collection: &IndexCollection,
) -> Result<BTreeSet<String>, String> {
    let changed_paths = collection
        .reports
        .iter()
        .map(|report| report.file.relative_path.clone())
        .collect::<Vec<_>>();
    let mut point_ids = BTreeSet::new();
    point_ids.extend(
        sqlite
            .vector_point_ids_for_latest_generation_layer_paths(
                root.id(),
                SemanticLayer::Fast,
                &changed_paths,
            )
            .map_err(|error| error.to_string())?,
    );
    point_ids.extend(
        sqlite
            .vector_point_ids_for_latest_generation_layer_missing_files(
                root.id(),
                SemanticLayer::Fast,
                &collection.active_paths,
            )
            .map_err(|error| error.to_string())?,
    );

    Ok(point_ids)
}

fn delete_stale_vector_points(
    root: &RepoRoot,
    store_config: &StoreConfig,
    embedding_model: &str,
    stale_point_ids: &BTreeSet<String>,
    protected_point_ids: &BTreeSet<String>,
    on_progress: &mut impl FnMut(IndexProgress),
) -> Result<usize, String> {
    let point_ids = stale_vector_point_ids_to_delete(stale_point_ids, protected_point_ids);

    if point_ids.is_empty() {
        on_progress(IndexProgress::new(
            "vector_cleanup",
            1,
            1,
            "No stale vector points to delete",
        ));
        return Ok(0);
    }

    let vector = SqliteVectorStore::new(store_config).map_err(|error| error.to_string())?;
    let vector_table = vector_table_name(root.id(), embedding_model);
    if !vector
        .table_exists(&vector_table)
        .map_err(|error| error.to_string())?
    {
        on_progress(IndexProgress::new(
            "vector_cleanup",
            1,
            1,
            format!("Skipped stale point deletion; collection {vector_table} is missing"),
        ));
        return Ok(0);
    }

    on_progress(IndexProgress::new(
        "vector_cleanup",
        0,
        point_ids.len(),
        format!("Deleting {} stale vector points", point_ids.len()),
    ));
    vector
        .delete_points(&vector_table, &point_ids)
        .map_err(|error| error.to_string())?;
    on_progress(IndexProgress::new(
        "vector_cleanup",
        point_ids.len(),
        point_ids.len(),
        format!("Deleted {} stale vector points", point_ids.len()),
    ));
    Ok(point_ids.len())
}

fn stale_vector_point_ids_to_delete(
    stale_point_ids: &BTreeSet<String>,
    protected_point_ids: &BTreeSet<String>,
) -> Vec<String> {
    stale_point_ids
        .difference(protected_point_ids)
        .cloned()
        .collect()
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

fn apply_embedding_size_limits(reports: &mut [IndexReport], max_chunk_bytes: usize) -> usize {
    let mut excluded = 0;
    for report in reports {
        for chunk in &mut report.chunks {
            let chunk_bytes = chunk.byte_range.end.saturating_sub(chunk.byte_range.start);
            if chunk.excluded_reason.is_none() && chunk_bytes > max_chunk_bytes {
                chunk.excluded_reason = Some("chunk_too_large_for_embedding".to_owned());
                excluded += 1;
            }
        }
    }
    excluded
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
        id: vector_point_id(&chunk.chunk.id).map_err(|error| error.to_string())?,
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
        vector_point_id: None,
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
        repository_ref_id: scope.repository_ref_id.map(str::to_owned),
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
    use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
    use std::time::{SystemTime, UNIX_EPOCH};

    use symdex_core::{
        ByteRange, CallEdge, ChunkKind, CodeChunk, FileFacts, Language, LineRange, RepoRoot,
        ResolutionStatus, Symbol, SymbolKind, content_hash, stable_id,
    };
    use symdex_embed::{LayeredEmbedConfig, LayeredEmbedConfigValues};
    use symdex_store::{
        FileRecord, QualityEmbeddingJobRecord, QualityJobSourceRow, RepositoryRecord, SqliteStore,
        StoreConfig, SymbolRecord,
    };

    use crate::{
        ContinuousIndexOptions, IndexCollection, IndexReport, IndexScope, QualityJobPreparation,
        RustAnalyzerEnrichmentConfig, RustAnalyzerEnrichmentSummary, RustAnalyzerReadiness,
        WatchSnapshot, apply_embedding_size_limits, chunk_record, chunk_texts,
        collect_index_reports, detect_watch_changes, diff_watch_snapshots, index_run_kind,
        plan_rust_analyzer_enrichment, prepare_quality_job, quality_chunk_embedding_record,
        quality_vector_point, resolve_cross_file_rust_calls,
        should_run_continuous_quality_catch_up, watch_snapshot,
    };

    #[test]
    fn index_scope_controls_unchanged_file_skipping() {
        assert!(!IndexScope::Full.skips_unchanged());
        assert!(IndexScope::Incremental.skips_unchanged());
        assert_eq!(IndexScope::Full.label(), "full");
        assert_eq!(IndexScope::Incremental.label(), "incremental");
    }

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
        assert!(public_record.vector_point_id.is_none());
        assert!(secret_record.vector_point_id.is_none());
        assert_eq!(
            secret_record.excluded_reason.as_deref(),
            Some("likely_access_token")
        );
    }

    #[test]
    fn prepare_quality_job_extracts_hash_verified_text() {
        let repo = TestRepo::new("quality-prepare-success");
        let source = "pub fn public() {}\n";
        repo.write("src/lib.rs", source);
        let root = RepoRoot::open(repo.path()).expect("repo root should open");
        let row = sample_quality_source_row(source, 0, source.len(), None);

        let prepared = match prepare_quality_job(&root, "generation-1", source.len(), &row)
            .expect("prepare should not fail")
        {
            QualityJobPreparation::Ready(prepared) => prepared,
            other => panic!("job should be current, got {other:?}"),
        };

        assert_eq!(prepared.text, source);
        assert_eq!(prepared.row.job.id, "quality-job-1");
    }

    #[test]
    fn prepare_quality_job_skips_stale_or_excluded_rows() {
        let repo = TestRepo::new("quality-prepare-stale");
        let source = "pub fn public() {}\n";
        repo.write("src/lib.rs", source);
        let root = RepoRoot::open(repo.path()).expect("repo root should open");
        let excluded = sample_quality_source_row(source, 0, source.len(), Some("secret_detected"));
        let mut stale_hash = sample_quality_source_row(source, 0, source.len(), None);
        stale_hash.job.text_hash = "stale-text".to_owned();

        assert!(matches!(
            prepare_quality_job(&root, "generation-1", source.len(), &excluded)
                .expect("excluded should not fail"),
            QualityJobPreparation::Excluded { .. }
        ));
        assert!(matches!(
            prepare_quality_job(&root, "generation-1", source.len(), &stale_hash)
                .expect("stale should not fail"),
            QualityJobPreparation::Stale
        ));
    }

    #[test]
    fn prepare_quality_job_skips_chunks_over_quality_size_limit() {
        let repo = TestRepo::new("quality-prepare-too-large");
        let source = "pub fn public() {}\n";
        repo.write("src/lib.rs", source);
        let root = RepoRoot::open(repo.path()).expect("repo root should open");
        let row = sample_quality_source_row(source, 0, source.len(), None);

        assert!(matches!(
            prepare_quality_job(&root, "generation-1", 8, &row)
                .expect("oversized quality job should not fail"),
            QualityJobPreparation::Excluded { reason }
                if reason == "quality_chunk_too_large_for_embedding"
        ));
    }

    #[test]
    fn quality_metadata_builders_do_not_include_source_text() {
        let source = "pub fn public() {}\n";
        let row = sample_quality_source_row(source, 0, source.len(), None);
        let point = quality_vector_point(
            "repo",
            &row,
            vec![0.1, 0.2, 0.3],
            "mxbai-embed-large",
            3,
            "700",
        )
        .expect("point should build");
        let embedding = quality_chunk_embedding_record(
            "repo",
            &row,
            "mxbai-embed-large",
            3,
            "symdex_repo_nomic_embed_text_v2_moe",
            &point.id,
            "700",
        );

        assert_eq!(point.payload.path, "src/lib.rs");
        assert_eq!(
            point.payload.embedding_model.as_deref(),
            Some("mxbai-embed-large")
        );
        assert_eq!(point.payload.embedding_dimension, Some(3));
        assert_eq!(
            point.payload.content_hash.as_deref(),
            Some(row.job.content_hash.as_str())
        );
        assert_eq!(point.payload.text_hash, row.job.text_hash);
        assert_eq!(embedding.semantic_layer, "quality");
        assert_eq!(embedding.embedding_dimension, 3);
        assert_eq!(embedding.status, "current");
    }

    #[test]
    fn embedding_size_limits_exclude_oversized_chunks_before_embedding() {
        let file = sample_file();
        let source = "a".repeat(128);
        let small = sample_chunk("small", 0, 16, None);
        let large = sample_chunk("large", 16, 128, None);
        let mut reports = vec![IndexReport {
            file,
            chunks: vec![small.clone(), large.clone()],
            symbols: Vec::new(),
            calls: Vec::new(),
            tests: Vec::new(),
            parse_diagnostics: Vec::new(),
            source,
        }];

        let excluded = apply_embedding_size_limits(&mut reports, 64);
        let chunks = chunk_texts(&reports);

        assert_eq!(excluded, 1);
        assert_eq!(reports[0].chunks[0].excluded_reason, None);
        assert_eq!(
            reports[0].chunks[1].excluded_reason.as_deref(),
            Some("chunk_too_large_for_embedding")
        );
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].chunk.id, small.id);
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
    fn index_run_kind_distinguishes_watch_batches() {
        assert_eq!(index_run_kind(false, None), "semantic");
        assert_eq!(index_run_kind(true, None), "offline");
        assert_eq!(index_run_kind(false, Some("watch")), "watch");
        assert_eq!(index_run_kind(true, Some("watch")), "watch");
    }

    #[test]
    fn continuous_quality_catch_up_policy_requires_semantic_watch_and_quality_enabled() {
        let enabled = LayeredEmbedConfig::from_values(LayeredEmbedConfigValues::default());
        let disabled = LayeredEmbedConfig::from_values(LayeredEmbedConfigValues {
            quality_enabled: Some("0"),
            ..LayeredEmbedConfigValues::default()
        });
        let semantic_watch = ContinuousIndexOptions::new(".", false);
        let offline_watch = ContinuousIndexOptions::new(".", true);
        let mut no_catch_up = ContinuousIndexOptions::new(".", false);
        no_catch_up.quality_catch_up = false;

        assert!(should_run_continuous_quality_catch_up(
            &semantic_watch,
            &enabled
        ));
        assert!(!should_run_continuous_quality_catch_up(
            &offline_watch,
            &enabled
        ));
        assert!(!should_run_continuous_quality_catch_up(
            &semantic_watch,
            &disabled
        ));
        assert!(!should_run_continuous_quality_catch_up(
            &no_catch_up,
            &enabled
        ));
    }

    #[test]
    fn deferred_vector_cleanup_keeps_newly_upserted_point_ids() {
        let stale = BTreeSet::from([
            "deleted-file-point".to_owned(),
            "unchanged-deterministic-point".to_owned(),
        ]);
        let protected = BTreeSet::from(["unchanged-deterministic-point".to_owned()]);

        let to_delete = super::stale_vector_point_ids_to_delete(&stale, &protected);

        assert_eq!(to_delete, vec!["deleted-file-point".to_owned()]);
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
    fn detect_watch_changes_respects_gitignore_globs_and_negation() {
        let repo = TestRepo::new("watch-glob-negation");
        repo.write(".gitignore", "*.generated.rs\n!src/keep.generated.rs\n");
        repo.write("src/lib.rs", "pub fn lib() {}\n");
        let root = RepoRoot::open(repo.path()).expect("repo root should open");
        let snapshot = watch_snapshot(&root).expect("snapshot should load");

        repo.write("src/drop.generated.rs", "pub fn ignored() {}\n");
        repo.write("src/keep.generated.rs", "pub fn kept() {}\n");
        let (_next, changes) =
            detect_watch_changes(&root, &snapshot).expect("changes should detect");

        assert_eq!(changes.created, vec!["src/keep.generated.rs"]);
        assert!(changes.modified.is_empty());
        assert!(changes.deleted.is_empty());
    }

    #[test]
    fn incremental_collection_skips_unchanged_files_with_gitignore_globs() {
        let repo = TestRepo::new("incremental-glob-unchanged");
        let lib_source = "pub fn lib() {}\n";
        repo.write(".gitignore", "*.generated.rs\n!src/keep.generated.rs\n");
        repo.write("src/lib.rs", lib_source);
        repo.write("src/drop.generated.rs", "pub fn ignored() {}\n");
        repo.write("src/keep.generated.rs", "pub fn kept() {}\n");
        let root = RepoRoot::open(repo.path()).expect("repo root should open");
        let db_dir = temp_path("incremental-glob-unchanged-db");
        fs::create_dir_all(&db_dir).expect("db directory should be created");
        let mut store = SqliteStore::open(&StoreConfig {
            sqlite_path: db_dir.join("symdex.sqlite"),
        })
        .expect("store should open");
        store.migrate().expect("store should migrate");
        store
            .upsert_repository(&RepositoryRecord {
                id: root.id().to_owned(),
                root_path: root.path().display().to_string(),
            })
            .expect("repository should persist");
        store
            .replace_file_facts(
                &FileRecord {
                    id: stable_id(&[root.id(), "src/lib.rs"]),
                    repository_id: root.id().to_owned(),
                    path: "src/lib.rs".to_owned(),
                    language: "rust".to_owned(),
                    content_hash: content_hash(lib_source.as_bytes()),
                    index_run_id: "run".to_owned(),
                    parser_version: Language::Rust.parser_version().to_owned(),
                },
                &[],
                &[],
                &[],
            )
            .expect("file facts should persist");

        let collection = collect_index_reports(&root, Some(&store), &mut |_| {})
            .expect("collection should succeed");
        let _ = fs::remove_dir_all(db_dir);

        assert_eq!(collection.files_seen, 2);
        assert_eq!(collection.files_skipped_unchanged, 1);
        assert_eq!(collection.reports.len(), 1);
        assert_eq!(
            collection.reports[0].file.relative_path,
            "src/keep.generated.rs"
        );
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
    fn resolves_cross_file_receiver_method_calls_from_caller_receiver() {
        let mut caller = sample_symbol("run");
        caller.id = stable_id(&["symbol", "Worker::run"]);
        caller.qualified_name = "Worker::run".to_owned();
        caller.kind = SymbolKind::Method;
        let mut self_call = sample_unresolved_call("Worker::run", "self.helper");
        self_call.caller_symbol_id = caller.id.clone();
        let mut self_type_call = sample_unresolved_call("Worker::run", "Self::static_helper");
        self_type_call.caller_symbol_id = caller.id.clone();
        let mut collection = collection_with_reports(vec![IndexReport {
            file: FileFacts {
                id: "file-worker-run".to_owned(),
                relative_path: "src/worker.rs".to_owned(),
                language: Language::Rust,
                content_hash: content_hash(b"worker-run"),
            },
            chunks: Vec::new(),
            symbols: vec![caller],
            calls: vec![self_call, self_type_call],
            tests: Vec::new(),
            parse_diagnostics: Vec::new(),
            source: String::new(),
        }]);
        let mut helper = sample_symbol_record("Worker::helper", "file-worker-methods");
        helper.kind = SymbolKind::Method.as_str().to_owned();
        let mut static_helper =
            sample_symbol_record("Worker::static_helper", "file-worker-methods");
        static_helper.kind = SymbolKind::Method.as_str().to_owned();

        resolve_cross_file_rust_calls(&mut collection, &[helper.clone(), static_helper.clone()]);

        let self_call = &collection.reports[0].calls[0];
        assert_eq!(
            self_call.callee_symbol_id.as_deref(),
            Some(helper.id.as_str())
        );
        assert_eq!(self_call.resolution_status, ResolutionStatus::ResolvedExact);

        let self_type_call = &collection.reports[0].calls[1];
        assert_eq!(
            self_type_call.callee_symbol_id.as_deref(),
            Some(static_helper.id.as_str())
        );
        assert_eq!(
            self_type_call.resolution_status,
            ResolutionStatus::ResolvedExact
        );
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

    fn sample_quality_source_row(
        source: &str,
        start_byte: usize,
        end_byte: usize,
        excluded_reason: Option<&str>,
    ) -> QualityJobSourceRow {
        let text = &source[start_byte..end_byte];
        let source_hash = content_hash(source.as_bytes());
        let text_hash = content_hash(text.as_bytes());
        QualityJobSourceRow {
            job: QualityEmbeddingJobRecord {
                id: "quality-job-1".to_owned(),
                repository_id: "repo".to_owned(),
                generation_id: "generation-1".to_owned(),
                chunk_id: stable_id(&["chunk", "quality-fixture"]),
                file_id: "file-1".to_owned(),
                path: "src/lib.rs".to_owned(),
                content_hash: source_hash.clone(),
                text_hash: text_hash.clone(),
                status: "running".to_owned(),
                attempts: 1,
                error_summary: None,
                created_at: "500".to_owned(),
                updated_at: "501".to_owned(),
            },
            current_file_id: Some("file-1".to_owned()),
            current_content_hash: Some(source_hash),
            language: Some("rust".to_owned()),
            chunk_kind: Some("function".to_owned()),
            current_text_hash: Some(text_hash),
            start_line: Some(1),
            end_line: Some(1),
            start_byte: Some(start_byte),
            end_byte: Some(end_byte),
            excluded_reason: excluded_reason.map(str::to_owned),
            symbol_id: Some("symbol-1".to_owned()),
            symbol_name: Some("public".to_owned()),
            index_run_id: Some("run-1".to_owned()),
            parser_version: Some(Language::Rust.parser_version().to_owned()),
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

    static NEXT_TEST_REPO_ID: AtomicU64 = AtomicU64::new(0);

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
        let id = NEXT_TEST_REPO_ID.fetch_add(1, AtomicOrdering::Relaxed);
        std::env::temp_dir().join(format!(
            "symdex-index-{name}-{}-{nonce}-{id}",
            std::process::id(),
        ))
    }
}

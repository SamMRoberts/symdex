//! Indexing orchestration shared by the CLI and TUI.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::thread;
use std::time::Duration;

use symdex_core::{
    CallEdge, CodeChunk, DiscoveryOptions, FileFacts, RepoRoot, Symbol, discover_indexable_files,
    index_source_file,
};
use symdex_embed::{EmbedConfig, OllamaClient};
use symdex_store::{
    CallRecord, ChunkRecord, FileRecord, IndexRunRecord, PointPayload, QdrantClient,
    RepositoryRecord, SqliteStore, StoreConfig, SymbolRecord, VectorPoint, current_timestamp,
    qdrant_collection_name, qdrant_point_id,
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
    pub embedding: EmbeddingSummary,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileIndexSummary {
    pub path: String,
    pub language: String,
    pub content_hash: String,
    pub chunks: Vec<ChunkIndexSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChunkIndexSummary {
    pub kind: String,
    pub start_line: usize,
    pub end_line: usize,
    pub symbol: Option<String>,
    pub excluded_reason: Option<String>,
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
        summary: IndexSummary,
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
                on_event(ContinuousIndexEvent::BatchCompleted { changes, summary });
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
    let collection = collect_index_reports(
        &root,
        if skip_unchanged { Some(&sqlite) } else { None },
        &mut on_progress,
    )?;
    let files = file_summaries(&collection.reports);
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
    let persistence = persist_structural_index(
        &mut sqlite,
        &root,
        &collection,
        &index_run_id,
        &mut on_progress,
    )?;

    let embedding = if options.offline {
        on_progress(IndexProgress::new(
            "embedding",
            1,
            1,
            "Embedding skipped for offline indexing",
        ));
        sqlite
            .record_index_run(&IndexRunRecord {
                id: index_run_id.clone(),
                repository_id: root.id().to_owned(),
                status: "success".to_owned(),
                embedding_model: "offline".to_owned(),
                embedding_dimension: None,
                files_seen: collection.files_seen,
                files_indexed: collection.reports.len(),
                chunks_embedded: 0,
                error_summary: None,
                parser_version: parser_version_summary(&collection),
                indexer_version: env!("CARGO_PKG_VERSION").to_owned(),
                run_kind: run_kind.to_owned(),
            })
            .map_err(|error| error.to_string())?;
        EmbeddingSummary::SkippedOffline
    } else {
        persist_semantic_index(
            &sqlite,
            &root,
            &store_config,
            &collection,
            &index_run_id,
            run_kind,
            &mut on_progress,
        )?
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
        reports.push(IndexReport {
            file: file.facts.clone(),
            chunks: file_index.chunks,
            symbols: file_index.symbols,
            calls: file_index.calls,
            source,
        });
        on_progress(IndexProgress::new(
            "parse",
            index + 1,
            files.len(),
            format!("Parsed {}", file.facts.relative_path),
        ));
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
        })
        .collect()
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
        chunks_indexed += chunks.len();
        symbols_indexed += symbols.len();
        calls_indexed += calls.len();
        sqlite
            .replace_file_facts(&file, &symbols, &chunks, &calls)
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
    run_kind: &str,
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
    sqlite
        .record_index_run(&IndexRunRecord {
            id: index_run_id.to_owned(),
            repository_id: root.id().to_owned(),
            status: "success".to_owned(),
            embedding_model: embed_config.model.clone(),
            embedding_dimension: Some(dimension),
            files_seen: collection.files_seen,
            files_indexed: collection.reports.len(),
            chunks_embedded: points.len(),
            error_summary: None,
            parser_version: parser_version_summary(collection),
            indexer_version: env!("CARGO_PKG_VERSION").to_owned(),
            run_kind: run_kind.to_owned(),
        })
        .map_err(|error| error.to_string())?;

    Ok(EmbeddingSummary::Completed {
        model: embed_config.model,
        dimension,
        qdrant_collection,
        chunks_embedded: points.len(),
    })
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
    source: String,
}

struct ChunkText<'a> {
    file: &'a FileFacts,
    chunk: &'a CodeChunk,
    text: String,
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    use symdex_core::{
        ByteRange, ChunkKind, CodeChunk, FileFacts, Language, LineRange, RepoRoot, content_hash,
        stable_id,
    };

    use crate::{
        IndexReport, WatchSnapshot, chunk_record, chunk_texts, detect_watch_changes,
        diff_watch_snapshots, watch_snapshot,
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

    fn sample_file() -> FileFacts {
        FileFacts {
            id: "file-1".to_owned(),
            relative_path: "src/lib.rs".to_owned(),
            language: Language::Rust,
            content_hash: content_hash(b"sample"),
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

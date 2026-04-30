//! Indexing orchestration shared by the CLI and TUI.

use std::fs;

use symdex_core::{
    CallEdge, CodeChunk, DiscoveryOptions, FileFacts, RepoRoot, Symbol, discover_rust_files,
    index_rust_file,
};
use symdex_embed::{EmbedConfig, OllamaClient};
use symdex_store::{
    CallRecord, ChunkRecord, FileRecord, IndexRunRecord, PointPayload, QdrantClient,
    RepositoryRecord, SqliteStore, StoreConfig, SymbolRecord, VectorPoint, qdrant_collection_name,
    qdrant_point_id,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexOptions {
    pub repo: String,
    pub offline: bool,
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

pub fn run_index_with_progress(
    options: &IndexOptions,
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

    let collection = collect_index_reports(
        &root,
        if options.offline { Some(&sqlite) } else { None },
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
    let persistence = persist_structural_index(&mut sqlite, &root, &collection, &mut on_progress)?;

    let embedding = if options.offline {
        on_progress(IndexProgress::new(
            "embedding",
            1,
            1,
            "Embedding skipped for offline indexing",
        ));
        EmbeddingSummary::SkippedOffline
    } else {
        persist_semantic_index(&sqlite, &root, &store_config, &collection, &mut on_progress)?
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
    let files = discover_rust_files(root, &DiscoveryOptions::default())
        .map_err(|error| error.to_string())?;
    on_progress(IndexProgress::new(
        "discover",
        0,
        files.len(),
        format!("Discovered {} Rust files", files.len()),
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
            index_rust_file(&file.facts, &source).map_err(|error| error.to_string())?;
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
        };
        let chunks = report
            .chunks
            .iter()
            .map(chunk_record)
            .collect::<Result<Vec<_>, _>>()?;
        let symbols = report.symbols.iter().map(symbol_record).collect::<Vec<_>>();
        let calls = report.calls.iter().map(call_record).collect::<Vec<_>>();
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
        .map(|(chunk, vector)| vector_point(root.id(), chunk, vector))
        .collect::<Result<Vec<_>, _>>()?;
    qdrant
        .upsert_points(&qdrant_collection, &points)
        .map_err(|error| error.to_string())?;
    on_progress(IndexProgress::new(
        "qdrant",
        5,
        5,
        format!("Upserted {} vector points", points.len()),
    ));
    sqlite
        .record_index_run(&IndexRunRecord {
            repository_id: root.id().to_owned(),
            status: "success".to_owned(),
            embedding_model: embed_config.model.clone(),
            embedding_dimension: Some(dimension),
            files_seen: collection.files_seen,
            files_indexed: collection.reports.len(),
            chunks_embedded: points.len(),
            error_summary: None,
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
        },
    })
}

fn chunk_record(chunk: &CodeChunk) -> Result<ChunkRecord, String> {
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
    })
}

fn symbol_record(symbol: &Symbol) -> SymbolRecord {
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
    }
}

fn call_record(call: &CallEdge) -> CallRecord {
    CallRecord {
        id: call.id.clone(),
        caller_symbol_id: call.caller_symbol_id.clone(),
        callee_text: call.callee_text.clone(),
        callee_symbol_id: call.callee_symbol_id.clone(),
        call_line: call.call_line,
        confidence: call.confidence,
        resolution_status: call.resolution_status.as_str().to_owned(),
    }
}

struct IndexCollection {
    files_seen: usize,
    files_skipped_unchanged: usize,
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
    use symdex_core::{
        ByteRange, ChunkKind, CodeChunk, FileFacts, Language, LineRange, content_hash, stable_id,
    };

    use crate::{IndexReport, chunk_record, chunk_texts};

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
        let public_record = chunk_record(&public).expect("public record");
        let secret_record = chunk_record(&secret).expect("secret record");
        assert!(public_record.qdrant_point_id.is_some());
        assert!(secret_record.qdrant_point_id.is_none());
        assert_eq!(
            secret_record.excluded_reason.as_deref(),
            Some("likely_access_token")
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
}

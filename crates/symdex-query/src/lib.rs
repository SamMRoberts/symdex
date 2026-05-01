//! Query orchestration shared by the CLI and TUI.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::Serialize;
use symdex_core::{DiscoveryOptions, RepoRoot, discover_rust_files};
use symdex_embed::{EmbedConfig, OllamaClient};
use symdex_store::{
    CallPath, CallResolutionSummary, CallSearchRow, ContextPack, CrossStoreHealthSummary,
    EmbeddingCoverageSummary, EvidenceFreshness, EvidenceProvenance, FileFreshnessSnapshot,
    IndexCoverageSummary, IndexRunsTimelineSummary, QdrantClient, SemanticNeighborhoodSummary,
    SqliteStore, StorageExplorerSummary, StoreConfig, SymbolOutlineSummary, SymbolSearchRow,
    clamp_call_path_depth, freshness_for_hash, qdrant_collection_name,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryMode {
    Semantic,
    Symbol,
}

impl QueryMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Semantic => "semantic",
            Self::Symbol => "symbol",
        }
    }

    pub fn toggled(self) -> Self {
        match self {
            Self::Semantic => Self::Symbol,
            Self::Symbol => Self::Semantic,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallDirection {
    Callers,
    Callees,
}

impl CallDirection {
    pub fn label(self) -> &'static str {
        match self {
            Self::Callers => "callers",
            Self::Callees => "callees",
        }
    }

    pub fn toggled(self) -> Self {
        match self {
            Self::Callers => Self::Callees,
            Self::Callees => Self::Callers,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum QueryResult {
    Semantic(SemanticSearchSummary),
    Symbol(SymbolSearchSummary),
}

#[derive(Debug, Clone, PartialEq)]
pub struct SymbolSearchSummary {
    pub repository_id: String,
    pub query: String,
    pub symbols: Vec<SymbolSearchRow>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SemanticSearchSummary {
    pub repository_id: String,
    pub qdrant_collection: String,
    pub query: String,
    pub results: Vec<SemanticSearchResult>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SemanticSearchResult {
    pub score: f64,
    pub path: String,
    pub start_line: usize,
    pub end_line: usize,
    pub symbol_name: Option<String>,
    pub chunk_kind: String,
    pub provenance: EvidenceProvenance,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CallGraphSummary {
    pub repository_id: String,
    pub query: String,
    pub direction: CallDirection,
    pub rows: Vec<CallSearchRow>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CallPathSummary {
    pub repository_id: String,
    pub source_query: String,
    pub target_query: String,
    pub max_depth: usize,
    pub paths: Vec<CallPath>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ImpactSummary {
    pub repository_id: String,
    pub query: String,
    pub max_depth: usize,
    pub direct_callers: Vec<ImpactCallEvidence>,
    pub direct_callees: Vec<ImpactCallEvidence>,
    pub transitive_callers: Vec<ImpactPathEvidence>,
    pub transitive_callees: Vec<ImpactPathEvidence>,
    pub related_files: Vec<ImpactRelatedFile>,
    pub tests_likely: Vec<String>,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ImpactCallEvidence {
    pub row: CallSearchRow,
    pub freshness: EvidenceFreshness,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ImpactPathEvidence {
    pub path: CallPath,
    pub edge_freshness: Vec<EvidenceFreshness>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ImpactRelatedFile {
    pub path: String,
    pub relationship_count: usize,
    pub freshness: EvidenceFreshness,
    pub provenance: Option<EvidenceProvenance>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FreshnessSummary {
    pub repository_id: String,
    pub symbol_query: Option<String>,
    pub files: Vec<FileFreshnessRow>,
    pub focus_symbols: Vec<SymbolSearchRow>,
    pub context_pack: Option<ContextPack>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RuntimeFailureInput {
    pub frames: Vec<RuntimeFrame>,
    pub failing_tests: Vec<String>,
    pub malformed_lines: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RuntimeFrame {
    pub ordinal: usize,
    pub raw: String,
    pub symbol: Option<String>,
    pub path: Option<String>,
    pub line: Option<usize>,
    pub column: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DebugContextPack {
    pub format: String,
    pub repository_id: String,
    pub frames: Vec<DebugFrameMatch>,
    pub call_paths_between_frames: Vec<DebugFrameCallPath>,
    pub likely_tests: Vec<String>,
    pub limits: DebugContextLimits,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DebugFrameMatch {
    pub frame: RuntimeFrame,
    pub normalized_path: Option<String>,
    pub file_freshness: EvidenceFreshness,
    pub file_provenance: Option<EvidenceProvenance>,
    pub matched_symbols: Vec<SymbolSearchRow>,
    pub calls_at_line: Vec<CallPath>,
    pub matched: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DebugFrameCallPath {
    pub from_frame: usize,
    pub to_frame: usize,
    pub from_symbol: String,
    pub to_symbol: String,
    pub paths: Vec<CallPath>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DebugContextLimits {
    pub max_frames: usize,
    pub max_symbols_per_frame: usize,
    pub max_calls_per_frame: usize,
    pub max_call_paths_between_frames: usize,
}

impl FreshnessSummary {
    pub fn count(&self, freshness: EvidenceFreshness) -> usize {
        self.files
            .iter()
            .filter(|row| row.freshness == freshness)
            .count()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileFreshnessRow {
    pub path: String,
    pub freshness: EvidenceFreshness,
    pub indexed_content_hash: Option<String>,
    pub current_content_hash: Option<String>,
    pub indexed_at: Option<String>,
    pub index_run_id: Option<String>,
    pub parser_version: Option<String>,
}

pub fn run_symbol_search(repo: &str, query: &str) -> Result<SymbolSearchSummary, String> {
    let query = query.trim();
    if query.is_empty() {
        return Err("symbol search requires a query".to_owned());
    }
    let root = RepoRoot::open(repo).map_err(|error| error.to_string())?;
    let sqlite = sqlite_for_read()?;
    let symbols = sqlite
        .find_symbols(root.id(), query)
        .map_err(|error| error.to_string())?;
    Ok(SymbolSearchSummary {
        repository_id: root.id().to_owned(),
        query: query.to_owned(),
        symbols,
    })
}

pub fn run_call_graph(
    repo: &str,
    query: &str,
    direction: CallDirection,
) -> Result<CallGraphSummary, String> {
    let query = query.trim();
    if query.is_empty() {
        return Err("call graph requires a symbol query".to_owned());
    }
    let root = RepoRoot::open(repo).map_err(|error| error.to_string())?;
    let sqlite = sqlite_for_read()?;
    let rows = match direction {
        CallDirection::Callers => sqlite.callers(root.id(), query),
        CallDirection::Callees => sqlite.callees(root.id(), query),
    }
    .map_err(|error| error.to_string())?;

    Ok(CallGraphSummary {
        repository_id: root.id().to_owned(),
        query: query.to_owned(),
        direction,
        rows,
    })
}

pub fn run_call_path(
    repo: &str,
    source_query: &str,
    target_query: &str,
    max_depth: usize,
) -> Result<CallPathSummary, String> {
    let source_query = source_query.trim();
    let target_query = target_query.trim();
    if source_query.is_empty() {
        return Err("call path requires a source symbol query".to_owned());
    }
    if target_query.is_empty() {
        return Err("call path requires a target symbol query".to_owned());
    }
    let max_depth = clamp_call_path_depth(max_depth);
    let root = RepoRoot::open(repo).map_err(|error| error.to_string())?;
    let sqlite = sqlite_for_read()?;
    let paths = sqlite
        .call_paths(root.id(), source_query, target_query, max_depth)
        .map_err(|error| error.to_string())?;
    Ok(CallPathSummary {
        repository_id: root.id().to_owned(),
        source_query: source_query.to_owned(),
        target_query: target_query.to_owned(),
        max_depth,
        paths,
    })
}

pub fn run_impact(repo: &str, query: &str) -> Result<ImpactSummary, String> {
    let query = query.trim();
    if query.is_empty() {
        return Err("impact requires a symbol query".to_owned());
    }
    let root = RepoRoot::open(repo).map_err(|error| error.to_string())?;
    let sqlite = sqlite_for_read()?;
    let current_hashes = current_hashes(&root)?;
    let max_depth = 4;
    let direct_callers = sqlite
        .callers(root.id(), query)
        .map(|rows| impact_call_evidence(rows, &current_hashes))
        .map_err(|error| error.to_string())?;
    let direct_callees = sqlite
        .callees(root.id(), query)
        .map(|rows| impact_call_evidence(rows, &current_hashes))
        .map_err(|error| error.to_string())?;
    let transitive_callers = sqlite
        .transitive_call_paths_to(root.id(), query, max_depth)
        .map(|paths| impact_path_evidence(paths, &current_hashes))
        .map_err(|error| error.to_string())?;
    let transitive_callees = sqlite
        .transitive_call_paths_from(root.id(), query, max_depth)
        .map(|paths| impact_path_evidence(paths, &current_hashes))
        .map_err(|error| error.to_string())?;
    let related_files = impact_related_files(
        &direct_callers,
        &direct_callees,
        &transitive_callers,
        &transitive_callees,
        &current_hashes,
    );

    Ok(ImpactSummary {
        repository_id: root.id().to_owned(),
        query: query.to_owned(),
        max_depth,
        direct_callers,
        direct_callees,
        transitive_callers,
        transitive_callees,
        related_files,
        tests_likely: Vec::new(),
        notes: vec![
            "metadata_only_no_source_text".to_owned(),
            "likely_tests_unavailable_until_test_discovery_mapping_is_indexed".to_owned(),
        ],
    })
}

pub fn run_context_pack(repo: &str, query: &str, limit: usize) -> Result<ContextPack, String> {
    let query = query.trim();
    if query.is_empty() {
        return Err("context-pack requires a symbol query".to_owned());
    }
    let root = RepoRoot::open(repo).map_err(|error| error.to_string())?;
    let sqlite = sqlite_for_read()?;
    sqlite
        .context_pack(root.id(), query, limit)
        .map_err(|error| error.to_string())
}

pub fn parse_runtime_input(input: &str) -> RuntimeFailureInput {
    let mut frames = Vec::new();
    let mut failing_tests = BTreeSet::new();
    let mut malformed_lines = Vec::new();
    let mut pending_symbol: Option<(usize, String, String)> = None;
    let mut in_failures = false;

    for (index, line) in input.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed == "failures:" {
            in_failures = true;
            continue;
        }
        if let Some(test) = parse_failing_test(trimmed, in_failures) {
            failing_tests.insert(test);
        }
        if let Some((path, line_number, column)) = parse_file_location(trimmed) {
            let (raw, symbol) = pending_symbol
                .take()
                .map(|(_, raw, symbol)| (format!("{raw}\n{trimmed}"), Some(symbol)))
                .unwrap_or_else(|| (trimmed.to_owned(), None));
            frames.push(RuntimeFrame {
                ordinal: frames.len(),
                raw,
                symbol,
                path: Some(path),
                line: Some(line_number),
                column,
            });
            continue;
        }
        if let Some(symbol) = parse_stack_symbol(trimmed) {
            if let Some((_, raw, pending)) = pending_symbol.take() {
                frames.push(RuntimeFrame {
                    ordinal: frames.len(),
                    raw,
                    symbol: Some(pending),
                    path: None,
                    line: None,
                    column: None,
                });
            }
            pending_symbol = Some((index, trimmed.to_owned(), symbol));
        } else if looks_like_runtime_noise(trimmed) {
            malformed_lines.push(trimmed.to_owned());
        }
    }

    if let Some((_, raw, symbol)) = pending_symbol {
        frames.push(RuntimeFrame {
            ordinal: frames.len(),
            raw,
            symbol: Some(symbol),
            path: None,
            line: None,
            column: None,
        });
    }

    RuntimeFailureInput {
        frames,
        failing_tests: failing_tests.into_iter().collect(),
        malformed_lines,
    }
}

pub fn run_debug_context_pack(
    repo: &str,
    runtime_input: &str,
    limit: usize,
) -> Result<DebugContextPack, String> {
    if runtime_input.trim().is_empty() {
        return Err("debug-context requires runtime failure input".to_owned());
    }
    let root = RepoRoot::open(repo).map_err(|error| error.to_string())?;
    let sqlite = sqlite_for_read()?;
    let current_hashes = current_hashes(&root)?;
    build_debug_context_pack(&root, &sqlite, runtime_input, limit, &current_hashes)
}

pub fn run_freshness_report(
    repo: &str,
    symbol_query: Option<&str>,
) -> Result<FreshnessSummary, String> {
    let root = RepoRoot::open(repo).map_err(|error| error.to_string())?;
    let sqlite = sqlite_for_read()?;
    let current_hashes = discover_rust_files(&root, &DiscoveryOptions::default())
        .map_err(|error| error.to_string())?
        .into_iter()
        .map(|file| (file.facts.relative_path, file.facts.content_hash))
        .collect::<BTreeMap<_, _>>();
    let indexed = sqlite
        .indexed_file_freshness_snapshots(root.id())
        .map_err(|error| error.to_string())?;

    let mut focus_symbols = Vec::new();
    let mut context_pack = None;
    let mut scoped_paths = BTreeSet::new();
    let query = symbol_query
        .map(str::trim)
        .filter(|query| !query.is_empty())
        .map(str::to_owned);
    if let Some(query) = &query {
        focus_symbols = sqlite
            .find_symbols(root.id(), query)
            .map_err(|error| error.to_string())?;
        for symbol in &focus_symbols {
            scoped_paths.insert(symbol.path.clone());
        }
        let pack = sqlite
            .context_pack(root.id(), query, 8)
            .map_err(|error| error.to_string())?;
        for path in &pack.files {
            scoped_paths.insert(path.clone());
        }
        context_pack = Some(pack);
    }

    Ok(FreshnessSummary {
        repository_id: root.id().to_owned(),
        symbol_query: query,
        files: freshness_rows(&indexed, &current_hashes, &scoped_paths),
        focus_symbols,
        context_pack,
    })
}

pub fn run_storage_explorer(repo: &str) -> Result<StorageExplorerSummary, String> {
    let root = RepoRoot::open(repo).map_err(|error| error.to_string())?;
    let sqlite = sqlite_for_read()?;
    let embed_config = EmbedConfig::from_env();
    sqlite
        .storage_explorer_summary(root.id(), &embed_config.model)
        .map_err(|error| error.to_string())
}

pub fn run_index_coverage(repo: &str) -> Result<IndexCoverageSummary, String> {
    let root = RepoRoot::open(repo).map_err(|error| error.to_string())?;
    let sqlite = sqlite_for_read()?;
    sqlite
        .index_coverage_summary(root.id())
        .map_err(|error| error.to_string())
}

pub fn run_symbol_outline(repo: &str) -> Result<SymbolOutlineSummary, String> {
    let root = RepoRoot::open(repo).map_err(|error| error.to_string())?;
    let sqlite = sqlite_for_read()?;
    sqlite
        .symbol_outline_summary(root.id())
        .map_err(|error| error.to_string())
}

pub fn run_call_resolution(repo: &str) -> Result<CallResolutionSummary, String> {
    let root = RepoRoot::open(repo).map_err(|error| error.to_string())?;
    let sqlite = sqlite_for_read()?;
    sqlite
        .call_resolution_summary(root.id())
        .map_err(|error| error.to_string())
}

pub fn run_embedding_coverage(repo: &str) -> Result<EmbeddingCoverageSummary, String> {
    let root = RepoRoot::open(repo).map_err(|error| error.to_string())?;
    let sqlite = sqlite_for_read()?;
    let embed_config = EmbedConfig::from_env();
    sqlite
        .embedding_coverage_summary(root.id(), &embed_config.model)
        .map_err(|error| error.to_string())
}

pub fn run_index_runs_timeline(repo: &str) -> Result<IndexRunsTimelineSummary, String> {
    let root = RepoRoot::open(repo).map_err(|error| error.to_string())?;
    let sqlite = sqlite_for_read()?;
    sqlite
        .index_runs_timeline_summary(root.id())
        .map_err(|error| error.to_string())
}

pub fn run_semantic_neighborhood(repo: &str) -> Result<SemanticNeighborhoodSummary, String> {
    let root = RepoRoot::open(repo).map_err(|error| error.to_string())?;
    let sqlite = sqlite_for_read()?;
    let embed_config = EmbedConfig::from_env();
    sqlite
        .semantic_neighborhood_summary(root.id(), &embed_config.model)
        .map_err(|error| error.to_string())
}

pub fn run_cross_store_health(repo: &str) -> Result<CrossStoreHealthSummary, String> {
    let root = RepoRoot::open(repo).map_err(|error| error.to_string())?;
    let sqlite = sqlite_for_read()?;
    let embed_config = EmbedConfig::from_env();
    sqlite
        .cross_store_health_summary(root.id(), &embed_config.model)
        .map_err(|error| error.to_string())
}

pub fn run_semantic_search(
    repo: &str,
    query: &str,
    limit: usize,
) -> Result<SemanticSearchSummary, String> {
    let query = query.trim();
    if query.is_empty() {
        return Err("semantic search requires a query".to_owned());
    }

    let root = RepoRoot::open(repo).map_err(|error| error.to_string())?;
    let embed_config = EmbedConfig::from_env();
    let embed_client =
        OllamaClient::new(embed_config.clone()).map_err(|error| error.to_string())?;
    let query_embedding = embed_client
        .embed_batch(&[query.to_owned()])
        .map_err(|error| error.to_string())?;
    let Some(vector) = query_embedding.embeddings.into_iter().next() else {
        return Err("embedding query returned no vector".to_owned());
    };

    let store_config = StoreConfig::from_env();
    let qdrant = QdrantClient::new(&store_config).map_err(|error| error.to_string())?;
    let qdrant_collection = qdrant_collection_name(root.id(), &embed_config.model);
    let results = qdrant
        .query_points(&qdrant_collection, vector, limit)
        .map_err(|error| error.to_string())?
        .into_iter()
        .map(|point| SemanticSearchResult {
            score: point.score,
            path: point.payload.path,
            start_line: point.payload.start_line,
            end_line: point.payload.end_line,
            symbol_name: point.payload.symbol_name,
            chunk_kind: point.payload.chunk_kind,
            provenance: EvidenceProvenance {
                content_hash: point.payload.content_hash,
                index_run_id: point.payload.index_run_id,
                parser_version: point.payload.parser_version,
                indexed_at: point.payload.indexed_at,
                embedding_model: point.payload.embedding_model,
                embedding_dimension: point.payload.embedding_dimension,
                embedded_at: None,
            },
        })
        .collect();

    Ok(SemanticSearchSummary {
        repository_id: root.id().to_owned(),
        qdrant_collection,
        query: query.to_owned(),
        results,
    })
}

fn build_debug_context_pack(
    root: &RepoRoot,
    sqlite: &SqliteStore,
    runtime_input: &str,
    limit: usize,
    current_hashes: &BTreeMap<String, String>,
) -> Result<DebugContextPack, String> {
    let parsed = parse_runtime_input(runtime_input);
    let max_frames = limit.clamp(1, 25);
    let max_symbols = limit.clamp(1, 10);
    let max_calls = limit.clamp(1, 10);
    let mut frames = Vec::new();

    for frame in parsed.frames.iter().take(max_frames) {
        let normalized_path = frame
            .path
            .as_deref()
            .and_then(|path| normalize_runtime_path(root, path));
        let file_provenance = match normalized_path.as_deref() {
            Some(path) => sqlite
                .file_provenance(root.id(), path)
                .map_err(|error| error.to_string())?,
            None => None,
        };
        let mut matched_symbols = match (normalized_path.as_deref(), frame.line) {
            (Some(path), Some(line)) => sqlite
                .symbols_at_location(root.id(), path, line)
                .map_err(|error| error.to_string())?,
            _ => Vec::new(),
        };
        if matched_symbols.is_empty()
            && let Some(symbol) = frame.symbol.as_deref()
        {
            matched_symbols = sqlite
                .find_symbols(root.id(), symbol)
                .map_err(|error| error.to_string())?;
        }
        matched_symbols.truncate(max_symbols);

        let mut calls_at_line = match (normalized_path.as_deref(), frame.line) {
            (Some(path), Some(line)) => sqlite
                .calls_at_location(root.id(), path, line)
                .map(|edges| {
                    edges
                        .into_iter()
                        .map(|edge| call_path_from_edge(&edge))
                        .collect()
                })
                .map_err(|error| error.to_string())?,
            _ => Vec::new(),
        };
        calls_at_line.truncate(max_calls);

        let provenance = file_provenance.clone().or_else(|| {
            matched_symbols
                .first()
                .map(|symbol| symbol.provenance.clone())
        });
        let current_hash = normalized_path
            .as_deref()
            .and_then(|path| current_hashes.get(path).map(String::as_str));
        let file_freshness = freshness_for_hash(
            provenance
                .as_ref()
                .and_then(|provenance| provenance.content_hash.as_deref()),
            current_hash,
        );
        let matched =
            file_provenance.is_some() || !matched_symbols.is_empty() || !calls_at_line.is_empty();

        frames.push(DebugFrameMatch {
            frame: frame.clone(),
            normalized_path,
            file_freshness,
            file_provenance: provenance,
            matched_symbols,
            calls_at_line,
            matched,
        });
    }

    let mut call_paths_between_frames = Vec::new();
    for pair in frames.windows(2) {
        let Some(from) = pair[0].matched_symbols.first() else {
            continue;
        };
        let Some(to) = pair[1].matched_symbols.first() else {
            continue;
        };
        let paths = sqlite
            .call_paths(root.id(), &from.qualified_name, &to.qualified_name, 4)
            .map_err(|error| error.to_string())?;
        if paths.is_empty() {
            continue;
        }
        call_paths_between_frames.push(DebugFrameCallPath {
            from_frame: pair[0].frame.ordinal,
            to_frame: pair[1].frame.ordinal,
            from_symbol: from.qualified_name.clone(),
            to_symbol: to.qualified_name.clone(),
            paths,
        });
        if call_paths_between_frames.len() >= max_calls {
            break;
        }
    }

    let mut notes = vec![
        "metadata_only_no_source_text".to_owned(),
        "likely_tests_limited_to_runtime_failure_names_until_test_mapping_is_indexed".to_owned(),
    ];
    if parsed.frames.is_empty() {
        notes.push("no_runtime_frames_parsed".to_owned());
    }
    if !parsed.malformed_lines.is_empty() {
        notes.push("malformed_runtime_lines_ignored".to_owned());
    }

    Ok(DebugContextPack {
        format: "symdex.debug_context.v1".to_owned(),
        repository_id: root.id().to_owned(),
        frames,
        call_paths_between_frames,
        likely_tests: parsed.failing_tests,
        limits: DebugContextLimits {
            max_frames,
            max_symbols_per_frame: max_symbols,
            max_calls_per_frame: max_calls,
            max_call_paths_between_frames: max_calls,
        },
        notes,
    })
}

fn call_path_from_edge(edge: &symdex_store::CallPathEdge) -> CallPath {
    CallPath {
        hops: 1,
        min_confidence: edge.confidence,
        terminal_resolution_status: edge.resolution_status.clone(),
        edges: vec![edge.clone()],
    }
}

fn normalize_runtime_path(root: &RepoRoot, path: &str) -> Option<String> {
    let path = path
        .trim()
        .trim_start_matches("file://")
        .trim_start_matches("./")
        .replace('\\', "/");
    if path.is_empty() {
        return None;
    }
    let candidate = Path::new(&path);
    if candidate.is_absolute() {
        let root_path = root.path().to_string_lossy().replace('\\', "/");
        let root_with_sep = format!("{root_path}/");
        if path == root_path {
            return None;
        }
        if let Some(relative) = path.strip_prefix(&root_with_sep) {
            return symdex_core::NormalizedRepoPath::new(relative)
                .ok()
                .map(|path| path.as_str().to_owned());
        }
        return None;
    }
    symdex_core::NormalizedRepoPath::new(&path)
        .ok()
        .map(|path| path.as_str().to_owned())
}

fn parse_file_location(line: &str) -> Option<(String, usize, Option<usize>)> {
    let marker = ".rs:";
    let marker_start = line.find(marker)?;
    let path_end = marker_start + ".rs".len();
    let path_start = line[..marker_start]
        .rfind(|character: char| {
            character.is_whitespace() || matches!(character, '\'' | '"' | '(' | ')' | '[' | ']')
        })
        .map(|index| index + 1)
        .unwrap_or(0);
    let path = line[path_start..path_end]
        .trim_start_matches("at ")
        .trim()
        .to_owned();
    let after_path = &line[path_end + 1..];
    let (line_number, rest) = parse_usize_prefix(after_path)?;
    let column = rest
        .strip_prefix(':')
        .and_then(|rest| parse_usize_prefix(rest).map(|(column, _)| column));
    Some((path, line_number, column))
}

fn parse_usize_prefix(input: &str) -> Option<(usize, &str)> {
    let digits = input
        .char_indices()
        .take_while(|(_, character)| character.is_ascii_digit())
        .map(|(index, character)| (index, character.len_utf8()))
        .last()
        .map(|(index, width)| index + width)?;
    let value = input[..digits].parse::<usize>().ok()?;
    Some((value, &input[digits..]))
}

fn parse_stack_symbol(line: &str) -> Option<String> {
    let colon = line.find(':')?;
    if !line[..colon]
        .trim()
        .chars()
        .all(|character| character.is_ascii_digit())
    {
        return None;
    }
    let symbol = line[colon + 1..].trim();
    if symbol.is_empty() || symbol.starts_with("at ") {
        return None;
    }
    Some(symbol.to_owned())
}

fn parse_failing_test(line: &str, in_failures: bool) -> Option<String> {
    if let Some(rest) = line.strip_prefix("test ")
        && let Some(test) = rest.strip_suffix(" ... FAILED")
    {
        return Some(test.trim().to_owned());
    }
    if let Some(rest) = line.strip_prefix("---- ")
        && let Some(test) = rest.strip_suffix(" stdout ----")
    {
        return Some(test.trim().to_owned());
    }
    if in_failures && line.contains("::") && !line.contains(' ') {
        return Some(line.to_owned());
    }
    None
}

fn looks_like_runtime_noise(line: &str) -> bool {
    line.contains("panicked")
        || line.contains("stack backtrace")
        || line.contains("FAILED")
        || line.contains(".rs")
}

fn sqlite_for_read() -> Result<SqliteStore, String> {
    let store_config = StoreConfig::from_env();
    let sqlite = SqliteStore::open(&store_config).map_err(|error| error.to_string())?;
    sqlite.migrate().map_err(|error| error.to_string())?;
    Ok(sqlite)
}

fn current_hashes(root: &RepoRoot) -> Result<BTreeMap<String, String>, String> {
    discover_rust_files(root, &DiscoveryOptions::default())
        .map_err(|error| error.to_string())
        .map(|files| {
            files
                .into_iter()
                .map(|file| (file.facts.relative_path, file.facts.content_hash))
                .collect()
        })
}

fn freshness_for_provenance(
    path: Option<&str>,
    provenance: &EvidenceProvenance,
    current_hashes: &BTreeMap<String, String>,
) -> EvidenceFreshness {
    let current = path.and_then(|path| current_hashes.get(path).map(String::as_str));
    freshness_for_hash(provenance.content_hash.as_deref(), current)
}

fn impact_call_evidence(
    rows: Vec<CallSearchRow>,
    current_hashes: &BTreeMap<String, String>,
) -> Vec<ImpactCallEvidence> {
    rows.into_iter()
        .map(|row| {
            let freshness =
                freshness_for_provenance(row.path.as_deref(), &row.provenance, current_hashes);
            ImpactCallEvidence { row, freshness }
        })
        .collect()
}

fn impact_path_evidence(
    paths: Vec<CallPath>,
    current_hashes: &BTreeMap<String, String>,
) -> Vec<ImpactPathEvidence> {
    paths
        .into_iter()
        .map(|path| {
            let edge_freshness = path
                .edges
                .iter()
                .map(|edge| {
                    freshness_for_provenance(
                        Some(edge.caller_path.as_str()),
                        &edge.provenance,
                        current_hashes,
                    )
                })
                .collect();
            ImpactPathEvidence {
                path,
                edge_freshness,
            }
        })
        .collect()
}

fn impact_related_files(
    direct_callers: &[ImpactCallEvidence],
    direct_callees: &[ImpactCallEvidence],
    transitive_callers: &[ImpactPathEvidence],
    transitive_callees: &[ImpactPathEvidence],
    current_hashes: &BTreeMap<String, String>,
) -> Vec<ImpactRelatedFile> {
    let mut files = BTreeMap::<String, (usize, Option<EvidenceProvenance>)>::new();
    for evidence in direct_callers.iter().chain(direct_callees.iter()) {
        if let Some(path) = &evidence.row.path {
            let entry = files.entry(path.clone()).or_insert((0, None));
            entry.0 += 1;
            entry
                .1
                .get_or_insert_with(|| evidence.row.provenance.clone());
        }
    }
    for path_evidence in transitive_callers.iter().chain(transitive_callees.iter()) {
        for edge in &path_evidence.path.edges {
            let caller = files.entry(edge.caller_path.clone()).or_insert((0, None));
            caller.0 += 1;
            caller.1.get_or_insert_with(|| edge.provenance.clone());
            if let Some(callee_path) = &edge.callee_path {
                let callee = files.entry(callee_path.clone()).or_insert((0, None));
                callee.0 += 1;
                callee.1.get_or_insert_with(|| edge.provenance.clone());
            }
        }
    }
    files
        .into_iter()
        .map(|(path, (relationship_count, provenance))| {
            let freshness = match &provenance {
                Some(provenance) => {
                    freshness_for_provenance(Some(path.as_str()), provenance, current_hashes)
                }
                None => EvidenceFreshness::Unknown,
            };
            ImpactRelatedFile {
                path,
                relationship_count,
                freshness,
                provenance,
            }
        })
        .collect()
}

fn freshness_rows(
    indexed: &[FileFreshnessSnapshot],
    current_hashes: &BTreeMap<String, String>,
    scoped_paths: &BTreeSet<String>,
) -> Vec<FileFreshnessRow> {
    let mut rows = Vec::new();
    let mut seen = BTreeSet::new();
    for file in indexed {
        if !scoped_paths.is_empty() && !scoped_paths.contains(&file.path) {
            continue;
        }
        let current = current_hashes.get(&file.path);
        rows.push(FileFreshnessRow {
            path: file.path.clone(),
            freshness: freshness_for_hash(Some(&file.content_hash), current.map(String::as_str)),
            indexed_content_hash: Some(file.content_hash.clone()),
            current_content_hash: current.cloned(),
            indexed_at: Some(file.indexed_at.clone()),
            index_run_id: file.index_run_id.clone(),
            parser_version: file.parser_version.clone(),
        });
        seen.insert(file.path.clone());
    }
    for (path, hash) in current_hashes {
        if seen.contains(path) || (!scoped_paths.is_empty() && !scoped_paths.contains(path)) {
            continue;
        }
        rows.push(FileFreshnessRow {
            path: path.clone(),
            freshness: freshness_for_hash(None, Some(hash)),
            indexed_content_hash: None,
            current_content_hash: Some(hash.clone()),
            indexed_at: None,
            index_run_id: None,
            parser_version: None,
        });
    }
    rows.sort_by(|left, right| left.path.cmp(&right.path));
    rows
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    use symdex_core::RepoRoot;
    use symdex_store::{
        CallRecord, EvidenceFreshness, FileFreshnessSnapshot, FileRecord, RepositoryRecord,
        SqliteStore, StoreConfig, SymbolRecord,
    };

    use crate::{
        CallDirection, QueryMode, build_debug_context_pack, freshness_rows, parse_runtime_input,
        run_call_graph, run_call_path, run_context_pack, run_debug_context_pack, run_impact,
        run_semantic_search, run_symbol_search,
    };

    #[test]
    fn query_mode_toggles_between_workbench_modes() {
        assert_eq!(QueryMode::Semantic.toggled(), QueryMode::Symbol);
        assert_eq!(QueryMode::Symbol.toggled(), QueryMode::Semantic);
    }

    #[test]
    fn call_direction_toggles_between_graph_modes() {
        assert_eq!(CallDirection::Callers.toggled(), CallDirection::Callees);
        assert_eq!(CallDirection::Callees.toggled(), CallDirection::Callers);
    }

    #[test]
    fn symbol_search_rejects_empty_query() {
        let error = run_symbol_search(".", " ").expect_err("empty query should fail");
        assert!(error.contains("requires a query"));
    }

    #[test]
    fn semantic_search_rejects_empty_query_before_service_calls() {
        let error = run_semantic_search(".", " ", 10).expect_err("empty query should fail");
        assert!(error.contains("requires a query"));
    }

    #[test]
    fn call_graph_rejects_empty_query() {
        let error =
            run_call_graph(".", " ", CallDirection::Callers).expect_err("empty query should fail");
        assert!(error.contains("requires a symbol query"));
    }

    #[test]
    fn call_path_rejects_empty_source_or_target_query() {
        let source_error =
            run_call_path(".", " ", "target", 2).expect_err("empty source should fail");
        assert!(source_error.contains("source symbol query"));

        let target_error =
            run_call_path(".", "source", " ", 2).expect_err("empty target should fail");
        assert!(target_error.contains("target symbol query"));
    }

    #[test]
    fn impact_rejects_empty_query() {
        let error = run_impact(".", " ").expect_err("empty query should fail");
        assert!(error.contains("requires a symbol query"));
    }

    #[test]
    fn context_pack_rejects_empty_query() {
        let error = run_context_pack(".", " ", 8).expect_err("empty query should fail");
        assert!(error.contains("requires a symbol query"));
    }

    #[test]
    fn debug_context_rejects_empty_input() {
        let error = run_debug_context_pack(".", " ", 8).expect_err("empty input should fail");
        assert!(error.contains("requires runtime failure input"));
    }

    #[test]
    fn runtime_parser_extracts_frames_and_failing_tests() {
        let parsed = parse_runtime_input(
            "test tests::fails ... FAILED\n\
             thread 'main' panicked at src/lib.rs:12:5:\n\
             stack backtrace:\n\
             0: crate::module::run\n\
                at src/lib.rs:12:5\n\
             failures:\n\
                 tests::fails\n",
        );

        assert_eq!(parsed.failing_tests, vec!["tests::fails"]);
        assert_eq!(parsed.frames.len(), 2);
        assert_eq!(parsed.frames[0].path.as_deref(), Some("src/lib.rs"));
        assert_eq!(parsed.frames[0].line, Some(12));
        assert_eq!(
            parsed.frames[1].symbol.as_deref(),
            Some("crate::module::run")
        );
    }

    #[test]
    fn debug_context_maps_fresh_stale_deleted_and_unmapped_frames() {
        let fixture = DebugFixture::new();
        let current_hashes = BTreeMap::from([
            ("src/fresh.rs".to_owned(), "hash-fresh".to_owned()),
            ("src/stale.rs".to_owned(), "hash-new".to_owned()),
        ]);
        let input = format!(
            "0: crate::fresh\n   at {}/src/fresh.rs:2:1\n\
             at src/stale.rs:2:1\n\
             at src/deleted.rs:2:1\n\
             at src/unknown.rs:9:1\n\
             not a stack trace line with .rs text",
            fixture.root.path().display()
        );

        let pack =
            build_debug_context_pack(&fixture.root, &fixture.store, &input, 8, &current_hashes)
                .expect("debug context should build");

        assert_eq!(
            pack.frames
                .iter()
                .map(|frame| frame.file_freshness)
                .collect::<Vec<_>>(),
            vec![
                EvidenceFreshness::Fresh,
                EvidenceFreshness::Stale,
                EvidenceFreshness::Deleted,
                EvidenceFreshness::Unknown,
            ]
        );
        assert!(pack.frames[0].matched);
        assert_eq!(
            pack.frames[0].matched_symbols[0].qualified_name,
            "crate::fresh"
        );
        assert_eq!(pack.frames[0].calls_at_line.len(), 1);
        assert!(!pack.frames[3].matched);
        assert!(
            pack.notes
                .iter()
                .any(|note| note == "malformed_runtime_lines_ignored")
        );
    }

    #[test]
    fn freshness_rows_compare_indexed_and_current_hashes() {
        let indexed = vec![
            snapshot("src/deleted.rs", "old"),
            snapshot("src/fresh.rs", "same"),
            snapshot("src/stale.rs", "old"),
        ];
        let current = BTreeMap::from([
            ("src/fresh.rs".to_owned(), "same".to_owned()),
            ("src/missing.rs".to_owned(), "new".to_owned()),
            ("src/stale.rs".to_owned(), "new".to_owned()),
        ]);

        let rows = freshness_rows(&indexed, &current, &BTreeSet::new());

        assert_eq!(
            rows.iter()
                .map(|row| (row.path.as_str(), row.freshness))
                .collect::<Vec<_>>(),
            vec![
                ("src/deleted.rs", EvidenceFreshness::Deleted),
                ("src/fresh.rs", EvidenceFreshness::Fresh),
                ("src/missing.rs", EvidenceFreshness::Missing),
                ("src/stale.rs", EvidenceFreshness::Stale),
            ]
        );
    }

    fn snapshot(path: &str, content_hash: &str) -> FileFreshnessSnapshot {
        FileFreshnessSnapshot {
            path: path.to_owned(),
            content_hash: content_hash.to_owned(),
            indexed_at: "now".to_owned(),
            index_run_id: Some("run".to_owned()),
            parser_version: Some("parser".to_owned()),
        }
    }

    struct DebugFixture {
        root: RepoRoot,
        store: SqliteStore,
        _root_path: PathBuf,
        _db_path: PathBuf,
    }

    impl DebugFixture {
        fn new() -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system time should be valid")
                .as_nanos();
            let base = std::env::temp_dir().join(format!(
                "symdex-debug-query-test-{}-{nonce}",
                std::process::id()
            ));
            let root_path = base.join("repo");
            fs::create_dir_all(root_path.join("src")).expect("repo should be created");
            fs::write(root_path.join("src/fresh.rs"), "fn fresh() {}\n")
                .expect("fresh file should be written");
            fs::write(root_path.join("src/stale.rs"), "fn stale() {}\n")
                .expect("stale file should be written");
            let db_path = base.join("symdex.sqlite");
            let root = RepoRoot::open(&root_path).expect("repo root should open");
            let store = SqliteStore::open(&StoreConfig {
                sqlite_path: db_path.clone(),
                qdrant_url: "http://localhost:6333".to_owned(),
            })
            .expect("store should open");
            store.migrate().expect("store should migrate");
            store
                .upsert_repository(&RepositoryRecord {
                    id: root.id().to_owned(),
                    root_path: root.path().display().to_string(),
                })
                .expect("repo should persist");
            persist_file(
                &store,
                root.id(),
                "file-fresh",
                "src/fresh.rs",
                "hash-fresh",
                "sym-fresh",
                "fresh",
                "crate::fresh",
            );
            persist_file(
                &store,
                root.id(),
                "file-stale",
                "src/stale.rs",
                "hash-old",
                "sym-stale",
                "stale",
                "crate::stale",
            );
            persist_file(
                &store,
                root.id(),
                "file-deleted",
                "src/deleted.rs",
                "hash-deleted",
                "sym-deleted",
                "deleted",
                "crate::deleted",
            );
            store
                .replace_file_facts(
                    &FileRecord {
                        id: "file-callee".to_owned(),
                        repository_id: root.id().to_owned(),
                        path: "src/callee.rs".to_owned(),
                        language: "rust".to_owned(),
                        content_hash: "hash-callee".to_owned(),
                        index_run_id: "run".to_owned(),
                        parser_version: "parser".to_owned(),
                    },
                    &[],
                    &[sample_symbol(
                        "sym-callee",
                        "file-callee",
                        "callee",
                        "crate::callee",
                    )],
                    &[],
                )
                .expect("callee file should persist");
            store
                .replace_file_facts(
                    &FileRecord {
                        id: "file-fresh".to_owned(),
                        repository_id: root.id().to_owned(),
                        path: "src/fresh.rs".to_owned(),
                        language: "rust".to_owned(),
                        content_hash: "hash-fresh".to_owned(),
                        index_run_id: "run".to_owned(),
                        parser_version: "parser".to_owned(),
                    },
                    &[],
                    &[sample_symbol(
                        "sym-fresh",
                        "file-fresh",
                        "fresh",
                        "crate::fresh",
                    )],
                    &[CallRecord {
                        id: "call-fresh-callee".to_owned(),
                        caller_symbol_id: "sym-fresh".to_owned(),
                        callee_text: "callee".to_owned(),
                        callee_symbol_id: Some("sym-callee".to_owned()),
                        call_line: 2,
                        confidence: 1.0,
                        resolution_status: "resolved_exact".to_owned(),
                        index_run_id: "run".to_owned(),
                        parser_version: "parser".to_owned(),
                    }],
                )
                .expect("fresh call should persist");
            Self {
                root,
                store,
                _root_path: root_path,
                _db_path: db_path,
            }
        }
    }

    impl Drop for DebugFixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(
                self._root_path
                    .parent()
                    .expect("fixture root should have parent"),
            );
        }
    }

    fn persist_file(
        store: &SqliteStore,
        repository_id: &str,
        file_id: &str,
        path: &str,
        content_hash: &str,
        symbol_id: &str,
        symbol_name: &str,
        qualified_name: &str,
    ) {
        store
            .replace_file_facts(
                &FileRecord {
                    id: file_id.to_owned(),
                    repository_id: repository_id.to_owned(),
                    path: path.to_owned(),
                    language: "rust".to_owned(),
                    content_hash: content_hash.to_owned(),
                    index_run_id: "run".to_owned(),
                    parser_version: "parser".to_owned(),
                },
                &[],
                &[sample_symbol(
                    symbol_id,
                    file_id,
                    symbol_name,
                    qualified_name,
                )],
                &[],
            )
            .expect("file facts should persist");
    }

    fn sample_symbol(id: &str, file_id: &str, name: &str, qualified_name: &str) -> SymbolRecord {
        SymbolRecord {
            id: id.to_owned(),
            file_id: file_id.to_owned(),
            parent_symbol_id: None,
            name: name.to_owned(),
            qualified_name: qualified_name.to_owned(),
            kind: "function".to_owned(),
            signature: Some(format!("fn {name}()")),
            start_line: 1,
            end_line: 3,
            start_byte: 0,
            end_byte: 16,
            index_run_id: "run".to_owned(),
            parser_version: "parser".to_owned(),
        }
    }
}

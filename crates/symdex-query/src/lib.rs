//! Query orchestration shared by the CLI and TUI.

use std::collections::{BTreeMap, BTreeSet};

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

    use symdex_store::{EvidenceFreshness, FileFreshnessSnapshot};

    use crate::{
        CallDirection, QueryMode, freshness_rows, run_call_graph, run_call_path, run_context_pack,
        run_impact, run_semantic_search, run_symbol_search,
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
}

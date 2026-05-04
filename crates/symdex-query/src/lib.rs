//! Query orchestration shared by the CLI and TUI.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::Serialize;
use symdex_core::{
    DiscoveryOptions, NormalizedRepoPath, RepoRoot, RepositoryRefSnapshot, SemanticLayer,
    SemanticLayerMode, SemanticLayerStatus, discover_indexable_files,
};
use symdex_embed::{EmbedConfig, LayeredEmbedConfig, OllamaClient};
use symdex_store::{
    CallPath, CallResolutionSummary, CallSearchRow, ContextPack, CrossStoreHealthSummary,
    EmbeddingCoverageSummary, EvidenceFreshness, EvidenceProvenance, ExpectedVectorPoint,
    FileFreshnessSnapshot, IndexCoverageSummary, IndexRunsTimelineSummary,
    QualityGenerationProgress, RetrievedPoint, ScoredPoint, SemanticLayerManifestSummary,
    SemanticNeighborhoodSummary, SemanticRoutingSummary, SqliteStore, SqliteVectorStore,
    StorageExplorerSummary, StorageHealthRow, StorageHealthStatus, StoreConfig,
    SymbolOutlineSummary, SymbolSearchRow, TestSearchRow, clamp_call_path_depth,
    freshness_for_hash, vector_table_name,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextPackMode {
    Structural,
    Unified,
}

impl ContextPackMode {
    pub fn parse(value: &str) -> Result<Self, String> {
        match value.trim() {
            "" | "structural" => Ok(Self::Structural),
            "unified" => Ok(Self::Unified),
            other => Err(format!(
                "unsupported context-pack mode `{other}`; expected `structural` or `unified`"
            )),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Structural => "structural",
            Self::Unified => "unified",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextEvidenceSource {
    Structural,
    Semantic,
    Both,
}

impl ContextEvidenceSource {
    fn merge(self, other: Self) -> Self {
        match (self, other) {
            (Self::Both, _) | (_, Self::Both) => Self::Both,
            (Self::Structural, Self::Semantic) | (Self::Semantic, Self::Structural) => Self::Both,
            (same, _) => same,
        }
    }

    fn sort_rank(self) -> usize {
        match self {
            Self::Both => 0,
            Self::Structural => 1,
            Self::Semantic => 2,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextEvidenceItemKind {
    Symbol,
    Call,
    Chunk,
}

impl ContextEvidenceItemKind {
    fn sort_rank(self) -> usize {
        match self {
            Self::Symbol => 0,
            Self::Call => 1,
            Self::Chunk => 2,
        }
    }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SemanticSearchOptions {
    pub semantic_layer: SemanticLayerMode,
}

impl Default for SemanticSearchOptions {
    fn default() -> Self {
        Self {
            semantic_layer: SemanticLayerMode::Auto,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SemanticSearchSummary {
    pub repository_id: String,
    pub vector_table: String,
    pub requested_layer: SemanticLayerMode,
    pub semantic_layer: SemanticLayer,
    pub embedding_model: String,
    pub generation_id: Option<String>,
    pub quality_status: SemanticLayerStatus,
    pub fallback_reason: Option<String>,
    pub query: String,
    pub results: Vec<SemanticSearchResult>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SemanticSearchResult {
    pub point_id: String,
    pub chunk_id: String,
    pub symbol_id: Option<String>,
    pub score: f64,
    pub path: String,
    pub start_line: usize,
    pub end_line: usize,
    pub symbol_name: Option<String>,
    pub chunk_kind: String,
    pub language: String,
    pub text_hash: String,
    pub provenance: EvidenceProvenance,
    pub reasons: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticStatusSummary {
    pub repository_id: String,
    pub generation_id: Option<String>,
    pub active_layer: SemanticLayer,
    pub quality_status: SemanticLayerStatus,
    pub fallback_reason: Option<String>,
    pub fast: SemanticStatusLayerSummary,
    pub quality: SemanticStatusLayerSummary,
    pub quality_progress: Option<QualityGenerationProgress>,
    pub latest_quality_error: Option<String>,
    pub quality_enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticStatusLayerSummary {
    pub semantic_layer: SemanticLayer,
    pub embedding_model: String,
    pub embedding_dimension: Option<usize>,
    pub vector_table: String,
    pub current_chunks: usize,
    pub stale_chunks: usize,
    pub blocked_chunks: usize,
    pub failed_chunks: usize,
    pub other_chunks: usize,
    pub total_chunks: usize,
    pub expected_chunks: usize,
    pub is_complete: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct UnifiedContextPack {
    pub format: String,
    pub mode: String,
    pub repository_id: String,
    pub query: String,
    pub items: Vec<ContextEvidenceItem>,
    pub files: Vec<ContextEvidenceFile>,
    pub limits: UnifiedContextPackLimits,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UnifiedContextPackLimits {
    pub max_symbols: usize,
    pub max_callers: usize,
    pub max_callees: usize,
    pub max_semantic: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ContextEvidenceItem {
    pub id: String,
    pub item_kind: ContextEvidenceItemKind,
    pub evidence_source: ContextEvidenceSource,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub relationship: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub point_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chunk_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol_kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_line: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_line: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub call_line: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub callee_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolution_status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub score: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chunk_kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text_hash: Option<String>,
    pub freshness: String,
    pub trust: EvidenceTrust,
    pub reasons: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provenance: Option<EvidenceProvenance>,
}

impl ContextEvidenceItem {
    fn merge(&mut self, other: ContextEvidenceItem) {
        let other_source = other.evidence_source;
        let other_has_score = other.score.is_some();
        self.evidence_source = self.evidence_source.merge(other.evidence_source);
        merge_option(&mut self.relationship, other.relationship);
        merge_option(&mut self.point_id, other.point_id);
        merge_option(&mut self.chunk_id, other.chunk_id);
        merge_option(&mut self.symbol_id, other.symbol_id);
        merge_option(&mut self.symbol, other.symbol);
        merge_option(&mut self.symbol_name, other.symbol_name);
        merge_option(&mut self.symbol_kind, other.symbol_kind);
        merge_option(&mut self.path, other.path);
        merge_option(&mut self.start_line, other.start_line);
        merge_option(&mut self.end_line, other.end_line);
        merge_option(&mut self.call_line, other.call_line);
        merge_option(&mut self.callee_text, other.callee_text);
        merge_option(&mut self.confidence, other.confidence);
        merge_option(&mut self.resolution_status, other.resolution_status);
        merge_option(&mut self.chunk_kind, other.chunk_kind);
        merge_option(&mut self.language, other.language);
        merge_option(&mut self.text_hash, other.text_hash);
        merge_score(&mut self.score, other.score);

        if other_has_score
            || matches!(
                other_source,
                ContextEvidenceSource::Semantic | ContextEvidenceSource::Both
            )
            || self.provenance.is_none()
        {
            self.freshness = other.freshness;
            self.trust = other.trust;
            self.provenance = other.provenance;
        }
        extend_unique(&mut self.reasons, other.reasons);
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ContextEvidenceFile {
    pub path: String,
    pub evidence_source: ContextEvidenceSource,
    pub freshness: String,
    pub trust: EvidenceTrust,
    pub reasons: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provenance: Option<EvidenceProvenance>,
}

impl ContextEvidenceFile {
    fn merge(&mut self, other: ContextEvidenceFile) {
        let other_source = other.evidence_source;
        self.evidence_source = self.evidence_source.merge(other.evidence_source);
        if matches!(
            other_source,
            ContextEvidenceSource::Semantic | ContextEvidenceSource::Both
        ) || self.provenance.is_none()
        {
            self.freshness = other.freshness;
            self.trust = other.trust;
            self.provenance = other.provenance;
        }
        extend_unique(&mut self.reasons, other.reasons);
    }
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VectorVerifySummary {
    pub repository_id: String,
    pub semantic_layer: String,
    pub collection_name: String,
    pub embedding_model: String,
    pub table_exists: bool,
    pub expected_vector_points: usize,
    pub vector_payload_points: usize,
    pub missing_points: usize,
    pub stale_payload_points: usize,
    pub orphaned_points: usize,
    pub missing_point_ids: Vec<String>,
    pub stale_payload_point_ids: Vec<String>,
    pub orphaned_point_ids: Vec<String>,
    pub rows: Vec<StorageHealthRow>,
    pub layer_summaries: Vec<VectorVerifySummary>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VectorVerifySemanticLayer {
    #[default]
    Fast,
    Quality,
    All,
}

impl VectorVerifySemanticLayer {
    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "fast" => Ok(Self::Fast),
            "quality" => Ok(Self::Quality),
            "all" => Ok(Self::All),
            other => Err(format!(
                "unsupported semantic layer `{other}`; expected fast, quality, or all"
            )),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Fast => "fast",
            Self::Quality => "quality",
            Self::All => "all",
        }
    }

    fn layers(self) -> Vec<SemanticLayer> {
        match self {
            Self::Fast => vec![SemanticLayer::Fast],
            Self::Quality => vec![SemanticLayer::Quality],
            Self::All => vec![SemanticLayer::Fast, SemanticLayer::Quality],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct VectorVerifyOptions {
    pub semantic_layer: VectorVerifySemanticLayer,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ImpactCallEvidence {
    pub row: CallSearchRow,
    pub freshness: EvidenceFreshness,
    pub trust: EvidenceTrust,
    pub reasons: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ImpactPathEvidence {
    pub path: CallPath,
    pub edge_freshness: Vec<EvidenceFreshness>,
    pub edge_trust: Vec<EvidenceTrust>,
    pub edge_reasons: Vec<Vec<String>>,
    pub trust: EvidenceTrust,
    pub reasons: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ImpactRelatedFile {
    pub path: String,
    pub relationship_count: usize,
    pub freshness: EvidenceFreshness,
    pub provenance: Option<EvidenceProvenance>,
    pub trust: EvidenceTrust,
    pub reasons: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EvidenceTrust {
    pub score: f64,
    pub level: String,
    pub factors: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FreshnessSummary {
    pub repository_id: String,
    pub symbol_query: Option<String>,
    pub files: Vec<FileFreshnessRow>,
    pub focus_symbols: Vec<SymbolSearchRow>,
    pub context_pack: Option<ContextPack>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FreshnessScope {
    pub symbol_query: Option<String>,
    pub paths: Vec<String>,
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
    pub trust: EvidenceTrust,
    pub reasons: Vec<String>,
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
    let repository_ref_id = active_ref_scope(&root, &sqlite)?;
    let symbols = if let Some(repository_ref_id) = repository_ref_id.as_deref() {
        sqlite.find_symbols_for_ref(root.id(), repository_ref_id, query)
    } else {
        sqlite.find_symbols(root.id(), query)
    }
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
    let repository_ref_id = active_ref_scope(&root, &sqlite)?;
    let rows = match (direction, repository_ref_id.as_deref()) {
        (CallDirection::Callers, Some(repository_ref_id)) => {
            sqlite.callers_for_ref(root.id(), repository_ref_id, query)
        }
        (CallDirection::Callees, Some(repository_ref_id)) => {
            sqlite.callees_for_ref(root.id(), repository_ref_id, query)
        }
        (CallDirection::Callers, None) => sqlite.callers(root.id(), query),
        (CallDirection::Callees, None) => sqlite.callees(root.id(), query),
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
    let repository_ref_id = active_ref_scope(&root, &sqlite)?;
    let paths = if let Some(repository_ref_id) = repository_ref_id.as_deref() {
        sqlite.call_paths_for_ref(
            root.id(),
            repository_ref_id,
            source_query,
            target_query,
            max_depth,
        )
    } else {
        sqlite.call_paths(root.id(), source_query, target_query, max_depth)
    }
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
    let repository_ref_id = active_ref_scope(&root, &sqlite)?;
    build_impact_summary(&root, &sqlite, repository_ref_id.as_deref(), query)
}

fn build_impact_summary(
    root: &RepoRoot,
    sqlite: &SqliteStore,
    repository_ref_id: Option<&str>,
    query: &str,
) -> Result<ImpactSummary, String> {
    let current_hashes = current_hashes(root)?;
    let max_depth = 4;
    let direct_callers = if let Some(repository_ref_id) = repository_ref_id {
        sqlite.callers_for_ref(root.id(), repository_ref_id, query)
    } else {
        sqlite.callers(root.id(), query)
    }
    .map(|rows| impact_call_evidence(rows, &current_hashes, "direct_caller"))
    .map_err(|error| error.to_string())?;
    let direct_callees = if let Some(repository_ref_id) = repository_ref_id {
        sqlite.callees_for_ref(root.id(), repository_ref_id, query)
    } else {
        sqlite.callees(root.id(), query)
    }
    .map(|rows| impact_call_evidence(rows, &current_hashes, "direct_callee"))
    .map_err(|error| error.to_string())?;
    let transitive_callers = if let Some(repository_ref_id) = repository_ref_id {
        sqlite.transitive_call_paths_to_for_ref(root.id(), repository_ref_id, query, max_depth)
    } else {
        sqlite.transitive_call_paths_to(root.id(), query, max_depth)
    }
    .map(|paths| impact_path_evidence(paths, &current_hashes, "transitive_caller"))
    .map_err(|error| error.to_string())?;
    let transitive_callees = if let Some(repository_ref_id) = repository_ref_id {
        sqlite.transitive_call_paths_from_for_ref(root.id(), repository_ref_id, query, max_depth)
    } else {
        sqlite.transitive_call_paths_from(root.id(), query, max_depth)
    }
    .map(|paths| impact_path_evidence(paths, &current_hashes, "transitive_callee"))
    .map_err(|error| error.to_string())?;
    let related_files = impact_related_files(
        &direct_callers,
        &direct_callees,
        &transitive_callers,
        &transitive_callees,
        &current_hashes,
    );
    let tests_likely = sqlite
        .likely_tests_for_symbol(root.id(), query)
        .map(test_names)
        .map_err(|error| error.to_string())?;
    let mut notes = vec!["metadata_only_no_source_text".to_owned()];
    if tests_likely.is_empty() {
        notes.push("likely_tests_unavailable_without_indexed_direct_test_evidence".to_owned());
    } else {
        notes.push("likely_tests_from_indexed_direct_test_calls".to_owned());
    }

    Ok(ImpactSummary {
        repository_id: root.id().to_owned(),
        query: query.to_owned(),
        max_depth,
        direct_callers,
        direct_callees,
        transitive_callers,
        transitive_callees,
        related_files,
        tests_likely,
        notes,
    })
}

pub fn run_context_pack(repo: &str, query: &str, limit: usize) -> Result<ContextPack, String> {
    let query = query.trim();
    if query.is_empty() {
        return Err("context-pack requires a symbol query".to_owned());
    }
    let root = RepoRoot::open(repo).map_err(|error| error.to_string())?;
    let sqlite = sqlite_for_read()?;
    let repository_ref_id = active_ref_scope(&root, &sqlite)?;
    if let Some(repository_ref_id) = repository_ref_id.as_deref() {
        sqlite.context_pack_for_ref(root.id(), repository_ref_id, query, limit)
    } else {
        sqlite.context_pack(root.id(), query, limit)
    }
    .map_err(|error| error.to_string())
}

pub fn run_unified_context_pack(
    repo: &str,
    query: &str,
    limit: usize,
) -> Result<UnifiedContextPack, String> {
    let query = query.trim();
    if query.is_empty() {
        return Err("context-pack requires a symbol query".to_owned());
    }
    let root = RepoRoot::open(repo).map_err(|error| error.to_string())?;
    let sqlite = sqlite_for_read()?;
    let current_hashes = current_hashes(&root)?;
    let repository_ref_id = active_ref_scope(&root, &sqlite)?;
    let structural = if let Some(repository_ref_id) = repository_ref_id.as_deref() {
        sqlite.context_pack_for_ref(root.id(), repository_ref_id, query, limit)
    } else {
        sqlite.context_pack(root.id(), query, limit)
    }
    .map_err(|error| error.to_string())?;
    let semantic = semantic_search_for_root(&root, query, limit, SemanticSearchOptions::default());
    Ok(build_unified_context_pack(
        structural,
        semantic,
        &current_hashes,
        limit,
    ))
}

pub fn parse_runtime_input(input: &str) -> RuntimeFailureInput {
    let mut frames = Vec::new();
    let mut failing_tests = BTreeSet::new();
    let mut malformed_lines = Vec::new();
    let mut pending_symbol: Option<(usize, String, String)> = None;
    let mut in_failures = false;
    let mut in_backtrace = false;

    for (index, line) in input.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed == "failures:" {
            in_failures = true;
            in_backtrace = false;
            continue;
        }
        if is_backtrace_header(trimmed) {
            in_backtrace = true;
            continue;
        }
        if let Some(test) = parse_failing_test(trimmed, in_failures) {
            failing_tests.insert(test);
        }
        if let Some((symbol, path, line_number, column)) = parse_inline_symbol_location(trimmed) {
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
            frames.push(RuntimeFrame {
                ordinal: frames.len(),
                raw: trimmed.to_owned(),
                symbol: Some(symbol),
                path: Some(path),
                line: Some(line_number),
                column,
            });
            continue;
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
        if let Some(symbol) = parse_stack_symbol(trimmed, in_backtrace) {
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
    run_scoped_freshness_report(
        repo,
        FreshnessScope {
            symbol_query: symbol_query.map(str::to_owned),
            paths: Vec::new(),
        },
    )
}

pub fn run_scoped_freshness_report(
    repo: &str,
    scope: FreshnessScope,
) -> Result<FreshnessSummary, String> {
    run_scoped_freshness_report_with_store_config(repo, scope, &StoreConfig::from_env())
}

pub fn run_scoped_freshness_report_with_store_config(
    repo: &str,
    scope: FreshnessScope,
    store_config: &StoreConfig,
) -> Result<FreshnessSummary, String> {
    let root = RepoRoot::open(repo).map_err(|error| error.to_string())?;
    let sqlite = sqlite_for_read_with_config(store_config)?;
    build_freshness_report(&root, &sqlite, scope)
}

fn build_freshness_report(
    root: &RepoRoot,
    sqlite: &SqliteStore,
    scope: FreshnessScope,
) -> Result<FreshnessSummary, String> {
    let current_hashes = current_hashes(root)?;
    let repository_ref_id = active_ref_scope(root, sqlite)?;
    let indexed = if let Some(repository_ref_id) = repository_ref_id.as_deref() {
        sqlite.indexed_file_freshness_snapshots_for_ref(root.id(), repository_ref_id)
    } else {
        sqlite.indexed_file_freshness_snapshots(root.id())
    }
    .map_err(|error| error.to_string())?;

    let explicit_paths = normalize_freshness_paths(&scope.paths)?;

    let mut focus_symbols = Vec::new();
    let mut context_pack = None;
    let mut symbol_scoped_paths = BTreeSet::new();
    let query = scope
        .symbol_query
        .as_deref()
        .map(str::trim)
        .filter(|query| !query.is_empty())
        .map(str::to_owned);
    if let Some(query) = &query {
        focus_symbols = if let Some(repository_ref_id) = repository_ref_id.as_deref() {
            sqlite.find_symbols_for_ref(root.id(), repository_ref_id, query)
        } else {
            sqlite.find_symbols(root.id(), query)
        }
        .map_err(|error| error.to_string())?;
        for symbol in &focus_symbols {
            symbol_scoped_paths.insert(symbol.path.clone());
        }
        let pack = if let Some(repository_ref_id) = repository_ref_id.as_deref() {
            sqlite.context_pack_for_ref(root.id(), repository_ref_id, query, 8)
        } else {
            sqlite.context_pack(root.id(), query, 8)
        }
        .map_err(|error| error.to_string())?;
        for path in &pack.files {
            symbol_scoped_paths.insert(path.clone());
        }
        context_pack = Some(pack);
    }

    if query.is_some() && !explicit_paths.is_empty() {
        let incompatible = explicit_paths
            .iter()
            .filter(|path| !symbol_scoped_paths.contains(*path))
            .cloned()
            .collect::<Vec<_>>();
        if !incompatible.is_empty() {
            return Err(format!(
                "paths are outside the symbol freshness scope: {}",
                incompatible.join(", ")
            ));
        }
    }

    let scoped_paths = if !explicit_paths.is_empty() {
        explicit_paths
    } else {
        symbol_scoped_paths
    };

    Ok(FreshnessSummary {
        repository_id: root.id().to_owned(),
        symbol_query: query,
        files: freshness_rows(&indexed, &current_hashes, &scoped_paths),
        focus_symbols,
        context_pack,
    })
}

fn normalize_freshness_paths(paths: &[String]) -> Result<BTreeSet<String>, String> {
    paths
        .iter()
        .map(|path| {
            NormalizedRepoPath::new(path)
                .map(|path| path.as_str().to_owned())
                .map_err(|error| error.to_string())
        })
        .collect()
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

pub fn run_vector_verify(repo: &str) -> Result<VectorVerifySummary, String> {
    run_vector_verify_with_options(repo, VectorVerifyOptions::default())
}

pub fn run_vector_verify_with_options(
    repo: &str,
    options: VectorVerifyOptions,
) -> Result<VectorVerifySummary, String> {
    let root = RepoRoot::open(repo).map_err(|error| error.to_string())?;
    let sqlite = sqlite_for_read()?;
    let targets = vector_verify_targets(&root, &sqlite, options.semantic_layer)?;
    let store_config = StoreConfig::from_env();
    let vector = SqliteVectorStore::new(&store_config).map_err(|error| error.to_string())?;
    let mut summaries = Vec::new();
    for target in targets {
        let table_exists = vector
            .table_exists(&target.collection_name)
            .map_err(|error| error.to_string())?;
        let actual = if table_exists {
            vector
                .scroll_points_for_repository(&target.collection_name, root.id())
                .map_err(|error| error.to_string())?
        } else {
            Vec::new()
        };
        summaries.push(vector_verify_summary(
            root.id(),
            target.semantic_layer.as_str(),
            target.collection_name,
            target.embedding_model,
            table_exists,
            target.expected,
            actual,
        ));
    }

    if options.semantic_layer == VectorVerifySemanticLayer::All {
        Ok(vector_verify_all_summary(root.id(), summaries))
    } else {
        summaries
            .into_iter()
            .next()
            .ok_or_else(|| "no vector verification target selected".to_owned())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct VectorVerifyTarget {
    semantic_layer: SemanticLayer,
    collection_name: String,
    embedding_model: String,
    expected: Vec<ExpectedVectorPoint>,
}

fn vector_verify_targets(
    root: &RepoRoot,
    sqlite: &SqliteStore,
    selection: VectorVerifySemanticLayer,
) -> Result<Vec<VectorVerifyTarget>, String> {
    let routing = sqlite
        .semantic_routing_summary(root.id())
        .map_err(|error| error.to_string())?;
    selection
        .layers()
        .into_iter()
        .map(|layer| vector_verify_target(root, sqlite, routing.as_ref(), layer))
        .collect()
}

fn vector_verify_target(
    root: &RepoRoot,
    sqlite: &SqliteStore,
    routing: Option<&SemanticRoutingSummary>,
    semantic_layer: SemanticLayer,
) -> Result<VectorVerifyTarget, String> {
    match semantic_layer {
        SemanticLayer::Fast => vector_verify_fast_target(root, sqlite, routing),
        SemanticLayer::Quality => vector_verify_quality_target(root, sqlite, routing),
    }
}

fn vector_verify_fast_target(
    root: &RepoRoot,
    sqlite: &SqliteStore,
    routing: Option<&SemanticRoutingSummary>,
) -> Result<VectorVerifyTarget, String> {
    let Some(routing) = routing else {
        return Err(format!(
            "layered fast semantic metadata is missing for {}; run `symdex index <repo>` to create chunk_embeddings before vector-verify",
            root.id()
        ));
    };
    let expected = sqlite
        .expected_vector_points_for_generation_layer(
            root.id(),
            &routing.generation_id,
            SemanticLayer::Fast,
        )
        .map_err(|error| error.to_string())?;
    if !routing.fast.is_complete {
        return Err(format!(
            "layered fast semantic metadata is incomplete for {}; run `symdex index <repo>` to refresh chunk_embeddings before vector-verify",
            root.id()
        ));
    }
    Ok(vector_target_from_manifest(&routing.fast, expected))
}

fn vector_verify_quality_target(
    root: &RepoRoot,
    sqlite: &SqliteStore,
    routing: Option<&SemanticRoutingSummary>,
) -> Result<VectorVerifyTarget, String> {
    if let Some(routing) = routing {
        let expected = sqlite
            .expected_vector_points_for_generation_layer(
                root.id(),
                &routing.generation_id,
                SemanticLayer::Quality,
            )
            .map_err(|error| error.to_string())?;
        if let Some(manifest) = &routing.quality {
            return Ok(vector_target_from_manifest(manifest, expected));
        }
    }

    let quality_config = LayeredEmbedConfig::from_env().quality_embed_config();
    Ok(VectorVerifyTarget {
        semantic_layer: SemanticLayer::Quality,
        collection_name: vector_table_name(root.id(), &quality_config.model),
        embedding_model: quality_config.model,
        expected: Vec::new(),
    })
}

fn vector_target_from_manifest(
    manifest: &SemanticLayerManifestSummary,
    expected: Vec<ExpectedVectorPoint>,
) -> VectorVerifyTarget {
    VectorVerifyTarget {
        semantic_layer: manifest.semantic_layer,
        collection_name: manifest.vector_table.clone(),
        embedding_model: manifest.embedding_model.clone(),
        expected,
    }
}

fn vector_verify_all_summary(
    repository_id: &str,
    summaries: Vec<VectorVerifySummary>,
) -> VectorVerifySummary {
    let expected_vector_points = summaries
        .iter()
        .map(|summary| summary.expected_vector_points)
        .sum();
    let vector_payload_points = summaries
        .iter()
        .map(|summary| summary.vector_payload_points)
        .sum();
    let missing_points = summaries.iter().map(|summary| summary.missing_points).sum();
    let stale_payload_points = summaries
        .iter()
        .map(|summary| summary.stale_payload_points)
        .sum();
    let orphaned_points = summaries
        .iter()
        .map(|summary| summary.orphaned_points)
        .sum();
    let table_exists = summaries.iter().all(|summary| summary.table_exists);
    let missing_point_ids = summaries
        .iter()
        .flat_map(|summary| summary.missing_point_ids.clone())
        .collect();
    let stale_payload_point_ids = summaries
        .iter()
        .flat_map(|summary| summary.stale_payload_point_ids.clone())
        .collect();
    let orphaned_point_ids = summaries
        .iter()
        .flat_map(|summary| summary.orphaned_point_ids.clone())
        .collect();
    let rows = summaries
        .iter()
        .map(|summary| StorageHealthRow {
            status: if summary.missing_points > 0 {
                StorageHealthStatus::Error
            } else if summary.stale_payload_points > 0
                || summary.orphaned_points > 0
                || !summary.table_exists
            {
                StorageHealthStatus::Warning
            } else {
                StorageHealthStatus::Ok
            },
            label: format!("{}_layer_verify", summary.semantic_layer),
            detail: format!(
                "layer={} collection={} expected={} missing={} stale={} orphaned={}",
                summary.semantic_layer,
                summary.collection_name,
                summary.expected_vector_points,
                summary.missing_points,
                summary.stale_payload_points,
                summary.orphaned_points
            ),
        })
        .collect();

    VectorVerifySummary {
        repository_id: repository_id.to_owned(),
        semantic_layer: VectorVerifySemanticLayer::All.as_str().to_owned(),
        collection_name: "<multiple>".to_owned(),
        embedding_model: "<multiple>".to_owned(),
        table_exists,
        expected_vector_points,
        vector_payload_points,
        missing_points,
        stale_payload_points,
        orphaned_points,
        missing_point_ids,
        stale_payload_point_ids,
        orphaned_point_ids,
        rows,
        layer_summaries: summaries,
    }
}

fn vector_verify_summary(
    repository_id: &str,
    semantic_layer: &str,
    collection_name: String,
    embedding_model: String,
    table_exists: bool,
    expected: Vec<ExpectedVectorPoint>,
    actual: Vec<RetrievedPoint>,
) -> VectorVerifySummary {
    let expected_by_id = expected
        .iter()
        .map(|point| (point.vector_point_id.clone(), point))
        .collect::<BTreeMap<_, _>>();
    let actual_by_id = actual
        .iter()
        .map(|point| (point.id.clone(), point))
        .collect::<BTreeMap<_, _>>();
    let mut rows = Vec::new();
    let mut missing_points = 0usize;
    let mut stale_payload_points = 0usize;
    let mut orphaned_points = 0usize;
    let mut missing_point_ids = Vec::new();
    let mut stale_payload_point_ids = Vec::new();
    let mut orphaned_point_ids = Vec::new();

    if !table_exists {
        if expected.is_empty() {
            rows.push(StorageHealthRow {
                status: StorageHealthStatus::Warning,
                label: "collection_missing".to_owned(),
                detail: format!(
                    "vector table {collection_name} is missing, and SQLite has no vector-backed chunks for this model."
                ),
            });
        } else {
            missing_points = expected.len();
            missing_point_ids.extend(expected.iter().map(|point| point.vector_point_id.clone()));
            rows.push(StorageHealthRow {
                status: StorageHealthStatus::Error,
                label: "collection_missing".to_owned(),
                detail: format!(
                    "vector table {collection_name} is missing for {} SQLite vector-backed chunks.",
                    expected.len()
                ),
            });
        }
    } else {
        for expected_point in &expected {
            let Some(actual_point) = actual_by_id.get(&expected_point.vector_point_id) else {
                missing_points += 1;
                missing_point_ids.push(expected_point.vector_point_id.clone());
                rows.push(StorageHealthRow {
                    status: StorageHealthStatus::Error,
                    label: "missing_point".to_owned(),
                    detail: format!(
                        "SQLite chunk {} expects vector point {} at {}:{}-{}, but the point was not returned.",
                        expected_point.chunk_id,
                        expected_point.vector_point_id,
                        expected_point.path,
                        expected_point.start_line,
                        expected_point.end_line
                    ),
                });
                continue;
            };

            let mismatches = vector_payload_mismatches(expected_point, &actual_point.payload);
            if !mismatches.is_empty() {
                stale_payload_points += 1;
                stale_payload_point_ids.push(expected_point.vector_point_id.clone());
                rows.push(StorageHealthRow {
                    status: StorageHealthStatus::Warning,
                    label: "stale_payload".to_owned(),
                    detail: format!(
                        "vector point {} for chunk {} has stale payload fields: {}.",
                        expected_point.vector_point_id,
                        expected_point.chunk_id,
                        mismatches.join(", ")
                    ),
                });
            }
        }

        for point_id in actual_by_id.keys() {
            if !expected_by_id.contains_key(point_id) {
                orphaned_points += 1;
                orphaned_point_ids.push(point_id.clone());
                rows.push(StorageHealthRow {
                    status: StorageHealthStatus::Warning,
                    label: "orphaned_point".to_owned(),
                    detail: format!(
                        "vector point {point_id} has repository payload {repository_id} but no matching SQLite chunk."
                    ),
                });
            }
        }
    }

    if rows.is_empty() {
        rows.push(StorageHealthRow {
            status: StorageHealthStatus::Ok,
            label: "vector_verify_ok".to_owned(),
            detail: format!(
                "SQLite vector-backed chunks and vector metadata metadata are aligned for {collection_name}."
            ),
        });
    }

    VectorVerifySummary {
        repository_id: repository_id.to_owned(),
        semantic_layer: semantic_layer.to_owned(),
        collection_name,
        embedding_model,
        table_exists,
        expected_vector_points: expected.len(),
        vector_payload_points: actual.len(),
        missing_points,
        stale_payload_points,
        orphaned_points,
        missing_point_ids,
        stale_payload_point_ids,
        orphaned_point_ids,
        rows,
        layer_summaries: Vec::new(),
    }
}

fn vector_payload_mismatches(
    expected: &ExpectedVectorPoint,
    actual: &symdex_store::PointPayload,
) -> Vec<&'static str> {
    let mut mismatches = Vec::new();
    if actual.chunk_id != expected.chunk_id {
        mismatches.push("chunk_id");
    }
    if actual.path != expected.path {
        mismatches.push("path");
    }
    if actual.start_line != expected.start_line {
        mismatches.push("start_line");
    }
    if actual.end_line != expected.end_line {
        mismatches.push("end_line");
    }
    if actual.text_hash != expected.text_hash {
        mismatches.push("text_hash");
    }
    if let Some(expected_model) = &expected.embedding_model
        && actual.embedding_model.as_ref() != Some(expected_model)
    {
        mismatches.push("embedding_model");
    }
    if let Some(expected_dimension) = expected.embedding_dimension
        && actual.embedding_dimension != Some(expected_dimension)
    {
        mismatches.push("embedding_dimension");
    }
    mismatches
}

fn semantic_reasons(
    score: f64,
    path: &str,
    symbol_name: Option<&str>,
    chunk_kind: &str,
) -> Vec<String> {
    let mut reasons = vec![
        "semantic_vector_match".to_owned(),
        format!("semantic_score:{score:.4}"),
        format!("path:{path}"),
        format!("chunk_kind:{chunk_kind}"),
    ];
    if let Some(symbol_name) = symbol_name {
        reasons.push(format!("symbol_payload:{symbol_name}"));
    } else {
        reasons.push("symbol_payload:missing".to_owned());
    }
    reasons
}

pub fn run_semantic_search(
    repo: &str,
    query: &str,
    limit: usize,
) -> Result<SemanticSearchSummary, String> {
    run_semantic_search_with_options(repo, query, limit, SemanticSearchOptions::default())
}

pub fn run_semantic_status(repo: &str) -> Result<SemanticStatusSummary, String> {
    let root = RepoRoot::open(repo).map_err(|error| error.to_string())?;
    let layered_config = LayeredEmbedConfig::from_env();
    let store_config = StoreConfig::from_env();
    let sqlite = sqlite_for_read_with_config(&store_config)?;
    let repository_ref_id = active_ref_scope(&root, &sqlite)?;
    let routing =
        semantic_routing_summary_for_scope(&sqlite, root.id(), repository_ref_id.as_deref())?;
    let quality_progress = match routing.as_ref() {
        Some(summary) => Some(
            sqlite
                .quality_generation_progress(root.id(), &summary.generation_id)
                .map_err(|error| error.to_string())?,
        ),
        None => None,
    };
    let latest_quality_error = match routing.as_ref() {
        Some(summary) => sqlite
            .latest_quality_generation_error(root.id(), &summary.generation_id)
            .map_err(|error| error.to_string())?,
        None => None,
    };
    Ok(semantic_status_from_routing(
        root.id(),
        routing,
        quality_progress,
        latest_quality_error,
        &layered_config,
    ))
}

fn semantic_status_from_routing(
    repository_id: &str,
    routing: Option<SemanticRoutingSummary>,
    quality_progress: Option<QualityGenerationProgress>,
    latest_quality_error: Option<String>,
    layered_config: &LayeredEmbedConfig,
) -> SemanticStatusSummary {
    let target = resolve_semantic_search_target(
        repository_id,
        SemanticSearchOptions::default(),
        routing.as_ref(),
        layered_config,
    )
    .expect("auto semantic status target should always resolve");
    let fast_config = layered_config.fast_embed_config();
    let quality_config = layered_config.quality_embed_config();
    let quality_enabled = layered_config.quality_enabled;

    match routing {
        Some(summary) => {
            let expected_chunks = summary.embeddable_chunks;
            let fast = SemanticStatusLayerSummary::from_manifest(&summary.fast);
            let quality = summary
                .quality
                .as_ref()
                .map(SemanticStatusLayerSummary::from_manifest)
                .unwrap_or_else(|| {
                    SemanticStatusLayerSummary::configured(
                        repository_id,
                        SemanticLayer::Quality,
                        quality_config.model.clone(),
                        expected_chunks,
                    )
                });
            SemanticStatusSummary {
                repository_id: summary.repository_id,
                generation_id: Some(summary.generation_id),
                active_layer: target.semantic_layer,
                quality_status: target.quality_status,
                fallback_reason: target.fallback_reason,
                fast,
                quality,
                quality_progress,
                latest_quality_error,
                quality_enabled,
            }
        }
        None => SemanticStatusSummary {
            repository_id: repository_id.to_owned(),
            generation_id: None,
            active_layer: target.semantic_layer,
            quality_status: target.quality_status,
            fallback_reason: target.fallback_reason,
            fast: SemanticStatusLayerSummary::configured(
                repository_id,
                SemanticLayer::Fast,
                fast_config.model,
                0,
            ),
            quality: SemanticStatusLayerSummary::configured(
                repository_id,
                SemanticLayer::Quality,
                quality_config.model,
                0,
            ),
            quality_progress: None,
            latest_quality_error: None,
            quality_enabled,
        },
    }
}

impl SemanticStatusLayerSummary {
    fn from_manifest(manifest: &SemanticLayerManifestSummary) -> Self {
        Self {
            semantic_layer: manifest.semantic_layer,
            embedding_model: manifest.embedding_model.clone(),
            embedding_dimension: Some(manifest.embedding_dimension),
            vector_table: manifest.vector_table.clone(),
            current_chunks: manifest.current_chunks,
            stale_chunks: manifest.stale_chunks,
            blocked_chunks: manifest.blocked_chunks,
            failed_chunks: manifest.failed_chunks,
            other_chunks: manifest.other_chunks,
            total_chunks: manifest.total_chunks,
            expected_chunks: manifest.expected_chunks,
            is_complete: manifest.is_complete,
        }
    }

    fn configured(
        repository_id: &str,
        semantic_layer: SemanticLayer,
        embedding_model: String,
        expected_chunks: usize,
    ) -> Self {
        Self {
            semantic_layer,
            vector_table: vector_table_name(repository_id, &embedding_model),
            embedding_model,
            embedding_dimension: None,
            current_chunks: 0,
            stale_chunks: 0,
            blocked_chunks: 0,
            failed_chunks: 0,
            other_chunks: 0,
            total_chunks: 0,
            expected_chunks,
            is_complete: false,
        }
    }
}

pub fn run_semantic_search_with_options(
    repo: &str,
    query: &str,
    limit: usize,
    options: SemanticSearchOptions,
) -> Result<SemanticSearchSummary, String> {
    let query = query.trim();
    if query.is_empty() {
        return Err("semantic search requires a query".to_owned());
    }

    let root = RepoRoot::open(repo).map_err(|error| error.to_string())?;
    semantic_search_for_root(&root, query, limit, options)
}

fn semantic_search_for_root(
    root: &RepoRoot,
    query: &str,
    limit: usize,
    options: SemanticSearchOptions,
) -> Result<SemanticSearchSummary, String> {
    let layered_config = LayeredEmbedConfig::from_env();
    let store_config = StoreConfig::from_env();
    let sqlite = sqlite_for_read_with_config(&store_config)?;
    let repository_ref_id = active_ref_scope(root, &sqlite)?;
    let routing =
        semantic_routing_summary_for_scope(&sqlite, root.id(), repository_ref_id.as_deref())?;
    let target =
        resolve_semantic_search_target(root.id(), options, routing.as_ref(), &layered_config)?;
    let embed_config = embed_config_for_semantic_target(&layered_config, &target);
    let embed_client =
        OllamaClient::new(embed_config.clone()).map_err(|error| error.to_string())?;
    let query_embedding = embed_client
        .embed_batch(&[query.to_owned()])
        .map_err(|error| error.to_string())?;
    let Some(query_vector) = query_embedding.embeddings.into_iter().next() else {
        return Err("embedding query returned no vector".to_owned());
    };

    let vector_store = SqliteVectorStore::new(&store_config).map_err(|error| error.to_string())?;
    let points = if let Some(repository_ref_id) = repository_ref_id.as_deref() {
        vector_store.query_points_for_ref(
            &target.vector_table,
            root.id(),
            repository_ref_id,
            query_vector,
            limit,
        )
    } else {
        vector_store.query_points(&target.vector_table, query_vector, limit)
    }
    .map_err(|error| error.to_string())?;
    let results = points.into_iter().map(semantic_result_from_point).collect();

    Ok(SemanticSearchSummary {
        repository_id: root.id().to_owned(),
        vector_table: target.vector_table,
        requested_layer: target.requested_layer,
        semantic_layer: target.semantic_layer,
        embedding_model: target.embedding_model,
        generation_id: target.generation_id,
        quality_status: target.quality_status,
        fallback_reason: target.fallback_reason,
        query: query.to_owned(),
        results,
    })
}

fn semantic_routing_summary_for_scope(
    sqlite: &SqliteStore,
    repository_id: &str,
    repository_ref_id: Option<&str>,
) -> Result<Option<SemanticRoutingSummary>, String> {
    if let Some(repository_ref_id) = repository_ref_id {
        let routing = sqlite
            .semantic_routing_summary_for_ref(repository_id, repository_ref_id)
            .map_err(|error| error.to_string())?;
        if routing.is_some() {
            return Ok(routing);
        }
    }
    sqlite
        .semantic_routing_summary(repository_id)
        .map_err(|error| error.to_string())
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SemanticSearchTarget {
    requested_layer: SemanticLayerMode,
    semantic_layer: SemanticLayer,
    embedding_model: String,
    vector_table: String,
    generation_id: Option<String>,
    quality_status: SemanticLayerStatus,
    fallback_reason: Option<String>,
}

fn resolve_semantic_search_target(
    repository_id: &str,
    options: SemanticSearchOptions,
    routing: Option<&SemanticRoutingSummary>,
    layered_config: &LayeredEmbedConfig,
) -> Result<SemanticSearchTarget, String> {
    match options.semantic_layer {
        SemanticLayerMode::Auto => {
            resolve_auto_semantic_target(repository_id, routing, layered_config)
        }
        SemanticLayerMode::Fast => Ok(resolve_fast_semantic_target(
            repository_id,
            options.semantic_layer,
            routing,
            layered_config,
            None,
        )),
        SemanticLayerMode::Quality => {
            resolve_quality_semantic_target(repository_id, options.semantic_layer, routing)
        }
    }
}

fn resolve_auto_semantic_target(
    repository_id: &str,
    routing: Option<&SemanticRoutingSummary>,
    layered_config: &LayeredEmbedConfig,
) -> Result<SemanticSearchTarget, String> {
    let Some(summary) = routing else {
        return Ok(resolve_fast_semantic_target(
            repository_id,
            SemanticLayerMode::Auto,
            None,
            layered_config,
            Some("semantic_generation_missing_using_fast_layer".to_owned()),
        ));
    };

    if summary.active_layer == SemanticLayer::Quality {
        match summary.quality.as_ref() {
            Some(quality) if summary.quality_status.quality_is_current() && quality.is_complete => {
                return Ok(target_from_manifest(
                    SemanticLayerMode::Auto,
                    SemanticLayerStatus::QualityReady,
                    Some(summary.generation_id.clone()),
                    quality,
                    None,
                ));
            }
            Some(quality) if !quality.is_complete => {
                return Ok(resolve_fast_semantic_target(
                    repository_id,
                    SemanticLayerMode::Auto,
                    Some(summary),
                    layered_config,
                    Some("quality_manifest_incomplete_using_fast_layer".to_owned()),
                ));
            }
            Some(_) => {
                return Ok(resolve_fast_semantic_target(
                    repository_id,
                    SemanticLayerMode::Auto,
                    Some(summary),
                    layered_config,
                    Some(format!(
                        "quality_status_{}_using_fast_layer",
                        summary.quality_status.as_str()
                    )),
                ));
            }
            None => {
                return Ok(resolve_fast_semantic_target(
                    repository_id,
                    SemanticLayerMode::Auto,
                    Some(summary),
                    layered_config,
                    Some("quality_manifest_missing_using_fast_layer".to_owned()),
                ));
            }
        }
    }

    let fallback_reason = auto_fast_fallback_reason(summary);
    Ok(resolve_fast_semantic_target(
        repository_id,
        SemanticLayerMode::Auto,
        Some(summary),
        layered_config,
        fallback_reason,
    ))
}

fn auto_fast_fallback_reason(summary: &SemanticRoutingSummary) -> Option<String> {
    match summary.quality_status {
        SemanticLayerStatus::FastReady => None,
        SemanticLayerStatus::QualityReady if summary.active_layer == SemanticLayer::Quality => None,
        SemanticLayerStatus::QualityReady => {
            Some("quality_ready_not_active_using_fast_layer".to_owned())
        }
        _ => match summary.quality.as_ref() {
            Some(quality) if !quality.is_complete => {
                Some("quality_manifest_incomplete_using_fast_layer".to_owned())
            }
            None => Some("quality_manifest_missing_using_fast_layer".to_owned()),
            Some(_) => Some(format!(
                "quality_status_{}_using_fast_layer",
                summary.quality_status.as_str()
            )),
        },
    }
}

fn resolve_fast_semantic_target(
    repository_id: &str,
    requested_layer: SemanticLayerMode,
    routing: Option<&SemanticRoutingSummary>,
    layered_config: &LayeredEmbedConfig,
    fallback_reason: Option<String>,
) -> SemanticSearchTarget {
    if let Some(summary) = routing {
        return target_from_manifest(
            requested_layer,
            summary.quality_status,
            Some(summary.generation_id.clone()),
            &summary.fast,
            fallback_reason,
        );
    }

    let fast_config = layered_config.fast_embed_config();
    SemanticSearchTarget {
        requested_layer,
        semantic_layer: SemanticLayer::Fast,
        embedding_model: fast_config.model.clone(),
        vector_table: vector_table_name(repository_id, &fast_config.model),
        generation_id: None,
        quality_status: SemanticLayerStatus::Missing,
        fallback_reason,
    }
}

fn resolve_quality_semantic_target(
    repository_id: &str,
    requested_layer: SemanticLayerMode,
    routing: Option<&SemanticRoutingSummary>,
) -> Result<SemanticSearchTarget, String> {
    let Some(summary) = routing else {
        return Err(format!(
            "quality semantic layer is unavailable for repository `{repository_id}`: no semantic generation recorded"
        ));
    };
    if !summary.quality_status.quality_is_current() {
        return Err(format!(
            "quality semantic layer is unavailable for repository `{repository_id}`: status is `{}`",
            summary.quality_status.as_str()
        ));
    }
    let Some(quality) = summary.quality.as_ref() else {
        return Err(format!(
            "quality semantic layer is unavailable for repository `{repository_id}`: quality manifest is missing"
        ));
    };
    if !quality.is_complete {
        return Err(format!(
            "quality semantic layer is unavailable for repository `{repository_id}`: quality manifest is incomplete ({}/{})",
            quality.current_chunks, quality.expected_chunks
        ));
    }

    Ok(target_from_manifest(
        requested_layer,
        summary.quality_status,
        Some(summary.generation_id.clone()),
        quality,
        None,
    ))
}

fn target_from_manifest(
    requested_layer: SemanticLayerMode,
    quality_status: SemanticLayerStatus,
    generation_id: Option<String>,
    manifest: &SemanticLayerManifestSummary,
    fallback_reason: Option<String>,
) -> SemanticSearchTarget {
    SemanticSearchTarget {
        requested_layer,
        semantic_layer: manifest.semantic_layer,
        embedding_model: manifest.embedding_model.clone(),
        vector_table: manifest.vector_table.clone(),
        generation_id,
        quality_status,
        fallback_reason,
    }
}

fn embed_config_for_semantic_target(
    layered_config: &LayeredEmbedConfig,
    target: &SemanticSearchTarget,
) -> EmbedConfig {
    let mut embed_config = match target.semantic_layer {
        SemanticLayer::Fast => layered_config.fast_embed_config(),
        SemanticLayer::Quality => layered_config.quality_embed_config(),
    };
    embed_config.model = target.embedding_model.clone();
    embed_config
}

fn semantic_result_from_point(point: ScoredPoint) -> SemanticSearchResult {
    let point_id = point.id.clone();
    let path = point.payload.path;
    let symbol_name = point.payload.symbol_name;
    let chunk_kind = point.payload.chunk_kind;
    let reasons = semantic_reasons(point.score, &path, symbol_name.as_deref(), &chunk_kind);
    SemanticSearchResult {
        point_id,
        chunk_id: point.payload.chunk_id,
        symbol_id: point.payload.symbol_id,
        score: point.score,
        path,
        start_line: point.payload.start_line,
        end_line: point.payload.end_line,
        symbol_name,
        chunk_kind,
        language: point.payload.language,
        text_hash: point.payload.text_hash,
        provenance: EvidenceProvenance {
            content_hash: point.payload.content_hash,
            index_run_id: point.payload.index_run_id,
            parser_version: point.payload.parser_version,
            indexed_at: point.payload.indexed_at,
            embedding_model: point.payload.embedding_model,
            embedding_dimension: point.payload.embedding_dimension,
            embedded_at: None,
        },
        reasons,
    }
}

fn build_unified_context_pack(
    structural: ContextPack,
    semantic: Result<SemanticSearchSummary, String>,
    current_hashes: &BTreeMap<String, String>,
    limit: usize,
) -> UnifiedContextPack {
    let max_rows = limit.clamp(1, 25);
    let mut notes = vec![
        "metadata_only_no_source_text".to_owned(),
        "unified_structural_semantic_requested".to_owned(),
        "structural_direct_relationships_only".to_owned(),
    ];
    let mut items = BTreeMap::new();
    let mut files = BTreeMap::new();

    for symbol in &structural.focus_symbols {
        upsert_context_item(
            &mut items,
            structural_symbol_item(&structural.query, symbol, current_hashes),
        );
        upsert_context_file(
            &mut files,
            &symbol.path,
            ContextEvidenceSource::Structural,
            Some(symbol.provenance.clone()),
            current_hashes,
            vec![
                "context_pack_file_from_structural_evidence".to_owned(),
                "relationship:focus_symbol".to_owned(),
            ],
        );
    }

    for row in &structural.direct_callers {
        upsert_context_item(
            &mut items,
            structural_call_item("direct_caller", row, current_hashes),
        );
        if let Some(path) = &row.path {
            upsert_context_file(
                &mut files,
                path,
                ContextEvidenceSource::Structural,
                Some(row.provenance.clone()),
                current_hashes,
                vec![
                    "context_pack_file_from_structural_evidence".to_owned(),
                    "relationship:direct_caller".to_owned(),
                ],
            );
        }
    }

    for row in &structural.direct_callees {
        upsert_context_item(
            &mut items,
            structural_call_item("direct_callee", row, current_hashes),
        );
        if let Some(path) = &row.path {
            upsert_context_file(
                &mut files,
                path,
                ContextEvidenceSource::Structural,
                Some(row.provenance.clone()),
                current_hashes,
                vec![
                    "context_pack_file_from_structural_evidence".to_owned(),
                    "relationship:direct_callee".to_owned(),
                ],
            );
        }
    }

    for path in &structural.files {
        upsert_context_file(
            &mut files,
            path,
            ContextEvidenceSource::Structural,
            None,
            current_hashes,
            vec!["context_pack_file_from_structural_file_set".to_owned()],
        );
    }

    match semantic {
        Ok(summary) => {
            notes.push("semantic_evidence_included".to_owned());
            notes.push(format!("semantic_collection:{}", summary.vector_table));
            notes.push(format!(
                "semantic_requested_layer:{}",
                summary.requested_layer.as_str()
            ));
            notes.push(format!(
                "semantic_active_layer:{}",
                summary.semantic_layer.as_str()
            ));
            notes.push(format!(
                "semantic_embedding_model:{}",
                summary.embedding_model
            ));
            notes.push(format!(
                "semantic_quality_status:{}",
                summary.quality_status.as_str()
            ));
            if let Some(generation_id) = &summary.generation_id {
                notes.push(format!("semantic_generation_id:{generation_id}"));
            }
            if let Some(fallback_reason) = &summary.fallback_reason {
                notes.push(format!("semantic_fallback:{fallback_reason}"));
            }
            notes.push(format!("semantic_results:{}", summary.results.len()));
            for result in &summary.results {
                let source = semantic_context_source(result, &items, &files);
                let item_id = semantic_context_key(result, &items);
                upsert_context_item(
                    &mut items,
                    semantic_context_item(item_id, result, source, current_hashes),
                );
                upsert_context_file(
                    &mut files,
                    &result.path,
                    source,
                    Some(result.provenance.clone()),
                    current_hashes,
                    vec![
                        "context_pack_file_from_semantic_evidence".to_owned(),
                        format!("chunk_id:{}", result.chunk_id),
                    ],
                );
            }
        }
        Err(error) => notes.push(semantic_unavailable_note(&error)),
    }

    let mut items = items.into_values().collect::<Vec<_>>();
    items.sort_by(compare_context_items);
    let files = files.into_values().collect::<Vec<_>>();

    UnifiedContextPack {
        format: "symdex.context_pack.v2".to_owned(),
        mode: ContextPackMode::Unified.label().to_owned(),
        repository_id: structural.repository_id,
        query: structural.query,
        items,
        files,
        limits: UnifiedContextPackLimits {
            max_symbols: max_rows,
            max_callers: max_rows,
            max_callees: max_rows,
            max_semantic: max_rows,
        },
        notes,
    }
}

fn structural_symbol_item(
    query: &str,
    symbol: &SymbolSearchRow,
    current_hashes: &BTreeMap<String, String>,
) -> ContextEvidenceItem {
    let freshness =
        freshness_for_provenance(Some(&symbol.path), &symbol.provenance, current_hashes);
    let trust = evidence_trust(freshness, Some(&symbol.provenance), None);
    ContextEvidenceItem {
        id: format!("symbol:{}", symbol.id),
        item_kind: ContextEvidenceItemKind::Symbol,
        evidence_source: ContextEvidenceSource::Structural,
        relationship: Some("focus_symbol".to_owned()),
        point_id: None,
        chunk_id: None,
        symbol_id: Some(symbol.id.clone()),
        symbol: Some(symbol.qualified_name.clone()),
        symbol_name: Some(symbol.name.clone()),
        symbol_kind: Some(symbol.kind.clone()),
        path: Some(symbol.path.clone()),
        start_line: Some(symbol.start_line),
        end_line: Some(symbol.end_line),
        call_line: None,
        callee_text: None,
        confidence: None,
        resolution_status: None,
        score: None,
        chunk_kind: None,
        language: None,
        text_hash: None,
        freshness: freshness.label().to_owned(),
        trust,
        reasons: symbol_context_reasons(query, symbol),
        provenance: Some(symbol.provenance.clone()),
    }
}

fn structural_call_item(
    relationship: &str,
    row: &CallSearchRow,
    current_hashes: &BTreeMap<String, String>,
) -> ContextEvidenceItem {
    let freshness = freshness_for_provenance(row.path.as_deref(), &row.provenance, current_hashes);
    let trust = evidence_trust(freshness, Some(&row.provenance), Some(row.confidence));
    ContextEvidenceItem {
        id: structural_call_key(relationship, row),
        item_kind: ContextEvidenceItemKind::Call,
        evidence_source: ContextEvidenceSource::Structural,
        relationship: Some(relationship.to_owned()),
        point_id: None,
        chunk_id: None,
        symbol_id: row.symbol_id.clone(),
        symbol: row
            .symbol_qualified_name
            .clone()
            .or_else(|| row.symbol_name.clone()),
        symbol_name: row.symbol_name.clone(),
        symbol_kind: row.symbol_kind.clone(),
        path: row.path.clone(),
        start_line: row.start_line,
        end_line: row.end_line,
        call_line: Some(row.call_line),
        callee_text: Some(row.callee_text.clone()),
        confidence: Some(row.confidence),
        resolution_status: Some(row.resolution_status.clone()),
        score: None,
        chunk_kind: None,
        language: None,
        text_hash: None,
        freshness: freshness.label().to_owned(),
        trust,
        reasons: call_reasons(row, relationship),
        provenance: Some(row.provenance.clone()),
    }
}

fn semantic_context_item(
    id: String,
    result: &SemanticSearchResult,
    source: ContextEvidenceSource,
    current_hashes: &BTreeMap<String, String>,
) -> ContextEvidenceItem {
    let freshness =
        freshness_for_provenance(Some(&result.path), &result.provenance, current_hashes);
    let trust = evidence_trust(freshness, Some(&result.provenance), Some(result.score));
    let mut reasons = result.reasons.clone();
    reasons.push(format!("point_id:{}", result.point_id));
    reasons.push(format!("chunk_id:{}", result.chunk_id));
    if let Some(symbol_id) = &result.symbol_id {
        reasons.push(format!("symbol_id:{symbol_id}"));
    }
    ContextEvidenceItem {
        id,
        item_kind: ContextEvidenceItemKind::Chunk,
        evidence_source: source,
        relationship: Some("semantic_neighbor".to_owned()),
        point_id: Some(result.point_id.clone()),
        chunk_id: Some(result.chunk_id.clone()),
        symbol_id: result.symbol_id.clone(),
        symbol: result.symbol_name.clone(),
        symbol_name: result.symbol_name.clone(),
        symbol_kind: None,
        path: Some(result.path.clone()),
        start_line: Some(result.start_line),
        end_line: Some(result.end_line),
        call_line: None,
        callee_text: None,
        confidence: None,
        resolution_status: None,
        score: Some(result.score),
        chunk_kind: Some(result.chunk_kind.clone()),
        language: Some(result.language.clone()),
        text_hash: Some(result.text_hash.clone()),
        freshness: freshness.label().to_owned(),
        trust,
        reasons,
        provenance: Some(result.provenance.clone()),
    }
}

fn semantic_context_key(
    result: &SemanticSearchResult,
    items: &BTreeMap<String, ContextEvidenceItem>,
) -> String {
    if let Some(symbol_id) = &result.symbol_id {
        let key = format!("symbol:{symbol_id}");
        if items.contains_key(&key) {
            return key;
        }
    }
    format!("chunk:{}", result.chunk_id)
}

fn semantic_context_source(
    result: &SemanticSearchResult,
    items: &BTreeMap<String, ContextEvidenceItem>,
    files: &BTreeMap<String, ContextEvidenceFile>,
) -> ContextEvidenceSource {
    let symbol_overlap = result
        .symbol_id
        .as_ref()
        .map(|symbol_id| items.contains_key(&format!("symbol:{symbol_id}")))
        .unwrap_or(false);
    if symbol_overlap || files.contains_key(&result.path) {
        ContextEvidenceSource::Both
    } else {
        ContextEvidenceSource::Semantic
    }
}

fn upsert_context_item(
    items: &mut BTreeMap<String, ContextEvidenceItem>,
    item: ContextEvidenceItem,
) {
    if let Some(existing) = items.get_mut(&item.id) {
        existing.merge(item);
    } else {
        items.insert(item.id.clone(), item);
    }
}

fn upsert_context_file(
    files: &mut BTreeMap<String, ContextEvidenceFile>,
    path: &str,
    source: ContextEvidenceSource,
    provenance: Option<EvidenceProvenance>,
    current_hashes: &BTreeMap<String, String>,
    reasons: Vec<String>,
) {
    let freshness = provenance
        .as_ref()
        .map_or(EvidenceFreshness::Unknown, |provenance| {
            freshness_for_provenance(Some(path), provenance, current_hashes)
        });
    let trust = evidence_trust(freshness, provenance.as_ref(), None);
    let file = ContextEvidenceFile {
        path: path.to_owned(),
        evidence_source: source,
        freshness: freshness.label().to_owned(),
        trust,
        reasons,
        provenance,
    };
    if let Some(existing) = files.get_mut(path) {
        existing.merge(file);
    } else {
        files.insert(path.to_owned(), file);
    }
}

fn symbol_context_reasons(query: &str, symbol: &SymbolSearchRow) -> Vec<String> {
    let mut reasons = vec![
        "relationship:focus_symbol".to_owned(),
        "symbol_index_match".to_owned(),
        format!("kind:{}", symbol.kind),
        format!("path:{}", symbol.path),
    ];
    if symbol.qualified_name == query {
        reasons.push("query_match:qualified_name_exact".to_owned());
    } else if symbol.name == query {
        reasons.push("query_match:name_exact".to_owned());
    } else if symbol.qualified_name.ends_with(query) {
        reasons.push("query_match:qualified_name_suffix".to_owned());
    } else {
        reasons.push("query_match:sqlite_like".to_owned());
    }
    reasons
}

fn structural_call_key(relationship: &str, row: &CallSearchRow) -> String {
    format!(
        "call:{relationship}:{}:{}:{}:{}:{}",
        row.path.as_deref().unwrap_or(""),
        row.call_line,
        row.callee_text,
        row.symbol_id.as_deref().unwrap_or(""),
        row.start_line.unwrap_or(0),
    )
}

fn compare_context_items(left: &ContextEvidenceItem, right: &ContextEvidenceItem) -> Ordering {
    left.evidence_source
        .sort_rank()
        .cmp(&right.evidence_source.sort_rank())
        .then_with(|| left.item_kind.sort_rank().cmp(&right.item_kind.sort_rank()))
        .then_with(|| compare_optional_f64_desc(left.score, right.score))
        .then_with(|| compare_optional_f64_desc(left.confidence, right.confidence))
        .then_with(|| {
            left.relationship
                .as_deref()
                .unwrap_or("")
                .cmp(right.relationship.as_deref().unwrap_or(""))
        })
        .then_with(|| {
            left.path
                .as_deref()
                .unwrap_or("")
                .cmp(right.path.as_deref().unwrap_or(""))
        })
        .then_with(|| {
            left.start_line
                .unwrap_or(usize::MAX)
                .cmp(&right.start_line.unwrap_or(usize::MAX))
        })
        .then_with(|| {
            left.end_line
                .unwrap_or(usize::MAX)
                .cmp(&right.end_line.unwrap_or(usize::MAX))
        })
        .then_with(|| left.id.cmp(&right.id))
}

fn compare_optional_f64_desc(left: Option<f64>, right: Option<f64>) -> Ordering {
    match (left, right) {
        (Some(left), Some(right)) => right.partial_cmp(&left).unwrap_or(Ordering::Equal),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

fn merge_option<T>(target: &mut Option<T>, incoming: Option<T>) {
    if target.is_none() {
        *target = incoming;
    }
}

fn merge_score(target: &mut Option<f64>, incoming: Option<f64>) {
    if let Some(incoming) = incoming
        && target.map(|score| incoming > score).unwrap_or(true)
    {
        *target = Some(incoming);
    }
}

fn extend_unique(target: &mut Vec<String>, incoming: Vec<String>) {
    for value in incoming {
        if !target.iter().any(|existing| existing == &value) {
            target.push(value);
        }
    }
}

fn semantic_unavailable_note(error: &str) -> String {
    let lower = error.to_ascii_lowercase();
    let category =
        if lower.contains("404") || lower.contains("not found") || lower.contains("collection") {
            "missing_vector_collection"
        } else if lower.contains("connection")
            || lower.contains("refused")
            || lower.contains("timed out")
            || lower.contains("timeout")
            || lower.contains("error sending request")
        {
            "local_service_unavailable"
        } else if lower.contains("embedding") || lower.contains("ollama") {
            "embedding_unavailable"
        } else {
            "query_error"
        };
    format!("semantic_unavailable:{category}")
}

fn build_debug_context_pack(
    root: &RepoRoot,
    sqlite: &SqliteStore,
    runtime_input: &str,
    limit: usize,
    current_hashes: &BTreeMap<String, String>,
) -> Result<DebugContextPack, String> {
    let parsed = parse_runtime_input(runtime_input);
    let mapped_tests = map_failing_tests(sqlite, root.id(), &parsed.failing_tests)?;
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
        let mut symbol_match_reason = None;
        let mut matched_symbols = match (normalized_path.as_deref(), frame.line) {
            (Some(path), Some(line)) => sqlite
                .symbols_at_location(root.id(), path, line)
                .map_err(|error| error.to_string())?,
            _ => Vec::new(),
        };
        if !matched_symbols.is_empty() {
            symbol_match_reason = Some("symbols_at_runtime_location");
        }
        if matched_symbols.is_empty()
            && let Some(symbol) = frame.symbol.as_deref()
        {
            matched_symbols = sqlite
                .find_symbols(root.id(), symbol)
                .map_err(|error| error.to_string())?;
            if !matched_symbols.is_empty() {
                symbol_match_reason = Some("symbol_name_fallback_match");
            }
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
        let trust = evidence_trust(file_freshness, provenance.as_ref(), None);
        let matched =
            file_provenance.is_some() || !matched_symbols.is_empty() || !calls_at_line.is_empty();
        let reasons = debug_frame_reasons(
            frame,
            normalized_path.as_deref(),
            file_provenance.is_some(),
            symbol_match_reason,
            calls_at_line.len(),
            matched,
        );

        frames.push(DebugFrameMatch {
            frame: frame.clone(),
            normalized_path,
            file_freshness,
            file_provenance: provenance,
            trust,
            reasons,
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

    let mut notes = vec!["metadata_only_no_source_text".to_owned()];
    if mapped_tests.used_indexed_tests {
        notes.push("likely_tests_mapped_to_indexed_tests".to_owned());
    }
    if mapped_tests.used_runtime_fallbacks {
        notes.push("likely_tests_include_unmatched_runtime_failure_names".to_owned());
    }
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
        likely_tests: mapped_tests.tests,
        limits: DebugContextLimits {
            max_frames,
            max_symbols_per_frame: max_symbols,
            max_calls_per_frame: max_calls,
            max_call_paths_between_frames: max_calls,
        },
        notes,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MappedTests {
    tests: Vec<String>,
    used_indexed_tests: bool,
    used_runtime_fallbacks: bool,
}

fn map_failing_tests(
    sqlite: &SqliteStore,
    repository_id: &str,
    failing_tests: &[String],
) -> Result<MappedTests, String> {
    let mut tests = BTreeSet::new();
    let mut used_indexed_tests = false;
    let mut used_runtime_fallbacks = false;
    for failing_test in failing_tests {
        let mut matches = Vec::new();
        for candidate in runtime_test_name_candidates(failing_test) {
            matches.extend(
                sqlite
                    .tests_matching_name(repository_id, &candidate)
                    .map_err(|error| error.to_string())?,
            );
            if !matches.is_empty() {
                break;
            }
        }
        if matches.is_empty() {
            used_runtime_fallbacks = true;
            tests.insert(failing_test.clone());
        } else {
            used_indexed_tests = true;
            for test in matches {
                tests.insert(test.qualified_name);
            }
        }
    }
    Ok(MappedTests {
        tests: tests.into_iter().collect(),
        used_indexed_tests,
        used_runtime_fallbacks,
    })
}

fn runtime_test_name_candidates(name: &str) -> Vec<String> {
    let mut candidates = BTreeSet::new();
    let trimmed = name.trim().trim_matches(':');
    if !trimmed.is_empty() {
        candidates.insert(trimmed.to_owned());
        let parts = trimmed
            .split("::")
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>();
        for start in 1..parts.len() {
            candidates.insert(parts[start..].join("::"));
        }
        if let Some(last) = parts.last() {
            candidates.insert((*last).to_owned());
        }
    }
    candidates.into_iter().collect()
}

fn test_names(rows: Vec<TestSearchRow>) -> Vec<String> {
    rows.into_iter()
        .map(|test| test.qualified_name)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
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
    if let Some(location) = parse_tracing_field_location(line) {
        return Some(location);
    }
    let (marker_start, extension_len) = runtime_path_marker(line)?;
    let path_end = marker_start + extension_len;
    let path_start = runtime_path_start(line, marker_start);
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

fn runtime_path_start(line: &str, marker_start: usize) -> usize {
    line[..marker_start]
        .rfind(|character: char| {
            character.is_whitespace() || matches!(character, '\'' | '"' | '(' | ')' | '[' | ']')
        })
        .map(|index| index + 1)
        .unwrap_or(0)
}

fn parse_inline_symbol_location(line: &str) -> Option<(String, String, usize, Option<usize>)> {
    if let Some((path, line_number, column)) = parse_tracing_field_location(line)
        && let Some(symbol) = parse_tracing_symbol(line)
    {
        return Some((symbol, path, line_number, column));
    }

    let (marker_start, _) = runtime_path_marker(line)?;
    let path_start = runtime_path_start(line, marker_start);
    let before_path = line[..path_start].trim_end();
    let candidate = before_path
        .strip_suffix(" at")
        .or_else(|| before_path.strip_suffix("@"))?
        .trim()
        .rsplit_once(|character: char| character.is_whitespace())
        .map(|(_, symbol)| symbol)
        .unwrap_or_else(|| before_path.trim_end_matches(" at").trim());
    let symbol = normalize_stack_symbol(candidate)?;
    if !looks_like_rust_symbol(&symbol) {
        return None;
    }
    let (path, line_number, column) = parse_file_location(line)?;
    Some((symbol, path, line_number, column))
}

fn parse_tracing_field_location(line: &str) -> Option<(String, usize, Option<usize>)> {
    let path = parse_named_field(line, "file")?;
    if !has_supported_runtime_path_extension(&path) {
        return None;
    }
    let line_number = parse_named_usize(line, "line")?;
    let column = parse_named_usize(line, "column");
    Some((path, line_number, column))
}

fn has_supported_runtime_path_extension(path: &str) -> bool {
    [
        ".tsx", ".mts", ".cts", ".jsx", ".mjs", ".cjs", ".rs", ".cs", ".ts", ".js",
    ]
    .iter()
    .any(|extension| path.ends_with(extension))
}

fn parse_tracing_symbol(line: &str) -> Option<String> {
    for key in ["target", "span", "module_path"] {
        if let Some(symbol) =
            parse_named_field(line, key).and_then(|value| normalize_stack_symbol(&value))
            && looks_like_rust_symbol(&symbol)
        {
            return Some(symbol);
        }
    }
    None
}

fn parse_named_usize(line: &str, key: &str) -> Option<usize> {
    parse_named_field(line, key)?.parse::<usize>().ok()
}

fn parse_named_field(line: &str, key: &str) -> Option<String> {
    let assignment = format!("{key}=");
    let start = line.find(&assignment)? + assignment.len();
    let rest = line[start..].trim_start();
    if let Some(rest) = rest.strip_prefix('"') {
        let end = rest.find('"')?;
        return Some(rest[..end].to_owned());
    }
    let end = rest
        .char_indices()
        .find(|(_, character)| character.is_whitespace() || matches!(character, ',' | ';'))
        .map(|(index, _)| index)
        .unwrap_or(rest.len());
    let value = rest[..end].trim_matches('"');
    if value.is_empty() {
        None
    } else {
        Some(value.to_owned())
    }
}

fn parse_usize_prefix(input: &str) -> Option<(usize, &str)> {
    let digits = input
        .char_indices()
        .find(|(_, character)| !character.is_ascii_digit())
        .map(|(index, _)| index)
        .unwrap_or(input.len());
    if digits == 0 {
        return None;
    }
    let value = input[..digits].parse::<usize>().ok()?;
    Some((value, &input[digits..]))
}

fn runtime_path_marker(line: &str) -> Option<(usize, usize)> {
    [
        ".tsx:", ".mts:", ".cts:", ".jsx:", ".mjs:", ".cjs:", ".rs:", ".cs:", ".ts:", ".js:",
    ]
    .iter()
    .filter_map(|marker| line.find(marker).map(|start| (start, marker.len() - 1)))
    .min_by_key(|(start, _)| *start)
}

fn parse_stack_symbol(line: &str, in_backtrace: bool) -> Option<String> {
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
    let symbol = normalize_stack_symbol(symbol)?;
    if !in_backtrace && !looks_like_rust_symbol(&symbol) {
        return None;
    }
    Some(symbol)
}

fn normalize_stack_symbol(symbol: &str) -> Option<String> {
    let symbol = symbol.trim();
    let symbol = if let Some((address, symbol)) = symbol.split_once(" - ") {
        if address.trim_start().starts_with("0x") {
            symbol.trim()
        } else {
            symbol
        }
    } else {
        symbol
    };
    let symbol = symbol.trim();
    if symbol.is_empty() || symbol.starts_with("at ") {
        None
    } else {
        Some(symbol.to_owned())
    }
}

fn looks_like_rust_symbol(symbol: &str) -> bool {
    symbol.contains("::") || symbol.starts_with('<')
}

fn is_backtrace_header(line: &str) -> bool {
    line == "stack backtrace:"
        || line == "backtrace:"
        || line.contains("RUST_BACKTRACE=full")
        || line.contains("RUST_BACKTRACE=1")
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
    if in_failures && !line.contains(' ') && is_probable_test_name(line) {
        return Some(line.to_owned());
    }
    None
}

fn is_probable_test_name(line: &str) -> bool {
    let trimmed = line.trim_matches(':');
    !trimmed.is_empty()
        && trimmed
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | ':'))
        && (trimmed.starts_with("tests::")
            || trimmed.contains("::tests::")
            || trimmed.contains("::"))
}

fn looks_like_runtime_noise(line: &str) -> bool {
    line.contains("panicked")
        || line.contains("stack backtrace")
        || line.contains("FAILED")
        || line.contains("RUST_BACKTRACE")
        || runtime_path_marker(line).is_some()
}

fn sqlite_for_read() -> Result<SqliteStore, String> {
    sqlite_for_read_with_config(&StoreConfig::from_env())
}

fn sqlite_for_read_with_config(store_config: &StoreConfig) -> Result<SqliteStore, String> {
    SqliteStore::open_read_only(store_config).map_err(|error| error.to_string())
}

fn active_ref_scope(root: &RepoRoot, sqlite: &SqliteStore) -> Result<Option<String>, String> {
    if !sqlite
        .repository_has_ref_file_manifests(root.id())
        .map_err(|error| error.to_string())?
    {
        return Ok(None);
    }
    let snapshot = RepositoryRefSnapshot::detect(root).map_err(|error| error.to_string())?;
    Ok(Some(snapshot.id))
}

fn current_hashes(root: &RepoRoot) -> Result<BTreeMap<String, String>, String> {
    discover_indexable_files(root, &DiscoveryOptions::from_env())
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

pub fn evidence_trust(
    freshness: EvidenceFreshness,
    provenance: Option<&EvidenceProvenance>,
    confidence: Option<f64>,
) -> EvidenceTrust {
    let freshness_score = match freshness {
        EvidenceFreshness::Fresh => 1.0,
        EvidenceFreshness::Stale => 0.55,
        EvidenceFreshness::Deleted => 0.25,
        EvidenceFreshness::Missing => 0.2,
        EvidenceFreshness::Unknown => 0.45,
    };
    let provenance_score = provenance.map_or(0.0, provenance_completeness);
    let index_score = provenance.map_or(0.0, index_metadata_completeness);
    let confidence_score = confidence.unwrap_or(1.0).clamp(0.0, 1.0);
    let score = round_trust_score(
        freshness_score * 0.35
            + provenance_score * 0.25
            + confidence_score * 0.25
            + index_score * 0.15,
    );
    let level = if score >= 0.85 {
        "high"
    } else if score >= 0.65 {
        "medium"
    } else if score >= 0.35 {
        "low"
    } else {
        "minimal"
    };
    let mut factors = vec![format!("freshness:{}", freshness.label())];
    if let Some(confidence) = confidence {
        factors.push(format!("confidence:{:.2}", confidence.clamp(0.0, 1.0)));
    } else {
        factors.push("confidence:not_applicable".to_owned());
    }
    match provenance {
        Some(provenance) => {
            factors.push(format!(
                "provenance:{}",
                completeness_label(provenance_completeness(provenance))
            ));
            factors.push(format!(
                "index_metadata:{}",
                completeness_label(index_metadata_completeness(provenance))
            ));
        }
        None => {
            factors.push("provenance:missing".to_owned());
            factors.push("index_metadata:missing".to_owned());
        }
    }
    EvidenceTrust {
        score,
        level: level.to_owned(),
        factors,
    }
}

fn provenance_completeness(provenance: &EvidenceProvenance) -> f64 {
    let present = [
        provenance.content_hash.is_some(),
        provenance.index_run_id.is_some(),
        provenance.parser_version.is_some(),
        provenance.indexed_at.is_some(),
    ]
    .into_iter()
    .filter(|present| *present)
    .count();
    present as f64 / 4.0
}

fn index_metadata_completeness(provenance: &EvidenceProvenance) -> f64 {
    match (
        provenance.index_run_id.is_some(),
        provenance.parser_version.is_some(),
    ) {
        (true, true) => 1.0,
        (true, false) | (false, true) => 0.6,
        (false, false) => 0.0,
    }
}

fn completeness_label(score: f64) -> &'static str {
    if score >= 1.0 {
        "complete"
    } else if score >= 0.5 {
        "partial"
    } else {
        "missing"
    }
}

fn round_trust_score(score: f64) -> f64 {
    (score * 100.0).round() / 100.0
}

fn call_reasons(row: &CallSearchRow, relationship: &str) -> Vec<String> {
    let mut reasons = vec![
        format!("relationship:{relationship}"),
        "persisted_call_edge".to_owned(),
        format!("callee_text:{}", row.callee_text),
        format!("resolution_status:{}", row.resolution_status),
        format!("confidence:{:.2}", row.confidence),
    ];
    if let Some(symbol) = &row.symbol_qualified_name {
        reasons.push(format!("symbol_match:{symbol}"));
    } else if let Some(symbol) = &row.symbol_name {
        reasons.push(format!("symbol_match:{symbol}"));
    } else {
        reasons.push("symbol_match:unresolved".to_owned());
    }
    if let Some(path) = &row.path {
        reasons.push(format!("path:{path}"));
    }
    reasons
}

fn path_reasons(path: &CallPath, relationship: &str) -> Vec<String> {
    vec![
        format!("relationship:{relationship}"),
        "bounded_transitive_call_path".to_owned(),
        format!("hops:{}", path.hops),
        format!("min_confidence:{:.2}", path.min_confidence),
        format!(
            "terminal_resolution_status:{}",
            path.terminal_resolution_status
        ),
    ]
}

fn edge_reasons(edge: &symdex_store::CallPathEdge, relationship: &str) -> Vec<String> {
    vec![
        format!("relationship:{relationship}"),
        "persisted_path_edge".to_owned(),
        format!("caller:{}", edge.caller_symbol_qualified_name),
        format!(
            "callee:{}",
            edge.callee_symbol_qualified_name
                .as_deref()
                .unwrap_or(&edge.callee_text)
        ),
        format!("resolution_status:{}", edge.resolution_status),
        format!("confidence:{:.2}", edge.confidence),
    ]
}

fn related_file_reasons(
    relationship_count: usize,
    provenance: Option<&EvidenceProvenance>,
) -> Vec<String> {
    let mut reasons = vec![
        "related_file_from_call_evidence".to_owned(),
        format!("relationship_count:{relationship_count}"),
    ];
    if provenance.is_some() {
        reasons.push("provenance:first_related_edge".to_owned());
    } else {
        reasons.push("provenance:missing".to_owned());
    }
    reasons
}

fn debug_frame_reasons(
    frame: &RuntimeFrame,
    normalized_path: Option<&str>,
    has_file_provenance: bool,
    symbol_match_reason: Option<&'static str>,
    calls_at_line: usize,
    matched: bool,
) -> Vec<String> {
    let mut reasons = vec![format!("runtime_frame:{}", frame.ordinal)];
    if let Some(path) = normalized_path {
        reasons.push(format!("runtime_path_normalized:{path}"));
    } else if frame.path.is_some() {
        reasons.push("runtime_path_outside_or_unindexed".to_owned());
    } else {
        reasons.push("runtime_path:missing".to_owned());
    }
    if has_file_provenance {
        reasons.push("file_provenance_match".to_owned());
    }
    if let Some(reason) = symbol_match_reason {
        reasons.push(reason.to_owned());
    }
    if calls_at_line > 0 {
        reasons.push(format!("calls_at_runtime_line:{calls_at_line}"));
    }
    if !matched {
        reasons.push("unmatched_runtime_frame".to_owned());
    }
    reasons
}

fn impact_call_evidence(
    rows: Vec<CallSearchRow>,
    current_hashes: &BTreeMap<String, String>,
    relationship: &str,
) -> Vec<ImpactCallEvidence> {
    rows.into_iter()
        .map(|row| {
            let freshness =
                freshness_for_provenance(row.path.as_deref(), &row.provenance, current_hashes);
            let trust = evidence_trust(freshness, Some(&row.provenance), Some(row.confidence));
            let reasons = call_reasons(&row, relationship);
            ImpactCallEvidence {
                row,
                freshness,
                trust,
                reasons,
            }
        })
        .collect()
}

fn impact_path_evidence(
    paths: Vec<CallPath>,
    current_hashes: &BTreeMap<String, String>,
    relationship: &str,
) -> Vec<ImpactPathEvidence> {
    paths
        .into_iter()
        .map(|path| {
            let edge_pairs = path
                .edges
                .iter()
                .map(|edge| {
                    let freshness = freshness_for_provenance(
                        Some(edge.caller_path.as_str()),
                        &edge.provenance,
                        current_hashes,
                    );
                    let trust =
                        evidence_trust(freshness, Some(&edge.provenance), Some(edge.confidence));
                    let reasons = edge_reasons(edge, relationship);
                    (freshness, trust, reasons)
                })
                .collect::<Vec<_>>();
            let edge_freshness = edge_pairs
                .iter()
                .map(|(freshness, _, _)| *freshness)
                .collect::<Vec<_>>();
            let edge_trust = edge_pairs
                .iter()
                .map(|(_, trust, _)| trust.clone())
                .collect::<Vec<_>>();
            let edge_reasons = edge_pairs
                .iter()
                .map(|(_, _, reasons)| reasons.clone())
                .collect::<Vec<_>>();
            let trust = aggregate_trust(&edge_trust);
            let reasons = path_reasons(&path, relationship);
            ImpactPathEvidence {
                path,
                edge_freshness,
                edge_trust,
                edge_reasons,
                trust,
                reasons,
            }
        })
        .collect()
}

fn aggregate_trust(trust: &[EvidenceTrust]) -> EvidenceTrust {
    if trust.is_empty() {
        return evidence_trust(EvidenceFreshness::Unknown, None, None);
    }
    let score =
        round_trust_score(trust.iter().map(|trust| trust.score).sum::<f64>() / trust.len() as f64);
    let level = if score >= 0.85 {
        "high"
    } else if score >= 0.65 {
        "medium"
    } else if score >= 0.35 {
        "low"
    } else {
        "minimal"
    };
    EvidenceTrust {
        score,
        level: level.to_owned(),
        factors: vec![format!("aggregate_edges:{}", trust.len())],
    }
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
            let trust = evidence_trust(freshness, provenance.as_ref(), None);
            let reasons = related_file_reasons(relationship_count, provenance.as_ref());
            ImpactRelatedFile {
                path,
                relationship_count,
                freshness,
                provenance,
                trust,
                reasons,
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
        seen.insert(path.clone());
    }
    for path in scoped_paths {
        if seen.contains(path) {
            continue;
        }
        rows.push(FileFreshnessRow {
            path: path.clone(),
            freshness: freshness_for_hash(None, None),
            indexed_content_hash: None,
            current_content_hash: None,
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
    use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
    use std::time::{SystemTime, UNIX_EPOCH};

    use symdex_core::{
        RepoRoot, RepositoryRefSnapshot, SemanticLayer, SemanticLayerMode, SemanticLayerStatus,
        content_hash,
    };
    use symdex_embed::LayeredEmbedConfig;
    use symdex_store::{
        CallRecord, CallSearchRow, ContextPack, ContextPackLimits, EvidenceFreshness,
        EvidenceProvenance, ExpectedVectorPoint, FileFreshnessSnapshot, FileRecord, PointPayload,
        RepositoryRecord, RetrievedPoint, ScoredPoint, SemanticLayerManifestSummary,
        SemanticRoutingSummary, SqliteStore, StorageHealthStatus, StoreConfig, SymbolRecord,
        SymbolSearchRow, TestRecord,
    };

    use crate::{
        CallDirection, ContextEvidenceSource, ContextPackMode, FreshnessScope, QueryMode,
        SemanticSearchOptions, SemanticSearchResult, SemanticSearchSummary,
        VectorVerifySemanticLayer, build_debug_context_pack, build_freshness_report,
        build_impact_summary, build_unified_context_pack, evidence_trust, freshness_rows,
        parse_runtime_input, resolve_semantic_search_target, run_call_graph, run_call_path,
        run_context_pack, run_debug_context_pack, run_impact, run_semantic_search,
        run_symbol_search, semantic_reasons, semantic_result_from_point,
        semantic_status_from_routing, vector_verify_all_summary, vector_verify_summary,
    };
    use symdex_store::QualityGenerationProgress;

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
    fn semantic_routing_defaults_to_fast_without_generation() {
        let target = resolve_semantic_search_target(
            "repo",
            SemanticSearchOptions::default(),
            None,
            &sample_layered_config(),
        )
        .expect("target should resolve");

        assert_eq!(target.requested_layer, SemanticLayerMode::Auto);
        assert_eq!(target.semantic_layer, SemanticLayer::Fast);
        assert_eq!(target.embedding_model, "fast-model");
        assert_eq!(target.vector_table, "symdex_repo_fast_model");
        assert_eq!(target.quality_status, SemanticLayerStatus::Missing);
        assert_eq!(
            target.fallback_reason.as_deref(),
            Some("semantic_generation_missing_using_fast_layer")
        );
    }

    #[test]
    fn semantic_routing_auto_uses_ready_active_quality() {
        let routing = sample_routing_summary(
            SemanticLayer::Quality,
            SemanticLayerStatus::QualityReady,
            Some(sample_manifest(SemanticLayer::Quality, true)),
        );

        let target = resolve_semantic_search_target(
            "repo",
            SemanticSearchOptions::default(),
            Some(&routing),
            &sample_layered_config(),
        )
        .expect("target should resolve");

        assert_eq!(target.requested_layer, SemanticLayerMode::Auto);
        assert_eq!(target.semantic_layer, SemanticLayer::Quality);
        assert_eq!(target.embedding_model, "quality-model");
        assert_eq!(target.vector_table, "symdex_repo_quality_model");
        assert_eq!(target.quality_status, SemanticLayerStatus::QualityReady);
        assert_eq!(target.fallback_reason, None);
    }

    #[test]
    fn semantic_routing_auto_falls_back_when_quality_is_pending() {
        let routing = sample_routing_summary(
            SemanticLayer::Quality,
            SemanticLayerStatus::QualityPending,
            Some(sample_manifest(SemanticLayer::Quality, false)),
        );

        let target = resolve_semantic_search_target(
            "repo",
            SemanticSearchOptions::default(),
            Some(&routing),
            &sample_layered_config(),
        )
        .expect("target should resolve");

        assert_eq!(target.semantic_layer, SemanticLayer::Fast);
        assert_eq!(target.embedding_model, "fast-model");
        assert_eq!(
            target.fallback_reason.as_deref(),
            Some("quality_manifest_incomplete_using_fast_layer")
        );
    }

    #[test]
    fn semantic_routing_auto_reports_active_fast_quality_fallback_reason() {
        let routing = sample_routing_summary(
            SemanticLayer::Fast,
            SemanticLayerStatus::QualityPending,
            Some(sample_manifest(SemanticLayer::Quality, false)),
        );

        let target = resolve_semantic_search_target(
            "repo",
            SemanticSearchOptions::default(),
            Some(&routing),
            &sample_layered_config(),
        )
        .expect("target should resolve");

        assert_eq!(target.semantic_layer, SemanticLayer::Fast);
        assert_eq!(
            target.fallback_reason.as_deref(),
            Some("quality_manifest_incomplete_using_fast_layer")
        );
    }

    #[test]
    fn semantic_status_defaults_to_fast_without_generation() {
        let status =
            semantic_status_from_routing("repo", None, None, None, &sample_layered_config());

        assert_eq!(status.repository_id, "repo");
        assert_eq!(status.generation_id, None);
        assert_eq!(status.active_layer, SemanticLayer::Fast);
        assert_eq!(status.quality_status, SemanticLayerStatus::Missing);
        assert_eq!(status.fast.embedding_model, "fast-model");
        assert_eq!(status.fast.embedding_dimension, None);
        assert_eq!(status.quality.embedding_model, "quality-model");
        assert_eq!(status.quality_progress, None);
        assert_eq!(
            status.fallback_reason.as_deref(),
            Some("semantic_generation_missing_using_fast_layer")
        );
    }

    #[test]
    fn semantic_status_reports_pending_quality_progress_and_error() {
        let routing = sample_routing_summary(
            SemanticLayer::Fast,
            SemanticLayerStatus::QualityFailed,
            Some(sample_manifest(SemanticLayer::Quality, false)),
        );
        let progress = sample_quality_progress(0, 1, 0, 1);
        let status = semantic_status_from_routing(
            "repo",
            Some(routing),
            Some(progress.clone()),
            Some("service unavailable".to_owned()),
            &sample_layered_config(),
        );

        assert_eq!(status.generation_id.as_deref(), Some("generation-1"));
        assert_eq!(status.active_layer, SemanticLayer::Fast);
        assert_eq!(status.quality_status, SemanticLayerStatus::QualityFailed);
        assert_eq!(status.quality_progress, Some(progress));
        assert_eq!(
            status.latest_quality_error.as_deref(),
            Some("service unavailable")
        );
        assert_eq!(
            status.fallback_reason.as_deref(),
            Some("quality_manifest_incomplete_using_fast_layer")
        );
    }

    #[test]
    fn semantic_status_reports_ready_quality_without_fallback() {
        let routing = sample_routing_summary(
            SemanticLayer::Quality,
            SemanticLayerStatus::QualityReady,
            Some(sample_manifest(SemanticLayer::Quality, true)),
        );
        let progress = sample_quality_progress(1, 0, 0, 0);
        let status = semantic_status_from_routing(
            "repo",
            Some(routing),
            Some(progress),
            None,
            &sample_layered_config(),
        );

        assert_eq!(status.active_layer, SemanticLayer::Quality);
        assert_eq!(status.quality_status, SemanticLayerStatus::QualityReady);
        assert_eq!(status.fallback_reason, None);
        assert_eq!(status.quality.current_chunks, 1);
        assert!(status.quality.is_complete);
    }

    #[test]
    fn semantic_routing_forced_fast_uses_fast_manifest() {
        let routing = sample_routing_summary(
            SemanticLayer::Quality,
            SemanticLayerStatus::QualityReady,
            Some(sample_manifest(SemanticLayer::Quality, true)),
        );

        let target = resolve_semantic_search_target(
            "repo",
            SemanticSearchOptions {
                semantic_layer: SemanticLayerMode::Fast,
            },
            Some(&routing),
            &sample_layered_config(),
        )
        .expect("target should resolve");

        assert_eq!(target.requested_layer, SemanticLayerMode::Fast);
        assert_eq!(target.semantic_layer, SemanticLayer::Fast);
        assert_eq!(target.embedding_model, "fast-model");
        assert_eq!(target.fallback_reason, None);
    }

    #[test]
    fn semantic_routing_forced_quality_requires_ready_complete_manifest() {
        let pending = sample_routing_summary(
            SemanticLayer::Fast,
            SemanticLayerStatus::QualityPending,
            Some(sample_manifest(SemanticLayer::Quality, false)),
        );
        let error = resolve_semantic_search_target(
            "repo",
            SemanticSearchOptions {
                semantic_layer: SemanticLayerMode::Quality,
            },
            Some(&pending),
            &sample_layered_config(),
        )
        .expect_err("pending quality should fail");
        assert!(error.contains("status is `quality_pending`"));

        let ready = sample_routing_summary(
            SemanticLayer::Fast,
            SemanticLayerStatus::QualityReady,
            Some(sample_manifest(SemanticLayer::Quality, true)),
        );
        let target = resolve_semantic_search_target(
            "repo",
            SemanticSearchOptions {
                semantic_layer: SemanticLayerMode::Quality,
            },
            Some(&ready),
            &sample_layered_config(),
        )
        .expect("ready quality should resolve");

        assert_eq!(target.requested_layer, SemanticLayerMode::Quality);
        assert_eq!(target.semantic_layer, SemanticLayer::Quality);
        assert_eq!(target.embedding_model, "quality-model");
        assert_eq!(target.fallback_reason, None);
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
    fn evidence_trust_combines_freshness_provenance_confidence_and_index_metadata() {
        let high = evidence_trust(
            EvidenceFreshness::Fresh,
            Some(&complete_provenance()),
            Some(1.0),
        );
        assert_eq!(high.score, 1.0);
        assert_eq!(high.level, "high");
        assert!(
            high.factors
                .iter()
                .any(|factor| factor == "provenance:complete")
        );
        assert!(
            high.factors
                .iter()
                .any(|factor| factor == "index_metadata:complete")
        );

        let partial = evidence_trust(
            EvidenceFreshness::Stale,
            Some(&partial_provenance()),
            Some(0.5),
        );
        assert_eq!(partial.score, 0.44);
        assert_eq!(partial.level, "low");
        assert!(
            partial
                .factors
                .iter()
                .any(|factor| factor == "provenance:partial")
        );

        let missing = evidence_trust(EvidenceFreshness::Unknown, None, None);
        assert_eq!(missing.score, 0.41);
        assert_eq!(missing.level, "low");
        assert!(
            missing
                .factors
                .iter()
                .any(|factor| factor == "provenance:missing")
        );
    }

    #[test]
    fn semantic_reasons_explain_vector_result_metadata() {
        let reasons = semantic_reasons(0.81234, "src/lib.rs", Some("crate::run"), "function");

        assert!(
            reasons
                .iter()
                .any(|reason| reason == "semantic_vector_match")
        );
        assert!(
            reasons
                .iter()
                .any(|reason| reason == "semantic_score:0.8123")
        );
        assert!(reasons.iter().any(|reason| reason == "chunk_kind:function"));
        assert!(
            reasons
                .iter()
                .any(|reason| reason == "symbol_payload:crate::run")
        );
    }

    #[test]
    fn context_pack_rejects_empty_query() {
        let error = run_context_pack(".", " ", 8).expect_err("empty query should fail");
        assert!(error.contains("requires a symbol query"));
    }

    #[test]
    fn context_pack_mode_parses_supported_values() {
        assert_eq!(
            ContextPackMode::parse("structural").expect("structural should parse"),
            ContextPackMode::Structural
        );
        assert_eq!(
            ContextPackMode::parse("unified").expect("unified should parse"),
            ContextPackMode::Unified
        );
        assert!(ContextPackMode::parse("semantic").is_err());
    }

    #[test]
    fn unified_context_pack_falls_back_to_structural_evidence_when_semantic_unavailable() {
        let structural = sample_context_pack();
        let current_hashes = BTreeMap::from([("src/lib.rs".to_owned(), "hash-current".to_owned())]);

        let pack = build_unified_context_pack(
            structural,
            Err("connection refused".to_owned()),
            &current_hashes,
            8,
        );

        assert_eq!(pack.format, "symdex.context_pack.v2");
        assert_eq!(pack.mode, "unified");
        assert!(
            pack.notes
                .iter()
                .any(|note| { note == "semantic_unavailable:local_service_unavailable" })
        );
        assert!(pack.items.iter().any(|item| {
            item.id == "symbol:sym-main"
                && item.evidence_source == ContextEvidenceSource::Structural
                && item.freshness == "stale"
        }));
        assert!(pack.files.iter().any(|file| {
            file.path == "src/lib.rs" && file.evidence_source == ContextEvidenceSource::Structural
        }));
        let json = serde_json::to_value(&pack).expect("pack should serialize");
        assert!(json.get("source_text").is_none());
        assert!(pack.items.iter().all(|item| {
            let item_json = serde_json::to_value(item).expect("item should serialize");
            item_json.get("source_text").is_none()
        }));
    }

    #[test]
    fn unified_context_pack_notes_missing_vector_collection() {
        let pack = build_unified_context_pack(
            sample_context_pack(),
            Err("vector table not found".to_owned()),
            &BTreeMap::new(),
            8,
        );

        assert!(
            pack.notes
                .iter()
                .any(|note| { note == "semantic_unavailable:missing_vector_collection" })
        );
    }

    #[test]
    fn unified_context_pack_merges_overlapping_semantic_and_structural_evidence() {
        let structural = sample_context_pack();
        let semantic = SemanticSearchSummary {
            repository_id: "repo".to_owned(),
            vector_table: "symdex_repo_model".to_owned(),
            requested_layer: SemanticLayerMode::Auto,
            semantic_layer: SemanticLayer::Fast,
            embedding_model: "model".to_owned(),
            generation_id: Some("generation-1".to_owned()),
            quality_status: SemanticLayerStatus::FastReady,
            fallback_reason: None,
            query: "main".to_owned(),
            results: vec![
                semantic_result(
                    "point-main",
                    "chunk-main",
                    Some("sym-main"),
                    "src/lib.rs",
                    0.91,
                ),
                semantic_result("point-other", "chunk-other", None, "src/other.rs", 0.88),
            ],
        };
        let current_hashes = BTreeMap::from([
            ("src/lib.rs".to_owned(), "hash-indexed".to_owned()),
            ("src/other.rs".to_owned(), "hash-indexed".to_owned()),
        ]);

        let pack = build_unified_context_pack(structural, Ok(semantic), &current_hashes, 8);

        let merged = pack
            .items
            .iter()
            .find(|item| item.id == "symbol:sym-main")
            .expect("semantic symbol hit should merge with structural symbol");
        assert_eq!(merged.evidence_source, ContextEvidenceSource::Both);
        assert_eq!(merged.score, Some(0.91));
        assert_eq!(merged.point_id.as_deref(), Some("point-main"));
        assert_eq!(merged.text_hash.as_deref(), Some("text-chunk-main"));
        assert_eq!(merged.freshness, "fresh");

        let semantic_only = pack
            .items
            .iter()
            .find(|item| item.id == "chunk:chunk-other")
            .expect("semantic-only chunk should be retained");
        assert_eq!(
            semantic_only.evidence_source,
            ContextEvidenceSource::Semantic
        );
        assert_eq!(semantic_only.score, Some(0.88));
        assert!(pack.files.iter().any(|file| {
            file.path == "src/lib.rs" && file.evidence_source == ContextEvidenceSource::Both
        }));
        assert!(pack.notes.iter().any(|note| note == "semantic_results:2"));
        assert_eq!(pack.items[0].evidence_source, ContextEvidenceSource::Both);
    }

    #[test]
    fn semantic_result_preserves_vector_identity_fields() {
        let expected = expected_point("point-id", "chunk-id", "text-hash");
        let result = semantic_result_from_point(ScoredPoint {
            id: "point-id".to_owned(),
            score: 0.77,
            payload: PointPayload {
                symbol_id: Some("sym-id".to_owned()),
                symbol_name: Some("crate::run".to_owned()),
                ..payload_for(&expected, "text-hash")
            },
        });

        assert_eq!(result.point_id, "point-id");
        assert_eq!(result.chunk_id, "chunk-id");
        assert_eq!(result.symbol_id.as_deref(), Some("sym-id"));
        assert_eq!(result.text_hash, "text-hash");
        assert_eq!(result.language, "rust");
        assert!(
            result
                .reasons
                .iter()
                .any(|reason| reason == "semantic_score:0.7700")
        );
    }

    #[test]
    fn debug_context_rejects_empty_input() {
        let error = run_debug_context_pack(".", " ", 8).expect_err("empty input should fail");
        assert!(error.contains("requires runtime failure input"));
    }

    #[test]
    fn vector_verify_summary_detects_missing_stale_and_orphaned_points() {
        let aligned = expected_point("point-ok", "chunk-ok", "hash-ok");
        let stale = expected_point("point-stale", "chunk-stale", "hash-current");
        let missing = expected_point("point-missing", "chunk-missing", "hash-missing");
        let summary = vector_verify_summary(
            "repo",
            "fast",
            "symdex_repo_model".to_owned(),
            "nomic-embed-text".to_owned(),
            true,
            vec![aligned.clone(), stale.clone(), missing],
            vec![
                retrieved_point("point-ok", payload_for(&aligned, "hash-ok")),
                retrieved_point("point-stale", payload_for(&stale, "old-hash")),
                retrieved_point("point-orphan", payload_for(&aligned, "hash-ok")),
            ],
        );

        assert_eq!(summary.expected_vector_points, 3);
        assert_eq!(summary.semantic_layer, "fast");
        assert_eq!(summary.vector_payload_points, 3);
        assert_eq!(summary.missing_points, 1);
        assert_eq!(summary.stale_payload_points, 1);
        assert_eq!(summary.orphaned_points, 1);
        assert_eq!(summary.missing_point_ids, vec!["point-missing"]);
        assert_eq!(summary.stale_payload_point_ids, vec!["point-stale"]);
        assert_eq!(summary.orphaned_point_ids, vec!["point-orphan"]);
        assert!(summary.rows.iter().any(|row| {
            row.status == StorageHealthStatus::Error && row.label == "missing_point"
        }));
        assert!(summary.rows.iter().any(|row| {
            row.status == StorageHealthStatus::Warning && row.label == "stale_payload"
        }));
        assert!(summary.rows.iter().any(|row| {
            row.status == StorageHealthStatus::Warning && row.label == "orphaned_point"
        }));
        assert!(!format!("{summary:?}").contains("source_text"));
    }

    #[test]
    fn vector_verify_summary_reports_ok_when_payloads_align() {
        let expected = expected_point("point-ok", "chunk-ok", "hash-ok");
        let summary = vector_verify_summary(
            "repo",
            "fast",
            "symdex_repo_model".to_owned(),
            "nomic-embed-text".to_owned(),
            true,
            vec![expected.clone()],
            vec![retrieved_point(
                "point-ok",
                payload_for(&expected, "hash-ok"),
            )],
        );

        assert_eq!(summary.missing_points, 0);
        assert_eq!(summary.stale_payload_points, 0);
        assert_eq!(summary.orphaned_points, 0);
        assert!(summary.missing_point_ids.is_empty());
        assert!(summary.stale_payload_point_ids.is_empty());
        assert!(summary.orphaned_point_ids.is_empty());
        assert!(summary.rows.iter().any(|row| {
            row.status == StorageHealthStatus::Ok && row.label == "vector_verify_ok"
        }));
    }

    #[test]
    fn vector_verify_summary_marks_missing_collection_points_repairable() {
        let expected = expected_point("point-missing", "chunk-missing", "hash-missing");
        let summary = vector_verify_summary(
            "repo",
            "quality",
            "symdex_repo_model".to_owned(),
            "nomic-embed-text".to_owned(),
            false,
            vec![expected],
            Vec::new(),
        );

        assert_eq!(summary.missing_points, 1);
        assert_eq!(summary.missing_point_ids, vec!["point-missing"]);
        assert!(summary.rows.iter().any(|row| {
            row.status == StorageHealthStatus::Error && row.label == "collection_missing"
        }));
    }

    #[test]
    fn vector_verify_semantic_layer_parses_maintenance_modes() {
        assert_eq!(
            VectorVerifySemanticLayer::parse("fast"),
            Ok(VectorVerifySemanticLayer::Fast)
        );
        assert_eq!(
            VectorVerifySemanticLayer::parse("quality"),
            Ok(VectorVerifySemanticLayer::Quality)
        );
        assert_eq!(
            VectorVerifySemanticLayer::parse("all"),
            Ok(VectorVerifySemanticLayer::All)
        );
        assert!(VectorVerifySemanticLayer::parse("auto").is_err());
    }

    #[test]
    fn vector_verify_all_summary_keeps_layer_health_independent() {
        let fast_expected = expected_point("point-fast", "chunk-fast", "hash-fast");
        let fast = vector_verify_summary(
            "repo",
            "fast",
            "symdex_repo_nomic_embed_text".to_owned(),
            "nomic-embed-text".to_owned(),
            true,
            vec![fast_expected.clone()],
            vec![retrieved_point(
                "point-fast",
                payload_for(&fast_expected, "hash-fast"),
            )],
        );
        let quality = vector_verify_summary(
            "repo",
            "quality",
            "symdex_repo_nomic_embed_text_v2_moe".to_owned(),
            "mxbai-embed-large".to_owned(),
            false,
            vec![expected_point(
                "point-quality",
                "chunk-quality",
                "hash-quality",
            )],
            Vec::new(),
        );

        let summary = vector_verify_all_summary("repo", vec![fast.clone(), quality]);

        assert_eq!(summary.semantic_layer, "all");
        assert_eq!(summary.expected_vector_points, 2);
        assert_eq!(summary.missing_points, 1);
        assert_eq!(summary.layer_summaries.len(), 2);
        assert_eq!(summary.layer_summaries[0].semantic_layer, "fast");
        assert_eq!(summary.layer_summaries[0].missing_points, 0);
        assert_eq!(summary.layer_summaries[1].semantic_layer, "quality");
        assert_eq!(summary.layer_summaries[1].missing_points, 1);
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
    fn runtime_parser_handles_common_rust_debug_outputs() {
        let parsed = parse_runtime_input(
            "running 1 test\n\
             test tests::unit::fails ... FAILED\n\
             ---- tests::unit::fails stdout ----\n\
             thread 'tests::unit::fails' panicked at crates/app/src/lib.rs:18:9:\n\
             Error: request failed\n\
             Caused by:\n\
                 0: while handling request\n\
                 1: disk full\n\
             stack backtrace:\n\
                0:     0x0000000100000000 - std::panicking::begin_panic\n\
                1: my_crate::service::run::{{closure}}\n\
                   at crates/app/src/service.rs:44:13\n\
                2: <my_crate::Worker as my_crate::Job>::poll\n\
                   at crates/app/src/worker.rs:51:5\n\
             tracing::event target=\"my_crate::worker\" file=\"crates/app/src/worker.rs\" line=52 column=7\n\
             async stack: my_crate::tasks::spawned at crates/app/src/tasks.rs:9:3\n\
             failures:\n\
                 tests::unit::fails\n",
        );

        assert_eq!(parsed.failing_tests, vec!["tests::unit::fails"]);
        assert!(parsed.frames.iter().any(|frame| {
            frame.path.as_deref() == Some("crates/app/src/lib.rs")
                && frame.line == Some(18)
                && frame.column == Some(9)
        }));
        assert!(parsed.frames.iter().all(|frame| {
            frame.symbol.as_deref() != Some("while handling request")
                && frame.symbol.as_deref() != Some("disk full")
        }));
        assert!(parsed.frames.iter().any(|frame| {
            frame.symbol.as_deref() == Some("my_crate::service::run::{{closure}}")
                && frame.path.as_deref() == Some("crates/app/src/service.rs")
                && frame.line == Some(44)
                && frame.column == Some(13)
        }));
        assert!(parsed.frames.iter().any(|frame| {
            frame.symbol.as_deref() == Some("<my_crate::Worker as my_crate::Job>::poll")
                && frame.path.as_deref() == Some("crates/app/src/worker.rs")
                && frame.line == Some(51)
                && frame.column == Some(5)
        }));
        assert!(parsed.frames.iter().any(|frame| {
            frame.symbol.as_deref() == Some("my_crate::worker")
                && frame.path.as_deref() == Some("crates/app/src/worker.rs")
                && frame.line == Some(52)
                && frame.column == Some(7)
        }));
        assert!(parsed.frames.iter().any(|frame| {
            frame.symbol.as_deref() == Some("my_crate::tasks::spawned")
                && frame.path.as_deref() == Some("crates/app/src/tasks.rs")
                && frame.line == Some(9)
                && frame.column == Some(3)
        }));
    }

    #[test]
    fn runtime_parser_extracts_active_language_file_locations() {
        let parsed = parse_runtime_input(
            "at src/Program.cs:10:5\n\
             at web/app.jsx:4:1\n\
             at web/util.ts:8:3\n\
             at web/component.tsx:12:7\n",
        );

        assert_eq!(
            parsed
                .frames
                .iter()
                .map(|frame| (frame.path.as_deref(), frame.line, frame.column))
                .collect::<Vec<_>>(),
            vec![
                (Some("src/Program.cs"), Some(10), Some(5)),
                (Some("web/app.jsx"), Some(4), Some(1)),
                (Some("web/util.ts"), Some(8), Some(3)),
                (Some("web/component.tsx"), Some(12), Some(7)),
            ]
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
             not a stack trace line with broken.rs: text",
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
        assert_eq!(pack.frames[0].trust.score, 1.0);
        assert_eq!(pack.frames[0].trust.level, "high");
        assert!(pack.frames[0].reasons.iter().any(|reason| {
            reason == "file_provenance_match"
                || reason == "symbols_at_runtime_location"
                || reason == "calls_at_runtime_line:1"
        }));
        assert_eq!(pack.frames[1].trust.score, 0.84);
        assert_eq!(pack.frames[1].trust.level, "medium");
        assert!(!pack.frames[3].matched);
        assert!(
            pack.frames[3]
                .reasons
                .iter()
                .any(|reason| reason == "unmatched_runtime_frame")
        );
        assert!(
            pack.notes
                .iter()
                .any(|note| note == "malformed_runtime_lines_ignored")
        );
    }

    #[test]
    fn debug_context_maps_runtime_failures_to_indexed_tests_when_available() {
        let mut fixture = DebugFixture::new();
        let repository_id = fixture.root.id().to_owned();
        persist_test_calling_callee(&mut fixture.store, &repository_id);
        let input = "test tests::covers_callee ... FAILED\ntest tests::missing_case ... FAILED\n";

        let pack =
            build_debug_context_pack(&fixture.root, &fixture.store, input, 8, &BTreeMap::new())
                .expect("debug context should build");

        assert_eq!(
            pack.likely_tests,
            vec!["crate::tests::covers_callee", "tests::missing_case"]
        );
        assert!(
            pack.notes
                .iter()
                .any(|note| note == "likely_tests_mapped_to_indexed_tests")
        );
        assert!(
            pack.notes
                .iter()
                .any(|note| { note == "likely_tests_include_unmatched_runtime_failure_names" })
        );
    }

    #[test]
    fn impact_includes_tests_that_directly_call_target_symbol() {
        let mut fixture = DebugFixture::new();
        let repository_id = fixture.root.id().to_owned();
        persist_test_calling_callee(&mut fixture.store, &repository_id);

        let summary = build_impact_summary(&fixture.root, &fixture.store, None, "callee")
            .expect("impact summary should build");

        assert_eq!(summary.tests_likely, vec!["crate::tests::covers_callee"]);
        let test_caller = summary
            .direct_callers
            .iter()
            .find(|evidence| {
                evidence.row.symbol_qualified_name.as_deref() == Some("crate::tests::covers_callee")
            })
            .expect("test caller should be included");
        assert_eq!(test_caller.trust.level, "medium");
        assert_eq!(test_caller.trust.score, 0.74);
        assert!(
            test_caller
                .reasons
                .iter()
                .any(|reason| reason == "relationship:direct_caller")
        );
        assert!(
            test_caller
                .reasons
                .iter()
                .any(|reason| { reason == "symbol_match:crate::tests::covers_callee" })
        );
        assert!(
            summary
                .related_files
                .iter()
                .all(|file| !file.trust.factors.is_empty())
        );
        assert!(
            summary
                .related_files
                .iter()
                .all(|file| !file.reasons.is_empty())
        );
        assert!(
            summary
                .notes
                .iter()
                .any(|note| note == "likely_tests_from_indexed_direct_test_calls")
        );
    }

    #[test]
    fn impact_includes_non_rust_tests_only_with_direct_call_evidence() {
        let mut fixture = DebugFixture::new();
        let repository_id = fixture.root.id().to_owned();
        persist_csharp_test_calling_callee(&mut fixture.store, &repository_id);

        let summary = build_impact_summary(&fixture.root, &fixture.store, None, "callee")
            .expect("impact summary should build");

        assert_eq!(
            summary.tests_likely,
            vec!["Demo::CalculatorTests::AddsNumbers"]
        );
        assert!(
            summary
                .notes
                .iter()
                .any(|note| note == "likely_tests_from_indexed_direct_test_calls")
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

    #[test]
    fn freshness_rows_include_unknown_for_explicit_scoped_paths() {
        let rows = freshness_rows(
            &[],
            &BTreeMap::new(),
            &BTreeSet::from(["src/unknown.rs".to_owned()]),
        );

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].path, "src/unknown.rs");
        assert_eq!(rows[0].freshness, EvidenceFreshness::Unknown);
        assert_eq!(rows[0].indexed_content_hash, None);
        assert_eq!(rows[0].current_content_hash, None);
    }

    #[test]
    fn scoped_freshness_report_returns_explicit_paths_only() {
        let mut fixture = DebugFixture::new();
        let missing_path = fixture.root.path().join("src/missing.rs");
        fs::write(&missing_path, "fn missing() {}\n").expect("missing file should be written");
        let fresh_hash = content_hash(b"fn fresh() {}\n");
        fixture
            .store
            .replace_file_facts(
                &FileRecord {
                    id: "file-fresh".to_owned(),
                    repository_id: fixture.root.id().to_owned(),
                    path: "src/fresh.rs".to_owned(),
                    language: "rust".to_owned(),
                    content_hash: fresh_hash,
                    index_run_id: "run".to_owned(),
                    parser_version: "parser".to_owned(),
                },
                &[sample_symbol(
                    "sym-fresh",
                    "file-fresh",
                    "fresh",
                    "crate::fresh",
                )],
                &[],
                &[],
            )
            .expect("fresh file should be updated");

        let summary = build_freshness_report(
            &fixture.root,
            &fixture.store,
            FreshnessScope {
                symbol_query: None,
                paths: vec![
                    "src/unknown.rs".to_owned(),
                    "src/fresh.rs".to_owned(),
                    "src/missing.rs".to_owned(),
                ],
            },
        )
        .expect("scoped freshness should build");

        assert_eq!(
            summary
                .files
                .iter()
                .map(|row| (row.path.as_str(), row.freshness))
                .collect::<Vec<_>>(),
            vec![
                ("src/fresh.rs", EvidenceFreshness::Fresh),
                ("src/missing.rs", EvidenceFreshness::Missing),
                ("src/unknown.rs", EvidenceFreshness::Unknown),
            ]
        );
    }

    #[test]
    fn freshness_report_uses_active_ref_manifest_when_available() {
        let mut fixture = DebugFixture::new();
        let active_ref =
            RepositoryRefSnapshot::detect(&fixture.root).expect("active ref should detect");
        let fresh_hash = content_hash(b"fn fresh() {}\n");
        let stale_hash = content_hash(b"fn stale() {}\n");
        fixture
            .store
            .sync_repository_ref(&active_ref)
            .expect("active ref should sync");
        fixture
            .store
            .replace_file_facts_for_ref_with_tests(
                &active_ref.id,
                &FileRecord {
                    id: "file-fresh-current".to_owned(),
                    repository_id: fixture.root.id().to_owned(),
                    path: "src/fresh.rs".to_owned(),
                    language: "rust".to_owned(),
                    content_hash: fresh_hash,
                    index_run_id: "run-current".to_owned(),
                    parser_version: "parser".to_owned(),
                },
                &[sample_symbol(
                    "sym-fresh-current",
                    "file-fresh-current",
                    "fresh",
                    "crate::fresh",
                )],
                &[],
                &[],
                &[],
            )
            .expect("fresh file should persist for active ref");
        fixture
            .store
            .replace_file_facts_for_ref_with_tests(
                &active_ref.id,
                &FileRecord {
                    id: "file-stale-current".to_owned(),
                    repository_id: fixture.root.id().to_owned(),
                    path: "src/stale.rs".to_owned(),
                    language: "rust".to_owned(),
                    content_hash: stale_hash,
                    index_run_id: "run-current".to_owned(),
                    parser_version: "parser".to_owned(),
                },
                &[sample_symbol(
                    "sym-stale-current",
                    "file-stale-current",
                    "stale",
                    "crate::stale",
                )],
                &[],
                &[],
                &[],
            )
            .expect("stale file should persist for active ref");

        let summary = build_freshness_report(
            &fixture.root,
            &fixture.store,
            FreshnessScope {
                symbol_query: None,
                paths: Vec::new(),
            },
        )
        .expect("freshness report should build");

        assert_eq!(summary.count(EvidenceFreshness::Stale), 0);
        assert_eq!(summary.count(EvidenceFreshness::Deleted), 0);
        assert_eq!(
            summary
                .files
                .iter()
                .map(|row| (row.path.as_str(), row.freshness))
                .collect::<Vec<_>>(),
            vec![
                ("src/fresh.rs", EvidenceFreshness::Fresh),
                ("src/stale.rs", EvidenceFreshness::Fresh),
            ]
        );
    }

    #[test]
    fn scoped_freshness_report_rejects_paths_outside_symbol_scope() {
        let fixture = DebugFixture::new();

        let error = build_freshness_report(
            &fixture.root,
            &fixture.store,
            FreshnessScope {
                symbol_query: Some("fresh".to_owned()),
                paths: vec!["src/stale.rs".to_owned()],
            },
        )
        .expect_err("incompatible symbol/path scope should fail");

        assert!(error.contains("paths are outside the symbol freshness scope"));
        assert!(error.contains("src/stale.rs"));
    }

    #[test]
    fn scoped_freshness_report_accepts_paths_inside_symbol_scope() {
        let fixture = DebugFixture::new();

        let summary = build_freshness_report(
            &fixture.root,
            &fixture.store,
            FreshnessScope {
                symbol_query: Some("fresh".to_owned()),
                paths: vec!["src/fresh.rs".to_owned()],
            },
        )
        .expect("compatible symbol/path scope should build");

        assert_eq!(summary.symbol_query.as_deref(), Some("fresh"));
        assert_eq!(summary.files.len(), 1);
        assert_eq!(summary.files[0].path, "src/fresh.rs");
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

    fn sample_context_pack() -> ContextPack {
        ContextPack {
            format: "symdex.context_pack.v1".to_owned(),
            repository_id: "repo".to_owned(),
            query: "main".to_owned(),
            focus_symbols: vec![SymbolSearchRow {
                id: "sym-main".to_owned(),
                name: "main".to_owned(),
                qualified_name: "crate::main".to_owned(),
                kind: "function".to_owned(),
                path: "src/lib.rs".to_owned(),
                start_line: 1,
                end_line: 5,
                provenance: complete_provenance_with_hash("hash-indexed"),
            }],
            direct_callers: vec![CallSearchRow {
                callee_text: "main".to_owned(),
                call_line: 12,
                confidence: 1.0,
                resolution_status: "resolved_exact".to_owned(),
                symbol_id: Some("sym-caller".to_owned()),
                symbol_name: Some("caller".to_owned()),
                symbol_qualified_name: Some("crate::caller".to_owned()),
                symbol_kind: Some("function".to_owned()),
                path: Some("src/lib.rs".to_owned()),
                start_line: Some(10),
                end_line: Some(15),
                provenance: complete_provenance_with_hash("hash-indexed"),
            }],
            direct_callees: Vec::new(),
            files: vec!["src/lib.rs".to_owned()],
            limits: ContextPackLimits {
                max_symbols: 8,
                max_callers: 8,
                max_callees: 8,
            },
            notes: vec![
                "metadata_only_no_source_text".to_owned(),
                "direct_relationships_only".to_owned(),
            ],
        }
    }

    fn sample_layered_config() -> LayeredEmbedConfig {
        LayeredEmbedConfig {
            ollama_url: "http://localhost:11434".to_owned(),
            fast_model: "fast-model".to_owned(),
            quality_model: "quality-model".to_owned(),
            quality_enabled: true,
            truncate: true,
            batch_size: 16,
            quality_batch_size: 4,
            quality_workers: 1,
            max_chunk_bytes: 32_768,
            quality_max_chunk_bytes: 512,
        }
    }

    fn sample_routing_summary(
        active_layer: SemanticLayer,
        quality_status: SemanticLayerStatus,
        quality: Option<SemanticLayerManifestSummary>,
    ) -> SemanticRoutingSummary {
        SemanticRoutingSummary {
            repository_id: "repo".to_owned(),
            generation_id: "generation-1".to_owned(),
            active_layer,
            quality_status,
            embeddable_chunks: 1,
            fast_embedded_chunks: 1,
            quality_embedded_chunks: usize::from(quality.as_ref().is_some_and(|manifest| {
                manifest.semantic_layer == SemanticLayer::Quality && manifest.is_complete
            })),
            fast: sample_manifest(SemanticLayer::Fast, true),
            quality,
        }
    }

    fn sample_manifest(
        semantic_layer: SemanticLayer,
        is_complete: bool,
    ) -> SemanticLayerManifestSummary {
        let (embedding_model, vector_table) = match semantic_layer {
            SemanticLayer::Fast => ("fast-model", "symdex_repo_fast_model"),
            SemanticLayer::Quality => ("quality-model", "symdex_repo_quality_model"),
        };
        SemanticLayerManifestSummary {
            semantic_layer,
            embedding_model: embedding_model.to_owned(),
            embedding_dimension: 768,
            vector_table: vector_table.to_owned(),
            current_chunks: usize::from(is_complete),
            stale_chunks: usize::from(!is_complete),
            blocked_chunks: 0,
            failed_chunks: 0,
            other_chunks: 0,
            total_chunks: 1,
            expected_chunks: 1,
            is_complete,
        }
    }

    fn sample_quality_progress(
        quality_embedded_chunks: usize,
        pending_jobs: usize,
        running_jobs: usize,
        failed_jobs: usize,
    ) -> QualityGenerationProgress {
        QualityGenerationProgress {
            repository_id: "repo".to_owned(),
            generation_id: "generation-1".to_owned(),
            embeddable_chunks: 1,
            quality_eligible_chunks: 1,
            quality_ineligible_chunks: 0,
            quality_embedded_chunks,
            pending_jobs,
            running_jobs,
            succeeded_jobs: quality_embedded_chunks,
            failed_jobs,
            skipped_stale_jobs: 0,
            skipped_excluded_jobs: 0,
        }
    }

    fn semantic_result(
        point_id: &str,
        chunk_id: &str,
        symbol_id: Option<&str>,
        path: &str,
        score: f64,
    ) -> SemanticSearchResult {
        SemanticSearchResult {
            point_id: point_id.to_owned(),
            chunk_id: chunk_id.to_owned(),
            symbol_id: symbol_id.map(str::to_owned),
            score,
            path: path.to_owned(),
            start_line: 1,
            end_line: 5,
            symbol_name: symbol_id.map(|_| "crate::main".to_owned()),
            chunk_kind: "function".to_owned(),
            language: "rust".to_owned(),
            text_hash: format!("text-{chunk_id}"),
            provenance: complete_semantic_provenance("hash-indexed"),
            reasons: semantic_reasons(score, path, symbol_id.map(|_| "crate::main"), "function"),
        }
    }

    struct DebugFixture {
        root: RepoRoot,
        store: SqliteStore,
        _root_path: PathBuf,
        _db_path: PathBuf,
    }

    static NEXT_DEBUG_FIXTURE_ID: AtomicU64 = AtomicU64::new(0);

    impl DebugFixture {
        fn new() -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system time should be valid")
                .as_nanos();
            let fixture_id = NEXT_DEBUG_FIXTURE_ID.fetch_add(1, AtomicOrdering::Relaxed);
            let base = std::env::temp_dir().join(format!(
                "symdex-debug-query-test-{}-{nonce}-{fixture_id}",
                std::process::id(),
            ));
            let root_path = base.join("repo");
            fs::create_dir_all(root_path.join("src")).expect("repo should be created");
            fs::write(root_path.join("src/fresh.rs"), "fn fresh() {}\n")
                .expect("fresh file should be written");
            fs::write(root_path.join("src/stale.rs"), "fn stale() {}\n")
                .expect("stale file should be written");
            let db_path = base.join("symdex.sqlite");
            let root = RepoRoot::open(&root_path).expect("repo root should open");
            let mut store = SqliteStore::open(&StoreConfig {
                sqlite_path: db_path.clone(),
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
                &mut store,
                root.id(),
                FileFixture {
                    file_id: "file-fresh",
                    path: "src/fresh.rs",
                    content_hash: "hash-fresh",
                    symbol_id: "sym-fresh",
                    symbol_name: "fresh",
                    qualified_name: "crate::fresh",
                },
            );
            persist_file(
                &mut store,
                root.id(),
                FileFixture {
                    file_id: "file-stale",
                    path: "src/stale.rs",
                    content_hash: "hash-old",
                    symbol_id: "sym-stale",
                    symbol_name: "stale",
                    qualified_name: "crate::stale",
                },
            );
            persist_file(
                &mut store,
                root.id(),
                FileFixture {
                    file_id: "file-deleted",
                    path: "src/deleted.rs",
                    content_hash: "hash-deleted",
                    symbol_id: "sym-deleted",
                    symbol_name: "deleted",
                    qualified_name: "crate::deleted",
                },
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
                    &[sample_symbol(
                        "sym-callee",
                        "file-callee",
                        "callee",
                        "crate::callee",
                    )],
                    &[],
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
                    &[sample_symbol(
                        "sym-fresh",
                        "file-fresh",
                        "fresh",
                        "crate::fresh",
                    )],
                    &[],
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

    fn persist_file(store: &mut SqliteStore, repository_id: &str, fixture: FileFixture<'_>) {
        store
            .replace_file_facts(
                &FileRecord {
                    id: fixture.file_id.to_owned(),
                    repository_id: repository_id.to_owned(),
                    path: fixture.path.to_owned(),
                    language: "rust".to_owned(),
                    content_hash: fixture.content_hash.to_owned(),
                    index_run_id: "run".to_owned(),
                    parser_version: "parser".to_owned(),
                },
                &[sample_symbol(
                    fixture.symbol_id,
                    fixture.file_id,
                    fixture.symbol_name,
                    fixture.qualified_name,
                )],
                &[],
                &[],
            )
            .expect("file facts should persist");
    }

    struct FileFixture<'a> {
        file_id: &'a str,
        path: &'a str,
        content_hash: &'a str,
        symbol_id: &'a str,
        symbol_name: &'a str,
        qualified_name: &'a str,
    }

    fn expected_point(point_id: &str, chunk_id: &str, text_hash: &str) -> ExpectedVectorPoint {
        ExpectedVectorPoint {
            vector_point_id: point_id.to_owned(),
            chunk_id: chunk_id.to_owned(),
            path: "src/lib.rs".to_owned(),
            start_line: 1,
            end_line: 3,
            text_hash: text_hash.to_owned(),
            embedding_model: Some("nomic-embed-text".to_owned()),
            embedding_dimension: Some(768),
        }
    }

    fn payload_for(expected: &ExpectedVectorPoint, text_hash: &str) -> PointPayload {
        PointPayload {
            repository_id: "repo".to_owned(),
            file_id: "file".to_owned(),
            chunk_id: expected.chunk_id.clone(),
            symbol_id: None,
            symbol_name: None,
            path: expected.path.clone(),
            language: "rust".to_owned(),
            chunk_kind: "function".to_owned(),
            start_line: expected.start_line,
            end_line: expected.end_line,
            text_hash: text_hash.to_owned(),
            parser_version: Some("parser".to_owned()),
            content_hash: Some("content-hash".to_owned()),
            index_run_id: Some("run".to_owned()),
            embedding_model: Some("nomic-embed-text".to_owned()),
            embedding_dimension: Some(768),
            indexed_at: Some("123".to_owned()),
        }
    }

    fn retrieved_point(id: &str, payload: PointPayload) -> RetrievedPoint {
        RetrievedPoint {
            id: id.to_owned(),
            payload,
        }
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

    fn persist_test_calling_callee(store: &mut SqliteStore, repository_id: &str) {
        store
            .replace_file_facts_with_tests(
                &FileRecord {
                    id: "file-test".to_owned(),
                    repository_id: repository_id.to_owned(),
                    path: "src/fresh_tests.rs".to_owned(),
                    language: "rust".to_owned(),
                    content_hash: "hash-test".to_owned(),
                    index_run_id: "run".to_owned(),
                    parser_version: "parser".to_owned(),
                },
                &[sample_symbol(
                    "sym-test",
                    "file-test",
                    "covers_callee",
                    "crate::tests::covers_callee",
                )],
                &[],
                &[CallRecord {
                    id: "call-test-callee".to_owned(),
                    caller_symbol_id: "sym-test".to_owned(),
                    callee_text: "callee".to_owned(),
                    callee_symbol_id: Some("sym-callee".to_owned()),
                    call_line: 3,
                    confidence: 1.0,
                    resolution_status: "resolved_exact".to_owned(),
                    index_run_id: "run".to_owned(),
                    parser_version: "parser".to_owned(),
                }],
                &[TestRecord {
                    id: "test-covers-callee".to_owned(),
                    repository_id: repository_id.to_owned(),
                    file_id: "file-test".to_owned(),
                    path: "src/fresh_tests.rs".to_owned(),
                    symbol_id: Some("sym-test".to_owned()),
                    name: "covers_callee".to_owned(),
                    qualified_name: "crate::tests::covers_callee".to_owned(),
                    framework: "rust_test".to_owned(),
                    language: "rust".to_owned(),
                    start_line: 2,
                    end_line: 4,
                    start_byte: 0,
                    end_byte: 32,
                    index_run_id: "run".to_owned(),
                    parser_version: "parser".to_owned(),
                }],
            )
            .expect("test file should persist");
    }

    fn persist_csharp_test_calling_callee(store: &mut SqliteStore, repository_id: &str) {
        store
            .replace_file_facts_with_tests(
                &FileRecord {
                    id: "file-csharp-test".to_owned(),
                    repository_id: repository_id.to_owned(),
                    path: "tests/CalculatorTests.cs".to_owned(),
                    language: "csharp".to_owned(),
                    content_hash: "hash-csharp-test".to_owned(),
                    index_run_id: "run".to_owned(),
                    parser_version: "parser".to_owned(),
                },
                &[sample_symbol(
                    "sym-csharp-test",
                    "file-csharp-test",
                    "AddsNumbers",
                    "Demo::CalculatorTests::AddsNumbers",
                )],
                &[],
                &[CallRecord {
                    id: "call-csharp-test-callee".to_owned(),
                    caller_symbol_id: "sym-csharp-test".to_owned(),
                    callee_text: "callee".to_owned(),
                    callee_symbol_id: Some("sym-callee".to_owned()),
                    call_line: 6,
                    confidence: 1.0,
                    resolution_status: "resolved_exact".to_owned(),
                    index_run_id: "run".to_owned(),
                    parser_version: "parser".to_owned(),
                }],
                &[TestRecord {
                    id: "test-csharp-adds-numbers".to_owned(),
                    repository_id: repository_id.to_owned(),
                    file_id: "file-csharp-test".to_owned(),
                    path: "tests/CalculatorTests.cs".to_owned(),
                    symbol_id: Some("sym-csharp-test".to_owned()),
                    name: "AddsNumbers".to_owned(),
                    qualified_name: "Demo::CalculatorTests::AddsNumbers".to_owned(),
                    framework: "nunit".to_owned(),
                    language: "csharp".to_owned(),
                    start_line: 4,
                    end_line: 8,
                    start_byte: 0,
                    end_byte: 96,
                    index_run_id: "run".to_owned(),
                    parser_version: "parser".to_owned(),
                }],
            )
            .expect("C# test file should persist");
    }

    fn complete_provenance() -> EvidenceProvenance {
        complete_provenance_with_hash("hash")
    }

    fn complete_provenance_with_hash(content_hash: &str) -> EvidenceProvenance {
        EvidenceProvenance {
            content_hash: Some(content_hash.to_owned()),
            index_run_id: Some("run".to_owned()),
            parser_version: Some("parser".to_owned()),
            indexed_at: Some("now".to_owned()),
            embedding_model: None,
            embedding_dimension: None,
            embedded_at: None,
        }
    }

    fn complete_semantic_provenance(content_hash: &str) -> EvidenceProvenance {
        EvidenceProvenance {
            embedding_model: Some("nomic-embed-text".to_owned()),
            embedding_dimension: Some(768),
            ..complete_provenance_with_hash(content_hash)
        }
    }

    fn partial_provenance() -> EvidenceProvenance {
        EvidenceProvenance {
            content_hash: Some("hash".to_owned()),
            index_run_id: None,
            parser_version: None,
            indexed_at: Some("now".to_owned()),
            embedding_model: None,
            embedding_dimension: None,
            embedded_at: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SemanticLayer {
    Fast,
    Quality,
}

impl SemanticLayer {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Fast => "fast",
            Self::Quality => "quality",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SemanticLayerMode {
    Auto,
    Fast,
    Quality,
}

impl SemanticLayerMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Fast => "fast",
            Self::Quality => "quality",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SemanticLayerStatus {
    Missing,
    FastReady,
    QualityPending,
    QualityReady,
    QualityStale,
    QualityBlocked,
    QualityFailed,
}

impl SemanticLayerStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Missing => "missing",
            Self::FastReady => "fast_ready",
            Self::QualityPending => "quality_pending",
            Self::QualityReady => "quality_ready",
            Self::QualityStale => "quality_stale",
            Self::QualityBlocked => "quality_blocked",
            Self::QualityFailed => "quality_failed",
        }
    }

    pub fn default_search_layer(self) -> Option<SemanticLayer> {
        match self {
            Self::Missing => None,
            Self::QualityReady => Some(SemanticLayer::Quality),
            Self::FastReady
            | Self::QualityPending
            | Self::QualityStale
            | Self::QualityBlocked
            | Self::QualityFailed => Some(SemanticLayer::Fast),
        }
    }

    pub fn quality_is_current(self) -> bool {
        self == Self::QualityReady
    }

    pub fn can_transition_to(self, next: Self) -> bool {
        if self == next {
            return true;
        }

        matches!(
            (self, next),
            (Self::Missing, Self::FastReady)
                | (
                    Self::FastReady,
                    Self::QualityPending | Self::QualityBlocked | Self::QualityFailed,
                )
                | (
                    Self::QualityPending,
                    Self::QualityReady
                        | Self::QualityStale
                        | Self::QualityBlocked
                        | Self::QualityFailed,
                )
                | (Self::QualityReady, Self::QualityStale)
                | (
                    Self::QualityStale,
                    Self::QualityPending | Self::QualityBlocked | Self::QualityFailed,
                )
                | (
                    Self::QualityBlocked | Self::QualityFailed,
                    Self::QualityPending
                )
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Language {
    CSharp,
    JavaScript,
    Rust,
    TypeScript,
}

impl Language {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CSharp => "csharp",
            Self::JavaScript => "javascript",
            Self::Rust => "rust",
            Self::TypeScript => "typescript",
        }
    }

    pub fn from_extension(extension: &str) -> Option<Self> {
        match extension {
            "cs" => Some(Self::CSharp),
            "js" | "jsx" | "mjs" | "cjs" => Some(Self::JavaScript),
            "rs" => Some(Self::Rust),
            "ts" | "tsx" | "mts" | "cts" => Some(Self::TypeScript),
            _ => None,
        }
    }

    pub fn parser_version(self) -> &'static str {
        match self {
            Self::CSharp => crate::CSHARP_PARSER_VERSION,
            Self::JavaScript => crate::JAVASCRIPT_PARSER_VERSION,
            Self::Rust => crate::RUST_PARSER_VERSION,
            Self::TypeScript => crate::TYPESCRIPT_PARSER_VERSION,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChunkKind {
    Function,
    Method,
    ImplSummary,
    TypeDefinition,
    Module,
    FileFallback,
}

impl ChunkKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Function => "function",
            Self::Method => "method",
            Self::ImplSummary => "impl_summary",
            Self::TypeDefinition => "type_definition",
            Self::Module => "module",
            Self::FileFallback => "file_fallback",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ByteRange {
    pub start: usize,
    pub end: usize,
}

impl ByteRange {
    pub fn new(start: usize, end: usize) -> Self {
        Self { start, end }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LineRange {
    pub start: usize,
    pub end: usize,
}

impl LineRange {
    pub fn new(start: usize, end: usize) -> Self {
        Self { start, end }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileFacts {
    pub id: String,
    pub relative_path: String,
    pub language: Language,
    pub content_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeChunk {
    pub id: String,
    pub file_id: String,
    pub relative_path: String,
    pub symbol_id: Option<String>,
    pub symbol_name: Option<String>,
    pub kind: ChunkKind,
    pub byte_range: ByteRange,
    pub line_range: LineRange,
    pub text_hash: String,
    pub excluded_reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymbolKind {
    Function,
    Method,
}

impl SymbolKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Function => "function",
            Self::Method => "method",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Symbol {
    pub id: String,
    pub file_id: String,
    pub parent_symbol_id: Option<String>,
    pub name: String,
    pub qualified_name: String,
    pub kind: SymbolKind,
    pub signature: Option<String>,
    pub byte_range: ByteRange,
    pub line_range: LineRange,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolutionStatus {
    ResolvedExact,
    ResolvedLocalCandidate,
    Unresolved,
    Ambiguous,
}

impl ResolutionStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ResolvedExact => "resolved_exact",
            Self::ResolvedLocalCandidate => "resolved_local_candidate",
            Self::Unresolved => "unresolved",
            Self::Ambiguous => "ambiguous",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct CallEdge {
    pub id: String,
    pub caller_symbol_id: String,
    pub callee_text: String,
    pub callee_symbol_id: Option<String>,
    pub call_line: usize,
    pub confidence: f32,
    pub resolution_status: ResolutionStatus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseDiagnostic {
    pub byte_range: ByteRange,
    pub line_range: LineRange,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredTest {
    pub id: String,
    pub file_id: String,
    pub relative_path: String,
    pub symbol_id: Option<String>,
    pub name: String,
    pub qualified_name: String,
    pub framework: String,
    pub language: Language,
    pub byte_range: ByteRange,
    pub line_range: LineRange,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SourceFileIndex {
    pub chunks: Vec<CodeChunk>,
    pub symbols: Vec<Symbol>,
    pub calls: Vec<CallEdge>,
    pub parse_diagnostics: Vec<ParseDiagnostic>,
    pub tests: Vec<DiscoveredTest>,
}

pub type RustFileIndex = SourceFileIndex;

#[cfg(test)]
mod tests {
    use crate::{SemanticLayer, SemanticLayerMode, SemanticLayerStatus};

    #[test]
    fn semantic_layer_names_match_storage_vocabulary() {
        assert_eq!(SemanticLayer::Fast.as_str(), "fast");
        assert_eq!(SemanticLayer::Quality.as_str(), "quality");
    }

    #[test]
    fn semantic_layer_mode_names_match_cli_vocabulary() {
        assert_eq!(SemanticLayerMode::Auto.as_str(), "auto");
        assert_eq!(SemanticLayerMode::Fast.as_str(), "fast");
        assert_eq!(SemanticLayerMode::Quality.as_str(), "quality");
    }

    #[test]
    fn semantic_layer_status_names_match_design_lifecycle() {
        assert_eq!(SemanticLayerStatus::Missing.as_str(), "missing");
        assert_eq!(SemanticLayerStatus::FastReady.as_str(), "fast_ready");
        assert_eq!(
            SemanticLayerStatus::QualityPending.as_str(),
            "quality_pending"
        );
        assert_eq!(SemanticLayerStatus::QualityReady.as_str(), "quality_ready");
        assert_eq!(SemanticLayerStatus::QualityStale.as_str(), "quality_stale");
        assert_eq!(
            SemanticLayerStatus::QualityBlocked.as_str(),
            "quality_blocked"
        );
        assert_eq!(
            SemanticLayerStatus::QualityFailed.as_str(),
            "quality_failed"
        );
    }

    #[test]
    fn semantic_layer_status_maps_to_default_search_layer() {
        assert_eq!(SemanticLayerStatus::Missing.default_search_layer(), None);
        assert_eq!(
            SemanticLayerStatus::QualityReady.default_search_layer(),
            Some(SemanticLayer::Quality)
        );

        for status in [
            SemanticLayerStatus::FastReady,
            SemanticLayerStatus::QualityPending,
            SemanticLayerStatus::QualityStale,
            SemanticLayerStatus::QualityBlocked,
            SemanticLayerStatus::QualityFailed,
        ] {
            assert_eq!(status.default_search_layer(), Some(SemanticLayer::Fast));
        }
    }

    #[test]
    fn semantic_layer_status_reports_current_quality_only_when_ready() {
        assert!(SemanticLayerStatus::QualityReady.quality_is_current());

        for status in [
            SemanticLayerStatus::Missing,
            SemanticLayerStatus::FastReady,
            SemanticLayerStatus::QualityPending,
            SemanticLayerStatus::QualityStale,
            SemanticLayerStatus::QualityBlocked,
            SemanticLayerStatus::QualityFailed,
        ] {
            assert!(!status.quality_is_current());
        }
    }

    #[test]
    fn semantic_layer_status_allows_documented_lifecycle_transitions() {
        let lifecycle = [
            SemanticLayerStatus::Missing,
            SemanticLayerStatus::FastReady,
            SemanticLayerStatus::QualityPending,
            SemanticLayerStatus::QualityReady,
            SemanticLayerStatus::QualityStale,
            SemanticLayerStatus::QualityPending,
            SemanticLayerStatus::QualityReady,
        ];

        for pair in lifecycle.windows(2) {
            assert!(pair[0].can_transition_to(pair[1]));
        }
    }

    #[test]
    fn semantic_layer_status_allows_quality_failure_retry_transitions() {
        assert!(
            SemanticLayerStatus::FastReady.can_transition_to(SemanticLayerStatus::QualityBlocked)
        );
        assert!(
            SemanticLayerStatus::FastReady.can_transition_to(SemanticLayerStatus::QualityFailed)
        );
        assert!(
            SemanticLayerStatus::QualityPending
                .can_transition_to(SemanticLayerStatus::QualityBlocked)
        );
        assert!(
            SemanticLayerStatus::QualityPending
                .can_transition_to(SemanticLayerStatus::QualityFailed)
        );
        assert!(
            SemanticLayerStatus::QualityBlocked
                .can_transition_to(SemanticLayerStatus::QualityPending)
        );
        assert!(
            SemanticLayerStatus::QualityFailed
                .can_transition_to(SemanticLayerStatus::QualityPending)
        );
    }

    #[test]
    fn semantic_layer_status_rejects_skipping_fast_readiness() {
        assert!(!SemanticLayerStatus::Missing.can_transition_to(SemanticLayerStatus::QualityReady));
        assert!(
            !SemanticLayerStatus::QualityStale.can_transition_to(SemanticLayerStatus::QualityReady)
        );
    }
}

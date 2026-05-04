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

    pub fn parse(value: &str) -> Result<Self, String> {
        match value.trim() {
            "fast" => Ok(Self::Fast),
            "quality" => Ok(Self::Quality),
            other => Err(format!(
                "unsupported semantic layer `{other}`; expected `fast` or `quality`"
            )),
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

    pub fn parse(value: &str) -> Result<Self, String> {
        match value.trim() {
            "" | "auto" => Ok(Self::Auto),
            "fast" => Ok(Self::Fast),
            "quality" => Ok(Self::Quality),
            other => Err(format!(
                "unsupported semantic layer mode `{other}`; expected `auto`, `fast`, or `quality`"
            )),
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

    pub fn parse(value: &str) -> Result<Self, String> {
        match value.trim() {
            "missing" => Ok(Self::Missing),
            "fast_ready" => Ok(Self::FastReady),
            "quality_pending" => Ok(Self::QualityPending),
            "quality_ready" => Ok(Self::QualityReady),
            "quality_stale" => Ok(Self::QualityStale),
            "quality_blocked" => Ok(Self::QualityBlocked),
            "quality_failed" => Ok(Self::QualityFailed),
            other => Err(format!(
                "unsupported semantic layer status `{other}`; expected layered semantic status value"
            )),
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
    Json,
    JavaScript,
    Rust,
    Toml,
    TypeScript,
    Yaml,
}

impl Language {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CSharp => "csharp",
            Self::Json => "json",
            Self::JavaScript => "javascript",
            Self::Rust => "rust",
            Self::Toml => "toml",
            Self::TypeScript => "typescript",
            Self::Yaml => "yaml",
        }
    }

    pub fn from_extension(extension: &str) -> Option<Self> {
        match extension {
            "cs" => Some(Self::CSharp),
            "json" => Some(Self::Json),
            "js" | "jsx" | "mjs" | "cjs" => Some(Self::JavaScript),
            "rs" => Some(Self::Rust),
            "toml" => Some(Self::Toml),
            "ts" | "tsx" | "mts" | "cts" => Some(Self::TypeScript),
            "yaml" | "yml" => Some(Self::Yaml),
            _ => None,
        }
    }

    pub fn parser_version(self) -> &'static str {
        match self {
            Self::CSharp => crate::CSHARP_PARSER_VERSION,
            Self::Json => crate::JSON_CONFIG_PARSER_VERSION,
            Self::JavaScript => crate::JAVASCRIPT_PARSER_VERSION,
            Self::Rust => crate::RUST_PARSER_VERSION,
            Self::Toml => crate::TOML_CONFIG_PARSER_VERSION,
            Self::TypeScript => crate::TYPESCRIPT_PARSER_VERSION,
            Self::Yaml => crate::YAML_CONFIG_PARSER_VERSION,
        }
    }

    pub fn is_config(self) -> bool {
        matches!(self, Self::Json | Self::Toml | Self::Yaml)
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymbolReferenceKind {
    Import,
    TypeReference,
    Implementation,
    Attribute,
    Inheritance,
    Decorator,
    ConfigLink,
}

impl SymbolReferenceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Import => "import",
            Self::TypeReference => "type_reference",
            Self::Implementation => "implementation",
            Self::Attribute => "attribute",
            Self::Inheritance => "inheritance",
            Self::Decorator => "decorator",
            Self::ConfigLink => "config_link",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SymbolReference {
    pub id: String,
    pub file_id: String,
    pub source_symbol_id: Option<String>,
    pub target_symbol_id: Option<String>,
    pub reference_text: String,
    pub reference_kind: SymbolReferenceKind,
    pub line: usize,
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
    pub symbol_references: Vec<SymbolReference>,
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
    fn semantic_layer_parses_storage_vocabulary() {
        assert_eq!(SemanticLayer::parse("fast"), Ok(SemanticLayer::Fast));
        assert_eq!(SemanticLayer::parse("quality"), Ok(SemanticLayer::Quality));
        assert!(SemanticLayer::parse("auto").is_err());
    }

    #[test]
    fn semantic_layer_mode_names_match_cli_vocabulary() {
        assert_eq!(SemanticLayerMode::Auto.as_str(), "auto");
        assert_eq!(SemanticLayerMode::Fast.as_str(), "fast");
        assert_eq!(SemanticLayerMode::Quality.as_str(), "quality");
    }

    #[test]
    fn semantic_layer_mode_parses_cli_vocabulary() {
        assert_eq!(SemanticLayerMode::parse(""), Ok(SemanticLayerMode::Auto));
        assert_eq!(
            SemanticLayerMode::parse("auto"),
            Ok(SemanticLayerMode::Auto)
        );
        assert_eq!(
            SemanticLayerMode::parse("fast"),
            Ok(SemanticLayerMode::Fast)
        );
        assert_eq!(
            SemanticLayerMode::parse("quality"),
            Ok(SemanticLayerMode::Quality)
        );
        assert!(SemanticLayerMode::parse("slow").is_err());
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
    fn semantic_layer_status_parses_design_lifecycle() {
        assert_eq!(
            SemanticLayerStatus::parse("missing"),
            Ok(SemanticLayerStatus::Missing)
        );
        assert_eq!(
            SemanticLayerStatus::parse("fast_ready"),
            Ok(SemanticLayerStatus::FastReady)
        );
        assert_eq!(
            SemanticLayerStatus::parse("quality_pending"),
            Ok(SemanticLayerStatus::QualityPending)
        );
        assert_eq!(
            SemanticLayerStatus::parse("quality_ready"),
            Ok(SemanticLayerStatus::QualityReady)
        );
        assert_eq!(
            SemanticLayerStatus::parse("quality_stale"),
            Ok(SemanticLayerStatus::QualityStale)
        );
        assert_eq!(
            SemanticLayerStatus::parse("quality_blocked"),
            Ok(SemanticLayerStatus::QualityBlocked)
        );
        assert_eq!(
            SemanticLayerStatus::parse("quality_failed"),
            Ok(SemanticLayerStatus::QualityFailed)
        );
        assert!(SemanticLayerStatus::parse("current").is_err());
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

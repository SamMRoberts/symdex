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

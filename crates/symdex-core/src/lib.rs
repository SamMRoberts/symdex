//! Pure domain logic for repository indexing.

mod discovery;
mod error;
mod git;
mod hash;
mod model;
mod parser;
mod path;
mod secrets;

pub use discovery::{
    DiscoveredFile, DiscoveryOptions, discover_indexable_files, discover_rust_files,
};
pub use error::{CoreError, Result};
pub use git::{RepositoryRefKind, RepositoryRefSnapshot};
pub use hash::{content_hash, stable_id};
pub use model::{
    ByteRange, CallEdge, ChunkKind, CodeChunk, DiscoveredTest, FileFacts, Language, LineRange,
    ParseDiagnostic, ResolutionStatus, RustFileIndex, SemanticLayer, SemanticLayerMode,
    SemanticLayerStatus, SourceFileIndex, Symbol, SymbolKind,
};
pub use parser::{extract_chunks, extract_rust_chunks, index_rust_file, index_source_file};
pub use path::{NormalizedRepoPath, RepoRoot};
pub use secrets::secret_exclusion_reason;

pub const EVIDENCE_CONTRACT_SCHEMA: &str = "symdex.mcp.evidence.v1";
pub const EVIDENCE_CONTRACT_VERSION: u64 = 1;
pub const CSHARP_PARSER_VERSION: &str = "tree-sitter-c-sharp:0.23.5;symdex-core:0.1.2";
pub const JAVASCRIPT_PARSER_VERSION: &str = "tree-sitter-javascript:0.25.0;symdex-core:0.1.2";
pub const RUST_PARSER_VERSION: &str = "tree-sitter-rust:0.24.2;symdex-core:0.1.2";
pub const TYPESCRIPT_PARSER_VERSION: &str = "tree-sitter-typescript:0.23.2;symdex-core:0.1.2";

//! Pure domain logic for repository indexing.

mod discovery;
mod error;
mod hash;
mod model;
mod parser;
mod path;
mod secrets;

pub use discovery::{DiscoveredFile, DiscoveryOptions, discover_rust_files};
pub use error::{CoreError, Result};
pub use hash::{content_hash, stable_id};
pub use model::{
    ByteRange, CallEdge, ChunkKind, CodeChunk, FileFacts, Language, LineRange, ResolutionStatus,
    RustFileIndex, Symbol, SymbolKind,
};
pub use parser::{extract_rust_chunks, index_rust_file};
pub use path::{NormalizedRepoPath, RepoRoot};
pub use secrets::secret_exclusion_reason;

pub const EVIDENCE_CONTRACT_SCHEMA: &str = "symdex.mcp.evidence.v1";
pub const EVIDENCE_CONTRACT_VERSION: u64 = 1;
pub const RUST_PARSER_VERSION: &str = "tree-sitter-rust:0.24.2;symdex-core:0.1.0";

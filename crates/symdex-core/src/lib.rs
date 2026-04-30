//! Pure domain logic for repository indexing.

mod discovery;
mod error;
mod hash;
mod model;
mod parser;
mod path;

pub use discovery::{DiscoveredFile, DiscoveryOptions, discover_rust_files};
pub use error::{CoreError, Result};
pub use hash::{content_hash, stable_id};
pub use model::{
    ByteRange, CallEdge, ChunkKind, CodeChunk, FileFacts, Language, LineRange, ResolutionStatus,
    RustFileIndex, Symbol, SymbolKind,
};
pub use parser::{extract_rust_chunks, index_rust_file};
pub use path::{NormalizedRepoPath, RepoRoot};

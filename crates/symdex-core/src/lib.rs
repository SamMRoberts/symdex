//! Pure domain logic for repository indexing.

mod discovery;
mod error;
mod hash;
mod model;
mod path;

pub use discovery::{DiscoveredFile, DiscoveryOptions, discover_rust_files};
pub use error::{CoreError, Result};
pub use hash::{content_hash, stable_id};
pub use model::{ByteRange, ChunkKind, FileFacts, Language, LineRange};
pub use path::{NormalizedRepoPath, RepoRoot};

# symdex

symdex is a local-first codebase intelligence system for AI coding agents.

Current implementation status: Rust workspace scaffold with core repository
discovery, deterministic hashing, path normalization, tree-sitter Rust function
chunking, and an initial CLI.

## Try it

```bash
cargo run -p symdex-cli -- doctor
cargo run -p symdex-cli -- init
cargo run -p symdex-cli -- index tests/fixtures/rust_basic
```

The current `index` command performs offline Rust file discovery, extracts
function and method chunks, and prints deterministic file and chunk facts.
Embeddings, SQLite persistence, Qdrant storage, and the MCP server are
represented by crate boundaries and will be implemented in later slices.

# symdex

symdex is a local-first codebase intelligence system for AI coding agents.

Current implementation status: Rust workspace scaffold with core repository
discovery, deterministic hashing, path normalization, tree-sitter Rust function
chunking, local Ollama embedding client, Qdrant collection creation, and an
initial CLI.

## Try it

```bash
cargo run -p symdex-cli -- doctor
cargo run -p symdex-cli -- init
cargo run -p symdex-cli -- index tests/fixtures/rust_basic
```

The current `index` command performs offline Rust file discovery, extracts
function and method chunks, and prints deterministic file and chunk facts.
SQLite persistence, vector upserts/search, and the MCP server remain later
slices.

# symdex

symdex is a local-first codebase intelligence system for AI coding agents.

Current implementation status: Rust workspace scaffold with core repository
discovery, deterministic hashing, path normalization, tree-sitter Rust function
chunking, local Ollama embedding client, Qdrant collection creation, semantic
and structural CLI queries, compact context packs, and a read-only MCP stdio
server.

## Try it

```bash
cargo run -p symdex-cli -- doctor
cargo run -p symdex-cli -- init
cargo run -p symdex-cli -- index --offline tests/fixtures/rust_basic
cargo run -p symdex-cli -- index-status tests/fixtures/rust_basic
cargo run -p symdex-cli -- symbol tests/fixtures/rust_basic add
cargo run -p symdex-cli -- context-pack tests/fixtures/rust_basic add
cargo run -p symdex-cli -- serve-mcp
```

With Ollama and Qdrant running locally, `index <repo>` embeds Rust chunks and
upserts vectors, and `search <repo> <query>` returns ranked path and line-range
evidence. SQLite persistence stores repository, file, and chunk facts locally.
SQLite also stores basic Rust symbols and conservative direct call edges for
`symbol`, `callers`, `callees`, `impact`, and `context-pack`. `serve-mcp`
exposes the same read-only evidence surface to coding agents through MCP tools.

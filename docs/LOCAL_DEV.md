# Local Development

## Required services

- Rust toolchain
- SQLite available through Rust crate bindings
- Qdrant running locally
- Ollama running locally
- `nomic-embed-text` pulled into Ollama

## Local setup commands

```bash
ollama pull nomic-embed-text
docker pull qdrant/qdrant
docker run -p 6333:6333 -p 6334:6334 \
  -v "$(pwd)/qdrant_storage:/qdrant/storage:z" \
  qdrant/qdrant
```

## Environment variables

```bash
symdex_DB_PATH=.symdex/symdex.sqlite
symdex_QDRANT_URL=http://localhost:6334
symdex_OLLAMA_URL=http://localhost:11434
symdex_EMBED_MODEL=nomic-embed-text
```

## Expected commands

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo run -p symdex-cli -- init
cargo run -p symdex-cli -- doctor
cargo run -p symdex-cli -- index .
cargo run -p symdex-cli -- search . "retry logic"
cargo run -p symdex-cli -- serve-mcp
```

Implemented CLI commands currently include:

- `init`: creates the local state directory for the configured SQLite path.
- `doctor`: prints local configuration and basic filesystem diagnostics.
- `index <repo>`: discovers Rust files, applies built-in excludes and scoped
  simple `.gitignore` rules, hashes file contents, extracts tree-sitter
  function and method chunks, and prints deterministic file and chunk facts.
- `serve-mcp`: previews the planned read-only tool names while the MCP server is
  still pending.

The service-dependent `doctor` checks for Qdrant, Ollama, model availability,
and vector dimensions are not wired yet.

## Local-only rule

The app should not require network access beyond local loopback services during normal indexing and querying.

## Diagnostics

`symdex doctor` should check:

- SQLite database path is writable
- Qdrant is reachable
- Ollama is reachable
- embedding model is available
- vector dimension can be determined
- configured repo root exists

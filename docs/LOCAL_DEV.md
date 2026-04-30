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
cargo run -p symdex-cli -- doctor
cargo run -p symdex-cli -- index .
cargo run -p symdex-cli -- search . "retry logic"
cargo run -p symdex-cli -- serve-mcp
```

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

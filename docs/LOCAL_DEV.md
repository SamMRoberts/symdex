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
SYMDEX_DB_PATH=.symdex/symdex.sqlite
SYMDEX_QDRANT_URL=http://localhost:6333
SYMDEX_OLLAMA_URL=http://localhost:11434
SYMDEX_EMBED_MODEL=nomic-embed-text
```

## Expected commands

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo run -p symdex-cli -- init
cargo run -p symdex-cli -- doctor
cargo run -p symdex-cli -- index .
cargo run -p symdex-cli -- index --offline .
cargo run -p symdex-cli -- index-status .
cargo run -p symdex-cli -- search . "retry logic"
cargo run -p symdex-cli -- serve-mcp
```

Implemented CLI commands currently include:

- `init`: creates the local state directory for the configured SQLite path.
- `doctor`: prints local configuration and basic filesystem diagnostics.
- `index <repo>`: discovers Rust files, applies built-in excludes and scoped
  simple `.gitignore` rules, hashes file contents, extracts tree-sitter
  function and method chunks, embeds chunk text with local Ollama, creates the
  Qdrant collection if needed, and upserts semantic vectors. Use
  `index --offline <repo>` for SQLite-backed discovery and chunking without
  service calls; unchanged files are skipped by content hash.
- `index-status <repo>`: reports SQLite file and chunk counts for the repository.
- `search <repo> <query>`: embeds the query locally and returns ranked Qdrant
  matches with scores, paths, line ranges, and symbol names.
- `serve-mcp`: previews the planned read-only tool names while the MCP server is
  still pending.

`doctor` checks whether Qdrant is reachable over REST, whether Ollama is
reachable, whether the configured embedding model is present, and whether vector
dimension probing succeeds. These checks report diagnostic status and do not
mutate repository data.

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

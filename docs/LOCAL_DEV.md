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
cargo run -p symdex-cli -- symbol . "my_symbol"
cargo run -p symdex-cli -- callers . "my_symbol"
cargo run -p symdex-cli -- callees . "my_symbol"
cargo run -p symdex-cli -- impact . "my_symbol"
cargo run -p symdex-cli -- context-pack . "my_symbol"
cargo run -p symdex-cli -- search . "retry logic"
cargo run -p symdex-cli -- tui .
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
  service calls; unchanged files are skipped by content hash. Chunks flagged as
  likely sensitive are counted as `chunks_excluded_from_embedding`, persisted as
  metadata, and omitted from Ollama/Qdrant embedding.
- `index-status <repo>`: reports SQLite file and chunk counts for the repository.
  When a semantic index has completed, it also reports the latest embedding
  model and vector dimension recorded for that repository.
- `symbol <repo> <query>`: searches local SQLite symbols by name or qualified
  name and returns path and line ranges.
- `callers <repo> <symbol>` / `callees <repo> <symbol>`: returns direct
  call relationships from the local SQLite index.
- `impact <repo> <symbol>`: prints direct callers and direct callees as a basic
  impact view.
- `context-pack <repo> <symbol>`: prints compact JSON evidence for editing
  context. The current format is `symdex.context_pack.v1` and includes focus
  symbols, direct callers, direct callees, involved files, section limits, and
  notes. It does not include source text.
- `search <repo> <query>`: embeds the query locally and returns ranked Qdrant
  matches with scores, paths, line ranges, and symbol names.
- `tui [repo]`: launches the local terminal UI control panel. The current TUI
  opens a repository/status dashboard backed by SQLite metadata and local
  service configuration. Use `o` to confirm offline indexing, `s` to confirm
  semantic indexing, `x` for the storage explorer, `d` to run doctor
  diagnostics, `i` for indexing controls, `w` for the query workbench, `g` for
  the symbol/call graph browser, and `p` for the impact/context-pack viewer,
  `Tab` / `Shift+Tab` to toggle view-local modes. The storage explorer always
  shows its own nested tab header for storage overview/index coverage/symbol
  outline/call resolution/embedding coverage/index runs timeline/semantic
  neighborhood/cross-store health. Use `r` to refresh repository/storage status,
  and `q` or `Esc` to quit.
- `serve-mcp`: runs the read-only MCP server over stdio. The server exposes
  `symdex_search`, `symdex_find_symbol`, `symdex_callers`, `symdex_callees`,
  `symdex_impact`, `symdex_context_pack`, and `symdex_index_status`.

`doctor` checks whether Qdrant is reachable over REST, whether Ollama is
reachable, whether the configured embedding model is present, and whether vector
dimension probing succeeds. These checks report diagnostic status and do not
mutate repository data.

The TUI should surface these same diagnostics. Semantic search and semantic
indexing views require local Ollama and Qdrant; status, structural queries, and
offline indexing should remain usable without those services.

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

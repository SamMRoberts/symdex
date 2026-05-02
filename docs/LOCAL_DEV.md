# Local Development

## Required services

- Rust toolchain
- SQLite available through Rust crate bindings
- `cargo-audit` installed for local dependency audits
- Qdrant running locally
- Ollama running locally
- `nomic-embed-text` pulled into Ollama

## Local setup commands

```bash
cargo install cargo-audit --locked
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
SYMDEX_FAST_EMBED_MODEL=nomic-embed-text
SYMDEX_QUALITY_EMBED_MODEL=nomic-embed-text-v2-moe
SYMDEX_QUALITY_INDEX=1
SYMDEX_QUALITY_BATCH_SIZE=16
SYMDEX_QUALITY_WORKERS=1
SYMDEX_EMBED_TRUNCATE=true
SYMDEX_EMBED_BATCH_SIZE=16
SYMDEX_EMBED_MAX_CHUNK_BYTES=32768
SYMDEX_RUST_ANALYZER=0
SYMDEX_RUST_ANALYZER_CMD=rust-analyzer
```

`SYMDEX_RUST_ANALYZER=1` enables an optional `symdex doctor` readiness check for
the configured rust-analyzer binary. The check runs `rust-analyzer --version`
only. Indexing uses the same opt-in flag to report a metadata-only enrichment
plan for changed Rust files, but does not run rust-analyzer project analysis by
default.

`SYMDEX_EMBED_TRUNCATE` defaults to `true`, matching Ollama's embedding API
behavior for oversized local inputs. Set it to `false` only when you want
semantic indexing to fail instead of truncating chunks that exceed the embedding
model context window.

`SYMDEX_EMBED_BATCH_SIZE` defaults to `16`. Symdex splits semantic indexing
requests into batches before calling Ollama `/api/embed`, which avoids oversized
request payloads while preserving result order.

Layered semantic indexing helpers also recognize `SYMDEX_FAST_EMBED_MODEL`,
`SYMDEX_QUALITY_EMBED_MODEL`, `SYMDEX_QUALITY_INDEX`,
`SYMDEX_QUALITY_BATCH_SIZE`, and `SYMDEX_QUALITY_WORKERS`. The fast model
defaults to `nomic-embed-text`; the quality model defaults to
`nomic-embed-text-v2-moe`. `SYMDEX_EMBED_MODEL` remains the compatibility
setting for the current single-model path and is used as the fast-model fallback
when `SYMDEX_FAST_EMBED_MODEL` is unset. `symdex index <repo>` queues quality
jobs when quality indexing is enabled and the quality model is available.
`symdex index-quality <repo>` manually drains those queued jobs in bounded
batches, then reports whether SQLite activation made quality the active layer or
kept default search on fast. `symdex index --watch <repo>` also performs
cooperative quality catch-up in semantic watch mode when quality indexing is
enabled, processing bounded quality batches during post-batch and idle watch
ticks.

`SYMDEX_EMBED_MAX_CHUNK_BYTES` defaults to `32768`. Chunks larger than this are
persisted as metadata-only structural evidence with
`chunk_too_large_for_embedding` and are not sent to Ollama.

## Expected commands

```bash
cargo fmt --all
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build --workspace --release
cargo audit
cargo run -p symdex-cli -- init
cargo run -p symdex-cli -- doctor
cargo run -p symdex-cli -- doctor .
cargo run -p symdex-cli -- index .
cargo run -p symdex-cli -- index-quality .
cargo run -p symdex-cli -- index --offline .
cargo run -p symdex-cli -- index --watch .
cargo run -p symdex-cli -- index-status .
cargo run -p symdex-cli -- staleness .
cargo run -p symdex-cli -- symbol . "my_symbol"
cargo run -p symdex-cli -- callers . "my_symbol"
cargo run -p symdex-cli -- callees . "my_symbol"
cargo run -p symdex-cli -- call-path . "source_symbol" "target_symbol" 4
cargo run -p symdex-cli -- impact . "my_symbol"
cargo run -p symdex-cli -- context-pack . "my_symbol"
cargo run -p symdex-cli -- context-pack . "my_symbol" --mode unified
cargo run -p symdex-cli -- debug-context . panic.log
cargo run -p symdex-cli -- search . "retry logic"
cargo run -p symdex-cli -- tui .
cargo run -p symdex-cli -- serve-mcp
```

Implemented CLI commands currently include:

Use top-level `--json` or `--output json` with read-only MCP-backed evidence
commands to print the same `symdex.mcp.evidence.v1` envelope used by MCP
`structuredContent`. JSON mode is currently supported for `index-status`,
`search`, `symbol`, `callers`, `callees`, `call-path`, `impact`,
`context-pack`, and `debug-context`.

- `init`: creates the local state directory for the configured SQLite path.
- `doctor [repo]`: prints local configuration, filesystem diagnostics, local
  service checks, optional rust-analyzer enrichment readiness when explicitly
  enabled, the active MCP evidence contract version, and repo-specific index
  freshness/provenance readiness when a repo path is provided.
- `index <repo>`: discovers eligible Rust, C#, JavaScript, and TypeScript files,
  applies built-in excludes and scoped glob-aware `.gitignore` rules with
  negation, hashes file contents, extracts tree-sitter function and method chunks where supported,
  embeds chunk text with local Ollama, creates the Qdrant collection if needed,
  and upserts semantic vectors. Use
  `index --offline <repo>` for SQLite-backed discovery and chunking without
  service calls; unchanged files are skipped by content hash. Chunks flagged as
  likely sensitive are counted as `chunks_excluded_from_embedding`, persisted as
  metadata, and omitted from Ollama/Qdrant embedding.
  When `SYMDEX_RUST_ANALYZER=1` is set, index output also reports optional
  rust-analyzer enrichment readiness and eligible Rust file, symbol, and call
  counts without applying rust-analyzer facts.
- `index --watch <repo>`: starts continuous indexing. It polls local eligible
  Rust, C#, JavaScript, and TypeScript files, debounces event bursts, detects
  created/modified/deleted paths by content-hash snapshots, and reindexes
  changed content through the incremental indexing path until stopped with
  `Ctrl+C`. Semantic watch batches queue quality jobs when quality indexing is
  enabled, then process bounded quality catch-up batches between fast watch
  work. Watch output includes metadata-only quality state, progress,
  completion, and failure events.
- `index-quality <repo>`: manually processes queued quality semantic embedding
  jobs for the latest generation. It claims bounded batches, re-reads files
  from disk, verifies file and chunk hashes, writes quality Qdrant points and
  quality `chunk_embeddings` rows, and reports succeeded, failed, and stale
  counts along with `quality_status`, `active_layer`, and
  `activation_reason`. Fast search remains active for partial, stale, blocked,
  or failed quality state; complete current quality coverage switches default
  search to quality.
- `index-status <repo>`: reports SQLite file and chunk counts for the repository.
  When a semantic index has completed, it also reports the latest embedding
  model and vector dimension recorded for that repository.
- `staleness <repo> [symbol]`: compares indexed file content hashes against the
  current eligible files for implemented languages and reports fresh, stale,
  deleted, missing, and unknown evidence states. With a symbol query, the report
  is scoped to files involved in the matching symbols and compact context pack.
- `qdrant-verify <repo>`: compares SQLite vector-backed chunk metadata against
  live Qdrant point payloads for the configured embedding model. It reports
  missing collections, missing points, stale payload metadata, and orphaned
  points without printing source text or vectors.
- `qdrant-repair <repo>`: runs the same verification first, deletes orphaned
  Qdrant points, then runs semantic indexing when missing collections, missing
  points, stale payload fields, model drift, or dimension drift require vectors
  to be rebuilt. It finishes with a second verification report.
- `symbol <repo> <query>`: searches local SQLite symbols by name or qualified
  name and returns path, line ranges, and provenance metadata.
- `callers <repo> <symbol>` / `callees <repo> <symbol>`: returns direct
  call relationships from the local SQLite index.
- `call-path <repo> <source> <target> [depth]`: traces deterministic bounded
  paths over persisted call edges from a source symbol to a target symbol. The
  depth is clamped to 1-8 hops, cycles are not followed, unresolved terminal
  edges can match the target by callee text, and output stays metadata-only with
  path, line, confidence, resolution, and provenance fields.
- `impact <repo> <symbol>`: prints direct callers, direct callees, bounded
  transitive caller/callee paths, related files, provenance, and staleness
  labels. Evidence rows include trust scores derived from freshness,
  provenance completeness, confidence, and index metadata completeness, plus
  compact reason tags explaining why each evidence row was returned. Likely tests
  list indexed tests that directly call the queried symbol when discovered test
  metadata and resolved call evidence are present.
- `context-pack <repo> <symbol> [--mode structural|unified]`: prints compact
  JSON evidence for editing context. The default structural mode preserves
  `symdex.context_pack.v1` and includes focus symbols, direct callers, direct
  callees, involved files, section limits, and notes. Unified mode returns
  `symdex.context_pack.v2`, runs structural retrieval and semantic search in one
  query path, deduplicates symbol/chunk/file evidence, and labels rows with
  `evidence_source` values of `structural`, `semantic`, or `both`. It does not
  include source text. If local semantic services are unavailable, unified mode
  returns structural evidence with a compact `semantic_unavailable:*` note.
- `debug-context <repo> <runtime-input|file|->`: parses runtime failure input
  such as stack traces, panic locations, failing test names, frame symbols, and
  indexed-language file paths, then prints `symdex.debug_context.v1` JSON. Rust
  parsing covers common `cargo test`, panic-hook, `anyhow`, `tracing`, full
  backtrace, and async stack-like output shapes. The pack maps frames to indexed
  files, symbols, calls at the failing line, freshness, and provenance when
  available. Passing `-` reads from stdin; a single existing path reads that
  file; otherwise remaining arguments are treated as inline runtime text. Frame
  matches include trust scores and reason tags, and the pack does not include
  source text.
- `search <repo> <query>`: embeds the query locally and returns ranked Qdrant
  matches with scores, paths, line ranges, symbol names, and provenance
  metadata. Text output also prints compact reason tags for each match.
- `tui [repo]`: launches the local terminal UI control panel. The current TUI
  opens an Overview tab backed by SQLite metadata and local service
  configuration, then uses a compact repository summary beside or above the
  active workflow on other tabs. Use `o` to confirm offline indexing, `s` to
  confirm semantic indexing, `c` to toggle continuous indexing, `[` / `]` to
  move between the Overview, Index, Storage, Doctor, Query, Calls, and Impact
  tabs, and `Tab` / `Shift+Tab` to toggle view-local modes including impact,
  call-path, context-pack, and debug-context evidence modes. In the Doctor tab,
  `Enter` starts diagnostics when no result rows are available. The storage
  explorer always shows its own nested tab header for storage overview/index
  coverage/symbol outline/call resolution/embedding coverage/index runs
  timeline/evidence freshness/semantic neighborhood/cross-store health. Use `r`
  to refresh repository/storage status, and `q` or `Esc` to quit.
- `serve-mcp`: runs the read-only MCP server over stdio. The server exposes
  `symdex_search`, `symdex_find_symbol`, `symdex_callers`, `symdex_callees`,
  `symdex_call_path`, `symdex_impact`, `symdex_context_pack`, and
  `symdex_debug_context`, `symdex_staleness_check`, and
  `symdex_index_status`. `symdex_context_pack` accepts `mode: "unified"` for
  combined structural and semantic context-pack evidence.

`doctor` checks whether Qdrant is reachable over REST, whether Ollama is
reachable, whether the configured embedding model is present, and whether vector
dimension probing succeeds. These checks report diagnostic status and do not
mutate repository data.

## CI checks

GitHub Actions runs the production hardening baseline on pull requests and on
pushes to `main`:

- `cargo fmt --all --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test --workspace`
- `cargo build --workspace --release`
- `cargo audit`

The TUI should surface these same diagnostics. Semantic search and semantic
indexing views require local Ollama and Qdrant; status, structural queries, and
offline indexing should remain usable without those services.

## Local-only rule

The app should not require network access beyond local loopback services during normal indexing and querying.

## Diagnostics

`symdex doctor` should check:

- SQLite database path is writable
- SQLite database file exists after `symdex init`
- Qdrant is reachable
- Ollama is reachable
- embedding model is available
- vector dimension can be determined
- active MCP evidence contract version is local-only and read-only
- optional rust-analyzer enrichment readiness when `SYMDEX_RUST_ANALYZER=1`
- configured repo root exists
- repo-specific index freshness and provenance consistency when a repo is passed

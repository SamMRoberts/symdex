# symdex

symdex is a local-first codebase intelligence system for AI coding agents. It
indexes repositories structurally and semantically so agents can reason from
compact, source-free evidence instead of guessing.

symdex stores structural facts in SQLite and vector projections in sqlite-vec in
the same local database. Ollama provides local embeddings. The CLI, TUI, and MCP
server all read the same local index.

## Current Status

Implemented today:

- Syntax-aware indexing for Rust, C#, JavaScript, and TypeScript.
- SQLite storage for repositories, files, chunks, symbols, calls, tests,
  provenance, semantic generations, and index runs.
- sqlite-vec vector storage for semantic search.
- Fast semantic indexing with `nomic-embed-text`.
- Deferred quality indexing with `mxbai-embed-large`.
- Structural queries: symbols, callers, callees, call paths, impact, staleness,
  context packs, and debug context packs.
- Semantic search with fast/quality layer routing and fallback metadata.
- Continuous indexing through a single background watcher per repository.
- Terminal UI built with `ratatui` and `crossterm`.
- MCP stdio server for local agents, with evidence tools read-only except the
  explicit local `symdex_watch_start` watcher-start tool.

## Quickstart

```bash
cargo run -p symdex-cli -- init
cargo run -p symdex-cli -- doctor .
cargo run -p symdex-cli -- index --offline tests/fixtures/rust_basic
cargo run -p symdex-cli -- index-status tests/fixtures/rust_basic
cargo run -p symdex-cli -- symbol tests/fixtures/rust_basic add
cargo run -p symdex-cli -- callers tests/fixtures/rust_basic add
cargo run -p symdex-cli -- impact tests/fixtures/rust_basic add
cargo run -p symdex-cli -- context-pack tests/fixtures/rust_basic add
cargo run -p symdex-cli -- staleness tests/fixtures/rust_basic
```

Offline indexing builds the SQLite structural index without Ollama. For semantic
indexing and search, start Ollama and install the embedding models:

```bash
ollama pull nomic-embed-text
ollama pull mxbai-embed-large
cargo run -p symdex-cli -- index .
cargo run -p symdex-cli -- index-quality .
cargo run -p symdex-cli -- search . "retry logic"
```

## Commands

```bash
cargo run -p symdex-cli -- init
cargo run -p symdex-cli -- doctor [repo]
cargo run -p symdex-cli -- index [--full|--incremental] [--offline] [--watch] <repo>
cargo run -p symdex-cli -- index-quality <repo>
cargo run -p symdex-cli -- index-status <repo>
cargo run -p symdex-cli -- semantic-status <repo>
cargo run -p symdex-cli -- staleness <repo> [symbol]
cargo run -p symdex-cli -- vector-verify <repo> [--semantic-layer fast|quality|all]
cargo run -p symdex-cli -- vector-repair <repo> [--semantic-layer fast|quality|all]
cargo run -p symdex-cli -- search <repo> "query"
cargo run -p symdex-cli -- symbol <repo> <query>
cargo run -p symdex-cli -- callers <repo> <symbol>
cargo run -p symdex-cli -- callees <repo> <symbol>
cargo run -p symdex-cli -- call-path <repo> <source> <target> [depth]
cargo run -p symdex-cli -- impact <repo> <symbol>
cargo run -p symdex-cli -- context-pack <repo> <symbol> [--mode structural|unified]
cargo run -p symdex-cli -- debug-context <repo> <runtime-input|file|->
cargo run -p symdex-cli -- watch start|status|stop <repo>
cargo run -p symdex-cli -- tui [repo]
cargo run -p symdex-cli -- serve-mcp [--watch <repo>]
```

Deprecated `qdrant-verify` and `qdrant-repair` aliases still route to the vector
maintenance commands for compatibility. Qdrant is no longer the vector backend.

## Indexing Model

Manual indexing has two modes:

- `index --offline <repo>` updates SQLite structural data only.
- `index <repo>` also embeds eligible chunks through local Ollama and writes
  sqlite-vec vector rows.

Index scope is explicit:

- `--incremental` skips unchanged files by content hash.
- `--full` reparses all eligible files.

Semantic indexing is layered:

- The fast layer uses `nomic-embed-text`, default max chunk size `2048` bytes.
- The quality layer uses `mxbai-embed-large`, default max chunk size `512` bytes.
- Default semantic search uses fast until quality is complete and current.
- If quality is missing, stale, partial, failed, or blocked, search falls back
  to fast and reports why.

Useful environment variables:

```bash
SYMDEX_DB_PATH=.symdex/symdex.sqlite
SYMDEX_OLLAMA_URL=http://localhost:11434
SYMDEX_FAST_EMBED_MODEL=nomic-embed-text
SYMDEX_QUALITY_EMBED_MODEL=mxbai-embed-large
SYMDEX_EMBED_MAX_CHUNK_BYTES=2048
SYMDEX_QUALITY_EMBED_MAX_CHUNK_BYTES=512
SYMDEX_QUALITY_INDEX=1
SYMDEX_DEBUG_DB_LOCKS=1
```

## Continuous Indexing

symdex enforces one watcher per repository. The watcher polls eligible files,
debounces changes, respects ignore/path-boundary rules, and runs incremental
indexing for changed content.

symdex also enforces a single writer service per configured SQLite database.
Manual indexing, quality catch-up, repair, migrations, watcher state updates,
and continuous indexing batches submit jobs to that service. The service owns
SQLite/sqlite-vec writes and keeps the sidecar OS lock only as its internal
duplicate-daemon guard. Read paths use read-only SQLite connections directly.
Set `SYMDEX_DEBUG_DB_LOCKS=1` to print stderr diagnostics for writer daemon
startup/attach, queued jobs, writer-gate wait and hold times, SQLite open modes,
and daemon-internal lease acquisition. `SYMDEX_DEBUG_WRITER=1` is accepted as an
alias.

Watcher lifetime is client-scoped:

- `symdex tui [repo]` starts or attaches the watcher and holds a lease while
  the TUI is open.
- `symdex serve-mcp --watch <repo>` starts or attaches the watcher and holds a
  lease while the MCP process is running.
- `symdex_watch_start` starts or attaches the watcher for the lifetime of that
  MCP server process.
- `symdex watch start <repo>` is attach-scoped; without another live TUI, MCP,
  or foreground client, the watcher exits after about 10 seconds.
- `symdex watch stop <repo>` explicitly asks the daemon to stop.

Use `symdex watch status <repo>` or `doctor <repo>` to inspect watcher state,
attached clients, shutdown grace, latest indexed path, and errors.

## TUI

```bash
cargo run -p symdex-cli -- tui .
```

The TUI is terminal-only and local-only. It shows repository status, indexing
controls, storage views, diagnostics, semantic search, structural queries, call
graphs, impact, context packs, debug context, freshness, provenance, and
sqlite-vec health.

Core keys:

- `[` / `]`: switch primary tabs.
- `Tab` / `Shift+Tab`: switch mode inside the active tab.
- `o`: confirm offline indexing.
- `s`: confirm semantic indexing.
- `c`: stop or restart the shared watcher.
- `r`: refresh status or run doctor from the Doctor tab.
- `q` / `Esc`: quit or back out of the current interaction.

The Index tab auto-refreshes shared watcher and semantic readiness state because
continuous indexing is owned by the background writer service, not the TUI
process itself.

## MCP

```bash
cargo run -p symdex-cli -- serve-mcp
cargo run -p symdex-cli -- serve-mcp --watch .
```

MCP tools:

- `symdex_search`
- `symdex_find_symbol`
- `symdex_callers`
- `symdex_callees`
- `symdex_call_path`
- `symdex_impact`
- `symdex_context_pack`
- `symdex_debug_context`
- `symdex_staleness_check`
- `symdex_index_status`
- `symdex_watch_status`
- `symdex_watch_start`

Successful tool responses use the stable `symdex.mcp.evidence.v1` envelope.
Evidence tools are read-only and metadata-first: paths, line ranges, scores,
freshness, provenance, active semantic layer, quality status, and compact
relationship evidence. They do not return full source files, vectors, or
embeddings. `symdex_watch_start` is the explicit local write-capable exception
for starting or attaching the scoped background watcher.

## Privacy And Safety

- All storage is local.
- SQLite and sqlite-vec live under the configured symdex state directory.
- Ollama calls go to the configured local Ollama URL.
- symdex does not add hosted services, telemetry, or remote embeddings.
- Indexed source is treated as sensitive and untrusted.
- Likely secret chunks are excluded from embeddings.
- Repository path boundaries are enforced; symlink escapes are rejected.
- TUI and MCP outputs are compact metadata by default, not source previews.

## Development

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets
cargo test --workspace
```

Docs live under [`docs/`](docs/). Start with [`docs/README.md`](docs/README.md)
for task-specific reading paths.

# Architecture

## Workspace layout

```text
crates/
  symdex-core/
  symdex-diagnostics/
  symdex-index/
  symdex-query/
  symdex-store/
  symdex-embed/
  symdex-cli/
  symdex-tui/
  symdex-mcp/
tests/
  fixtures/
docs/
```

## Crate responsibilities

### `symdex-core`

Pure domain logic:

- repository scanning
- ignore handling inputs
- file hashing
- tree-sitter parsing
- syntax-aware chunking
- symbol extraction
- call extraction
- language-agnostic parser/chunker interfaces with per-language tree-sitter
  adapters hidden behind core domain types
- indexing plans and domain types
- shared evidence contract schema/version constants used by CLI, TUI,
  diagnostics, and MCP

This crate should not depend on Qdrant, Ollama, MCP, or CLI frameworks.
Per-language parser details must not leak into CLI, TUI, MCP, store, Qdrant, or
Ollama layers. Those layers should consume stable language slugs, parser
versions, chunks, symbols, calls, and provenance metadata.

### `symdex-store`

Persistence adapters:

- SQLite schema migrations
- file/symbol/chunk/call persistence
- Qdrant collection management
- vector upserts and searches
- repository index metadata
- semantic generation state for layered fast/quality indexing
- per-layer embedding manifests
- deferred quality embedding job persistence

The crate keeps Qdrant REST client code and Qdrant request/response DTOs in
`src/qdrant.rs`, re-exporting only the stable adapter types and helpers through
`lib.rs`. SQLite schema, row mapping, and storage summaries remain in `lib.rs`
until they are split into dedicated modules.

Keep database DTOs separate from domain types.

For layered semantic indexing, this crate should treat SQLite as the source of
truth for active layer selection, quality readiness, generation IDs, and Qdrant
expected manifests. Qdrant remains a metadata-only projection of embeddable
chunks.

### `symdex-diagnostics`

Local diagnostics:

- current workspace and configured local service endpoints
- SQLite parent path checks
- Ollama model and embedding dimension checks
- Qdrant health checks
- optional rust-analyzer enrichment readiness checks when explicitly enabled
- fast and quality embedding model readiness when layered indexing is enabled

Do not mutate repository data. Keep diagnostics local and reusable by CLI and
TUI.

### `symdex-index`

Indexing orchestration:

- repository indexing workflow shared by CLI and TUI
- continuous indexing watch orchestration shared by CLI and TUI
- file-event debounce and coalescing before reindex work is scheduled
- structural SQLite persistence
- optional semantic embedding and Qdrant upserts
- compact indexing summaries without source text
- fast semantic generation creation using `nomic-embed-text`
- quality semantic job queueing and worker orchestration using
  `nomic-embed-text-v2-moe`

Call core, store, and embed APIs directly. Do not depend on CLI, TUI, or MCP.

Continuous indexing must keep the fast semantic layer synchronous and queue
quality work as deferred/background work. It must not block watch mode on the
quality model.

### `symdex-query`

Query orchestration:

- SQLite symbol search
- SQLite callers and callees
- impact summaries and context-pack retrieval
- semantic query embedding
- active semantic layer routing
- Qdrant vector search
- compact query result summaries without source text
- storage-visualization summaries that combine SQLite structural metadata with
  Qdrant semantic coverage metadata

Call core, store, and embed APIs directly. Do not depend on CLI, TUI, or MCP.

For layered semantic indexing, this crate must ask SQLite which semantic layer
is active before embedding the query. Default search uses the fast layer until
quality is complete and current. It must not use partial quality results unless
a future explicit diagnostic mode is designed.

### `symdex-embed`

Local embedding adapter:

- Ollama HTTP client
- `nomic-embed-text` model checks
- `nomic-embed-text-v2-moe` quality model checks when layered indexing is enabled
- batch embedding requests
- vector dimension discovery
- retry behavior for transient local service failures

The adapter should support explicit model selection by caller so fast and
quality layers can use different embedding models without relying on global
process state.

### `symdex-cli`

User-facing commands:

- argument parsing
- diagnostics
- progress output
- non-interactive continuous indexing launch path
- command presentation
- semantic layer selection flags and semantic-status output when layered
  indexing is implemented

Do not put core indexing logic here.

### `symdex-tui`

Terminal UI:

- app state and reducers
- `ratatui` layouts and widgets
- `crossterm` input and terminal lifecycle
- view orchestration for dashboard, indexing controls, diagnostics, queries, impact, and context packs
- continuous indexing toggle, confirmation state, watch status, and watch error display
- metadata-only storage visualizations for index coverage, file details, symbol outlines, call resolution, embedding coverage, and index runs
- active semantic layer and quality-index status display when layered indexing
  is enabled

The crate keeps terminal setup/help in `src/terminal.rs` and index-job screen
navigation reducers in `src/navigation.rs`. The main `lib.rs` owns app state,
event orchestration, and rendering until larger view modules are split out.

Do not shell out to the `symdex` binary. Call Rust library APIs directly.

### `symdex-mcp`

MCP server:

- tool definitions
- input validation
- output shaping
- path boundary enforcement
- read-only query operations
- stable cross-agent evidence contract envelope using the shared `symdex-core`
  contract constants
- semantic result metadata for active layer, model, generation, and quality
  fallback status when available

## Boundary rules

- Core emits facts; store persists facts; CLI, TUI, and MCP present facts.
- SQLite decides semantic-layer readiness; Qdrant does not decide routing.
- Fast semantic indexing is the availability path. Quality semantic indexing is
  deferred precision work.
- Never let MCP invoke indexing side effects until a write-capable design is approved.
- Require TUI confirmation before long-running jobs such as indexing.
- Continuous indexing is an ongoing local job; TUI and CLI entry points must use shared `symdex-index` APIs and must not spawn `symdex` subprocesses.
- Continuous indexing must not block on `nomic-embed-text-v2-moe`; it should
  update fast vectors and queue quality work.
- Keep TUI rendering and event types out of core, store, embed, and MCP crates.
- Keep storage visualization queries outside `symdex-tui` when they require
  nontrivial SQLite/Qdrant aggregation; expose typed summaries from shared
  library crates instead.
- Prefer stable serialized structs for MCP outputs.
- MCP successful tool calls must include the current cross-agent evidence
  contract metadata so multiple local agents can interpret the same index
  consistently.
- Keep all path normalization centralized.
- Do not expose absolute paths unless user configuration allows it.

## Layered semantic indexing reference

Use `docs/LAYERED_SEMANTIC_INDEXING.md` for the authoritative fast/quality
semantic indexing design and `docs/LAYERED_SEMANTIC_INDEXING_TASKS.md` for the
implementation slice order on the `quality-index` branch.

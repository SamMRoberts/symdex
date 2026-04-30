# Architecture

## Workspace layout

```text
crates/
  symdex-core/
  symdex-diagnostics/
  symdex-index/
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
- indexing plans and domain types

This crate should not depend on Qdrant, Ollama, MCP, or CLI frameworks.

### `symdex-store`

Persistence adapters:

- SQLite schema migrations
- file/symbol/chunk/call persistence
- Qdrant collection management
- vector upserts and searches
- repository index metadata

Keep database DTOs separate from domain types.

### `symdex-diagnostics`

Local diagnostics:

- current workspace and configured local service endpoints
- SQLite parent path checks
- Ollama model and embedding dimension checks
- Qdrant health checks

Do not mutate repository data. Keep diagnostics local and reusable by CLI and
TUI.

### `symdex-index`

Indexing orchestration:

- repository indexing workflow shared by CLI and TUI
- structural SQLite persistence
- optional semantic embedding and Qdrant upserts
- compact indexing summaries without source text

Call core, store, and embed APIs directly. Do not depend on CLI, TUI, or MCP.

### `symdex-embed`

Local embedding adapter:

- Ollama HTTP client
- `nomic-embed-text` model checks
- batch embedding requests
- vector dimension discovery
- retry behavior for transient local service failures

### `symdex-cli`

User-facing commands:

- argument parsing
- diagnostics
- progress output
- command presentation

Do not put core indexing logic here.

### `symdex-tui`

Terminal UI:

- app state and reducers
- `ratatui` layouts and widgets
- `crossterm` input and terminal lifecycle
- view orchestration for dashboard, indexing controls, diagnostics, queries, impact, and context packs

Do not shell out to the `symdex` binary. Call Rust library APIs directly.

### `symdex-mcp`

MCP server:

- tool definitions
- input validation
- output shaping
- path boundary enforcement
- read-only query operations

## Boundary rules

- Core emits facts; store persists facts; CLI, TUI, and MCP present facts.
- Never let MCP invoke indexing side effects until a write-capable design is approved.
- Require TUI confirmation before long-running jobs such as indexing.
- Keep TUI rendering and event types out of core, store, embed, and MCP crates.
- Prefer stable serialized structs for MCP outputs.
- Keep all path normalization centralized.
- Do not expose absolute paths unless user configuration allows it.

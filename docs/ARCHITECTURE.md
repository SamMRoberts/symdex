# Architecture

## Workspace layout

```text
crates/
  symdex-core/
  symdex-store/
  symdex-embed/
  symdex-cli/
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
- command orchestration

Do not put core indexing logic here.

### `symdex-mcp`

MCP server:

- tool definitions
- input validation
- output shaping
- path boundary enforcement
- read-only query operations

## Boundary rules

- Core emits facts; store persists facts; MCP presents facts.
- Never let MCP invoke indexing side effects until a write-capable design is approved.
- Prefer stable serialized structs for MCP outputs.
- Keep all path normalization centralized.
- Do not expose absolute paths unless user configuration allows it.

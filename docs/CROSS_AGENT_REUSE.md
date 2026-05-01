# Cross-Agent Reuse

Cross-agent reuse means multiple local coding agents can read the same symdex
index without each agent rebuilding or inventing its own evidence model.

## Contract

- The shared index is local SQLite plus local Qdrant.
- The supported agent-facing protocol is read-only MCP over stdio.
- Successful MCP tool results use the stable envelope
  `symdex.mcp.evidence.v1`.
- The envelope carries:
  - `schema_version`
  - `contract_version`
  - local-only/read-only/source-text policy
  - freshness/provenance availability
  - the actual tool payload under `data`
- Tool names use underscores and must remain stable unless a new contract
  version is introduced.

## Privacy Guarantees

- Agents receive metadata evidence, not source text by default.
- Repository roots are explicit inputs and must be valid directory roots.
- Paths are normalized relative to the repository root before use.
- MCP tools must not start indexing, continuous indexing, reset, delete, or
  mutate repository data.
- The local service choices remain under user control:
  - SQLite path from `SYMDEX_DB_PATH`
  - Qdrant URL from `SYMDEX_QDRANT_URL`
  - Ollama URL and model from embed configuration

## Read-Only Access Pattern

1. One user process indexes a repository with the CLI or TUI.
2. One or more local agents connect to `symdex serve-mcp`.
3. Agents call read-only tools with an explicit `repo` root.
4. Tool responses include compact evidence under `data` plus contract metadata.
5. Agents inspect `freshness` and `provenance` before trusting evidence.

The MCP server may read SQLite and Qdrant, embed semantic search queries through
local Ollama, and compute freshness from current file hashes. It must not
execute indexed repository code or expose source text by default.

## Diagnostics Expectations

Cross-agent diagnostics should make these facts visible:

- SQLite database path and parent directory health.
- Qdrant endpoint health.
- Ollama endpoint, model, and vector dimension health.
- Index freshness and provenance consistency.
- Whether an agent is receiving stale, missing, deleted, unknown, or fresh
  evidence.

## Future Work

- Add a CLI/TUI cross-agent readiness report that combines local diagnostics,
  index freshness, provenance coverage, and MCP contract version.
- Add tests that simulate multiple MCP clients reading the same indexed
  repository without write-capable tools.

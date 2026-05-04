# Cross-Agent Reuse

Cross-agent reuse means multiple local coding agents can read the same symdex
index without each agent rebuilding or inventing its own evidence model.

## Contract

- The shared index is local SQLite plus local sqlite-vec.
- Only ONE local process may write to that shared database for a repository at
  a time. Other local agents and clients must use read-only evidence paths or
  attach to the single writer instead of starting their own write loop.
- The supported agent-facing protocol is MCP over stdio. Evidence tools are
  read-only; `symdex_watch_start` is the explicit local-only watcher-start
  exception.
- Successful MCP tool results use the stable envelope
  `symdex.mcp.evidence.v1`.
- The envelope carries:
  - `schema_version`
  - `contract_version`
  - local-only/read-only/source-text policy for evidence tools
  - freshness/provenance availability
  - the actual tool payload under `data`
- Tool names use underscores and must remain stable unless a new contract
  version is introduced.
- The evidence contract schema and version live in `symdex-core` so CLI, TUI,
  MCP, and diagnostics can report the same current contract.

## Privacy Guarantees

- Agents receive metadata evidence, not source text by default.
- Repository roots are explicit inputs and must be valid directory roots.
- Paths are normalized relative to the repository root before use.
- MCP evidence tools must not start indexing, reset, delete, or mutate
  repository data. `symdex_watch_start` may start or attach the single local
  background watcher for an explicit repo.
- The local service choices remain under user control:
  - SQLite path from `SYMDEX_DB_PATH`
  - sqlite-vec URL from `SYMDEX_DB_PATH`
  - Ollama URL and model from embed configuration

## Read-Only Access Pattern

1. One user process owns database writes for the repository. In continuous mode
   this is the shared watcher started or attached by the TUI, an MCP server,
   foreground watch, or `symdex_watch_start`; in manual mode it is the single
   manual indexing, quality, repair, or cleanup command currently running.
2. One or more local agents connect to `symdex serve-mcp`.
3. Agents call evidence tools with an explicit `repo` root, and may call
   `symdex_watch_status` or `symdex_watch_start` to manage watcher readiness.
   Watcher leases are held by live TUI/MCP/CLI clients; when none remain the
   watcher exits after about 10 seconds.
4. If a write-capable operation is requested while another writer is active, it
   must attach to the active writer, queue or coalesce work, or fail closed with
   a clear owner/status message. It must not open a second SQLite/sqlite-vec
   writer.
5. Tool responses include compact evidence under `data` plus contract metadata.
6. Agents inspect `freshness` and `provenance` before trusting evidence.

The MCP server may read SQLite and sqlite-vec, embed semantic search queries through
local Ollama, and compute freshness from current file hashes. It must not
execute indexed repository code or expose source text by default.

## Diagnostics Expectations

`symdex doctor [repo]` makes these facts visible:

- SQLite database path and parent directory health.
- sqlite-vec extension health.
- Ollama endpoint, model, and vector dimension health.
- Index freshness and provenance consistency.
- Semantic quality-layer progress and fallback state, separate from file
  freshness.
- Whether an agent is receiving stale, missing, deleted, unknown, or fresh
  evidence.
- The active MCP evidence contract schema/version and local-only/read-only
  policy.

## Future Work

- Surface the full repo-aware cross-agent readiness report in the TUI Doctor
  tab.
- Add fixture-backed multi-agent tests with populated files, symbols, calls, and
  provenance rows.

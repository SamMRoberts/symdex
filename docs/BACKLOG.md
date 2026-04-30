# Backlog

## Phase 0 — Harness and docs

- Create repo-level `AGENTS.md`.
- Create docs index and project specs.
- Decide workspace crate layout.
- Add initial CLI command list.

## Phase 1 — Semantic MVP

- Rust workspace scaffold.
- `doctor` command.
- repo file discovery.
- `.gitignore` support.
- content hashing.
- tree-sitter Rust parsing.
- function-level chunk extraction.
- Ollama embedding client.
- Qdrant collection creation.
- semantic search CLI.

## Phase 2 — SQLite structural index

- migrations.
- repositories/files/chunks tables.
- stable IDs.
- incremental indexing.
- deleted file cleanup.
- index status command.

## Phase 3 — Symbols and calls

- symbol extraction.
- qualified names.
- call extraction.
- unresolved calls.
- direct callers/callees CLI.
- basic impact analysis.

## Phase 4 — MCP server

- `serve-mcp`.
- `symdex.search`.
- `symdex.find_symbol`.
- `symdex.callers`.
- `symdex.callees`.
- `symdex.impact`.
- contract tests.

## Phase 5 — Hardening

- secret detection.
- path boundary tests.
- model/dimension migration behavior.
- large repo performance pass.
- compact context pack format.

## Do not start yet

- web UI
- hosted mode
- multi-repo org search
- write tools
- agent code editing
- non-Rust languages

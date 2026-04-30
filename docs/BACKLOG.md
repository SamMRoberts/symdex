# Backlog

## Phase 0 — Harness and docs

- [x] Create repo-level `AGENTS.md`.
- [x] Create docs index and project specs.
- [x] Decide workspace crate layout.
- [x] Add initial CLI command list.

## Phase 1 — Semantic MVP

- [x] Rust workspace scaffold.
- [x] `doctor` command.
- [x] repo file discovery.
- [x] scoped `.gitignore` support for simple rules.
- [x] content hashing.
- [x] tree-sitter Rust parsing.
- [x] function-level chunk extraction.
- [x] Ollama embedding client.
- [x] Qdrant collection creation.
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

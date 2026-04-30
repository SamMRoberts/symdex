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
- [x] semantic search CLI.

## Phase 2 — SQLite structural index

- [x] migrations.
- [x] repositories/files/chunks tables.
- [x] stable IDs.
- [x] incremental structural indexing by content hash.
- [x] deleted file cleanup.
- [x] index status command.

## Phase 3 — Symbols and calls

- [x] symbol extraction.
- [x] qualified names.
- [x] call extraction.
- [x] unresolved calls.
- [x] direct callers/callees CLI.
- [x] basic impact analysis.

## Phase 4 — MCP server

- [x] `serve-mcp`.
- [x] `symdex.search`.
- [x] `symdex.find_symbol`.
- [x] `symdex.callers`.
- [x] `symdex.callees`.
- [x] `symdex.impact`.
- [x] `symdex.index_status`.
- [x] contract tests.

## Phase 5 — Hardening

- [x] secret detection.
- [x] path boundary tests.
- [x] model/dimension migration behavior.
- [x] large repo performance pass.
- [x] compact context pack format.

## Phase 6 — TUI

- [x] TUI workspace crate scaffold.
- [x] `symdex tui [repo]` launch command.
- [x] repository/status dashboard.
- [x] indexing controls with confirmation.
- doctor diagnostics view.
- query workbench for search and symbols.
- symbol/call graph browser.
- impact and context-pack viewer.
- [x] TUI state reducer tests.
- [x] `ratatui` render tests.
- [x] CLI launch smoke.

## Do not start yet

- web UI
- hosted mode
- multi-repo org search
- write tools
- agent code editing
- non-Rust languages

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
- [x] `symdex_search`.
- [x] `symdex_find_symbol`.
- [x] `symdex_callers`.
- [x] `symdex_callees`.
- [x] `symdex_impact`.
- [x] `symdex_index_status`.
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
- [x] doctor diagnostics view.
- [x] query workbench for search and symbols.
- [x] symbol/call graph browser.
- [x] impact and context-pack viewer.
- [x] TUI state reducer tests.
- [x] `ratatui` render tests.
- [x] CLI launch smoke.

## Phase 7 — TUI visual polish

- [x] ratatui `Tabs` for major views.
- [x] status-colored labels for service health and job state.
- [x] `Table` widgets for diagnostics, query evidence, call graph rows, impact rows, and context-pack metadata.
- [x] status-colored labels for confidence and resolution inside table rows.
- [x] stateful selection/focus for result lists and tables.
- [x] per-view footer help text.
- [x] confirmation panel styling for long-running jobs.
- [x] progress `Gauge` for indexing when progress reporting exists.
- [x] narrow-terminal render tests for 80x24 layout.
- [x] render tests for active tabs and status colors.
- [x] render tests for table headers.
- [x] render tests for selected rows.

## Phase 8 — Doctor diagnostics interaction

- [x] actionable Doctor row selection with selected-check details panel.
- [x] `Enter` toggle/focus behavior for selected-check details in Doctor view.
- [x] footer help update for Doctor `Enter` action.
- [x] TUI reducer/render tests for Doctor selected-check detail behavior.

## Phase 9 — TUI storage visualizations

- [x] storage explorer view for SQLite structural data and Qdrant semantic projection.
- [x] index coverage view grouped by file with chunk, symbol, call, embedding, and exclusion counts.
- [x] selected-file detail drawer for chunks, symbols, calls, vector status, and exclusion reasons.
- [x] symbol outline view using `symbols.parent_symbol_id`.
- [x] call resolution dashboard grouped by resolution status and confidence bucket.
- [x] embedding coverage view comparing SQLite chunks with Qdrant vector-backed chunks.
- [ ] index runs timeline using `index_runs` metadata.
- [ ] semantic neighborhood view using Qdrant metadata only.
- [ ] cross-store health warnings for missing vectors, missing collections, excluded chunks, and model/dimension drift.
- [x] TUI state/render tests for the first storage visualization, including 80x24 layout and selected-row drill-down behavior.

## Do not start yet

- web UI
- hosted mode
- multi-repo org search
- write tools
- agent code editing
- non-Rust languages

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
- [x] index runs timeline using `index_runs` metadata.
- [x] semantic neighborhood view using Qdrant metadata only.
- [x] cross-store health warnings for missing vectors, missing collections, excluded chunks, and model/dimension drift.
- [x] TUI state/render tests for the first storage visualization, including 80x24 layout and selected-row drill-down behavior.

## Phase 10 — Continuous indexing

- [x] Shared continuous indexing API in `symdex-index`.
- [x] Polling filesystem watcher for created and modified eligible files.
- [x] Debounce and coalesce event bursts before reindexing.
- [x] Reuse manual indexing ignore, path-boundary, hashing, parser, secret-filtering, SQLite, Ollama, and Qdrant rules.
- [x] Offline continuous indexing path that updates SQLite without Ollama or Qdrant.
- [x] Semantic continuous indexing path that updates Qdrant when local services are available.
- [x] CLI launch path such as `symdex index --watch <repo>`.
- [x] TUI continuous indexing toggle with explicit on/off labels and first-enable confirmation.
- [x] TUI watch status showing pending debounce state, queued event count, last reindexed file, and latest error.
- [x] TUI toggle state tests.
- [x] Tests for event coalescing, ignored paths, created-file indexing, modified-file reindexing, and unchanged-content skips.

## Phase 11 — Evidence freshness and provenance

- [x] Index provenance schema for files, chunks, symbols, calls, vectors, and index runs.
- [x] Store parser version, embedding model, vector dimension, content hash, index run ID, and indexed timestamp with returned evidence.
- [x] Staleness detection by comparing indexed content hashes against current eligible files.
- [x] CLI staleness report for repositories, files, symbols, and context packs.
- [x] TUI freshness/provenance panels for repository status, storage views, and evidence rows.
- [x] MCP response fields for freshness state and provenance metadata.
- [x] Tests for fresh, stale, deleted, missing, and unknown evidence states.

## Phase 12 — Explicit graph traversal and repeatable impact

- [x] Call path tracing API over persisted call edges with bounded traversal depth.
- [x] CLI command for call path tracing between source and target symbols.
- [x] TUI call path view with paths, hops, confidence, resolution status, and file/line evidence.
- [x] MCP tool for compact call path tracing.
- [x] Expand impact analysis to include bounded transitive paths and related files.
- [x] Add test discovery and mapping design before claiming likely affected tests.
- [x] Impact output includes provenance and staleness labels for every evidence row.
- [x] Tests for deterministic traversal order, unresolved edges, ambiguous edges, cycles, and depth limits.

## Phase 13 — Debug context and runtime mapping

- [x] Runtime-to-source input parser for stack traces, panic locations, failing test names, frame symbols, and file paths.
- [x] Source mapping API that joins runtime frames to indexed files, symbols, calls, and likely tests when available.
- [x] Debug context pack format for reusable debugging evidence bundles.
- [x] CLI command for building debug context packs from runtime failure input.
- [x] TUI debug context-pack viewer with matched frames, call paths, likely tests, provenance, and staleness.
- [x] MCP tool for debug context packs with compact context-window-safe output.
- [x] Tests for mapped frames, unmapped frames, stale frames, deleted files, and malformed stack traces.

## Phase 14 — Cross-agent local reuse

- [x] Stable MCP evidence contract envelope advertised in `initialize` and successful tool results.
- [x] Version stable evidence contracts across CLI, TUI, and MCP.
- [x] Document local/private indexing guarantees for multi-agent reuse.
- [x] Read-only cross-agent access patterns for shared SQLite and Qdrant state.
- [x] Repository root boundary checks for multi-agent requests.
- [x] Diagnostics for model, vector DB, SQLite path, index freshness, and provenance consistency.
- [x] Tests for multiple agents reading the same index without write-capable tools.

## Phase 15 — Active multi-language expansion

- [x] Define language-agnostic parser/chunker interfaces in `symdex-core`.
- [x] Add C# discovery and tree-sitter parsing for `.cs` files.
- [x] Add JavaScript discovery and tree-sitter parsing for `.js`, `.jsx`, `.mjs`,
  and `.cjs` files.
- [x] Add TypeScript discovery and tree-sitter parsing for `.ts`, `.tsx`,
  `.mts`, and `.cts` files.
- [x] Extract chunks, symbols, and conservative call edges for each new language
  through the same contracts used by Rust.
- [x] Reuse existing ignore, path-boundary, hashing, secret-detection, SQLite,
  Qdrant, manual indexing, continuous indexing, provenance, and MCP evidence
  rules.
- [x] Add fixture-backed tests for discovery, chunking, symbols, calls, secrets,
  incremental indexing, continuous indexing, and MCP evidence for C#,
  JavaScript, and TypeScript.

## Suggested next steps, prioritized

### P0 — Production hardening foundation

- [x] Add CI for fmt, clippy, tests, release build, and dependency audit.
- [x] Add JSON CLI output mirroring MCP contracts.
- [x] Record failed and partial index runs, not only successful summaries.
- [x] Implement Qdrant delete, verify, and repair lifecycle for changed or deleted chunks.
  - [x] Delete stale Qdrant points for changed and deleted chunks before SQLite cleanup.
  - [x] Verify SQLite/Qdrant vector lifecycle state.
  - [x] Repair missing, stale, orphaned, or drifted vectors.
- [x] Split large store and TUI files into modules.

### P1 — Debugging credibility

- [x] Add partial parsing with diagnostics instead of fail-closed syntax errors.
- [x] Add test discovery and failing-test mapping.
- [x] Add evidence trust scoring combining freshness, provenance, confidence, and parse/index completeness.
- [ ] Add explainability metadata for why results were returned.
- [ ] Improve debug-context parsing for common Rust outputs: `cargo test`, `anyhow`, `tracing`, `RUST_BACKTRACE=full`, panic hooks, and async stack-like output.

### P2 — Semantic precision

- [ ] Add optional rust-analyzer enrichment.
- [ ] Improve module, import, trait, and method resolution.
- [ ] Add macro-aware limitations and diagnostics.
- [ ] Add type-definition and impl/trait chunks.
- [ ] Add cross-file and cross-module call resolution.

### P3 — Operational polish

- [ ] Add structured logging and metrics.
- [ ] Add benchmarks and large-repo tests.
- [ ] Add install and release packaging.
- [ ] Improve the config model and CLI parser.
- [ ] Continue TUI modularization and UX polish for warnings, stale rows, failed runs, and repair actions.

## Do not start yet

- web UI
- hosted mode
- multi-repo org search
- write tools
- agent code editing
- languages beyond Rust, C#, JavaScript, and TypeScript

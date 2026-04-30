# TUI

## Goal

Build a native terminal UI for symdex as a local-first full control panel.

The TUI should make existing indexing, diagnostics, semantic search, structural
queries, impact analysis, and context packs easier to inspect without changing
the privacy or safety model.

## Command

```bash
symdex tui [repo]
```

`symdex-cli` owns parsing this command and launching the TUI. The implementation
lives in `crates/symdex-tui`.

## Stack

- Rust
- `ratatui` for layout, widgets, and rendering
- `crossterm` for terminal input, alternate screen, raw mode, and events

The TUI calls Rust library APIs directly. It must not shell out to the `symdex`
binary.

## MVP Views

### Repository Dashboard

- Show selected repository root and repository ID.
- Show SQLite index counts for files, chunks, symbols, and calls.
- Show latest embedding model and vector dimension when available.
- Show local service status for SQLite path, Ollama, and Qdrant.

### Indexing Controls

- Offer offline indexing with `o` and semantic indexing with `s`.
- Require explicit `y` confirmation before starting an indexing job.
- Show running, completed, failed, and cancelled states.
- Show final counts for files, chunks, symbols, calls, excluded chunks, and
  embedding status.
- Do not add reset/delete actions until matching CLI support exists.

### Doctor Diagnostics

- Show the same local diagnostics as `symdex doctor` with `d`.
- Surface service failures without panics.
- Keep diagnostics local and avoid logging source text.

### Query Workbench

- Support semantic search input with `w`, `Tab`, typed query text, and `Enter`.
- Support symbol search input with the same workbench controls.
- Display compact ranked evidence with path, line range, symbol, score, and kind.
- Show empty and error states.

### Symbol and Call Browser

- Show direct callers and callees with `g`, typed symbol text, `Tab`, and `Enter`.
- Preserve resolution status, confidence, and unresolved/ambiguous labels.

### Impact and Context Pack Viewer

- Show the basic impact view using direct callers and callees.
- Show `symdex.context_pack.v1` metadata.
- Do not include source text by default.

## Interaction Rules

- Keyboard-first; no mouse requirement.
- Use predictable keys for navigation, tabs, refresh, confirmation, cancel, and quit.
- Keep visible focus state.
- Keep views compact enough for agent-facing evidence review.
- Never hide long-running work; show loading/running/completed/failed states.
- Fail closed on invalid repository roots or missing indexes.

Current dashboard keys:

- `o`: request offline indexing confirmation
- `s`: request semantic indexing confirmation
- `d`: run doctor diagnostics
- `i`: return to indexing controls
- `w`: open the query workbench
- `g`: open the symbol/call graph browser
- `Tab`: toggle symbol and semantic query modes in the query workbench
- `Tab`: toggle callers and callees in the symbol/call graph browser
- typed text: edit the query workbench input
- typed text: edit the symbol/call graph browser input
- `Backspace`: edit the query workbench input
- `Backspace`: edit the symbol/call graph browser input
- `Enter`: run the current query workbench query
- `Enter`: run the current symbol/call graph lookup
- `r`: refresh repository status
- `y`: confirm a pending indexing job
- `n` or `Esc`: cancel a pending indexing job
- `Enter`: dismiss completed or failed job state
- `q`: quit

## Boundaries

- No web UI.
- No hosted services.
- No remote embeddings.
- No telemetry.
- No source execution.
- No source previews until a future source-preview design exists.
- No destructive maintenance actions until CLI support and a design doc exist.

## Testing Expectations

- State reducer tests for navigation, query input, confirmation flow, loading,
  empty, and error states.
- `ratatui` test-backend rendering tests for key screens.
- CLI smoke test for `symdex tui --help` or equivalent launch path.
- No-service tests for offline index/status/query screens using SQLite fixtures.

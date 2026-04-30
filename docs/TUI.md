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

- Offer offline indexing and semantic indexing actions.
- Require explicit confirmation before starting an indexing job.
- Show progress summaries and final counts.
- Do not add reset/delete actions until matching CLI support exists.

### Doctor Diagnostics

- Show the same local diagnostics as `symdex doctor`.
- Surface service failures without panics.
- Keep diagnostics local and avoid logging source text.

### Query Workbench

- Support semantic search input.
- Support symbol search input.
- Display compact ranked evidence with path, line range, symbol, score, and kind.
- Show empty and error states.

### Symbol and Call Browser

- Show symbol search results.
- Show direct callers and callees.
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

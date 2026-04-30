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

## Visual Design

The TUI should feel like a graphical control panel inside the terminal, not a
plain command transcript. Use `ratatui` styling and widgets deliberately while
keeping the interface compact, readable, and useful over SSH and in small
terminal panes.

### Color System

- Use a restrained dark-terminal-first palette with high contrast.
- Use color semantically and consistently:
  - green for healthy/complete/success states
  - yellow for warnings, pending confirmations, stale data, or partial results
  - red for errors, failed jobs, unreachable services, and rejected inputs
  - cyan or blue for active focus, selected tabs, commands, and primary labels
  - dim gray for secondary metadata, inactive controls, and empty-state hints
- Avoid color-only meaning. Every colored status must also include a text label
  such as `ok`, `missing`, `running`, `failed`, `unresolved`, or `stale`.
- Use `Style`, `Color`, `Modifier`, and `Span` rather than embedding ANSI escape
  sequences manually.
- Keep the palette readable on terminals with reduced color support. Do not
  require truecolor.

### Layout and Widgets

- Use `ratatui` widgets instead of hand-formatted text whenever they provide
  clearer structure:
  - `Tabs` for major views: Dashboard, Index, Storage, Doctor, Query, Calls,
    Impact.
  - `Block` with styled borders and titles for each focused panel.
  - `Table` for evidence lists with columns for path, line range, score,
    confidence, kind, status, and symbol.
  - `List` for compact single-column result lists, command palettes, and notes.
  - `Paragraph` with wrapping for status messages, errors, and confirmations.
  - `Gauge` for long-running indexing progress when progress is available.
  - `Sparkline` or `BarChart` only when backed by real metrics, not decoration.
- Use `TableState`, `ListState`, or equivalent stateful widgets for
  focused/selected rows.
- Use split panes for drill-down visualizations: a selectable list/table on one
  side and detail rows, relationship summaries, or health notes on the other.
- Use gauges or ratio labels only when backed by real counts, such as embedded
  chunks divided by embeddable chunks.
- Prefer split panes for workflows that compare data:
  - repository/status summary on the left
  - active view details on the right
  - footer/status bar at the bottom
- Keep footer help persistent and concise. It should show current-view keys,
  not the entire global key list.
- Use full-width panels rather than nested card-like boxes. Avoid deeply nested
  borders that waste terminal space.
- Preserve compact evidence density. Styling should improve scanning, not turn
  the app into a sparse dashboard.

### Interaction Feedback

- Highlight the active tab and focused input/list row.
- Use `Up` and `Down` to move the selected evidence row in completed diagnostic,
  storage, query, call graph, impact, and context-pack tables.
- In the Storage view, row selection should drive a visible detail panel for
  the selected SQLite/Qdrant metric and nearby storage health notes.
- In the Storage view, `F2` toggles between storage overview, index coverage,
  symbol outline, call resolution, embedding coverage, and index runs timeline
  modes, plus semantic neighborhood payload and cross-store health views.
- In the Doctor view, row selection must drive visible detail output for the
  selected check rather than highlight-only behavior.
- In the Doctor view, `Enter` should toggle an expanded selected-check detail
  panel (or focus it if already visible) showing the full check label, status,
  and diagnostic message.
- Style pending confirmation states with a warning color and explicit `y/n`
  choices.
- Style loading/running states distinctly from idle states.
- Style empty states as deliberate placeholders, not blank panels.
- Style failed states with a short red status line plus the error text.
- Show selected query mode, graph direction, and impact/context mode as tabs or
  segmented controls rather than only inline prose.

### Accessibility and Terminal Compatibility

- Do not rely on mouse input.
- Do not rely on Unicode symbols that render poorly in common terminal fonts.
  ASCII fallbacks are preferred for status markers.
- Keep borders and labels legible at 80x24.
- Avoid blinking text and excessive modifiers.
- Ensure long paths, symbols, and errors wrap or truncate intentionally with
  preserved line ranges and status labels.

## MVP Views

### Repository Dashboard

- Show selected repository root and repository ID.
- Show SQLite index counts for files, chunks, symbols, and calls.
- Show latest embedding model and vector dimension when available.
- Show local service status for SQLite path, Ollama, and Qdrant.
- Use status-colored labels for local service health and index freshness.
- Use compact table rows for index counts and local service targets instead of
  plain paragraphs.
- Keep repository identity, index freshness, counts, service targets, and
  embedding readiness visually grouped inside the status pane.

### Storage Explorer

The TUI should add a storage-focused view for inspecting how SQLite and Qdrant
represent the indexed repository.

- Treat SQLite as the structural source of truth:
  - `repositories`
  - `index_runs`
  - `files`
  - `symbols`
  - `chunks`
  - `calls`
- Treat Qdrant as the semantic projection of eligible chunks:
  - collection name
  - embedding model
  - vector dimension
  - point count when available
  - payload fields: repository, file, chunk, symbol, path, language, kind, line
    range, and text hash
- Do not display source text.
- Keep collection and point inspection metadata-only.
- Surface cross-store mismatches as warning/error rows rather than hidden
  implementation details.
- The first implementation is a Storage tab that renders a selectable
  SQLite/Qdrant metric table plus a detail/health panel.
- The Storage tab also includes `F2` modes for file-grouped index coverage,
  symbol outlines, call resolution, embedding coverage, and index runs
  timeline, plus semantic neighborhood payload metadata and cross-store health
  warnings.

### Index Coverage View

- Show a selectable file table with:
  - path
  - language
  - chunks
  - symbols
  - calls
  - embeddable chunks
  - embedded/vector-backed chunks
  - excluded chunks
- Use status labels such as `covered`, `metadata-only`, `excluded`, `stale`, and
  `missing-vector`.
- Selecting a file should drive a detail pane with chunk, symbol, call, and
  exclusion summaries for that file.
- Use gauges only for real ratios, such as embedded chunks over embeddable
  chunks.
- The first implementation lives in the Storage tab behind `F2` and renders a
  compact `Table` plus a selected-file coverage detail panel.

### File Detail Drawer

- For the selected file, show separate compact sections for:
  - chunks with kind, line range, symbol, vector point status, and exclusion
    reason
  - symbols with kind, qualified name, parent relationship, and line range
  - calls with caller symbol, callee text, call line, confidence, and resolution
    status
- Preserve metadata-only behavior. Do not show source previews unless a future
  source-preview design explicitly adds them.
- The first implementation is part of the Storage tab index coverage mode and
  shows bounded chunk, symbol, and call summaries for the selected file.

### Symbol Outline View

- Render `symbols.parent_symbol_id` relationships as a keyboard-navigable
  outline when parent data exists.
- Show symbol kind, qualified name, and line range.
- Selecting a symbol should allow jumping to callers/callees, impact, and
  context-pack views using the symbol query.
- The first implementation lives in the Storage tab behind `F2` and shows
  symbol depth, child count, path, line range, and selected-symbol detail.

### Call Resolution Dashboard

- Summarize calls by `resolution_status` and confidence buckets.
- Show direct counts for resolved, unresolved, and ambiguous/future statuses.
- Selecting a bucket should show rows with caller symbol, callee text, call line,
  path, confidence, and resolution status.
- Highlight unresolved or low-confidence call evidence with warning colors.
- The first implementation lives in the Storage tab behind `F2` and shows
  bucket counts, average confidence, and representative call rows.

### Embedding Coverage View

- Compare SQLite chunks against Qdrant-backed semantic coverage:
  - total chunks
  - chunks excluded from embedding
  - chunks with `qdrant_point_id`
  - chunks missing vector metadata
  - latest model and dimension
  - Qdrant collection name
- Show model or dimension drift as an error state.
- Show excluded chunks by reason so secret filtering remains auditable without
  exposing source text.
- The first implementation lives in the Storage tab behind `F2` and shows a
  compact metric table plus a selected-metric detail panel with collection,
  exclusion-reason, and health summaries.

### Index Runs Timeline

- Render `index_runs` as a compact table:
  - started/finished time
  - status
  - files seen/indexed
  - chunks embedded
  - model
  - dimension
  - error summary
- Selecting a run should show detailed counts and any error summary.
- Failed or partial runs should be visually distinct from successful runs.
- The first implementation lives in the Storage tab behind `F2` and shows a
  compact timeline table plus a selected-run detail panel with status, counts,
  model, dimension, timestamps, and error summary.

### Semantic Neighborhood View

- Future semantic-neighborhood features may use Qdrant metadata to inspect
  semantically related chunks, but must remain metadata-first.
- The first version should show path, line range, symbol, chunk kind, score, and
  text hash only.
- The first implementation lives in the Storage tab behind `F2` and shows
  vector-backed chunk payload metadata from the Qdrant projection: path, line
  range, symbol, chunk kind, language, text hash, point ID, collection, and a
  `metadata` score label when no live nearest-neighbor score is available.
- Do not fetch or display full source text as part of this view.

### Cross-Store Health View

- Surface cross-store health warnings for:
  - missing expected collection metadata
  - embeddable chunks missing vector point IDs
  - intentionally excluded chunks
  - configured model drift from the latest indexed model
  - inconsistent recorded vector dimensions across successful runs
- Selecting a health row should show the expected collection name, status, and
  detailed warning text.
- The first implementation lives in the Storage tab behind `F2` and uses
  SQLite metadata plus recorded Qdrant point IDs. It does not require live
  Qdrant service checks.

### Indexing Controls

- Offer offline indexing with `o` and semantic indexing with `s`.
- Require explicit `y` confirmation before starting an indexing job.
- Show running, completed, failed, and cancelled states.
- Show final counts for files, chunks, symbols, calls, excluded chunks, and
  embedding status.
- Use a confirmation panel styled as a warning state.
- Use a progress gauge while indexing is running, backed by local indexing
  progress events.
- The first progress gauge is phase-based: discovery/parsing, SQLite
  persistence, embedding, and Qdrant upload. It must show the phase name and
  completed/total counts so the gauge is not color-only.
- Do not add reset/delete actions until matching CLI support exists.

### Doctor Diagnostics

- Show the same local diagnostics as `symdex doctor` with `d`.
- Surface service failures without panics.
- Keep diagnostics local and avoid logging source text.
- Render diagnostics as a table with check name, status, and detail columns.
- Color status labels by severity.
- Keep `Up` / `Down` row navigation stateful and visible.
- Display selected-check details in a dedicated panel below or beside the table
  when diagnostics are completed.
- The selected-check details panel should include:
  - check label
  - status label
  - full diagnostic detail text (wrapped)
  - optional remediation hint when available from diagnostics output
- In the Doctor view, `Enter` toggles or focuses the selected-check details
  panel instead of being a no-op.

### Query Workbench

- Support semantic search input with `w`, `F2`, typed query text, and `Enter`.
- Support symbol search input with the same workbench controls.
- Display compact ranked evidence with path, line range, symbol, score, and kind.
- Show empty and error states.
- Render mode selection as tabs.
- Render results as a table when columns fit; fall back to a compact list on
  narrow terminals.

### Symbol and Call Browser

- Show direct callers and callees with `g`, typed symbol text, `F2`, and `Enter`.
- Preserve resolution status, confidence, and unresolved/ambiguous labels.
- Render callers/callees as tabs or a segmented control.
- Use a table for symbol, path, line range, confidence, callee text, and
  resolution status.

### Impact and Context Pack Viewer

- Show the basic impact view using direct callers and callees with `p`, typed
  symbol text, `F2`, and `Enter`.
- Show `symdex.context_pack.v1` metadata with the same viewer controls.
- Do not include source text by default.
- Render impact sections as separate panels or tables for direct callers and
  direct callees.
- Render context-pack metadata as grouped sections: focus symbols,
  relationships, files, limits, and notes.

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
- `x`: open the storage explorer
- `w`: open the query workbench
- `g`: open the symbol/call graph browser
- `p`: open the impact/context-pack viewer
- `Tab`: switch to the next major TUI view
- `Shift+Tab`: switch to the previous major TUI view
- `F2`: toggle storage overview, index coverage, symbol outline, call resolution, embedding coverage, index runs timeline, semantic neighborhood, and cross-store health in the storage explorer
- `F2`: toggle symbol and semantic query modes in the query workbench
- `F2`: toggle callers and callees in the symbol/call graph browser
- `F2`: toggle impact and context-pack modes in the impact/context-pack viewer
- `Up` / `Down`: move the selected row in completed result and storage tables
- `Enter`: toggle/focus selected-check details in Doctor diagnostics view
- typed text: edit the query workbench input
- typed text: edit the symbol/call graph browser input
- typed text: edit the impact/context-pack viewer input
- `Backspace`: edit the query workbench input
- `Backspace`: edit the symbol/call graph browser input
- `Backspace`: edit the impact/context-pack viewer input
- `Enter`: run the current query workbench query
- `Enter`: run the current symbol/call graph lookup
- `Enter`: run the current impact/context-pack lookup
- `r`: refresh repository and storage status
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
- Rendering tests should assert semantic styling where practical: active tab,
  selected row, success/warning/error labels, and table headers.
- Add narrow-terminal render tests for 80x24 to ensure the richer widget layout
  remains usable without overlap.
- CLI smoke test for `symdex tui --help` or equivalent launch path.
- No-service tests for offline index/status/query screens using SQLite fixtures.

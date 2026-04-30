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
  - `Tabs` for major views: Dashboard, Index, Doctor, Query, Calls, Impact.
  - `Block` with styled borders and titles for each focused panel.
  - `Table` for evidence lists with columns for path, line range, score,
    confidence, kind, status, and symbol.
  - `List` for compact single-column result lists, command palettes, and notes.
  - `Paragraph` with wrapping for status messages, errors, and confirmations.
  - `Gauge` for long-running indexing progress when progress is available.
  - `Sparkline` or `BarChart` only when backed by real metrics, not decoration.
- Use `TableState`, `ListState`, or equivalent stateful widgets for
  focused/selected rows.
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
  query, call graph, impact, and context-pack tables.
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
- `w`: open the query workbench
- `g`: open the symbol/call graph browser
- `p`: open the impact/context-pack viewer
- `Tab`: switch to the next major TUI view
- `Shift+Tab`: switch to the previous major TUI view
- `F2`: toggle symbol and semantic query modes in the query workbench
- `F2`: toggle callers and callees in the symbol/call graph browser
- `F2`: toggle impact and context-pack modes in the impact/context-pack viewer
- `Up` / `Down`: move the selected row in completed result tables
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
- Rendering tests should assert semantic styling where practical: active tab,
  selected row, success/warning/error labels, and table headers.
- Add narrow-terminal render tests for 80x24 to ensure the richer widget layout
  remains usable without overlap.
- CLI smoke test for `symdex tui --help` or equivalent launch path.
- No-service tests for offline index/status/query screens using SQLite fixtures.

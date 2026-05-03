# TUI

## Goal

Build a native terminal UI for symdex as a local-first full control panel.

The TUI should make existing indexing, diagnostics, semantic search, structural
queries, impact analysis, context packs, and debug context packs easier to inspect without changing
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
  - Nested `Tabs` inside the Storage view for storage overview, index coverage,
    symbol outline, call resolution, embedding coverage, index runs, evidence
    freshness, semantic neighborhood, and cross-store health.
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
  - full repository/status detail in the Dashboard/Overview view
  - compact repository/status summary beside or above data-heavy active views
  - two-container footer at the bottom
- Use adaptive body layouts rather than a fixed permanent status pane. Wide
  terminals should reserve most horizontal space for the active workflow, while
  narrow terminals should stack the compact repository summary above the active
  panel.
- Keep footer help persistent and concise. It should show current-view keys,
  not the entire global key list.
- The footer must be two separate stacked containers:
  - top container: stylized keyboard shortcut hints for the active view
  - bottom container: status bar content, including current view/status labels,
    running job state, warnings, errors, and the latest user-facing message
- Do not combine shortcut hints and status messages into the same bordered
  panel.
- Use full-width panels rather than nested card-like boxes. Avoid deeply nested
  borders that waste terminal space.
- Preserve compact evidence density. Styling should improve scanning, not turn
  the app into a sparse dashboard.
- Selectable tables must render from the full result set rather than a fixed
  row cap. Each table pane should use the available pane height and rely on
  stateful table selection to scroll independently when the selected row moves
  beyond the currently visible viewport.

### Ratatui widget evaluation

The current TUI uses the right stable baseline from `ratatui`: `Block`,
`Paragraph`, `List`, `Table`, `Tabs`, `Gauge`, `Scrollbar`, and `BarChart`.
Keep these as the default building blocks because they are built in, compact,
easy to test with the ratatui test backend, and already match symdex's local
metadata-first model.

Built-in widget decisions:

| Widget | Decision | Symdex fit |
|---|---|---|
| `Block` | Keep using | Primary panel boundary, titles, status-colored borders, footer containers, and detail panes. |
| `Paragraph` | Keep using | Wrapped status, confirmations, details, and input echoes. |
| `List` | Keep using | Compact notes and single-column fallback rows. |
| `Table` | Keep using | Best default for evidence, storage, diagnostics, calls, impact, and context-pack metadata. |
| `Tabs` | Keep using | Major views, storage subviews, and mode selectors. |
| `Gauge` | Keep using | Manual indexing progress and fast/quality semantic readiness where real counts exist. |
| `Sparkline` | Avoid unless a future series metric needs it | Current job-state summaries are clearer as count-backed bar charts. |
| `Scrollbar` | Use now | Long selectable tables and metadata line panes need visible position without reducing evidence density. |
| `BarChart` | Use now | Useful for real bucketed metrics: call resolution buckets, index-run outcomes, freshness status counts, embedding coverage, and quality job states. |
| `Chart` | Defer | Potentially useful for index-run duration/throughput over time, but only after store/query APIs expose stable time-series metrics. |
| `Canvas` | Defer | Could visualize call paths or graph topology, but table evidence is clearer and more accessible for the MVP. |
| `Calendar` | Avoid for now | Index activity is better shown as timeline rows or bar charts; calendar layout spends too much space at 80x24. |

Third-party widget decisions:

| Widget crate | Decision | Symdex fit |
|---|---|---|
| `tui-tree-widget` | Good candidate | Symbol outlines and future file/package hierarchy would benefit from collapsible keyboard navigation. Keep metadata-only labels: path, symbol kind, line range, child count, and status. |
| `ratatui-textarea` | Good candidate | Debug-context input often needs pasted multiline panic/backtrace text. It can also improve query/call/context inputs if single-line behavior remains fast. |
| `tui-scrollview` | Good candidate | Detail drawers and context-pack/debug-context metadata can exceed available height; use only if built-in scrollbars plus existing state are insufficient. |
| `tui-widget-list` | Good candidate with caution | Could simplify stateful scrolling lists, but tables remain better for most symdex evidence. Consider it for command/result lists only after scrollbar work. |
| `throbber-widgets-tui` | Optional | Could replace the current ASCII continuous-index activity indicator, but the existing indicator is sufficient unless users need clearer running states. |
| `tui-checkbox` | Optional | Could make future settings toggles clearer, such as offline/semantic/full/incremental, but current explicit text confirmations are safer. |
| `tui-menu` | Defer | Menus can help discoverability, but symdex must stay keyboard-first and text-entry-friendly; avoid menu systems that steal letter keys. |
| `tui-nodes` | Defer | Possible call-graph topology view, but tables with confidence and line evidence are more actionable and accessible today. |
| `tui-logger` | Defer | A log panel could help diagnostics, but logs must never include source text and structured logging is not yet a TUI surface. |
| `tui-piechart` | Avoid | Pie charts are less precise than tables or bar charts for health and coverage counts. |
| `tui-big-text` | Avoid | Decorative large labels reduce evidence density and hurt 80x24 usability. |
| `ratatui-image` | Avoid | Image rendering does not support codebase intelligence workflows and complicates terminal compatibility. |
| `tui-term` | Avoid | Embedding a terminal risks source execution workflows and conflicts with the TUI boundary that it must call Rust APIs directly. |

Recommended implementation order:

1. Add built-in `Scrollbar` support to every selectable table and any bounded
   detail pane whose rows can exceed the visible height.
2. Add `BarChart` summaries for storage health and semantic/indexing counts
   that already exist in SQLite/query summaries.
3. Evaluate `ratatui-textarea` for debug-context multiline input, including
   paste, cursor, and 80x24 render tests.
4. Evaluate `tui-tree-widget` for symbol outline and optional file hierarchy
   drill-down, preserving table fallbacks for narrow terminals.
5. Revisit `Chart`, `Canvas`, `tui-nodes`, and `tui-logger` only after their
   backing data contracts exist and their accessibility tradeoffs are tested.

Phase 10.5 implementation status:

- Built-in `Scrollbar` indicators are used for selectable table panes and long
  line-list metadata panes. They use ASCII thumb/track symbols for terminal
  compatibility and keep row selection as the scrolling driver.
- Built-in `BarChart` summaries are used only where real local counts already
  exist: call-resolution buckets, embedding coverage, index-run outcomes,
  freshness states, and fast/quality job state counts. Compact layouts keep the
  detail pane visible and defer charts when height is constrained.
- `ratatui-textarea` remains deferred. Debug-context input is the strongest fit,
  but the current single-line query/call/context inputs remain faster and safer
  until multiline debug-context editing needs paste/cursor behavior beyond the
  existing string input state.
- `tui-tree-widget` remains deferred. The current symbol outline table already
  exposes path, symbol kind, line range, child count, and status-safe metadata;
  a tree widget should wait until collapse/expand state and file hierarchy
  navigation are needed enough to justify an additional dependency.
- Image, embedded-terminal, decorative big-text, pie-chart, graph/canvas, and
  mouse/menu-centric widgets remain avoided until a design proves they improve
  evidence review without reducing 80x24 metadata density or weakening the
  local read-only TUI boundary.

### Interaction Feedback

- Highlight the active tab and focused input/list row.
- Use `Up` and `Down` to move the selected evidence row in completed diagnostic,
  storage, query, call graph, impact, context-pack, and debug-context tables.
- Completed table views must allow `Up` and `Down` to reach every row returned
  by the backing query or summary, even when the terminal cannot display all
  rows at once.
- In the Storage view, row selection should drive a visible detail panel for
  the selected SQLite/sqlite-vec metric and nearby storage health notes.
- In the Storage view, always show a self-contained storage tab header above
  the active storage visualization so users can see every storage subview
  without relying on footer help.
- In the Storage view, `Tab` and `Shift+Tab` cycle between storage overview,
  index coverage, symbol outline, call resolution, embedding coverage, index
  runs timeline, evidence freshness, semantic neighborhood payload, and
  cross-store health views.
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
- Show selected query mode, graph direction, and impact/call-path/context/debug mode as tabs or
  segmented controls rather than only inline prose.
- Completed Query, Calls, and Impact-style result views should keep a visible
  mode/status bar above the result table so the selected mode remains obvious
  after results render.

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
- Show active semantic layer, quality readiness, fallback state, and compact
  quality job counts.
- Show local service status for SQLite path, Ollama, and sqlite-vec.
- Use status-colored labels for local service health and index freshness.
- Use compact table rows for index counts and local service targets instead of
  plain paragraphs.
- Keep repository identity, index freshness, counts, service targets, and
  embedding readiness visually grouped inside the status pane.
- The Dashboard/Overview tab is the full home surface for repository and local
  service status. Other tabs should use a compact repository summary so active
  tables and detail panes get more screen space.
- The Overview should surface compact operational state for indexing, active
  semantic layer, quality readiness, quality job counts, continuous indexing,
  storage mode, query mode, call direction, and evidence mode without showing
  source text.

### Storage Explorer

The TUI should add a storage-focused view for inspecting how SQLite and sqlite-vec
represent the indexed repository.

- Treat SQLite as the structural source of truth:
  - `repositories`
  - `index_runs`
  - `files`
  - `symbols`
  - `chunks`
  - `calls`
- Treat sqlite-vec as the semantic projection of eligible chunks:
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
  SQLite/sqlite-vec metric table plus a detail/health panel.
- The Storage tab must always render a nested tab header for its storage
  visualizations. The first labels may be compact for narrow terminals, such as
  `Store`, `Files`, `Syms`, `Calls`, `Vecs`, `Runs`, `Fresh`, `Near`, and
  `Health`.
- The Storage tab also includes `Tab` / `Shift+Tab` modes for file-grouped
  index coverage, symbol outlines, call resolution, embedding coverage, index
  runs timeline, evidence freshness, semantic neighborhood payload metadata, and
  cross-store health warnings.

### Evidence Freshness View

- Compare indexed file content hashes with the current eligible files for
  implemented languages.
- Show fresh, stale, deleted, missing, and unknown labels with selected-row
  provenance details.
- Include content hash, index run ID, parser version, and indexed timestamp
  without showing source text.

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
- The first implementation lives in the Storage tab mode cycle and renders a
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
- The first implementation lives in the Storage tab mode cycle and shows
  symbol depth, child count, path, line range, and selected-symbol detail.

### Call Resolution Dashboard

- Summarize calls by `resolution_status` and confidence buckets.
- Show direct counts for resolved, unresolved, and ambiguous/future statuses.
- Selecting a bucket should show rows with caller symbol, callee text, call line,
  path, confidence, and resolution status.
- Highlight unresolved or low-confidence call evidence with warning colors.
- The first implementation lives in the Storage tab mode cycle and shows
  bucket counts, average confidence, and representative call rows.

### Embedding Coverage View

- Compare SQLite chunks against sqlite-vec-backed semantic coverage:
  - total chunks
  - chunks excluded from embedding
  - chunks with current fast `chunk_embeddings` rows
  - chunks missing vector metadata
  - latest model and dimension
  - sqlite-vec collection name
- Show model or dimension drift as an error state.
- Show excluded chunks by reason so secret filtering remains auditable without
  exposing source text.
- The first implementation lives in the Storage tab mode cycle and shows a
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
- The first implementation lives in the Storage tab mode cycle and shows a
  compact timeline table plus a selected-run detail panel with status, counts,
  model, dimension, timestamps, and error summary.

### Semantic Neighborhood View

- Future semantic-neighborhood features may use sqlite-vec metadata to inspect
  semantically related chunks, but must remain metadata-first.
- The first version should show path, line range, symbol, chunk kind, score, and
  text hash only.
- The first implementation lives in the Storage tab mode cycle and shows
  vector-backed chunk payload metadata from the sqlite-vec projection: path, line
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
- The first implementation lives in the Storage tab mode cycle and uses
  SQLite metadata plus recorded sqlite-vec point IDs. It does not require live
  sqlite-vec service checks.

### Indexing Controls

- Offer offline indexing with `o` and semantic indexing with `s`.
- Offer full and incremental manual indexing as separate scope choices, toggled
  with `Tab` / `Shift+Tab` on the Index tab. Offline and semantic indexing both
  use the selected scope after explicit `y` confirmation. The selected scope
  defaults to incremental.
- Show fast and quality semantic readiness as progress bars in the Index tab.
  Each bar should display the percentage and ready/expected chunk counts for
  `fast_ready` and `quality_ready` coverage.
- Show fast and quality pending-job, running-job, and skipped-stale-job
  sparklines in the Index tab, using each job count as a percentage of the
  total expected chunk count.
- Offer continuous indexing as a toggleable mode.
- Continuous indexing is off by default.
- When continuous indexing is on, modified or newly created eligible files are
  automatically reindexed after debounce.
- Continuous indexing must use the same ignore, path-boundary, hashing,
  parsing, secret-detection, and embedding rules as manual indexing.
- Require explicit `y` confirmation before starting an indexing job.
- Require explicit confirmation before enabling continuous indexing for the
  first time in a session because it starts an ongoing local job.
- Toggling continuous indexing off should stop the watcher promptly without
  deleting index data.
- Show running, completed, failed, and cancelled states.
- Show continuous indexing state with explicit `on` / `off` labels, pending
  debounce state, last reindexed file, queued event count, and latest error.
- When continuous indexing is on, show an animated ratatui-rendered activity
  indicator in the Indexing controls and the global footer status row.
- Show final counts for files, chunks, symbols, calls, excluded chunks, and
  embedding status.
- Use a confirmation panel styled as a warning state.
- Use a progress gauge while indexing is running, backed by local indexing
  progress events.
- The first progress gauge is phase-based: discovery/parsing, SQLite
  persistence, embedding, and sqlite-vec upload. It must show the phase name and
  completed/total counts so the gauge is not color-only.
- Do not add reset/delete actions until matching CLI support exists.

### Doctor Diagnostics

- Show the same local diagnostics as `symdex doctor` from the Doctor tab.
- In the Doctor tab, `Enter` starts diagnostics when no diagnostic result rows
  are available.
- In the Doctor tab, `r` reruns diagnostics after an idle, completed, or failed
  diagnostics state. If diagnostics are already running, keep the existing run
  and show that status instead of starting a duplicate worker.
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
  panel when diagnostic result rows are available.

### Query Workbench

- Support semantic search input from the Query tab with `Tab` / `Shift+Tab`,
  typed query text, and `Enter`.
- Support symbol search input with the same workbench controls.
- Display compact ranked evidence with path, line range, symbol, score, and kind.
- Show empty and error states.
- Render mode selection as tabs.
- Render results as a table when columns fit; fall back to a compact list on
  narrow terminals.

### Symbol and Call Browser

- Show direct callers and callees from the Calls tab with typed symbol text,
  `Tab` / `Shift+Tab`, and `Enter`.
- Preserve resolution status, confidence, and unresolved/ambiguous labels.
- Render callers/callees as tabs or a segmented control.
- Use a table for symbol, path, line range, confidence, callee text, and
  resolution status.

### Impact, Call Path, Context Pack, and Debug Context Viewer

- Show impact using direct callers/callees, bounded transitive path counts,
  freshness labels, and related-file metadata from the Impact tab with typed
  symbol text, `Tab` / `Shift+Tab`, and `Enter`.
- Show call path tracing from the same tab with typed `source -> target` input.
  Render paths as rows with path number, hop number, edge, confidence, file,
  call line, and resolution status.
- Show `symdex.context_pack.v1` metadata with the same viewer controls.
- Show `symdex.debug_context.v1` metadata from runtime failure input with
  matched frames, call paths between frames, likely tests, freshness, and
  provenance labels.
- Do not include source text by default.
- Render impact sections as separate panels or tables for direct callers and
  direct callees.
- Render context-pack metadata as grouped sections: focus symbols,
  relationships, files, limits, and notes.

## Interaction Rules

- Keyboard-first; no mouse requirement.
- Use predictable keys for navigation, view selection, mode switching, refresh,
  confirmation, cancel, and quit.
- Primary views are selected with bracket navigation instead of letter keys:
  `[` moves to the previous major tab and `]` moves to the next major tab.
- Letter keys must remain available to text-entry views for query, call graph,
  impact, context-pack, and debug-context inputs.
- Keep visible focus state.
- Keep views compact enough for agent-facing evidence review.
- Never hide long-running work; show loading/running/completed/failed states.
- Fail closed on invalid repository roots or missing indexes.

Current dashboard keys:

- `o`: request offline indexing confirmation
- `s`: request semantic indexing confirmation
- `c`: toggle continuous indexing confirmation
- `[`: move to the previous primary tab
- `]`: move to the next primary tab
- `Tab`: switch to the next mode in the active view
- `Shift+Tab`: switch to the previous mode in the active view
- `Tab` / `Shift+Tab`: cycle storage overview, index coverage, symbol outline,
  call resolution, embedding coverage, index runs timeline, semantic
  neighborhood, and cross-store health in the storage explorer
- `Tab` / `Shift+Tab`: toggle symbol and semantic query modes in the query workbench
- `Tab` / `Shift+Tab`: toggle callers and callees in the symbol/call graph browser
- `Tab` / `Shift+Tab`: toggle impact, call-path, context-pack, and debug-context modes in the impact/call-path/context-pack/debug-context viewer
- `Up` / `Down`: move the selected row in completed result and storage tables
- `Enter`: start Doctor diagnostics when the Doctor tab has no result rows
- `Enter`: toggle/focus selected-check details in Doctor diagnostics view
- `r`: rerun Doctor diagnostics from the Doctor tab
- typed text: edit the query workbench input
- typed text: edit the symbol/call graph browser input
- typed text: edit the impact/call-path/context-pack/debug-context viewer input
- `Backspace`: edit the query workbench input
- `Backspace`: edit the symbol/call graph browser input
- `Backspace`: edit the impact/call-path/context-pack/debug-context viewer input
- `Enter`: run the current query workbench query
- `Enter`: run the current symbol/call graph lookup
- `Enter`: run the current impact/call-path/context-pack/debug-context lookup
- `r`: refresh repository and storage status
- `y`: confirm a pending indexing job
- `n` or `Esc`: cancel a pending indexing job or continuous-indexing toggle
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
  nested storage tab header, selected row, success/warning/error labels, and
  table headers.
- Add narrow-terminal render tests for 80x24 to ensure the richer widget layout
  remains usable without overlap.
- CLI smoke test for `symdex tui --help` or equivalent launch path.
- No-service tests for offline index/status/query screens using SQLite fixtures.

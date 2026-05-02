# Continuous Indexing

## Goal

Continuous indexing keeps the local index fresh while a user or agent edits a
repository. It is a local-only watch mode that can be toggled on or off. When
enabled, modified or newly created eligible files are automatically reindexed.

## Product Contract

- Continuous indexing is off by default.
- Users must be able to toggle it on and off from the TUI.
- A CLI launch path should also exist for non-interactive use, such as
  `symdex index --watch <repo>` or an equivalent command.
- Watch mode must never execute indexed repository code.
- Watch mode must never send source text, embeddings, paths, or metadata to
  remote services.
- Watch mode must obey the same local service configuration as manual indexing.
- Offline continuous indexing should remain possible without Ollama or Qdrant.
- Semantic continuous indexing requires local Ollama and Qdrant, just like
  manual semantic indexing.

## Event Handling

- Watch for eligible file create and modify events under the configured
  repository root. The current implementation uses a polling watcher that
  compares discovered file content hashes across intervals.
- Ignore files and directories excluded by built-in excludes, glob-aware
  `.gitignore` rules, and future `.symdexignore` rules.
- Reject symlink escapes and paths outside the configured repository root.
- Debounce and coalesce bursts of filesystem events before indexing.
- Reindex only files whose content hash changed.
- Created files should be discovered, parsed, persisted to SQLite, and embedded
  when semantic indexing is enabled and chunks are embeddable.
- Modified files should replace prior SQLite facts and Qdrant points
  atomically where practical.
- Deleted-file cleanup should continue to be handled by the incremental
  indexing path, even though the first continuous MVP is focused on created and
  modified files.

## TUI Behavior

- The Indexing view should show a continuous indexing toggle with a clear
  `on` / `off` status label.
- Toggling continuous indexing on should require explicit confirmation the
  first time in a session because it starts an ongoing local job.
- Toggling continuous indexing off should stop watching promptly and leave
  already completed index updates intact.
- The status row should show the latest watch event, latest reindexed file,
  pending debounce state, current indexing state, and any error.
- Continuous indexing must not block query, storage, or diagnostics views.
- Manual indexing should remain available while continuous mode is off.
- If a manual indexing job is running, continuous indexing should queue or
  coalesce file events instead of running concurrent writes.

## Implementation Boundaries

- `symdex-index` owns watch orchestration, event coalescing, and per-file
  reindex jobs.
- `symdex-core` owns path normalization, ignore decisions, parsing, chunking,
  hashing, symbol extraction, and call extraction.
- `symdex-store` owns SQLite updates, Qdrant point replacement, and index run
  metadata.
- `symdex-cli` owns argument parsing and a non-interactive watch launch path.
- `symdex-tui` owns toggle state, rendering, confirmation, and event display.
- The TUI and CLI must call shared Rust APIs directly. They must not shell out
  to `symdex` subprocesses.
- The MCP server remains read-only for the MVP and must not start or stop
  continuous indexing.

Current implementation status:

- `symdex-index` exposes shared watch snapshot, diff, and continuous polling
  APIs.
- `symdex index --watch <repo>` starts the non-interactive watch loop and uses
  the shared indexing APIs directly.
- Continuous batches call the incremental index path so unchanged files are
  skipped by content hash.
- The TUI Indexing view exposes a `c` toggle with first-enable confirmation,
  explicit `on` / `off` labels, pending debounce state, queued event count,
  last reindexed file, latest error display, and an animated activity indicator
  while continuous indexing is on.

## Observability

- Log summaries only: event counts, paths, index counts, status labels, and
  errors.
- Do not log source text.
- Record index run summaries for continuous indexing batches so storage views
  can show when watch-driven updates occurred.
- Surface watch health in diagnostics when available, including watcher active
  state and the most recent error.

## Testing

- Unit test event coalescing and debounce behavior with synthetic paths.
- Unit test that ignored paths, symlink escapes, and out-of-root paths do not
  schedule reindex work.
- Integration test that a created eligible implemented-language file is indexed
  in continuous mode.
- Integration test that a modified eligible implemented-language file replaces
  stale SQLite facts.
- Integration test that unchanged content after a filesystem event is skipped.
- Test offline continuous indexing without Qdrant or Ollama.
- Test semantic continuous indexing with mocked or opt-in local Ollama/Qdrant.
- TUI reducer tests should cover toggle on, confirmation, toggle off, queued
  event display, and error state rendering.

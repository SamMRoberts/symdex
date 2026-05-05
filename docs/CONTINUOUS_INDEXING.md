# Continuous Indexing

## Goal

Continuous indexing keeps the local index fresh while a user or agent edits a
repository. It is a local-only watch mode that can be toggled on or off. When
enabled, modified or newly created eligible files are automatically reindexed.

## Product Contract

- `symdex tui [repo]` starts or attaches to the single background watcher for
  the repository.
- Users must be able to toggle it off and back on from the TUI, CLI, and
  explicit watcher MCP tools.
- A CLI launch path should also exist for non-interactive use, such as
  `symdex index --watch <repo>` or an equivalent command.
- Watch mode must never execute indexed repository code.
- Watch mode must never send source text, embeddings, paths, or metadata to
  remote services.
- Watch mode must obey the same local service configuration as manual indexing.
- Offline continuous indexing should remain possible without Ollama or sqlite-vec.
- Semantic continuous indexing requires local Ollama and sqlite-vec, just like
  manual semantic indexing.
- When layered semantic indexing is enabled, continuous indexing must update the
  fast `nomic-embed-text` layer synchronously and queue the quality
  `mxbai-embed-large` layer as deferred work.
- Continuous indexing must not wait for quality indexing before returning to
  watch mode.
- Single-writer rule: only ONE process may write to each configured local
  SQLite/sqlite-vec database file at a time. Database-role-scoped writer
  services own mutations for their files. Structural indexing, manual indexing,
  repair, migrations, and future structural write-capable tools use the
  structural writer. Quality catch-up claims, completions, embeddings, and
  activation metadata use the quality-semantic database role. Watcher status and
  client lease rows use the watch database role, and index-run/event rows use the
  events database role. TUI, MCP, diagnostics, status, and query paths read
  shared state without structural migrations or cleanup writes.
- Manual structural writer jobs take priority over structural watcher writes.
  The watcher keeps polling and coalescing filesystem changes while a manual
  index, repair, or migration owns the structural gate. If that gate is busy,
  the watcher defers only the incremental structural batch and leaves the prior
  snapshot active so the next poll folds in any additional changes. Watcher
  status, client lease heartbeats, index events, and quality catch-up do not wait
  on the structural SQLite writer gate.

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
- Modified files should replace prior SQLite facts and sqlite-vec points
  atomically where practical.
- Deleted-file cleanup should continue to be handled by the incremental
  indexing path, even though the first continuous MVP is focused on created and
  modified files.
- Branch-aware indexing is being introduced incrementally. The current local ref
  can be recorded in SQLite, index-run provenance, and the `ref_files` path
  manifest. Continuous indexing also links the active ref to the semantic
  generation recorded by each semantic batch, so status and search can route
  through the checked-out ref's generation while preserving legacy repo-wide
  fallback for older indexes.

## Layered Semantic Indexing

`docs/LAYERED_SEMANTIC_INDEXING.md` defines the authoritative layered semantic
indexing behavior. This document adds the continuous-indexing-specific contract.

Watch mode must treat the fast semantic layer as the only synchronous semantic
availability requirement:

```text
file changes
  -> debounce/coalesce
  -> structural SQLite update
  -> fast nomic-embed-text embedding
  -> fast sqlite-vec upsert in the fast semantic database role
  -> mark quality stale when needed
  -> enqueue quality jobs
  -> return to watching
```

The quality layer must be treated as eventual precision work:

```text
quality queue
  -> background worker
  -> verify hashes
  -> embed with mxbai-embed-large
  -> quality sqlite-vec upsert in the quality semantic database role
  -> activate quality only after complete/current
```

If a quality index was active before a file change, watch mode must switch
semantic search back to the fast layer as soon as the quality generation becomes
stale. Default semantic search may return to the quality layer only after the
quality worker completes and activates the latest generation.

Continuous indexing must expose enough metadata for the CLI, TUI, and MCP query
outputs to distinguish these states:

- fast layer current
- quality pending
- quality stale
- quality blocked
- quality failed
- quality current and active
- search fallback to fast because quality is unavailable

## TUI Behavior

- The Indexing view should show a continuous indexing toggle with a clear
  `on` / `off` status label.
- Toggling continuous indexing on should require explicit confirmation the
  first time in a session because it starts an ongoing local job.
- Toggling continuous indexing off should stop watching promptly and leave
  already completed index updates intact.
- The status row should show the latest watch event, latest reindexed file,
  pending debounce state, current indexing state, and any error.
- Because the writer service owns continuous indexing, the TUI should
  refresh watcher status and semantic/index readiness from shared state on an
  interval rather than relying only on in-process events.
- TUI refreshes must stay read-only while continuous indexing is active. They
  must not run SQLite migrations, prune watcher leases, repair vectors, or
  perform quality catch-up from the UI refresh path.
- Continuous indexing must not block query, storage, or diagnostics views.
- Manual indexing should remain available while continuous mode is off.
- If a manual indexing job is running, continuous indexing should queue or
  coalesce file events instead of running concurrent writes.
- When layered semantic indexing is enabled, the TUI should show active semantic
  layer, fast readiness, quality status, quality job counts, and fallback-to-fast
  state without requiring a live sqlite-vec query for deterministic rendering.

## Implementation Boundaries

- `symdex-index` owns watch orchestration, event coalescing, and per-file
  reindex jobs.
- `symdex-index` also owns queueing quality embedding jobs after fast indexing
  updates the latest semantic generation.
- `symdex-core` owns path normalization, ignore decisions, parsing, chunking,
  hashing, symbol extraction, and call extraction.
- `symdex-store` owns SQLite updates, semantic generation state,
  quality-job persistence, sqlite-vec point replacement, and index run metadata.
- `symdex-store` is the persistence boundary, but it does not grant permission
  for every process to write. Write-capable callers must first satisfy the
  single-writer rule for the configured database.
- `symdex-query` owns active semantic layer routing for search. It must not
  decide to use a partial quality layer unless an explicit future diagnostic
  mode is added.
- `symdex-cli` owns argument parsing and a non-interactive watch launch path.
- `symdex-tui` owns toggle state, rendering, confirmation, and event display.
- The TUI and CLI must call shared Rust APIs directly. They must not shell out
  to `symdex` subprocesses.
- MCP evidence tools remain read-only. `symdex_watch_start` is the explicit
  local-only watcher-start exception; MCP does not expose watcher stop.

Current implementation status:

- `symdex-index` exposes shared watch snapshot, diff, and continuous polling
  APIs.
- `symdex-writer` owns the local writer daemon, writer client, job protocol, and
  database-role-keyed IPC endpoints. `symdex-store` still exposes the advisory
  lock primitives, but only writer daemons should acquire them.
- `symdex watch start <repo>` starts or attaches the background watcher and
  prints status. Watchers are client-scoped, so this command alone does not make
  a permanent daemon; without a live TUI, MCP server, or foreground watcher the
  daemon exits after about 10 seconds. `symdex watch status <repo>` reads shared
  SQLite watcher state without running migrations or pruning stale client rows,
  keeping status polling read-only while the watcher writes index updates.
  `symdex watch stop <repo>` asks the structural writer service to stop the
  managed watcher. Watcher status, client attach, heartbeat, and detach metadata
  are stored in the watch database role (`.symdex/symdex-watch.sqlite` by
  default). Client heartbeat and detach jobs are routed to the watch-role writer
  endpoint so long-running structural indexing jobs do not block lease updates.
- `symdex index --watch <repo>` attaches a foreground client to the same
  writer-managed watcher rather than opening a separate SQLite writer.
- Manual `symdex index`, `symdex index-quality`, `vector-repair`, migrations,
  and future write-capable MCP tools submit jobs to the same writer service
  before mutating SQLite or sqlite-vec.
- `symdex serve-mcp --watch <repo>` starts or attaches the single background
  watcher before serving MCP and holds a client lease until the MCP process
  exits. Live TUI/MCP/CLI clients heartbeat their lease and reinsert it if a
  transient stale-client prune removed the row. MCP stdout remains
  protocol-only.
- Continuous batches call the incremental index path so unchanged files are
  skipped by content hash.
- Quality progress and activation are evaluated against the current fast
  embedding manifest. Superseded terminal quality jobs are retained as history
  but do not keep the latest generation stale, and manual quality catch-up queues
  missing current-fast quality jobs before claiming work. Continuous quality
  catch-up also treats current stale quality jobs as work because the worker
  requeues those terminal rows before claiming the next batch.
- Watch-driven batches are recorded with `run_kind = watch` in events-role
  index-run metadata so storage views can distinguish watch updates from manual
  runs without writing those telemetry rows into structural SQLite.
- In semantic watch mode, when `SYMDEX_QUALITY_INDEX` is enabled, watch mode
  automatically runs cooperative quality catch-up after fast batches and during
  idle ticks. Each catch-up tick uses the same hash-verifying quality worker
  path as `symdex index-quality <repo>` and is bounded by
  `SYMDEX_QUALITY_BATCH_SIZE` before returning to watch polling. Quality
  catch-up uses the `quality_semantic` writer lane, so it can continue while the
  structural writer lane is occupied by manual structural work.
- The TUI starts continuous indexing on launch and exposes a `c` toggle to stop
  or confirm restarting watch mode, with explicit `on` / `off` labels, pending
  debounce state, queued event count, last reindexed file, active semantic
  layer, quality status, quality job counts, latest watch and quality errors,
  and an animated activity indicator while continuous indexing is on.
- CLI watch output prints metadata-only quality state, progress, completion,
  and failure events alongside fast watch events.

## Observability

- Log summaries only: event counts, paths, index counts, status labels, and
  errors.
- Do not log source text.
- Record index run summaries in the events database role for continuous indexing
  batches so storage views can show when watch-driven updates occurred.
- Record per-file `file_index_events` for watch batches so created, modified,
  deleted, skipped unchanged, and paths removed from discovery by ignore or
  support rules can be debugged from local events-role metadata.
- Record watcher status and client leases in the watch database role so frequent
  control-plane updates do not compete with structural indexing writes.
- Surface watch health in diagnostics when available, including watcher active
  state and the most recent error.
- Surface quality-layer status separately from fast-layer indexing status.
  Quality failures should be visible but must not make a valid fast layer appear
  unavailable.

## Testing

- Unit test event coalescing and debounce behavior with synthetic paths.
- Unit test that ignored paths, symlink escapes, and out-of-root paths do not
  schedule reindex work.
- Integration test that a created eligible implemented-language file is indexed
  in continuous mode.
- Integration test that a modified eligible implemented-language file replaces
  stale SQLite facts.
- Integration test that unchanged content after a filesystem event is skipped.
- Test that only one process can own write-capable indexing/quality/repair work
  for a repository database at a time, and that TUI/MCP/status/query refreshes
  remain read-only while the writer is active.
- Test offline continuous indexing without sqlite-vec or Ollama.
- Test semantic continuous indexing with mocked or opt-in local Ollama/sqlite-vec.
- Test that continuous indexing marks quality stale, queues quality jobs, and
  returns without waiting for the quality worker.
- Test that default semantic search routes to fast while quality is pending or
  stale after a watch-driven update.
- TUI reducer tests should cover toggle on, confirmation, toggle off, queued
  event display, quality status display, fallback-to-fast display, and error
  state rendering.

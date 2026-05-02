# Layered Semantic Indexing

## Purpose

Symdex should provide fast semantic search immediately after indexing while also
supporting a higher-quality semantic layer that can take longer to build. The
fast layer is the availability path. The quality layer is the eventual precision
path.

This design keeps continuous indexing responsive by using `nomic-embed-text` for
synchronous fast indexing and `nomic-embed-text-v2-moe` for deferred quality
indexing.

## Goals

- Keep manual and continuous indexing fast enough for normal edit loops.
- Make semantic search available as soon as the fast layer is ready.
- Build the quality layer in the background without blocking watch mode.
- Route default semantic search to quality only when quality is complete and
  current for the latest fast semantic generation.
- Fall back to fast search whenever quality is missing, stale, partial, failed,
  blocked, or explicitly disabled.
- Keep SQLite as the source of truth for structural facts and semantic-layer
  readiness.
- Keep Qdrant collections as metadata-only vector projections of embeddable
  chunks.
- Preserve local-only privacy and existing source-text exclusion rules.

## Non-goals

- Do not merge raw scores from fast and quality models by default.
- Do not use partial quality results for default semantic search.
- Do not make `nomic-embed-text-v2-moe` mandatory for normal indexing.
- Do not block continuous indexing on quality embedding work.
- Do not store source text in Qdrant payloads or long-lived quality job rows.
- Do not execute indexed repository code.

## Semantic layers

| Layer | Model | Timing | Role |
|---|---|---|---|
| `fast` | `nomic-embed-text` | synchronous | availability, broad recall, continuous indexing |
| `quality` | `nomic-embed-text-v2-moe` | deferred/background | higher precision semantic search |

The fast layer must be updated during normal semantic indexing. The quality
layer must be built from the latest SQLite structural facts and verified source
hashes after the fast generation is available.

## Source of truth

SQLite owns:

- files
- chunks
- symbols
- calls
- tests
- content hashes
- text hashes
- index runs
- semantic generation state
- semantic layer readiness
- per-layer embedding manifests
- quality job state

Qdrant owns:

- dense vectors
- metadata-only payloads
- one collection per repository/model layer

Qdrant payloads must continue to exclude source text.

## Collection naming

Use model-derived Qdrant collection names so incompatible models never share a
collection:

```text
symdex_<repository_id>_<embedding_model_slug>
```

Typical collections:

```text
symdex_<repo_id>_nomic_embed_text
symdex_<repo_id>_nomic_embed_text_v2_moe
```

If a model reports a different vector dimension for an existing collection,
fail closed for that layer and require repair, reset, migration, or reindex.
A quality-layer failure must not disable a valid fast layer.

## Semantic generation lifecycle

A semantic generation represents one coherent fast semantic index projection for
the current SQLite structural facts.

State transitions:

```text
missing
  -> fast_ready
  -> quality_pending
  -> quality_ready
  -> quality_stale
  -> quality_pending
  -> quality_ready
```

Failure states:

```text
quality_blocked
quality_failed
```

Search routing:

| State | Default search layer | Notes |
|---|---|---|
| `missing` | none | semantic search unavailable |
| `fast_ready` | fast | fast vectors are current |
| `quality_pending` | fast | quality build incomplete |
| `quality_ready` | quality | quality vectors are complete/current |
| `quality_stale` | fast | quality no longer matches latest fast generation |
| `quality_blocked` | fast | quality model/service unavailable |
| `quality_failed` | fast | quality job failed; fast remains usable |

Default semantic search must never silently query a partial quality layer.

## Activation rule

The quality layer may become active only when all of the following are true:

- The quality generation ID matches the latest completed fast generation ID.
- The quality embedding model is `nomic-embed-text-v2-moe` unless explicitly
  overridden by configuration.
- The quality vector dimension is known and stable for that model/collection.
- Every current embeddable chunk has a current quality embedding row.
- No current quality jobs for the generation are pending or running.
- The latest generation has no `failed` or `skipped_stale` quality jobs.
- A `quality_blocked` generation stays blocked until a later queue retry can
  produce complete quality coverage.

Activation must be atomic from the query layer's perspective: a search should
see either active `fast` or active `quality`, never an in-between state.
Current activation is gated by SQLite generation metadata, quality job state,
and the per-layer `chunk_embeddings` manifest. Layer-aware Qdrant manifest
verification is deferred to the verify/repair slice; activation does not return
source text or Qdrant vectors.

## Manual indexing behavior

`semantic index` terminology below means the current non-offline indexing path.

```text
symdex index <repo>
```

Required behavior:

1. Discover files, apply ignore rules, and compute file hashes.
2. Parse changed files with tree-sitter.
3. Persist structural facts to SQLite.
4. Embed current embeddable chunks with the fast model.
5. Upsert fast vectors into the fast Qdrant collection.
6. Record a new fast semantic generation.
7. Mark any previous active quality generation stale if the structural snapshot
   changed.
8. Queue quality jobs for current embeddable chunks when quality indexing is
   enabled.
9. Return without waiting for quality completion.

Manual indexing may offer a flag to wait for quality work, but this must be
explicit. The default path should preserve fast availability.

## Continuous indexing behavior

Continuous indexing must stay responsive. Watch-driven batches must perform the
same structural and fast semantic update path as manual indexing, then enqueue
quality work and return.

When a file changes:

1. Debounce and coalesce filesystem events.
2. Reindex changed files through normal parser, chunker, symbol, call,
   secret-detection, SQLite, fast embedding, and fast Qdrant paths.
3. Record a new fast semantic generation if embeddable content changed.
4. Switch active semantic layer to `fast` if the previous quality generation is
   no longer current.
5. Mark stale quality jobs and embeddings for changed/deleted chunks.
6. Queue replacement quality jobs.
7. Do not wait for `nomic-embed-text-v2-moe`.

This keeps continuous indexing latency bounded by the fast layer and structural
work. Quality indexing may lag behind active edits.

The implemented watch path performs cooperative quality catch-up when semantic
watch mode is active and quality indexing is enabled. After a fast watch batch
completes, and on later idle ticks, watch mode processes at most
`SYMDEX_QUALITY_BATCH_SIZE` quality jobs through the same hash-verifying worker
used by `symdex index-quality <repo>`. If more work remains, later idle ticks
continue the catch-up. CLI and TUI events report active layer, quality status,
activation reason, and job counts without exposing source text.

## Quality worker behavior

The quality worker may be an explicit CLI command, a background task started by
watch mode, a TUI-triggered local job, or a shared library API. All entry points
must use the same orchestration logic.

Worker loop:

```text
while enabled:
  if a manual or watch fast-index job is active:
    pause or yield

  claim a small batch of pending quality jobs

  for each job:
    reload source file from disk
    verify file content hash still matches the queued job
    extract chunk text from stored byte range
    verify chunk text hash still matches the queued job
    embed with nomic-embed-text-v2-moe
    upsert into quality Qdrant collection
    record chunk_embeddings row for the quality layer
    mark job succeeded

  if failures remain and no pending/running work remains:
    mark the generation quality_failed
```

The worker must never trust stale queued source text. It should store metadata
and re-read source files only long enough to embed current chunks.

The implemented manual entry point is:

```text
symdex index-quality <repo>
```

It drains all pending jobs for the latest semantic generation by repeatedly
claiming bounded batches using `SYMDEX_QUALITY_BATCH_SIZE`. It writes quality
Qdrant points and quality `chunk_embeddings` rows, records the first successful
quality dimension on the generation, refreshes SQLite activation state, and
reports the resulting `quality_status`, `active_layer`, and activation reason.
Default search remains on `fast` until the latest generation has complete
quality coverage and no pending, running, failed, or stale quality jobs; only
then does the worker atomically switch `active_layer` to `quality`.

## Job staleness

A quality job is stale when any of these are true:

- Its generation ID is not the latest fast generation ID.
- Its file content hash no longer matches the current `files.content_hash`.
- Its chunk text hash no longer matches the current `chunks.text_hash`.
- Its chunk no longer exists.
- The chunk now has an `excluded_reason`.

Stale pending or running jobs should transition to `skipped_stale` and must not
activate quality. Terminal job history such as `succeeded`, `failed`,
`skipped_stale`, and `skipped_excluded` is preserved when a new fast generation
supersedes older quality work.

If `skipped_stale` exists on the latest generation, activation keeps
`quality_status = quality_pending` and `active_layer = fast`. A later fast
generation and quality queue pass must produce a clean, complete manifest before
quality can become active.

If quality indexing is enabled but the configured quality model or local service
is unavailable during queue readiness checks, mark the latest semantic
generation `quality_blocked` and do not create pending quality job rows. A later
retry can re-check readiness and enqueue jobs for the latest fast generation.

## Schema direction

The existing `chunks.qdrant_point_id`, `chunks.embedding_model`,
`chunks.embedding_dimension`, and `chunks.embedded_at` fields are sufficient for
one semantic layer but not for two. Layered indexing should move toward a
separate embedding manifest table.

Recommended table:

```sql
CREATE TABLE chunk_embeddings (
  id TEXT PRIMARY KEY,
  repository_id TEXT NOT NULL,
  file_id TEXT NOT NULL,
  chunk_id TEXT NOT NULL,
  semantic_layer TEXT NOT NULL,
  embedding_model TEXT NOT NULL,
  embedding_dimension INTEGER NOT NULL,
  content_hash TEXT NOT NULL,
  text_hash TEXT NOT NULL,
  qdrant_collection TEXT NOT NULL,
  qdrant_point_id TEXT NOT NULL,
  generation_id TEXT NOT NULL,
  embedded_at TEXT NOT NULL,
  status TEXT NOT NULL DEFAULT 'current',
  UNIQUE(chunk_id, semantic_layer, embedding_model, embedding_dimension)
);
```

Recommended generation table:

```sql
CREATE TABLE semantic_generations (
  id TEXT PRIMARY KEY,
  repository_id TEXT NOT NULL,
  fast_model TEXT NOT NULL,
  fast_dimension INTEGER NOT NULL,
  fast_completed_at TEXT NOT NULL,
  quality_model TEXT,
  quality_dimension INTEGER,
  quality_status TEXT NOT NULL,
  quality_started_at TEXT,
  quality_completed_at TEXT,
  active_layer TEXT NOT NULL,
  files_seen INTEGER NOT NULL,
  embeddable_chunks INTEGER NOT NULL,
  fast_embedded_chunks INTEGER NOT NULL,
  quality_embedded_chunks INTEGER NOT NULL,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);
```

Recommended job table:

```sql
CREATE TABLE quality_embedding_jobs (
  id TEXT PRIMARY KEY,
  repository_id TEXT NOT NULL,
  generation_id TEXT NOT NULL,
  chunk_id TEXT NOT NULL,
  file_id TEXT NOT NULL,
  path TEXT NOT NULL,
  content_hash TEXT NOT NULL,
  text_hash TEXT NOT NULL,
  status TEXT NOT NULL,
  attempts INTEGER NOT NULL DEFAULT 0,
  error_summary TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  UNIQUE(repository_id, generation_id, chunk_id)
);
```

Recommended statuses:

```text
pending
running
succeeded
failed
skipped_stale
skipped_excluded
```

## Query routing

The query layer should ask SQLite for the active semantic layer before embedding
the user query.

Default routing:

```text
if latest semantic generation active_layer == quality
  and quality_status == quality_ready
   and quality manifest is complete:
     embed query with quality model
     search quality collection
else:
     embed query with fast model
     search fast collection
```

The shared query library resolves this from SQLite using the latest semantic
generation plus compact `chunk_embeddings` coverage summaries. If no semantic
generation exists, auto and forced-fast routing fall back to the configured fast
model and collection naming convention for compatibility with older indexes.

Search summaries should expose:

- `semantic_layer`
- `embedding_model`
- `qdrant_collection`
- `quality_status`
- `generation_id`
- whether the query fell back to fast because quality was unavailable

Planned CLI flags:

```text
--semantic-layer auto     # default
--semantic-layer fast     # force fast
--semantic-layer quality  # require current quality, fail if unavailable
```

Do not implement partial-quality search as the default. If added later, it must
be an explicit diagnostic or experimental mode.

## Verification and repair

Qdrant verification should become layer-aware:

```text
symdex qdrant-verify <repo> --semantic-layer fast
symdex qdrant-verify <repo> --semantic-layer quality
symdex qdrant-verify <repo> --all-semantic-layers
```

Expected manifests should come from `chunk_embeddings` for the selected layer.
The old `chunks.qdrant_point_id` path may remain as a migration compatibility
path until the layered schema is fully adopted.

Repair should rebuild missing or stale points through the normal indexing path
for the selected layer instead of writing ad-hoc Qdrant payloads.

## Configuration

Recommended defaults:

```text
fast model: nomic-embed-text
quality model: nomic-embed-text-v2-moe
quality indexing: enabled when model is available, otherwise blocked/degraded
quality workers: 1
quality batch size: small, e.g. 8-16 chunks
pause quality while fast indexing: true
```

Suggested environment variables:

```text
SYMDEX_FAST_EMBED_MODEL=nomic-embed-text
SYMDEX_QUALITY_EMBED_MODEL=nomic-embed-text-v2-moe
SYMDEX_QUALITY_INDEX=1
SYMDEX_QUALITY_BATCH_SIZE=16
SYMDEX_QUALITY_WORKERS=1
```

Keep existing `SYMDEX_EMBED_MODEL` behavior as a compatibility path until the
layered runtime paths are fully implemented. The compatibility variable remains
the current single-model setting and the fallback for the fast model when
`SYMDEX_FAST_EMBED_MODEL` is unset. The quality model is configured separately
through `SYMDEX_QUALITY_EMBED_MODEL` and defaults to
`nomic-embed-text-v2-moe`. Avoid breaking existing single-model workflows during
the migration.

## CLI and TUI surface

Recommended commands:

```text
symdex index <repo>
symdex index <repo> --no-quality
symdex index-quality <repo>
symdex semantic-status <repo>
symdex search <repo> <query> --semantic-layer auto
symdex search <repo> <query> --semantic-layer fast
symdex search <repo> <query> --semantic-layer quality
symdex qdrant-verify <repo> --semantic-layer fast
symdex qdrant-verify <repo> --semantic-layer quality
```

The TUI should show:

- active layer: fast or quality
- fast model/dimension/collection
- quality model/dimension/collection
- quality status: pending, ready/current, stale, blocked, failed
- quality job counts by status
- latest quality error summary
- fallback-to-fast indicator when quality is not active

## Failure behavior

| Failure | Required behavior |
|---|---|
| quality model missing | mark quality `blocked`; keep fast active |
| quality embedding fails | mark job `failed`; keep fast active |
| quality Qdrant collection missing | recreate during quality worker if possible |
| quality dimension drift | fail quality layer closed; keep fast active |
| file changes during quality job | skip stale job; keep fast active |
| partial quality index exists | do not use by default |
| fast index fails | do not claim quality is current for new changes |

## Privacy and safety

- Do not store source text in Qdrant payloads.
- Do not store source text in quality job rows.
- Do not log source text from quality jobs.
- Continue to exclude secret-like chunks from all semantic layers.
- Never execute repository code during fast or quality indexing.
- Re-read files only for hash verification and transient embedding input.

## Testing requirements

Add tests for:

- fast generation creation after semantic indexing
- quality jobs queued after fast indexing
- quality worker skips stale jobs after file hash changes
- default search routes to fast while quality is pending
- default search routes to quality only after complete activation
- default search falls back to fast when quality is stale, blocked, failed, or partial
- forced quality search fails when quality is unavailable
- continuous indexing marks quality stale and returns without waiting for quality
- layer-aware Qdrant collection naming and manifests
- model dimension drift isolated to the affected layer
- metadata-only output for semantic layer status

Service-dependent Ollama/Qdrant behavior should remain opt-in. Job selection,
state transitions, routing, and activation rules should be unit-testable without
live services.

## Implementation order

1. Add semantic-layer domain types and config defaults.
2. Add SQLite migration tables for `chunk_embeddings`, `semantic_generations`,
   and `quality_embedding_jobs`.
3. Keep current single-model indexing behavior working while also recording fast
   layer metadata.
4. Add active-layer query routing and status output while still using fast only.
5. Add quality job queue creation after fast indexing.
6. Add manual `index-quality` worker path.
7. Add automatic background quality worker for watch/manual flows if enabled.
8. Add layer-aware verification and repair.
9. Update TUI and MCP outputs to expose active layer and quality status.

Each slice should keep tests passing and preserve local-only behavior.

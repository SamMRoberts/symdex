# Layered Semantic Indexing Tasks

This task plan breaks `docs/LAYERED_SEMANTIC_INDEXING.md` into small slices that
coding agents can implement safely. Keep each slice testable and avoid changing
query routing, storage schema, and background worker behavior in one large PR.

## Slice 1 — Semantic layer domain types

Goal: introduce vocabulary without changing behavior.

Tasks:

- Add `SemanticLayer` enum with `Fast` and `Quality` variants.
- Add `SemanticLayerStatus` values for `missing`, `fast_ready`,
  `quality_pending`, `quality_ready`, `quality_stale`, `quality_blocked`, and
  `quality_failed`.
- Add config helpers for:
  - fast model default: `nomic-embed-text`
  - quality model default: `mxbai-embed-large`
  - quality enabled flag
  - quality batch size
  - quality worker count
- Preserve `SYMDEX_EMBED_MODEL` compatibility for the current single-model path.
- Add unit tests for config defaults and env overrides.

Acceptance:

- Existing indexing/search behavior is unchanged.
- New types are available to index/query/store layers.
- Tests pass without Ollama or sqlite-vec.

## Slice 2 — SQLite schema migration

Goal: add persistence for multiple semantic layers while preserving existing
single-layer data.

Tasks:

- Add `chunk_embeddings` table.
- Add `semantic_generations` table.
- Add `quality_embedding_jobs` table.
- Add indexes for:
  - embeddings by repository/layer/model
  - embeddings by generation/chunk
  - jobs by repository/generation/status
  - latest generation by repository
- Keep current `chunks.vector_point_id` fields as nullable compatibility schema
  until migration is fully adopted.
- Add migration tests.

Acceptance:

- Existing local databases migrate idempotently.
- Old query/index paths still work.
- New tables can be read/written in isolated store tests.

## Slice 3 — Fast generation recording

Goal: keep current fast indexing behavior but record semantic generation state.

Tasks:

- Treat current `nomic-embed-text` semantic indexing as the `fast` layer.
- After a successful fast sqlite-vec upsert, create/update the latest
  `semantic_generations` row.
- Record fast model, dimension, chunk count, and active layer.
- Record fast `chunk_embeddings` rows for current embeddable chunks.
- Initially keep existing `chunks.vector_point_id` writes for compatibility.

Acceptance:

- `symdex index <repo>` still produces fast semantic search.
- Store tests can verify latest generation is `fast_ready`.
- Repeated indexing remains idempotent for unchanged chunks.

## Slice 4 — Active-layer query routing

Goal: make query routing consult SQLite readiness state while still usually
routing to fast.

Status: implemented in the shared store/query library. The CLI `--semantic-layer`
flag remains deferred so text output and MCP-backed JSON output can be kept in
parity when it is exposed.

Tasks:

- Add store query for active semantic layer and generation status.
- Update semantic search orchestration to choose model/collection from active
  layer instead of raw `SYMDEX_EMBED_MODEL` only.
- Add `--semantic-layer auto|fast|quality` to CLI search if practical in this
  slice; otherwise expose the lower-level library enum first.
- Include semantic layer metadata in semantic search summaries:
  - active layer
  - embedding model
  - collection
  - generation ID
  - quality status
  - fallback reason when applicable

Acceptance:

- Default search uses fast when quality is missing or not current.
- Forced fast search uses fast.
- Forced quality search fails clearly when quality is unavailable.
- Search output remains source-text-free.

## Slice 5 — Quality job queue creation

Goal: queue quality jobs after fast indexing without running the quality worker.

Status: implemented for the manual/continuous shared fast indexing path.
Blocked quality readiness creates no pending job rows, superseded stale marking
updates only old `pending` and `running` jobs, and `quality_dimension` remains
unset until a future worker records real quality embeddings.

Tasks:

- After fast indexing completes, enqueue jobs for current embeddable chunks.
- Do not enqueue chunks with `excluded_reason`.
- Dedupe by repository, generation, and chunk.
- Mark old jobs stale when generation changes.
- Mark quality status as `quality_pending` when jobs are queued.
- Mark quality as `quality_blocked` when quality indexing is enabled but the
  model is unavailable during readiness checks.

Acceptance:

- Fast indexing remains synchronous and unchanged from a user latency
  standpoint.
- Quality jobs are visible in SQLite.
- Default search still routes to fast while jobs are pending.

## Slice 6 — Manual quality worker

Status: implemented.

Goal: implement an explicit `index-quality` path before automatic background
execution.

Tasks:

- Add shared library function to process quality jobs for one repository.
- Add CLI command such as `symdex index-quality <repo>`.
- Worker must:
  - claim a bounded batch
  - re-read files from disk
  - verify file content hash
  - extract chunk text by byte range
  - verify chunk text hash
  - embed with `mxbai-embed-large`
  - upsert quality sqlite-vec point
  - write `chunk_embeddings` row for quality
  - mark job succeeded or failed
- Stale jobs become `skipped_stale` and are not embedded.

Acceptance:

- Worker can complete quality embeddings for a stable repo.
- Worker skips stale jobs after file changes.
- Fast search remains active until activation rules pass.

## Slice 7 — Quality activation and fallback

Goal: default search shifts to quality only when quality is complete/current.

Tasks:

- Count current embeddable chunks for the latest fast generation.
- Count current quality embeddings for the same generation.
- Verify no pending/running jobs remain for that generation.
- Defer layer-aware sqlite-vec manifest verification to Slice 9 verify/repair work.
- Atomically set active layer to `quality` when complete.
- Set active layer back to `fast` when a new fast generation makes quality
  stale.

Implementation note: Slice 7 activation is SQLite-gated. It uses the latest
semantic generation, quality job counts, and per-layer `chunk_embeddings`
coverage to decide whether quality can become active.

Acceptance:

- Default search uses fast while quality is partial.
- Default search uses quality after full completion.
- Any file change switches default search back to fast until quality catches up.

## Slice 8 — Continuous indexing integration

Goal: keep watch mode fast while preserving quality eventual consistency.

Tasks:

- Ensure watch-driven fast indexing queues quality jobs and returns promptly.
- Mark quality stale on changed/deleted embeddable chunks.
- Add optional background worker execution when watch mode is active.
- Pause/yield quality work while a fast manual/watch index run is active.
- Surface watch events and quality state separately.

Implementation note: Slice 8 uses the shared incremental fast indexing path for
watch batches, records watch batches with `run_kind = watch`, and performs
cooperative quality catch-up in bounded `SYMDEX_QUALITY_BATCH_SIZE` batches
after fast watch batches and during idle watch ticks.

Acceptance:

- Continuous indexing does not wait for `mxbai-embed-large`.
- Default search falls back to fast immediately after watched changes.
- Quality can catch up after edits stop.

## Slice 9 — Layer-aware verify and repair

Goal: make maintenance commands understand both semantic layers.

Tasks:

- Add `--semantic-layer fast|quality|all` to verify/repair commands.
- Build expected manifests from `chunk_embeddings` for the selected layer.
- Build expected manifests from layered metadata and isolate quality health from
  fast health.
- Repair missing/stale quality points through the quality worker path, not a
  separate ad-hoc sqlite-vec write path.
- Keep orphan cleanup metadata-only.

Implementation note: Slice 9 adds `--semantic-layer fast|quality|all` to
`vector-verify` and `vector-repair`. Verification reads current expected points
from latest-generation `chunk_embeddings` for the selected layer. Slice Q10
removed the fast-layer fallback to legacy `chunks.vector_point_id`; legacy-only
local databases must run `symdex index <repo>` to create layered manifests.
Repair dispatches fast work through normal semantic indexing and quality work
through the quality worker path.

Acceptance:

- Fast and quality collections can be verified independently.
- Missing quality points do not mark fast layer unhealthy.
- Dimension drift fails closed only for the affected layer.

## Slice 10 — TUI and MCP status exposure

Goal: make active layer and quality readiness visible to users and agents.

Implementation note: Slice 10 adds a shared query-layer semantic status summary
and exposes it through `symdex semantic-status <repo>`, compact TUI Overview
rows, and additive MCP `symdex_search` metadata. MCP search now uses the routed
query-layer semantic search path instead of a separate fast-model-only path.

Tasks:

- Add semantic status summary API.
- Add CLI command such as `symdex semantic-status <repo>`.
- Add TUI rows for:
  - active semantic layer
  - fast model/dimension/collection
  - quality model/dimension/collection
  - quality job counts
  - quality status and latest error
  - fallback-to-fast reason
- Add MCP semantic outputs fields for active layer, model, collection,
  generation, quality status, and fallback reason.

Acceptance:

- Users can tell whether search is fast or quality-backed.
- Agents can avoid over-trusting fast fallback results.
- No source text is exposed.

## Testing rules for every slice

- Unit-test state transitions without live Ollama/sqlite-vec.
- Keep service-dependent Ollama/sqlite-vec tests opt-in.
- Add migration tests for every schema change.
- Add regression tests for source-text exclusion in outputs and payloads.
- Keep existing single-model workflows working until a deliberate migration
  removes compatibility fields.

## Do not combine initially

Avoid combining these in a single PR unless the user explicitly requests it:

- schema migration + worker + query routing
- continuous indexing + quality worker activation
- TUI/MCP status + storage model rewrite
- repair command changes + activation rules

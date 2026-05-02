# Layered Semantic Indexing Backlog

This backlog is specific to the `quality-index` branch. It supplements
`BACKLOG.md` and the implementation slices in
`LAYERED_SEMANTIC_INDEXING_TASKS.md`.

## Phase Q1 — Design and harness alignment

- [x] Add repo-level AGENTS.md guidance for fast/quality semantic layers.
- [x] Add authoritative layered semantic indexing design doc.
- [x] Add implementation task breakdown.
- [x] Link layered indexing from the docs index.
- [x] Update continuous-indexing docs to state quality indexing is deferred.
- [x] Update architecture docs with crate boundaries for active-layer routing,
  semantic generation state, and quality job orchestration.

## Phase Q2 — Domain and configuration foundation

- [x] Add semantic layer domain types: `fast`, `quality`, and active-layer mode.
- [x] Add semantic generation status values for `missing`, `fast_ready`,
  `quality_pending`, `quality_ready`, `quality_stale`, `quality_blocked`, and
  `quality_failed` states.
- [x] Add layer-specific model configuration with defaults:
  - fast: `nomic-embed-text`
  - quality: `nomic-embed-text-v2-moe`
- [x] Preserve current `SYMDEX_EMBED_MODEL` behavior for compatibility.
- [x] Add tests for config defaults, env overrides, and status transitions.

## Phase Q3 — SQLite storage model

- [x] Add `chunk_embeddings` migration.
- [x] Add `semantic_generations` migration.
- [x] Add `quality_embedding_jobs` migration.
- [x] Add indexes for generation lookup, job status, and per-layer manifests.
- [x] Keep legacy chunk embedding columns working during migration.
- [x] Add migration and store tests.

## Phase Q4 — Fast-layer generation tracking

- [x] Treat existing semantic indexing as the `fast` layer.
- [x] Record fast semantic generation rows after successful fast Qdrant upsert.
- [x] Record fast `chunk_embeddings` rows for embeddable chunks.
- [x] Make unchanged fast indexing idempotent.
- [x] Keep semantic search behavior unchanged except for added metadata.

## Phase Q5 — Active-layer query routing

- [x] Add store API for active semantic layer and quality status.
- [x] Route default semantic search through active-layer metadata.
- [x] Add forced search modes: auto, fast, quality.
- [x] Fail clearly when forced quality search is requested but quality is not
  current.
- [x] Include active layer, model, collection, generation, quality status, and
  fallback reason in semantic search summaries.

## Phase Q6 — Quality queue and manual worker

- [x] Queue quality jobs after successful fast indexing.
- [x] Exclude secret-blocked and too-large chunks from quality jobs.
- [x] Mark old jobs stale when a new fast generation supersedes them.
- [x] Add manual `symdex index-quality <repo>` command.
- [x] Implement worker hash verification before embedding.
- [x] Upsert quality vectors to the quality Qdrant collection.
- [x] Record quality `chunk_embeddings` rows.

## Phase Q7 — Quality activation

- [ ] Activate quality only when the latest fast generation has complete quality
  coverage.
- [ ] Keep default search on fast while quality is partial, stale, blocked, or
  failed.
- [ ] Switch default search back to fast immediately after new fast generation
  changes make quality stale.
- [ ] Add tests for complete, partial, stale, blocked, and failed quality states.

## Phase Q8 — Continuous indexing integration

- [ ] Ensure watch batches update fast synchronously and queue quality work.
- [ ] Ensure watch batches do not wait for `nomic-embed-text-v2-moe`.
- [ ] Mark quality stale on changed/deleted embeddable chunks.
- [ ] Add optional background quality worker behavior for watch mode.
- [ ] Surface fast/quality state in watch events and TUI state.

## Phase Q9 — Verification, repair, TUI, and MCP

- [ ] Add layer-aware Qdrant verify and repair flags.
- [ ] Build expected manifests from `chunk_embeddings`.
- [ ] Keep missing quality points isolated from fast-layer health.
- [ ] Add `semantic-status` CLI output.
- [ ] Add TUI active-layer and quality-status display.
- [ ] Add MCP semantic output metadata for active layer and fallback status.

## Phase Q10 — Cleanup and compatibility removal

- [ ] Audit old `chunks.qdrant_point_id` compatibility behavior.
- [ ] Decide whether to keep compatibility fields, migrate them, or deprecate
  them after layered manifests are stable.
- [ ] Update docs once implementation behavior replaces planned behavior.
- [ ] Add release notes for local database migration impact.

## Guardrails

- Do not route default search to partial quality results.
- Do not block continuous indexing on quality work.
- Do not store source text in Qdrant or long-lived job rows.
- Do not mix fast and quality vectors in one Qdrant collection.
- Do not make quality failures degrade valid fast search.

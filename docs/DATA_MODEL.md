# Data Model

## SQLite tables

Initial schema names are stable enough for early implementation but may change before release.

Current implementation runs idempotent SQLite migrations at `symdex init`,
`symdex index`, and `symdex index-status`. It creates all tables listed below,
while the current indexing write path persists repositories, files, chunks,
symbols, calls, tests, and legacy chunk embedding provenance. Layered semantic
tables are present for generation manifests and quality work metadata, but the
current indexing path continues to use legacy chunk embedding columns until
fast-layer generation tracking is wired in.

Migrations also create indexes for large-repo query paths: repository file
lookups, chunk-by-file cleanup, symbol name and qualified-name lookup,
caller/callee traversal, and index-run metadata checks.
Layered semantic indexes cover latest generation lookup, per-layer embedding
manifests, and quality job status scans.

### `repositories`

```sql
CREATE TABLE repositories (
  id TEXT PRIMARY KEY,
  root_path TEXT NOT NULL UNIQUE,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);
```

### `index_runs`

```sql
CREATE TABLE index_runs (
  id TEXT PRIMARY KEY,
  repository_id TEXT NOT NULL,
  started_at TEXT NOT NULL,
  finished_at TEXT,
  status TEXT NOT NULL,
  embedding_model TEXT NOT NULL,
  embedding_dimension INTEGER,
  files_seen INTEGER DEFAULT 0,
  files_indexed INTEGER DEFAULT 0,
  chunks_embedded INTEGER DEFAULT 0,
  error_summary TEXT,
  parser_version TEXT,
  indexer_version TEXT,
  run_kind TEXT NOT NULL DEFAULT 'manual'
);
```

Indexing records a row when a run starts and finalizes it when the run finishes.
Run status values are `running`, `success`, `skipped`, `partial`, and `failed`.
Successful semantic runs include the embedding model, vector dimension, and
embedded chunk count. Semantic runs with no changed embeddable chunks finish as
`skipped`. Semantic failures after SQLite persistence finish as `partial` with a
metadata-only `error_summary`; earlier recorded failures finish as `failed`.
`index-status` and `symdex_index_status` expose the latest successful embedding
model and dimension when present.

Continuous indexing records compact batch summaries in `index_runs` through the
same indexing path, so watch-driven updates are visible in storage views. The UI
can distinguish manual/offline and semantic batches through `run_kind`, status,
timestamps, files seen/indexed, chunks embedded, model, dimension, and any
metadata-only error summary.

### `files`

```sql
CREATE TABLE files (
  id TEXT PRIMARY KEY,
  repository_id TEXT NOT NULL,
  path TEXT NOT NULL,
  language TEXT NOT NULL,
  content_hash TEXT NOT NULL,
  indexed_at TEXT NOT NULL,
  index_run_id TEXT,
  parser_version TEXT,
  UNIQUE(repository_id, path)
);
```

`language` stores a stable language slug such as `rust`, `csharp`,
`javascript`, or `typescript`. The schema is intentionally language-neutral; no
table change is required when adding C#, JavaScript, TypeScript, or future
languages that follow the same evidence contracts.

### `symbols`

```sql
CREATE TABLE symbols (
  id TEXT PRIMARY KEY,
  file_id TEXT NOT NULL,
  parent_symbol_id TEXT,
  name TEXT NOT NULL,
  qualified_name TEXT,
  kind TEXT NOT NULL,
  signature TEXT,
  start_line INTEGER NOT NULL,
  end_line INTEGER NOT NULL,
  start_byte INTEGER NOT NULL,
  end_byte INTEGER NOT NULL,
  index_run_id TEXT,
  parser_version TEXT
);
```

### `chunks`

```sql
CREATE TABLE chunks (
  id TEXT PRIMARY KEY,
  file_id TEXT NOT NULL,
  symbol_id TEXT,
  kind TEXT NOT NULL,
  text_hash TEXT NOT NULL,
  start_line INTEGER NOT NULL,
  end_line INTEGER NOT NULL,
  start_byte INTEGER NOT NULL,
  end_byte INTEGER NOT NULL,
  qdrant_point_id TEXT,
  excluded_reason TEXT,
  index_run_id TEXT,
  parser_version TEXT,
  embedding_model TEXT,
  embedding_dimension INTEGER,
  embedded_at TEXT
);
```

`excluded_reason` is set when a chunk is kept as metadata but withheld from
embedding. Chunks with an exclusion reason do not get a Qdrant point ID in the
current implementation.

Before semantic indexing replaces changed-file chunk rows or removes deleted
files, it reads existing non-null `qdrant_point_id` values for those chunks and
uses them to delete stale Qdrant points. This keeps SQLite as the source of
truth for vector lifecycle cleanup while avoiding source text in Qdrant payloads
or cleanup reports.

### `calls`

```sql
CREATE TABLE calls (
  id TEXT PRIMARY KEY,
  caller_symbol_id TEXT NOT NULL,
  callee_text TEXT NOT NULL,
  callee_symbol_id TEXT,
  call_line INTEGER NOT NULL,
  confidence REAL NOT NULL,
  resolution_status TEXT NOT NULL,
  index_run_id TEXT,
  parser_version TEXT
);
```

### `tests`

```sql
CREATE TABLE tests (
  id TEXT PRIMARY KEY,
  repository_id TEXT NOT NULL,
  file_id TEXT NOT NULL,
  path TEXT NOT NULL,
  symbol_id TEXT,
  name TEXT NOT NULL,
  qualified_name TEXT NOT NULL,
  framework TEXT NOT NULL,
  language TEXT NOT NULL,
  start_line INTEGER NOT NULL,
  end_line INTEGER NOT NULL,
  start_byte INTEGER NOT NULL,
  end_byte INTEGER NOT NULL,
  index_run_id TEXT,
  parser_version TEXT,
  indexed_at TEXT NOT NULL
);
```

The current write path stores Rust, C#, JavaScript, and TypeScript test
metadata discovered from parser evidence. Test rows are structural facts only:
they include names, framework labels, paths, ranges, provenance, and optional
symbol linkage, but no source text. `symbol_id` is present for symbol-backed
tests such as Rust functions, C# methods, and unambiguous JavaScript or
TypeScript named callbacks. It is absent for metadata-only callback-style tests,
such as inline Jest, Vitest, or Mocha callbacks. The `tests` table supports
exact/suffix failing-test name lookup for debug context packs and direct
test-to-symbol call lookup for impact summaries.

### `semantic_generations`

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

This table tracks metadata for a semantic generation. `active_layer` is `fast`
or `quality`. `quality_status` uses the layered semantic status vocabulary:
`missing`, `fast_ready`, `quality_pending`, `quality_ready`, `quality_stale`,
`quality_blocked`, or `quality_failed`. Rows do not contain source text.

### `chunk_embeddings`

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

This table is the planned per-layer vector manifest. It keeps fast and quality
metadata separate by `semantic_layer`, model, dimension, generation, collection,
and point ID so the two layers do not share one Qdrant collection. `status` is
metadata-only and currently supports `current`, `stale`, `blocked`, and
`failed`.

### `quality_embedding_jobs`

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

Quality jobs are long-lived metadata rows for deferred quality embedding work.
They include enough hashes and path metadata to detect stale work before
embedding, but never include source text. Current status values are `pending`,
`running`, `succeeded`, `failed`, `skipped_stale`, and `skipped_excluded`.

Provenance columns are nullable for compatibility with existing local SQLite
databases. New indexing writes `index_run_id` and parser version metadata for
files, chunks, symbols, and calls. Semantic indexing also fills chunk embedding
model, dimension, and embedding timestamp metadata after vector upsert.
`parser_version` must include the per-language parser identity and symdex
indexer/chunker version so mixed-language indexes remain auditable.

## Qdrant collection

Collection name pattern:

```text
symdex_<repository_id>_<embedding_model_slug>
```

The current slugger lowercases ASCII alphanumerics and converts other
characters to underscores so generated collection names are safe for REST paths.

Payload fields:

- `repository_id`
- `file_id`
- `chunk_id`
- `symbol_id`
- `symbol_name`
- `path`
- `language`
- `chunk_kind`
- `start_line`
- `end_line`
- `text_hash`
- `parser_version`
- `content_hash`
- `index_run_id`
- `embedding_model`
- `embedding_dimension`
- `indexed_at`

Do not store source text in Qdrant payloads.

Returned evidence rows now include compact provenance metadata where available:
content hash, index run ID, parser version, indexed timestamp, embedding model,
embedding dimension, and embedding timestamp. Freshness checks compare persisted
content hashes with the current eligible file hashes and label rows as `fresh`,
`stale`, `deleted`, `missing`, or `unknown`.

Returned impact and debug-context evidence also includes an `EvidenceTrust`
score where the query layer has enough metadata to evaluate it. The score is a
deterministic 0.0-1.0 heuristic over freshness, provenance completeness,
confidence when the evidence is call or semantic evidence, and index metadata
completeness from `index_run_id` plus `parser_version`. Trust levels are
`high`, `medium`, `low`, and `minimal`. The score is not a correctness proof;
it is a compact ordering aid for agents deciding which metadata-only evidence is
fresh, well-provenanced, and directly supported by indexed facts.

Query-time evidence rows also include compact `reasons` tags where available.
These tags explain why the row was returned without adding source text or new
storage schema. Examples include `semantic_vector_match`,
`relationship:direct_caller`, `bounded_transitive_call_path`,
`symbols_at_runtime_location`, `symbol_name_fallback_match`, and
`related_file_from_call_evidence`.

Unified context packs are also query-time derived metadata. Structural
context-pack mode preserves the `symdex.context_pack.v1` shape. Unified mode
returns `symdex.context_pack.v2`, merging SQLite symbol/call/file evidence with
Qdrant semantic chunk evidence and annotating rows with `evidence_source` values
of `structural`, `semantic`, or `both`. This does not add storage tables or
persist merged rows; the v2 pack is assembled from existing SQLite and Qdrant
metadata for each query.

Use cosine distance unless a selected embedding model requires otherwise.

Before writing vectors, symdex checks the latest successful run for the same
repository and embedding model. If the vector dimension changed, indexing fails
closed with a reset/reindex message instead of mixing incompatible points in the
same Qdrant collection. Different model names use different collection names.

The Qdrant verifier treats SQLite as the expected vector manifest. It compares
each non-null `chunks.qdrant_point_id` with Qdrant payload rows filtered by
`repository_id`, checking point ID, chunk ID, path, line range, text hash,
embedding model, and embedding dimension. Missing collections and missing
points are errors; stale payload fields and orphaned Qdrant points are warnings.
The report is metadata-only and does not request vectors or source text.

The Qdrant repair command uses verifier metadata as its repair plan. Orphaned
point IDs are deleted from Qdrant. Missing or stale expected points, including
payload model or dimension drift, are rebuilt through semantic indexing rather
than by a separate write path so SQLite remains the structural source of truth.

## TUI visualization mapping

The TUI should visualize storage metadata without showing source text by
default. Treat SQLite as the structural source of truth and Qdrant as the
semantic projection of embeddable chunks.

### Storage explorer

Use SQLite tables to show repository structure:

- `repositories`: selected repository identity and root metadata.
- `index_runs`: latest and historical indexing status.
- `files`: indexed paths, languages, content hashes, and indexed timestamps.
- `symbols`: symbol names, qualified names, kinds, nesting, and line ranges.
- `chunks`: chunk kinds, line ranges, text hashes, vector point IDs, and
  exclusion reasons.
- `calls`: caller/callee links, call lines, confidence, and resolution status.

Use Qdrant metadata to show semantic storage:

- collection name and expected embedding model/dimension
- point counts for the selected repository collection
- payload fields for selected points, excluding source text

### Index coverage view

Group by `files.path` and aggregate:

- chunk count from `chunks`
- symbol count from `symbols`
- call count from `calls` joined through caller symbols
- embeddable chunk count from chunks where `excluded_reason IS NULL`
- vector-backed chunk count from chunks with `qdrant_point_id IS NOT NULL`
- excluded chunk count grouped by `excluded_reason`

Use status labels such as `covered`, `metadata-only`, `excluded`, `stale`, and
`missing-vector` when the counts expose gaps.

### File detail drawer

For the selected file, show metadata rows from:

- `chunks`: kind, line range, text hash, vector point ID, exclusion reason
- `symbols`: kind, qualified name, parent symbol, line range
- `calls`: call line, callee text, resolved callee symbol, confidence, status

Do not show source previews unless a future source-preview design explicitly
allows it.

### Symbol outline view

Use `symbols.parent_symbol_id` to render nested symbol outlines. Rows should
show symbol kind, qualified name or display name, line range, and whether
related chunks/calls exist.

### Call resolution dashboard

Use `calls.resolution_status` and `calls.confidence` to group call edges into
resolved, unresolved, ambiguous, and low-confidence buckets. Rows should include
caller symbol, callee text, call line, path, confidence, and resolution status.

### Embedding coverage view

Compare SQLite chunk metadata with Qdrant collection metadata:

- chunks with `excluded_reason` are metadata-only and intentionally unembedded
- chunks with `qdrant_point_id` should have matching Qdrant points
- chunks without `qdrant_point_id` and without `excluded_reason` are missing
  vectors
- latest successful `index_runs.embedding_model` and `embedding_dimension`
  should match the selected Qdrant collection metadata

Surface missing collections, missing points, model drift, and dimension drift
as warning or error rows.

The CLI `qdrant-verify` command implements this live comparison against Qdrant.
The TUI can use the same status labels when it grows live cross-store actions.

The first TUI implementation uses SQLite metadata and recorded Qdrant point IDs
to show total, embeddable, vector-backed, missing-vector, and excluded chunk
counts plus latest model, dimension, collection, run count, exclusion reasons,
and health notes. It does not require a live Qdrant service for deterministic
offline rendering.

The cross-store health view consolidates these checks into selectable warning
rows. It flags missing collection metadata when embeddable chunks have no
successful semantic run or no vector-backed chunks, missing vectors when
eligible chunks lack `qdrant_point_id`, excluded chunks when `excluded_reason`
is present, model drift when the configured model differs from the latest
indexed model, and dimension drift when successful runs for the same model have
recorded multiple vector dimensions.

### Index runs timeline

Use `index_runs.started_at`, `finished_at`, `status`, `files_seen`,
`files_indexed`, `chunks_embedded`, `embedding_model`, `embedding_dimension`,
and `error_summary` for a compact run timeline.

The first TUI implementation reads `index_runs` directly from SQLite and shows
the latest 50 runs as metadata-only rows ordered by start time. Selecting a row
shows timestamps, status, file/chunk counts, embedding model and dimension, and
the stored error summary when present.

### Semantic neighborhood view

When visualizing nearby Qdrant points, show payload metadata only:

- path
- line range
- symbol name
- chunk kind
- language
- score
- text hash

Do not show full chunk text in semantic-neighborhood rows.

The first TUI implementation uses vector-backed chunk records as the
deterministic Qdrant payload projection and does not require a live Qdrant
service. Rows show path, line range, symbol name, chunk kind, language, text
hash, collection, and point ID. Score is labeled `metadata` until a future live
nearest-neighbor interaction supplies real scores.

## ID strategy

Use deterministic IDs where possible:

```text
file_id   = hash(repository_id + normalized_relative_path)
symbol_id = hash(file_id + kind + qualified_name + start_byte + signature_hash)
chunk_id  = hash(file_id + kind + start_byte + end_byte + text_hash)
call_id   = hash(caller_symbol_id + callee_text + call_line)
```

## Call path traversal

Call path tracing reads the persisted `calls` table joined to caller and callee
`symbols` plus caller file provenance. It does not add tables or mutate index
state.

Current behavior:

- source and target symbols are resolved by exact symbol ID, name, or qualified
  name within one repository
- traversal depth is clamped to 1-8 hops
- edge order is deterministic by caller qualified name, file path, call line,
  callee text, and call ID
- resolved edges are traversed through `callee_symbol_id`
- unresolved or ambiguous edges are preserved as terminal evidence when
  `callee_text` matches the target query
- cycles are skipped by tracking visited symbol IDs per candidate path
- returned edges include caller/callee metadata, call line, confidence,
  resolution status, freshness, trust, reason tags, and provenance; they never
  include source text

## Impact traversal and related-file evidence

Impact analysis reuses the same persisted call-edge graph. It keeps direct
callers and direct callees as first-class evidence, then adds bounded
transitive caller paths into the queried symbol and bounded transitive callee
paths out of the queried symbol. Transitive paths use the same deterministic
ordering, 1-8 hop clamp, cycle avoidance, unresolved terminal preservation, and
metadata-only edge shape as call path tracing.

Impact related files are derived from direct relationships and transitive path
edges. Each related-file row includes a deterministic relationship count,
freshness label, trust score, and the first available provenance record for that
file. Direct call rows, transitive paths, path edges, and related-file rows also
carry reason tags that distinguish direct caller/callee evidence, bounded
transitive paths, and file relationships derived from call evidence. The impact
report also includes indexed tests that directly call the queried symbol through
resolved call edges. Metadata-only tests without symbol linkage remain
searchable as test facts but do not appear in `tests_likely`. When no direct
indexed test evidence is available, `tests_likely` remains empty and the output
includes an explanatory note instead of guessing.

## Debug context packs

Debug context packs are query-time metadata bundles and do not add new tables.
Runtime input is parsed into frames containing optional frame symbols, file
paths, line numbers, and columns. Relative paths are normalized with repository
path rules; absolute paths are accepted only when they are under the selected
repository root.

The Rust-oriented parser recognizes common `cargo test` output, panic-hook
locations, `RUST_BACKTRACE=1` and `RUST_BACKTRACE=full` frame lines, `anyhow`
cause lists without treating each cause as a stack frame, tracing-style
`target=... file=... line=... column=...` metadata, and async stack-like lines
that include a symbol followed by `at path:line:column`.

The current `symdex.debug_context.v1` format joins parsed frames to existing
SQLite `files`, `symbols`, and `calls` rows. Returned frame evidence includes
the normalized path, matched symbols covering the runtime line, calls recorded
at that line, freshness labels from current file hashes, and provenance
metadata. Failing test names found in runtime input are mapped to indexed Rust
test facts when an exact or suffix match exists; unmatched runtime names are
kept as fallbacks and labeled with a note. Frame matches include trust scores so
agents can distinguish fresh, fully provenanced runtime evidence from stale,
deleted, or weakly provenanced matches. They also include reason tags that
identify path normalization, file provenance matches, symbol-at-location
matches, symbol-name fallback matches, calls at the runtime line, and unmatched
frames. Debug context packs do not include source text and do not mutate index
state.

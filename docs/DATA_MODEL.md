# Data Model

## SQLite tables

Initial schema names are stable enough for early implementation but may change before release.

Current implementation runs idempotent SQLite migrations at `symdex init`,
`symdex index`, and `symdex index-status`. It creates all tables listed below,
while the current indexing write path persists repositories, files, chunks,
symbols, calls, symbol references, external dependency facts, dependency usage
links, tests, conservative test targets, short-lived runtime observations,
fast semantic generations, and fast/quality
`chunk_embeddings` manifests. The older chunk-level vector columns remain
nullable compatibility schema, but layered manifests are the authoritative
semantic projection.

Migrations also create indexes for large-repo query paths: repository file
lookups, chunk-by-file cleanup, symbol name and qualified-name lookup,
caller/callee traversal, and index-run metadata checks.
Symbol-reference indexes cover source-symbol, target-symbol, kind, and
resolution-status scans for broader structural evidence beyond calls.
Dependency indexes cover package-name lookup, manifest cleanup, usage-by-file,
usage-by-source-symbol, and dependency-to-import joins for impact evidence.
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

### `repository_refs`

```sql
CREATE TABLE repository_refs (
  id TEXT PRIMARY KEY,
  repository_id TEXT NOT NULL,
  ref_kind TEXT NOT NULL,
  ref_name TEXT,
  ref_identity TEXT NOT NULL,
  head_oid TEXT,
  is_current INTEGER NOT NULL DEFAULT 0,
  last_seen_at TEXT,
  deleted_at TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  UNIQUE(repository_id, ref_kind, ref_identity)
);
```

`repository_refs` stores local worktree ref metadata for branch-aware indexing.
Attached local branches use `ref_kind = branch`, detached HEADs use `detached`,
unusual refs use `other`, and non-Git repositories use a stable `non_git`
working-tree ref. Current indexing records the active ref and attaches it to
index-run provenance. Local branch refs missing from current Git metadata are
marked with `deleted_at`; full branch-specific snapshot and vector garbage
collection is a later branch-aware indexing slice.

### `ref_files`

```sql
CREATE TABLE ref_files (
  repository_ref_id TEXT NOT NULL,
  repository_id TEXT NOT NULL,
  path TEXT NOT NULL,
  file_id TEXT NOT NULL,
  indexed_at TEXT NOT NULL,
  index_run_id TEXT,
  PRIMARY KEY(repository_ref_id, path)
);
```

`ref_files` records the current file manifest for each local repository ref.
Indexing upserts a path mapping for each active file it persists and removes
paths missing from that ref's latest discovery result. `file_id` points at the
content-addressed `files` snapshot for that path, so two local refs can retain
different indexed facts for the same repo-relative path when their content
hashes differ. Branch-aware cleanup removes file snapshots only after no
remaining ref manifest points at them. Structural symbol, call graph, impact,
and context-pack query paths use the live worktree ref when `ref_files`
manifests exist. Semantic search also filters sqlite-vec candidates through the
active ref manifest when manifests exist.

### `index_runs`

```sql
CREATE TABLE index_runs (
  id TEXT PRIMARY KEY,
  repository_id TEXT NOT NULL,
  repository_ref_id TEXT,
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
New index runs record `repository_ref_id` when the active local ref is known.
The current branch-awareness slices record active ref metadata, populate
`ref_files`, and route structural and semantic evidence queries through the
active ref manifest when available. File facts are content-addressed snapshots,
while semantic routing prefers the active ref's linked generation from
`semantic_generation_refs` and falls back to the repo-wide latest generation for
legacy indexes without ref mappings.
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
metadata-only error summary. Watch-driven batches are currently recorded with
`run_kind = watch`.

### `file_index_events`

```sql
CREATE TABLE file_index_events (
  id TEXT PRIMARY KEY,
  index_run_id TEXT NOT NULL,
  repository_id TEXT NOT NULL,
  repository_ref_id TEXT,
  path TEXT NOT NULL,
  old_content_hash TEXT,
  new_content_hash TEXT,
  action TEXT NOT NULL,
  reason TEXT NOT NULL,
  status TEXT NOT NULL,
  error_summary TEXT,
  occurred_at TEXT NOT NULL
);
```

`file_index_events` records the per-file decisions that make up an index run.
The table is append-only telemetry keyed by `index_run_id`, repo-relative path,
and action. It complements `index_runs`: the run row answers whether a batch
finished, while file events answer why each path was created, updated, deleted,
or skipped.

Current indexing writes events for:

- `created` with reason `new_file` when a discovered path has no prior indexed
  hash.
- `updated` with reason `content_changed` when a discovered path replaces a
  prior indexed hash.
- `updated` with reason `parsed_with_diagnostics` when syntax-aware parsing
  produced usable facts with parser diagnostics.
- `skipped` with reason `unchanged_content_hash` when incremental indexing
  links the existing file snapshot into the active ref manifest.
- `deleted` with reason `missing_from_discovery` when a previously indexed path
  is absent from the current discovery result because it was removed, ignored,
  unsupported, or no longer inside the configured indexing scope.
- `failed` with reason `read_failed` or `parse_failed` when collection aborts
  on a path-specific file read or parser error.

Events store `old_content_hash` and `new_content_hash` when available, plus
`repository_ref_id` for branch-aware runs. Source text is never stored in this
table. Parser or read failures that abort collection record `status = failed`
and a metadata-only `error_summary` when the failing path is known; aggregate
failure status remains in `index_runs`.

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
  parser_version TEXT
);
```

`id` is derived from repository identity, repo-relative path, and content hash.
This lets branch-specific manifests preserve same-path/different-content file
snapshots without reindexing or overwriting another local ref's structural
facts. A unique index on `(repository_id, path, content_hash)` prevents duplicate
snapshots for identical content, while ordinary path indexes keep current-path
lookups fast. Older local databases with the legacy `(repository_id, path)`
unique constraint are migrated to the snapshot shape during `migrate`.

`language` stores a stable language slug such as `rust`, `csharp`,
`javascript`, `typescript`, `toml`, `yaml`, or `json`. The schema is
intentionally language-neutral; no table change is required when adding C#,
JavaScript, TypeScript, fallback-only configuration formats, or future languages
that follow the same evidence contracts. Configuration chunks use nullable
`symbol_id` fields because they do not emit code symbols.

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
  vector_point_id TEXT,
  excluded_reason TEXT,
  index_run_id TEXT,
  parser_version TEXT,
  embedding_model TEXT,
  embedding_dimension INTEGER,
  embedded_at TEXT
);
```

`excluded_reason` is set when a chunk is kept as metadata but withheld from
embedding. The chunk-level `vector_point_id`, `embedding_model`,
`embedding_dimension`, and `embedded_at` columns are retained only as nullable
compatibility fields for local databases created before layered semantic
manifests. New indexing leaves them unset and records vector provenance in
`chunk_embeddings`.

Before semantic indexing replaces changed-file chunk rows or removes deleted
files, it reads current fast `chunk_embeddings` point IDs for those paths from
the latest semantic generation. New fast embeddings are staged and upserted to
sqlite-vec before SQLite mutation; after structural facts and the new semantic
generation are persisted, the previously collected stale point IDs are deleted
unless the new manifest reused the same deterministic point ID. This keeps
SQLite as the source of truth for vector lifecycle cleanup while avoiding source
text in sqlite-vec payloads or cleanup reports, and it preserves the previous
complete manifest if local embedding or vector upsert fails before SQLite
replacement.

When an active `ref_files` manifest is available, fast semantic generation
recording carries forward only fast `chunk_embeddings` whose file snapshot is
linked by that active ref manifest. Historical same-path snapshots can remain in
`files`, chunks, and older embedding rows, but they must not become members of
the latest ref-linked fast generation or drive quality job readiness.

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

### `symbol_references`

```sql
CREATE TABLE symbol_references (
  id TEXT PRIMARY KEY,
  file_id TEXT NOT NULL,
  source_symbol_id TEXT,
  target_symbol_id TEXT,
  reference_text TEXT NOT NULL,
  reference_kind TEXT NOT NULL,
  line INTEGER NOT NULL,
  confidence REAL NOT NULL,
  resolution_status TEXT NOT NULL,
  index_run_id TEXT,
  parser_version TEXT
);
```

`symbol_references` stores conservative structural references that are useful
to coding agents but are not caller/callee execution edges. Reference kinds are
`import`, `type_reference`, `implementation`, `attribute`, `inheritance`,
`decorator`, and `config_link`. `file_id` anchors lifecycle cleanup for
file-level references. `source_symbol_id` is nullable because imports,
file-level attributes, and future configuration links can originate outside a
function or method symbol. `target_symbol_id` is nullable and should only be
set when local evidence resolves the reference conservatively.

Current Rust extraction records `use` declarations, type-like syntax nodes,
`impl` relationships, and attributes. C#, JavaScript, and TypeScript have
conservative parser hooks for imports/usings, inheritance or base lists,
attributes/decorators, and type-like syntax where tree-sitter exposes stable
nodes. The table stores no source text beyond the compact reference expression,
and unresolved or ambiguous references are retained with confidence and
`resolution_status` metadata.

### `dependencies`

```sql
CREATE TABLE dependencies (
  id TEXT PRIMARY KEY,
  repository_id TEXT NOT NULL,
  file_id TEXT NOT NULL,
  manifest_path TEXT NOT NULL,
  package_manager TEXT NOT NULL,
  dependency_name TEXT NOT NULL,
  package_name TEXT NOT NULL,
  version_req TEXT,
  dependency_kind TEXT NOT NULL,
  index_run_id TEXT,
  parser_version TEXT,
  indexed_at TEXT NOT NULL
);
```

`dependencies` stores metadata-only package manifest facts. Current indexing
extracts conservative Cargo facts from root `Cargo.toml` dependency sections,
including package aliases, plus npm facts from root `package.json` dependency
maps. It stores manifest path, package manager, manifest key, package name,
requested version, and dependency kind; it does not store lockfile resolution
graphs or downloaded package metadata.

### `dependency_usages`

```sql
CREATE TABLE dependency_usages (
  id TEXT PRIMARY KEY,
  repository_id TEXT NOT NULL,
  dependency_id TEXT NOT NULL,
  file_id TEXT NOT NULL,
  source_symbol_id TEXT,
  usage_kind TEXT NOT NULL,
  import_path TEXT NOT NULL,
  referenced_symbol TEXT,
  line INTEGER NOT NULL,
  confidence REAL NOT NULL,
  reason TEXT NOT NULL,
  index_run_id TEXT,
  parser_version TEXT,
  indexed_at TEXT NOT NULL
);
```

`dependency_usages` links manifest dependencies to conservative import evidence
from `symbol_references`. It records usage kind, import path, optional
referenced symbol, line, confidence, and reason. Current matching is limited to
import/use/using statements that match known Cargo or npm dependency names.
These rows let impact and debug workflows answer questions such as "which
symbols import sqlx/sqlite?" without storing source text.

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

### `test_targets`

```sql
CREATE TABLE test_targets (
  id TEXT PRIMARY KEY,
  repository_id TEXT NOT NULL,
  test_id TEXT NOT NULL,
  target_symbol_id TEXT,
  target_file_id TEXT NOT NULL,
  relationship_kind TEXT NOT NULL,
  confidence REAL NOT NULL,
  reason TEXT NOT NULL,
  index_run_id TEXT,
  parser_version TEXT,
  indexed_at TEXT NOT NULL
);
```

`test_targets` records conservative relationships between indexed tests and
the symbols or files they likely cover. It exists so impact analysis does not
depend only on direct call-edge joins at query time. Current relationship kinds
are `direct_call`, `same_module`, `naming_convention`, and `fixture_path`.
Rows include a numeric confidence and compact reason string so downstream
surfaces can explain why a test was considered relevant without reading source
text.

Direct-call rows require a resolved call from a symbol-linked test to a target
symbol. Naming rows require an exact normalized test-name-to-symbol-name match
in the same file, such as `test_target` for `target`. Fixture-path rows link
test files such as `tests/calculator_tests.rs` to indexed source files with a
matching stem such as `src/calculator.rs`. Same-module rows are low-confidence
file-level evidence for colocated source-file tests and are not strong enough
by themselves to drive `tests_likely`.

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

This table tracks metadata for a semantic generation. Current semantic indexing
records the fast layer after successful fast sqlite-vec upsert with
`active_layer = fast` and `quality_status = fast_ready`. Generation IDs are
deterministic over the current fast manifest, so unchanged manifests reuse the
same generation ID and preserve existing quality fields such as
`quality_status`, `quality_dimension`, `quality_completed_at`, `active_layer`,
and `quality_embedded_chunks`. A changed fast manifest creates or selects a new
generation whose default active layer is fast until quality catches up. Rows do
not contain source text.

The manual quality worker refreshes activation state from SQLite manifests and
job counts. A latest generation becomes `quality_ready` with
`active_layer = quality` only when the current quality manifest covers every
quality-eligible chunk, the remaining fast-embeddable chunks are explicitly
accounted for as `skipped_excluded`, the quality model and dimension are known,
and no pending, running, failed, or `skipped_stale` jobs remain. Partial
eligible coverage remains `quality_pending`; terminal failures become
`quality_failed`; blocked generations remain `quality_blocked`; and
latest-generation `skipped_stale` jobs keep default routing on fast until a
later generation can complete cleanly.
`quality_completed_at` is set only by successful activation.

### `semantic_generation_refs`

```sql
CREATE TABLE semantic_generation_refs (
  repository_ref_id TEXT PRIMARY KEY,
  repository_id TEXT NOT NULL,
  generation_id TEXT NOT NULL,
  linked_at TEXT NOT NULL
);
```

This table records the current semantic generation associated with each local
repository ref. Semantic indexing links the active ref to the generation it
records. If a run has no changed embeddable chunks, the active ref is relinked
to the latest known generation when one exists. Semantic status and search use
this mapping when `ref_files` manifests are present, then fall back to the
legacy repo-wide latest generation for older indexes that have no ref mapping.
The mapping stores only metadata and is removed when the repository ref row is
removed.

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
  vector_table TEXT NOT NULL,
  vector_point_id TEXT NOT NULL,
  generation_id TEXT NOT NULL,
  embedded_at TEXT NOT NULL,
  status TEXT NOT NULL DEFAULT 'current',
  UNIQUE(chunk_id, semantic_layer, embedding_model, embedding_dimension)
);
```

This table is the per-layer vector manifest. Current semantic indexing writes
`fast` rows directly from the successful fast sqlite-vec upsert and writes `quality`
rows from the deferred quality worker. It keeps fast and quality metadata
separate by `semantic_layer`, model, dimension, generation, collection, and
point ID so the two layers do not share one sqlite-vec collection. `status` is
metadata-only and currently supports `current`, `stale`, `blocked`, and
`failed`.

Layer-aware sqlite-vec verification builds expected fast and quality point manifests
from `current` rows in this table for the latest semantic generation. Older
single-model local databases that predate layered manifests should run
`symdex index <repo>` to create fast `chunk_embeddings` rows before using
layer-aware verification.

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
Fast semantic indexing creates `pending` jobs only for current embeddable chunks
when quality indexing is enabled and the configured quality model is available.
Chunks with `excluded_reason`, including secret-blocked chunks, do not get
quality jobs. Oversized chunks that are otherwise embeddable keep their quality
jobs; the worker splits their source text into overlapping model-sized segments
and records one averaged vector for the original chunk. If a new fast generation
supersedes queued work, only old `pending` and `running` jobs are marked
`skipped_stale`; terminal history is preserved. If the quality model or service
is unavailable, the semantic generation is marked `quality_blocked` and no
pending quality jobs are created.
`semantic_generations.quality_dimension` remains null until the quality worker
records actual quality embeddings.

The manual quality worker claims oldest `pending` jobs in bounded batches,
marks them `running`, increments `attempts`, and then revalidates current file
and chunk metadata before embedding. Successful jobs transactionally write a
quality-layer `chunk_embeddings` row and move to `succeeded`. Service or vector
write failures move to `failed` with a compact metadata-only error summary.
Stale jobs move to `skipped_stale` and are not embedded. Chunks that are current
but not eligible for the quality layer, such as chunks over the quality model's
size limit or chunks that now have an `excluded_reason`, move to
`skipped_excluded`. Queue repair treats current terminal jobs, including
`skipped_excluded`, as accounted-for work rather than re-queueing them as
missing quality coverage. User-facing latest quality errors are reported from
`failed` jobs only, so intentional exclusions do not appear as worker failures.

After worker progress, `semantic_generations.quality_embedded_chunks` is
refreshed from current quality manifest rows. The first successful quality
embedding records `quality_dimension`. If failures remain and no pending or
running jobs remain for the latest generation, `quality_status` becomes
`quality_failed`; otherwise a complete, clean latest generation is atomically
marked `quality_ready` with `active_layer = quality`. Latest-generation
`skipped_stale` jobs are treated as incomplete work, not successful coverage.
`skipped_excluded` jobs reduce the quality layer's eligible chunk count.

Provenance columns are nullable for compatibility with existing local SQLite
databases. New indexing writes `index_run_id` and parser version metadata for
files, chunks, symbols, and calls. Semantic indexing records fast and quality
vector provenance in `chunk_embeddings`; legacy chunk embedding columns are no
longer authoritative and are left unset by new indexing.
`parser_version` must include the per-language parser identity and symdex
indexer/chunker version so mixed-language indexes remain auditable.

## sqlite-vec collection

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

Do not store source text in sqlite-vec payloads.

Returned evidence rows now include compact provenance metadata where available:
content hash, index run ID, parser version, indexed timestamp, embedding model,
embedding dimension, and embedding timestamp. Freshness checks compare persisted
content hashes with the current eligible file hashes and label rows as `fresh`,
`stale`, `deleted`, `missing`, or `unknown`. When `ref_files` manifests exist,
repository-level freshness reports compare only the active local ref manifest so
retained snapshots from other refs or earlier same-path content do not appear as
repairable stale rows.

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
sqlite-vec semantic chunk evidence and annotating rows with `evidence_source` values
of `structural`, `semantic`, or `both`. This does not add storage tables or
persist merged rows; the v2 pack is assembled from existing SQLite and sqlite-vec
metadata for each query.

Use cosine distance unless a selected embedding model requires otherwise.

Before writing vectors, symdex checks the latest successful run for the same
repository and embedding model. If the vector dimension changed, indexing fails
closed with a reset/reindex message instead of mixing incompatible points in the
same sqlite-vec collection. Different model names use different collection names.

The sqlite-vec verifier treats SQLite as the expected vector manifest. It compares
latest-generation `chunk_embeddings` rows for the selected semantic layer with
sqlite-vec payload rows filtered by `repository_id`, checking point ID, chunk ID,
path, line range, text hash, embedding model, and embedding dimension. Missing
collections and missing points are errors; stale payload fields and orphaned
sqlite-vec points are warnings. The report is metadata-only and does not request
vectors or source text.

The sqlite-vec repair command uses verifier metadata as its repair plan. Orphaned
point IDs are deleted from sqlite-vec. Missing or stale expected points, including
payload model or dimension drift, are rebuilt through semantic indexing rather
than by a separate write path so SQLite remains the structural source of truth.

## TUI visualization mapping

The TUI should visualize storage metadata without showing source text by
default. Treat SQLite as the structural source of truth and sqlite-vec as the
semantic projection of embeddable chunks.

### Storage explorer

Use SQLite tables to show repository structure:

- `repositories`: selected repository identity and root metadata.
- `index_runs`: latest and historical indexing status.
- `files`: indexed paths, languages, content hashes, and indexed timestamps.
- `symbols`: symbol names, qualified names, kinds, nesting, and line ranges.
- `chunks`: chunk kinds, line ranges, text hashes, compatibility vector fields,
  and exclusion reasons.
- `calls`: caller/callee links, call lines, confidence, and resolution status.
- `symbol_references`: imports, type references, implementations, inheritance,
  attributes/decorators, confidence, and resolution status.
- `dependencies` and `dependency_usages`: manifest package facts plus
  conservative import-to-dependency links.
- `tests` and `test_targets`: discovered test metadata plus conservative
  test-to-code relationship kind, confidence, and reason.

Use sqlite-vec metadata to show semantic storage:

- collection name and expected embedding model/dimension
- point counts for the selected repository collection
- payload fields for selected points, excluding source text

### Index coverage view

Group by `files.path` and aggregate:

- chunk count from `chunks`
- symbol count from `symbols`
- call count from `calls` joined through caller symbols
- symbol-reference count from `symbol_references` joined through files and,
  when present, source symbols
- dependency and dependency-usage counts from `dependencies` and
  `dependency_usages`
- embeddable chunk count from chunks where `excluded_reason IS NULL`
- vector-backed chunk count from current fast `chunk_embeddings` rows for the
  latest semantic generation
- excluded chunk count grouped by `excluded_reason`

Use status labels such as `covered`, `metadata-only`, `excluded`, `stale`, and
`missing-vector` when the counts expose gaps.

### File detail drawer

For the selected file, show metadata rows from:

- `chunks`: kind, line range, text hash, layered vector status, exclusion reason
- `symbols`: kind, qualified name, parent symbol, line range
- `calls`: call line, callee text, resolved callee symbol, confidence, status
- `symbol_references`: reference line, kind, text, resolved target symbol,
  confidence, status

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

Compare SQLite chunk metadata with sqlite-vec collection metadata:

- chunks with `excluded_reason` are metadata-only and intentionally unembedded
- current fast `chunk_embeddings` rows should have matching sqlite-vec points
- chunks without a current fast `chunk_embeddings` row and without
  `excluded_reason` are missing vectors
- latest successful `index_runs.embedding_model` and `embedding_dimension`
  should match the selected sqlite-vec collection metadata

Surface missing vector tables, missing points, model drift, and dimension drift
as warning or error rows.

The CLI `vector-verify` command implements this live comparison against sqlite-vec.
The TUI can use the same status labels when it grows live cross-store actions.

The first TUI implementation uses SQLite metadata and latest-generation fast
`chunk_embeddings` rows to show total, embeddable, vector-backed,
missing-vector, and excluded chunk counts plus latest model, dimension,
collection, run count, exclusion reasons, and health notes. It does not require
a live sqlite-vec service for deterministic offline rendering.

The cross-store health view consolidates these checks into selectable warning
rows. It flags missing collection metadata when embeddable chunks have no
successful semantic run or no vector-backed chunks, missing vectors when
eligible chunks lack a current fast `chunk_embeddings` row, excluded chunks when
`excluded_reason` is present, model drift when the configured model differs from
the latest indexed model, and dimension drift when successful runs for the same
model have recorded multiple vector dimensions.

### Index runs timeline

Use `index_runs.started_at`, `finished_at`, `status`, `files_seen`,
`files_indexed`, `chunks_embedded`, `embedding_model`, `embedding_dimension`,
and `error_summary` for a compact run timeline.

The first TUI implementation reads `index_runs` directly from SQLite and shows
the latest 50 runs as metadata-only rows ordered by start time. Selecting a row
shows timestamps, status, file/chunk counts, embedding model and dimension, and
the stored error summary when present.

### Semantic neighborhood view

When visualizing nearby sqlite-vec points, show payload metadata only:

- path
- line range
- symbol name
- chunk kind
- language
- score
- text hash

Do not show full chunk text in semantic-neighborhood rows.

The first TUI implementation uses vector-backed chunk records as the
deterministic sqlite-vec payload projection and does not require a live sqlite-vec
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
symbol_reference_id = hash(file_id + source_symbol_id + reference_text + reference_kind + line)
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
report also includes indexed tests that target the queried symbol or its file
through persisted `test_targets` rows with at least moderate confidence.
Existing direct-call joins remain a compatibility fallback for databases that
were indexed before `test_targets` existed. Metadata-only tests can contribute
when fixture-path evidence links them to a target file, but weak same-module
hints stay below the likely-test threshold. When no indexed test-target
evidence is available, `tests_likely` remains empty and the output includes an
explanatory note instead of guessing.

## Debug context packs

Debug context packs are metadata bundles built from runtime input. Runtime
input is parsed into frames containing optional frame symbols, file paths, line
numbers, and columns. Relative paths are normalized with repository path rules;
absolute paths are accepted only when they are under the selected repository
root.

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
frames. Debug context packs do not include source text.

### `runtime_observations`

```sql
CREATE TABLE runtime_observations (
  id TEXT PRIMARY KEY,
  repository_id TEXT NOT NULL,
  observation_id TEXT NOT NULL,
  input_hash TEXT NOT NULL,
  observation_kind TEXT NOT NULL,
  ordinal INTEGER,
  runtime_symbol TEXT,
  runtime_path TEXT,
  normalized_path TEXT,
  line INTEGER,
  column INTEGER,
  failing_test_name TEXT,
  mapped_test_name TEXT,
  matched INTEGER NOT NULL,
  match_kind TEXT NOT NULL,
  match_summary TEXT NOT NULL,
  observed_at TEXT NOT NULL,
  expires_at TEXT NOT NULL
);
```

`runtime_observations` is a short-lived metadata cache for repeated debugging
workflows. `symdex_debug_context` appends one row per parsed frame and failing
test name after it builds the normal debug context pack. Rows store a hash of
the full runtime input, not the pasted log. Frame rows can include the parsed
runtime symbol, parsed path, normalized repo-relative path, line, column,
match kind, freshness/trust summary, reason tags, matched symbol names, and
call-at-line counts. Failing-test rows store the parsed failing test name and
the indexed test name when mapping succeeds.

The cache currently uses a 24-hour expiry window and prunes expired rows during
new debug-context writes. It is intended for comparing repeated failures by
metadata shape and `input_hash`; it is not an audit log. Do not store raw
runtime output, source snippets, stack logs, or full files in this table.

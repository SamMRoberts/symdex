# Data Model

## SQLite tables

Initial schema names are stable enough for early implementation but may change before release.

Current implementation runs idempotent SQLite migrations at `symdex init`,
`symdex index`, and `symdex index-status`. It creates all tables listed below,
while the current write path persists repositories, files, chunks, symbols, and
calls.

Migrations also create indexes for large-repo query paths: repository file
lookups, chunk-by-file cleanup, symbol name and qualified-name lookup,
caller/callee traversal, and index-run metadata checks.

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
  error_summary TEXT
);
```

Successful semantic indexing runs are recorded here with the embedding model,
vector dimension, and embedded chunk count. `index-status` and
`symdex_index_status` expose the latest successful embedding model and
dimension when present.

### `files`

```sql
CREATE TABLE files (
  id TEXT PRIMARY KEY,
  repository_id TEXT NOT NULL,
  path TEXT NOT NULL,
  language TEXT NOT NULL,
  content_hash TEXT NOT NULL,
  indexed_at TEXT NOT NULL,
  UNIQUE(repository_id, path)
);
```

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
  end_byte INTEGER NOT NULL
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
  excluded_reason TEXT
);
```

`excluded_reason` is set when a chunk is kept as metadata but withheld from
embedding. Chunks with an exclusion reason do not get a Qdrant point ID in the
current implementation.

### `calls`

```sql
CREATE TABLE calls (
  id TEXT PRIMARY KEY,
  caller_symbol_id TEXT NOT NULL,
  callee_text TEXT NOT NULL,
  callee_symbol_id TEXT,
  call_line INTEGER NOT NULL,
  confidence REAL NOT NULL,
  resolution_status TEXT NOT NULL
);
```

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

Do not store source text in Qdrant payloads.

Use cosine distance unless a selected embedding model requires otherwise.

Before writing vectors, symdex checks the latest successful run for the same
repository and embedding model. If the vector dimension changed, indexing fails
closed with a reset/reindex message instead of mixing incompatible points in the
same Qdrant collection. Different model names use different collection names.

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

The first TUI implementation uses SQLite metadata and recorded Qdrant point IDs
to show total, embeddable, vector-backed, missing-vector, and excluded chunk
counts plus latest model, dimension, collection, run count, exclusion reasons,
and health notes. It does not require a live Qdrant service for deterministic
offline rendering.

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

## ID strategy

Use deterministic IDs where possible:

```text
file_id   = hash(repository_id + normalized_relative_path)
symbol_id = hash(file_id + kind + qualified_name + start_byte + signature_hash)
chunk_id  = hash(file_id + kind + start_byte + end_byte + text_hash)
call_id   = hash(caller_symbol_id + callee_text + call_line)
```

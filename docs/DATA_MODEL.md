# Data Model

## SQLite tables

Initial schema names are stable enough for early implementation but may change before release.

Current implementation runs idempotent SQLite migrations at `symdex init`,
`symdex index`, and `symdex index-status`. It creates all tables listed below,
while the current write path persists repositories, files, chunks, symbols, and
calls.

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

## ID strategy

Use deterministic IDs where possible:

```text
file_id   = hash(repository_id + normalized_relative_path)
symbol_id = hash(file_id + kind + qualified_name + start_byte + signature_hash)
chunk_id  = hash(file_id + kind + start_byte + end_byte + text_hash)
call_id   = hash(caller_symbol_id + callee_text + call_line)
```

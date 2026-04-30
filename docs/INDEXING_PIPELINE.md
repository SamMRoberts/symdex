# Indexing Pipeline

## Pipeline

```text
repo root
  -> discover files
  -> apply ignore rules
  -> hash contents
  -> skip unchanged files
  -> parse with tree-sitter
  -> extract symbols
  -> extract call-like references
  -> build chunks
  -> detect sensitive chunks
  -> embed allowed chunks with Ollama
  -> persist facts to SQLite
  -> upsert vectors to Qdrant
  -> write index run summary
```

## File discovery

Respect:

- `.gitignore`
- `.symdexignore` when added
- built-in excludes: `.git`, `target`, `node_modules`, vendor caches, generated lock-heavy folders

Never follow symlinks outside the repository root.

Current implementation applies built-in directory excludes and simple scoped
`.gitignore` rules from the repository root and nested directories. Literal
file paths, directory suffix rules, and basename rules are supported. Glob
patterns and negation rules are intentionally not implemented yet.
Repository roots must be directories, discovered symlinked files and directories
are skipped, and canonicalized symlink escapes are rejected by path
normalization.

## Chunking strategy

Preferred chunk units:

1. function
2. method
3. impl block summary
4. struct/enum/trait definition
5. module-level summary
6. fallback file chunk only when no better unit exists

Every chunk must include:

- repo-relative path
- byte range
- start/end line
- language
- symbol ID when available
- content hash
- chunk kind

Current implementation extracts Rust `function_item` syntax nodes as function
chunks, classifies functions under `impl_item` nodes as method chunks, and emits
a file fallback chunk only when no function-like chunks exist. Files with
tree-sitter syntax errors currently fail closed instead of producing partial
chunks.

Current implementation also scans each chunk for likely sensitive material
before embedding. Private key markers, credential-looking assignments, token
prefixes, and credentialed database connection strings set `excluded_reason` on
the chunk. Excluded chunks are persisted to SQLite as metadata, but are not sent
to Ollama and do not get Qdrant point IDs.

## Embeddings

Use Ollama with `nomic-embed-text`.

Current implementation uses Ollama `POST /api/embed` with `truncate: false` for
batch embeddings and `GET /api/tags` for local model availability. Vector
dimension probing embeds a tiny diagnostic string through the same local model.

Store:

- embedding model
- model tag
- vector dimension
- content hash
- embedding timestamp
- Qdrant point ID

If model name or vector dimension changes, require full reindex or collection migration.

Current implementation records successful semantic index runs in SQLite. Before
upserting vectors, it rejects a same-repository, same-model dimension change so
an existing Qdrant collection is not reused with incompatible vector sizes.
Different model names map to different Qdrant collection names.

## Qdrant Collections

Current implementation creates Qdrant collections through the REST API on the
configured `SYMDEX_QDRANT_URL`, defaulting to `http://localhost:6333`. Collection
creation uses dense vectors with cosine distance and validates generated
collection names before sending requests.

Vector upserts use Qdrant `PUT /collections/:collection_name/points?wait=true`.
Point IDs are deterministic UUID strings derived from chunk stable hashes.
Payloads include repository, file, chunk, symbol, path, language, line range,
chunk kind, and text hash metadata. Payloads intentionally do not include source
text.

Semantic search uses Qdrant `POST /collections/:collection_name/points/query`
with the embedded query vector, `with_payload: true`, and `with_vector: false`.

## Call extraction

Start conservative.

Capture:

- caller symbol ID
- callee textual name
- call site line
- confidence
- resolution status

Resolution states:

- `resolved_exact`
- `resolved_local_candidate`
- `unresolved`
- `ambiguous`

Never drop unresolved calls. They are useful evidence.

Current implementation extracts Rust function and method symbols from
`function_item` nodes. Free functions use module-derived qualified names, while
methods include the enclosing `impl` type when tree-sitter exposes it. Call
extraction records `call_expression` nodes inside indexed functions. Resolution
is local and conservative: exact qualified-name matches are
`resolved_exact`, single suffix/name matches are `resolved_local_candidate`,
multiple matches are `ambiguous`, and all other calls are preserved as
`unresolved`.

## Incremental indexing

A file can be skipped only when:

- path is unchanged
- content hash is unchanged
- index version is unchanged
- parser/chunker version is unchanged
- embedding model and dimension are unchanged

Changed files should replace their SQLite facts and Qdrant points atomically where practical.

Current implementation skips unchanged files by path plus content hash for
`symdex index --offline`, persists changed file/chunk facts to SQLite, and
removes SQLite rows for deleted files. Semantic `symdex index` currently parses
and embeds all discovered chunks so Qdrant can be rebuilt even when SQLite
already has matching structural facts. Same-model dimension changes fail closed;
automated collection migration/reset remains future hardening.

Current SQLite migrations include indexes for file cleanup, symbol lookup,
caller/callee traversal, and index-run metadata. This keeps structural queries
from degrading into broad table scans as repositories grow.

## Continuous indexing

Continuous indexing is a local watch mode layered on top of incremental
indexing. It can be toggled on or off and is off by default.

When enabled:

- watch for created and modified eligible files under the repository root
- apply built-in excludes, `.gitignore`, and future `.symdexignore` rules
- reject symlink escapes and paths outside the repository root
- debounce and coalesce bursts of filesystem events before indexing
- hash candidate files and skip unchanged content
- reindex changed or new files through the same parser, chunker, symbol, call,
  secret-detection, SQLite, Ollama, and Qdrant paths as manual indexing
- record compact index run summaries for watch-driven batches

Continuous indexing must not execute repository code. It must not bypass model
or dimension checks. Offline watch mode should update SQLite structural facts
without Qdrant or Ollama; semantic watch mode requires local Ollama and Qdrant
just like manual semantic indexing.

If a manual index job is running, continuous indexing should queue or coalesce
events and avoid concurrent writes. If a file changes repeatedly during a
debounce window, only the latest content hash should be indexed.

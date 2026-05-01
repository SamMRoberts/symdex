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
  -> discover tests
  -> build chunks
  -> detect sensitive chunks
  -> embed allowed chunks with Ollama
  -> persist facts to SQLite
  -> upsert vectors to Qdrant
  -> start index run summary
  -> finish index run summary as success, skipped, partial, or failed
```

## File discovery

Respect:

- `.gitignore`
- `.symdexignore` when added
- built-in excludes: `.git`, `target`, `node_modules`, vendor caches, generated lock-heavy folders

Never follow symlinks outside the repository root.

Active language targets:

| Language | Extensions | Parser target | Language slug |
|---|---|---|---|
| Rust | `.rs` | `tree-sitter-rust` | `rust` |
| C# | `.cs` | `tree-sitter-c-sharp` | `csharp` |
| JavaScript | `.js`, `.jsx`, `.mjs`, `.cjs` | `tree-sitter-javascript` | `javascript` |
| TypeScript | `.ts`, `.tsx`, `.mts`, `.cts` | `tree-sitter-typescript` | `typescript` |

Rust, C#, JavaScript, and TypeScript are implemented language targets. C#, JS,
and TS support starts conservatively with syntax-aware function/method chunks,
symbols, and call-like references; it does not claim whole-language type
inference. Future languages must be added through the same discovery, parsing,
chunking, symbol, call, hashing, secret-detection, embedding, SQLite, Qdrant,
manual indexing, and continuous indexing contracts.

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
Rust structural chunks for `struct`, `enum`, `union`, `type`, `trait`, and
`impl` items. `impl` items use `impl_summary` chunks, while type aliases,
nominal types, and traits use `type_definition` chunks. Trait impl summaries
preserve both sides of the implementation, for example `impl Runnable for Mode`.
A file fallback chunk is emitted only when no better chunkable unit exists.
Files with tree-sitter syntax errors produce partial chunks where possible and
return metadata-only parse diagnostics with line and byte ranges instead of
failing the whole index run.

Rust test discovery is metadata-only and conservative. Functions with Rust test
attributes such as `#[test]`, `#[tokio::test]`, `#[async_std::test]`, or
`#[actix_rt::test]` are persisted as indexed test facts with symbol linkage,
qualified names, byte ranges, and line ranges. C#, JavaScript, and TypeScript
test discovery is intentionally not claimed yet; future support must use the
same parser, symbol, call, secret-detection, provenance, and path-boundary
contracts.

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

Current implementation records started and finished index runs in SQLite. Runs
finish as `success`, `skipped`, `partial`, or `failed`. Offline indexing records
successful structural runs, semantic indexing records skipped runs when there
are no chunks to embed, and semantic failures after SQLite persistence are
recorded as partial runs with metadata-only error summaries. Before upserting
vectors, semantic indexing rejects a same-repository, same-model dimension
change so an existing Qdrant collection is not reused with incompatible vector
sizes. Different model names map to different Qdrant collection names.

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

Semantic indexing captures existing Qdrant point IDs from SQLite before changed
file facts are replaced or deleted-file rows are removed. When the target
collection exists, stale points for changed and deleted chunks are deleted from
Qdrant before SQLite mutation so vector cleanup does not lose the old point IDs.
If stale point deletion fails, semantic indexing fails before replacing SQLite
facts and records the run failure in `index_runs`.

`symdex qdrant-verify <repo>` performs a metadata-only lifecycle check for the
configured embedding model. It derives the expected point manifest from SQLite
chunks with `qdrant_point_id`, scrolls Qdrant payloads filtered by
`repository_id`, and reports missing collections, missing points, stale payload
fields, and orphaned points. The verifier requests payloads only, not vectors,
and never returns source text.

`symdex qdrant-repair <repo>` starts from that verification report. Orphaned
Qdrant points are deleted directly because SQLite has no matching chunk for
them. Missing collections, missing points, stale payload fields, and payload
model or dimension drift are repaired by running the normal semantic indexing
path, preserving the same parser, hashing, secret-detection, embedding,
provenance, and index-run lifecycle behavior as `symdex index <repo>`.

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
methods include the enclosing `impl` container when tree-sitter exposes it.
Trait impl methods use Rust-like containers such as `<Mode as Runnable>::run`
so trait methods do not collapse into inherent method names. Call extraction
records `call_expression` nodes inside indexed functions. Resolution
is local and conservative: exact qualified-name matches are `resolved_exact`,
single suffix/name matches are `resolved_local_candidate`, multiple matches are
`ambiguous`, and all other calls are preserved as `unresolved`. Rust resolution
also normalizes leading `crate::`, `self::`, and `super::` prefixes and applies
simple file-local `use` aliases such as `use crate::module::function as alias;`
or `use crate::module as alias;` before falling back to suffix matching. `self::`
and `super::` targets in `use` declarations are interpreted from the caller
module scope. It also handles simple one-level grouped imports such as
`use crate::module::{function, other as alias, self as module_alias};`. Within
Rust modules, `self::name()` and `super::name()` calls add candidates from the
caller module and immediate parent module, so nested modules do not collapse to
unrelated same-named root symbols. Relative scoped calls such as `Type::method()`
inside a module add a caller-module candidate such as `module::Type::method`, so
same-named root methods are not treated as exact matches from nested modules.
Inside Rust methods, calls through `self.method()` and `Self::method()` also add
a candidate for the enclosing impl receiver, so they can resolve exactly when the
target method is present in the same file. Other receiver expressions remain
conservative because symdex does not perform type inference. Rust macro
invocations are preserved as unresolved call edges with low confidence; macro
expansion is not analyzed. Each macro invocation also emits a
metadata-only diagnostic noting that the invocation was preserved without
expansion.

After per-file parsing, the indexer performs a conservative Rust cross-file
resolution pass before persisting SQLite facts. Qualified module calls such as
`crate::module::function()` are normalized and matched against Rust symbols from
the current index batch plus already persisted unchanged Rust files. A single
qualified match is upgraded to `resolved_exact`; multiple matches are preserved
as `ambiguous`; unresolved calls without qualified module paths are left
unchanged. Cross-file `self::` and `super::` calls use the caller symbol's module
context before matching persisted symbols, matching the per-file resolver's
caller-scope behavior. Symbols from files being replaced are ignored so
incremental indexing does not resolve against stale facts.

Optional rust-analyzer enrichment is guarded behind explicit opt-in readiness
diagnostics. `symdex doctor` can check whether a local `rust-analyzer` binary is
available when `SYMDEX_RUST_ANALYZER=1` is set, but indexing does not invoke
project analysis by default. Index runs also report a metadata-only enrichment
plan when explicitly enabled: disabled, not ready, skipped because no changed
Rust files were indexed, or planned with eligible Rust file, symbol, and call
counts. This plan is reporting only; it does not mutate persisted symbols or
calls. Future symbol and call fact application must keep this opt-in boundary,
preserve source-text privacy, and avoid executing indexed repository code.

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

Files indexed from tree-sitter error trees are included in normal structural and
semantic indexing summaries with parse diagnostics. These diagnostics are not
source previews; they contain only the diagnostic message, line range, and byte
range so downstream agents can account for parse completeness without receiving
file contents.

Current SQLite migrations include indexes for file cleanup, symbol lookup,
caller/callee traversal, and index-run metadata. This keeps structural queries
from degrading into broad table scans as repositories grow.

## Continuous indexing

Continuous indexing is a local watch mode layered on top of incremental
indexing. It can be toggled on or off and is off by default.

Current implementation uses a polling watcher: it discovers eligible Rust, C#,
JavaScript, and TypeScript files at a fixed interval, compares
path-to-content-hash snapshots, debounces detected changes, and runs an
incremental index batch when created, modified, or deleted paths are found.

When enabled:

- watch for created and modified eligible files under the repository root
- apply built-in excludes, `.gitignore`, and future `.symdexignore` rules
- reject symlink escapes and paths outside the repository root
- debounce and coalesce bursts of filesystem events before indexing
- hash candidate files and skip unchanged content
- reindex changed or new files through the same parser, chunker, symbol, call,
  secret-detection, SQLite, Ollama, and Qdrant paths as manual indexing
- record compact index run summaries for watch-driven batches when semantic
  indexing runs

Continuous indexing must not execute repository code. It must not bypass model
or dimension checks. Offline watch mode should update SQLite structural facts
without Qdrant or Ollama; semantic watch mode requires local Ollama and Qdrant
just like manual semantic indexing.

If a manual index job is running, continuous indexing should queue or coalesce
events and avoid concurrent writes. If a file changes repeatedly during a
debounce window, only the latest content hash should be indexed.

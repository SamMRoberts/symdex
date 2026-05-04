# Indexing Pipeline

## Pipeline

```text
repo root
  -> resolve local Git/ref metadata
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
  -> start index run summary
  -> collect stale vector point IDs from the latest generation
  -> embed allowed chunks with Ollama
  -> upsert new vectors to sqlite-vec
  -> persist facts to SQLite
  -> record the fast semantic generation and queue quality work
  -> delete stale sqlite-vec points not reused by the new manifest
  -> finish index run summary as success, skipped, partial, or failed
```

## File discovery

Before discovery, indexing resolves local repository-ref metadata without
executing repository code. Attached local branches are recorded by branch name,
detached HEADs by object ID, unusual refs as `other`, and non-Git repositories
as a stable `non_git` working-tree ref. This first slice stores the ref metadata
and attaches it to index-run provenance. Indexing also records a `ref_files`
path manifest for the active ref and removes paths missing from that ref's
latest discovery result. Repo-wide file facts are removed only when no remaining
ref manifest points at them. Structural symbol, call graph, impact, and
context-pack queries use the live worktree ref when `ref_files` manifests exist.
Semantic generation selection remains repo-wide until the later semantic-routing
slice.

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
chunking, symbol, call, hashing, secret-detection, embedding, SQLite, sqlite-vec,
manual indexing, and continuous indexing contracts.

Current implementation applies built-in directory excludes and scoped
`.gitignore` rules from the repository root and nested directories. Rules are
ordered and glob-aware, including `*`, `**`, `?`, character classes, directory
rules, basename rules, nested scope, and `!` negation. Built-in excludes such as
`.git`, `target`, `node_modules`, and local vector-store cache directories are
hard excludes and cannot be re-included by `.gitignore` negation. Repository roots must be
directories, discovered symlinked files and directories are skipped, and
canonicalized symlink escapes are rejected by path normalization.

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

Test discovery is metadata-only and conservative. Functions with Rust test
attributes such as `#[test]`, `#[tokio::test]`, `#[async_std::test]`, or
`#[actix_rt::test]` are persisted as indexed test facts with symbol linkage,
qualified names, byte ranges, and line ranges. C# methods with NUnit, xUnit,
or MSTest test attributes are persisted through the same symbol-linked path.
JavaScript and TypeScript Jest, Vitest, and Mocha `test` / `it` calls are
discovered from tree-sitter call expressions when framework imports or
test-like paths provide conservative evidence. Nested `describe` calls provide
suite context for qualified names. Named callback tests are linked to an indexed
symbol only when the callback reference is unambiguous; anonymous callback tests
are persisted as metadata-only rows with no symbol link so they cannot overclaim
call coverage.

Current implementation also scans each chunk for likely sensitive material
before embedding. Private key markers, credential-looking assignments, token
prefixes, and credentialed database connection strings set `excluded_reason` on
the chunk. Excluded chunks are persisted to SQLite as metadata, but are not sent
to Ollama and do not get sqlite-vec point IDs.

## Embeddings

Use Ollama with `nomic-embed-text`.

Current implementation uses Ollama `POST /api/embed` for batch embeddings and
`GET /api/tags` for local model availability. `SYMDEX_EMBED_TRUNCATE` defaults
to `true`, but Symdex also excludes large chunks before embedding because some
Ollama/model combinations return context-length errors instead of truncating.
`SYMDEX_EMBED_BATCH_SIZE` defaults to `16`, so full-repository semantic indexing
is split into smaller Ollama requests while preserving embedding order.
`SYMDEX_EMBED_MAX_CHUNK_BYTES` defaults to `2048` for fast indexing, while
`SYMDEX_QUALITY_EMBED_MAX_CHUNK_BYTES` defaults to `512` for quality indexing.
Larger chunks are kept as metadata-only structural evidence with
`chunk_too_large_for_embedding` and are omitted from Ollama/sqlite-vec. Vector
dimension probing embeds a tiny diagnostic string through the same local model.

Store:

- embedding model
- model tag
- vector dimension
- content hash
- embedding timestamp
- sqlite-vec point ID

If model name or vector dimension changes, require full reindex or collection migration.

Current implementation records started and finished index runs in SQLite. Runs
finish as `success`, `skipped`, `partial`, or `failed`. Offline indexing records
successful structural runs, semantic indexing records skipped runs when there
are no chunks to embed, and semantic embedding or sqlite-vec upsert failures before
SQLite replacement are recorded as failed runs so the previous semantic
generation remains intact. Failures after SQLite replacement, such as generation
finalization or stale-vector cleanup failures, are recorded as partial runs with
metadata-only error summaries. Before upserting vectors, semantic indexing
rejects a same-repository, same-model dimension change so an existing sqlite-vec
collection is not reused with incompatible vector sizes. Different model names
map to different sqlite-vec collection names.
Continuous watch batches use the same incremental indexing path and are recorded
with `run_kind = watch` in index-run metadata.

The SQLite schema also includes additive layered semantic tables for
`semantic_generations`, `chunk_embeddings`, and `quality_embedding_jobs`.
After a successful fast sqlite-vec upsert, semantic indexing records a deterministic
fast semantic generation and current fast `chunk_embeddings` manifest in SQLite.
The older chunk-level vector columns remain nullable compatibility schema, but
new indexing does not use them as the authoritative fast manifest.
When quality indexing is enabled and the quality model is locally available,
semantic indexing then marks superseded pending/running quality jobs stale and
first carries forward current quality `chunk_embeddings` rows whose chunk ID,
content hash, text hash, and quality model still match the latest fast manifest.
It then queues metadata-only `quality_embedding_jobs` rows only for changed or
newly embeddable chunks missing reusable quality coverage. This keeps the latest
generation complete without forcing a full quality rebuild after every
incremental fast index. If the quality model or service is unavailable, the
latest generation is marked `quality_blocked` and no pending quality jobs are
created.
Semantic search consults SQLite readiness metadata from the latest semantic
generation and compact `chunk_embeddings` summaries before choosing the active
model and sqlite-vec collection. The manual quality worker is available through
`symdex index-quality <repo>`; it drains pending latest-generation jobs in
bounded batches, revalidates hashes from disk before embedding, writes quality
sqlite-vec points and quality `chunk_embeddings` rows, then refreshes activation
state in SQLite. Activation switches default routing to quality only when the
latest fast generation has complete current quality coverage, a known quality
dimension, and no pending, running, failed, or stale quality jobs. Partial,
stale, blocked, or failed quality state leaves `active_layer = fast`.

## sqlite-vec Tables

Current implementation creates sqlite-vec `vec0` virtual tables inside the
configured SQLite database. Table creation uses dense vectors with cosine
distance and validates generated table names before creating virtual tables.

Vector upserts write sqlite-vec rows plus metadata-only `vector_points` rows in
SQLite. Point IDs are deterministic strings derived from chunk stable hashes.
Metadata includes repository, file, chunk, symbol, path, language, line range,
chunk kind, and text hash. Metadata intentionally does not include source text.

Semantic indexing captures existing latest-generation fast `chunk_embeddings`
point IDs from SQLite before changed-file facts are replaced or deleted-file
rows are removed. It stages fast embeddings and upserts new sqlite-vec points before
SQLite mutation, then records the new fast generation after structural facts are
persisted. Stale points for changed and deleted chunks are deleted only after
the new manifest is recorded, and point IDs that were just upserted are protected
from deletion because deterministic IDs can be reused for unchanged chunks.
This ordering prevents a local Ollama or sqlite-vec failure from replacing current
chunks and cascading away the previous complete vector manifest.

`symdex vector-verify <repo>` performs a metadata-only lifecycle check for the
selected semantic layer. It derives the expected point manifest from
latest-generation `chunk_embeddings`, reads sqlite-vec metadata filtered by
`repository_id`, and reports missing tables, missing points, stale payload
fields, and orphaned points. The verifier uses metadata only and never returns
source text.

`symdex vector-repair <repo>` starts from that verification report. Orphaned
sqlite-vec points are deleted directly because SQLite has no matching chunk for
them. Missing collections, missing points, stale payload fields, and payload
model or dimension drift are repaired by running the normal semantic indexing
path, preserving the same parser, hashing, secret-detection, embedding,
provenance, and index-run lifecycle behavior as `symdex index <repo>`.

Semantic search uses sqlite-vec KNN queries with the embedded query vector.
The embedded query model and target collection come from active-layer routing:
auto search uses quality only when the active generation is `quality_ready` and
the quality manifest is complete, otherwise it uses the fast layer. Forced
quality routing fails clearly when quality is unavailable or incomplete.

`symdex index --watch <repo>` performs cooperative quality catch-up in semantic
watch mode when quality indexing is enabled. Watch-driven fast batches queue
quality jobs and emit completion before catch-up begins. Quality catch-up then
processes bounded batches through the normal quality worker during post-batch or
idle watch ticks, refreshing activation state after each bounded run.

sqlite-vec verification and repair are layer-aware maintenance paths. `vector-verify`
and `vector-repair` accept `--semantic-layer fast|quality|all`. Verification
builds expected fast and quality manifests from latest-generation
`chunk_embeddings` rows for the selected layer. Fast verification no longer uses
legacy `chunks.vector_point_id` metadata; legacy-only local databases need a
fresh `symdex index <repo>` run before layered verification. Repair routes fast
rebuilds through normal semantic indexing and quality rebuilds through the
quality worker path.

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
unrelated same-named root symbols. Unqualified calls such as `helper()` and
relative scoped calls such as `Type::method()` inside a module add caller-module
candidates such as `module::helper` and `module::Type::method`, so same-named
root symbols are not treated as exact matches from nested modules. Inside Rust
methods, calls through `self.method()` and `Self::method()` also add a candidate
for the enclosing impl receiver, so they can resolve exactly when the target
method is present in the same file. Other receiver expressions remain
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
unchanged. Cross-file `self::` and `super::` calls, unqualified calls, and
relative scoped calls use the caller symbol's module context before matching
persisted symbols, matching the per-file resolver's caller-scope behavior. When
the caller is a method, cross-file `self.method()` and `Self::method()` calls can
also resolve to persisted unchanged methods on the same impl receiver. Symbols
from files being replaced are ignored so incremental indexing does not resolve
against stale facts.

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

Changed files should replace their SQLite facts and sqlite-vec points atomically where practical.

Current implementation exposes scope independently from semantic/offline mode:
`symdex index --full <repo>` reparses every eligible file and rebuilds eligible
semantic vectors when not offline, while `symdex index --incremental <repo>`
skips unchanged files by path plus content hash. Plain `symdex index <repo>`
keeps the legacy semantic default of full scope, and plain
`symdex index --offline <repo>` keeps the legacy structural default of
incremental scope; either command can be made explicit with `--full` or
`--incremental`. Incremental runs persist changed file/chunk facts to SQLite and
remove SQLite rows for deleted files. Same-model dimension changes fail closed;
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
indexing. A single background watcher owns watch work for each repository.
`symdex tui [repo]`, `symdex watch start <repo>`, and `symdex serve-mcp --watch
<repo>` start or attach that watcher; `symdex watch stop <repo>` or the TUI
toggle stops it.

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
  secret-detection, SQLite, Ollama, and sqlite-vec paths as manual indexing
- record compact index run summaries for watch-driven batches when semantic
  indexing runs

Continuous indexing must not execute repository code. It must not bypass model
or dimension checks. Offline watch mode should update SQLite structural facts
without sqlite-vec or Ollama; semantic watch mode requires local Ollama and sqlite-vec
just like manual semantic indexing.

If a manual index job is running, continuous indexing should queue or coalesce
events and avoid concurrent writes. If a file changes repeatedly during a
debounce window, only the latest content hash should be indexed.

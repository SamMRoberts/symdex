# Testing

## Test pyramid

1. Unit tests for parsing, chunking, hashing, path normalization, and secret detection.
2. Integration tests for SQLite migrations and query behavior.
3. Integration tests for sqlite-vec adapter behind an opt-in feature or environment flag.
4. Integration tests for Ollama adapter behind an opt-in feature or environment flag.
5. MCP contract tests for input validation and output shape.
6. TUI state and render tests using `ratatui` test backends.

## Fixtures

Store under:

```text
tests/fixtures/
  rust_basic/
  rust_calls/
  rust_ignore/
  rust_secrets/
  csharp_basic/
  javascript_basic/
  typescript_basic/
```

Fixtures should be tiny and purpose-built.

## Required test areas

- syntax chunk line ranges
- partial parse diagnostics for syntax-error files without aborting indexing
- stable IDs across repeated runs
- changed file reindex
- continuous indexing reindexes modified files
- continuous indexing indexes newly created eligible files
- continuous indexing skips ignored, out-of-root, symlink-escape, and unchanged files
- continuous indexing toggle, debounce, queued event, and error states
- deleted file cleanup
- unresolved calls preserved
- ambiguous calls labeled
- bounded call path traversal order, unresolved terminal edges, cycles, and depth limits
- repo-root path enforcement
- ignored files not indexed
- likely secrets excluded from embeddings
- MCP tools reject invalid paths
- TUI navigation and confirmation flows
- TUI loading, empty, and error states
- TUI render snapshots or buffer assertions for key screens
- TUI selectable table panes render from full result sets and scroll selected
  rows beyond the initially visible viewport.
- TUI storage visualization state and render coverage for SQLite/sqlite-vec
  metadata, selected-row drill-down, empty stores, missing vectors, excluded
  chunks, and model/dimension drift
- Evidence freshness coverage for fresh, stale, deleted, missing, and unknown
  states, including metadata-only TUI rendering.
- Evidence trust scoring coverage for freshness, provenance completeness,
  confidence, and index metadata completeness, including impact and
  debug-context evidence rows.
- Evidence explainability coverage for semantic result reasons, direct impact
  call reasons, related-file reasons, MCP contract reason availability, and
  debug-context frame match reasons.
- Unified context-pack coverage for structural-only fallback, semantic-only
  chunks, overlapping structural/semantic evidence marked as `both`, stale
  freshness labels, missing-vector or unavailable-semantic notes, and continued
  absence of source text.
- Debug context coverage for parsed panic/file locations, stack-frame symbols,
  failing test names, mapped frames, unmapped frames, stale frames, deleted
  files, malformed runtime input, common Rust `cargo test`, `anyhow`, `tracing`,
  full backtrace, panic-hook, and async stack-like output, and TUI debug context
  pack rendering.
- Test discovery coverage for Rust recognized test attributes, C# NUnit/xUnit/
  MSTest attributes, JavaScript and TypeScript Jest/Vitest/Mocha `test` / `it` /
  `describe` shapes, module- or suite-qualified test names, SQLite test
  persistence/replacement, failing-test name mapping, metadata-only anonymous
  callback rows, and impact likely-test evidence only from direct indexed test
  calls.
- Rust call-resolution coverage for exact local calls, unresolved calls,
  normalized `crate::` prefixes, explicit `use ... as ...` function aliases,
  module aliases used in scoped calls, simple grouped `use` aliases,
  module-relative `use` aliases from caller scope, caller-scope Rust `self::` /
  `super::` module calls, caller-module relative `helper()` and `Type::method()`
  calls, and exact Rust `self.method()` / `Self::method()` resolution to methods
  on the enclosing impl receiver.
- Rust cross-file call-resolution coverage for qualified module calls resolved
  from the current index batch, persisted unchanged Rust symbols used during
  incremental indexing, caller-scope `super::` calls resolved against persisted
  unchanged Rust symbols, caller-module unqualified and scoped calls resolved
  against persisted unchanged Rust symbols, cross-file `self.method()` /
  `Self::method()` calls resolved against persisted unchanged methods on the
  same impl receiver, and stale persisted symbols ignored for files being
  replaced.
- Rust macro coverage for unresolved macro call edges and metadata-only
  diagnostics that macro invocations are preserved without expansion.
- Rust chunking coverage for function, method, type-definition, trait,
  impl-summary, trait impl-summary names, trait impl method qualified names, and
  fallback chunks.
- Optional rust-analyzer readiness coverage for default-off behavior, explicit
  truthy opt-in flags, command override parsing, and doctor check status without
  requiring rust-analyzer in ordinary tests.
- Optional rust-analyzer enrichment planning coverage for disabled, not-ready,
  no-Rust-file, and planned eligible Rust file/symbol/call count states without
  requiring rust-analyzer in ordinary tests.

Current path-boundary tests cover file paths rejected as repository roots,
canonical symlink escapes rejected by normalization, symlinked files and
directories skipped during discovery, and MCP repo arguments rejected when they
do not name a directory root.

Current discovery tests cover built-in hard excludes, root and nested
`.gitignore` scope, glob patterns using `*`, `**`, `?`, and character classes,
directory rules, basename rules, ordered `!` negation, and protection against
re-including built-in excluded directories.

Current migration tests also assert that structural-query indexes are created
for symbols, calls, chunks, files, and index runs.

Continuous indexing tests should use synthetic filesystem events where possible
for debounce and coalescing behavior, plus tiny fixture repositories for
end-to-end created-file and modified-file reindex behavior. Offline continuous
indexing should be testable without sqlite-vec or Ollama; semantic continuous
indexing should use mocked adapters or the existing opt-in local service test
flags.

Current continuous indexing tests cover snapshot diff coalescing, created-file
and modified-file detection, ignored path skips including glob rules and
negated includes, unsupported-language path skips, and unchanged-content skips
through the shared discovery path. TUI state/render tests cover
continuous-indexing
toggle confirmation, stopping an active watcher, pending debounce display,
queued event count, and latest error rendering.

## Test commands

Default local checks:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build --workspace --release
cargo audit
```

The CI workflow runs the same production hardening baseline on pull requests and
pushes to `main`, and installs `cargo-audit` before running the dependency
audit. Service-dependent checks remain opt-in so ordinary CI does not require
local sqlite-vec or Ollama services.

sqlite-vec verifier unit tests keep the comparison logic deterministic by building
SQLite expected-point manifests and sqlite-vec payload rows in memory. Live sqlite-vec
scroll behavior remains part of the service-dependent adapter checks. Repair
uses the verifier's captured point IDs for orphan deletion and the existing
semantic index path for vector rebuilds, so focused tests cover the repair plan
classification while live end-to-end repair remains service-dependent.

Service-dependent checks:

```bash
SYMDEX_TEST_SQLITE_VEC=1 cargo test -p symdex-store sqlite_vec
SYMDEX_TEST_OLLAMA=1 cargo test -p symdex-embed ollama
```

Current multi-language tests cover discovery, parser dispatch, syntax-aware
chunking, symbol extraction, conservative call extraction, runtime path parsing,
and continuous-indexing snapshots for C#, JavaScript, and TypeScript. Broaden
these fixture-backed tests when adding deeper language-specific behavior.
Current parser tests also cover syntax-error files returning partial indexes
with metadata-only parse diagnostics instead of failing closed.

Future TUI checks:

```bash
cargo test -p symdex-tui
cargo run -p symdex-cli -- tui --help
```

Current TUI tests cover Overview/dashboard rendering, adaptive compact summary
rendering, compact key-chip footer rendering, doctor diagnostics rendering,
query workbench rendering and input state, symbol/call graph rendering and input
state, impact/call-path/context-pack/debug-context rendering and input state, the storage explorer
metric table, always-visible nested storage tab header, and detail panel,
index coverage table and selected-file detail panel with chunk, symbol, and
call metadata, query and storage table scrolling beyond the initially visible
rows, symbol outline table and
selected-symbol detail panel, call resolution bucket table and selected-bucket
detail panel, embedding coverage table and selected-metric detail panel with
exclusion-reason and health summaries, index runs timeline table and
selected-run detail panel, semantic neighborhood payload table and selected-row
detail panel, cross-store health warning table and selected-warning detail
panel, selected table rows, Doctor selected-check detail behavior, separate
footer containers for shortcut hints and status messages, 80x24 narrow-terminal
rendering, bracket-based primary tab navigation with letter-key text input,
the indexing confirmation reducer, and continuous-indexing reducer and render
coverage for toggle confirmation, stopping an active watcher, on/off labels,
pending debounce, queued event count, latest reindexed file, watch errors, and
the animated continuous-indexing activity indicator.
Current freshness tests cover hash-to-state classification, file freshness
aggregation over indexed and current file sets, returned symbol/call provenance,
trust scoring, explainability reason tags, and the TUI evidence freshness panel.
Current cross-agent reuse tests cover the shared MCP evidence contract envelope,
read-only tool annotations, underscore-only tool names, repo root validation,
and two independent MCP readers using the same SQLite index-status path without
write-capable tools. Current MCP staleness tests cover tool schema, path
validation, symbol scope, explicit paths, stale/deleted/missing/unknown states,
and source-free envelope output.
Current diagnostics tests cover optional rust-analyzer readiness configuration
without invoking project analysis or requiring rust-analyzer to be installed.

Current call path tests cover deterministic path order, unresolved terminal
edges matched by callee text, ambiguous terminal edges matched by callee text,
cycle avoidance, depth limits, and deterministic transitive impact paths.
Current debug context tests cover runtime input parsing, mapped frame evidence,
unmapped frames, fresh/stale/deleted freshness labels, calls at failing lines,
malformed runtime lines, indexed failing-test mapping, unmatched failing-test
fallbacks, common Rust runtime output shapes, and impact likely-test evidence
from direct indexed test calls.

TUI storage visualizations should use SQLite fixtures for deterministic
structural data and mocked or adapter-level sqlite-vec metadata for semantic
coverage checks. Tests should assert labels and counts instead of source text.

## Agent expectation

When changing behavior, add tests. When unable to run service-dependent tests, run unit tests and state which integration checks remain unverified.

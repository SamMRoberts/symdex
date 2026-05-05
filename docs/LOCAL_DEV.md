# Local Development

## Required services

- Rust toolchain
- SQLite available through Rust crate bindings
- `cargo-audit` installed for local dependency audits
- Ollama running locally
- `nomic-embed-text` pulled into Ollama

## Local setup commands

```bash
cargo install cargo-audit --locked
ollama pull nomic-embed-text
```

## Environment variables

```bash
SYMDEX_DB_PATH=.symdex/symdex.sqlite
SYMDEX_OLLAMA_URL=http://localhost:11434
SYMDEX_EMBED_MODEL=nomic-embed-text
SYMDEX_FAST_EMBED_MODEL=nomic-embed-text
SYMDEX_QUALITY_EMBED_MODEL=mxbai-embed-large
SYMDEX_QUALITY_INDEX=1
SYMDEX_QUALITY_BATCH_SIZE=16
SYMDEX_QUALITY_WORKERS=1
SYMDEX_EMBED_TRUNCATE=true
SYMDEX_EMBED_BATCH_SIZE=16
SYMDEX_EMBED_MAX_CHUNK_BYTES=2048
SYMDEX_QUALITY_EMBED_MAX_CHUNK_BYTES=512
SYMDEX_RUST_ANALYZER_CMD=rust-analyzer
SYMDEX_DEBUG_DB_LOCKS=1
```

`SYMDEX_DB_PATH` remains the structural SQLite path. Additional database roles
derive sibling SQLite files from that path: `.symdex/symdex-fast.sqlite` for the
fast sqlite-vec projection, `.symdex/symdex-quality.sqlite` for the quality
sqlite-vec projection, `.symdex/symdex-watch.sqlite` for watcher status and
client leases, and `.symdex/symdex-events.sqlite` for index-run summaries and
per-file index events, and `.symdex/symdex-runtime.sqlite` for short-lived
debug-context runtime observation metadata. Existing legacy vector collections
in the structural database remain readable as a compatibility fallback until the
split storage migration is complete.

Rust-analyzer enrichment auto-detects the configured rust-analyzer binary by
default. `SYMDEX_RUST_ANALYZER_CMD` defaults to `rust-analyzer`; if that command
can be launched, `symdex doctor` checks readiness with `rust-analyzer --version`
and indexing reports a metadata-only enrichment plan for changed Rust files. If
the command is missing, enrichment is disabled. Set `SYMDEX_RUST_ANALYZER=0` to
force-disable auto-detected enrichment or `SYMDEX_RUST_ANALYZER=1` to force a
readiness check for the configured command. Current indexing does not run
rust-analyzer project analysis or apply rust-analyzer facts.

`SYMDEX_EMBED_TRUNCATE` defaults to `true`, matching Ollama's embedding API
behavior for oversized local inputs. Symdex also splits large chunks into
overlapping, right-sized embedding segments because some Ollama/model
combinations return context-length errors instead of truncating. Segment vectors
are averaged into one vector for the original structural chunk.

`SYMDEX_EMBED_BATCH_SIZE` defaults to `16`. Symdex splits semantic indexing
requests into batches before calling Ollama `/api/embed`, which avoids oversized
request payloads while preserving result order.

Layered semantic indexing helpers also recognize `SYMDEX_FAST_EMBED_MODEL`,
`SYMDEX_QUALITY_EMBED_MODEL`, `SYMDEX_QUALITY_INDEX`,
`SYMDEX_QUALITY_BATCH_SIZE`, `SYMDEX_QUALITY_WORKERS`, and
`SYMDEX_QUALITY_EMBED_MAX_CHUNK_BYTES`. The fast model defaults to
`nomic-embed-text`; the quality model defaults to `mxbai-embed-large`.
`SYMDEX_EMBED_MODEL` remains the compatibility setting for the current
single-model path and is used as the fast-model fallback when
`SYMDEX_FAST_EMBED_MODEL` is unset. `symdex index <repo>` queues quality jobs
when quality indexing is enabled and the quality model is available.
`symdex index-quality <repo>` manually drains those queued jobs in bounded
batches, then reports whether SQLite activation made quality the active layer or
kept default search on fast. `symdex index --watch <repo>` also performs
cooperative quality catch-up in semantic watch mode when quality indexing is
enabled, processing bounded quality batches during post-batch and idle watch
ticks.

`SYMDEX_EMBED_MAX_CHUNK_BYTES` defaults to `2048` for fast indexing.
`SYMDEX_QUALITY_EMBED_MAX_CHUNK_BYTES` defaults to `512` for quality indexing.
Chunks larger than the active layer limit are sent to Ollama as multiple
overlapping segments no larger than the configured byte limit, except that a
single UTF-8 scalar may exceed a very small limit to avoid invalid text splits.
Secret-blocked chunks remain metadata-only structural evidence and are not sent
to Ollama.

`SYMDEX_DEBUG_DB_LOCKS=1` enables stderr diagnostics for SQLite lock
troubleshooting. Logs include writer-service daemon startup and attach attempts,
database role names, job start/finish events, writer-gate wait and hold
durations, daemon-internal lease acquire/release events, SQLite
read-write/read-only opens, migrations, and watcher client
attach/heartbeat/detach routing. `SYMDEX_DEBUG_WRITER=1` is an alias. The logs
include local DB and repo paths when enabled; leave it unset for normal CLI/TUI
output. For TUI or continuous-indexing sessions, redirect stderr to a file so
diagnostics do not interfere with terminal rendering:

```bash
SYMDEX_DEBUG_DB_LOCKS=1 cargo run -p symdex-cli -- tui . 2>symdex-db-locks.log
```

## Expected commands

```bash
cargo fmt --all
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build --workspace --release
cargo audit
cargo run -p symdex-cli -- init
cargo run -p symdex-cli -- doctor
cargo run -p symdex-cli -- doctor .
cargo run -p symdex-cli -- index .
cargo run -p symdex-cli -- index --full .
cargo run -p symdex-cli -- index --incremental .
cargo run -p symdex-cli -- index-quality .
cargo run -p symdex-cli -- index --offline .
cargo run -p symdex-cli -- index --watch .
cargo run -p symdex-cli -- watch start .
cargo run -p symdex-cli -- watch status .
cargo run -p symdex-cli -- watch stop .
cargo run -p symdex-cli -- index-status .
cargo run -p symdex-cli -- semantic-status .
cargo run -p symdex-cli -- staleness .
cargo run -p symdex-cli -- symbol . "my_symbol"
cargo run -p symdex-cli -- callers . "my_symbol"
cargo run -p symdex-cli -- callees . "my_symbol"
cargo run -p symdex-cli -- call-path . "source_symbol" "target_symbol" 4
cargo run -p symdex-cli -- impact . "my_symbol"
cargo run -p symdex-cli -- explain-change . '[{"path":"src/lib.rs","start_line":1,"end_line":5,"description":"adjust behavior"}]'
cargo run -p symdex-cli -- context-pack . "my_symbol"
cargo run -p symdex-cli -- context-pack . "my_symbol" --mode unified
cargo run -p symdex-cli -- debug-context . panic.log
cargo run -p symdex-cli -- search . "retry logic"
cargo run -p symdex-cli -- tui .
cargo run -p symdex-cli -- serve-mcp
cargo run -p symdex-cli -- serve-mcp --watch .
```

Implemented CLI commands currently include:

Use top-level `--json` or `--output json` with read-only MCP-backed evidence
commands to print the same `symdex.mcp.evidence.v1` envelope used by MCP
`structuredContent`. JSON mode is currently supported for `index-status`,
`search`, `symbol`, `callers`, `callees`, `call-path`, `impact`,
`explain-change`, `context-pack`, and `debug-context`. `semantic-status` supports top-level
`--json` as plain local command JSON for semantic layer readiness metadata.

- `init`: submits a migration job to the database-file-scoped writer service.
- `doctor [repo]`: prints local configuration, filesystem diagnostics, local
  service checks, auto-detected or explicitly overridden rust-analyzer
  enrichment readiness, the active MCP evidence contract version, and repo-specific index
  freshness/provenance readiness when a repo path is provided. Repo-specific
  diagnostics also report semantic quality-layer progress separately from file
  freshness, including pending, running, failed, stale, excluded, embedded, and
  fallback status. Incomplete quality catch-up reports as `pending` while fast
  search remains active; `unreachable` is reserved for blocked quality
  model/service availability. Quality progress is scoped to the latest current
  fast embeddings, so terminal jobs for superseded file snapshots do not block
  readiness. New SQLite databases are initialized in WAL mode and store
  connections use a 30-second busy timeout so diagnostics, TUI refreshes, MCP
  calls, and watcher catch-up can overlap normal local reads and writes without
  changing journal mode during routine migrations. TUI status refreshes and
  watcher status polling avoid schema migrations in their read paths while
  continuous indexing is active.
- `index <repo>`: discovers eligible Rust, C#, JavaScript, TypeScript, TOML,
  YAML, and scoped opt-in JSON files,
  applies built-in excludes and scoped glob-aware `.gitignore` rules with
  negation, hashes file contents, extracts tree-sitter function and method chunks where supported,
  embeds chunk text with local Ollama, creates the sqlite-vec collection if needed,
  and upserts semantic vectors. Use `index --full <repo>` to force all eligible
  files through parsing and embedding, or `index --incremental <repo>` to skip
  unchanged files by content hash. Use `index --offline <repo>` for
  SQLite-backed discovery and chunking without service calls; offline still
  accepts `--full` or `--incremental`. Chunks flagged as
  likely sensitive are counted as `chunks_excluded_from_embedding`, persisted as
  metadata, and omitted from Ollama/sqlite-vec embedding.
  JSON indexing is disabled by default; set `SYMDEX_INDEX_JSON_PATHS` to a
  comma-separated list of repo-relative folders, such as
  `SYMDEX_INDEX_JSON_PATHS=config,.vscode`, to include matching `.json` files in
  those folders and subfolders.
  When rust-analyzer enrichment is auto-detected or explicitly enabled, index
  output also reports readiness and eligible Rust file, symbol, and call counts
  without applying rust-analyzer facts.
- `watch start|status|stop <repo>`: manages the single background watcher for a
  repository. Watchers are client-scoped: TUI, MCP, and CLI attachments keep
  them alive, and they exit after about 10 seconds with no live clients. Live
  clients heartbeat their lease and reinsert it if a transient stale-client prune
  removed the row. The watcher polls local eligible Rust, C#, JavaScript,
  TypeScript, TOML, YAML, and scoped opt-in JSON files,
  debounces event bursts, detects created/modified/deleted paths by content-hash
  snapshots, and reindexes changed content through the incremental semantic
  indexing path.
- `index --watch <repo>`: starts the legacy foreground continuous-indexing loop,
  guarded by the same one-watcher-per-repo state. Watch mode is always
  incremental; use a separate manual `index --full <repo>` when a forced rebuild
  is needed. Semantic watch batches queue quality jobs when quality indexing is
  enabled, then process bounded quality catch-up batches between fast watch
  work. Watch output includes metadata-only quality state, progress,
  completion, and failure events.
- `index-quality <repo>`: manually processes queued quality semantic embedding
  jobs for the latest generation. It claims bounded batches, re-reads files
  from disk, verifies file and chunk hashes, writes quality sqlite-vec points and
  quality `chunk_embeddings` rows, and reports succeeded, failed, and stale
  counts along with `quality_status`, `active_layer`, and
  `activation_reason`. Fast search remains active for partial, stale, blocked,
  or failed quality state; complete current quality coverage switches default
  search to quality.
- `index-status <repo>`: reports SQLite file and chunk counts for the repository.
  When a semantic index has completed, it also reports the latest embedding
  model and vector dimension recorded for that repository. This command is
  read-only and does not run migrations, repository upserts, or ref syncs.
- `semantic-status <repo>`: reports the active default semantic layer, latest
  generation ID, quality readiness state, fallback-to-fast reason, fast and
  quality model/collection metadata, per-layer coverage counts, quality job
  counts, and the latest quality job error when one is recorded. Output remains
  metadata-only and does not include source text or vectors.
- `staleness <repo> [symbol]`: compares indexed file content hashes against the
  current eligible files for implemented languages and reports fresh, stale,
  deleted, missing, and unknown evidence states. With a symbol query, the report
  is scoped to files involved in the matching symbols and compact context pack.
- `vector-verify <repo>`: compares SQLite vector metadata with sqlite-vec payloads
  for the selected semantic layer and reports missing, stale, or orphaned
  points without returning source text. Use `--semantic-layer fast`,
  `--semantic-layer quality`, or `--semantic-layer all` to verify the fast and
  quality collections independently. Verification expects latest-generation
  `chunk_embeddings` manifests; older single-model local databases should run
  `symdex index <repo>` first to create layered fast metadata.
- `vector-repair <repo>`: deletes sqlite-vec orphan points, then re-runs semantic
  indexing when missing or stale fast vector metadata requires rebuilding
  points. With `--semantic-layer quality`, repair runs the quality worker path
  instead so hash verification, quality job state, and activation refresh remain
  centralized. Repair runs through the same writer service as indexing before
  any sqlite-vec or SQLite mutation.
- `symbol <repo> <query>`: searches local SQLite symbols by name or qualified
  name and returns path, line ranges, and provenance metadata.
- `callers <repo> <symbol>` / `callees <repo> <symbol>`: returns direct
  call relationships from the local SQLite index.
- `call-path <repo> <source> <target> [depth]`: traces deterministic bounded
  paths over persisted call edges from a source symbol to a target symbol. The
  depth is clamped to 1-8 hops, cycles are not followed, unresolved terminal
  edges can match the target by callee text, and output stays metadata-only with
  path, line, confidence, resolution, and provenance fields.
- `impact <repo> <symbol>`: prints direct callers, direct callees, bounded
  transitive caller/callee paths, related files, provenance, and staleness
  labels. Evidence rows include trust scores derived from freshness,
  provenance completeness, confidence, and index metadata completeness, plus
  compact reason tags explaining why each evidence row was returned. Likely tests
  list indexed tests with moderate-confidence `test_targets` evidence for the
  queried symbol or its file, with direct-call joins retained as a compatibility
  fallback for older indexes. When package manifests and import references
  match, impact also prints metadata-only `external_dependencies` rows with
  package manager, package name, import path, freshness, trust, and reason tags.
- `explain-change <repo> <targets-json|file|->`: accepts proposed
  `{ path, start_line, end_line, description }` targets, maps the line ranges
  to intersecting indexed symbols, reuses impact analysis, and prints a
  metadata-only pre-edit safety report with direct and transitive relationships,
  likely tests, freshness, trust, and reason tags.
- `context-pack <repo> <symbol> [--mode structural|unified]`: prints compact
  JSON evidence for editing context. The default structural mode preserves
  `symdex.context_pack.v1` and includes focus symbols, direct callers, direct
  callees, involved files, section limits, and notes. Unified mode returns
  `symdex.context_pack.v2`, runs structural retrieval and semantic search in one
  query path, deduplicates symbol/chunk/file evidence, and labels rows with
  `evidence_source` values of `structural`, `semantic`, or `both`. It does not
  include source text. If local semantic services are unavailable, unified mode
  returns structural evidence with a compact `semantic_unavailable:*` note.
- `debug-context <repo> <runtime-input|file|->`: parses runtime failure input
  such as stack traces, panic locations, failing test names, frame symbols, and
  indexed-language file paths, then prints `symdex.debug_context.v1` JSON. Rust
  parsing covers common `cargo test`, panic-hook, `anyhow`, `tracing`, full
  backtrace, and async stack-like output shapes. C# parsing covers
  `at Namespace.Type.Method(...) in path.cs:line N`, and Node/V8 parsing covers
  `at name (path.js:line:column)` plus async JS/TS variants. The pack maps
  frames to indexed files, symbols, calls at the failing line, freshness, and
  provenance when available. Passing `-` reads from stdin; a single existing path reads that
  file; otherwise remaining arguments are treated as inline runtime text. Frame
  matches include trust scores and reason tags, and the pack does not include
  source text. Each run also appends short-lived metadata-only
  `runtime_observations` rows in the runtime database role with an input hash,
  normalized paths, failing test names, match summaries, and expiry metadata for
  repeated-failure comparison.
- `search <repo> <query>`: embeds the query locally through the active semantic
  routing path and returns ranked sqlite-vec matches with scores, paths, line
  ranges, symbol names, active layer metadata, fallback reason, and provenance
  metadata. Text output also prints compact reason tags for each match.
- `tui [repo]`: launches the local terminal UI control panel and starts
  continuous semantic indexing. The current TUI opens an Overview tab backed by
  SQLite metadata and local service configuration, then uses a compact
  repository summary beside or above the active workflow on other tabs. Use
  `Tab` / `Shift+Tab` on the Index tab to select full or incremental scope
  (default incremental), `o` to confirm offline indexing, `s` to confirm
  semantic indexing, `c` to stop or restart continuous indexing, and inspect
  fast/quality readiness progress bars plus fast/quality pending, running, and
  skipped-stale job sparklines. Use `[` / `]` to
  move between the Overview, Index, Storage, Doctor, Query, Calls, and Impact
  tabs, and `Tab` / `Shift+Tab` to toggle view-local
  modes including impact, call-path, context-pack, and debug-context evidence
  modes. In the Doctor tab, `Enter` starts diagnostics when no result rows are
  available, and `r` reruns diagnostics after a completed or failed run. The
  storage
  explorer always shows its own nested tab header for storage overview/index
  coverage/symbol outline/call resolution/embedding coverage/index runs
  timeline/evidence freshness/semantic neighborhood/cross-store health. Use `f`
  in Storage to confirm semantic incremental indexing for stale evidence, `r`
  outside the Doctor tab to refresh repository/storage status, and `q` or `Esc`
  to quit.
- `serve-mcp [--watch <repo>]`: runs the MCP server over stdio. With
  `--watch <repo>`, it starts or attaches the repository background watcher
  before serving tools and holds that watcher lease until the MCP process exits.
  The server exposes
  `symdex_search`, `symdex_find_symbol`, `symdex_callers`, `symdex_callees`,
  `symdex_call_path`, `symdex_impact`, `symdex_context_pack`, and
  `symdex_debug_context`, `symdex_staleness_check`, `symdex_index_status`,
  `symdex_watch_status`, and `symdex_watch_start`. `symdex_context_pack` accepts
  `mode: "unified"` for
  combined structural and semantic context-pack evidence.

`doctor` checks whether sqlite-vec is reachable over REST, whether Ollama is
reachable, whether the configured embedding model is present, and whether vector
dimension probing succeeds. These checks report diagnostic status and do not
mutate repository data.

## CI checks

GitHub Actions runs the production hardening baseline on pull requests and on
pushes to `main`:

- `cargo fmt --all --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test --workspace`
- `cargo build --workspace --release`
- `cargo audit`

The TUI should surface these same diagnostics. Semantic search and semantic
indexing views require local Ollama and sqlite-vec; status, structural queries, and
offline indexing should remain usable without those services.

## Local-only rule

The app should not require network access beyond local loopback services during normal indexing and querying.

## Diagnostics

`symdex doctor` should check:

- SQLite database path is writable
- SQLite database file exists after `symdex init`
- sqlite-vec is reachable
- Ollama is reachable
- embedding model is available
- vector dimension can be determined
- active MCP evidence contract version and watcher-start exception
- auto-detected or explicitly overridden rust-analyzer enrichment readiness
- configured repo root exists
- repo-specific index freshness, provenance consistency, and watcher status
  when a repo is passed

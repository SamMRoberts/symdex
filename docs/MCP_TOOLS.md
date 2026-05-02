# MCP Tools

## Principles

- Tools are read-only for MVP.
- Return compact evidence, not long prose.
- Validate repo roots and all paths.
- Prefer JSON-compatible structured output.
- Include confidence and ambiguity markers.
- Do not return entire source files.

## Tool contracts

The MVP server runs over stdio with JSON-RPC messages and supports the standard
`initialize`, `ping`, `tools/list`, and `tools/call` methods. Tool call
responses include compact JSON in `structuredContent` and mirrored text content
for hosts that only display text results.

Successful tool call `structuredContent` uses the stable cross-agent envelope:

```json
{
  "schema_version": "symdex.mcp.evidence.v1",
  "contract_version": 1,
  "contract": {
    "schema": "symdex.mcp.evidence.v1",
    "version": 1,
    "local_only": true,
    "read_only": true,
    "source_text": "omitted_by_default",
    "index_access": "shared_local_sqlite_and_qdrant",
    "path_policy": "repository_root_required",
    "freshness": "included_when_available",
    "provenance": "included_when_available",
    "trust": "included_when_available",
    "reasons": "included_when_available"
  },
  "data": {
    "results": []
  }
}
```

The examples below show the `data` payload for each tool. The envelope is always
present on successful tool calls and is also advertised by `initialize` as
`symdexContract`. The current schema and version are defined in `symdex-core`
so CLI, TUI, diagnostics, and MCP share the same contract identifier.
Parser-version examples below use Rust, but C#, JavaScript, and TypeScript
evidence uses each language's parser version string.

The CLI mirrors this envelope when run with top-level `--json` or
`--output json` for MCP-backed read-only commands. For example,
`symdex --json search <repo> <query>` prints the same
`symdex.mcp.evidence.v1` object that MCP returns in `structuredContent`.

### `symdex_search`

Semantic search over indexed chunks.

Input:

```json
{
  "repo": "/path/to/repo",
  "query": "where is retry logic handled?",
  "limit": 8
}
```

Output:

```json
{
  "results": [
    {
      "path": "crates/foo/src/lib.rs",
      "start_line": 42,
      "end_line": 88,
      "symbol": "foo::retry::run_with_backoff",
      "score": 0.82,
      "chunk_kind": "function",
      "text_hash": "sha256:...",
      "freshness": "fresh",
      "trust": {
        "score": 0.93,
        "level": "high",
        "factors": ["freshness:fresh", "confidence:0.82"]
      },
      "reasons": [
        "semantic_vector_match",
        "semantic_score:0.8200",
        "path:crates/foo/src/lib.rs",
        "chunk_kind:function",
        "symbol_payload:foo::retry::run_with_backoff"
      ],
      "provenance": {
        "content_hash": "sha256:...",
        "index_run_id": "repo-semantic-...",
        "parser_version": "tree-sitter-rust-...",
        "indexed_at": "2026-04-30T12:00:00Z",
        "embedding_model": "nomic-embed-text",
        "embedding_dimension": 768,
        "embedded_at": null
      }
    }
  ]
}
```

The search tool embeds the query with the configured local Ollama model and
queries the local Qdrant collection. It returns chunk metadata, freshness state,
trust, reason tags, and provenance only; it does not return source excerpts in
the current MVP.

### `symdex_find_symbol`

Find symbols by exact or fuzzy name.

Input:

```json
{
  "repo": "/path/to/repo",
  "name": "run_with_backoff",
  "limit": 10
}
```

Output rows include `freshness`, `trust`, `reasons`, and `provenance` with
content hash, index run ID, parser version, and indexed timestamp.

### `symdex_callers`

Find direct callers of a symbol.

Input:

```json
{
  "repo": "/path/to/repo",
  "symbol": "foo::retry::run_with_backoff"
}
```

Output rows include call confidence/resolution data, `freshness`, `trust`,
`reasons`, and `provenance`.

### `symdex_callees`

Find direct callees from a symbol.

Input:

```json
{
  "repo": "/path/to/repo",
  "symbol": "foo::retry::run_with_backoff"
}
```

Output rows include call confidence/resolution data, `freshness`, `trust`,
`reasons`, and `provenance`.

### `symdex_call_path`

Trace compact bounded call paths between a source symbol and a target symbol.

Input:

```json
{
  "repo": "/path/to/repo",
  "source": "foo::api::handler",
  "target": "foo::db::save",
  "max_depth": 4
}
```

Output:

```json
{
  "repository_id": "stable-repo-id",
  "source": "foo::api::handler",
  "target": "foo::db::save",
  "max_depth": 4,
  "paths": [
    {
      "hops": 2,
      "min_confidence": 1.0,
      "terminal_resolution_status": "resolved_exact",
      "edges": [
        {
          "caller_symbol_qualified_name": "foo::api::handler",
          "callee_symbol_qualified_name": "foo::service::run",
          "callee_text": "run",
          "caller_path": "src/api.rs",
          "caller_start_line": 10,
          "caller_end_line": 30,
          "call_line": 18,
          "confidence": 1.0,
          "resolution_status": "resolved_exact",
          "freshness": "fresh",
          "trust": {
            "score": 1.0,
            "level": "high",
            "factors": ["freshness:fresh", "confidence:1.00"]
          },
          "reasons": [
            "relationship:call_path_edge",
            "persisted_path_edge",
            "caller:foo::api::handler",
            "callee:foo::service::run",
            "resolution_status:resolved_exact",
            "confidence:1.00"
          ],
          "provenance": {
            "content_hash": "sha256:...",
            "index_run_id": "repo-semantic-...",
            "parser_version": "tree-sitter-rust-...",
            "indexed_at": "2026-04-30T12:00:00Z",
            "embedding_model": null,
            "embedding_dimension": null,
            "embedded_at": null
          }
        }
      ]
    }
  ]
}
```

`max_depth` is clamped to 1-8 hops. Traversal follows resolved persisted call
edges, avoids cycles, returns paths in deterministic index order, and keeps
unresolved or ambiguous edges as terminal evidence when their `callee_text`
matches the target query. Path and edge rows include reason tags that explain
the relationship and traversal evidence. The tool does not return source text.

### `symdex_impact`

Return likely affected files and symbols.

Input:

```json
{
  "repo": "/path/to/repo",
  "symbol": "foo::retry::run_with_backoff",
  "depth": 2
}
```

Output separates:

- direct callers
- direct callees
- transitive callers
- transitive callees
- related files
- same-file symbols
- tests likely to cover the symbol
- unresolved candidates

Direct and transitive evidence rows include provenance, freshness labels, and
trust scores. Related-file rows include path, relationship count, freshness,
trust, and provenance.
`tests_likely` contains indexed Rust test qualified names when a discovered test
directly calls the queried symbol through resolved call evidence. When no direct
indexed test evidence is available, the list stays empty and a note explains
that no likely-test evidence was found.

### `symdex_context_pack`

Return compact metadata-only evidence for editing context.

Input:

```json
{
  "repo": "/path/to/repo",
  "symbol": "foo::retry::run_with_backoff",
  "limit": 8
}
```

Output:

```json
{
  "format": "symdex.context_pack.v1",
  "repository_id": "stable-repo-id",
  "query": "foo::retry::run_with_backoff",
  "focus_symbols": [],
  "direct_callers": [],
  "direct_callees": [],
  "files": [],
  "limits": {
    "max_symbols": 8,
    "max_callers": 8,
    "max_callees": 8
  },
  "notes": [
    "metadata_only_no_source_text",
    "direct_relationships_only"
  ]
}
```

The context pack is intentionally compact and does not return source text. It is
currently structural only; semantic hits can be combined by calling
`symdex_search` separately.

Planned next step: add a `mode: "unified"` option that combines structural
context-pack evidence with semantic search evidence in one call. Unified rows
should identify whether they came from `structural`, `semantic`, or `both`
evidence paths while preserving compact metadata-only output.

### `symdex_debug_context`

Build a compact debugging evidence pack from runtime failure input.

Input:

```json
{
  "repo": "/path/to/repo",
  "input": "thread 'main' panicked at src/lib.rs:42:5:\\nstack backtrace:\\n0: crate::run\\n   at src/lib.rs:42:5",
  "limit": 8
}
```

Output:

```json
{
  "format": "symdex.debug_context.v1",
  "repository_id": "stable-repo-id",
  "frames": [
    {
      "frame": {
        "ordinal": 0,
        "symbol": "crate::run",
        "path": "src/lib.rs",
        "line": 42,
        "column": 5
      },
      "normalized_path": "src/lib.rs",
      "file_freshness": "fresh",
      "file_provenance": {
        "content_hash": "sha256:...",
        "index_run_id": "repo-...",
        "parser_version": "tree-sitter-rust-...",
        "indexed_at": "2026-04-30T12:00:00Z",
        "embedding_model": null,
        "embedding_dimension": null,
        "embedded_at": null
      },
      "matched_symbols": [],
      "calls_at_line": [],
      "matched": true
    }
  ],
  "call_paths_between_frames": [],
  "likely_tests": [],
  "limits": {
    "max_frames": 8,
    "max_symbols_per_frame": 8,
    "max_calls_per_frame": 8,
    "max_call_paths_between_frames": 8
  },
  "notes": [
    "metadata_only_no_source_text",
    "likely_tests_mapped_to_indexed_tests"
  ]
}
```

The tool parses panic/file locations, stack-frame symbols, indexed-language file
paths, and failing test names. It maps frames to indexed SQLite file/symbol/call
evidence, maps failing test names to indexed Rust tests when available, keeps
unmatched runtime test names as fallbacks, adds freshness, trust, and
provenance, and returns source-free metadata only.

Planned parser expansion: keep the Rust parser behavior and add conservative C#
and Node/V8 stack frame patterns. Unmapped frames must remain visible with an
explicit status instead of being dropped.

### `symdex_index_status`

Return local SQLite index counts.

Input:

```json
{
  "repo": "/path/to/repo"
}
```

Output:

```json
{
  "repository_id": "stable-repo-id",
  "files_indexed": 42,
  "chunks_indexed": 120,
  "symbols_indexed": 80,
  "calls_indexed": 240,
  "embedding_model": "nomic-embed-text",
  "embedding_dimension": 768,
  "last_indexed_at": "2026-04-30T12:00:00Z"
}
```

## Future write-capable tools

Do not add mutation tools until a dedicated design doc exists. Candidate future tools include:

- request reindex
- clear index
- persist context pack

## Planned MCP tools

These tools are not implemented yet. They are documented here so future work
keeps the same local-only, compact, metadata-first contract.

### `symdex_staleness_check`

Read-only tool for explicit index freshness checks.

Input:

```json
{
  "repo": "/path/to/repo",
  "symbol": "optional::symbol",
  "paths": ["optional/path.rs"]
}
```

Rules:

- Accept either `symbol`, `paths`, both when they are compatible, or neither for
  a repository-wide report.
- Reuse the same staleness logic as `symdex staleness`.
- Enforce repository root boundaries and fail closed on ambiguous paths.
- Return freshness states `fresh`, `stale`, `deleted`, `missing`, or `unknown`.
- Include indexed and current hashes for stale files.
- Do not return source text.

### `symdex_semantic_neighborhood`

Read-only tool for finding vector-nearest chunks to an indexed chunk or symbol.

Input:

```json
{
  "repo": "/path/to/repo",
  "symbol": "optional::symbol",
  "chunk_id": "optional-chunk-id",
  "limit": 8
}
```

Rules:

- Require exactly one of `symbol` or `chunk_id`.
- Look up the existing vector point and query local Qdrant for nearest
  neighbors.
- Return path, line range, symbol, chunk kind, score, freshness, trust, reason
  tags, and provenance.
- Do not embed source text into the response or return vectors.

### `symdex_request_reindex`

Potential future write-capable tool. Do not implement until a design doc is
approved.

Required design decisions before code:

- caller trust and confirmation model
- repo and path scoping
- offline structural default behavior
- explicit `semantic: true` opt-in for Qdrant/Ollama work
- concurrency with manual and continuous indexing
- index run ID reporting and failure semantics

### `symdex_explain_change`

Potential future read-only pre-edit safety tool. Do not implement until a design
doc is approved.

Expected shape:

- Input is a repo plus proposed change targets containing path, line range, and
  short description.
- The tool maps changed ranges to indexed symbols, runs impact analysis,
  deduplicates evidence, and returns likely affected symbols, files, tests, and
  call paths.
- Output stays metadata-only with freshness, trust, reason tags, and provenance.

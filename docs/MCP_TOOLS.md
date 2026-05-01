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
and provenance only; it does not return source excerpts in the current MVP.

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

Output rows include `freshness` plus `provenance` with content hash, index run
ID, parser version, and indexed timestamp.

### `symdex_callers`

Find direct callers of a symbol.

Input:

```json
{
  "repo": "/path/to/repo",
  "symbol": "foo::retry::run_with_backoff"
}
```

Output rows include call confidence/resolution data, `freshness`, and
`provenance`.

### `symdex_callees`

Find direct callees from a symbol.

Input:

```json
{
  "repo": "/path/to/repo",
  "symbol": "foo::retry::run_with_backoff"
}
```

Output rows include call confidence/resolution data, `freshness`, and
`provenance`.

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

Output should separate:

- direct callers
- transitive callers
- same-file symbols
- tests likely to cover the symbol
- unresolved candidates

The current MVP fills direct callers and direct callees. The other buckets are
present but empty until deeper impact analysis is implemented.

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

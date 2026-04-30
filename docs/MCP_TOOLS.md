# MCP Tools

## Principles

- Tools are read-only for MVP.
- Return compact evidence, not long prose.
- Validate repo roots and all paths.
- Prefer JSON-compatible structured output.
- Include confidence and ambiguity markers.
- Do not return entire source files.

## Tool contracts

### `symdex.search`

Semantic search over indexed chunks.

Input:

```json
{
  "repo": "/path/to/repo",
  "query": "where is retry logic handled?",
  "limit": 8,
  "filters": {
    "language": "rust",
    "path_prefix": "crates/"
  }
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
      "snippet": "compact excerpt"
    }
  ]
}
```

### `symdex.find_symbol`

Find symbols by exact or fuzzy name.

Input:

```json
{
  "repo": "/path/to/repo",
  "name": "run_with_backoff",
  "limit": 10
}
```

### `symdex.callers`

Find direct callers of a symbol.

Input:

```json
{
  "repo": "/path/to/repo",
  "symbol": "foo::retry::run_with_backoff",
  "include_unresolved_candidates": true
}
```

### `symdex.callees`

Find direct callees from a symbol.

Input:

```json
{
  "repo": "/path/to/repo",
  "symbol": "foo::retry::run_with_backoff"
}
```

### `symdex.impact`

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

### `symdex.index_status`

Return index freshness and model metadata.

## Future write-capable tools

Do not add mutation tools until a dedicated design doc exists. Candidate future tools include:

- request reindex
- clear index
- generate context pack

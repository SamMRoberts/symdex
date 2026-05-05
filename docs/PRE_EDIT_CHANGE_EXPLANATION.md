# Pre-Edit Change Explanation

## Purpose

`symdex_explain_change` gives an agent a deterministic safety briefing before
editing files. The tool accepts proposed line-range targets, maps them to
indexed symbols, reuses existing impact analysis, and returns compact
metadata-only evidence about likely affected code and tests.

This is a read-only analysis tool. It does not reindex, write files, execute
repository code, call hosted services, or store proposed changes.

## Input

The MCP input is:

```json
{
  "repo": "/path/to/repo",
  "targets": [
    {
      "path": "src/lib.rs",
      "start_line": 10,
      "end_line": 24,
      "description": "Change parser error handling"
    }
  ]
}
```

Rules:

- `repo` must be a repository root accepted by `RepoRoot`.
- `targets` must contain 1-25 entries.
- `path` may be repo-relative, or an absolute path that resolves inside the
  repository root.
- `start_line` and `end_line` are 1-based inclusive lines, and `end_line` must
  be greater than or equal to `start_line`.
- `description` is required, trimmed, and returned as metadata. It is not used
  for semantic search or source inspection.

## Mapping

For each target, the query layer normalizes the path and finds indexed symbols
whose stored line range intersects the proposed edit range. Ref-scoped indexes
use the active `ref_files` manifest when one exists; older indexes use the
repository-wide fallback.

Each matched symbol gets freshness, trust, provenance, and reason tags. When a
target has no intersecting symbols, the output keeps the target with
`matched = false` and an explanatory note. The tool never falls back to source
text.

## Impact Union

For every matched symbol, `symdex_explain_change` runs the same structural
impact analysis used by `symdex_impact`:

- direct callers
- direct callees
- bounded transitive callers
- bounded transitive callees
- related files
- likely tests from indexed test-target evidence

The result is deduplicated across all matched symbols. Deduplication keys are
metadata identifiers such as symbol IDs, paths, line ranges, call IDs, and call
path edge IDs. The output preserves reason tags and trust metadata from the
underlying evidence.

## Output

The output format is `symdex.explain_change.v1` and includes:

- normalized proposed targets
- intersecting symbols per target
- deduplicated affected symbols
- direct and transitive impact evidence
- related files
- likely tests
- freshness, trust, provenance, and reason tags
- notes for unmatched targets or unavailable evidence

The response is intentionally compact and context-window safe. It does not
include source text, diffs, full files, embeddings, or raw runtime/log input.

## MCP Contract

`symdex_explain_change` is local-only, read-only, and metadata-only. It is safe
to call before editing because it does not mutate the repository, index, vector
store, or runtime-observation cache.

Future write-capable variants, such as reindex-before-explain or
post-edit verification, require a separate design covering authorization,
confirmation, writer coordination, and failure behavior.

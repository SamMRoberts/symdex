# App Spec

## Product

symdex is a local-first codebase intelligence system for AI coding agents.

It answers questions such as:

- Which code is relevant to this requested change?
- Which symbols define this behavior?
- Which functions call or are called by this symbol?
- What files and tests are likely affected?
- What compact context should an AI agent receive before editing?
- How complete and healthy is the local structural and semantic index?
- Which files, chunks, symbols, calls, and embeddings explain the evidence?
- Which explicit call paths connect two symbols?
- Is the evidence fresh relative to the current working tree?
- Which indexed facts explain a stack trace, panic, or failing test?

## Non-goals for MVP

- No hosted SaaS.
- No cloud embeddings.
- No web UI.
- No mutation tools over user repositories.
- No perfect whole-language type inference.
- No language support beyond Rust, C#, JavaScript, and TypeScript until those
  paths are reliable.

## MVP user flow

1. User installs local dependencies.
2. User runs `symdex init`.
3. User runs `symdex index /path/to/repo`.
4. User can optionally enable continuous indexing so modified or newly created eligible files are automatically reindexed.
5. User asks semantic or structural questions through CLI, TUI, or MCP.
6. User can inspect local status, diagnostics, indexing controls, queries, and context packs in the TUI.
7. User can visualize SQLite structural data and sqlite-vec semantic coverage in the TUI without exposing source text.
8. Agent receives compact ranked evidence with file paths and line ranges.

## Primary commands

```bash
symdex init
symdex index <repo>
symdex index --watch <repo>
symdex watch start <repo>
symdex watch status <repo>
symdex watch stop <repo>
symdex search <repo> "query"
symdex symbol <repo> <symbol>
symdex callers <repo> <symbol>
symdex callees <repo> <symbol>
symdex impact <repo> <symbol>
symdex explain-change <repo> <targets-json|file|->
symdex context-pack <repo> <symbol>
symdex tui [repo]
symdex doctor [repo]
symdex serve-mcp
symdex serve-mcp --watch <repo>
```

## Success criteria

- Indexing a small Rust repo completes locally without network access after setup.
- C#, JavaScript, and TypeScript indexing use the same local-only contracts as
  Rust for discovery, parsing, chunking, symbols, calls, provenance, and
  continuous indexing.
- Continuous indexing uses one local background watcher per repository. Watchers
  run only while TUI, MCP, or CLI clients hold leases, can be toggled on and
  off, and reindex modified or newly created eligible files without source
  execution.
- Only one local process writes to a repository's SQLite/sqlite-vec database at
  a time. Write-capable indexing, quality, repair, cleanup, and future mutating
  MCP operations coordinate through that writer or refuse instead of racing a
  second writer. The debug-context runtime observation cache is a narrow
  metadata-only append exception and must never store pasted logs or source text.
- Semantic search returns relevant function-level chunks.
- Symbol search returns exact path and line ranges.
- Call graph records direct calls where syntax makes them obvious.
- MCP tools return compact JSON evidence that a coding agent can use immediately.
- Pre-edit explanation maps proposed line-range changes to indexed symbols,
  impact evidence, likely tests, freshness, trust, and reason tags without
  storing source text or mutating the index.
- TUI provides a local keyboard-first control panel for indexing, storage health, diagnostics, queries, impact, and context packs.
- TUI makes index health inspectable with local visualizations for coverage,
  fast and quality semantic readiness progress, file details, symbol outlines,
  call resolution, embedding coverage, and index runs.
- TUI storage visualizations are grouped under an always-visible nested storage tab header so users can see the available storage panes while inspecting any one pane.
- TUI widget choices prefer built-in ratatui widgets first; third-party widgets are limited to clear metadata-first improvements such as tree navigation or multiline debug input.
- Unresolved or ambiguous relationships are labeled instead of fabricated.

## Future product directions

These are planned but not yet implemented. See `FUTURE_FEATURES.md` for the
feature contracts.

- Unified context packs that merge structural and semantic evidence in one
  agent-facing response.
- Multi-language test discovery for C#, JavaScript, and TypeScript.
- Runtime-to-source mapping for C# and Node/V8 stack traces in addition to the
  existing Rust-focused debug context behavior.
- Future write-capable reindex requests only after an explicit design doc and
  trust/confirmation model.
- Future languages beyond Rust, C#, JavaScript, and TypeScript, added only
  through the same evidence contracts after active targets are reliable.

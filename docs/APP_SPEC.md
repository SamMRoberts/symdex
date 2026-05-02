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
7. User can visualize SQLite structural data and Qdrant semantic coverage in the TUI without exposing source text.
8. Agent receives compact ranked evidence with file paths and line ranges.

## Primary commands

```bash
symdex init
symdex index <repo>
symdex index --watch <repo>
symdex search <repo> "query"
symdex symbol <repo> <symbol>
symdex callers <repo> <symbol>
symdex callees <repo> <symbol>
symdex impact <repo> <symbol>
symdex context-pack <repo> <symbol>
symdex tui [repo]
symdex doctor [repo]
symdex serve-mcp
```

## Success criteria

- Indexing a small Rust repo completes locally without network access after setup.
- C#, JavaScript, and TypeScript indexing use the same local-only contracts as
  Rust for discovery, parsing, chunking, symbols, calls, provenance, and
  continuous indexing.
- Continuous indexing can be toggled on and off and reindexes modified or newly created eligible files without source execution.
- Semantic search returns relevant function-level chunks.
- Symbol search returns exact path and line ranges.
- Call graph records direct calls where syntax makes them obvious.
- MCP tools return compact JSON evidence that a coding agent can use immediately.
- TUI provides a local keyboard-first control panel for indexing, storage health, diagnostics, queries, impact, and context packs.
- TUI makes index health inspectable with local visualizations for coverage, file details, symbol outlines, call resolution, embedding coverage, and index runs.
- TUI storage visualizations are grouped under an always-visible nested storage tab header so users can see the available storage panes while inspecting any one pane.
- Unresolved or ambiguous relationships are labeled instead of fabricated.

## Future product directions

These are planned but not yet implemented. See `FUTURE_FEATURES.md` for the
feature contracts.

- Unified context packs that merge structural and semantic evidence in one
  agent-facing response.
- A read-only MCP staleness check so agents can explicitly verify whether
  index evidence is fresh before querying or requesting reindexing.
- More correct `.gitignore` glob and negation handling during discovery.
- Multi-language test discovery for C#, JavaScript, and TypeScript.
- Runtime-to-source mapping for C# and Node/V8 stack traces in addition to the
  existing Rust-focused debug context behavior.
- Future write-capable reindex requests only after an explicit design doc and
  trust/confirmation model.
- Pre-edit change explanation that maps proposed line-range edits to impact,
  likely tests, freshness, and trust before an agent changes files.
- Future languages beyond Rust, C#, JavaScript, and TypeScript, added only
  through the same evidence contracts after active targets are reliable.

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

## Non-goals for MVP

- No hosted SaaS.
- No cloud embeddings.
- No web UI.
- No mutation tools over user repositories.
- No perfect whole-language type inference.
- No multi-language support beyond Rust until the Rust path is reliable.

## MVP user flow

1. User installs local dependencies.
2. User runs `symdex init`.
3. User runs `symdex index /path/to/repo`.
4. User asks semantic or structural questions through CLI, TUI, or MCP.
5. User can inspect local status, diagnostics, indexing controls, queries, and context packs in the TUI.
6. User can visualize SQLite structural data and Qdrant semantic coverage in the TUI without exposing source text.
7. Agent receives compact ranked evidence with file paths and line ranges.

## Primary commands

```bash
symdex init
symdex index <repo>
symdex search <repo> "query"
symdex symbol <repo> <symbol>
symdex callers <repo> <symbol>
symdex callees <repo> <symbol>
symdex impact <repo> <symbol>
symdex context-pack <repo> <symbol>
symdex tui [repo]
symdex doctor
symdex serve-mcp
```

## Success criteria

- Indexing a small Rust repo completes locally without network access after setup.
- Semantic search returns relevant function-level chunks.
- Symbol search returns exact path and line ranges.
- Call graph records direct calls where syntax makes them obvious.
- MCP tools return compact JSON evidence that a coding agent can use immediately.
- TUI provides a local keyboard-first control panel for indexing, diagnostics, queries, impact, and context packs.
- TUI makes index health inspectable with local visualizations for coverage, file details, symbol outlines, call resolution, embedding coverage, and index runs.
- Unresolved or ambiguous relationships are labeled instead of fabricated.

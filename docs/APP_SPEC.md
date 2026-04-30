# App Spec

## Product

symdex is a local-first codebase intelligence system for AI coding agents.

It answers questions such as:

- Which code is relevant to this requested change?
- Which symbols define this behavior?
- Which functions call or are called by this symbol?
- What files and tests are likely affected?
- What compact context should an AI agent receive before editing?

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
4. User asks semantic or structural questions through CLI or MCP.
5. Agent receives compact ranked evidence with file paths and line ranges.

## Primary commands

```bash
symdex init
symdex index <repo>
symdex search <repo> "query"
symdex symbol <repo> <symbol>
symdex callers <repo> <symbol>
symdex callees <repo> <symbol>
symdex impact <repo> <symbol>
symdex doctor
symdex serve-mcp
```

## Success criteria

- Indexing a small Rust repo completes locally without network access after setup.
- Semantic search returns relevant function-level chunks.
- Symbol search returns exact path and line ranges.
- Call graph records direct calls where syntax makes them obvious.
- MCP tools return compact JSON evidence that a coding agent can use immediately.
- Unresolved or ambiguous relationships are labeled instead of fabricated.

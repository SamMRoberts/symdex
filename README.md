# symdex

symdex is a local-first codebase intelligence system for AI coding agents.

It indexes Rust, C#, JavaScript, and TypeScript repositories structurally and
semantically so agents can reason from compact local evidence: paths, line
ranges, symbols, calls, scores, diagnostics, freshness, provenance, and context
packs. Source text stays local and is not shown by default in agent-facing
outputs or the TUI.

Current implementation status: Rust workspace with tree-sitter indexing for
Rust, C#, JavaScript, and TypeScript, deterministic hashing, path normalization,
SQLite storage, Qdrant vector storage, local Ollama embeddings, structural and
semantic CLI queries, continuous indexing, a native terminal UI, debug context
packs, compact context packs, and a read-only MCP stdio server.

## Try it

```bash
cargo run -p symdex-cli -- init
cargo run -p symdex-cli -- doctor
cargo run -p symdex-cli -- index --offline tests/fixtures/rust_basic
cargo run -p symdex-cli -- index-status tests/fixtures/rust_basic
cargo run -p symdex-cli -- symbol tests/fixtures/rust_basic add
cargo run -p symdex-cli -- callers tests/fixtures/rust_basic add
cargo run -p symdex-cli -- impact tests/fixtures/rust_basic add
cargo run -p symdex-cli -- context-pack tests/fixtures/rust_basic add
cargo run -p symdex-cli -- staleness tests/fixtures/rust_basic
cargo run -p symdex-cli -- tui tests/fixtures/rust_basic
cargo run -p symdex-cli -- serve-mcp
```

With Ollama and Qdrant running locally, `index <repo>` embeds Rust chunks and
upserts vectors, and `search <repo> <query>` returns ranked path and line-range
evidence. Use `index --offline <repo>` for SQLite-backed structural indexing
without service calls.

## Commands

```bash
cargo run -p symdex-cli -- index <repo>
cargo run -p symdex-cli -- index --watch <repo>
cargo run -p symdex-cli -- search <repo> "retry logic"
cargo run -p symdex-cli -- symbol <repo> <symbol>
cargo run -p symdex-cli -- callers <repo> <symbol>
cargo run -p symdex-cli -- callees <repo> <symbol>
cargo run -p symdex-cli -- impact <repo> <symbol>
cargo run -p symdex-cli -- context-pack <repo> <symbol>
cargo run -p symdex-cli -- tui [repo]
cargo run -p symdex-cli -- serve-mcp
```

`index --watch <repo>` runs continuous indexing for created or modified eligible
Rust, C#, JavaScript, and TypeScript files. It uses polling, debounce, content
hashes, and the same ignore and path-boundary rules as manual indexing. Stop the
non-interactive watch process with `Ctrl+C`.

## TUI

`symdex tui [repo]` opens a local terminal control panel built with `ratatui` and
`crossterm`.

- `[` / `]` move between primary tabs: Index, Storage, Doctor, Query, Calls,
  and Impact.
- `Tab` / `Shift+Tab` switch modes inside the active view, such as storage
  panes, query modes, callers/callees, or impact/context-pack.
- `o` starts offline indexing confirmation, `s` starts semantic indexing
  confirmation, and `c` toggles continuous indexing confirmation.
- Continuous indexing shows an animated activity indicator while enabled.
- The footer separates shortcut hints from status messages into distinct
  terminal containers.
- Storage visualizations include explorer, index coverage, symbol outline, call
  resolution, embedding coverage, index runs, semantic neighborhood, and
  cross-store health.

## Local Services

Semantic indexing and semantic search require:

- Qdrant on `localhost:6333`
- Ollama on `localhost:11434`
- the `nomic-embed-text` model installed in Ollama

Structural indexing, SQLite status, symbol queries, call queries, impact, context
packs, diagnostics, and offline TUI workflows remain local and usable without
Qdrant or Ollama.

## MCP

`serve-mcp` exposes the same read-only evidence surface to coding agents through
MCP tools:

- `symdex_search`
- `symdex_find_symbol`
- `symdex_callers`
- `symdex_callees`
- `symdex_impact`
- `symdex_context_pack`
- `symdex_index_status`
- `symdex_debug_context`

The next planned MCP additions are a unified context-pack mode and a read-only
`symdex_staleness_check` tool, followed by `.gitignore` glob correctness and
broader test/runtime mapping work.

# Agent Guidance

- Keep Symdex local-only: no network calls, telemetry, LLMs, embeddings, or vector databases.
- Treat `.symdex/` and SQLite files as generated runtime cache; never commit them.
- Prefer deterministic Tree-sitter and SQL evidence over inferred semantic claims.
- Add tests under `tests/` using artificial fixtures, not private user repositories.
- Keep CLI, TUI, query services, database access, and parser/indexer code separated so future MCP tools can call the same services.
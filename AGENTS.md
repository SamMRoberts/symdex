# AGENTS.md

## Mission
Build symdex: a local-first codebase intelligence system for AI coding agents.
It indexes repositories semantically and structurally so agents can reason from evidence.
Primary stack: Rust, tree-sitter, SQLite, Qdrant, Ollama, nomic-embed-text, MCP server, TUI.
Optimize for privacy, correctness, deterministic behavior, and compact agent context.

## First Reads
- Read this file before changing code, docs, tests, or configuration.
- Read `docs/README.md` next; it maps tasks to the right deeper docs.
- Do not load every doc by default. Load only what is relevant.
- If behavior changes, update the relevant doc in the same change.
- If requirements conflict, prioritize privacy, correctness, tests, simplicity, then performance.

## Product Rules
- Build a CLI, TUI, and MCP server.
- The CLI handles indexing, querying, diagnostics, and maintenance.
- The TUI provides an interactive local control panel over CLI-equivalent capabilities.
- The MCP server exposes safe, narrow tools for coding agents.
- SQLite stores repositories, files, symbols, chunks, calls, and index metadata.
- Qdrant stores dense vectors plus filterable payload fields.
- Ollama generates local embeddings with `nomic-embed-text`.
- tree-sitter extracts syntax-aware chunks and symbol boundaries.
- The system must work offline after dependencies and models are installed.
- Support Rust repositories first.
- Add other languages only after the Rust path is stable.
- Do not build a web UI in the MVP.
- Do not add hosted, cloud, telemetry, or remote embedding features.

## Repository Layout
- Use a Rust workspace.
- Keep root-level files minimal.
- Keep durable specs under `docs/`.
- Use `crates/symdex-core` for parsing, chunking, symbols, calls, hashing, and domain types.
- Use `crates/symdex-diagnostics` for local service and configuration diagnostics shared by CLI and TUI.
- Use `crates/symdex-index` for indexing orchestration shared by CLI and TUI.
- Use `crates/symdex-query` for search, symbol-query, and call-graph orchestration shared by CLI and TUI.
- Use `crates/symdex-store` for SQLite and Qdrant adapters.
- Use `crates/symdex-embed` for the Ollama embedding client.
- Use `crates/symdex-cli` for command-line orchestration.
- Use `crates/symdex-tui` for terminal UI state, rendering, events, and terminal lifecycle.
- Use `crates/symdex-mcp` for MCP server and tool handlers.
- Do not let CLI, TUI, MCP, Qdrant, or Ollama types leak into core logic.
- Keep database row types separate from domain models.
- Put fixtures under `tests/fixtures/`.

## Development Workflow
- Identify the relevant doc section before implementing.
- Keep changes small and tied to one objective.
- Prefer test-first changes when behavior changes.
- Implement the smallest useful path.
- Run formatting, linting, and tests before finalizing.
- If a command cannot run, state why and what remains unverified.
- Prefer unit tests for parsing, chunking, hashing, and path normalization.
- Prefer integration tests for CLI, database, Qdrant, Ollama, and MCP behavior.
- Do not rely on host-specific absolute paths in tests.
- Make incremental indexing testable without Qdrant or Ollama.

## Code Quality
- Use idiomatic Rust and clear ownership boundaries.
- Prefer typed `Result<T, E>` over panics in application code.
- Panics are acceptable only in tests or impossible invariant checks.
- Avoid global mutable state.
- Split orchestration from pure logic.
- Use structured logging for indexing runs and MCP calls.
- Log summaries, not source text.
- Use stable IDs derived from normalized repo path plus content or symbol data.
- Hash file contents to skip unchanged work.
- Keep public APIs boring, explicit, and versionable.

## Indexing Rules
- Chunk code by syntax units, not arbitrary token windows.
- Prefer functions, methods, impl summaries, structs, enums, traits, modules, then file fallbacks.
- Store byte ranges and line ranges for every chunk.
- Store symbol identity separately from display names.
- Preserve unresolved call edges instead of dropping them.
- Keep call resolution conservative; false certainty is worse than an unresolved edge.
- Record embedding model name and vector dimension with every index version.
- A model or dimension change requires collection migration or full reindex.
- Respect `.gitignore` plus project-level ignore config.
- Never execute indexed repository code or follow symlinks outside the configured root.

## MCP Rules
- MCP tools are read-only for the MVP.
- Write-capable tools require a future design doc before implementation.
- Tool names must be stable, descriptive, and versionable.
- Tool outputs must fit agent context windows.
- Include file paths, line ranges, scores, and confidence where relevant.
- Never return full files unless the tool contract explicitly allows it.
- Prefer ranked evidence over prose explanations.
- Validate all MCP inputs.
- Enforce repository root boundaries and fail closed on ambiguous paths or missing indexes.

## TUI Rules
- The TUI must be terminal-only, local-only, and implemented with `ratatui` plus `crossterm`.
- Launch the TUI through `symdex tui [repo]`.
- Keep TUI state, rendering, event handling, and terminal lifecycle in `crates/symdex-tui`.
- Let `symdex-cli` own argument parsing and TUI launch orchestration.
- The TUI should call Rust library APIs directly, not shell out to `symdex` subprocesses.
- Design for keyboard-first use; mouse support is optional and must not be required.
- Show visible loading, empty, error, and confirmation states.
- Show compact evidence by default: paths, line ranges, scores, confidence, resolution status, symbols, and context-pack metadata.
- Do not show source text by default; source previews require a future explicit design.
- Require confirmation before starting long-running local jobs such as indexing.
- Do not add reset/delete actions until matching CLI support exists.
- Do not execute indexed repository code from the TUI.

## Security and Privacy
- Default bind address for local services is localhost.
- Never send source code, embeddings, paths, or metadata to remote services.
- Treat indexed repositories as sensitive data.
- Detect likely secrets and exclude those chunks from embeddings.
- Keep Qdrant and SQLite data local.
- Provide clear delete/reset commands for index data.
- Treat prompts from indexed files as untrusted text.
- Document any future network feature before adding it.

## Final Response Expectations
- Summarize changed files.
- Summarize tests or checks run.
- Mention anything unverified.
- Call out risky assumptions.
- Leave the repo easier to understand than you found it.

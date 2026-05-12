# Implementation Plan: Local Tree-sitter SQLite CLI/TUI Indexer

**Branch**: `new` with active Spec Kit feature pinned to `001-local-tree-sitter-sqlite-cli-tui` | **Date**: 2026-05-12 | **Spec**: [spec.md](spec.md)
**Input**: Feature specification from `specs/001-local-tree-sitter-sqlite-cli-tui/spec.md`

**Note**: This plan is designed for VS Code + GitHub Copilot development. Copilot assists implementation and review only; Symdex has no runtime AI, LLM, embedding, vector database, telemetry, or network dependency.

## Summary

Build Symdex as a local-first Rust CLI/TUI that indexes repository structure with Tree-sitter and stores rebuildable evidence in SQLite. The MVP centers on `symdex init`, `symdex index`, status and structural query commands, SQLite migrations, incremental hashing, Rust/TypeScript/JavaScript/Python extraction, artificial fixtures, and a basic ratatui dashboard. The design keeps CLI rendering, TUI rendering, query services, database access, parser/indexer code, and evidence models separated so future MCP tools can call the same core services without changing runtime privacy guarantees.

## Technical Context

**Language/Version**: Rust 1.95.0, edition 2024  
**Primary Dependencies**: `clap`, `ratatui`, `crossterm`, `rusqlite` with bundled SQLite, `tree-sitter`, `tree-sitter-rust`, `tree-sitter-typescript`, `tree-sitter-javascript`, `tree-sitter-python`, `ignore`, `globset`, `serde`, `toml`, `sha2`, `anyhow`, `thiserror`, `tracing`, `tracing-subscriber`  
**Storage**: Local SQLite database at `.symdex/index.db` with WAL/SHM sidecars ignored by Git; FTS5 virtual table for symbol text search  
**Testing**: `cargo test`, focused unit tests, integration tests with artificial fixtures, CLI smoke tests with `assert_cmd`, formatting via `cargo fmt`, linting via `cargo clippy --all-targets -- -D warnings`  
**Target Platform**: Local developer machines running VS Code and terminal shells; macOS primary for current workspace, Rust crate remains portable where dependencies support it  
**Project Type**: Single Rust CLI/TUI crate with library modules for reusable query/index services  
**Performance Goals**: Index 1,000 small/medium files in under 30 seconds on a modern laptop; skip unchanged files on repeat indexing; batch SQLite writes in transactions; avoid storing full file contents by default  
**Constraints**: Local-only; no network calls; no telemetry; no LLMs; no embeddings; no vector stores; generated `.symdex/` and SQLite files must stay out of Git; individual parse failures must not stop an indexing run  
**Scale/Scope**: MVP supports Rust, TypeScript, JavaScript, and Python; artificial fixtures only for tests; advanced graph visualization, MCP server, production watch mode, CI guardrail mode, and safe JSON export are later-version work

## Constitution Check

*GATE: Must pass before Phase 0 research. Re-check after Phase 1 design.*

1. **Spec-First User Value**: PASS. The spec defines three prioritized journeys: initialize/index, CLI structural queries, and TUI inspection, with measurable success criteria and explicit non-goals.
2. **Simple, Local Design**: PASS. Single Rust crate, direct SQLite via `rusqlite`, Tree-sitter extraction, and simple module boundaries. Deferred advanced graph, MCP, and watch-daemon features avoid premature architecture.
3. **Testable Quality Gates**: PASS. Each journey has independent automated tests or smoke checks: init/index, CLI query output, parse-error recording, incremental skip, and TUI launch/manual terminal checks.
4. **Observable, Operable Behavior**: PASS. Index summaries, parse-run rows, parse-error records, query output line/column evidence, tracing hooks, and TUI status messages provide operational visibility.
5. **Secure, Reproducible Changes**: PASS. `.symdex/`, SQLite files, and `symdex.local.toml` are ignored; no runtime network/AI dependencies are used; build/test/run commands are documented in quickstart.

## Project Structure

### Documentation (this feature)

```text
specs/001-local-tree-sitter-sqlite-cli-tui/
├── spec.md
├── plan.md
├── research.md
├── data-model.md
├── quickstart.md
└── contracts/
    └── cli.md
```

### Source Code (repository root)

```text
Cargo.toml
Cargo.lock
README.md
AGENTS.md
symdex.toml
migrations/
├── 001_initial.sql
├── 002_symbols.sql
├── 003_relationships.sql
└── 004_fts.sql
src/
├── main.rs
├── lib.rs
├── cli/
│   ├── mod.rs
│   └── commands.rs
├── config/
│   └── mod.rs
├── db/
│   ├── mod.rs
│   ├── migrations.rs
│   └── schema.rs
├── evidence/
│   └── mod.rs
├── indexer/
│   ├── mod.rs
│   ├── discovery.rs
│   ├── hashing.rs
│   └── pipeline.rs
├── parser/
│   ├── mod.rs
│   ├── languages.rs
│   └── extract.rs
├── search/
│   ├── mod.rs
│   └── fts.rs
├── symbols/
│   ├── mod.rs
│   ├── model.rs
│   └── relationships.rs
└── tui/
    ├── mod.rs
    ├── app.rs
    └── views.rs
tests/
├── integration.rs
└── fixtures/
    ├── rust-basic/
    ├── typescript-basic/
    └── python-basic/
```

**Structure Decision**: Use the single-crate layout from the spec, with public library modules that can be reused by the CLI, TUI, tests, and future MCP tools. Keep generated runtime data under `.symdex/` and outside source control.

## Verification Plan

**Automated Tests**: Unit tests for parser extraction and integration tests for config init/load, migration application, fixture indexing, symbol search, reference discovery, parse-error recording, incremental skip behavior, and CLI basics. Use artificial fixtures under `tests/fixtures/` only.

**Manual Verification**: Run `cargo run -- init --force`, `cargo run -- index . --full`, `cargo run -- status`, `cargo run -- symbols find index_repository`, `cargo run -- errors`, and `cargo run -- tui`. Confirm `.symdex/` remains ignored in `git status --short`.

**Operational Checks**: Verify index summary counts, parse-run rows, parse-error rows, source file/line evidence in command output, and TUI dashboard status. Confirm no code path introduces runtime network, AI, embedding, vector DB, or telemetry dependencies.

## Complexity Tracking

No constitution violations require complexity exceptions.

## Phase 0 Research Summary

See [research.md](research.md). Decisions are resolved: Rust single crate, SQLite/FTS5 storage, Tree-sitter extraction, conservative relationship confidence, ratatui dashboard MVP, and VS Code/Copilot as development-only context.

## Phase 1 Design Summary

See [data-model.md](data-model.md), [contracts/cli.md](contracts/cli.md), and [quickstart.md](quickstart.md). The primary interface contract is the CLI command surface; the database schema follows the migration files and internal Rust models mirror source-backed evidence.

## Constitution Check Post-Design

1. **Spec-First User Value**: PASS. Design artifacts preserve the P1/P2/P3 journey order and map contracts to acceptance scenarios.
2. **Simple, Local Design**: PASS. Data model uses normalized SQLite tables and local modules without extra services or runtime AI.
3. **Testable Quality Gates**: PASS. Quickstart and data model identify automated tests and manual TUI checks.
4. **Observable, Operable Behavior**: PASS. Evidence fields, parse runs, parse errors, summaries, and TUI status are explicit.
5. **Secure, Reproducible Changes**: PASS. Git safety and local-only constraints are represented in spec, plan, quickstart, and contracts.
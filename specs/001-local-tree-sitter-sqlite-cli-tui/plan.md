# Implementation Plan: Local Tree-sitter SQLite CLI/TUI Indexer

**Feature**: `001-local-tree-sitter-sqlite-cli-tui` pinned by `.specify/feature.json` for this workspace | **Date**: 2026-05-12 | **Spec**: [spec.md](spec.md)
**Input**: Feature specification from `specs/001-local-tree-sitter-sqlite-cli-tui/spec.md`

**Note**: This plan is designed for VS Code + GitHub Copilot development. Copilot assists implementation and review only; Symdex has no runtime AI, LLM, embedding, vector database, telemetry, source upload, automatic code modification, or network dependency.

## Summary

Build Symdex as a local-first Rust CLI/TUI that indexes repository structure with Tree-sitter and stores rebuildable structural evidence in SQLite. The MVP centers on `symdex init`, `symdex index`, status and structural query commands, SQLite migrations, incremental hashing, Rust/TypeScript/JavaScript/Python extraction, artificial fixtures, and a basic ratatui dashboard/navigation shell. The design keeps CLI rendering, TUI rendering, query services, database access, parser/indexer code, and evidence models separated so future tools can call the same core services without weakening local-only guarantees.

## Technical Context

**Language/Version**: Rust 1.95.0, edition 2024  
**Primary Dependencies**: `clap`, `ratatui`, `crossterm`, `rusqlite` with bundled SQLite, `tree-sitter`, `tree-sitter-rust`, `tree-sitter-typescript`, `tree-sitter-javascript`, `tree-sitter-python`, `ignore`, `globset`, `serde`, `toml`, `sha2`, `anyhow`, `thiserror`, `tracing`, `tracing-subscriber`  
**Storage**: Local SQLite database at `.symdex/index.db` with WAL/SHM sidecars ignored by Git; FTS5 virtual table for symbol text search; no full source file contents stored by default
**Testing**: `cargo test`, focused unit tests, integration tests with artificial fixtures, CLI smoke tests with `assert_cmd`, formatting via `cargo fmt`, linting via `cargo clippy --all-targets -- -D warnings`  
**Target Platform**: Local developer machines running VS Code and terminal shells; macOS primary for current workspace, Rust crate remains portable where dependencies support it  
**Project Type**: Single Rust CLI/TUI crate with library modules for reusable query/index services  
**Performance Goals**: Index 1,000 small/medium files in under 30 seconds on a modern laptop; skip unchanged files on repeat indexing; batch SQLite writes in transactions; avoid loading entire large repositories into memory
**Constraints**: Local-only deterministic runtime; no network calls; no telemetry; no LLMs; no embeddings; no vector stores; no source upload; no automatic code modification; generated `.symdex/` and SQLite files must stay out of Git; individual parse failures must not stop an indexing run
**Scale/Scope**: MVP supports Rust, TypeScript, JavaScript, and Python; artificial fixtures only for tests; advanced graph visualization, MCP server, production watch mode, CI guardrail mode, and safe JSON export are later-version work

## Constitution Check

*GATE: Must pass before Phase 0 research. Re-check after Phase 1 design.*

1. **Spec-First User Value**: PASS. The spec defines three prioritized journeys: initialize/index, CLI structural queries, and TUI inspection, with measurable success criteria, assumptions, non-goals, and evidence needs.
2. **Simple, Local, Deterministic Design**: PASS. Single Rust crate, direct SQLite via `rusqlite`, Tree-sitter extraction, and simple module boundaries preserve local-only deterministic behavior. Deferred advanced graph, MCP, watch-daemon, semantic search, and runtime AI features avoid premature architecture.
3. **Testable Quality Gates**: PASS. Each journey has independent automated tests or smoke checks: init/index, CLI query output, parse-error recording, incremental skip, parser/language fixture coverage, and TUI launch/manual terminal checks.
4. **Observable, Operable Behavior**: PASS. Index summaries, parse-run rows, parse-error records, query output line/column evidence, database paths, recovery guidance, tracing hooks, and TUI status messages provide operational visibility.
5. **Secure, Reproducible Local Artifacts**: PASS. `.symdex/`, SQLite files, and `symdex.local.toml` are ignored; no runtime network/AI dependencies are used; full source contents are not stored by default; build/test/run commands are documented in quickstart.

## Project Structure

### Documentation (this feature)

```text
specs/001-local-tree-sitter-sqlite-cli-tui/
├── spec.md
├── plan.md
├── research.md
├── data-model.md
├── quickstart.md
├── contracts/
│   └── cli.md
└── tasks.md
```

### Source Code (repository root)

```text
Cargo.toml
Cargo.lock
README.md
AGENTS.md
symdex.toml
docs/
└── specs/
    ├── project-discovery.md
    ├── parser-pipeline.md
    ├── symbol-model.md
    ├── sqlite-schema.md
    ├── incremental-indexing.md
    ├── cli-commands.md
    ├── tui-navigation.md
    ├── evidence-model.md
    └── git-safety.md
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
    ├── javascript-basic/
    └── python-basic/
```

**Structure Decision**: Use the single-crate layout from the spec, with public library modules that can be reused by the CLI, TUI, tests, and future MCP tools. Keep generated runtime data under `.symdex/` and outside source control.

## Verification Plan

**Automated Tests**: Unit tests for parser extraction and TUI app state, plus integration tests for config init/load, missing-config guidance, migration application, Rust/TypeScript/JavaScript/Python fixture indexing, symbol search, reference discovery, parse-error recording, incremental skip behavior, import/relationship queries, CLI basics, and generated-fixture performance smoke coverage. Use artificial fixtures under `tests/fixtures/` only.

**Manual Verification**: Run `cargo run -- init --force`, `cargo run -- index . --full`, `cargo run -- status`, `cargo run -- symbols find index_repository`, `cargo run -- errors`, and `cargo run -- tui`. In the TUI, verify dashboard, files, symbols, symbol detail, references, callers/callees, imports, parse errors, search navigation, `?` help, and `q` clean shutdown. Confirm `.symdex/` remains ignored in `git status --short`.

**Manual TUI Rationale And Risk**: Full terminal alternate-screen rendering and restoration are manually verified because headless terminal integration tests are brittle for the MVP and would add more harness complexity than product behavior. Residual risk is a regression in real terminal layout or restoration; this is mitigated by automated app-state tests plus the quickstart TUI checklist before release.

**Operational Checks**: Verify index summary counts, parse-run rows, parse-error rows, missing-config and unindexed-repository recovery guidance, source file/line evidence in command output, generated-fixture indexing throughput, and TUI dashboard status. Confirm no code path introduces runtime network, AI, embedding, vector DB, source upload, automatic code modification, or telemetry dependencies.

## Complexity Tracking

No constitution violations require complexity exceptions.

## Phase 0 Research Summary

See [research.md](research.md). Decisions are resolved: Rust single crate, SQLite/FTS5 storage, Tree-sitter extraction, conservative relationship confidence, ratatui dashboard MVP, and VS Code/Copilot as development-only context.

## Phase 1 Design Summary

See [data-model.md](data-model.md), [contracts/cli.md](contracts/cli.md), and [quickstart.md](quickstart.md). The primary interface contract is the CLI command surface; the database schema follows the migration files and internal Rust models mirror source-backed evidence.

## Constitution Check Post-Design

1. **Spec-First User Value**: PASS. Design artifacts preserve the P1/P2/P3 journey order and map contracts to acceptance scenarios and evidence expectations.
2. **Simple, Local, Deterministic Design**: PASS. Data model uses normalized SQLite tables and local modules without extra services, runtime AI, source upload, telemetry, or network behavior.
3. **Testable Quality Gates**: PASS. Quickstart, data model, and contracts identify automated tests, artificial fixture coverage for each claimed language, and manual TUI checks with rationale.
4. **Observable, Operable Behavior**: PASS. Evidence fields, parse runs, parse errors, summaries, recovery guidance, and TUI status are explicit.
5. **Secure, Reproducible Local Artifacts**: PASS. Git safety and local-only constraints are represented in spec, plan, quickstart, contracts, and repository guidance.

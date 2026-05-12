# Research: Local Tree-sitter SQLite CLI/TUI Indexer

## Decision: Rust Single-Crate CLI/TUI With Reusable Library Modules

**Rationale**: Rust matches the project spec, provides strong local binary distribution ergonomics, and works well with Tree-sitter, SQLite, and terminal UI libraries. A single crate keeps the MVP simple while `src/lib.rs` exposes internal modules for CLI, TUI, tests, and future MCP reuse.

**Alternatives considered**: Multi-crate workspace was rejected for MVP because there is no current need for independent package publishing. Python was rejected because the required stack and performance goals point to Rust.

## Decision: SQLite Via `rusqlite` Migrations And FTS5

**Rationale**: SQLite is local, deterministic, rebuildable, easy to ignore from Git, and supports FTS5 without embeddings or vector stores. `rusqlite` with bundled SQLite gives predictable local development in VS Code.

**Alternatives considered**: PostgreSQL and client/server stores were rejected because the tool must be local-only and cache-like. Vector databases were explicitly rejected by the spec and constitution privacy constraints.

## Decision: Tree-sitter Grammars For Rust, TypeScript, JavaScript, And Python

**Rationale**: Tree-sitter provides deterministic syntax trees and syntax-error locations without runtime network calls. Initial language support matches the requested MVP and leaves a clear extension point in `parser/languages.rs`.

**Alternatives considered**: Regex-only extraction was rejected as too brittle for structural evidence. Compiler frontends were rejected for MVP because they add complexity and often require project-specific build configuration.

## Decision: Conservative Relationship Confidence

**Rationale**: Symdex must not overclaim semantics. Relationships proven structurally use `confidence = structural`; unresolved name-based links use `confidence = name_match`. This keeps CLI answers explainable and aligned with evidence records.

**Alternatives considered**: Full semantic resolution and type-aware call graphs were rejected for MVP because they require language-specific build context and could imply stronger claims than Tree-sitter evidence supports.

## Decision: `ratatui` Dashboard MVP

**Rationale**: The TUI requirement is satisfied first by a useful dashboard and navigation shell while detailed structural query output remains available through CLI commands. This keeps the first TUI small, testable, and local-only.

**Alternatives considered**: Advanced graph navigation was rejected because the spec lists it as later-version work. A web UI was rejected because the requested interface is terminal-first.

## Decision: VS Code + GitHub Copilot Development Context Only

**Rationale**: The user is building with VS Code and Copilot, so `.github/copilot-instructions.md` should point to this plan. Copilot can assist coding, review, and task execution, but it is not a runtime feature and must not introduce LLM/network behavior into Symdex.

**Alternatives considered**: Runtime Copilot/LLM integration was rejected because the spec explicitly forbids LLMs, embeddings, cloud sync, telemetry, and AI-generated summaries.
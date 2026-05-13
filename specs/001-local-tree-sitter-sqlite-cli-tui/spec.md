# Feature Specification: Local Tree-sitter SQLite CLI/TUI Indexer

**Feature Branch**: `001-local-tree-sitter-sqlite-cli-tui`  
**Created**: 2026-05-12  
**Status**: Draft  
**Input**: User description: "Build a local-first Rust CLI/TUI application that parses a source code repository with Tree-sitter and stores deterministic structural code intelligence in a local SQLite database. Build with VS Code and GitHub Copilot."

## User Scenarios & Testing *(mandatory)*

### User Story 1 - Initialize And Index A Repository (Priority: P1)

A developer initializes Symdex in a repository, runs an index, and gets a generated local SQLite database under `.symdex/` without committing private runtime data.

**Why this priority**: This is the minimum useful flow; no query command or TUI view matters until the local index exists safely.

**Independent Test**: Run `symdex init`, then `symdex index .`, and verify `.symdex/index.db` exists, is ignored by Git, and the command prints a summary with scanned, parsed, skipped, symbol, import, reference, relationship, and parse-error counts.

**Acceptance Scenarios**:

1. **Given** a repository without `symdex.toml`, **When** the user runs `symdex init`, **Then** `symdex.toml` and `.symdex/` are created without requiring network access.
2. **Given** a repository with supported language files, **When** the user runs `symdex index .`, **Then** Symdex walks non-ignored files, parses supported files, stores generated records in SQLite, and prints an index summary.
3. **Given** unchanged files after a prior index, **When** the user runs `symdex index .` again, **Then** Symdex skips unchanged files by content hash.

---

### User Story 2 - Query Structural Code Facts From CLI (Priority: P2)

A developer or coding agent uses deterministic CLI commands to find symbols, references, imports, relationships, parse errors, and index status.

**Why this priority**: The CLI is the primary interface for both humans and future agent integrations.

**Independent Test**: Run indexing against artificial fixtures, then verify `status`, `symbols find`, `symbols in`, `refs`, `callers`, `callees`, `imports`, `errors`, and `files with-errors` return source-backed results.

**Acceptance Scenarios**:

1. **Given** an indexed repository, **When** the user runs `symdex symbols find parse_config`, **Then** output includes symbol name, kind, language, file path, line range, signature, visibility, and match explanation.
2. **Given** an indexed repository, **When** the user runs `symdex refs parse_config`, **Then** output includes known reference locations and reference kind.
3. **Given** a file with syntax errors, **When** the user runs `symdex errors --file <file>`, **Then** output includes parse-error line, column, and message.

---

### User Story 3 - Inspect Index Status In A TUI (Priority: P3)

A developer launches a terminal UI to inspect a local dashboard and navigate required structural views without AI integration.

**Why this priority**: The TUI improves repeated local inspection, but it can start as a dashboard because the CLI already exposes the detailed query surface.

**Independent Test**: Run `symdex tui` after indexing and verify the dashboard opens with navigation entries, current status counts, help text, and required keyboard controls.

**Acceptance Scenarios**:

1. **Given** an indexed repository, **When** the user runs `symdex tui`, **Then** the TUI displays navigation entries for dashboard, files, symbols, symbol detail, references, callers/callees, imports, parse errors, and search with local status details.
2. **Given** the TUI is open, **When** the user presses `?`, **Then** key help is shown.
3. **Given** the TUI is open, **When** the user presses `q`, **Then** Symdex exits cleanly and restores the terminal.

### Edge Cases

- Missing `symdex.toml` should be recoverable via `symdex init`; query commands should instruct the user to index first when no repository record exists.
- Existing `symdex.toml` should not be overwritten by `symdex init` unless `--force` is provided.
- Unsupported file extensions should be skipped without failing the run.
- Oversized files should be skipped according to `max_file_size_bytes`.
- Individual file parse failures should create parse-error records without stopping the entire indexing run.
- Database-open or migration failures are fatal.
- Deleted files should be marked deleted or excluded from query results on the next index.
- `--watch` is accepted in the MVP but may perform a single pass with a message that continuous watch mode is later-version scope.

## Scope & Non-Goals *(mandatory)*

- **In Scope**: Rust CLI, config initialization, local SQLite migrations, Tree-sitter parsing for Rust/TypeScript/JavaScript/Python, incremental indexing by hash, structured CLI queries, FTS5 symbol search support, basic ratatui dashboard, artificial fixtures, and tests.
- **Out of Scope**: LLM integration, embeddings, vector search, Qdrant, semantic repo Q&A, telemetry, cloud sync, remote APIs, AI-generated summaries, automatic code modification, advanced graph visualization, production watch daemon, MCP server, and safe JSON export.
- **Assumptions**: Users run Symdex locally from VS Code or a terminal; GitHub Copilot may assist development but is not a runtime dependency; generated `.symdex/` content is disposable cache; the MVP prioritizes structural evidence over inferred semantics.

## Requirements *(mandatory)*

### Functional Requirements

- **FR-001**: System MUST provide `symdex init` to create `symdex.toml` and `.symdex/`, refusing to overwrite existing config unless `--force` is passed.
- **FR-002**: System MUST load `symdex.toml` and optional `symdex.local.toml` overrides.
- **FR-003**: System MUST ignore generated `.symdex/`, SQLite files, and `symdex.local.toml` by default.
- **FR-004**: System MUST provide `symdex index <path> [--full] [--watch]` to detect repo root, walk files, apply ignore/include/exclude rules, hash files, parse changed files, remove deleted files from active results, persist generated records, update FTS, and print a summary.
- **FR-005**: System MUST support Rust, TypeScript, JavaScript, and Python Tree-sitter parsing in the MVP.
- **FR-006**: System MUST persist repositories, files, parse runs, parse errors, symbols, symbol relationships, symbol references, imports, and FTS records using SQLite migrations.
- **FR-007**: System MUST extract symbol name, kind, language, file path, line range, byte range, parent symbol, signature when available, visibility when available, and stable hash.
- **FR-008**: System MUST support relationship kinds `contains`, `calls`, `references`, `imports`, `exports`, `implements`, `extends`, `tests`, and `depends_on` where there is structural or name-match evidence.
- **FR-009**: System MUST avoid overclaiming relationships; unresolved name-based relationships MUST use `confidence = name_match`.
- **FR-010**: System MUST provide `symdex status` with repo path, database path, last index time, files indexed, symbols indexed, relationships indexed, parse errors, and supported languages.
- **FR-011**: System MUST provide `symdex symbols find <name>` and `symdex symbols in <file>`.
- **FR-012**: System MUST provide `symdex refs <symbol-name>`, `symdex callers <symbol-name>`, `symdex callees <symbol-name>`, `symdex imports <file>`, `symdex errors [--file <file>]`, and `symdex files with-errors`.
- **FR-013**: System MUST provide `symdex tui` with navigation entries for dashboard, files, symbols, symbol detail, references, callers/callees, imports, parse errors, and search with local status details.
- **FR-014**: System MUST expose diagnosable output through command summaries, parse-error records, line/column evidence, and TUI status messages.
- **FR-015**: System MUST protect source privacy by making no network calls, storing no full file contents by default, and keeping generated database/cache files out of Git.

### Key Entities *(include if feature involves data)*

- **Repository**: Indexed root path, display name, and lifecycle timestamps.
- **File**: Repository-relative path, optional absolute path, detected language, content hash, size, indexed timestamp, and deletion marker.
- **Parse Run**: Index run metadata including timestamps, full reindex flag, scanned/parsed/skipped counts, and parse-error count.
- **Parse Error**: Syntax or parser failure evidence tied to a file and parse run.
- **Symbol**: Structural code unit with kind, language, source location, parent, signature, visibility, and stable hash.
- **Symbol Relationship**: Evidence-backed structural or name-match relation between symbols/files.
- **Symbol Reference**: Source location where a name is referenced or called.
- **Import**: File-level import/use/module dependency evidence.
- **Evidence**: Internal model that ties claims to file path, line range, symbol name, relationship kind, source table, and source record id.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: A user can initialize and index an artificial Rust fixture with one command sequence and receive a successful summary.
- **SC-002**: A repeat index over unchanged fixture files reports skipped files rather than reparsing them.
- **SC-003**: CLI symbol search returns a known fixture symbol with file path and line range.
- **SC-004**: CLI parse-error search returns a known fixture syntax error with line and column.
- **SC-005**: `cargo test` passes using artificial Rust, TypeScript, JavaScript, and Python fixtures without depending on private repositories.
- **SC-006**: Generated `.symdex/` database files are absent from `git status --short` because they are ignored.
- **SC-007**: No runtime code path calls external APIs, telemetry, LLMs, embeddings, vector databases, or cloud services.

## Constitution Alignment *(mandatory)*

- **User Value**: P1 creates the local index, P2 exposes deterministic structural answers, and P3 adds a local inspection interface.
- **Simplicity**: Use a single Rust crate with separated modules for CLI, TUI, query services, database, parser/indexer, and evidence; defer MCP, watch daemon, graph views, and advanced resolution.
- **Testing**: Automated unit/integration tests cover config loading, migrations, indexing, incremental skip, parse errors, symbol extraction, references, imports, relationships, and CLI basics against artificial fixtures.
- **Operability**: Commands print summaries and source-backed evidence; parse failures become records; TUI displays local status counts and messages.
- **Security/Reproducibility**: Generated runtime data is ignored, local override config is ignored, no network/runtime AI dependencies are used, and Cargo commands provide repeatable build/test/run validation.
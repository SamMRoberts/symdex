# Tasks: Local Tree-sitter SQLite CLI/TUI Indexer

**Input**: Design documents from `/specs/001-local-tree-sitter-sqlite-cli-tui/`
**Prerequisites**: plan.md, spec.md, research.md, data-model.md, contracts/cli.md, quickstart.md

**Tests**: Behavior changes require automated tests at the narrowest useful level. Changed boundaries, persistence, parser/language support, and user-facing flows require integration, contract, or smoke coverage unless a task explicitly names manual verification. Tests use artificial fixtures under `tests/fixtures/`, never private repositories.

**Organization**: Tasks are grouped by user story to enable independent implementation and testing of each story.

## Format: `[ID] [P?] [Story] Description`

- **[P]**: Can run in parallel (different files, no dependencies)
- **[Story]**: Which user story this task belongs to (US1, US2, US3)
- Each task includes exact file paths

## Phase 1: Setup (Shared Infrastructure)

**Purpose**: Establish the Rust project, local-only guardrails, configuration template, and documentation skeleton.

- [ ] T001 Create Rust package metadata and dependency set in Cargo.toml
- [ ] T002 Create Cargo.lock by resolving Rust dependencies with cargo build in Cargo.lock
- [ ] T003 [P] Add generated runtime data ignores for `.symdex/`, SQLite files, and symdex.local.toml in .gitignore
- [ ] T004 [P] Add local-only project guidance for agents in AGENTS.md
- [ ] T005 [P] Add user-facing overview and command list in README.md
- [ ] T006 [P] Add default project configuration template in symdex.toml
- [ ] T007 [P] Add quick reference design docs in docs/specs/project-discovery.md, docs/specs/parser-pipeline.md, docs/specs/symbol-model.md, docs/specs/sqlite-schema.md, docs/specs/incremental-indexing.md, docs/specs/cli-commands.md, docs/specs/tui-navigation.md, docs/specs/evidence-model.md, and docs/specs/git-safety.md

---

## Phase 2: Foundational (Blocking Prerequisites)

**Purpose**: Core module boundaries, data schema, models, configuration, and parser foundations that all user stories rely on.

**CRITICAL**: No user story work can begin until this phase is complete.

- [ ] T008 Create crate module entrypoints in src/lib.rs and executable bootstrap in src/main.rs
- [ ] T009 [P] Define CLI argument structures and subcommands in src/cli/mod.rs
- [ ] T010 [P] Implement configuration defaults, TOML loading, local override merging, required-config errors, config initialization, and repository root discovery in src/config/mod.rs
- [ ] T011 [P] Add SQLite migration 001 for repositories, files, parse_runs, and parse_errors in migrations/001_initial.sql
- [ ] T012 [P] Add SQLite migration 002 for symbols in migrations/002_symbols.sql
- [ ] T013 [P] Add SQLite migration 003 for relationships, references, and imports in migrations/003_relationships.sql
- [ ] T014 [P] Add SQLite migration 004 for FTS5 and indexes in migrations/004_fts.sql
- [ ] T015 Implement migration runner and database opening with foreign keys enabled in src/db/migrations.rs and src/db/mod.rs
- [ ] T016 [P] Define query/status row DTOs in src/db/schema.rs
- [ ] T017 [P] Define extracted symbol, reference, import, relationship, parse-error, and extracted-file models in src/symbols/model.rs
- [ ] T018 [P] Define relationship kind and confidence constants in src/symbols/relationships.rs
- [ ] T019 [P] Define internal evidence model in src/evidence/mod.rs
- [ ] T020 [P] Implement supported language detection and Tree-sitter grammar loading for Rust, TypeScript, JavaScript, and Python in src/parser/languages.rs
- [ ] T021 [P] Implement file hashing helper in src/indexer/hashing.rs
- [ ] T022 [P] Implement ignore-aware file discovery with include/exclude glob handling in src/indexer/discovery.rs
- [ ] T023 [P] Create artificial Rust, TypeScript, JavaScript, and Python fixture projects in tests/fixtures/rust-basic/src/lib.rs, tests/fixtures/typescript-basic/src/index.ts, tests/fixtures/javascript-basic/src/index.js, tests/fixtures/python-basic/src/main.py, and tests/fixtures/python-basic/src/bad.py

**Checkpoint**: Foundation ready; user story implementation can now begin.

---

## Phase 3: User Story 1 - Initialize And Index A Repository (Priority: P1) MVP

**Goal**: A developer can initialize Symdex, build a local SQLite index under `.symdex/`, and repeat indexing with unchanged files skipped.

**Independent Test**: Run `symdex init`, `symdex index .`, and a second `symdex index .` against an artificial fixture; verify `.symdex/index.db` is created, summary counts are printed, missing config guidance is recoverable, and unchanged files are skipped.

### Tests for User Story 1

- [ ] T024 [P] [US1] Add config init/load and existing-config refusal integration tests in tests/integration.rs
- [ ] T025 [P] [US1] Add missing symdex.toml guidance integration tests for config-dependent commands in tests/integration.rs
- [ ] T026 [P] [US1] Add migration and full indexing integration test for tests/fixtures/rust-basic in tests/integration.rs
- [ ] T027 [P] [US1] Add incremental skip integration test for unchanged tests/fixtures/rust-basic files in tests/integration.rs
- [ ] T028 [P] [US1] Add parse-error recording integration test using tests/fixtures/python-basic/src/bad.py in tests/integration.rs

### Implementation for User Story 1

- [ ] T029 [US1] Implement `symdex init` command handler in src/cli/commands.rs using src/config/mod.rs
- [ ] T030 [US1] Implement repository upsert, parse-run start/finish, file hash lookup, full-index clearing, deleted-file marking, and file index replacement in src/db/mod.rs
- [ ] T031 [US1] Implement Tree-sitter extraction for symbols, imports, call references, relationships, and parse errors in src/parser/extract.rs
- [ ] T032 [US1] Implement indexing pipeline orchestration with config loading, discovery, hashing, incremental skip, transactions, extraction, persistence, and summary counts in src/indexer/pipeline.rs
- [ ] T033 [US1] Wire `symdex index <path> [--full] [--watch]` output and MVP watch-mode message in src/cli/commands.rs
- [ ] T034 [US1] Ensure generated `.symdex/index.db`, `.symdex/index.db-wal`, `.symdex/index.db-shm`, `.symdex/cache/`, and `.symdex/logs/` remain ignored by .gitignore

**Checkpoint**: User Story 1 is independently functional and testable.

---

## Phase 4: User Story 2 - Query Structural Code Facts From CLI (Priority: P2)

**Goal**: A developer or coding agent can query status, symbols, references, callers, callees, imports, parse errors, and files with errors from the CLI.

**Independent Test**: Index artificial fixtures, then run CLI query commands and verify outputs include source-backed fields from contracts/cli.md.

### Tests for User Story 2

- [ ] T035 [P] [US2] Add CLI smoke test for `symdex init`, `symdex index .`, and `symdex symbols find parse_config` in tests/integration.rs
- [ ] T036 [P] [US2] Add database query test for symbol lookup and reference lookup in tests/integration.rs
- [ ] T037 [P] [US2] Add parse-error CLI/query test for `symdex errors --file src/bad.py` in tests/integration.rs
- [ ] T038 [P] [US2] Add import and relationship query coverage for TypeScript, JavaScript, and Rust fixtures in tests/integration.rs

### Implementation for User Story 2

- [ ] T039 [US2] Implement repository status query in src/db/mod.rs and `symdex status` rendering in src/cli/commands.rs
- [ ] T040 [US2] Implement exact/prefix symbol lookup and file symbol lookup in src/db/mod.rs and wire `symdex symbols find` and `symdex symbols in` in src/cli/commands.rs
- [ ] T041 [US2] Implement FTS-backed symbol search helper in src/search/fts.rs without embeddings or vector storage
- [ ] T042 [US2] Implement reference query in src/db/mod.rs and wire `symdex refs <symbol-name>` in src/cli/commands.rs
- [ ] T043 [US2] Implement caller and callee relationship queries in src/db/mod.rs and wire `symdex callers` and `symdex callees` in src/cli/commands.rs
- [ ] T044 [US2] Implement imports query in src/db/mod.rs and wire `symdex imports <file>` in src/cli/commands.rs
- [ ] T045 [US2] Implement parse-error and files-with-errors queries in src/db/mod.rs and wire `symdex errors [--file]` and `symdex files with-errors` in src/cli/commands.rs
- [ ] T046 [US2] Add user-facing error handling for unindexed repositories and fatal database/config failures in src/cli/commands.rs

**Checkpoint**: User Story 2 is independently functional after User Story 1 indexing exists.

---

## Phase 5: User Story 3 - Inspect Index Status In A TUI (Priority: P3)

**Goal**: A developer can launch a basic ratatui dashboard with required navigation entries, local status counts, and keyboard controls.

**Independent Test**: Run `symdex tui` after indexing and manually verify dashboard, help, navigation shortcuts, and clean terminal restoration.

### Tests for User Story 3

- [ ] T047 [P] [US3] Add TUI app state unit coverage for default dashboard selection and status message behavior in src/tui/app.rs
- [ ] T048 [P] [US3] Add manual TUI verification checklist to specs/001-local-tree-sitter-sqlite-cli-tui/quickstart.md

### Implementation for User Story 3

- [ ] T049 [US3] Implement TUI app state model with status, selected view, search text, selected symbol detail, and required view inventory in src/tui/app.rs
- [ ] T050 [US3] Implement ratatui layout with navigation pane, dashboard/status pane, details pane, and bottom status pane in src/tui/views.rs
- [ ] T051 [US3] Implement render helpers for dashboard, files, symbols, symbol detail, references, callers/callees, imports, parse errors, and search views in src/tui/views.rs
- [ ] T052 [US3] Implement terminal lifecycle, event loop, keyboard controls, help message, view navigation, and clean shutdown in src/tui/mod.rs
- [ ] T053 [US3] Wire `symdex tui [path]` command handler in src/cli/commands.rs

**Checkpoint**: User Story 3 provides the MVP TUI without requiring runtime AI integration.

---

## Phase 6: Polish & Cross-Cutting Concerns

**Purpose**: Final validation, documentation, privacy guardrails, and release readiness across all user stories.

- [ ] T054 [P] Update README.md with final quickstart, supported languages, command list, and MVP scope notes
- [ ] T055 [P] Update docs/specs/git-safety.md to state generated runtime data and private repository index exports must not be committed
- [ ] T056 [P] Audit Cargo.toml, Cargo.lock, and src/**/*.rs to confirm no runtime network, telemetry, LLM, embedding, vector DB, Qdrant, cloud sync, source-upload, or automatic code modification dependencies were introduced
- [ ] T057 [P] Add generated-fixture performance smoke coverage for the 1,000-file indexing goal in tests/integration.rs
- [ ] T058 Run `cargo fmt --check` and fix formatting in src/**/*.rs and tests/**/*.rs if needed
- [ ] T059 Run `cargo clippy --all-targets -- -D warnings` and fix lint findings in src/**/*.rs and tests/**/*.rs if needed
- [ ] T060 Run `cargo test` and fix failing tests in src/**/*.rs and tests/**/*.rs if needed
- [ ] T061 Run quickstart smoke commands from specs/001-local-tree-sitter-sqlite-cli-tui/quickstart.md and verify `.symdex/` remains absent from `git status --short`

---

## Dependencies & Execution Order

### Phase Dependencies

- **Setup (Phase 1)**: No dependencies; can start immediately.
- **Foundational (Phase 2)**: Depends on Setup completion; blocks all user stories.
- **User Story 1 (Phase 3)**: Depends on Foundational; provides the MVP index required by later query/TUI stories.
- **User Story 2 (Phase 4)**: Depends on Foundational and uses indexed data from US1; query services can be built against fixtures once persistence exists.
- **User Story 3 (Phase 5)**: Depends on Foundational and status query support; can proceed after status query from US2 is available.
- **Polish (Phase 6)**: Depends on all desired user stories being complete.

### User Story Dependencies

- **US1 Initialize And Index (P1)**: No dependency on other user stories; this is the MVP.
- **US2 CLI Structural Queries (P2)**: Requires US1 indexing/persistence data but remains independently testable after indexing a fixture.
- **US3 TUI Status Inspection (P3)**: Requires indexed status data and can be validated independently once status query works.

### Within Each User Story

- Write tests before implementation and confirm they fail for missing behavior.
- Models/schema before persistence services.
- Persistence/query services before CLI rendering.
- Core implementation before manual smoke verification.
- Complete each story checkpoint before moving to the next priority when working sequentially.

### Parallel Opportunities

- Setup docs/config tasks T003-T007 can run in parallel.
- Foundational schema/model/parser/discovery tasks T009-T014 and T016-T023 can run in parallel once T008 exists.
- US1 tests T024-T028 can run in parallel before US1 implementation.
- US2 tests T035-T038 can run in parallel after fixtures and foundational modules exist.
- US2 query implementations T040-T045 mostly touch separate query paths and can be split carefully once db query helpers are established.
- US3 tasks T047-T048 can run in parallel; T049 and T052 touch separate TUI files, while T050-T051 should be sequenced because both update src/tui/views.rs.
- Polish docs/audit/performance tasks T054-T057 can run in parallel before validation commands T058-T061.

---

## Parallel Example: User Story 1

```bash
# Launch tests for User Story 1 together:
Task: "Add config init/load and existing-config refusal integration tests in tests/integration.rs"
Task: "Add missing symdex.toml guidance integration tests in tests/integration.rs"
Task: "Add migration and full indexing integration test for rust-basic fixture in tests/integration.rs"
Task: "Add incremental skip integration test for unchanged rust-basic files in tests/integration.rs"
Task: "Add parse-error recording integration test using tests/fixtures/python-basic/src/bad.py in tests/integration.rs"
```

## Parallel Example: User Story 2

```bash
# Launch independent CLI/query coverage together:
Task: "Add CLI smoke test for init/index/symbols find in tests/integration.rs"
Task: "Add database query test for symbol lookup and reference lookup in tests/integration.rs"
Task: "Add parse-error CLI/query test in tests/integration.rs"
Task: "Add import and relationship query coverage in tests/integration.rs"
```

## Parallel Example: User Story 3

```bash
# Split TUI state, view, and manual checklist work:
Task: "Add TUI app state unit coverage in src/tui/app.rs"
Task: "Add manual TUI verification checklist to quickstart.md"
Task: "Implement ratatui layout in src/tui/views.rs"
Task: "Implement terminal lifecycle and keyboard controls in src/tui/mod.rs"
```

---

## Implementation Strategy

### MVP First (User Story 1 Only)

1. Complete Phase 1 setup.
2. Complete Phase 2 foundation.
3. Complete Phase 3 User Story 1.
4. Stop and validate `symdex init`, `symdex index .`, incremental skip behavior, parse-error recording, missing-config guidance, and Git ignore safety.

### Incremental Delivery

1. Deliver local indexing and SQLite cache safety from US1.
2. Add CLI query surface from US2 and validate against artificial fixtures.
3. Add basic TUI dashboard/navigation shell from US3 and validate terminal behavior manually.
4. Run final format, lint, test, performance smoke, quickstart smoke, and local-only dependency audit.

### VS Code + Copilot Workflow

1. Keep [plan.md](plan.md), [contracts/cli.md](contracts/cli.md), and this tasks file open while implementing.
2. Ask Copilot to complete one task or small task group at a time.
3. Run `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, and `cargo test` after each story checkpoint.
4. Reject any suggestion that adds runtime network clients, telemetry, LLMs, embeddings, vector databases, cloud sync, source upload behavior, or automatic code modification.

## Task Summary

- **Total tasks**: 61
- **Setup tasks**: 7
- **Foundational tasks**: 16
- **US1 tasks**: 11
- **US2 tasks**: 12
- **US3 tasks**: 7
- **Polish tasks**: 8
- **Suggested MVP scope**: Complete through Phase 3, User Story 1.
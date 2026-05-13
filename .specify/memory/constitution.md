<!--
Sync Impact Report
Version change: 1.0.0 -> 1.1.0
Modified principles:
- I. Spec-First User Value -> I. Spec-First User Value
- II. Simple, Local Design -> II. Simple, Local, Deterministic Design
- III. Testable Quality Gates -> III. Testable Quality Gates
- IV. Observable, Operable Behavior -> IV. Observable, Operable Behavior
- V. Secure, Reproducible Changes -> V. Secure, Reproducible Local Artifacts
Added sections:
- None
Removed sections:
- None
Templates requiring updates:
- .specify/templates/plan-template.md: updated
- .specify/templates/spec-template.md: updated
- .specify/templates/tasks-template.md: updated
- .specify/templates/commands/*.md: not present
Follow-up TODOs:
- None
-->
# Symdex Constitution

## Core Principles

### I. Spec-First User Value
Every feature MUST begin with a written specification that names the target user,
the problem being solved, prioritized user journeys, measurable success criteria,
and explicit non-goals or assumptions. Implementation details MUST remain out of
the specification until the plan phase. For Symdex, specifications MUST state the
deterministic structural question being answered and the evidence the user needs.
Rationale: local code intelligence work moves quickly only when user value and
claim boundaries are explicit before code is written.

### II. Simple, Local, Deterministic Design
Solutions MUST use the smallest design that satisfies the approved specification
and MUST follow existing project structure before adding new layers, services,
frameworks, or cross-cutting abstractions. Symdex runtime behavior MUST remain
local-only and deterministic: no network calls, telemetry, cloud sync, LLMs,
embeddings, vector databases, semantic inference services, source upload, or
automatic code modification. Any added dependency, persistent store, or
architectural boundary MUST be justified in the plan with the simpler rejected
alternative. Rationale: Symdex is a structural indexer whose answers must be
rebuildable from local Tree-sitter and SQLite evidence.

### III. Testable Quality Gates
Each user journey MUST define an independent test before implementation begins.
Behavior changes MUST include automated tests at the narrowest useful level and
integration or contract tests for changed boundaries, persistence, or user-facing
flows. Tests MUST use artificial fixtures rather than private user repositories,
and language-support claims MUST include fixture coverage for every claimed
language. If automated testing is not practical for a change, the plan MUST
document the reason, manual verification steps, and residual risk. Rationale:
every slice must be demonstrably correct without depending on private code or
unrelated future work.

### IV. Observable, Operable Behavior
Features that introduce runtime behavior MUST define the errors, logs, metrics,
or user-visible states needed to diagnose success and failure. Structural answers
MUST expose source-backed evidence such as file paths, line ranges, symbol names,
relationship kinds, parse-error records, database paths, or recovery guidance.
Plans MUST include performance and operational expectations when latency,
throughput, reliability, or recoverability matters to the user journey. Rationale:
a local index is trustworthy only when users can inspect what was indexed, what
failed, and why a result matched.

### V. Secure, Reproducible Local Artifacts
Changes MUST keep secrets, generated runtime data, SQLite files, parse logs,
absolute local paths, private repository index exports, and machine-specific
metadata out of source control. Symdex MUST treat `.symdex/` and SQLite files as
rebuildable local cache, and MUST avoid storing full source file contents by
default. Builds, tests, and local verification commands MUST be recorded in the
relevant plan or quickstart when they are introduced or changed. Rationale:
contributors need repeatable commands and safe local artifacts before indexed
metadata can be trusted or handed off.

## Technical Constraints

Symdex is a Rust CLI/TUI application that uses Tree-sitter for parsing,
SQLite/FTS5 for local storage and search, and artificial fixture repositories for
tests. Runtime architecture MUST keep CLI rendering, TUI rendering, query
services, database access, parser/indexer code, and evidence models separated so
future tools can reuse core services without weakening local-only guarantees.
Implementation plans MUST identify supported languages, migrations, generated
cache locations, testing tools, performance goals, and operational constraints
before design begins. Technology choices MUST prefer standard tooling,
checked-in configuration, deterministic commands, and minimal new dependencies.

## Development Workflow

Work MUST proceed through specification, clarification when needed, planning,
task generation, implementation, and validation. Plans MUST pass the Constitution
Check before Phase 0 research and again after Phase 1 design. Tasks MUST be
grouped by independently testable user story, with foundational work separated
from story work and cross-cutting polish. Reviews MUST verify local-only runtime
constraints, generated-cache Git safety, artificial-fixture coverage, tests,
documentation, and operational notes before a change is accepted. Release checks
MUST include `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`,
`cargo test`, and documented smoke commands when the Rust CLI/TUI is changed.

## Governance

This constitution supersedes conflicting workflow guidance in generated specs,
plans, tasks, and runtime documentation. Amendments MUST update this file, include
a Sync Impact Report, propagate required changes to dependent templates, and
record a semantic version change. Compliance is reviewed during planning, task
generation, implementation review, and release readiness.

Versioning policy: MAJOR versions remove or redefine governance obligations in a
backward-incompatible way, MINOR versions add principles or materially expand
required guidance, and PATCH versions clarify wording without changing required
behavior. The ratification date remains the original adoption date; the last
amended date changes whenever this constitution changes.

**Version**: 1.1.0 | **Ratified**: 2026-05-12 | **Last Amended**: 2026-05-12

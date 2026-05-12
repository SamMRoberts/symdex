<!--
Sync Impact Report
Version change: template -> 1.0.0
Modified principles:
- PRINCIPLE_1_NAME placeholder -> I. Spec-First User Value
- PRINCIPLE_2_NAME placeholder -> II. Simple, Local Design
- PRINCIPLE_3_NAME placeholder -> III. Testable Quality Gates
- PRINCIPLE_4_NAME placeholder -> IV. Observable, Operable Behavior
- PRINCIPLE_5_NAME placeholder -> V. Secure, Reproducible Changes
Added sections:
- Technical Constraints
- Development Workflow
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
the specification until the plan phase. Rationale: Symdex work is small enough to
move quickly, but it still needs a clear user outcome before code is written.

### II. Simple, Local Design
Solutions MUST use the smallest design that satisfies the approved specification
and MUST follow existing project structure before adding new layers, services,
frameworks, or cross-cutting abstractions. Any added dependency, persistent store,
or architectural boundary MUST be justified in the plan with the simpler rejected
alternative. Rationale: simplicity keeps future feature slices easy to understand,
test, and change.

### III. Testable Quality Gates
Each user journey MUST define an independent test before implementation begins.
Behavior changes MUST include automated tests at the narrowest useful level and
integration or contract tests for changed boundaries, persistence, or user-facing
flows. If automated testing is not practical for a change, the plan MUST document
the reason, manual verification steps, and residual risk. Rationale: every slice
must be demonstrably correct without depending on unrelated future work.

### IV. Observable, Operable Behavior
Features that introduce runtime behavior MUST define the errors, logs, metrics,
or user-visible states needed to diagnose success and failure. Plans MUST include
performance and operational expectations when latency, throughput, reliability,
or recoverability matters to the user journey. Rationale: a feature is incomplete
if maintainers cannot tell whether it is working or why it failed.

### V. Secure, Reproducible Changes
Changes MUST keep secrets out of source control, preserve least-privilege access,
and document any new configuration, environment variable, permission, or data
retention behavior. Builds, tests, and local verification commands MUST be
recorded in the relevant plan or quickstart when they are introduced or changed.
Rationale: contributors need repeatable steps and safe defaults before work can
be trusted or handed off.

## Technical Constraints

The repository currently defines Spec Kit workflow assets and no application
runtime stack. New implementation plans MUST identify the selected language,
frameworks, storage, testing tools, supported platforms, performance goals, and
operational constraints before design begins. Technology choices MUST prefer
standard tooling, checked-in configuration, deterministic commands, and minimal
new dependencies.

## Development Workflow

Work MUST proceed through specification, clarification when needed, planning,
task generation, implementation, and validation. Plans MUST pass the Constitution
Check before Phase 0 research and again after Phase 1 design. Tasks MUST be
grouped by independently testable user story, with foundational work separated
from story work and cross-cutting polish. Reviews MUST verify the applicable
constitution gates, tests, documentation, and operational notes before a change is
accepted.

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

**Version**: 1.0.0 | **Ratified**: 2026-05-12 | **Last Amended**: 2026-05-12

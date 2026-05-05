# Future Features

This document defines planned product directions that are not yet implemented.
They should guide future design and backlog work without changing the current
MVP contracts.

## Goals

- Improve precision for active non-Rust targets without weakening conservative
  evidence contracts.
- Add new agent workflows only after their local-only, metadata-first contracts
  are explicit.
- Preserve local-first privacy guarantees while adding richer debugging and
  provenance workflows.

## Feature Set

### Impact Analysis Follow-Ups

Future impact work should deepen precision without replacing the current
metadata-only report.

Requirements:

- Add richer same-file symbol grouping where indexed relationships support it.
- Improve broader C#, JavaScript, and TypeScript call resolution without turning
  weak hints into certain edges.
- Add deeper explanation metadata and ranking for why impact rows are returned.
- Extend likely-test evidence for C#, JavaScript, and TypeScript as non-Rust
  call resolution and runtime parsing become more precise.
- Keep the output deterministic for the same index version and query.

### Runtime-To-Source Mapping Follow-Ups

Runtime parsing now covers C# and Node/V8 stack frames. Future runtime work
should deepen precision for those shapes and any later active language targets.

Requirements:

- Continue preserving unmapped runtime frames with explicit status instead of
  dropping them.
- Continue joining runtime frames through the existing file, symbol, call,
  likely-test, freshness, trust, and provenance pipeline.
- Add fixture-backed parser coverage before expanding to any additional runtime
  stack shapes.

### Scoped Reindex Requests

Scoped reindex requests are a potential future write-capable MCP workflow. They
must have a design doc before implementation.

Requirements:

- Scope every request to one configured repository root.
- Align optional path-scoped reindexing with the existing explicit full and
  incremental manual index scopes.
- Default to offline structural reindexing.
- Require explicit `semantic: true` before using Ollama or sqlite-vec.
- Do not execute indexed repository code.
- Return index run IDs and status so follow-up evidence can cite the new run.
- Define caller trust, confirmation, concurrency, and failure semantics before
  code is written.

### Targeted Semantic Neighborhoods

Targeted semantic neighborhoods should add an evidence workflow for code similar
to one indexed chunk or symbol.

Requirements:

- Start from an existing indexed chunk or symbol, not from arbitrary source text.
- Query local sqlite-vec for nearest vector neighbors.
- Expose the workflow through a read-only MCP tool after the query contract is
  stable.
- Return path, line range, symbol, chunk kind, score, freshness, trust, reason
  tags, and provenance.
- Do not return vectors or source text.

### Languages After Active Targets

Languages beyond Rust, C#, JavaScript, and TypeScript should wait until the
active target set is reliable.

Requirements:

- Add each future language through the same parser/chunker/symbol/call/indexing
  contracts rather than one-off query paths.
- Preserve local-only privacy, deterministic IDs, provenance, staleness, secret
  filtering, and metadata-only evidence outputs.
- Add fixture-backed tests before marking a future language supported.

## Non-Goals

- No web UI.
- No hosted or cloud indexing.
- No source execution.
- No mutation tools over user repositories.
- No source text by default.

# Future Features

This document defines planned product directions that are not yet implemented.
They should guide future design and backlog work without changing the current
MVP contracts.

## Goals

- Make graph-derived evidence explicit and repeatable instead of hidden in chat
  retrieval.
- Reuse the same local index across CLIs, TUIs, MCP tools, and multiple coding
  agents.
- Preserve local-first privacy guarantees while adding richer debugging and
  provenance workflows.

## Feature Set

### Unified Context Packs

Unified context packs should combine structural and semantic evidence for an
editing target in one compact response.

Requirements:

- Start from the existing symbol-focused context pack.
- Add a `unified` mode that also runs semantic search for the target.
- Merge and deduplicate evidence from focus symbols, callers, callees, involved
  files, and semantic matches.
- Label each row's evidence source as structural, semantic, or both.
- Preserve source-text omission, freshness, trust, reason tags, provenance, and
  deterministic ordering.

### Call Path Tracing

Call path tracing should find explicit paths through the call graph between a
source symbol and a target symbol.

Requirements:

- Traverse persisted call edges rather than relying on ad hoc chat retrieval.
- Return bounded paths with path length, edge resolution status, confidence, and
  file/line evidence.
- Preserve unresolved or ambiguous edges as labeled graph facts.
- Support CLI, TUI, and MCP surfaces after the core traversal API is stable.

### Impact Analysis

Impact analysis has grown from direct callers/callees into a repeatable change
impact report with bounded transitive paths, related files, and direct indexed
Rust test evidence. Future work is focused on broader test discovery, richer
same-file symbol grouping, and deeper explanation metadata.

Requirements:

- Keep direct callers, direct callees, bounded transitive call paths, related
  files, and likely tests when test mapping exists.
- Keep the output deterministic for the same index version and query.
- Explain evidence with paths, line ranges, relationship type, confidence, and
  staleness status.
- Avoid claiming affected tests beyond indexed test evidence and documented
  mapping limits.
- Extend likely-test evidence to C#, JavaScript, and TypeScript only after
  parser-backed test discovery is implemented for those languages.

### Pre-Edit Change Explanation

Pre-edit change explanation should give agents a deterministic safety briefing
before they modify files.

Requirements:

- Accept proposed change targets as path plus line range plus short
  description.
- Enforce repository root boundaries for every path.
- Map line ranges to indexed symbols and related files.
- Reuse impact analysis, call path traversal, likely-test mapping, freshness,
  trust, and provenance.
- Return compact evidence without source text.
- Remain read-only unless a future design explicitly adds write behavior.

### Debug Context Packs

Debug context packs should package reusable evidence for debugging tasks.

Requirements:

- Accept a failure signal such as a stack trace, failing test name, panic
  location, symbol, or file path.
- Include matched frames, related symbols, call paths, likely tests, relevant
  context-pack sections, and index provenance.
- Be reusable across agents and sessions as a structured artifact rather than a
  one-off chat summary.
- Exclude source text by default unless a future source-preview design explicitly
  allows it.

### Index Provenance

Index provenance should make it clear exactly what evidence was indexed and
when.

Requirements:

- Track index run ID, timestamp, repository ID, normalized root, file path,
  content hash, parser version, embedding model, vector dimension, and status.
- Expose provenance for files, chunks, symbols, calls, vectors, context packs,
  and future debug packs.
- Allow TUI and MCP outputs to cite the index run or freshness state behind
  returned evidence.
- Support audits without logging or returning source text.

### Local And Private Indexing

Local/private indexing should remain a hard product boundary as richer features
are added.

Requirements:

- Keep user control over embedding model, vector database, SQLite storage, and
  repository roots.
- Do not add hosted indexing, cloud embeddings, telemetry, or remote metadata
  sync without a future explicit design doc.
- Make local service choices visible in diagnostics and provenance.
- Keep offline structural workflows usable without Qdrant or Ollama.

### Cross-Agent Reuse

Cross-agent reuse should let many local agents consume the same index safely.

Requirements:

- Keep evidence contracts stable and versioned across CLI, TUI, and MCP.
- Prefer read-only agent access until write-capable tools have a design doc.
- Include compact response shapes suitable for agent context windows.
- Guard repository boundaries and fail closed for ambiguous roots or stale
  indexes.

### Runtime-To-Source Mapping

Runtime-to-source mapping should connect runtime failures to indexed source
evidence.

Requirements:

- Parse stack traces, panic locations, failing test names, and runtime frame
  symbols into normalized source references.
- Join those references to indexed files, symbols, call graph edges, and likely
  tests.
- Produce focused debugging evidence through CLI, TUI, MCP, and debug context
  packs.
- Preserve unmapped frames with explicit status instead of dropping them.
- Expand beyond Rust with conservative C# and Node/V8 stack frame parsing before
  adding lower-priority runtimes.

### Staleness Detection

Staleness detection should warn when evidence may no longer match the working
tree.

Requirements:

- Compare indexed content hashes and timestamps against current eligible files.
- Label evidence as fresh, stale, missing, deleted, or unknown where relevant.
- Surface staleness in CLI outputs, TUI status panels, MCP responses, context
  packs, debug packs, and the direct read-only MCP staleness check.
- Avoid automatic destructive cleanup; stale evidence warnings should guide
  reindexing or continuous indexing.

### Scoped Reindex Requests

Scoped reindex requests are a potential future write-capable MCP workflow. They
must have a design doc before implementation.

Requirements:

- Scope every request to one configured repository root.
- Allow optional path-scoped reindexing and explicit full reindex requests.
- Default to offline structural reindexing.
- Require explicit `semantic: true` before using Ollama or Qdrant.
- Do not execute indexed repository code.
- Return index run IDs and status so follow-up evidence can cite the new run.
- Define caller trust, confirmation, concurrency, and failure semantics before
  code is written.

### Semantic Neighborhoods

Semantic neighborhoods should expose "code similar to this indexed chunk or
symbol" as reusable metadata-only evidence.

Requirements:

- Start from an existing indexed chunk or symbol, not from arbitrary source text.
- Query local Qdrant for nearest vector neighbors.
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

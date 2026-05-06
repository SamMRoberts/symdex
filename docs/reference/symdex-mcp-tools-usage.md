---
description: "Use when: working in any repository and using Symdex MCP tools to gather metadata-first evidence (search, freshness, call graph, impact, watcher state, and runtime debug context) before broad file reads."
---

# Symdex MCP Tool Usage

Use symdex MCP tools as the first evidence pass when they can answer a question
from the local index. They are designed to provide compact, metadata-first
context for agents without reading whole files or exposing source text.

This guide is not limited to the Symdex codebase. Use it when Symdex MCP tools
are available and indexing the repository you are currently editing.

## Default Workflow

- Start with `symdex_index_status` or `symdex_staleness_check` when evidence may
  be stale, missing, branch-sensitive, or affected by recent edits.
- Use `symdex_search` for semantic discovery when you know the concept but not
  the symbol or file.
- Use `symdex_find_symbol`, `symdex_callers`, `symdex_callees`, and
  `symdex_call_path` for symbol-level navigation, impact analysis, and call-flow
  questions.
- Use `symdex_context_pack` when you need a compact bundle of ranked evidence
  before editing or explaining a subsystem.
- Use `symdex_explain_change` before risky edits when you can describe proposed
  file/range changes and want likely impacted symbols or tests.
- Use `symdex_debug_context` for stack traces, panic locations, failing test
  names, Rust backtraces, C# stack frames, and Node/V8 JS/TS frames. Treat this
  as a local metadata-write exception: it stores only normalized runtime
  observation metadata and never pasted logs or source text.
- Use `symdex_watch_status` to inspect continuous indexing state. Use
  `symdex_watch_start` only when you intentionally need to start or attach the
  local repository watcher; this is the explicit local-only write-capable MCP
  exception.

## Evidence Rules

- Prefer MCP evidence over broad file reads when the question is about symbols,
  call relationships, impact, freshness, semantic routing, or runtime failures.
- Treat indexed evidence as potentially stale until freshness says otherwise,
  especially after editing files or switching local branches.
- Keep tool inputs rooted in the current repository. Use repository-relative
  paths where supported, and never pass paths outside the repo.
- Do not ask MCP tools for source text. The symdex contracts return paths, line
  ranges, scores, confidence, freshness, provenance, trust, and reason tags.
- Do not add or use write-capable MCP tools unless a dedicated design doc exists.
  Current allowed exceptions are `symdex_watch_start` and the metadata cache in
  `symdex_debug_context`.
- When MCP evidence conflicts with current file contents, trust the current file
  for edits and use `symdex_staleness_check` or indexing commands to explain the
  mismatch.

## When To Fall Back To Files

- Read files directly when you need exact implementation details, public API
  signatures, tests, or docs beyond the metadata returned by MCP.
- After MCP identifies a small set of relevant files or line ranges, read only
  those focused regions before editing.
- For behavior changes, still update the durable docs listed in `docs/README.md`
  when those docs describe the changed behavior.
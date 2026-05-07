---
description: "Use when: working in any repository and using Symdex MCP tools to gather metadata-first evidence (search, freshness, call graph, impact, watcher state, and runtime debug context) before broad file reads."
---

# Symdex MCP Tool Usage

Use symdex MCP tools as the first evidence pass when they can answer a question
from the local index. They are designed to provide compact, metadata-first
context for agents without reading whole files or exposing source text.

This guide is not limited to the Symdex codebase. Use it when Symdex MCP tools
are available and indexing the repository you are currently editing.

## Mental Model

Treat symdex as an evidence oracle, not as a replacement for your coding
agent's normal source reading. Symdex intentionally returns paths, line ranges,
symbols, call relationships, freshness, provenance, trust, and reason tags
instead of full source text. The agent should use that compact evidence to
narrow the search space, then read the highlighted source ranges directly before
explaining or changing code.

This matters most when the question is about:

- where behavior is implemented;
- what depends on a symbol or file;
- which tests are likely relevant;
- whether evidence is fresh for the current worktree;
- how a runtime failure maps back to indexed files, symbols, calls, and tests.

For obvious single-file edits or source-level reasoning after the relevant file
is already known, direct repository inspection may be enough.

## Setup Loop

1. Run `symdex doctor <repo>` to confirm SQLite/sqlite-vec storage, local Ollama,
   model readiness, freshness, provenance, semantic quality fallback, and the
   active MCP evidence contract.
2. Run `symdex index <repo>` for an initial semantic index, or
   `symdex index --offline <repo>` when local embeddings are unavailable and
   structural evidence is sufficient.
3. Keep evidence current with `symdex serve-mcp --watch <repo>`, the TUI, or an
   explicitly attached watcher through `symdex_watch_start`.
4. If symdex results feel worse than direct Copilot/codebase answers, check
   `symdex_staleness_check`, `symdex_index_status`, `symdex_watch_status`, and
   `semantic-status <repo>` before trusting the evidence.

## Default Workflow

1. Check freshness and status with `symdex_index_status`,
   `symdex_staleness_check`, `symdex_watch_status`, or the CLI
   `semantic-status <repo>` command.
2. Use `symdex_search` for semantic discovery when you know the concept but not
   the symbol or file.
3. Use `symdex_find_symbol` when you know the symbol or a likely symbol name.
4. Use `symdex_context_pack` with `mode: "unified"` before nontrivial edits or
   subsystem explanations. Unified mode combines structural and semantic
   evidence in one compact pack.
5. Read the actual source files and line ranges identified by symdex.
6. Use `symdex_impact` or `symdex_explain_change` before editing symbols,
   parsers, indexers, APIs, database paths, or other high-fanout code.
7. Make the change through normal source editing.
8. Recheck freshness or rerun indexing when the index may no longer describe the
   current worktree.
9. Run focused tests suggested by impact or debug evidence, plus any repository
   checks required by the task.

## Tool Selection

| Situation | Start with |
|---|---|
| "Where is this behavior handled?" | `symdex_search` |
| "What owns this symbol/name?" | `symdex_find_symbol` |
| "What should I read before editing this area?" | `symdex_context_pack` with `mode: "unified"` |
| "Who calls this?" | `symdex_callers` |
| "What does this call?" | `symdex_callees` |
| "How does one symbol reach another?" | `symdex_call_path` |
| "What might this change affect?" | `symdex_impact` |
| "What should I check before editing this file/range?" | `symdex_explain_change` |
| "How does this panic, stack trace, or failing test map to code?" | `symdex_debug_context` |
| "Can I trust this evidence?" | `symdex_staleness_check`, `symdex_index_status`, `symdex_watch_status`, `semantic-status <repo>` |

## Relationship And Impact Questions

Use symdex for relationship questions where broad agent reasoning can
hallucinate or miss indirect evidence. `symdex_callers`, `symdex_callees`,
`symdex_call_path`, and `symdex_impact` return conservative persisted facts with
confidence, resolution status, freshness, provenance, trust, and reason tags.

Prefer rows that are fresh, high-trust, and supported by precise reason tags such
as direct callers, direct callees, bounded transitive call paths, or indexed test
targets. Treat unresolved, ambiguous, stale, or low-confidence rows as leads to
inspect, not as proof.

## Pre-Edit Safety

Before a risky edit, call `symdex_explain_change` with the file path, line range,
and short description of the proposed change. Use its affected symbols, direct
and transitive relationships, related files, likely tests, freshness, trust, and
reason tags as the checklist for what the coding agent should inspect before
editing.

This is one of symdex's strongest uses: it can surface indirect callers,
relationship evidence, and likely test targets before the agent reads or changes
source.

## Debugging

For failing tests, panics, and stack traces, pass the runtime text or file to
`symdex_debug_context`. The tool maps frames and failing test names to indexed
files, symbols, calls-at-line, likely tests, freshness, provenance, and repeated
failure metadata. It stores only short-lived normalized metadata and does not
store pasted logs or source text.

After receiving the debug context, ask the coding agent to inspect only the
highlighted files and ranges first. Fall back to broader source inspection only
when frames are unmatched, stale, or low-confidence.

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
- Skip symdex for obvious single-file edits when the relevant source is already
  known and no dependency, freshness, or likely-test question exists.
- Be cautious with dynamic JavaScript/TypeScript, reflection-heavy code, macros,
  or framework wiring where conservative call resolution may be incomplete.
- If the index is stale, missing, or blocked by unavailable semantic services,
  use direct repository inspection until indexing is repaired.
- For behavior changes, still update the durable docs listed in `docs/README.md`
  when those docs describe the changed behavior.

## Standing Instruction For Coding Agents

Use this standing instruction in an agent profile or repository instruction file:

> Use symdex as the first evidence pass for codebase discovery, impact analysis,
> call relationships, stale-index checks, and debugging context. Prefer
> `symdex_context_pack` unified mode before nontrivial edits. Always inspect
> freshness, trust, provenance, and reason tags. After symdex identifies files
> and line ranges, read the source directly before changing code. If symdex
> evidence is stale, missing, or low-confidence, say so and fall back to direct
> repository inspection.

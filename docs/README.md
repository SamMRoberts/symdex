# Docs Index

Use this folder as the agent-facing project memory.

## What to read

| Task | Read first |
|---|---|
| Product requirements | `APP_SPEC.md` |
| Crate layout or module boundaries | `ARCHITECTURE.md` |
| Parsing, chunking, embeddings, reindexing | `INDEXING_PIPELINE.md` |
| Layered fast/quality semantic indexing | `LAYERED_SEMANTIC_INDEXING.md`, `LAYERED_SEMANTIC_INDEXING_TASKS.md`, `LAYERED_SEMANTIC_INDEXING_BACKLOG.md`, `INDEXING_PIPELINE.md`, `DATA_MODEL.md`, `CONTINUOUS_INDEXING.md` |
| Language support or parser expansion | `INDEXING_PIPELINE.md`, `ARCHITECTURE.md`, `BACKLOG.md` |
| Continuous indexing/watch mode | `CONTINUOUS_INDEXING.md` |
| SQLite schema or sqlite-vec payloads | `DATA_MODEL.md` |
| MCP tool contracts | `MCP_TOOLS.md` |
| Cross-agent index reuse | `CROSS_AGENT_REUSE.md` |
| Terminal UI design | `TUI.md` |
| Local setup and commands | `LOCAL_DEV.md` |
| Test expectations | `TESTING.md` |
| Privacy, secret handling, path safety | `SECURITY.md` |
| Future feature planning | `FUTURE_FEATURES.md` |
| Post-analysis implementation priorities | `ANALYSIS_REPORT.md`, `BACKLOG.md` |
| Planning implementation work | `BACKLOG.md` |
| External references | `REFERENCES.md` |

## Agent rule

Do not treat this folder as optional. If a code change modifies behavior described here, update the relevant doc in the same change.

## Project summary

symdex is a local-only code intelligence backend for AI coding agents. It combines syntax-aware indexing, semantic search, call/callee relationships, compact context packs, a terminal UI, and MCP tools so agents can ground code edits in repository evidence instead of guessing.

## Current planning signal

`ANALYSIS_REPORT.md` identifies the next product shift as moving from strong
evidence metadata toward actionable debugging intelligence. The current
recommended implementation sequence is captured in `BACKLOG.md` under
"Analysis-driven next steps"; use that section when choosing new work.

For the `quality-index` branch, the semantic indexing direction is defined in
`LAYERED_SEMANTIC_INDEXING.md`: fast `nomic-embed-text` indexing remains the
availability path, while deferred `mxbai-embed-large` quality indexing
becomes active for default search only after it is complete and current. Use
`LAYERED_SEMANTIC_INDEXING_TASKS.md` for the implementation slice order and
`LAYERED_SEMANTIC_INDEXING_BACKLOG.md` for branch-specific progress tracking.

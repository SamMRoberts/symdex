# Docs Index

Use this folder as the agent-facing project memory.

## What to read

| Task | Read first |
|---|---|
| Product requirements | `APP_SPEC.md` |
| Crate layout or module boundaries | `ARCHITECTURE.md` |
| Parsing, chunking, embeddings, reindexing | `INDEXING_PIPELINE.md` |
| Continuous indexing/watch mode | `CONTINUOUS_INDEXING.md` |
| SQLite schema or Qdrant payloads | `DATA_MODEL.md` |
| MCP tool contracts | `MCP_TOOLS.md` |
| Terminal UI design | `TUI.md` |
| Local setup and commands | `LOCAL_DEV.md` |
| Test expectations | `TESTING.md` |
| Privacy, secret handling, path safety | `SECURITY.md` |
| Planning implementation work | `BACKLOG.md` |
| External references | `REFERENCES.md` |

## Agent rule

Do not treat this folder as optional. If a code change modifies behavior described here, update the relevant doc in the same change.

## Project summary

symdex is a local-only code intelligence backend for AI coding agents. It combines syntax-aware indexing, semantic search, call/callee relationships, compact context packs, a terminal UI, and MCP tools so agents can ground code edits in repository evidence instead of guessing.

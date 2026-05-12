# Symdex

Symdex is a local-first structural code indexer. It parses repositories with
Tree-sitter, stores deterministic code intelligence in SQLite, and answers CLI or
TUI questions about files, symbols, imports, references, parse errors, and simple
relationships.

Symdex does not use LLMs, embeddings, vector databases, remote telemetry, cloud
sync, or network APIs. The generated database lives under `.symdex/` and is
ignored by Git.

## Quick Start

```bash
cargo build
cargo run -- init
cargo run -- index .
cargo run -- status
cargo run -- symbols find main
cargo run -- errors
cargo run -- tui
```

## Supported Languages

- Rust
- TypeScript
- JavaScript
- Python

## Core Commands

```bash
symdex init [--force]
symdex index <repo> [--full] [--watch]
symdex status
symdex symbols find <name>
symdex symbols in <file>
symdex refs <symbol-name>
symdex callers <symbol-name>
symdex callees <symbol-name>
symdex imports <file>
symdex errors [--file <file>]
symdex files with-errors
symdex tui
```

`--watch` is accepted for CLI compatibility with the product direction, but the
MVP performs a single indexing pass and reports that continuous watch mode is a
later-version feature.
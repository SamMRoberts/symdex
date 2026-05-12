# Quickstart: Local Tree-sitter SQLite CLI/TUI Indexer

## Prerequisites

- VS Code
- GitHub Copilot for development assistance
- Rust toolchain with `cargo`, `rustc`, `rustfmt`, and `clippy`

## Build

```bash
cargo build
```

## Initialize Symdex In A Repository

```bash
cargo run -- init
```

Use `--force` only when intentionally replacing an existing `symdex.toml`:

```bash
cargo run -- init --force
```

## Index The Current Repository

```bash
cargo run -- index .
```

Force a full rebuild:

```bash
cargo run -- index . --full
```

## Query The Index

```bash
cargo run -- status
cargo run -- symbols find parse_config
cargo run -- symbols in src/parser/extract.rs
cargo run -- refs parse_config
cargo run -- callers parse_config
cargo run -- callees parse_config
cargo run -- imports src/main.rs
cargo run -- errors
cargo run -- files with-errors
```

## Launch The TUI

```bash
cargo run -- tui
```

Controls: `q` quit, `Tab`/down next view, `Shift+Tab`/up previous view, `/` search, `Enter` open selected item, `Esc` back/dashboard, `r` re-index message, `f` files, `s` symbols, `d` symbol detail, `v` references, `c` callers/callees, `i` imports, `e` errors, `?` help.

Manual TUI verification checklist:

- Dashboard opens after indexing and shows repository, database, file, symbol, relationship, parse-error, and language status.
- Navigation lists dashboard, files, symbols, symbol detail, references, callers/callees, imports, parse errors, and search.
- `Tab` or down arrow advances through views; `Shift+Tab` or up arrow moves backward.
- `/`, `f`, `s`, `d`, `v`, `c`, `i`, and `e` switch to search, files, symbols, symbol detail, references, callers/callees, imports, and parse-error views.
- `?` shows help, `Enter` updates the status message for the selected view, `Esc` returns to dashboard, and `q` exits cleanly with the terminal restored.

## Validate

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

## Git Safety Check

After indexing, verify generated runtime data is ignored:

```bash
git status --short
```

`.symdex/`, `*.db`, `*.db-wal`, `*.db-shm`, `*.sqlite`, `*.sqlite3`, and `symdex.local.toml` should not appear as tracked or untracked files.

## Local-Only Safety Check

Symdex runtime code must not add network clients, telemetry, LLM calls, embeddings, vector databases, cloud sync, or source upload behavior. Copilot may assist inside VS Code, but it is not a runtime dependency.
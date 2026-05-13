# CLI Contract: Symdex MVP

All commands are local-only. Commands MUST NOT call external APIs, telemetry, LLMs, embeddings, or vector databases. Text output is stable enough for smoke tests but not a formal JSON API in the MVP.

## `symdex init [--force]`

**Creates**: `symdex.toml`, `.symdex/`

**Success Output**: Config path and runtime data directory.

**Failure Cases**: Existing `symdex.toml` without `--force`; cannot create `.symdex/`; cannot write config.

## `symdex index <path> [--full] [--watch]`

**Behavior**: Detect repo root, load config, apply migrations, walk files, hash files, skip unchanged files, parse changed files, persist generated records, mark missing files deleted, update FTS, print summary.

**Success Output Fields**: Repo path, database path, files scanned, files parsed, files skipped, files deleted, symbols indexed, references indexed, imports indexed, relationships indexed, parse errors.

**MVP Note**: `--watch` is accepted but performs one indexing pass and prints that continuous watch mode is later-version scope.

## `symdex status [path]`

**Success Output Fields**: Repo path, database path, last index time, files indexed, symbols indexed, relationships indexed, parse errors, supported languages.

## `symdex symbols find <name>`

**Query**: Exact and prefix symbol search, backed by structured SQL/FTS where available.

**Success Output Fields**: Symbol name, kind, language, file path, start/end line range, signature, visibility, match explanation.

## `symdex symbols in <file>`

**Query**: Active symbols in a repository-relative file path.

**Success Output Fields**: Same as `symbols find`.

## `symdex refs <symbol-name>`

**Query**: References where `referenced_name` matches the provided symbol name.

**Success Output Fields**: File path, line, column, reference kind, referenced name.

## `symdex callers <symbol-name>`

**Query**: `calls` relationships where the target symbol or evidence references the provided name.

**Success Output Fields**: Source symbol, confidence, source file, evidence.

## `symdex callees <symbol-name>`

**Query**: `calls` relationships where the source symbol matches the provided name.

**Success Output Fields**: Source symbol, target symbol or evidence, confidence.

## `symdex imports <file>`

**Query**: Imports for a repository-relative file path.

**Success Output Fields**: File path, line, column, raw import text, optional imported path/symbol.

## `symdex errors [--file <file>]`

**Query**: Parse errors for all active files or one file.

**Success Output Fields**: File path, line, column, message.

## `symdex files with-errors`

**Query**: Distinct active files with parse errors.

**Success Output Fields**: One file path per line.

## `symdex tui [path]`

**Behavior**: Launch ratatui dashboard with required navigation entries, local status details, and view-specific detail panes for the MVP dashboard/navigation shell. Detailed source-backed lists remain available through CLI query commands.

**Controls**: `q` quit, `Tab`/down next view, `Shift+Tab`/up previous view, `/` search message, `Enter` open selected item, `Esc` dashboard/back, `r` re-index message, `f` files, `s` symbols, `d` symbol detail, `v` references, `c` callers/callees, `i` imports, `e` errors, `?` help.

## Common Error Contract

- Missing `symdex.toml`: commands that require configuration MUST fail with a recoverable message instructing the user to run `symdex init`.
- Existing `symdex.toml`: `symdex init` MUST refuse to overwrite it unless `--force` is passed.
- Unindexed repository: query commands MUST tell the user to run `symdex index .`.
- Invalid path or repository root: commands MUST fail without creating remote state or uploading source contents.
- Database open or migration failure: commands MUST fail fatally with the database path in the error context.
- File parse failures: `symdex index` MUST continue indexing other files, persist parse-error rows, and include parse-error counts in the summary.
- Unsupported or oversized files: `symdex index` MUST skip them without failing the whole run.
- TUI startup failures: `symdex tui` MUST surface configuration/database errors before entering alternate-screen mode when possible, and must restore the terminal on clean quit.
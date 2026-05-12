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

**Behavior**: Launch ratatui dashboard with required navigation entries and local status details.

**Controls**: `q` quit, `/` search message, `Enter` open selected item, `Esc` dashboard/back, `r` re-index message, `e` errors, `s` symbols, `c` callers/callees, `i` imports, `?` help.

## Common Error Contract

Query commands that cannot find an indexed repository MUST tell the user to run `symdex index .`. Fatal errors include database-open failure, migration failure, invalid repository root, and inability to create `.symdex/`.
# Data Model: Local Tree-sitter SQLite CLI/TUI Indexer

## Repository

**Fields**: `id`, `root_path`, `name`, `created_at`, `updated_at`

**Relationships**: Has many files and parse runs.

**Validation Rules**: `root_path` is unique and non-empty. Timestamps are UTC text values.

## File

**Fields**: `id`, `repository_id`, `path`, `absolute_path`, `language`, `content_hash`, `size_bytes`, `indexed_at`, `deleted_at`

**Relationships**: Belongs to repository; has many symbols, parse errors, references, imports, and file-scoped relationships.

**Validation Rules**: `(repository_id, path)` is unique. `path` is repository-relative. `content_hash` is required. `deleted_at` excludes stale records from active query results.

**State Transitions**: `discovered -> indexed -> unchanged skipped -> changed reindexed -> deleted`.

## Parse Run

**Fields**: `id`, `repository_id`, `started_at`, `finished_at`, `full_reindex`, `files_scanned`, `files_parsed`, `files_skipped`, `parse_errors`

**Relationships**: Belongs to repository; has many parse errors.

**Validation Rules**: Counters default to zero and are updated when the run finishes. Failed database-open or migration state prevents parse-run creation.

## Parse Error

**Fields**: `id`, `file_id`, `parse_run_id`, `start_line`, `start_column`, `end_line`, `end_column`, `message`

**Relationships**: Belongs to file and parse run.

**Validation Rules**: Start line and column are required. Message must describe Tree-sitter syntax error or parser/file failure.

## Symbol

**Fields**: `id`, `file_id`, `parent_symbol_id`, `name`, `kind`, `language`, `signature`, `visibility`, `start_line`, `start_column`, `end_line`, `end_column`, `start_byte`, `end_byte`, `symbol_hash`

**Relationships**: Belongs to file; optionally belongs to parent symbol; may be source or target of relationships; may be linked from references.

**Validation Rules**: `kind` is one of the supported symbol kinds where extraction can identify it; `symbol_hash` is derived from relative path, name, kind, byte range, and signature.

## Symbol Relationship

**Fields**: `id`, `source_symbol_id`, `target_symbol_id`, `source_file_id`, `target_file_id`, `relationship_kind`, `confidence`, `evidence`

**Relationships**: Connects source file/symbol to optional target file/symbol.

**Validation Rules**: `relationship_kind` supports `contains`, `calls`, `references`, `imports`, `exports`, `implements`, `extends`, `tests`, and `depends_on`. `confidence` is `structural` only when Tree-sitter evidence supports it; otherwise use `name_match`.

## Symbol Reference

**Fields**: `id`, `file_id`, `symbol_id`, `referenced_name`, `reference_kind`, `start_line`, `start_column`, `end_line`, `end_column`

**Relationships**: Belongs to file and optionally resolves to symbol.

**Validation Rules**: `referenced_name` and source location are required. Unresolved references remain queryable by name.

## Import

**Fields**: `id`, `file_id`, `import_text`, `imported_path`, `imported_symbol`, `start_line`, `start_column`

**Relationships**: Belongs to file.

**Validation Rules**: Raw import text and source location are required. Path/symbol fields may be null when extraction cannot split them confidently.

## Symbol FTS

**Fields**: `name`, `kind`, `signature`, `file_path`

**Relationships**: Uses symbol row id as FTS row id.

**Validation Rules**: Contentless FTS table; no full source contents stored by default.

## Evidence

**Fields**: `claim_type`, `file_path`, `line_range`, `symbol_name`, `relationship_kind`, `source_table`, `source_record_id`

**Relationships**: Internal Rust model can point to records from symbols, references, relationships, imports, or parse errors.

**Validation Rules**: Evidence must identify the source table and enough location detail for CLI/TUI output to explain why a result matched.
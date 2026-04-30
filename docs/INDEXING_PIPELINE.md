# Indexing Pipeline

## Pipeline

```text
repo root
  -> discover files
  -> apply ignore rules
  -> hash contents
  -> skip unchanged files
  -> parse with tree-sitter
  -> extract symbols
  -> extract call-like references
  -> build chunks
  -> detect sensitive chunks
  -> embed allowed chunks with Ollama
  -> persist facts to SQLite
  -> upsert vectors to Qdrant
  -> write index run summary
```

## File discovery

Respect:

- `.gitignore`
- `.symdexignore` when added
- built-in excludes: `.git`, `target`, `node_modules`, vendor caches, generated lock-heavy folders

Never follow symlinks outside the repository root.

Current implementation applies built-in directory excludes and simple scoped
`.gitignore` rules from the repository root and nested directories. Literal
file paths, directory suffix rules, and basename rules are supported. Glob
patterns and negation rules are intentionally not implemented yet.

## Chunking strategy

Preferred chunk units:

1. function
2. method
3. impl block summary
4. struct/enum/trait definition
5. module-level summary
6. fallback file chunk only when no better unit exists

Every chunk must include:

- repo-relative path
- byte range
- start/end line
- language
- symbol ID when available
- content hash
- chunk kind

Current implementation extracts Rust `function_item` syntax nodes as function
chunks, classifies functions under `impl_item` nodes as method chunks, and emits
a file fallback chunk only when no function-like chunks exist. Files with
tree-sitter syntax errors currently fail closed instead of producing partial
chunks.

## Embeddings

Use Ollama with `nomic-embed-text`.

Store:

- embedding model
- model tag
- vector dimension
- content hash
- embedding timestamp
- Qdrant point ID

If model name or vector dimension changes, require full reindex or collection migration.

## Call extraction

Start conservative.

Capture:

- caller symbol ID
- callee textual name
- call site line
- confidence
- resolution status

Resolution states:

- `resolved_exact`
- `resolved_local_candidate`
- `unresolved`
- `ambiguous`

Never drop unresolved calls. They are useful evidence.

## Incremental indexing

A file can be skipped only when:

- path is unchanged
- content hash is unchanged
- index version is unchanged
- parser/chunker version is unchanged
- embedding model and dimension are unchanged

Changed files should replace their SQLite facts and Qdrant points atomically where practical.

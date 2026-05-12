CREATE VIRTUAL TABLE symbol_fts USING fts5(
  name,
  kind,
  signature,
  file_path,
  content='',
  tokenize='unicode61'
);

CREATE INDEX idx_files_repo_path ON files(repository_id, path);
CREATE INDEX idx_files_hash ON files(content_hash);
CREATE INDEX idx_symbols_name ON symbols(name);
CREATE INDEX idx_symbols_kind ON symbols(kind);
CREATE INDEX idx_symbols_file ON symbols(file_id);
CREATE INDEX idx_symbols_parent ON symbols(parent_symbol_id);
CREATE INDEX idx_relationships_source ON symbol_relationships(source_symbol_id);
CREATE INDEX idx_relationships_target ON symbol_relationships(target_symbol_id);
CREATE INDEX idx_relationships_kind ON symbol_relationships(relationship_kind);
CREATE INDEX idx_references_name ON symbol_references(referenced_name);
CREATE INDEX idx_imports_file ON imports(file_id);
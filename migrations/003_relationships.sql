CREATE TABLE symbol_relationships (
  id INTEGER PRIMARY KEY,
  source_symbol_id INTEGER,
  target_symbol_id INTEGER,
  source_file_id INTEGER NOT NULL,
  target_file_id INTEGER,
  relationship_kind TEXT NOT NULL,
  confidence TEXT NOT NULL DEFAULT 'structural',
  evidence TEXT,
  FOREIGN KEY(source_symbol_id) REFERENCES symbols(id) ON DELETE CASCADE,
  FOREIGN KEY(target_symbol_id) REFERENCES symbols(id) ON DELETE SET NULL,
  FOREIGN KEY(source_file_id) REFERENCES files(id) ON DELETE CASCADE,
  FOREIGN KEY(target_file_id) REFERENCES files(id) ON DELETE SET NULL
);

CREATE TABLE symbol_references (
  id INTEGER PRIMARY KEY,
  file_id INTEGER NOT NULL,
  symbol_id INTEGER,
  referenced_name TEXT NOT NULL,
  reference_kind TEXT NOT NULL,
  start_line INTEGER NOT NULL,
  start_column INTEGER NOT NULL,
  end_line INTEGER NOT NULL,
  end_column INTEGER NOT NULL,
  FOREIGN KEY(file_id) REFERENCES files(id) ON DELETE CASCADE,
  FOREIGN KEY(symbol_id) REFERENCES symbols(id) ON DELETE SET NULL
);

CREATE TABLE imports (
  id INTEGER PRIMARY KEY,
  file_id INTEGER NOT NULL,
  import_text TEXT NOT NULL,
  imported_path TEXT,
  imported_symbol TEXT,
  start_line INTEGER NOT NULL,
  start_column INTEGER NOT NULL,
  FOREIGN KEY(file_id) REFERENCES files(id) ON DELETE CASCADE
);
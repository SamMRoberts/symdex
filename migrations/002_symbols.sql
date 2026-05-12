CREATE TABLE symbols (
  id INTEGER PRIMARY KEY,
  file_id INTEGER NOT NULL,
  parent_symbol_id INTEGER,
  name TEXT NOT NULL,
  kind TEXT NOT NULL,
  language TEXT NOT NULL,
  signature TEXT,
  visibility TEXT,
  start_line INTEGER NOT NULL,
  start_column INTEGER NOT NULL,
  end_line INTEGER NOT NULL,
  end_column INTEGER NOT NULL,
  start_byte INTEGER NOT NULL,
  end_byte INTEGER NOT NULL,
  symbol_hash TEXT NOT NULL,
  FOREIGN KEY(file_id) REFERENCES files(id) ON DELETE CASCADE,
  FOREIGN KEY(parent_symbol_id) REFERENCES symbols(id) ON DELETE CASCADE
);
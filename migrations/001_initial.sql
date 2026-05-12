CREATE TABLE repositories (
  id INTEGER PRIMARY KEY,
  root_path TEXT NOT NULL UNIQUE,
  name TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);

CREATE TABLE files (
  id INTEGER PRIMARY KEY,
  repository_id INTEGER NOT NULL,
  path TEXT NOT NULL,
  absolute_path TEXT,
  language TEXT,
  content_hash TEXT NOT NULL,
  size_bytes INTEGER NOT NULL,
  indexed_at TEXT NOT NULL,
  deleted_at TEXT,
  UNIQUE(repository_id, path),
  FOREIGN KEY(repository_id) REFERENCES repositories(id) ON DELETE CASCADE
);

CREATE TABLE parse_runs (
  id INTEGER PRIMARY KEY,
  repository_id INTEGER NOT NULL,
  started_at TEXT NOT NULL,
  finished_at TEXT,
  full_reindex INTEGER NOT NULL DEFAULT 0,
  files_scanned INTEGER NOT NULL DEFAULT 0,
  files_parsed INTEGER NOT NULL DEFAULT 0,
  files_skipped INTEGER NOT NULL DEFAULT 0,
  parse_errors INTEGER NOT NULL DEFAULT 0,
  FOREIGN KEY(repository_id) REFERENCES repositories(id) ON DELETE CASCADE
);

CREATE TABLE parse_errors (
  id INTEGER PRIMARY KEY,
  file_id INTEGER NOT NULL,
  parse_run_id INTEGER NOT NULL,
  start_line INTEGER NOT NULL,
  start_column INTEGER NOT NULL,
  end_line INTEGER,
  end_column INTEGER,
  message TEXT NOT NULL,
  FOREIGN KEY(file_id) REFERENCES files(id) ON DELETE CASCADE,
  FOREIGN KEY(parse_run_id) REFERENCES parse_runs(id) ON DELETE CASCADE
);
//! Persistence boundary for SQLite and sqlite-vec adapters.

mod vector;

use std::collections::BTreeSet;
use std::env;
use std::fmt::{Display, Formatter};
use std::fs::{File, OpenOptions};
use std::io::{ErrorKind, Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use fs2::FileExt;
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use symdex_core::{
    RepositoryRefKind, RepositoryRefSnapshot, SemanticLayer, SemanticLayerStatus, stable_id,
};

pub use vector::{
    PointPayload, RetrievedPoint, ScoredPoint, SqliteVectorStore, VectorPoint, vector_point_id,
    vector_rowid, vector_table_name,
};

#[cfg(test)]
pub(crate) use vector::validate_vector_table_name;

pub const MAX_CALL_PATH_DEPTH: usize = 8;

pub fn clamp_call_path_depth(depth: usize) -> usize {
    depth.clamp(1, MAX_CALL_PATH_DEPTH)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreConfig {
    pub sqlite_path: PathBuf,
}

impl StoreConfig {
    pub fn from_env() -> Self {
        Self {
            sqlite_path: env_path("SYMDEX_DB_PATH", "symdex_DB_PATH")
                .unwrap_or_else(|| PathBuf::from(".symdex/symdex.sqlite")),
        }
    }
}

pub fn sqlite_parent(config: &StoreConfig) -> Option<PathBuf> {
    config.sqlite_path.parent().map(PathBuf::from)
}

pub fn writer_lock_path(config: &StoreConfig) -> PathBuf {
    let lock_name = config
        .sqlite_path
        .file_name()
        .and_then(|name| name.to_str())
        .map(|name| format!("{name}.writer.lock"))
        .unwrap_or_else(|| "symdex.sqlite.writer.lock".to_owned());
    config
        .sqlite_path
        .parent()
        .map(|parent| parent.join(&lock_name))
        .unwrap_or_else(|| PathBuf::from(lock_name))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WriterLeaseKind {
    Init,
    ManualIndex,
    WatcherDaemon,
    WatcherForeground,
    WatcherLauncher,
    QualityIndex,
    VectorRepair,
    Maintenance,
}

impl WriterLeaseKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Init => "init",
            Self::ManualIndex => "manual_index",
            Self::WatcherDaemon => "watcher_daemon",
            Self::WatcherForeground => "watcher_foreground",
            Self::WatcherLauncher => "watcher_launcher",
            Self::QualityIndex => "quality_index",
            Self::VectorRepair => "vector_repair",
            Self::Maintenance => "maintenance",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriterLeaseRequest {
    pub kind: WriterLeaseKind,
    pub operation: String,
    pub repository_id: Option<String>,
    pub repo_root: Option<String>,
}

impl WriterLeaseRequest {
    pub fn new(kind: WriterLeaseKind, operation: impl Into<String>) -> Self {
        Self {
            kind,
            operation: operation.into(),
            repository_id: None,
            repo_root: None,
        }
    }

    pub fn for_repo(mut self, repository_id: &str, repo_root: impl Into<String>) -> Self {
        self.repository_id = Some(repository_id.to_owned());
        self.repo_root = Some(repo_root.into());
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WriterLeaseInfo {
    pub owner_kind: String,
    pub pid: u32,
    pub operation: String,
    pub repository_id: Option<String>,
    pub repo_root: Option<String>,
    pub started_at: String,
}

impl WriterLeaseInfo {
    pub fn from_request(request: &WriterLeaseRequest) -> Self {
        Self {
            owner_kind: request.kind.as_str().to_owned(),
            pid: std::process::id(),
            operation: request.operation.clone(),
            repository_id: request.repository_id.clone(),
            repo_root: request.repo_root.clone(),
            started_at: current_timestamp(),
        }
    }

    pub fn read_for(config: &StoreConfig) -> Result<Option<Self>> {
        read_writer_lease_info(&writer_lock_path(config))
    }
}

#[derive(Debug)]
pub struct WriterLease {
    file: File,
    lock_path: PathBuf,
    pub info: WriterLeaseInfo,
}

impl WriterLease {
    pub fn acquire(config: &StoreConfig, request: WriterLeaseRequest) -> Result<Self> {
        if let Some(parent) = sqlite_parent(config) {
            std::fs::create_dir_all(&parent).map_err(StoreError::Io)?;
        }
        let lock_path = writer_lock_path(config);
        let mut file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(&lock_path)
            .map_err(StoreError::Io)?;
        match file.try_lock_exclusive() {
            Ok(()) => {}
            Err(error) if error.kind() == ErrorKind::WouldBlock => {
                let owner = read_writer_lease_info(&lock_path).ok().flatten();
                return Err(StoreError::WriterBusy { owner });
            }
            Err(error) => return Err(StoreError::Io(error)),
        }

        let info = WriterLeaseInfo::from_request(&request);
        file.set_len(0).map_err(StoreError::Io)?;
        file.seek(SeekFrom::Start(0)).map_err(StoreError::Io)?;
        let metadata = serde_json::to_vec(&info).map_err(StoreError::Json)?;
        file.write_all(&metadata).map_err(StoreError::Io)?;
        file.write_all(b"\n").map_err(StoreError::Io)?;
        file.sync_data().map_err(StoreError::Io)?;
        Ok(Self {
            file,
            lock_path,
            info,
        })
    }

    pub fn lock_path(&self) -> &PathBuf {
        &self.lock_path
    }
}

impl Drop for WriterLease {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

fn read_writer_lease_info(lock_path: &PathBuf) -> Result<Option<WriterLeaseInfo>> {
    let mut file = match File::open(lock_path) {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(StoreError::Io(error)),
    };
    let mut contents = String::new();
    file.read_to_string(&mut contents).map_err(StoreError::Io)?;
    if contents.trim().is_empty() {
        return Ok(None);
    }
    serde_json::from_str(contents.trim())
        .map(Some)
        .map_err(StoreError::Json)
}

#[derive(Debug)]
pub struct SqliteStore {
    connection: Connection,
}

impl SqliteStore {
    pub fn open(config: &StoreConfig) -> Result<Self> {
        symdex_sqlite_vec::register_sqlite_vec().map_err(StoreError::SqliteVecRegistration)?;
        if let Some(parent) = sqlite_parent(config) {
            std::fs::create_dir_all(&parent).map_err(StoreError::Io)?;
        }
        let initialize_wal = !config.sqlite_path.exists();
        let connection = Connection::open(&config.sqlite_path).map_err(StoreError::Sqlite)?;
        connection
            .busy_timeout(Duration::from_secs(30))
            .map_err(StoreError::Sqlite)?;
        connection
            .execute_batch("PRAGMA foreign_keys = ON;")
            .map_err(StoreError::Sqlite)?;
        if initialize_wal {
            connection
                .execute_batch("PRAGMA journal_mode = WAL;")
                .map_err(StoreError::Sqlite)?;
        }
        Ok(Self { connection })
    }

    pub fn open_read_only(config: &StoreConfig) -> Result<Self> {
        symdex_sqlite_vec::register_sqlite_vec().map_err(StoreError::SqliteVecRegistration)?;
        let connection =
            Connection::open_with_flags(&config.sqlite_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
                .map_err(StoreError::Sqlite)?;
        connection
            .busy_timeout(Duration::from_secs(30))
            .map_err(StoreError::Sqlite)?;
        connection
            .execute_batch("PRAGMA foreign_keys = ON;")
            .map_err(StoreError::Sqlite)?;
        Ok(Self { connection })
    }

    pub fn migrate(&self) -> Result<()> {
        self.connection
            .execute_batch(SCHEMA)
            .map_err(StoreError::Sqlite)?;
        self.ensure_compatibility_columns()?;
        self.ensure_content_addressed_files()?;
        Ok(())
    }

    fn ensure_content_addressed_files(&self) -> Result<()> {
        if self.files_has_unique_repository_path_constraint()? {
            self.connection
                .execute_batch(
                    "PRAGMA foreign_keys = OFF;
                     BEGIN;
                     CREATE TABLE files_new (
                       id TEXT PRIMARY KEY,
                       repository_id TEXT NOT NULL,
                       path TEXT NOT NULL,
                       language TEXT NOT NULL,
                       content_hash TEXT NOT NULL,
                       indexed_at TEXT NOT NULL,
                       index_run_id TEXT,
                       parser_version TEXT,
                       FOREIGN KEY(repository_id) REFERENCES repositories(id) ON DELETE CASCADE
                     );
                     INSERT OR IGNORE INTO files_new (
                       id, repository_id, path, language, content_hash, indexed_at,
                       index_run_id, parser_version
                     )
                     SELECT id, repository_id, path, language, content_hash, indexed_at,
                            index_run_id, parser_version
                       FROM files;
                     DROP TABLE files;
                     ALTER TABLE files_new RENAME TO files;
                     COMMIT;
                     PRAGMA foreign_keys = ON;",
                )
                .map_err(StoreError::Sqlite)?;
        }
        self.connection
            .execute_batch(FILES_CONTENT_ADDRESSING_INDEXES)
            .map_err(StoreError::Sqlite)?;
        Ok(())
    }

    fn files_has_unique_repository_path_constraint(&self) -> Result<bool> {
        let mut statement = self
            .connection
            .prepare("PRAGMA index_list(files)")
            .map_err(StoreError::Sqlite)?;
        let rows = statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(1)?, row.get::<_, i64>(2)?))
            })
            .map_err(StoreError::Sqlite)?;
        for row in rows {
            let (index_name, unique) = row.map_err(StoreError::Sqlite)?;
            if unique == 0 {
                continue;
            }
            if self.index_columns(&index_name)? == ["repository_id", "path"] {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn index_columns(&self, index_name: &str) -> Result<Vec<String>> {
        let escaped = index_name.replace('"', "\"\"");
        let mut statement = self
            .connection
            .prepare(&format!("PRAGMA index_info(\"{escaped}\")"))
            .map_err(StoreError::Sqlite)?;
        let rows = statement
            .query_map([], |row| row.get::<_, String>(2))
            .map_err(StoreError::Sqlite)?;
        collect_rows(rows)
    }

    fn ensure_compatibility_columns(&self) -> Result<()> {
        for column in COMPATIBILITY_COLUMNS {
            self.ensure_column(column)?;
        }
        Ok(())
    }

    fn ensure_column(&self, column: &ProvenanceColumn) -> Result<()> {
        if self.column_exists(column.table, column.name)? {
            return Ok(());
        }
        self.connection
            .execute(column.alter_sql, [])
            .map_err(StoreError::Sqlite)?;
        Ok(())
    }

    fn column_exists(&self, table: &str, column: &str) -> Result<bool> {
        if !table
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '_')
        {
            return Err(StoreError::UnexpectedResponse(format!(
                "invalid SQLite table name `{table}`"
            )));
        }
        let mut statement = self
            .connection
            .prepare(&format!("PRAGMA table_info({table})"))
            .map_err(StoreError::Sqlite)?;
        let rows = statement
            .query_map([], |row| row.get::<_, String>(1))
            .map_err(StoreError::Sqlite)?;
        for row in rows {
            if row.map_err(StoreError::Sqlite)? == column {
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub fn upsert_repository(&self, repository: &RepositoryRecord) -> Result<()> {
        let now = timestamp();
        self.connection
            .execute(
                "INSERT INTO repositories (id, root_path, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?3)
                 ON CONFLICT(id) DO UPDATE SET
                   root_path = excluded.root_path,
                   updated_at = excluded.updated_at",
                params![repository.id, repository.root_path, now],
            )
            .map_err(StoreError::Sqlite)?;
        Ok(())
    }

    pub fn sync_repository_ref(
        &self,
        snapshot: &RepositoryRefSnapshot,
    ) -> Result<RepositoryRefSyncSummary> {
        let now = timestamp();
        let current = RepositoryRefRecord::from_snapshot(snapshot, true, &now);
        let transaction = self
            .connection
            .unchecked_transaction()
            .map_err(StoreError::Sqlite)?;
        transaction
            .execute(
                "UPDATE repository_refs
                    SET is_current = 0,
                        updated_at = ?2
                  WHERE repository_id = ?1",
                params![snapshot.repository_id, now],
            )
            .map_err(StoreError::Sqlite)?;
        for branch in &snapshot.local_branches {
            let record = RepositoryRefRecord::local_branch(&snapshot.repository_id, branch, &now);
            upsert_repository_ref_in_transaction(&transaction, &record)?;
        }
        upsert_repository_ref_in_transaction(&transaction, &current)?;

        let mut deleted_refs = 0usize;
        {
            if !snapshot.local_branches.is_empty() {
                let active = snapshot
                    .local_branches
                    .iter()
                    .map(String::as_str)
                    .collect::<std::collections::BTreeSet<_>>();
                let mut statement = transaction
                    .prepare(
                        "SELECT id, ref_name
                           FROM repository_refs
                          WHERE repository_id = ?1
                            AND ref_kind = 'branch'
                            AND deleted_at IS NULL",
                    )
                    .map_err(StoreError::Sqlite)?;
                let rows = statement
                    .query_map(params![snapshot.repository_id], |row| {
                        Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
                    })
                    .map_err(StoreError::Sqlite)?;
                for row in rows {
                    let (id, ref_name) = row.map_err(StoreError::Sqlite)?;
                    if ref_name
                        .as_deref()
                        .is_some_and(|name| !active.contains(name))
                    {
                        deleted_refs += transaction
                            .execute(
                                "UPDATE repository_refs
                                    SET deleted_at = ?2,
                                        is_current = 0,
                                        updated_at = ?2
                                  WHERE id = ?1",
                                params![id, now],
                            )
                            .map_err(StoreError::Sqlite)?;
                    }
                }
            }
        }
        transaction.commit().map_err(StoreError::Sqlite)?;
        Ok(RepositoryRefSyncSummary {
            current,
            deleted_refs,
        })
    }

    pub fn current_repository_ref(
        &self,
        repository_id: &str,
    ) -> Result<Option<RepositoryRefRecord>> {
        self.connection
            .query_row(
                "SELECT id, repository_id, ref_kind, ref_name, ref_identity, head_oid,
                        is_current, last_seen_at, deleted_at, created_at, updated_at
                   FROM repository_refs
                  WHERE repository_id = ?1
                    AND is_current = 1
                    AND deleted_at IS NULL
                  ORDER BY last_seen_at DESC, updated_at DESC, id DESC
                  LIMIT 1",
                params![repository_id],
                repository_ref_record,
            )
            .optional()
            .map_err(StoreError::Sqlite)
    }

    pub fn latest_file_state_for_path(
        &self,
        repository_id: &str,
        path: &str,
    ) -> Result<Option<FileStateRecord>> {
        self.connection
            .query_row(
                "SELECT path, content_hash
                   FROM files
                  WHERE repository_id = ?1
                    AND path = ?2
                  ORDER BY indexed_at DESC, index_run_id DESC, id DESC
                  LIMIT 1",
                params![repository_id, path],
                file_state_record,
            )
            .optional()
            .map_err(StoreError::Sqlite)
    }

    pub fn file_unchanged(
        &self,
        repository_id: &str,
        file_id: &str,
        path: &str,
        content_hash: &str,
        parser_version: &str,
    ) -> Result<bool> {
        let unchanged: i64 = self
            .connection
            .query_row(
                "SELECT EXISTS(
                   SELECT 1
                     FROM files
                    WHERE repository_id = ?1
                      AND id = ?2
                      AND path = ?3
                      AND content_hash = ?4
                      AND parser_version = ?5
                 )",
                params![repository_id, file_id, path, content_hash, parser_version],
                |row| row.get(0),
            )
            .map_err(StoreError::Sqlite)?;
        Ok(unchanged != 0)
    }

    pub fn file_index_states(&self, repository_id: &str) -> Result<Vec<FileStateRecord>> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT path, content_hash
                   FROM files
                  WHERE repository_id = ?1
                  ORDER BY path, indexed_at DESC, index_run_id DESC, id DESC",
            )
            .map_err(StoreError::Sqlite)?;
        let rows = statement
            .query_map(params![repository_id], file_state_record)
            .map_err(StoreError::Sqlite)?;
        let mut latest = Vec::new();
        let mut seen = BTreeSet::new();
        for row in rows {
            let state = row.map_err(StoreError::Sqlite)?;
            if seen.insert(state.path.clone()) {
                latest.push(state);
            }
        }
        Ok(latest)
    }

    pub fn ref_file_index_states(&self, repository_ref_id: &str) -> Result<Vec<FileStateRecord>> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT ref_files.path, files.content_hash
                   FROM ref_files
                   JOIN files ON files.id = ref_files.file_id
                  WHERE ref_files.repository_ref_id = ?1
                  ORDER BY ref_files.path",
            )
            .map_err(StoreError::Sqlite)?;
        let rows = statement
            .query_map(params![repository_ref_id], file_state_record)
            .map_err(StoreError::Sqlite)?;
        collect_rows(rows)
    }

    pub fn record_file_index_events(&mut self, events: &[FileIndexEventRecord]) -> Result<()> {
        if events.is_empty() {
            return Ok(());
        }
        let occurred_at = timestamp();
        let transaction = self.connection.transaction().map_err(StoreError::Sqlite)?;
        for event in events {
            transaction
                .execute(
                    "INSERT INTO file_index_events (
                       id, index_run_id, repository_id, repository_ref_id, path,
                       old_content_hash, new_content_hash, action, reason, status,
                       error_summary, occurred_at
                     )
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                    params![
                        event.id,
                        event.index_run_id,
                        event.repository_id,
                        event.repository_ref_id,
                        event.path,
                        event.old_content_hash,
                        event.new_content_hash,
                        event.action,
                        event.reason,
                        event.status,
                        event.error_summary,
                        occurred_at,
                    ],
                )
                .map_err(StoreError::Sqlite)?;
        }
        transaction.commit().map_err(StoreError::Sqlite)?;
        Ok(())
    }

    pub fn link_file_to_ref(&mut self, repository_ref_id: &str, file: &FileRecord) -> Result<()> {
        let indexed_at = timestamp();
        self.connection
            .execute(
                "INSERT INTO ref_files (
                   repository_ref_id, repository_id, path, file_id, indexed_at, index_run_id
                 )
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(repository_ref_id, path) DO UPDATE SET
                   repository_id = excluded.repository_id,
                   file_id = excluded.file_id,
                   indexed_at = excluded.indexed_at,
                   index_run_id = excluded.index_run_id",
                params![
                    repository_ref_id,
                    file.repository_id,
                    file.path,
                    file.id,
                    indexed_at,
                    file.index_run_id,
                ],
            )
            .map_err(StoreError::Sqlite)?;
        Ok(())
    }

    pub fn replace_file_facts(
        &mut self,
        file: &FileRecord,
        symbols: &[SymbolRecord],
        chunks: &[ChunkRecord],
        calls: &[CallRecord],
    ) -> Result<()> {
        self.replace_file_facts_with_tests(file, symbols, chunks, calls, &[])
    }

    pub fn replace_file_facts_with_tests(
        &mut self,
        file: &FileRecord,
        symbols: &[SymbolRecord],
        chunks: &[ChunkRecord],
        calls: &[CallRecord],
        tests: &[TestRecord],
    ) -> Result<()> {
        self.replace_file_facts_inner(None, file, symbols, chunks, calls, &[], tests)
    }

    pub fn replace_file_facts_with_references_and_tests(
        &mut self,
        file: &FileRecord,
        symbols: &[SymbolRecord],
        chunks: &[ChunkRecord],
        calls: &[CallRecord],
        symbol_references: &[SymbolReferenceRecord],
        tests: &[TestRecord],
    ) -> Result<()> {
        self.replace_file_facts_inner(None, file, symbols, chunks, calls, symbol_references, tests)
    }

    pub fn replace_file_facts_for_ref_with_tests(
        &mut self,
        repository_ref_id: &str,
        file: &FileRecord,
        symbols: &[SymbolRecord],
        chunks: &[ChunkRecord],
        calls: &[CallRecord],
        tests: &[TestRecord],
    ) -> Result<()> {
        self.replace_file_facts_inner(
            Some(repository_ref_id),
            file,
            symbols,
            chunks,
            calls,
            &[],
            tests,
        )
    }

    pub fn replace_file_facts_for_ref_with_references_and_tests(
        &mut self,
        repository_ref_id: &str,
        file: &FileRecord,
        symbols: &[SymbolRecord],
        chunks: &[ChunkRecord],
        calls: &[CallRecord],
        symbol_references: &[SymbolReferenceRecord],
        tests: &[TestRecord],
    ) -> Result<()> {
        self.replace_file_facts_inner(
            Some(repository_ref_id),
            file,
            symbols,
            chunks,
            calls,
            symbol_references,
            tests,
        )
    }

    fn replace_file_facts_inner(
        &mut self,
        repository_ref_id: Option<&str>,
        file: &FileRecord,
        symbols: &[SymbolRecord],
        chunks: &[ChunkRecord],
        calls: &[CallRecord],
        symbol_references: &[SymbolReferenceRecord],
        tests: &[TestRecord],
    ) -> Result<()> {
        let indexed_at = timestamp();
        let transaction = self.connection.transaction().map_err(StoreError::Sqlite)?;
        transaction
            .execute(
                "INSERT INTO files (
                   id, repository_id, path, language, content_hash, indexed_at,
                   index_run_id, parser_version
                 )
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                                    ON CONFLICT(id) DO UPDATE SET
                                        repository_id = excluded.repository_id,
                                        path = excluded.path,
                    language = excluded.language,
                    content_hash = excluded.content_hash,
                    indexed_at = excluded.indexed_at,
                    index_run_id = excluded.index_run_id,
                    parser_version = excluded.parser_version",
                params![
                    file.id,
                    file.repository_id,
                    file.path,
                    file.language,
                    file.content_hash,
                    indexed_at,
                    file.index_run_id,
                    file.parser_version
                ],
            )
            .map_err(StoreError::Sqlite)?;
        if let Some(repository_ref_id) = repository_ref_id {
            transaction
                .execute(
                    "INSERT INTO ref_files (
                       repository_ref_id, repository_id, path, file_id, indexed_at, index_run_id
                     )
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                     ON CONFLICT(repository_ref_id, path) DO UPDATE SET
                       repository_id = excluded.repository_id,
                       file_id = excluded.file_id,
                       indexed_at = excluded.indexed_at,
                       index_run_id = excluded.index_run_id",
                    params![
                        repository_ref_id,
                        file.repository_id,
                        file.path,
                        file.id,
                        indexed_at,
                        file.index_run_id,
                    ],
                )
                .map_err(StoreError::Sqlite)?;
        }
        let stale_same_path_file_ids = {
            let mut statement = transaction
                .prepare(
                    "SELECT id
                       FROM files
                      WHERE repository_id = ?1
                        AND path = ?2
                        AND id <> ?3
                      ORDER BY indexed_at, id",
                )
                .map_err(StoreError::Sqlite)?;
            let rows = statement
                .query_map(params![file.repository_id, file.path, file.id], |row| {
                    row.get::<_, String>(0)
                })
                .map_err(StoreError::Sqlite)?;
            collect_rows(rows)?
        };
        transaction
            .execute(
                "DELETE FROM calls
                 WHERE caller_symbol_id IN (SELECT id FROM symbols WHERE file_id = ?1)",
                params![file.id],
            )
            .map_err(StoreError::Sqlite)?;
        transaction
            .execute(
                "DELETE FROM symbol_references
                 WHERE file_id = ?1",
                params![file.id],
            )
            .map_err(StoreError::Sqlite)?;
        transaction
            .execute("DELETE FROM tests WHERE file_id = ?1", params![file.id])
            .map_err(StoreError::Sqlite)?;
        transaction
            .execute("DELETE FROM chunks WHERE file_id = ?1", params![file.id])
            .map_err(StoreError::Sqlite)?;
        transaction
            .execute("DELETE FROM symbols WHERE file_id = ?1", params![file.id])
            .map_err(StoreError::Sqlite)?;

        for stale_file_id in stale_same_path_file_ids {
            let ref_count: i64 = if repository_ref_id.is_some() {
                transaction
                    .query_row(
                        "SELECT COUNT(*) FROM ref_files WHERE file_id = ?1",
                        params![&stale_file_id],
                        |row| row.get(0),
                    )
                    .map_err(StoreError::Sqlite)?
            } else {
                0
            };
            if ref_count > 0 {
                continue;
            }
            transaction
                .execute(
                    "DELETE FROM calls
                     WHERE caller_symbol_id IN (SELECT id FROM symbols WHERE file_id = ?1)",
                    params![&stale_file_id],
                )
                .map_err(StoreError::Sqlite)?;
            transaction
                .execute(
                    "DELETE FROM symbol_references
                     WHERE file_id = ?1",
                    params![&stale_file_id],
                )
                .map_err(StoreError::Sqlite)?;
            transaction
                .execute(
                    "DELETE FROM tests WHERE file_id = ?1",
                    params![&stale_file_id],
                )
                .map_err(StoreError::Sqlite)?;
            transaction
                .execute(
                    "DELETE FROM chunks WHERE file_id = ?1",
                    params![&stale_file_id],
                )
                .map_err(StoreError::Sqlite)?;
            transaction
                .execute(
                    "DELETE FROM symbols WHERE file_id = ?1",
                    params![&stale_file_id],
                )
                .map_err(StoreError::Sqlite)?;
            transaction
                .execute("DELETE FROM files WHERE id = ?1", params![&stale_file_id])
                .map_err(StoreError::Sqlite)?;
        }

        {
            let mut statement = transaction
                .prepare(
                    "INSERT INTO symbols (
                        id, file_id, parent_symbol_id, name, qualified_name, kind, signature,
                        start_line, end_line, start_byte, end_byte, index_run_id, parser_version
                      )
                      VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                )
                .map_err(StoreError::Sqlite)?;
            for symbol in symbols {
                statement
                    .execute(params![
                        symbol.id,
                        symbol.file_id,
                        symbol.parent_symbol_id,
                        symbol.name,
                        symbol.qualified_name,
                        symbol.kind,
                        symbol.signature,
                        symbol.start_line as i64,
                        symbol.end_line as i64,
                        symbol.start_byte as i64,
                        symbol.end_byte as i64,
                        symbol.index_run_id,
                        symbol.parser_version,
                    ])
                    .map_err(StoreError::Sqlite)?;
            }
        }

        {
            let mut statement = transaction
                .prepare(
                    "INSERT INTO symbol_references (
                        id, file_id, source_symbol_id, target_symbol_id, reference_text, reference_kind,
                        line, confidence, resolution_status, index_run_id, parser_version
                      )
                      VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
                      ON CONFLICT(id) DO UPDATE SET
                        file_id = excluded.file_id,
                        source_symbol_id = excluded.source_symbol_id,
                        target_symbol_id = excluded.target_symbol_id,
                        reference_text = excluded.reference_text,
                        reference_kind = excluded.reference_kind,
                        line = excluded.line,
                        confidence = excluded.confidence,
                        resolution_status = excluded.resolution_status,
                        index_run_id = excluded.index_run_id,
                        parser_version = excluded.parser_version",
                )
                .map_err(StoreError::Sqlite)?;
            for reference in symbol_references {
                statement
                    .execute(params![
                        reference.id,
                        reference.file_id,
                        reference.source_symbol_id,
                        reference.target_symbol_id,
                        reference.reference_text,
                        reference.reference_kind,
                        reference.line as i64,
                        reference.confidence as f64,
                        reference.resolution_status,
                        reference.index_run_id,
                        reference.parser_version,
                    ])
                    .map_err(StoreError::Sqlite)?;
            }
        }

        {
            let mut statement = transaction
                .prepare(
                     "INSERT INTO chunks (
                        id, file_id, symbol_id, kind, text_hash,
                        start_line, end_line, start_byte, end_byte,
                        vector_point_id, excluded_reason, index_run_id, parser_version,
                        embedding_model, embedding_dimension, embedded_at
                      )
                      VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
                )
                .map_err(StoreError::Sqlite)?;
            for chunk in chunks {
                statement
                    .execute(params![
                        chunk.id,
                        chunk.file_id,
                        chunk.symbol_id,
                        chunk.kind,
                        chunk.text_hash,
                        chunk.start_line as i64,
                        chunk.end_line as i64,
                        chunk.start_byte as i64,
                        chunk.end_byte as i64,
                        chunk.vector_point_id,
                        chunk.excluded_reason,
                        chunk.index_run_id,
                        chunk.parser_version,
                        chunk.embedding_model,
                        chunk.embedding_dimension.map(|dimension| dimension as i64),
                        chunk.embedded_at,
                    ])
                    .map_err(StoreError::Sqlite)?;
            }
        }

        {
            let mut statement = transaction
                .prepare(
                    "INSERT INTO calls (
                        id, caller_symbol_id, callee_text, callee_symbol_id,
                        call_line, confidence, resolution_status, index_run_id, parser_version
                      )
                      VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                      ON CONFLICT(id) DO UPDATE SET
                        caller_symbol_id = excluded.caller_symbol_id,
                        callee_text = excluded.callee_text,
                        callee_symbol_id = excluded.callee_symbol_id,
                        call_line = excluded.call_line,
                        confidence = excluded.confidence,
                        resolution_status = excluded.resolution_status,
                        index_run_id = excluded.index_run_id,
                        parser_version = excluded.parser_version",
                )
                .map_err(StoreError::Sqlite)?;
            for call in calls {
                statement
                    .execute(params![
                        call.id,
                        call.caller_symbol_id,
                        call.callee_text,
                        call.callee_symbol_id,
                        call.call_line as i64,
                        call.confidence as f64,
                        call.resolution_status,
                        call.index_run_id,
                        call.parser_version,
                    ])
                    .map_err(StoreError::Sqlite)?;
            }
        }

        {
            let mut statement = transaction
                .prepare(
                    "INSERT INTO tests (
                        id, repository_id, file_id, symbol_id, name, qualified_name, framework,
                        language, path, start_line, end_line, start_byte, end_byte,
                        index_run_id, parser_version, indexed_at
                      )
                      VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
                )
                .map_err(StoreError::Sqlite)?;
            for test in tests {
                statement
                    .execute(params![
                        test.id,
                        test.repository_id,
                        test.file_id,
                        test.symbol_id,
                        test.name,
                        test.qualified_name,
                        test.framework,
                        test.language,
                        test.path,
                        test.start_line as i64,
                        test.end_line as i64,
                        test.start_byte as i64,
                        test.end_byte as i64,
                        test.index_run_id,
                        test.parser_version,
                        timestamp(),
                    ])
                    .map_err(StoreError::Sqlite)?;
            }
        }

        transaction.commit().map_err(StoreError::Sqlite)?;
        Ok(())
    }

    pub fn ref_file_paths(&self, repository_ref_id: &str) -> Result<Vec<String>> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT path
                   FROM ref_files
                  WHERE repository_ref_id = ?1
                  ORDER BY path",
            )
            .map_err(StoreError::Sqlite)?;
        let rows = statement
            .query_map(params![repository_ref_id], |row| row.get(0))
            .map_err(StoreError::Sqlite)?;
        collect_rows(rows)
    }

    pub fn remove_missing_ref_files(
        &mut self,
        repository_ref_id: &str,
        active_paths: &[String],
    ) -> Result<usize> {
        let existing = self.ref_file_paths(repository_ref_id)?;
        let active: BTreeSet<&str> = active_paths.iter().map(String::as_str).collect();
        let missing: Vec<String> = existing
            .into_iter()
            .filter(|path| !active.contains(path.as_str()))
            .collect();
        if missing.is_empty() {
            return Ok(0);
        }

        let transaction = self.connection.transaction().map_err(StoreError::Sqlite)?;
        for path in &missing {
            transaction
                .execute(
                    "DELETE FROM ref_files
                      WHERE repository_ref_id = ?1
                        AND path = ?2",
                    params![repository_ref_id, path],
                )
                .map_err(StoreError::Sqlite)?;
        }
        transaction.commit().map_err(StoreError::Sqlite)?;
        Ok(missing.len())
    }

    pub fn remove_unreferenced_missing_files(
        &mut self,
        repository_id: &str,
        active_paths: &[String],
    ) -> Result<usize> {
        let existing = self.file_paths(repository_id)?;
        let active: BTreeSet<&str> = active_paths.iter().map(String::as_str).collect();
        let missing: Vec<String> = existing
            .into_iter()
            .filter(|path| !active.contains(path.as_str()))
            .collect();
        if missing.is_empty() {
            return Ok(0);
        }

        let transaction = self.connection.transaction().map_err(StoreError::Sqlite)?;
        let mut removed = 0usize;
        for path in &missing {
            let file_ids = {
                let mut statement = transaction
                    .prepare("SELECT id FROM files WHERE repository_id = ?1 AND path = ?2")
                    .map_err(StoreError::Sqlite)?;
                let rows = statement
                    .query_map(params![repository_id, path], |row| row.get::<_, String>(0))
                    .map_err(StoreError::Sqlite)?;
                collect_rows(rows)?
            };
            for file_id in file_ids {
                let ref_count: i64 = transaction
                    .query_row(
                        "SELECT COUNT(*) FROM ref_files WHERE file_id = ?1",
                        params![&file_id],
                        |row| row.get(0),
                    )
                    .map_err(StoreError::Sqlite)?;
                if ref_count > 0 {
                    continue;
                }
                transaction
                    .execute(
                        "DELETE FROM calls
                     WHERE caller_symbol_id IN (SELECT id FROM symbols WHERE file_id = ?1)",
                        params![&file_id],
                    )
                    .map_err(StoreError::Sqlite)?;
                transaction
                    .execute(
                        "DELETE FROM symbol_references
                         WHERE file_id = ?1",
                        params![&file_id],
                    )
                    .map_err(StoreError::Sqlite)?;
                transaction
                    .execute("DELETE FROM tests WHERE file_id = ?1", params![&file_id])
                    .map_err(StoreError::Sqlite)?;
                transaction
                    .execute("DELETE FROM chunks WHERE file_id = ?1", params![&file_id])
                    .map_err(StoreError::Sqlite)?;
                transaction
                    .execute("DELETE FROM symbols WHERE file_id = ?1", params![&file_id])
                    .map_err(StoreError::Sqlite)?;
                removed += transaction
                    .execute("DELETE FROM files WHERE id = ?1", params![&file_id])
                    .map_err(StoreError::Sqlite)?;
            }
        }
        transaction.commit().map_err(StoreError::Sqlite)?;
        Ok(removed)
    }

    pub fn remove_missing_files(
        &mut self,
        repository_id: &str,
        active_paths: &[String],
    ) -> Result<usize> {
        let existing = self.file_paths(repository_id)?;
        let active: BTreeSet<&str> = active_paths.iter().map(String::as_str).collect();
        let missing: Vec<String> = existing
            .into_iter()
            .filter(|path| !active.contains(path.as_str()))
            .collect();
        if missing.is_empty() {
            return Ok(0);
        }

        let transaction = self.connection.transaction().map_err(StoreError::Sqlite)?;
        for path in &missing {
            let file_ids = {
                let mut statement = transaction
                    .prepare("SELECT id FROM files WHERE repository_id = ?1 AND path = ?2")
                    .map_err(StoreError::Sqlite)?;
                let rows = statement
                    .query_map(params![repository_id, path], |row| row.get::<_, String>(0))
                    .map_err(StoreError::Sqlite)?;
                collect_rows(rows)?
            };
            for file_id in file_ids {
                transaction
                    .execute(
                        "DELETE FROM calls
                         WHERE caller_symbol_id IN (SELECT id FROM symbols WHERE file_id = ?1)",
                        params![&file_id],
                    )
                    .map_err(StoreError::Sqlite)?;
                transaction
                    .execute(
                        "DELETE FROM symbol_references
                         WHERE file_id = ?1",
                        params![&file_id],
                    )
                    .map_err(StoreError::Sqlite)?;
                transaction
                    .execute("DELETE FROM tests WHERE file_id = ?1", params![&file_id])
                    .map_err(StoreError::Sqlite)?;
                transaction
                    .execute("DELETE FROM chunks WHERE file_id = ?1", params![&file_id])
                    .map_err(StoreError::Sqlite)?;
                transaction
                    .execute("DELETE FROM symbols WHERE file_id = ?1", params![&file_id])
                    .map_err(StoreError::Sqlite)?;
                transaction
                    .execute("DELETE FROM files WHERE id = ?1", params![&file_id])
                    .map_err(StoreError::Sqlite)?;
            }
        }
        transaction.commit().map_err(StoreError::Sqlite)?;
        Ok(missing.len())
    }

    pub fn vector_point_ids_for_latest_generation_layer_paths(
        &self,
        repository_id: &str,
        semantic_layer: SemanticLayer,
        paths: &[String],
    ) -> Result<Vec<String>> {
        if paths.is_empty() {
            return Ok(Vec::new());
        }
        let Some(generation) = self.latest_semantic_generation(repository_id)? else {
            return Ok(Vec::new());
        };
        let mut point_ids = std::collections::BTreeSet::new();
        let mut statement = self
            .connection
            .prepare(
                "SELECT chunk_embeddings.vector_point_id
                 FROM chunk_embeddings
                 JOIN files ON chunk_embeddings.file_id = files.id
                 WHERE files.repository_id = ?1
                   AND files.path = ?2
                   AND chunk_embeddings.repository_id = ?1
                   AND chunk_embeddings.generation_id = ?3
                   AND chunk_embeddings.semantic_layer = ?4
                   AND chunk_embeddings.vector_store = 'sqlite_vec'
                   AND chunk_embeddings.status = 'current'
                 ORDER BY chunk_embeddings.vector_point_id",
            )
            .map_err(StoreError::Sqlite)?;
        for path in paths {
            let rows = statement
                .query_map(
                    params![repository_id, path, generation.id, semantic_layer.as_str()],
                    |row| row.get::<_, String>(0),
                )
                .map_err(StoreError::Sqlite)?;
            for row in rows {
                point_ids.insert(row.map_err(StoreError::Sqlite)?);
            }
        }
        Ok(point_ids.into_iter().collect())
    }

    pub fn vector_point_ids_for_latest_generation_layer_missing_files(
        &self,
        repository_id: &str,
        semantic_layer: SemanticLayer,
        active_paths: &[String],
    ) -> Result<Vec<String>> {
        let existing = self.file_paths(repository_id)?;
        let active: std::collections::BTreeSet<&str> =
            active_paths.iter().map(String::as_str).collect();
        let missing: Vec<String> = existing
            .into_iter()
            .filter(|path| !active.contains(path.as_str()))
            .collect();
        self.vector_point_ids_for_latest_generation_layer_paths(
            repository_id,
            semantic_layer,
            &missing,
        )
    }

    pub fn vector_point_ids_referenced_by_ref_files(
        &self,
        repository_id: &str,
        vector_table: &str,
    ) -> Result<BTreeSet<String>> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT DISTINCT vector_points.vector_point_id
                   FROM vector_points
                   JOIN ref_files ON ref_files.file_id = vector_points.file_id
                  WHERE vector_points.vector_store = 'sqlite_vec'
                    AND vector_points.repository_id = ?1
                    AND vector_points.vector_table = ?2",
            )
            .map_err(StoreError::Sqlite)?;
        let rows = statement
            .query_map(params![repository_id, vector_table], |row| row.get(0))
            .map_err(StoreError::Sqlite)?;
        collect_rows(rows).map(|rows: Vec<String>| rows.into_iter().collect())
    }

    pub fn expected_vector_points_for_generation_layer(
        &self,
        repository_id: &str,
        generation_id: &str,
        semantic_layer: SemanticLayer,
    ) -> Result<Vec<ExpectedVectorPoint>> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT chunk_embeddings.vector_point_id, chunks.id, files.path,
                        chunks.start_line, chunks.end_line, chunk_embeddings.text_hash,
                        chunk_embeddings.embedding_model, chunk_embeddings.embedding_dimension
                 FROM chunk_embeddings
                 JOIN chunks ON chunk_embeddings.chunk_id = chunks.id
                 JOIN files ON chunk_embeddings.file_id = files.id
                           AND chunks.file_id = files.id
                 WHERE chunk_embeddings.repository_id = ?1
                   AND files.repository_id = ?1
                   AND chunk_embeddings.generation_id = ?2
                   AND chunk_embeddings.semantic_layer = ?3
                   AND chunk_embeddings.vector_store = 'sqlite_vec'
                   AND chunk_embeddings.status = 'current'
                 ORDER BY files.path, chunks.start_line, chunks.id",
            )
            .map_err(StoreError::Sqlite)?;
        let rows = statement
            .query_map(
                params![repository_id, generation_id, semantic_layer.as_str()],
                |row| {
                    Ok(ExpectedVectorPoint {
                        vector_point_id: row.get(0)?,
                        chunk_id: row.get(1)?,
                        path: row.get(2)?,
                        start_line: row.get::<_, i64>(3)? as usize,
                        end_line: row.get::<_, i64>(4)? as usize,
                        text_hash: row.get(5)?,
                        embedding_model: Some(row.get(6)?),
                        embedding_dimension: Some(row.get::<_, i64>(7)? as usize),
                    })
                },
            )
            .map_err(StoreError::Sqlite)?;
        collect_rows(rows)
    }

    pub fn repository_status(&self, repository_id: &str) -> Result<RepositoryStatus> {
        let files_indexed: usize = self
            .connection
            .query_row(
                "SELECT COUNT(*) FROM files WHERE repository_id = ?1",
                params![repository_id],
                |row| row.get::<_, i64>(0),
            )
            .map_err(StoreError::Sqlite)?
            .try_into()
            .map_err(|_| StoreError::UnexpectedResponse("negative file count".to_owned()))?;
        let chunks_indexed: usize = self
            .connection
            .query_row(
                "SELECT COUNT(*)
                 FROM chunks
                 JOIN files ON chunks.file_id = files.id
                 WHERE files.repository_id = ?1",
                params![repository_id],
                |row| row.get::<_, i64>(0),
            )
            .map_err(StoreError::Sqlite)?
            .try_into()
            .map_err(|_| StoreError::UnexpectedResponse("negative chunk count".to_owned()))?;
        let last_indexed_at: Option<String> = self
            .connection
            .query_row(
                "SELECT MAX(indexed_at) FROM files WHERE repository_id = ?1",
                params![repository_id],
                |row| row.get(0),
            )
            .map_err(StoreError::Sqlite)?;
        let embedding = self.latest_embedding_run(repository_id)?;
        let current_ref = self.current_repository_ref(repository_id)?;
        Ok(RepositoryStatus {
            repository_id: repository_id.to_owned(),
            current_ref_id: current_ref.as_ref().map(|reference| reference.id.clone()),
            current_ref_kind: current_ref
                .as_ref()
                .map(|reference| reference.ref_kind.clone()),
            current_ref_name: current_ref
                .as_ref()
                .and_then(|reference| reference.ref_name.clone()),
            current_head_oid: current_ref
                .as_ref()
                .and_then(|reference| reference.head_oid.clone()),
            files_indexed,
            chunks_indexed,
            symbols_indexed: self.count_joined(repository_id, "symbols")?,
            calls_indexed: self.count_calls(repository_id)?,
            last_indexed_at,
            embedding_model: embedding.as_ref().map(|run| run.embedding_model.clone()),
            embedding_dimension: embedding.and_then(|run| run.embedding_dimension),
        })
    }

    pub fn watcher_status(&self, repository_id: &str) -> Result<Option<WatcherStatusRecord>> {
        self.connection
            .query_row(
                "SELECT repository_id, root_path, mode, owner_kind, owner_pid, socket_path,
                        state, started_at, updated_at, heartbeat_at, files_seen, queued_events,
                        last_indexed_path, last_error, active_layer, quality_status,
                        quality_pending_jobs, quality_running_jobs, quality_failed_jobs,
                        quality_stale_jobs
                   FROM watchers
                  WHERE repository_id = ?1",
                params![repository_id],
                watcher_status_record,
            )
            .optional()
            .map_err(StoreError::Sqlite)
    }

    pub fn upsert_watcher_status(&self, status: &WatcherStatusRecord) -> Result<()> {
        let now = timestamp();
        self.connection
            .execute(
                "INSERT INTO watchers (
                   repository_id, root_path, mode, owner_kind, owner_pid, socket_path,
                   state, started_at, updated_at, heartbeat_at, files_seen, queued_events,
                   last_indexed_path, last_error, active_layer, quality_status,
                   quality_pending_jobs, quality_running_jobs, quality_failed_jobs,
                   quality_stale_jobs
                 )
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20)
                 ON CONFLICT(repository_id) DO UPDATE SET
                   root_path = excluded.root_path,
                   mode = excluded.mode,
                   owner_kind = excluded.owner_kind,
                   owner_pid = excluded.owner_pid,
                   socket_path = excluded.socket_path,
                   state = excluded.state,
                   started_at = excluded.started_at,
                   updated_at = excluded.updated_at,
                   heartbeat_at = excluded.heartbeat_at,
                   files_seen = excluded.files_seen,
                   queued_events = excluded.queued_events,
                   last_indexed_path = excluded.last_indexed_path,
                   last_error = excluded.last_error,
                   active_layer = excluded.active_layer,
                   quality_status = excluded.quality_status,
                   quality_pending_jobs = excluded.quality_pending_jobs,
                   quality_running_jobs = excluded.quality_running_jobs,
                   quality_failed_jobs = excluded.quality_failed_jobs,
                   quality_stale_jobs = excluded.quality_stale_jobs",
                params![
                    status.repository_id,
                    status.root_path,
                    status.mode,
                    status.owner_kind,
                    status.owner_pid.map(i64::from),
                    status.socket_path,
                    status.state,
                    status.started_at.as_deref().unwrap_or(&now),
                    now,
                    status.heartbeat_at.as_deref().unwrap_or(&now),
                    status.files_seen as i64,
                    status.queued_events as i64,
                    status.last_indexed_path,
                    status.last_error,
                    status.active_layer,
                    status.quality_status,
                    status.quality_pending_jobs as i64,
                    status.quality_running_jobs as i64,
                    status.quality_failed_jobs as i64,
                    status.quality_stale_jobs as i64,
                ],
            )
            .map_err(StoreError::Sqlite)?;
        Ok(())
    }

    pub fn mark_watcher_stopped(&self, repository_id: &str) -> Result<()> {
        let now = timestamp();
        self.connection
            .execute(
                "UPDATE watchers
                    SET state = 'stopped',
                        updated_at = ?2,
                        heartbeat_at = ?2,
                        owner_pid = NULL,
                        socket_path = NULL,
                        queued_events = 0
                  WHERE repository_id = ?1",
                params![repository_id, now],
            )
            .map_err(StoreError::Sqlite)?;
        Ok(())
    }

    pub fn mark_watcher_failed(&self, repository_id: &str, error: &str) -> Result<()> {
        let now = timestamp();
        self.connection
            .execute(
                "UPDATE watchers
                    SET state = 'failed',
                        updated_at = ?2,
                        heartbeat_at = ?2,
                        last_error = ?3,
                        owner_pid = NULL,
                        socket_path = NULL
                  WHERE repository_id = ?1",
                params![repository_id, now, error],
            )
            .map_err(StoreError::Sqlite)?;
        Ok(())
    }

    pub fn upsert_watcher_client(&self, client: &WatcherClientRecord) -> Result<()> {
        let now = timestamp();
        self.connection
            .execute(
                "INSERT INTO watcher_clients (
                   repository_id, client_id, client_kind, pid, started_at,
                   heartbeat_at, last_seen_at
                 )
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)
                 ON CONFLICT(repository_id, client_id) DO UPDATE SET
                   client_kind = excluded.client_kind,
                   pid = excluded.pid,
                   heartbeat_at = excluded.heartbeat_at,
                   last_seen_at = excluded.last_seen_at",
                params![
                    client.repository_id,
                    client.client_id,
                    client.client_kind,
                    client.pid.map(i64::from),
                    client.started_at.as_deref().unwrap_or(&now),
                    client.heartbeat_at.as_deref().unwrap_or(&now),
                ],
            )
            .map_err(StoreError::Sqlite)?;
        Ok(())
    }

    pub fn heartbeat_watcher_client(&self, repository_id: &str, client_id: &str) -> Result<usize> {
        let now = timestamp();
        let updated = self
            .connection
            .execute(
                "UPDATE watcher_clients
                    SET heartbeat_at = ?3,
                        last_seen_at = ?3
                  WHERE repository_id = ?1 AND client_id = ?2",
                params![repository_id, client_id, now],
            )
            .map_err(StoreError::Sqlite)?;
        Ok(updated)
    }

    pub fn remove_watcher_client(&self, repository_id: &str, client_id: &str) -> Result<()> {
        self.connection
            .execute(
                "DELETE FROM watcher_clients
                  WHERE repository_id = ?1 AND client_id = ?2",
                params![repository_id, client_id],
            )
            .map_err(StoreError::Sqlite)?;
        Ok(())
    }

    pub fn watcher_clients(&self, repository_id: &str) -> Result<Vec<WatcherClientRecord>> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT repository_id, client_id, client_kind, pid, started_at,
                        heartbeat_at, last_seen_at
                   FROM watcher_clients
                  WHERE repository_id = ?1
                  ORDER BY client_kind, started_at, client_id",
            )
            .map_err(StoreError::Sqlite)?;
        let rows = statement
            .query_map(params![repository_id], watcher_client_record)
            .map_err(StoreError::Sqlite)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(StoreError::Sqlite)
    }

    pub fn prune_stale_watcher_clients(
        &self,
        repository_id: &str,
        stale_before_timestamp: &str,
    ) -> Result<usize> {
        let removed = self
            .connection
            .execute(
                "DELETE FROM watcher_clients
                  WHERE repository_id = ?1 AND heartbeat_at < ?2",
                params![repository_id, stale_before_timestamp],
            )
            .map_err(StoreError::Sqlite)?;
        Ok(removed)
    }

    pub fn ensure_embedding_compatible(
        &self,
        repository_id: &str,
        embedding_model: &str,
        embedding_dimension: usize,
    ) -> Result<()> {
        let Some(previous) = self.latest_embedding_run_for_model(repository_id, embedding_model)?
        else {
            return Ok(());
        };
        if let Some(previous_dimension) = previous.embedding_dimension
            && previous_dimension != embedding_dimension
        {
            return Err(StoreError::EmbeddingDimensionChanged {
                repository_id: repository_id.to_owned(),
                embedding_model: embedding_model.to_owned(),
                previous_dimension,
                current_dimension: embedding_dimension,
            });
        }
        Ok(())
    }

    pub fn start_index_run(&self, run: &IndexRunRecord) -> Result<()> {
        let now = timestamp();
        self.connection
            .execute(
                                "INSERT INTO index_runs (
                                     id, repository_id, repository_ref_id, started_at, finished_at, status, embedding_model,
                   embedding_dimension, files_seen, files_indexed, chunks_embedded,
                   error_summary, parser_version, indexer_version, run_kind
                  )
                                    VALUES (?1, ?2, ?3, ?4, NULL, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
                  ON CONFLICT(id) DO UPDATE SET
                                        repository_ref_id = excluded.repository_ref_id,
                    started_at = excluded.started_at,
                    finished_at = NULL,
                    status = excluded.status,
                    embedding_model = excluded.embedding_model,
                    embedding_dimension = excluded.embedding_dimension,
                    files_seen = excluded.files_seen,
                    files_indexed = excluded.files_indexed,
                    chunks_embedded = excluded.chunks_embedded,
                    error_summary = excluded.error_summary,
                    parser_version = excluded.parser_version,
                    indexer_version = excluded.indexer_version,
                    run_kind = excluded.run_kind",
                params![
                    run.id,
                    run.repository_id,
                    run.repository_ref_id,
                    now,
                    run.status,
                    run.embedding_model,
                    run.embedding_dimension.map(|dimension| dimension as i64),
                    run.files_seen as i64,
                    run.files_indexed as i64,
                    run.chunks_embedded as i64,
                    run.error_summary,
                    run.parser_version,
                    run.indexer_version,
                    run.run_kind,
                ],
            )
            .map_err(StoreError::Sqlite)?;
        Ok(())
    }

    pub fn finish_index_run(&self, run: &IndexRunRecord) -> Result<()> {
        let now = timestamp();
        self.connection
            .execute(
                                "INSERT INTO index_runs (
                                     id, repository_id, repository_ref_id, started_at, finished_at, status, embedding_model,
                   embedding_dimension, files_seen, files_indexed, chunks_embedded,
                   error_summary, parser_version, indexer_version, run_kind
                  )
                                    VALUES (?1, ?2, ?3, ?4, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
                  ON CONFLICT(id) DO UPDATE SET
                                        repository_ref_id = excluded.repository_ref_id,
                    finished_at = excluded.finished_at,
                    status = excluded.status,
                    embedding_model = excluded.embedding_model,
                    embedding_dimension = excluded.embedding_dimension,
                    files_seen = excluded.files_seen,
                    files_indexed = excluded.files_indexed,
                    chunks_embedded = excluded.chunks_embedded,
                    error_summary = excluded.error_summary,
                    parser_version = excluded.parser_version,
                    indexer_version = excluded.indexer_version,
                    run_kind = excluded.run_kind",
                params![
                    run.id,
                    run.repository_id,
                    run.repository_ref_id,
                    now,
                    run.status,
                    run.embedding_model,
                    run.embedding_dimension.map(|dimension| dimension as i64),
                    run.files_seen as i64,
                    run.files_indexed as i64,
                    run.chunks_embedded as i64,
                    run.error_summary,
                    run.parser_version,
                    run.indexer_version,
                    run.run_kind,
                ],
            )
            .map_err(StoreError::Sqlite)?;
        Ok(())
    }

    pub fn record_index_run(&self, run: &IndexRunRecord) -> Result<()> {
        self.finish_index_run(run)
    }

    pub fn record_fast_semantic_generation(
        &mut self,
        input: FastSemanticGenerationInput<'_>,
    ) -> Result<SemanticGenerationRecord> {
        let transaction = self.connection.transaction().map_err(StoreError::Sqlite)?;
        let manifest = current_fast_embedding_manifest(
            &transaction,
            input.repository_id,
            input.repository_ref_id,
            input.fast_model,
            input.fast_dimension,
            input.upserted_embeddings,
        )?;
        let generation_id = fast_semantic_generation_id(
            input.repository_id,
            input.fast_model,
            input.fast_dimension,
            &manifest,
        );
        let generation = SemanticGenerationRecord {
            id: generation_id.clone(),
            repository_id: input.repository_id.to_owned(),
            fast_model: input.fast_model.to_owned(),
            fast_dimension: input.fast_dimension,
            fast_completed_at: input.completed_at.to_owned(),
            quality_model: None,
            quality_dimension: None,
            quality_status: SemanticLayerStatus::FastReady.as_str().to_owned(),
            quality_started_at: None,
            quality_completed_at: None,
            active_layer: SemanticLayer::Fast.as_str().to_owned(),
            files_seen: input.files_seen,
            embeddable_chunks: manifest.len(),
            fast_embedded_chunks: manifest.len(),
            quality_embedded_chunks: 0,
            created_at: input.completed_at.to_owned(),
            updated_at: input.completed_at.to_owned(),
        };

        transaction
            .execute(
                "INSERT INTO semantic_generations (
                   id, repository_id, fast_model, fast_dimension, fast_completed_at,
                   quality_model, quality_dimension, quality_status, quality_started_at,
                   quality_completed_at, active_layer, files_seen, embeddable_chunks,
                   fast_embedded_chunks, quality_embedded_chunks, created_at, updated_at
                 )
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)
                 ON CONFLICT(id) DO UPDATE SET
                   repository_id = excluded.repository_id,
                   fast_model = excluded.fast_model,
                   fast_dimension = excluded.fast_dimension,
                   fast_completed_at = excluded.fast_completed_at,
                   quality_model = semantic_generations.quality_model,
                   quality_dimension = semantic_generations.quality_dimension,
                   quality_status = semantic_generations.quality_status,
                   quality_started_at = semantic_generations.quality_started_at,
                   quality_completed_at = semantic_generations.quality_completed_at,
                   active_layer = semantic_generations.active_layer,
                   files_seen = excluded.files_seen,
                   embeddable_chunks = excluded.embeddable_chunks,
                   fast_embedded_chunks = excluded.fast_embedded_chunks,
                   quality_embedded_chunks = semantic_generations.quality_embedded_chunks,
                   updated_at = excluded.updated_at",
                params![
                    generation.id,
                    generation.repository_id,
                    generation.fast_model,
                    generation.fast_dimension as i64,
                    generation.fast_completed_at,
                    generation.quality_model,
                    generation
                        .quality_dimension
                        .map(|dimension| dimension as i64),
                    generation.quality_status,
                    generation.quality_started_at,
                    generation.quality_completed_at,
                    generation.active_layer,
                    generation.files_seen as i64,
                    generation.embeddable_chunks as i64,
                    generation.fast_embedded_chunks as i64,
                    generation.quality_embedded_chunks as i64,
                    generation.created_at,
                    generation.updated_at,
                ],
            )
            .map_err(StoreError::Sqlite)?;

        {
            let mut statement = transaction
                .prepare(
                    "INSERT INTO chunk_embeddings (
                       id, repository_id, file_id, chunk_id, semantic_layer, embedding_model,
                       embedding_dimension, content_hash, text_hash, vector_table,
                       vector_point_id, vector_store, vector_rowid,
                       generation_id, embedded_at, status
                     )
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, 'sqlite_vec', ?12, ?13, ?14, ?15)
                     ON CONFLICT(chunk_id, semantic_layer, embedding_model, embedding_dimension)
                     DO UPDATE SET
                       id = excluded.id,
                       repository_id = excluded.repository_id,
                       file_id = excluded.file_id,
                       content_hash = excluded.content_hash,
                       text_hash = excluded.text_hash,
                       vector_table = excluded.vector_table,
                       vector_point_id = excluded.vector_point_id,
                       vector_store = excluded.vector_store,
                       vector_rowid = excluded.vector_rowid,
                       generation_id = excluded.generation_id,
                       embedded_at = excluded.embedded_at,
                       status = excluded.status",
                )
                .map_err(StoreError::Sqlite)?;
            let semantic_layer = SemanticLayer::Fast.as_str();
            for row in &manifest {
                statement
                    .execute(params![
                        chunk_embedding_id(
                            input.repository_id,
                            &generation_id,
                            &row.chunk_id,
                            semantic_layer,
                            input.fast_model,
                            input.fast_dimension,
                        ),
                        input.repository_id,
                        row.file_id,
                        row.chunk_id,
                        semantic_layer,
                        input.fast_model,
                        input.fast_dimension as i64,
                        row.content_hash,
                        row.text_hash,
                        input.vector_table,
                        row.vector_point_id,
                        vector_rowid(&row.vector_point_id)?,
                        generation_id,
                        input.completed_at,
                        "current",
                    ])
                    .map_err(StoreError::Sqlite)?;
            }
        }

        transaction.commit().map_err(StoreError::Sqlite)?;
        self.latest_semantic_generation(input.repository_id)?
            .filter(|record| record.id == generation_id)
            .ok_or_else(|| {
                StoreError::UnexpectedResponse("recorded generation not found".to_owned())
            })
    }

    pub fn upsert_semantic_generation(&self, generation: &SemanticGenerationRecord) -> Result<()> {
        self.connection
            .execute(
                "INSERT INTO semantic_generations (
                   id, repository_id, fast_model, fast_dimension, fast_completed_at,
                   quality_model, quality_dimension, quality_status, quality_started_at,
                   quality_completed_at, active_layer, files_seen, embeddable_chunks,
                   fast_embedded_chunks, quality_embedded_chunks, created_at, updated_at
                 )
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)
                 ON CONFLICT(id) DO UPDATE SET
                   repository_id = excluded.repository_id,
                   fast_model = excluded.fast_model,
                   fast_dimension = excluded.fast_dimension,
                   fast_completed_at = excluded.fast_completed_at,
                   quality_model = excluded.quality_model,
                   quality_dimension = excluded.quality_dimension,
                   quality_status = excluded.quality_status,
                   quality_started_at = excluded.quality_started_at,
                   quality_completed_at = excluded.quality_completed_at,
                   active_layer = excluded.active_layer,
                   files_seen = excluded.files_seen,
                   embeddable_chunks = excluded.embeddable_chunks,
                   fast_embedded_chunks = excluded.fast_embedded_chunks,
                   quality_embedded_chunks = excluded.quality_embedded_chunks,
                   created_at = excluded.created_at,
                   updated_at = excluded.updated_at",
                params![
                    generation.id,
                    generation.repository_id,
                    generation.fast_model,
                    generation.fast_dimension as i64,
                    generation.fast_completed_at,
                    generation.quality_model,
                    generation
                        .quality_dimension
                        .map(|dimension| dimension as i64),
                    generation.quality_status,
                    generation.quality_started_at,
                    generation.quality_completed_at,
                    generation.active_layer,
                    generation.files_seen as i64,
                    generation.embeddable_chunks as i64,
                    generation.fast_embedded_chunks as i64,
                    generation.quality_embedded_chunks as i64,
                    generation.created_at,
                    generation.updated_at,
                ],
            )
            .map_err(StoreError::Sqlite)?;
        Ok(())
    }

    pub fn latest_semantic_generation(
        &self,
        repository_id: &str,
    ) -> Result<Option<SemanticGenerationRecord>> {
        self.connection
            .query_row(
                "SELECT id, repository_id, fast_model, fast_dimension, fast_completed_at,
                        quality_model, quality_dimension, quality_status, quality_started_at,
                        quality_completed_at, active_layer, files_seen, embeddable_chunks,
                        fast_embedded_chunks, quality_embedded_chunks, created_at, updated_at
                 FROM semantic_generations
                 WHERE repository_id = ?1
                 ORDER BY fast_completed_at DESC, created_at DESC, id DESC
                 LIMIT 1",
                params![repository_id],
                semantic_generation_record,
            )
            .optional()
            .map_err(StoreError::Sqlite)
    }

    pub fn link_semantic_generation_to_ref(
        &mut self,
        repository_id: &str,
        repository_ref_id: &str,
        generation_id: &str,
        linked_at: &str,
    ) -> Result<()> {
        self.connection
            .execute(
                "INSERT INTO semantic_generation_refs (
                   repository_ref_id, repository_id, generation_id, linked_at
                 )
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(repository_ref_id) DO UPDATE SET
                   repository_id = excluded.repository_id,
                   generation_id = excluded.generation_id,
                   linked_at = excluded.linked_at",
                params![repository_ref_id, repository_id, generation_id, linked_at],
            )
            .map_err(StoreError::Sqlite)?;
        Ok(())
    }

    pub fn latest_semantic_generation_for_ref(
        &self,
        repository_id: &str,
        repository_ref_id: &str,
    ) -> Result<Option<SemanticGenerationRecord>> {
        self.connection
            .query_row(
                "SELECT semantic_generations.id, semantic_generations.repository_id,
                        semantic_generations.fast_model, semantic_generations.fast_dimension,
                        semantic_generations.fast_completed_at,
                        semantic_generations.quality_model,
                        semantic_generations.quality_dimension,
                        semantic_generations.quality_status,
                        semantic_generations.quality_started_at,
                        semantic_generations.quality_completed_at,
                        semantic_generations.active_layer,
                        semantic_generations.files_seen,
                        semantic_generations.embeddable_chunks,
                        semantic_generations.fast_embedded_chunks,
                        semantic_generations.quality_embedded_chunks,
                        semantic_generations.created_at,
                        semantic_generations.updated_at
                   FROM semantic_generation_refs
                   JOIN semantic_generations
                     ON semantic_generations.id = semantic_generation_refs.generation_id
                  WHERE semantic_generation_refs.repository_id = ?1
                    AND semantic_generation_refs.repository_ref_id = ?2
                  LIMIT 1",
                params![repository_id, repository_ref_id],
                semantic_generation_record,
            )
            .optional()
            .map_err(StoreError::Sqlite)
    }

    pub fn semantic_routing_summary_for_ref(
        &self,
        repository_id: &str,
        repository_ref_id: &str,
    ) -> Result<Option<SemanticRoutingSummary>> {
        let Some(generation) =
            self.latest_semantic_generation_for_ref(repository_id, repository_ref_id)?
        else {
            return Ok(None);
        };
        self.semantic_routing_summary_for_generation(generation)
    }

    pub fn semantic_routing_summary(
        &self,
        repository_id: &str,
    ) -> Result<Option<SemanticRoutingSummary>> {
        let Some(generation) = self.latest_semantic_generation(repository_id)? else {
            return Ok(None);
        };
        self.semantic_routing_summary_for_generation(generation)
    }

    fn semantic_routing_summary_for_generation(
        &self,
        generation: SemanticGenerationRecord,
    ) -> Result<Option<SemanticRoutingSummary>> {
        let active_layer = SemanticLayer::parse(&generation.active_layer)
            .map_err(|error| StoreError::UnexpectedResponse(error.to_owned()))?;
        let quality_status = SemanticLayerStatus::parse(&generation.quality_status)
            .map_err(|error| StoreError::UnexpectedResponse(error.to_owned()))?;
        let fast = self.semantic_layer_manifest_summary(
            &generation,
            SemanticLayer::Fast,
            &generation.fast_model,
            generation.fast_dimension,
            generation.embeddable_chunks,
        )?;
        let quality_expected_chunks =
            self.quality_expected_chunks(&generation.repository_id, &generation.id)?;

        let quality = match (&generation.quality_model, generation.quality_dimension) {
            (Some(model), Some(dimension)) => Some(self.semantic_layer_manifest_summary(
                &generation,
                SemanticLayer::Quality,
                model,
                dimension,
                quality_expected_chunks,
            )?),
            _ => self
                .semantic_layer_manifest_aggregate(&generation, SemanticLayer::Quality)?
                .map(|aggregate| {
                    SemanticLayerManifestSummary::from_aggregate(
                        SemanticLayer::Quality,
                        quality_expected_chunks,
                        aggregate,
                    )
                })
                .transpose()?,
        };

        Ok(Some(SemanticRoutingSummary {
            repository_id: generation.repository_id,
            generation_id: generation.id,
            active_layer,
            quality_status,
            embeddable_chunks: generation.embeddable_chunks,
            fast_embedded_chunks: generation.fast_embedded_chunks,
            quality_embedded_chunks: generation.quality_embedded_chunks,
            fast,
            quality,
        }))
    }

    fn semantic_layer_manifest_summary(
        &self,
        generation: &SemanticGenerationRecord,
        semantic_layer: SemanticLayer,
        embedding_model: &str,
        embedding_dimension: usize,
        expected_chunks: usize,
    ) -> Result<SemanticLayerManifestSummary> {
        let Some(aggregate) = self.semantic_layer_manifest_aggregate(generation, semantic_layer)?
        else {
            return Ok(SemanticLayerManifestSummary::empty(
                semantic_layer,
                expected_chunks,
                embedding_model.to_owned(),
                embedding_dimension,
                vector_table_name(&generation.repository_id, embedding_model),
            ));
        };

        SemanticLayerManifestSummary::from_aggregate(semantic_layer, expected_chunks, aggregate)
    }

    fn quality_expected_chunks(&self, repository_id: &str, generation_id: &str) -> Result<usize> {
        let progress = self.quality_generation_progress(repository_id, generation_id)?;
        Ok(progress.quality_eligible_chunks)
    }

    fn semantic_layer_manifest_aggregate(
        &self,
        generation: &SemanticGenerationRecord,
        semantic_layer: SemanticLayer,
    ) -> Result<Option<SemanticLayerManifestAggregate>> {
        self.connection
            .query_row(
                "SELECT embedding_model, embedding_dimension, vector_table,
                        SUM(CASE WHEN status = 'current' THEN 1 ELSE 0 END),
                        SUM(CASE WHEN status = 'stale' THEN 1 ELSE 0 END),
                        SUM(CASE WHEN status = 'blocked' THEN 1 ELSE 0 END),
                        SUM(CASE WHEN status = 'failed' THEN 1 ELSE 0 END),
                        SUM(CASE WHEN status NOT IN ('current', 'stale', 'blocked', 'failed') THEN 1 ELSE 0 END),
                        COUNT(*)
                 FROM chunk_embeddings
                 WHERE repository_id = ?1
                   AND generation_id = ?2
                   AND semantic_layer = ?3
                   AND vector_store = 'sqlite_vec'
                 GROUP BY embedding_model, embedding_dimension, vector_table
                 ORDER BY 4 DESC, 9 DESC, embedding_model, vector_table
                 LIMIT 1",
                params![
                    generation.repository_id,
                    generation.id,
                    semantic_layer.as_str()
                ],
                |row| {
                    Ok(SemanticLayerManifestAggregate {
                        embedding_model: row.get(0)?,
                        embedding_dimension: row.get::<_, i64>(1)?,
                        vector_table: row.get(2)?,
                        current_chunks: row.get::<_, i64>(3)?,
                        stale_chunks: row.get::<_, i64>(4)?,
                        blocked_chunks: row.get::<_, i64>(5)?,
                        failed_chunks: row.get::<_, i64>(6)?,
                        other_chunks: row.get::<_, i64>(7)?,
                        total_chunks: row.get::<_, i64>(8)?,
                    })
                },
            )
            .optional()
            .map_err(StoreError::Sqlite)
    }

    pub fn upsert_chunk_embedding(&self, embedding: &ChunkEmbeddingRecord) -> Result<()> {
        self.connection
            .execute(
                "INSERT INTO chunk_embeddings (
                   id, repository_id, file_id, chunk_id, semantic_layer, embedding_model,
                   embedding_dimension, content_hash, text_hash, vector_table,
                   vector_point_id, vector_store, vector_rowid,
                   generation_id, embedded_at, status
                 )
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, 'sqlite_vec', ?12, ?13, ?14, ?15)
                 ON CONFLICT(chunk_id, semantic_layer, embedding_model, embedding_dimension)
                 DO UPDATE SET
                   id = excluded.id,
                   repository_id = excluded.repository_id,
                   file_id = excluded.file_id,
                   content_hash = excluded.content_hash,
                   text_hash = excluded.text_hash,
                   vector_table = excluded.vector_table,
                   vector_point_id = excluded.vector_point_id,
                   vector_store = excluded.vector_store,
                   vector_rowid = excluded.vector_rowid,
                   generation_id = excluded.generation_id,
                   embedded_at = excluded.embedded_at,
                   status = excluded.status",
                params![
                    embedding.id,
                    embedding.repository_id,
                    embedding.file_id,
                    embedding.chunk_id,
                    embedding.semantic_layer,
                    embedding.embedding_model,
                    embedding.embedding_dimension as i64,
                    embedding.content_hash,
                    embedding.text_hash,
                    embedding.vector_table,
                    embedding.vector_point_id,
                    vector_rowid(&embedding.vector_point_id)?,
                    embedding.generation_id,
                    embedding.embedded_at,
                    embedding.status,
                ],
            )
            .map_err(StoreError::Sqlite)?;
        Ok(())
    }

    pub fn chunk_embeddings_for_generation(
        &self,
        repository_id: &str,
        generation_id: &str,
        semantic_layer: &str,
    ) -> Result<Vec<ChunkEmbeddingRecord>> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT id, repository_id, file_id, chunk_id, semantic_layer, embedding_model,
                        embedding_dimension, content_hash, text_hash, vector_table,
                        vector_point_id, generation_id, embedded_at, status
                 FROM chunk_embeddings
                 WHERE repository_id = ?1
                   AND generation_id = ?2
                   AND semantic_layer = ?3
                 ORDER BY chunk_id, embedding_model, embedding_dimension",
            )
            .map_err(StoreError::Sqlite)?;
        let rows = statement
            .query_map(
                params![repository_id, generation_id, semantic_layer],
                chunk_embedding_record,
            )
            .map_err(StoreError::Sqlite)?;
        collect_rows(rows)
    }

    pub fn upsert_quality_embedding_job(&self, job: &QualityEmbeddingJobRecord) -> Result<()> {
        self.connection
            .execute(
                "INSERT INTO quality_embedding_jobs (
                   id, repository_id, generation_id, chunk_id, file_id, path,
                   content_hash, text_hash, status, attempts, error_summary,
                   created_at, updated_at
                 )
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
                 ON CONFLICT(repository_id, generation_id, chunk_id) DO UPDATE SET
                   id = excluded.id,
                   file_id = excluded.file_id,
                   path = excluded.path,
                   content_hash = excluded.content_hash,
                   text_hash = excluded.text_hash,
                   status = excluded.status,
                   attempts = excluded.attempts,
                   error_summary = excluded.error_summary,
                   updated_at = excluded.updated_at",
                params![
                    job.id,
                    job.repository_id,
                    job.generation_id,
                    job.chunk_id,
                    job.file_id,
                    job.path,
                    job.content_hash,
                    job.text_hash,
                    job.status,
                    job.attempts as i64,
                    job.error_summary,
                    job.created_at,
                    job.updated_at,
                ],
            )
            .map_err(StoreError::Sqlite)?;
        Ok(())
    }

    pub fn quality_embedding_job_id(
        repository_id: &str,
        generation_id: &str,
        chunk_id: &str,
    ) -> String {
        stable_id(&[
            "quality-embedding-job",
            repository_id,
            generation_id,
            chunk_id,
        ])
    }

    pub fn chunk_embedding_id(
        repository_id: &str,
        generation_id: &str,
        chunk_id: &str,
        semantic_layer: &str,
        embedding_model: &str,
        embedding_dimension: usize,
    ) -> String {
        chunk_embedding_id(
            repository_id,
            generation_id,
            chunk_id,
            semantic_layer,
            embedding_model,
            embedding_dimension,
        )
    }

    pub fn mark_superseded_quality_jobs_stale(
        &self,
        repository_id: &str,
        current_generation_id: &str,
        updated_at: &str,
    ) -> Result<usize> {
        let updated = self
            .connection
            .execute(
                "UPDATE quality_embedding_jobs
                 SET status = 'skipped_stale', updated_at = ?3
                 WHERE repository_id = ?1
                   AND generation_id != ?2
                   AND status IN ('pending', 'running')",
                params![repository_id, current_generation_id, updated_at],
            )
            .map_err(StoreError::Sqlite)?;
        Ok(updated)
    }

    pub fn queue_quality_embedding_jobs(
        &mut self,
        generation: &SemanticGenerationRecord,
        quality_model: &str,
        jobs: &[QualityEmbeddingJobRecord],
        queued_at: &str,
    ) -> Result<QualityQueueSummary> {
        for job in jobs {
            if job.repository_id != generation.repository_id || job.generation_id != generation.id {
                return Err(StoreError::UnexpectedResponse(
                    "quality job does not belong to generation".to_owned(),
                ));
            }
        }

        let transaction = self.connection.transaction().map_err(StoreError::Sqlite)?;
        let skipped_stale_jobs = mark_superseded_quality_jobs_stale_in_transaction(
            &transaction,
            &generation.repository_id,
            &generation.id,
            queued_at,
        )?;
        upsert_quality_embedding_jobs_in_transaction(&transaction, jobs)?;
        let updated_generation = quality_generation_with_status(
            generation,
            quality_model,
            SemanticLayerStatus::QualityPending,
            queued_at,
        );
        upsert_semantic_generation_in_transaction(&transaction, &updated_generation)?;
        transaction.commit().map_err(StoreError::Sqlite)?;

        Ok(QualityQueueSummary {
            repository_id: generation.repository_id.clone(),
            generation_id: generation.id.clone(),
            quality_model: quality_model.to_owned(),
            quality_status: SemanticLayerStatus::QualityPending,
            queued_jobs: jobs.len(),
            skipped_stale_jobs,
        })
    }

    pub fn carry_forward_quality_embeddings_for_fast_generation(
        &mut self,
        repository_id: &str,
        generation_id: &str,
        quality_model: &str,
    ) -> Result<QualityCarryForwardSummary> {
        use std::collections::BTreeSet;

        let transaction = self.connection.transaction().map_err(StoreError::Sqlite)?;
        let embeddings = {
            let mut statement = transaction
                .prepare(
                    "SELECT fast.file_id, fast.chunk_id, fast.content_hash, fast.text_hash,
                            quality.embedding_dimension, quality.vector_table,
                            quality.vector_point_id, quality.embedded_at
                     FROM chunk_embeddings AS fast
                     JOIN chunk_embeddings AS quality
                       ON quality.repository_id = fast.repository_id
                      AND quality.chunk_id = fast.chunk_id
                      AND quality.semantic_layer = 'quality'
                      AND quality.embedding_model = ?3
                      AND quality.content_hash = fast.content_hash
                      AND quality.text_hash = fast.text_hash
                      AND quality.status = 'current'
                     WHERE fast.repository_id = ?1
                       AND fast.generation_id = ?2
                       AND fast.semantic_layer = 'fast'
                       AND fast.status = 'current'
                     ORDER BY fast.chunk_id",
                )
                .map_err(StoreError::Sqlite)?;
            let rows = statement
                .query_map(
                    params![repository_id, generation_id, quality_model],
                    |row| {
                        let chunk_id = row.get::<_, String>(1)?;
                        let dimension = row.get::<_, i64>(4)? as usize;
                        Ok(ChunkEmbeddingRecord {
                            id: chunk_embedding_id(
                                repository_id,
                                generation_id,
                                &chunk_id,
                                SemanticLayer::Quality.as_str(),
                                quality_model,
                                dimension,
                            ),
                            repository_id: repository_id.to_owned(),
                            file_id: row.get(0)?,
                            chunk_id,
                            semantic_layer: SemanticLayer::Quality.as_str().to_owned(),
                            embedding_model: quality_model.to_owned(),
                            embedding_dimension: dimension,
                            content_hash: row.get(2)?,
                            text_hash: row.get(3)?,
                            vector_table: row.get(5)?,
                            vector_point_id: row.get(6)?,
                            generation_id: generation_id.to_owned(),
                            embedded_at: row.get(7)?,
                            status: "current".to_owned(),
                        })
                    },
                )
                .map_err(StoreError::Sqlite)?;
            collect_rows(rows)?
        };

        let dimensions = embeddings
            .iter()
            .map(|embedding| embedding.embedding_dimension)
            .collect::<BTreeSet<_>>();
        let quality_dimension = if dimensions.len() == 1 {
            dimensions.first().copied()
        } else {
            None
        };
        for embedding in &embeddings {
            upsert_chunk_embedding_in_transaction(&transaction, embedding)?;
        }
        transaction.commit().map_err(StoreError::Sqlite)?;

        Ok(QualityCarryForwardSummary {
            repository_id: repository_id.to_owned(),
            generation_id: generation_id.to_owned(),
            quality_model: quality_model.to_owned(),
            carried_embeddings: embeddings.len(),
            quality_dimension,
        })
    }

    pub fn quality_embedding_jobs_for_fast_generation(
        &self,
        repository_id: &str,
        generation_id: &str,
        quality_model: &str,
        queued_at: &str,
    ) -> Result<Vec<QualityEmbeddingJobRecord>> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT embeddings.file_id, embeddings.chunk_id, files.path,
                        embeddings.content_hash, embeddings.text_hash
                 FROM chunk_embeddings AS embeddings
                 JOIN files
                   ON files.id = embeddings.file_id
                  AND files.repository_id = embeddings.repository_id
                 LEFT JOIN chunk_embeddings AS quality
                   ON quality.repository_id = embeddings.repository_id
                  AND quality.generation_id = embeddings.generation_id
                  AND quality.chunk_id = embeddings.chunk_id
                  AND quality.semantic_layer = 'quality'
                  AND quality.embedding_model = ?3
                  AND quality.content_hash = embeddings.content_hash
                  AND quality.text_hash = embeddings.text_hash
                  AND quality.status = 'current'
                 WHERE embeddings.repository_id = ?1
                   AND embeddings.generation_id = ?2
                   AND embeddings.semantic_layer = 'fast'
                   AND embeddings.status = 'current'
                   AND quality.chunk_id IS NULL
                 ORDER BY files.path, embeddings.chunk_id",
            )
            .map_err(StoreError::Sqlite)?;
        let rows = statement
            .query_map(
                params![repository_id, generation_id, quality_model],
                |row| {
                    let chunk_id = row.get::<_, String>(1)?;
                    Ok(QualityEmbeddingJobRecord {
                        id: Self::quality_embedding_job_id(repository_id, generation_id, &chunk_id),
                        repository_id: repository_id.to_owned(),
                        generation_id: generation_id.to_owned(),
                        file_id: row.get(0)?,
                        chunk_id,
                        path: row.get(2)?,
                        content_hash: row.get(3)?,
                        text_hash: row.get(4)?,
                        status: "pending".to_owned(),
                        attempts: 0,
                        error_summary: None,
                        created_at: queued_at.to_owned(),
                        updated_at: queued_at.to_owned(),
                    })
                },
            )
            .map_err(StoreError::Sqlite)?;
        collect_rows(rows)
    }

    pub fn mark_quality_generation_blocked(
        &mut self,
        generation: &SemanticGenerationRecord,
        quality_model: &str,
        blocked_at: &str,
    ) -> Result<QualityQueueSummary> {
        let transaction = self.connection.transaction().map_err(StoreError::Sqlite)?;
        let skipped_stale_jobs = mark_superseded_quality_jobs_stale_in_transaction(
            &transaction,
            &generation.repository_id,
            &generation.id,
            blocked_at,
        )?;
        let updated_generation = quality_generation_with_status(
            generation,
            quality_model,
            SemanticLayerStatus::QualityBlocked,
            blocked_at,
        );
        upsert_semantic_generation_in_transaction(&transaction, &updated_generation)?;
        transaction.commit().map_err(StoreError::Sqlite)?;

        Ok(QualityQueueSummary {
            repository_id: generation.repository_id.clone(),
            generation_id: generation.id.clone(),
            quality_model: quality_model.to_owned(),
            quality_status: SemanticLayerStatus::QualityBlocked,
            queued_jobs: 0,
            skipped_stale_jobs,
        })
    }

    pub fn quality_jobs_by_status(
        &self,
        repository_id: &str,
        generation_id: &str,
        status: &str,
    ) -> Result<Vec<QualityEmbeddingJobRecord>> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT id, repository_id, generation_id, chunk_id, file_id, path,
                        content_hash, text_hash, status, attempts, error_summary,
                        created_at, updated_at
                 FROM quality_embedding_jobs
                 WHERE repository_id = ?1
                   AND generation_id = ?2
                   AND status = ?3
                 ORDER BY updated_at, id",
            )
            .map_err(StoreError::Sqlite)?;
        let rows = statement
            .query_map(
                params![repository_id, generation_id, status],
                quality_embedding_job_record,
            )
            .map_err(StoreError::Sqlite)?;
        collect_rows(rows)
    }

    pub fn claim_quality_embedding_jobs(
        &mut self,
        repository_id: &str,
        generation_id: &str,
        limit: usize,
        claimed_at: &str,
    ) -> Result<Vec<QualityEmbeddingJobRecord>> {
        if limit == 0 {
            return Err(StoreError::InvalidLimit(limit));
        }

        let transaction = self.connection.transaction().map_err(StoreError::Sqlite)?;
        let ids = {
            let mut statement = transaction
                .prepare(
                    "SELECT id
                     FROM quality_embedding_jobs
                     WHERE repository_id = ?1
                       AND generation_id = ?2
                       AND status = 'pending'
                     ORDER BY updated_at, id
                     LIMIT ?3",
                )
                .map_err(StoreError::Sqlite)?;
            let rows = statement
                .query_map(params![repository_id, generation_id, limit as i64], |row| {
                    row.get::<_, String>(0)
                })
                .map_err(StoreError::Sqlite)?;
            collect_rows(rows)?
        };

        let mut claimed = Vec::with_capacity(ids.len());
        for id in ids {
            transaction
                .execute(
                    "UPDATE quality_embedding_jobs
                     SET status = 'running', attempts = attempts + 1, updated_at = ?2
                     WHERE id = ?1
                       AND status = 'pending'",
                    params![id, claimed_at],
                )
                .map_err(StoreError::Sqlite)?;
            let job = transaction
                .query_row(
                    "SELECT id, repository_id, generation_id, chunk_id, file_id, path,
                            content_hash, text_hash, status, attempts, error_summary,
                            created_at, updated_at
                     FROM quality_embedding_jobs
                     WHERE id = ?1",
                    params![id],
                    quality_embedding_job_record,
                )
                .map_err(StoreError::Sqlite)?;
            claimed.push(job);
        }

        transaction.commit().map_err(StoreError::Sqlite)?;
        Ok(claimed)
    }

    pub fn requeue_current_terminal_quality_embedding_jobs(
        &self,
        repository_id: &str,
        generation_id: &str,
        requeued_at: &str,
    ) -> Result<usize> {
        let requeued = self
            .connection
            .execute(
                "UPDATE quality_embedding_jobs
                    SET status = 'pending',
                        error_summary = NULL,
                        updated_at = ?3
                  WHERE repository_id = ?1
                    AND generation_id = ?2
                    AND (
                        status = 'skipped_stale'
                        OR (
                            status = 'failed'
                            AND error_summary LIKE 'SQLite error: database is locked%'
                        )
                    )
                    AND EXISTS (
                        SELECT 1
                          FROM chunk_embeddings AS fast
                         WHERE fast.repository_id = quality_embedding_jobs.repository_id
                           AND fast.generation_id = quality_embedding_jobs.generation_id
                           AND fast.file_id = quality_embedding_jobs.file_id
                           AND fast.chunk_id = quality_embedding_jobs.chunk_id
                           AND fast.semantic_layer = 'fast'
                           AND fast.status = 'current'
                           AND fast.content_hash = quality_embedding_jobs.content_hash
                           AND fast.text_hash = quality_embedding_jobs.text_hash
                    )
                    AND NOT EXISTS (
                        SELECT 1
                          FROM chunk_embeddings AS quality
                         WHERE quality.repository_id = quality_embedding_jobs.repository_id
                           AND quality.generation_id = quality_embedding_jobs.generation_id
                           AND quality.chunk_id = quality_embedding_jobs.chunk_id
                           AND quality.semantic_layer = 'quality'
                           AND quality.content_hash = quality_embedding_jobs.content_hash
                           AND quality.text_hash = quality_embedding_jobs.text_hash
                           AND quality.status = 'current'
                    )",
                params![repository_id, generation_id, requeued_at],
            )
            .map_err(StoreError::Sqlite)?;
        Ok(requeued)
    }

    pub fn quality_job_source_rows(&self, job_ids: &[String]) -> Result<Vec<QualityJobSourceRow>> {
        let mut rows = Vec::with_capacity(job_ids.len());
        let mut statement = self
            .connection
            .prepare(
                "SELECT jobs.id, jobs.repository_id, jobs.generation_id, jobs.chunk_id,
                        jobs.file_id, jobs.path, jobs.content_hash, jobs.text_hash,
                        jobs.status, jobs.attempts, jobs.error_summary,
                        jobs.created_at, jobs.updated_at,
                        files.id, files.content_hash, files.language,
                        chunks.kind, chunks.text_hash, chunks.start_line, chunks.end_line,
                        chunks.start_byte, chunks.end_byte, chunks.excluded_reason,
                        chunks.symbol_id, symbols.name, chunks.index_run_id,
                        chunks.parser_version
                 FROM quality_embedding_jobs AS jobs
                 LEFT JOIN files
                   ON files.id = jobs.file_id
                  AND files.repository_id = jobs.repository_id
                  AND files.path = jobs.path
                 LEFT JOIN chunks
                   ON chunks.id = jobs.chunk_id
                  AND chunks.file_id = files.id
                 LEFT JOIN symbols
                   ON symbols.id = chunks.symbol_id
                 WHERE jobs.id = ?1",
            )
            .map_err(StoreError::Sqlite)?;

        for id in job_ids {
            if let Some(row) = statement
                .query_row(params![id], quality_job_source_row)
                .optional()
                .map_err(StoreError::Sqlite)?
            {
                rows.push(row);
            }
        }

        Ok(rows)
    }

    pub fn complete_quality_embedding_job(
        &mut self,
        job_id: &str,
        completion: QualityJobCompletion,
        completed_at: &str,
    ) -> Result<()> {
        let transaction = self.connection.transaction().map_err(StoreError::Sqlite)?;
        match completion {
            QualityJobCompletion::Succeeded { embedding } => {
                upsert_chunk_embedding_in_transaction(&transaction, embedding.as_ref())?;
                transaction
                    .execute(
                        "UPDATE quality_embedding_jobs
                         SET status = 'succeeded', error_summary = NULL, updated_at = ?2
                         WHERE id = ?1",
                        params![job_id, completed_at],
                    )
                    .map_err(StoreError::Sqlite)?;
            }
            QualityJobCompletion::Failed { error_summary } => {
                transaction
                    .execute(
                        "UPDATE quality_embedding_jobs
                         SET status = 'failed', error_summary = ?2, updated_at = ?3
                         WHERE id = ?1",
                        params![job_id, error_summary, completed_at],
                    )
                    .map_err(StoreError::Sqlite)?;
            }
            QualityJobCompletion::SkippedStale => {
                transaction
                    .execute(
                        "UPDATE quality_embedding_jobs
                         SET status = 'skipped_stale', error_summary = NULL, updated_at = ?2
                         WHERE id = ?1",
                        params![job_id, completed_at],
                    )
                    .map_err(StoreError::Sqlite)?;
            }
            QualityJobCompletion::SkippedExcluded { reason } => {
                transaction
                    .execute(
                        "UPDATE quality_embedding_jobs
                         SET status = 'skipped_excluded', error_summary = ?2, updated_at = ?3
                         WHERE id = ?1",
                        params![job_id, reason, completed_at],
                    )
                    .map_err(StoreError::Sqlite)?;
            }
        }
        transaction.commit().map_err(StoreError::Sqlite)?;
        Ok(())
    }

    pub fn quality_generation_progress(
        &self,
        repository_id: &str,
        generation_id: &str,
    ) -> Result<QualityGenerationProgress> {
        let embeddable_chunks = self
            .connection
            .query_row(
                "SELECT embeddable_chunks
                 FROM semantic_generations
                 WHERE repository_id = ?1
                   AND id = ?2",
                params![repository_id, generation_id],
                |row| row.get::<_, i64>(0),
            )
            .map_err(StoreError::Sqlite)?;
        let quality_embedded_chunks = self
            .connection
            .query_row(
                "SELECT COUNT(*)
                                     FROM chunk_embeddings AS quality
                                    WHERE quality.repository_id = ?1
                                        AND quality.generation_id = ?2
                                        AND quality.semantic_layer = 'quality'
                                        AND quality.status = 'current'
                                        AND EXISTS (
                                                SELECT 1
                                                    FROM chunk_embeddings AS fast
                                                 WHERE fast.repository_id = quality.repository_id
                                                     AND fast.generation_id = quality.generation_id
                                                     AND fast.file_id = quality.file_id
                                                     AND fast.chunk_id = quality.chunk_id
                                                     AND fast.semantic_layer = 'fast'
                                                     AND fast.status = 'current'
                                                     AND fast.content_hash = quality.content_hash
                                                     AND fast.text_hash = quality.text_hash
                                        )",
                params![repository_id, generation_id],
                |row| row.get::<_, i64>(0),
            )
            .map_err(StoreError::Sqlite)?;
        let embeddable_chunks = usize_count(embeddable_chunks, "embeddable chunk count")?;
        let quality_embedded_chunks =
            usize_count(quality_embedded_chunks, "quality embedded chunk count")?;
        let mut progress = QualityGenerationProgress {
            repository_id: repository_id.to_owned(),
            generation_id: generation_id.to_owned(),
            embeddable_chunks,
            quality_eligible_chunks: embeddable_chunks,
            quality_ineligible_chunks: 0,
            quality_embedded_chunks,
            pending_jobs: 0,
            running_jobs: 0,
            succeeded_jobs: 0,
            failed_jobs: 0,
            skipped_stale_jobs: 0,
            skipped_excluded_jobs: 0,
        };

        let mut statement = self
            .connection
            .prepare(
                "SELECT jobs.status, COUNT(*)
                                     FROM quality_embedding_jobs AS jobs
                                    WHERE jobs.repository_id = ?1
                                        AND jobs.generation_id = ?2
                                        AND EXISTS (
                                                SELECT 1
                                                    FROM chunk_embeddings AS fast
                                                 WHERE fast.repository_id = jobs.repository_id
                                                     AND fast.generation_id = jobs.generation_id
                                                     AND fast.file_id = jobs.file_id
                                                     AND fast.chunk_id = jobs.chunk_id
                                                     AND fast.semantic_layer = 'fast'
                                                     AND fast.status = 'current'
                                                     AND fast.content_hash = jobs.content_hash
                                                     AND fast.text_hash = jobs.text_hash
                                        )
                                    GROUP BY jobs.status",
            )
            .map_err(StoreError::Sqlite)?;
        let rows = statement
            .query_map(params![repository_id, generation_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })
            .map_err(StoreError::Sqlite)?;
        for row in collect_rows(rows)? {
            let count = usize_count(row.1, "quality job status count")?;
            match row.0.as_str() {
                "pending" => progress.pending_jobs = count,
                "running" => progress.running_jobs = count,
                "succeeded" => progress.succeeded_jobs = count,
                "failed" => progress.failed_jobs = count,
                "skipped_stale" => progress.skipped_stale_jobs = count,
                "skipped_excluded" => progress.skipped_excluded_jobs = count,
                _ => {}
            }
        }
        progress.quality_ineligible_chunks = progress.skipped_excluded_jobs;
        progress.quality_eligible_chunks = progress
            .embeddable_chunks
            .saturating_sub(progress.quality_ineligible_chunks);

        Ok(progress)
    }

    pub fn latest_quality_generation_error(
        &self,
        repository_id: &str,
        generation_id: &str,
    ) -> Result<Option<String>> {
        self.connection
            .query_row(
                "SELECT error_summary
                 FROM quality_embedding_jobs
                 WHERE repository_id = ?1
                   AND generation_id = ?2
                   AND error_summary IS NOT NULL
                 ORDER BY updated_at DESC, id DESC
                 LIMIT 1",
                params![repository_id, generation_id],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(StoreError::Sqlite)
    }

    pub fn refresh_quality_generation_progress(
        &mut self,
        repository_id: &str,
        generation_id: &str,
        quality_dimension: Option<usize>,
        updated_at: &str,
    ) -> Result<QualityGenerationProgress> {
        let transaction = self.connection.transaction().map_err(StoreError::Sqlite)?;
        let current_embeddings = transaction
            .query_row(
                "SELECT COUNT(*)
                                     FROM chunk_embeddings AS quality
                                    WHERE quality.repository_id = ?1
                                        AND quality.generation_id = ?2
                                        AND quality.semantic_layer = 'quality'
                                        AND quality.status = 'current'
                                        AND EXISTS (
                                                SELECT 1
                                                    FROM chunk_embeddings AS fast
                                                 WHERE fast.repository_id = quality.repository_id
                                                     AND fast.generation_id = quality.generation_id
                                                     AND fast.file_id = quality.file_id
                                                     AND fast.chunk_id = quality.chunk_id
                                                     AND fast.semantic_layer = 'fast'
                                                     AND fast.status = 'current'
                                                     AND fast.content_hash = quality.content_hash
                                                     AND fast.text_hash = quality.text_hash
                                        )",
                params![repository_id, generation_id],
                |row| row.get::<_, i64>(0),
            )
            .map_err(StoreError::Sqlite)?;
        let pending_or_running = transaction
            .query_row(
                "SELECT COUNT(*)
                 FROM quality_embedding_jobs
                 WHERE repository_id = ?1
                   AND generation_id = ?2
                   AND status IN ('pending', 'running')",
                params![repository_id, generation_id],
                |row| row.get::<_, i64>(0),
            )
            .map_err(StoreError::Sqlite)?;
        let failed = transaction
            .query_row(
                "SELECT COUNT(*)
                 FROM quality_embedding_jobs
                 WHERE repository_id = ?1
                   AND generation_id = ?2
                   AND status = 'failed'",
                params![repository_id, generation_id],
                |row| row.get::<_, i64>(0),
            )
            .map_err(StoreError::Sqlite)?;
        let status_update = if pending_or_running == 0 && failed > 0 {
            Some(SemanticLayerStatus::QualityFailed.as_str())
        } else {
            None
        };

        match (quality_dimension, status_update) {
            (Some(dimension), Some(status)) => {
                transaction
                    .execute(
                        "UPDATE semantic_generations
                         SET quality_dimension = COALESCE(quality_dimension, ?3),
                             quality_started_at = COALESCE(quality_started_at, ?4),
                             quality_embedded_chunks = ?5,
                             quality_status = ?6,
                             active_layer = 'fast',
                             updated_at = ?4
                         WHERE repository_id = ?1
                           AND id = ?2",
                        params![
                            repository_id,
                            generation_id,
                            dimension as i64,
                            updated_at,
                            current_embeddings,
                            status
                        ],
                    )
                    .map_err(StoreError::Sqlite)?;
            }
            (Some(dimension), None) => {
                transaction
                    .execute(
                        "UPDATE semantic_generations
                         SET quality_dimension = COALESCE(quality_dimension, ?3),
                             quality_started_at = COALESCE(quality_started_at, ?4),
                             quality_embedded_chunks = ?5,
                             active_layer = 'fast',
                             updated_at = ?4
                         WHERE repository_id = ?1
                           AND id = ?2",
                        params![
                            repository_id,
                            generation_id,
                            dimension as i64,
                            updated_at,
                            current_embeddings
                        ],
                    )
                    .map_err(StoreError::Sqlite)?;
            }
            (None, Some(status)) => {
                transaction
                    .execute(
                        "UPDATE semantic_generations
                         SET quality_embedded_chunks = ?3,
                             quality_status = ?4,
                             active_layer = 'fast',
                             updated_at = ?5
                         WHERE repository_id = ?1
                           AND id = ?2",
                        params![
                            repository_id,
                            generation_id,
                            current_embeddings,
                            status,
                            updated_at
                        ],
                    )
                    .map_err(StoreError::Sqlite)?;
            }
            (None, None) => {
                transaction
                    .execute(
                        "UPDATE semantic_generations
                         SET quality_embedded_chunks = ?3,
                             active_layer = 'fast',
                             updated_at = ?4
                         WHERE repository_id = ?1
                           AND id = ?2",
                        params![repository_id, generation_id, current_embeddings, updated_at],
                    )
                    .map_err(StoreError::Sqlite)?;
            }
        }

        transaction.commit().map_err(StoreError::Sqlite)?;
        self.quality_generation_progress(repository_id, generation_id)
    }

    pub fn refresh_quality_activation(
        &mut self,
        repository_id: &str,
        generation_id: &str,
        quality_dimension: Option<usize>,
        updated_at: &str,
    ) -> Result<QualityActivationSummary> {
        let transaction = self.connection.transaction().map_err(StoreError::Sqlite)?;
        let generation = transaction
            .query_row(
                "SELECT id, repository_id, fast_model, fast_dimension, fast_completed_at,
                        quality_model, quality_dimension, quality_status, quality_started_at,
                        quality_completed_at, active_layer, files_seen, embeddable_chunks,
                        fast_embedded_chunks, quality_embedded_chunks, created_at, updated_at
                 FROM semantic_generations
                 WHERE repository_id = ?1
                   AND id = ?2",
                params![repository_id, generation_id],
                semantic_generation_record,
            )
            .map_err(StoreError::Sqlite)?;
        let latest_generation_id = transaction
            .query_row(
                "SELECT id
                 FROM semantic_generations
                 WHERE repository_id = ?1
                 ORDER BY fast_completed_at DESC, created_at DESC, id DESC
                 LIMIT 1",
                params![repository_id],
                |row| row.get::<_, String>(0),
            )
            .map_err(StoreError::Sqlite)?;
        let previous_status = SemanticLayerStatus::parse(&generation.quality_status)
            .map_err(|error| StoreError::UnexpectedResponse(error.to_owned()))?;
        let previous_active_layer = SemanticLayer::parse(&generation.active_layer)
            .map_err(|error| StoreError::UnexpectedResponse(error.to_owned()))?;
        let current_embeddings = transaction
            .query_row(
                "SELECT COUNT(*)
                                     FROM chunk_embeddings AS quality
                                    WHERE quality.repository_id = ?1
                                        AND quality.generation_id = ?2
                                        AND quality.semantic_layer = 'quality'
                                        AND quality.status = 'current'
                                        AND EXISTS (
                                                SELECT 1
                                                    FROM chunk_embeddings AS fast
                                                 WHERE fast.repository_id = quality.repository_id
                                                     AND fast.generation_id = quality.generation_id
                                                     AND fast.file_id = quality.file_id
                                                     AND fast.chunk_id = quality.chunk_id
                                                     AND fast.semantic_layer = 'fast'
                                                     AND fast.status = 'current'
                                                     AND fast.content_hash = quality.content_hash
                                                     AND fast.text_hash = quality.text_hash
                                        )",
                params![repository_id, generation_id],
                |row| row.get::<_, i64>(0),
            )
            .map_err(StoreError::Sqlite)?;
        let counts =
            quality_job_status_counts_in_transaction(&transaction, repository_id, generation_id)?;
        let quality_embedded_chunks =
            usize_count(current_embeddings, "quality embedded chunk count")?;
        let pending_or_running = counts.pending_jobs + counts.running_jobs;
        let effective_dimension = generation.quality_dimension.or(quality_dimension);
        let is_latest = generation.id == latest_generation_id;

        let (quality_status, active_layer, reason, completed_at) = if !is_latest {
            (
                previous_status,
                previous_active_layer,
                QualityActivationReason::GenerationNotLatest,
                generation.quality_completed_at.clone(),
            )
        } else if generation.quality_model.is_none() {
            (
                previous_status,
                SemanticLayer::Fast,
                QualityActivationReason::QualityModelMissing,
                None,
            )
        } else if effective_dimension.is_none() {
            (
                previous_status,
                SemanticLayer::Fast,
                QualityActivationReason::QualityDimensionMissing,
                None,
            )
        } else if generation.embeddable_chunks == 0 {
            (
                previous_status,
                SemanticLayer::Fast,
                QualityActivationReason::NoEmbeddableChunks,
                None,
            )
        } else if pending_or_running > 0 {
            (
                SemanticLayerStatus::QualityPending,
                SemanticLayer::Fast,
                QualityActivationReason::QualityJobsPending,
                None,
            )
        } else if counts.failed_jobs > 0 {
            (
                SemanticLayerStatus::QualityFailed,
                SemanticLayer::Fast,
                QualityActivationReason::QualityJobsFailed,
                None,
            )
        } else if counts.skipped_stale_jobs > 0 {
            (
                SemanticLayerStatus::QualityPending,
                SemanticLayer::Fast,
                QualityActivationReason::QualityJobsStale,
                None,
            )
        } else if quality_embedded_chunks + counts.skipped_excluded_jobs
            == generation.embeddable_chunks
        {
            (
                SemanticLayerStatus::QualityReady,
                SemanticLayer::Quality,
                QualityActivationReason::QualityComplete,
                Some(updated_at.to_owned()),
            )
        } else if previous_status == SemanticLayerStatus::QualityBlocked {
            (
                previous_status,
                SemanticLayer::Fast,
                QualityActivationReason::QualityBlocked,
                None,
            )
        } else if previous_status == SemanticLayerStatus::QualityStale {
            (
                previous_status,
                SemanticLayer::Fast,
                QualityActivationReason::QualityStale,
                None,
            )
        } else {
            (
                SemanticLayerStatus::QualityPending,
                SemanticLayer::Fast,
                QualityActivationReason::QualityCoverageIncomplete,
                None,
            )
        };

        if is_latest {
            transaction
                .execute(
                    "UPDATE semantic_generations
                     SET quality_dimension = COALESCE(quality_dimension, ?3),
                         quality_started_at = CASE
                           WHEN ?3 IS NOT NULL THEN COALESCE(quality_started_at, ?4)
                           ELSE quality_started_at
                         END,
                         quality_embedded_chunks = ?5,
                         quality_status = ?6,
                         quality_completed_at = ?7,
                         active_layer = ?8,
                         updated_at = ?4
                     WHERE repository_id = ?1
                       AND id = ?2",
                    params![
                        repository_id,
                        generation_id,
                        effective_dimension.map(|dimension| dimension as i64),
                        updated_at,
                        quality_embedded_chunks as i64,
                        quality_status.as_str(),
                        completed_at,
                        active_layer.as_str(),
                    ],
                )
                .map_err(StoreError::Sqlite)?;
        }

        let progress = QualityGenerationProgress {
            repository_id: repository_id.to_owned(),
            generation_id: generation_id.to_owned(),
            embeddable_chunks: generation.embeddable_chunks,
            quality_eligible_chunks: generation
                .embeddable_chunks
                .saturating_sub(counts.skipped_excluded_jobs),
            quality_ineligible_chunks: counts.skipped_excluded_jobs,
            quality_embedded_chunks,
            pending_jobs: counts.pending_jobs,
            running_jobs: counts.running_jobs,
            succeeded_jobs: counts.succeeded_jobs,
            failed_jobs: counts.failed_jobs,
            skipped_stale_jobs: counts.skipped_stale_jobs,
            skipped_excluded_jobs: counts.skipped_excluded_jobs,
        };
        transaction.commit().map_err(StoreError::Sqlite)?;

        Ok(QualityActivationSummary {
            repository_id: repository_id.to_owned(),
            generation_id: generation_id.to_owned(),
            previous_status,
            quality_status,
            active_layer,
            reason,
            progress,
        })
    }

    pub fn new_index_run_id(repository_id: &str, run_kind: &str) -> String {
        format!("{repository_id}-{run_kind}-{}", timestamp_nanos())
    }

    pub fn find_symbols(&self, repository_id: &str, query: &str) -> Result<Vec<SymbolSearchRow>> {
        let like = format!("%{query}%");
        let mut statement = self
            .connection
            .prepare(
                "SELECT symbols.id, symbols.name, symbols.qualified_name, symbols.kind,
                        files.path, symbols.start_line, symbols.end_line,
                        files.content_hash, symbols.index_run_id, symbols.parser_version,
                        files.indexed_at
                 FROM symbols
                 JOIN files ON symbols.file_id = files.id
                 WHERE files.repository_id = ?1
                   AND (symbols.name = ?2 OR symbols.qualified_name = ?2
                        OR symbols.name LIKE ?3 OR symbols.qualified_name LIKE ?3)
                 ORDER BY
                   CASE
                     WHEN symbols.qualified_name = ?2 THEN 0
                     WHEN symbols.name = ?2 THEN 1
                     ELSE 2
                   END,
                   files.path,
                   symbols.start_line
                 LIMIT 25",
            )
            .map_err(StoreError::Sqlite)?;
        let rows = statement
            .query_map(params![repository_id, query, like], |row| {
                Ok(SymbolSearchRow {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    qualified_name: row.get(2)?,
                    kind: row.get(3)?,
                    path: row.get(4)?,
                    start_line: row.get::<_, i64>(5)? as usize,
                    end_line: row.get::<_, i64>(6)? as usize,
                    provenance: EvidenceProvenance {
                        content_hash: row.get(7)?,
                        index_run_id: row.get(8)?,
                        parser_version: row.get(9)?,
                        indexed_at: row.get(10)?,
                        embedding_model: None,
                        embedding_dimension: None,
                        embedded_at: None,
                    },
                })
            })
            .map_err(StoreError::Sqlite)?;
        collect_rows(rows)
    }

    pub fn find_symbols_for_ref(
        &self,
        repository_id: &str,
        repository_ref_id: &str,
        query: &str,
    ) -> Result<Vec<SymbolSearchRow>> {
        let like = format!("%{query}%");
        let mut statement = self
            .connection
            .prepare(
                "SELECT symbols.id, symbols.name, symbols.qualified_name, symbols.kind,
                        files.path, symbols.start_line, symbols.end_line,
                        files.content_hash, symbols.index_run_id, symbols.parser_version,
                        files.indexed_at
                 FROM symbols
                 JOIN files ON symbols.file_id = files.id
                 JOIN ref_files ON ref_files.file_id = files.id
                 WHERE files.repository_id = ?1
                   AND ref_files.repository_ref_id = ?2
                   AND (symbols.name = ?3 OR symbols.qualified_name = ?3
                        OR symbols.name LIKE ?4 OR symbols.qualified_name LIKE ?4)
                 ORDER BY
                   CASE
                     WHEN symbols.qualified_name = ?3 THEN 0
                     WHEN symbols.name = ?3 THEN 1
                     ELSE 2
                   END,
                   files.path,
                   symbols.start_line
                 LIMIT 25",
            )
            .map_err(StoreError::Sqlite)?;
        let rows = statement
            .query_map(
                params![repository_id, repository_ref_id, query, like],
                |row| {
                    Ok(SymbolSearchRow {
                        id: row.get(0)?,
                        name: row.get(1)?,
                        qualified_name: row.get(2)?,
                        kind: row.get(3)?,
                        path: row.get(4)?,
                        start_line: row.get::<_, i64>(5)? as usize,
                        end_line: row.get::<_, i64>(6)? as usize,
                        provenance: EvidenceProvenance {
                            content_hash: row.get(7)?,
                            index_run_id: row.get(8)?,
                            parser_version: row.get(9)?,
                            indexed_at: row.get(10)?,
                            embedding_model: None,
                            embedding_dimension: None,
                            embedded_at: None,
                        },
                    })
                },
            )
            .map_err(StoreError::Sqlite)?;
        collect_rows(rows)
    }

    pub fn repository_has_ref_file_manifests(&self, repository_id: &str) -> Result<bool> {
        let count: i64 = self
            .connection
            .query_row(
                "SELECT COUNT(*) FROM ref_files WHERE repository_id = ?1",
                params![repository_id],
                |row| row.get(0),
            )
            .map_err(StoreError::Sqlite)?;
        Ok(count > 0)
    }

    pub fn file_provenance(
        &self,
        repository_id: &str,
        path: &str,
    ) -> Result<Option<EvidenceProvenance>> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT content_hash, index_run_id, parser_version, indexed_at
                   FROM files
                  WHERE repository_id = ?1 AND path = ?2
                  LIMIT 1",
            )
            .map_err(StoreError::Sqlite)?;
        let mut rows = statement
            .query_map(params![repository_id, path], |row| {
                Ok(EvidenceProvenance {
                    content_hash: row.get(0)?,
                    index_run_id: row.get(1)?,
                    parser_version: row.get(2)?,
                    indexed_at: row.get(3)?,
                    embedding_model: None,
                    embedding_dimension: None,
                    embedded_at: None,
                })
            })
            .map_err(StoreError::Sqlite)?;
        rows.next().transpose().map_err(StoreError::Sqlite)
    }

    pub fn symbols_at_location(
        &self,
        repository_id: &str,
        path: &str,
        line: usize,
    ) -> Result<Vec<SymbolSearchRow>> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT symbols.id, symbols.name, symbols.qualified_name, symbols.kind,
                        files.path, symbols.start_line, symbols.end_line,
                        files.content_hash, symbols.index_run_id, symbols.parser_version,
                        files.indexed_at
                   FROM symbols
                   JOIN files ON symbols.file_id = files.id
                  WHERE files.repository_id = ?1
                    AND files.path = ?2
                    AND symbols.start_line <= ?3
                    AND symbols.end_line >= ?3
                  ORDER BY (symbols.end_line - symbols.start_line), symbols.start_line, symbols.qualified_name
                  LIMIT 10",
            )
            .map_err(StoreError::Sqlite)?;
        let rows = statement
            .query_map(params![repository_id, path, line as i64], |row| {
                Ok(SymbolSearchRow {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    qualified_name: row.get(2)?,
                    kind: row.get(3)?,
                    path: row.get(4)?,
                    start_line: row.get::<_, i64>(5)? as usize,
                    end_line: row.get::<_, i64>(6)? as usize,
                    provenance: EvidenceProvenance {
                        content_hash: row.get(7)?,
                        index_run_id: row.get(8)?,
                        parser_version: row.get(9)?,
                        indexed_at: row.get(10)?,
                        embedding_model: None,
                        embedding_dimension: None,
                        embedded_at: None,
                    },
                })
            })
            .map_err(StoreError::Sqlite)?;
        collect_rows(rows)
    }

    pub fn calls_at_location(
        &self,
        repository_id: &str,
        path: &str,
        line: usize,
    ) -> Result<Vec<CallPathEdge>> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT calls.id, calls.callee_text, calls.call_line, calls.confidence,
                        calls.resolution_status,
                        caller.id, caller.name, caller.qualified_name, caller.kind,
                        caller_files.path, caller.start_line, caller.end_line,
                        callee.id, callee.name, callee.qualified_name, callee.kind,
                        callee_files.path, callee.start_line, callee.end_line,
                        caller_files.content_hash, calls.index_run_id, calls.parser_version,
                        caller_files.indexed_at
                   FROM calls
                   JOIN symbols caller ON calls.caller_symbol_id = caller.id
                   JOIN files caller_files ON caller.file_id = caller_files.id
                   LEFT JOIN symbols callee ON calls.callee_symbol_id = callee.id
                   LEFT JOIN files callee_files ON callee.file_id = callee_files.id
                  WHERE caller_files.repository_id = ?1
                    AND caller_files.path = ?2
                    AND calls.call_line = ?3
                  ORDER BY caller.qualified_name, calls.callee_text, calls.id
                  LIMIT 25",
            )
            .map_err(StoreError::Sqlite)?;
        let rows = statement
            .query_map(params![repository_id, path, line as i64], call_path_edge)
            .map_err(StoreError::Sqlite)?;
        collect_rows(rows)
    }

    pub fn callers(&self, repository_id: &str, symbol_query: &str) -> Result<Vec<CallSearchRow>> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT calls.callee_text, calls.call_line, calls.confidence, calls.resolution_status,
                        caller.id, caller.name, caller.qualified_name, caller.kind,
                        files.path, caller.start_line, caller.end_line,
                        files.content_hash, calls.index_run_id, calls.parser_version,
                        files.indexed_at
                  FROM calls
                  JOIN symbols target ON calls.callee_symbol_id = target.id
                  JOIN symbols caller ON calls.caller_symbol_id = caller.id
                 JOIN files ON caller.file_id = files.id
                 WHERE files.repository_id = ?1
                   AND (target.id = ?2 OR target.name = ?2 OR target.qualified_name = ?2)
                 ORDER BY files.path, calls.call_line
                 LIMIT 50",
            )
            .map_err(StoreError::Sqlite)?;
        let rows = statement
            .query_map(params![repository_id, symbol_query], call_search_row)
            .map_err(StoreError::Sqlite)?;
        collect_rows(rows)
    }

    pub fn callers_for_ref(
        &self,
        repository_id: &str,
        repository_ref_id: &str,
        symbol_query: &str,
    ) -> Result<Vec<CallSearchRow>> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT calls.callee_text, calls.call_line, calls.confidence, calls.resolution_status,
                        caller.id, caller.name, caller.qualified_name, caller.kind,
                        caller_files.path, caller.start_line, caller.end_line,
                        caller_files.content_hash, calls.index_run_id, calls.parser_version,
                        caller_files.indexed_at
                  FROM calls
                  JOIN symbols target ON calls.callee_symbol_id = target.id
                  JOIN files target_files ON target.file_id = target_files.id
                  JOIN ref_files target_ref ON target_ref.file_id = target_files.id
                  JOIN symbols caller ON calls.caller_symbol_id = caller.id
                  JOIN files caller_files ON caller.file_id = caller_files.id
                  JOIN ref_files caller_ref ON caller_ref.file_id = caller_files.id
                 WHERE caller_files.repository_id = ?1
                   AND target_ref.repository_ref_id = ?2
                   AND caller_ref.repository_ref_id = ?2
                   AND (target.id = ?3 OR target.name = ?3 OR target.qualified_name = ?3)
                 ORDER BY caller_files.path, calls.call_line
                 LIMIT 50",
            )
            .map_err(StoreError::Sqlite)?;
        let rows = statement
            .query_map(
                params![repository_id, repository_ref_id, symbol_query],
                call_search_row,
            )
            .map_err(StoreError::Sqlite)?;
        collect_rows(rows)
    }

    pub fn tests_matching_name(
        &self,
        repository_id: &str,
        test_name: &str,
    ) -> Result<Vec<TestSearchRow>> {
        let suffix = format!("%::{test_name}");
        let mut statement = self
            .connection
            .prepare(
                "SELECT tests.id, tests.name, tests.qualified_name, tests.framework,
                        tests.language, tests.path, tests.start_line, tests.end_line,
                        files.content_hash, tests.index_run_id, tests.parser_version, tests.indexed_at
                   FROM tests
                   JOIN files ON tests.file_id = files.id
                  WHERE tests.repository_id = ?1
                    AND (tests.name = ?2 OR tests.qualified_name = ?2 OR tests.qualified_name LIKE ?3)
                  ORDER BY
                    CASE
                      WHEN tests.qualified_name = ?2 THEN 0
                      WHEN tests.name = ?2 THEN 1
                      ELSE 2
                    END,
                    tests.path,
                    tests.start_line
                  LIMIT 25",
            )
            .map_err(StoreError::Sqlite)?;
        let rows = statement
            .query_map(params![repository_id, test_name, suffix], test_search_row)
            .map_err(StoreError::Sqlite)?;
        collect_rows(rows)
    }

    pub fn likely_tests_for_symbol(
        &self,
        repository_id: &str,
        symbol_query: &str,
    ) -> Result<Vec<TestSearchRow>> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT DISTINCT tests.id, tests.name, tests.qualified_name, tests.framework,
                        tests.language, tests.path, tests.start_line, tests.end_line,
                        files.content_hash, tests.index_run_id, tests.parser_version, tests.indexed_at
                   FROM tests
                   JOIN files ON tests.file_id = files.id
                   LEFT JOIN calls ON calls.caller_symbol_id = tests.symbol_id
                   LEFT JOIN symbols target ON calls.callee_symbol_id = target.id
                  WHERE tests.repository_id = ?1
                    AND (
                      tests.id = ?2 OR tests.name = ?2 OR tests.qualified_name = ?2
                      OR target.id = ?2 OR target.name = ?2 OR target.qualified_name = ?2
                    )
                  ORDER BY tests.path, tests.start_line, tests.qualified_name
                  LIMIT 25",
            )
            .map_err(StoreError::Sqlite)?;
        let rows = statement
            .query_map(params![repository_id, symbol_query], test_search_row)
            .map_err(StoreError::Sqlite)?;
        collect_rows(rows)
    }

    pub fn callees(&self, repository_id: &str, symbol_query: &str) -> Result<Vec<CallSearchRow>> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT calls.callee_text, calls.call_line, calls.confidence, calls.resolution_status,
                        callee.id, callee.name, callee.qualified_name, callee.kind,
                        files.path, callee.start_line, callee.end_line,
                        files.content_hash, calls.index_run_id, calls.parser_version,
                        files.indexed_at
                  FROM calls
                  JOIN symbols caller ON calls.caller_symbol_id = caller.id
                  LEFT JOIN symbols callee ON calls.callee_symbol_id = callee.id
                 JOIN files ON caller.file_id = files.id
                 WHERE files.repository_id = ?1
                   AND (caller.id = ?2 OR caller.name = ?2 OR caller.qualified_name = ?2)
                 ORDER BY calls.call_line, calls.callee_text
                 LIMIT 50",
            )
            .map_err(StoreError::Sqlite)?;
        let rows = statement
            .query_map(params![repository_id, symbol_query], call_search_row)
            .map_err(StoreError::Sqlite)?;
        collect_rows(rows)
    }

    pub fn callees_for_ref(
        &self,
        repository_id: &str,
        repository_ref_id: &str,
        symbol_query: &str,
    ) -> Result<Vec<CallSearchRow>> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT calls.callee_text, calls.call_line, calls.confidence, calls.resolution_status,
                        callee.id, callee.name, callee.qualified_name, callee.kind,
                        caller_files.path, callee.start_line, callee.end_line,
                        caller_files.content_hash, calls.index_run_id, calls.parser_version,
                        caller_files.indexed_at
                  FROM calls
                  JOIN symbols caller ON calls.caller_symbol_id = caller.id
                  JOIN files caller_files ON caller.file_id = caller_files.id
                  JOIN ref_files caller_ref ON caller_ref.file_id = caller_files.id
                  LEFT JOIN symbols callee ON calls.callee_symbol_id = callee.id
                  LEFT JOIN files callee_files ON callee.file_id = callee_files.id
                  LEFT JOIN ref_files callee_ref
                    ON callee_ref.file_id = callee_files.id
                   AND callee_ref.repository_ref_id = ?2
                 WHERE caller_files.repository_id = ?1
                   AND caller_ref.repository_ref_id = ?2
                   AND (callee.id IS NULL OR callee_ref.repository_ref_id IS NOT NULL)
                   AND (caller.id = ?3 OR caller.name = ?3 OR caller.qualified_name = ?3)
                 ORDER BY calls.call_line, calls.callee_text
                 LIMIT 50",
            )
            .map_err(StoreError::Sqlite)?;
        let rows = statement
            .query_map(
                params![repository_id, repository_ref_id, symbol_query],
                call_search_row,
            )
            .map_err(StoreError::Sqlite)?;
        collect_rows(rows)
    }

    pub fn call_paths(
        &self,
        repository_id: &str,
        source_query: &str,
        target_query: &str,
        max_depth: usize,
    ) -> Result<Vec<CallPath>> {
        let max_depth = clamp_call_path_depth(max_depth);
        let sources = self.resolve_symbol_refs(repository_id, source_query)?;
        let targets = self.resolve_symbol_refs(repository_id, target_query)?;
        let target_ids = targets
            .iter()
            .map(|symbol| symbol.id.clone())
            .collect::<std::collections::BTreeSet<_>>();
        let edges = self.call_path_edges(repository_id)?;
        let mut edges_by_caller = std::collections::BTreeMap::<String, Vec<CallPathEdge>>::new();
        for edge in edges {
            edges_by_caller
                .entry(edge.caller_symbol_id.clone())
                .or_default()
                .push(edge);
        }

        let mut paths = Vec::new();
        for source in sources {
            let mut visited = std::collections::BTreeSet::from([source.id.clone()]);
            let mut stack = Vec::new();
            trace_call_paths(
                &source.id,
                max_depth,
                &TraceContext {
                    target_query,
                    target_ids: &target_ids,
                    edges_by_caller: &edges_by_caller,
                },
                &mut visited,
                &mut stack,
                &mut paths,
            );
            if paths.len() >= 50 {
                break;
            }
        }
        paths.truncate(50);
        Ok(paths)
    }

    pub fn call_paths_for_ref(
        &self,
        repository_id: &str,
        repository_ref_id: &str,
        source_query: &str,
        target_query: &str,
        max_depth: usize,
    ) -> Result<Vec<CallPath>> {
        let max_depth = clamp_call_path_depth(max_depth);
        let sources =
            self.resolve_symbol_refs_for_ref(repository_id, repository_ref_id, source_query)?;
        let targets =
            self.resolve_symbol_refs_for_ref(repository_id, repository_ref_id, target_query)?;
        let target_ids = targets
            .iter()
            .map(|symbol| symbol.id.clone())
            .collect::<std::collections::BTreeSet<_>>();
        let edges = self.call_path_edges_for_ref(repository_id, repository_ref_id)?;
        let mut edges_by_caller = std::collections::BTreeMap::<String, Vec<CallPathEdge>>::new();
        for edge in edges {
            edges_by_caller
                .entry(edge.caller_symbol_id.clone())
                .or_default()
                .push(edge);
        }

        let mut paths = Vec::new();
        for source in sources {
            let mut visited = std::collections::BTreeSet::from([source.id.clone()]);
            let mut stack = Vec::new();
            trace_call_paths(
                &source.id,
                max_depth,
                &TraceContext {
                    target_query,
                    target_ids: &target_ids,
                    edges_by_caller: &edges_by_caller,
                },
                &mut visited,
                &mut stack,
                &mut paths,
            );
            if paths.len() >= 50 {
                break;
            }
        }
        paths.truncate(50);
        Ok(paths)
    }

    pub fn transitive_call_paths_to(
        &self,
        repository_id: &str,
        target_query: &str,
        max_depth: usize,
    ) -> Result<Vec<CallPath>> {
        let max_depth = clamp_call_path_depth(max_depth);
        let targets = self.resolve_symbol_refs(repository_id, target_query)?;
        let target_ids = targets
            .iter()
            .map(|symbol| symbol.id.clone())
            .collect::<std::collections::BTreeSet<_>>();
        let sources = self.all_symbol_refs(repository_id)?;
        let edges = self.call_path_edges(repository_id)?;
        let mut edges_by_caller = std::collections::BTreeMap::<String, Vec<CallPathEdge>>::new();
        for edge in edges {
            edges_by_caller
                .entry(edge.caller_symbol_id.clone())
                .or_default()
                .push(edge);
        }

        let mut paths = Vec::new();
        for source in sources {
            if target_ids.contains(&source.id) {
                continue;
            }
            let mut visited = std::collections::BTreeSet::from([source.id.clone()]);
            let mut stack = Vec::new();
            let mut source_paths = Vec::new();
            trace_call_paths(
                &source.id,
                max_depth,
                &TraceContext {
                    target_query,
                    target_ids: &target_ids,
                    edges_by_caller: &edges_by_caller,
                },
                &mut visited,
                &mut stack,
                &mut source_paths,
            );
            paths.extend(source_paths.into_iter().filter(|path| path.hops > 1));
            if paths.len() >= 50 {
                break;
            }
        }
        paths.truncate(50);
        Ok(paths)
    }

    pub fn transitive_call_paths_to_for_ref(
        &self,
        repository_id: &str,
        repository_ref_id: &str,
        target_query: &str,
        max_depth: usize,
    ) -> Result<Vec<CallPath>> {
        let max_depth = clamp_call_path_depth(max_depth);
        let targets =
            self.resolve_symbol_refs_for_ref(repository_id, repository_ref_id, target_query)?;
        let target_ids = targets
            .iter()
            .map(|symbol| symbol.id.clone())
            .collect::<std::collections::BTreeSet<_>>();
        let sources = self.all_symbol_refs_for_ref(repository_id, repository_ref_id)?;
        let edges = self.call_path_edges_for_ref(repository_id, repository_ref_id)?;
        let mut edges_by_caller = std::collections::BTreeMap::<String, Vec<CallPathEdge>>::new();
        for edge in edges {
            edges_by_caller
                .entry(edge.caller_symbol_id.clone())
                .or_default()
                .push(edge);
        }

        let mut paths = Vec::new();
        for source in sources {
            if target_ids.contains(&source.id) {
                continue;
            }
            let mut visited = std::collections::BTreeSet::from([source.id.clone()]);
            let mut stack = Vec::new();
            let mut source_paths = Vec::new();
            trace_call_paths(
                &source.id,
                max_depth,
                &TraceContext {
                    target_query,
                    target_ids: &target_ids,
                    edges_by_caller: &edges_by_caller,
                },
                &mut visited,
                &mut stack,
                &mut source_paths,
            );
            paths.extend(source_paths.into_iter().filter(|path| path.hops > 1));
            if paths.len() >= 50 {
                break;
            }
        }
        paths.truncate(50);
        Ok(paths)
    }

    pub fn transitive_call_paths_from(
        &self,
        repository_id: &str,
        source_query: &str,
        max_depth: usize,
    ) -> Result<Vec<CallPath>> {
        let max_depth = clamp_call_path_depth(max_depth);
        let sources = self.resolve_symbol_refs(repository_id, source_query)?;
        let edges = self.call_path_edges(repository_id)?;
        let mut edges_by_caller = std::collections::BTreeMap::<String, Vec<CallPathEdge>>::new();
        for edge in edges {
            edges_by_caller
                .entry(edge.caller_symbol_id.clone())
                .or_default()
                .push(edge);
        }

        let mut paths = Vec::new();
        for source in sources {
            let mut visited = std::collections::BTreeSet::from([source.id.clone()]);
            let mut stack = Vec::new();
            trace_reachable_call_paths(
                &source.id,
                max_depth,
                &edges_by_caller,
                &mut visited,
                &mut stack,
                &mut paths,
            );
            if paths.len() >= 50 {
                break;
            }
        }
        paths.truncate(50);
        Ok(paths)
    }

    pub fn transitive_call_paths_from_for_ref(
        &self,
        repository_id: &str,
        repository_ref_id: &str,
        source_query: &str,
        max_depth: usize,
    ) -> Result<Vec<CallPath>> {
        let max_depth = clamp_call_path_depth(max_depth);
        let sources =
            self.resolve_symbol_refs_for_ref(repository_id, repository_ref_id, source_query)?;
        let edges = self.call_path_edges_for_ref(repository_id, repository_ref_id)?;
        let mut edges_by_caller = std::collections::BTreeMap::<String, Vec<CallPathEdge>>::new();
        for edge in edges {
            edges_by_caller
                .entry(edge.caller_symbol_id.clone())
                .or_default()
                .push(edge);
        }

        let mut paths = Vec::new();
        for source in sources {
            let mut visited = std::collections::BTreeSet::from([source.id.clone()]);
            let mut stack = Vec::new();
            trace_reachable_call_paths(
                &source.id,
                max_depth,
                &edges_by_caller,
                &mut visited,
                &mut stack,
                &mut paths,
            );
            if paths.len() >= 50 {
                break;
            }
        }
        paths.truncate(50);
        Ok(paths)
    }

    fn resolve_symbol_refs(
        &self,
        repository_id: &str,
        symbol_query: &str,
    ) -> Result<Vec<SymbolRef>> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT symbols.id
                  FROM symbols
                  JOIN files ON symbols.file_id = files.id
                 WHERE files.repository_id = ?1
                   AND (symbols.id = ?2 OR symbols.name = ?2 OR symbols.qualified_name = ?2)
                 ORDER BY symbols.qualified_name, files.path, symbols.start_line
                 LIMIT 25",
            )
            .map_err(StoreError::Sqlite)?;
        let rows = statement
            .query_map(params![repository_id, symbol_query], |row| {
                Ok(SymbolRef { id: row.get(0)? })
            })
            .map_err(StoreError::Sqlite)?;
        collect_rows(rows)
    }

    fn resolve_symbol_refs_for_ref(
        &self,
        repository_id: &str,
        repository_ref_id: &str,
        symbol_query: &str,
    ) -> Result<Vec<SymbolRef>> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT symbols.id
                  FROM symbols
                  JOIN files ON symbols.file_id = files.id
                  JOIN ref_files ON ref_files.file_id = files.id
                 WHERE files.repository_id = ?1
                   AND ref_files.repository_ref_id = ?2
                   AND (symbols.id = ?3 OR symbols.name = ?3 OR symbols.qualified_name = ?3)
                 ORDER BY symbols.qualified_name, files.path, symbols.start_line
                 LIMIT 25",
            )
            .map_err(StoreError::Sqlite)?;
        let rows = statement
            .query_map(
                params![repository_id, repository_ref_id, symbol_query],
                |row| Ok(SymbolRef { id: row.get(0)? }),
            )
            .map_err(StoreError::Sqlite)?;
        collect_rows(rows)
    }

    fn all_symbol_refs(&self, repository_id: &str) -> Result<Vec<SymbolRef>> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT symbols.id
                  FROM symbols
                  JOIN files ON symbols.file_id = files.id
                 WHERE files.repository_id = ?1
                 ORDER BY symbols.qualified_name, files.path, symbols.start_line
                 LIMIT 500",
            )
            .map_err(StoreError::Sqlite)?;
        let rows = statement
            .query_map(params![repository_id], |row| {
                Ok(SymbolRef { id: row.get(0)? })
            })
            .map_err(StoreError::Sqlite)?;
        collect_rows(rows)
    }

    fn all_symbol_refs_for_ref(
        &self,
        repository_id: &str,
        repository_ref_id: &str,
    ) -> Result<Vec<SymbolRef>> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT symbols.id
                  FROM symbols
                  JOIN files ON symbols.file_id = files.id
                  JOIN ref_files ON ref_files.file_id = files.id
                 WHERE files.repository_id = ?1
                   AND ref_files.repository_ref_id = ?2
                 ORDER BY symbols.qualified_name, files.path, symbols.start_line
                 LIMIT 500",
            )
            .map_err(StoreError::Sqlite)?;
        let rows = statement
            .query_map(params![repository_id, repository_ref_id], |row| {
                Ok(SymbolRef { id: row.get(0)? })
            })
            .map_err(StoreError::Sqlite)?;
        collect_rows(rows)
    }

    fn call_path_edges(&self, repository_id: &str) -> Result<Vec<CallPathEdge>> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT calls.id, calls.callee_text, calls.call_line, calls.confidence,
                        calls.resolution_status,
                        caller.id, caller.name, caller.qualified_name, caller.kind,
                        caller_file.path, caller.start_line, caller.end_line,
                        callee.id, callee.name, callee.qualified_name, callee.kind,
                        callee_file.path, callee.start_line, callee.end_line,
                        caller_file.content_hash, calls.index_run_id, calls.parser_version,
                        caller_file.indexed_at
                   FROM calls
                   JOIN symbols caller ON calls.caller_symbol_id = caller.id
                   JOIN files caller_file ON caller.file_id = caller_file.id
                   LEFT JOIN symbols callee ON calls.callee_symbol_id = callee.id
                   LEFT JOIN files callee_file ON callee.file_id = callee_file.id
                  WHERE caller_file.repository_id = ?1
                  ORDER BY caller.qualified_name, caller_file.path, calls.call_line,
                           calls.callee_text, calls.id",
            )
            .map_err(StoreError::Sqlite)?;
        let rows = statement
            .query_map(params![repository_id], call_path_edge)
            .map_err(StoreError::Sqlite)?;
        collect_rows(rows)
    }

    fn call_path_edges_for_ref(
        &self,
        repository_id: &str,
        repository_ref_id: &str,
    ) -> Result<Vec<CallPathEdge>> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT calls.id, calls.callee_text, calls.call_line, calls.confidence,
                        calls.resolution_status,
                        caller.id, caller.name, caller.qualified_name, caller.kind,
                        caller_file.path, caller.start_line, caller.end_line,
                        callee.id, callee.name, callee.qualified_name, callee.kind,
                        callee_file.path, callee.start_line, callee.end_line,
                        caller_file.content_hash, calls.index_run_id, calls.parser_version,
                        caller_file.indexed_at
                   FROM calls
                   JOIN symbols caller ON calls.caller_symbol_id = caller.id
                   JOIN files caller_file ON caller.file_id = caller_file.id
                   JOIN ref_files caller_ref ON caller_ref.file_id = caller_file.id
                   LEFT JOIN symbols callee ON calls.callee_symbol_id = callee.id
                   LEFT JOIN files callee_file ON callee.file_id = callee_file.id
                   LEFT JOIN ref_files callee_ref
                     ON callee_ref.file_id = callee_file.id
                    AND callee_ref.repository_ref_id = ?2
                  WHERE caller_file.repository_id = ?1
                    AND caller_ref.repository_ref_id = ?2
                    AND (callee.id IS NULL OR callee_ref.repository_ref_id IS NOT NULL)
                  ORDER BY caller.qualified_name, caller_file.path, calls.call_line,
                           calls.callee_text, calls.id",
            )
            .map_err(StoreError::Sqlite)?;
        let rows = statement
            .query_map(params![repository_id, repository_ref_id], call_path_edge)
            .map_err(StoreError::Sqlite)?;
        collect_rows(rows)
    }

    pub fn context_pack(
        &self,
        repository_id: &str,
        symbol_query: &str,
        limit: usize,
    ) -> Result<ContextPack> {
        let limit = limit.clamp(1, 25);
        let mut focus_symbols = self.find_symbols(repository_id, symbol_query)?;
        focus_symbols.truncate(limit);
        let mut direct_callers = self.callers(repository_id, symbol_query)?;
        direct_callers.truncate(limit);
        let mut direct_callees = self.callees(repository_id, symbol_query)?;
        direct_callees.truncate(limit);

        let mut files = std::collections::BTreeSet::new();
        for symbol in &focus_symbols {
            files.insert(symbol.path.clone());
        }
        for row in direct_callers.iter().chain(direct_callees.iter()) {
            if let Some(path) = &row.path {
                files.insert(path.clone());
            }
        }

        Ok(ContextPack {
            format: "symdex.context_pack.v1".to_owned(),
            repository_id: repository_id.to_owned(),
            query: symbol_query.to_owned(),
            focus_symbols,
            direct_callers,
            direct_callees,
            files: files.into_iter().collect(),
            limits: ContextPackLimits {
                max_symbols: limit,
                max_callers: limit,
                max_callees: limit,
            },
            notes: vec![
                "metadata_only_no_source_text".to_owned(),
                "direct_relationships_only".to_owned(),
            ],
        })
    }

    pub fn context_pack_for_ref(
        &self,
        repository_id: &str,
        repository_ref_id: &str,
        symbol_query: &str,
        limit: usize,
    ) -> Result<ContextPack> {
        let limit = limit.clamp(1, 25);
        let mut focus_symbols =
            self.find_symbols_for_ref(repository_id, repository_ref_id, symbol_query)?;
        focus_symbols.truncate(limit);
        let mut direct_callers =
            self.callers_for_ref(repository_id, repository_ref_id, symbol_query)?;
        direct_callers.truncate(limit);
        let mut direct_callees =
            self.callees_for_ref(repository_id, repository_ref_id, symbol_query)?;
        direct_callees.truncate(limit);

        let mut files = std::collections::BTreeSet::new();
        for symbol in &focus_symbols {
            files.insert(symbol.path.clone());
        }
        for row in direct_callers.iter().chain(direct_callees.iter()) {
            if let Some(path) = &row.path {
                files.insert(path.clone());
            }
        }

        Ok(ContextPack {
            format: "symdex.context_pack.v1".to_owned(),
            repository_id: repository_id.to_owned(),
            query: symbol_query.to_owned(),
            focus_symbols,
            direct_callers,
            direct_callees,
            files: files.into_iter().collect(),
            limits: ContextPackLimits {
                max_symbols: limit,
                max_callers: limit,
                max_callees: limit,
            },
            notes: vec![
                "metadata_only_no_source_text".to_owned(),
                "direct_relationships_only".to_owned(),
                "repository_ref_scoped".to_owned(),
            ],
        })
    }

    pub fn storage_explorer_summary(
        &self,
        repository_id: &str,
        embedding_model: &str,
    ) -> Result<StorageExplorerSummary> {
        let status = self.repository_status(repository_id)?;
        let sqlite = SqliteStorageSummary {
            repositories: self.count_repositories(repository_id)?,
            files: status.files_indexed,
            chunks: status.chunks_indexed,
            symbols: status.symbols_indexed,
            calls: status.calls_indexed,
            index_runs: self.count_index_runs(repository_id)?,
        };
        let chunk_projection = self.chunk_projection(repository_id)?;
        let latest_embedding = self.latest_embedding_run(repository_id)?;
        let projected_model = latest_embedding
            .as_ref()
            .map(|run| run.embedding_model.as_str())
            .unwrap_or(embedding_model);
        let vector = VectorStorageProjection {
            collection_name: vector_table_name(repository_id, projected_model),
            embedding_model: projected_model.to_owned(),
            embedding_dimension: latest_embedding
                .as_ref()
                .and_then(|run| run.embedding_dimension),
            embeddable_chunks: chunk_projection.embeddable_chunks,
            vector_backed_chunks: chunk_projection.vector_backed_chunks,
            excluded_chunks: chunk_projection.excluded_chunks,
            missing_vector_chunks: chunk_projection.missing_vector_chunks,
        };
        let warnings = storage_warnings(&sqlite, &vector);
        Ok(StorageExplorerSummary {
            repository_id: repository_id.to_owned(),
            sqlite,
            vector,
            warnings,
        })
    }

    pub fn index_coverage_summary(&self, repository_id: &str) -> Result<IndexCoverageSummary> {
        let latest_generation_id = self
            .latest_semantic_generation(repository_id)?
            .map(|generation| generation.id);
        let mut statement = self
            .connection
            .prepare(
                "SELECT
                   files.id,
                   files.path,
                   files.language,
                   (SELECT COUNT(*) FROM chunks WHERE chunks.file_id = files.id),
                   (SELECT COUNT(*) FROM symbols WHERE symbols.file_id = files.id),
                   (SELECT COUNT(*)
                    FROM calls
                    JOIN symbols caller ON calls.caller_symbol_id = caller.id
                    WHERE caller.file_id = files.id),
                   (SELECT COUNT(*)
                    FROM chunks
                    WHERE chunks.file_id = files.id
                      AND chunks.excluded_reason IS NULL),
                   (SELECT COUNT(*)
                                        FROM chunks
                                        JOIN chunk_embeddings
                                            ON chunk_embeddings.chunk_id = chunks.id
                                         AND chunk_embeddings.file_id = files.id
                                        WHERE chunks.file_id = files.id
                                            AND chunk_embeddings.repository_id = files.repository_id
                                            AND chunk_embeddings.generation_id = ?2
                                            AND chunk_embeddings.semantic_layer = 'fast'
                                            AND chunk_embeddings.status = 'current'),
                   (SELECT COUNT(*)
                    FROM chunks
                    WHERE chunks.file_id = files.id
                      AND chunks.excluded_reason IS NOT NULL)
                 FROM files
                 WHERE files.repository_id = ?1
                 ORDER BY files.path
                 LIMIT 200",
            )
            .map_err(StoreError::Sqlite)?;
        let rows = statement
            .query_map(
                params![repository_id, latest_generation_id.as_deref()],
                |row| {
                    let chunks = row.get::<_, i64>(3)? as usize;
                    let embeddable_chunks = row.get::<_, i64>(6)? as usize;
                    let vector_backed_chunks = row.get::<_, i64>(7)? as usize;
                    let excluded_chunks = row.get::<_, i64>(8)? as usize;
                    Ok(FileCoverageBaseRow {
                        file_id: row.get(0)?,
                        path: row.get(1)?,
                        language: row.get(2)?,
                        chunks,
                        symbols: row.get::<_, i64>(4)? as usize,
                        calls: row.get::<_, i64>(5)? as usize,
                        embeddable_chunks,
                        vector_backed_chunks,
                        excluded_chunks,
                        status: file_coverage_status(
                            chunks,
                            embeddable_chunks,
                            vector_backed_chunks,
                            excluded_chunks,
                        ),
                    })
                },
            )
            .map_err(StoreError::Sqlite)?;
        let files = collect_rows(rows)?
            .into_iter()
            .map(|row| {
                let detail =
                    self.file_detail_summary(&row.file_id, latest_generation_id.as_deref())?;
                Ok(FileCoverageRow {
                    path: row.path,
                    language: row.language,
                    chunks: row.chunks,
                    symbols: row.symbols,
                    calls: row.calls,
                    embeddable_chunks: row.embeddable_chunks,
                    vector_backed_chunks: row.vector_backed_chunks,
                    excluded_chunks: row.excluded_chunks,
                    status: row.status,
                    detail,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(IndexCoverageSummary {
            repository_id: repository_id.to_owned(),
            files,
        })
    }

    pub fn symbol_outline_summary(&self, repository_id: &str) -> Result<SymbolOutlineSummary> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT symbols.id, symbols.parent_symbol_id, symbols.kind,
                        symbols.qualified_name, symbols.name, symbols.start_line,
                        symbols.end_line, files.path
                 FROM symbols
                 JOIN files ON symbols.file_id = files.id
                 WHERE files.repository_id = ?1
                 ORDER BY files.path, symbols.start_line, symbols.qualified_name
                 LIMIT 300",
            )
            .map_err(StoreError::Sqlite)?;
        let rows = statement
            .query_map(params![repository_id], |row| {
                Ok(SymbolOutlineBaseRow {
                    id: row.get(0)?,
                    parent_symbol_id: row.get(1)?,
                    kind: row.get(2)?,
                    qualified_name: row.get(3)?,
                    name: row.get(4)?,
                    start_line: row.get::<_, i64>(5)? as usize,
                    end_line: row.get::<_, i64>(6)? as usize,
                    path: row.get(7)?,
                })
            })
            .map_err(StoreError::Sqlite)?;
        let bases = collect_rows(rows)?;
        Ok(SymbolOutlineSummary {
            repository_id: repository_id.to_owned(),
            symbols: symbol_outline_rows(bases),
        })
    }

    pub fn call_resolution_summary(&self, repository_id: &str) -> Result<CallResolutionSummary> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT files.path, caller.qualified_name, calls.callee_text, calls.call_line,
                        calls.confidence, calls.resolution_status
                 FROM calls
                 JOIN symbols caller ON calls.caller_symbol_id = caller.id
                 JOIN files ON caller.file_id = files.id
                 WHERE files.repository_id = ?1
                 ORDER BY calls.resolution_status, calls.confidence ASC, files.path,
                          calls.call_line, calls.callee_text
                 LIMIT 500",
            )
            .map_err(StoreError::Sqlite)?;
        let rows = statement
            .query_map(params![repository_id], |row| {
                let confidence: f64 = row.get(4)?;
                Ok(CallResolutionEdgeRow {
                    path: row.get(0)?,
                    caller_symbol: row.get(1)?,
                    callee_text: row.get(2)?,
                    call_line: row.get::<_, i64>(3)? as usize,
                    confidence,
                    resolution_status: row.get(5)?,
                    confidence_bucket: confidence_bucket(confidence),
                })
            })
            .map_err(StoreError::Sqlite)?;
        Ok(CallResolutionSummary {
            repository_id: repository_id.to_owned(),
            buckets: call_resolution_buckets(collect_rows(rows)?),
        })
    }

    pub fn embedding_coverage_summary(
        &self,
        repository_id: &str,
        configured_embedding_model: &str,
    ) -> Result<EmbeddingCoverageSummary> {
        let status = self.repository_status(repository_id)?;
        let chunk_projection = self.chunk_projection(repository_id)?;
        let latest_embedding = self.latest_embedding_run(repository_id)?;
        let projected_model = latest_embedding
            .as_ref()
            .map(|run| run.embedding_model.as_str())
            .unwrap_or(configured_embedding_model);
        let embedding_dimension = latest_embedding
            .as_ref()
            .and_then(|run| run.embedding_dimension);
        let latest_chunks_embedded = latest_embedding.as_ref().map(|run| run.chunks_embedded);
        let exclusion_reasons = self.embedding_exclusion_reasons(repository_id)?;

        let mut health = embedding_coverage_health(
            &status,
            &chunk_projection,
            latest_embedding.as_ref(),
            configured_embedding_model,
        );
        if health.is_empty() {
            health.push(StorageHealthRow {
                status: StorageHealthStatus::Ok,
                label: "embedding_coverage_ok".to_owned(),
                detail: "All embeddable SQLite chunks have recorded vector point metadata."
                    .to_owned(),
            });
        }

        Ok(EmbeddingCoverageSummary {
            repository_id: repository_id.to_owned(),
            collection_name: vector_table_name(repository_id, projected_model),
            configured_embedding_model: configured_embedding_model.to_owned(),
            embedding_model: projected_model.to_owned(),
            embedding_dimension,
            total_chunks: status.chunks_indexed,
            embeddable_chunks: chunk_projection.embeddable_chunks,
            vector_backed_chunks: chunk_projection.vector_backed_chunks,
            missing_vector_chunks: chunk_projection.missing_vector_chunks,
            excluded_chunks: chunk_projection.excluded_chunks,
            latest_chunks_embedded,
            exclusion_reasons,
            health,
        })
    }

    pub fn index_runs_timeline_summary(
        &self,
        repository_id: &str,
    ) -> Result<IndexRunsTimelineSummary> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT id, started_at, finished_at, status, embedding_model,
                        embedding_dimension, files_seen, files_indexed,
                        chunks_embedded, error_summary, run_kind
                 FROM index_runs
                 WHERE repository_id = ?1
                 ORDER BY started_at DESC, finished_at DESC, id DESC
                 LIMIT 50",
            )
            .map_err(StoreError::Sqlite)?;
        let rows = statement
            .query_map(params![repository_id], |row| {
                Ok(IndexRunTimelineRow {
                    id: row.get(0)?,
                    started_at: row.get(1)?,
                    finished_at: row.get(2)?,
                    status: row.get(3)?,
                    embedding_model: row.get(4)?,
                    embedding_dimension: row
                        .get::<_, Option<i64>>(5)?
                        .map(|dimension| dimension as usize),
                    files_seen: row.get::<_, i64>(6)? as usize,
                    files_indexed: row.get::<_, i64>(7)? as usize,
                    chunks_embedded: row.get::<_, i64>(8)? as usize,
                    error_summary: row.get(9)?,
                    run_kind: row.get(10)?,
                })
            })
            .map_err(StoreError::Sqlite)?;
        Ok(IndexRunsTimelineSummary {
            repository_id: repository_id.to_owned(),
            runs: collect_rows(rows)?,
        })
    }

    pub fn semantic_neighborhood_summary(
        &self,
        repository_id: &str,
        configured_embedding_model: &str,
    ) -> Result<SemanticNeighborhoodSummary> {
        let latest_generation = self.latest_semantic_generation(repository_id)?;
        let latest_embedding = self.latest_embedding_run(repository_id)?;
        let projected_model = latest_generation
            .as_ref()
            .map(|generation| generation.fast_model.clone())
            .or_else(|| {
                latest_embedding
                    .as_ref()
                    .map(|run| run.embedding_model.clone())
            })
            .unwrap_or_else(|| configured_embedding_model.to_owned());
        let rows = if let Some(generation) = latest_generation {
            let mut statement = self
                .connection
                .prepare(
                    "SELECT chunk_embeddings.vector_point_id, files.path, chunks.start_line,
                            chunks.end_line, symbols.qualified_name, chunks.kind,
                            files.language, chunk_embeddings.text_hash
                     FROM chunk_embeddings
                     JOIN chunks ON chunk_embeddings.chunk_id = chunks.id
                     JOIN files ON chunk_embeddings.file_id = files.id
                               AND chunks.file_id = files.id
                     LEFT JOIN symbols ON chunks.symbol_id = symbols.id
                     WHERE chunk_embeddings.repository_id = ?1
                       AND files.repository_id = ?1
                       AND chunk_embeddings.generation_id = ?2
                       AND chunk_embeddings.semantic_layer = 'fast'
                       AND chunk_embeddings.status = 'current'
                     ORDER BY files.path, chunks.start_line, chunks.end_line, chunks.id
                     LIMIT 100",
                )
                .map_err(StoreError::Sqlite)?;
            let rows = statement
                .query_map(params![repository_id, generation.id], |row| {
                    Ok(SemanticNeighborhoodRow {
                        vector_point_id: row.get(0)?,
                        path: row.get(1)?,
                        start_line: row.get::<_, i64>(2)? as usize,
                        end_line: row.get::<_, i64>(3)? as usize,
                        symbol_name: row.get(4)?,
                        chunk_kind: row.get(5)?,
                        language: row.get(6)?,
                        score: None,
                        text_hash: row.get(7)?,
                    })
                })
                .map_err(StoreError::Sqlite)?;
            collect_rows(rows)?
        } else {
            Vec::new()
        };
        let health = semantic_neighborhood_health(&rows);
        Ok(SemanticNeighborhoodSummary {
            repository_id: repository_id.to_owned(),
            collection_name: vector_table_name(repository_id, &projected_model),
            embedding_model: projected_model,
            rows,
            health,
        })
    }

    pub fn cross_store_health_summary(
        &self,
        repository_id: &str,
        configured_embedding_model: &str,
    ) -> Result<CrossStoreHealthSummary> {
        let projection = self.chunk_projection(repository_id)?;
        let latest_embedding = self.latest_embedding_run(repository_id)?;
        let projected_model = latest_embedding
            .as_ref()
            .map(|run| run.embedding_model.as_str())
            .unwrap_or(configured_embedding_model);
        let mut rows = Vec::new();

        if projection.embeddable_chunks > 0 && latest_embedding.is_none() {
            rows.push(StorageHealthRow {
                status: StorageHealthStatus::Error,
                label: "missing_collection".to_owned(),
                detail: "Embeddable chunks exist, but no successful semantic index run has recorded collection metadata.".to_owned(),
            });
        } else if projection.embeddable_chunks > 0 && projection.vector_backed_chunks == 0 {
            rows.push(StorageHealthRow {
                status: StorageHealthStatus::Error,
                label: "missing_collection".to_owned(),
                detail: "Embeddable chunks exist, but no chunks have recorded vector point IDs."
                    .to_owned(),
            });
        }
        if projection.missing_vector_chunks > 0 {
            rows.push(StorageHealthRow {
                status: StorageHealthStatus::Warning,
                label: "missing_vectors".to_owned(),
                detail: format!(
                    "{} embeddable chunks are missing recorded vector point IDs.",
                    projection.missing_vector_chunks
                ),
            });
        }
        if projection.excluded_chunks > 0 {
            rows.push(StorageHealthRow {
                status: StorageHealthStatus::Warning,
                label: "excluded_chunks".to_owned(),
                detail: format!(
                    "{} chunks are intentionally excluded from semantic embedding.",
                    projection.excluded_chunks
                ),
            });
        }
        if let Some(latest) = &latest_embedding
            && latest.embedding_model != configured_embedding_model
        {
            rows.push(StorageHealthRow {
                status: StorageHealthStatus::Error,
                label: "model_drift".to_owned(),
                detail: format!(
                    "Configured model {configured_embedding_model} differs from latest indexed model {}.",
                    latest.embedding_model
                ),
            });
        }
        let dimensions = self.successful_embedding_dimensions(repository_id, projected_model)?;
        if dimensions.len() > 1 {
            rows.push(StorageHealthRow {
                status: StorageHealthStatus::Error,
                label: "dimension_drift".to_owned(),
                detail: format!(
                    "Successful runs for model {projected_model} recorded multiple dimensions: {}.",
                    dimensions
                        .iter()
                        .map(usize::to_string)
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            });
        }
        if rows.is_empty() {
            rows.push(StorageHealthRow {
                status: StorageHealthStatus::Ok,
                label: "cross_store_ok".to_owned(),
                detail:
                    "SQLite chunk metadata and recorded vector projection metadata are aligned."
                        .to_owned(),
            });
        }

        Ok(CrossStoreHealthSummary {
            repository_id: repository_id.to_owned(),
            collection_name: vector_table_name(repository_id, projected_model),
            rows,
        })
    }

    pub fn indexed_file_freshness_snapshots(
        &self,
        repository_id: &str,
    ) -> Result<Vec<FileFreshnessSnapshot>> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT path, content_hash, indexed_at, index_run_id, parser_version
                 FROM files
                 WHERE repository_id = ?1
                 ORDER BY path",
            )
            .map_err(StoreError::Sqlite)?;
        let rows = statement
            .query_map(params![repository_id], |row| {
                Ok(FileFreshnessSnapshot {
                    path: row.get(0)?,
                    content_hash: row.get(1)?,
                    indexed_at: row.get(2)?,
                    index_run_id: row.get(3)?,
                    parser_version: row.get(4)?,
                })
            })
            .map_err(StoreError::Sqlite)?;
        collect_rows(rows)
    }

    pub fn indexed_file_freshness_snapshots_for_ref(
        &self,
        repository_id: &str,
        repository_ref_id: &str,
    ) -> Result<Vec<FileFreshnessSnapshot>> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT ref_files.path, files.content_hash, files.indexed_at,
                        files.index_run_id, files.parser_version
                   FROM ref_files
                   JOIN files ON files.id = ref_files.file_id
                             AND files.repository_id = ref_files.repository_id
                  WHERE ref_files.repository_id = ?1
                    AND ref_files.repository_ref_id = ?2
                  ORDER BY ref_files.path",
            )
            .map_err(StoreError::Sqlite)?;
        let rows = statement
            .query_map(params![repository_id, repository_ref_id], |row| {
                Ok(FileFreshnessSnapshot {
                    path: row.get(0)?,
                    content_hash: row.get(1)?,
                    indexed_at: row.get(2)?,
                    index_run_id: row.get(3)?,
                    parser_version: row.get(4)?,
                })
            })
            .map_err(StoreError::Sqlite)?;
        collect_rows(rows)
    }

    pub fn rust_symbols_for_repository(&self, repository_id: &str) -> Result<Vec<SymbolRecord>> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT symbols.id, symbols.file_id, symbols.parent_symbol_id, symbols.name,
                        symbols.qualified_name, symbols.kind, symbols.signature,
                        symbols.start_line, symbols.end_line, symbols.start_byte,
                        symbols.end_byte, symbols.index_run_id, symbols.parser_version
                 FROM symbols
                 JOIN files ON symbols.file_id = files.id
                 WHERE files.repository_id = ?1
                   AND files.language = 'rust'
                 ORDER BY symbols.qualified_name, files.path, symbols.start_line, symbols.id",
            )
            .map_err(StoreError::Sqlite)?;
        let rows = statement
            .query_map(params![repository_id], |row| {
                Ok(SymbolRecord {
                    id: row.get(0)?,
                    file_id: row.get(1)?,
                    parent_symbol_id: row.get(2)?,
                    name: row.get(3)?,
                    qualified_name: row.get(4)?,
                    kind: row.get(5)?,
                    signature: row.get(6)?,
                    start_line: row.get::<_, i64>(7)? as usize,
                    end_line: row.get::<_, i64>(8)? as usize,
                    start_byte: row.get::<_, i64>(9)? as usize,
                    end_byte: row.get::<_, i64>(10)? as usize,
                    index_run_id: row.get(11)?,
                    parser_version: row.get(12)?,
                })
            })
            .map_err(StoreError::Sqlite)?;
        collect_rows(rows)
    }

    fn file_paths(&self, repository_id: &str) -> Result<Vec<String>> {
        let mut statement = self
            .connection
            .prepare("SELECT DISTINCT path FROM files WHERE repository_id = ?1")
            .map_err(StoreError::Sqlite)?;
        let rows = statement
            .query_map(params![repository_id], |row| row.get(0))
            .map_err(StoreError::Sqlite)?;
        let mut paths = Vec::new();
        for row in rows {
            paths.push(row.map_err(StoreError::Sqlite)?);
        }
        Ok(paths)
    }

    fn count_joined(&self, repository_id: &str, table: &str) -> Result<usize> {
        let sql = format!(
            "SELECT COUNT(*)
             FROM {table}
             JOIN files ON {table}.file_id = files.id
             WHERE files.repository_id = ?1"
        );
        let count: i64 = self
            .connection
            .query_row(&sql, params![repository_id], |row| row.get(0))
            .map_err(StoreError::Sqlite)?;
        count
            .try_into()
            .map_err(|_| StoreError::UnexpectedResponse(format!("negative {table} count")))
    }

    fn count_calls(&self, repository_id: &str) -> Result<usize> {
        let count: i64 = self
            .connection
            .query_row(
                "SELECT COUNT(*)
                 FROM calls
                 JOIN symbols ON calls.caller_symbol_id = symbols.id
                 JOIN files ON symbols.file_id = files.id
                 WHERE files.repository_id = ?1",
                params![repository_id],
                |row| row.get(0),
            )
            .map_err(StoreError::Sqlite)?;
        count
            .try_into()
            .map_err(|_| StoreError::UnexpectedResponse("negative call count".to_owned()))
    }

    fn count_repositories(&self, repository_id: &str) -> Result<usize> {
        self.count_scalar(
            "SELECT COUNT(*) FROM repositories WHERE id = ?1",
            repository_id,
            "repository",
        )
    }

    fn count_index_runs(&self, repository_id: &str) -> Result<usize> {
        self.count_scalar(
            "SELECT COUNT(*) FROM index_runs WHERE repository_id = ?1",
            repository_id,
            "index run",
        )
    }

    fn count_scalar(&self, sql: &str, repository_id: &str, label: &str) -> Result<usize> {
        let count: i64 = self
            .connection
            .query_row(sql, params![repository_id], |row| row.get(0))
            .map_err(StoreError::Sqlite)?;
        count
            .try_into()
            .map_err(|_| StoreError::UnexpectedResponse(format!("negative {label} count")))
    }

    fn chunk_projection(&self, repository_id: &str) -> Result<ChunkProjectionCounts> {
        let latest_generation_id = self
            .latest_semantic_generation(repository_id)?
            .map(|generation| generation.id);
        self.connection
            .query_row(
                "SELECT
                   COALESCE(SUM(CASE WHEN chunks.excluded_reason IS NULL THEN 1 ELSE 0 END), 0),
                                     COALESCE(SUM(CASE WHEN chunk_embeddings.vector_point_id IS NOT NULL THEN 1 ELSE 0 END), 0),
                   COALESCE(SUM(CASE WHEN chunks.excluded_reason IS NOT NULL THEN 1 ELSE 0 END), 0),
                   COALESCE(SUM(CASE
                                         WHEN chunks.excluded_reason IS NULL AND chunk_embeddings.vector_point_id IS NULL
                     THEN 1 ELSE 0 END), 0)
                 FROM chunks
                 JOIN files ON chunks.file_id = files.id
                                 LEFT JOIN chunk_embeddings
                                     ON chunk_embeddings.chunk_id = chunks.id
                                    AND chunk_embeddings.file_id = files.id
                                    AND chunk_embeddings.repository_id = files.repository_id
                                    AND chunk_embeddings.generation_id = ?2
                                    AND chunk_embeddings.semantic_layer = 'fast'
                                    AND chunk_embeddings.status = 'current'
                 WHERE files.repository_id = ?1",
                                params![repository_id, latest_generation_id.as_deref()],
                |row| {
                    Ok(ChunkProjectionCounts {
                        embeddable_chunks: row.get::<_, i64>(0)? as usize,
                        vector_backed_chunks: row.get::<_, i64>(1)? as usize,
                        excluded_chunks: row.get::<_, i64>(2)? as usize,
                        missing_vector_chunks: row.get::<_, i64>(3)? as usize,
                    })
                },
            )
            .map_err(StoreError::Sqlite)
    }

    fn file_detail_summary(
        &self,
        file_id: &str,
        latest_generation_id: Option<&str>,
    ) -> Result<FileDetailSummary> {
        Ok(FileDetailSummary {
            chunks: self.file_chunk_details(file_id, latest_generation_id)?,
            symbols: self.file_symbol_details(file_id)?,
            calls: self.file_call_details(file_id)?,
        })
    }

    fn file_chunk_details(
        &self,
        file_id: &str,
        latest_generation_id: Option<&str>,
    ) -> Result<Vec<FileChunkDetailRow>> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT chunks.kind, symbols.qualified_name, chunks.start_line,
                        chunks.end_line,
                        EXISTS(
                          SELECT 1
                          FROM chunk_embeddings
                          WHERE chunk_embeddings.chunk_id = chunks.id
                            AND chunk_embeddings.file_id = chunks.file_id
                            AND chunk_embeddings.generation_id = ?2
                            AND chunk_embeddings.semantic_layer = 'fast'
                            AND chunk_embeddings.status = 'current'
                        ),
                        chunks.excluded_reason
                 FROM chunks
                 LEFT JOIN symbols ON chunks.symbol_id = symbols.id
                 WHERE chunks.file_id = ?1
                 ORDER BY chunks.start_line, chunks.kind
                 LIMIT 6",
            )
            .map_err(StoreError::Sqlite)?;
        let rows = statement
            .query_map(params![file_id, latest_generation_id], |row| {
                let has_vector: bool = row.get(4)?;
                let excluded_reason: Option<String> = row.get(5)?;
                Ok(FileChunkDetailRow {
                    kind: row.get(0)?,
                    symbol: row.get(1)?,
                    start_line: row.get::<_, i64>(2)? as usize,
                    end_line: row.get::<_, i64>(3)? as usize,
                    vector_status: chunk_vector_status(has_vector, &excluded_reason),
                    excluded_reason,
                })
            })
            .map_err(StoreError::Sqlite)?;
        collect_rows(rows)
    }

    fn file_symbol_details(&self, file_id: &str) -> Result<Vec<FileSymbolDetailRow>> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT kind, qualified_name, parent_symbol_id, start_line, end_line
                 FROM symbols
                 WHERE file_id = ?1
                 ORDER BY start_line, qualified_name
                 LIMIT 6",
            )
            .map_err(StoreError::Sqlite)?;
        let rows = statement
            .query_map(params![file_id], |row| {
                Ok(FileSymbolDetailRow {
                    kind: row.get(0)?,
                    qualified_name: row.get(1)?,
                    parent_symbol_id: row.get(2)?,
                    start_line: row.get::<_, i64>(3)? as usize,
                    end_line: row.get::<_, i64>(4)? as usize,
                })
            })
            .map_err(StoreError::Sqlite)?;
        collect_rows(rows)
    }

    fn file_call_details(&self, file_id: &str) -> Result<Vec<FileCallDetailRow>> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT caller.qualified_name, calls.callee_text, calls.call_line,
                        calls.confidence, calls.resolution_status
                 FROM calls
                 JOIN symbols caller ON calls.caller_symbol_id = caller.id
                 WHERE caller.file_id = ?1
                 ORDER BY calls.call_line, calls.callee_text
                 LIMIT 6",
            )
            .map_err(StoreError::Sqlite)?;
        let rows = statement
            .query_map(params![file_id], |row| {
                Ok(FileCallDetailRow {
                    caller_symbol: row.get(0)?,
                    callee_text: row.get(1)?,
                    call_line: row.get::<_, i64>(2)? as usize,
                    confidence: row.get(3)?,
                    resolution_status: row.get(4)?,
                })
            })
            .map_err(StoreError::Sqlite)?;
        collect_rows(rows)
    }

    fn embedding_exclusion_reasons(
        &self,
        repository_id: &str,
    ) -> Result<Vec<EmbeddingExclusionRow>> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT chunks.excluded_reason, COUNT(*)
                 FROM chunks
                 JOIN files ON chunks.file_id = files.id
                 WHERE files.repository_id = ?1
                   AND chunks.excluded_reason IS NOT NULL
                 GROUP BY chunks.excluded_reason
                 ORDER BY COUNT(*) DESC, chunks.excluded_reason
                 LIMIT 12",
            )
            .map_err(StoreError::Sqlite)?;
        let rows = statement
            .query_map(params![repository_id], |row| {
                Ok(EmbeddingExclusionRow {
                    reason: row.get(0)?,
                    chunks: row.get::<_, i64>(1)? as usize,
                })
            })
            .map_err(StoreError::Sqlite)?;
        collect_rows(rows)
    }

    fn successful_embedding_dimensions(
        &self,
        repository_id: &str,
        embedding_model: &str,
    ) -> Result<Vec<usize>> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT DISTINCT embedding_dimension
                 FROM index_runs
                 WHERE repository_id = ?1
                   AND embedding_model = ?2
                   AND status = 'success'
                   AND embedding_dimension IS NOT NULL
                 ORDER BY embedding_dimension",
            )
            .map_err(StoreError::Sqlite)?;
        let rows = statement
            .query_map(params![repository_id, embedding_model], |row| {
                Ok(row.get::<_, i64>(0)? as usize)
            })
            .map_err(StoreError::Sqlite)?;
        collect_rows(rows)
    }

    fn latest_embedding_run(&self, repository_id: &str) -> Result<Option<EmbeddingIndexMetadata>> {
        self.connection
            .query_row(
                "SELECT embedding_model, embedding_dimension, chunks_embedded
                 FROM index_runs
                 WHERE repository_id = ?1
                   AND status = 'success'
                   AND embedding_dimension IS NOT NULL
                 ORDER BY finished_at DESC, started_at DESC
                 LIMIT 1",
                params![repository_id],
                embedding_index_metadata,
            )
            .optional()
            .map_err(StoreError::Sqlite)
    }

    fn latest_embedding_run_for_model(
        &self,
        repository_id: &str,
        embedding_model: &str,
    ) -> Result<Option<EmbeddingIndexMetadata>> {
        self.connection
            .query_row(
                "SELECT embedding_model, embedding_dimension, chunks_embedded
                 FROM index_runs
                 WHERE repository_id = ?1
                   AND embedding_model = ?2
                   AND status = 'success'
                   AND embedding_dimension IS NOT NULL
                 ORDER BY finished_at DESC, started_at DESC
                 LIMIT 1",
                params![repository_id, embedding_model],
                embedding_index_metadata,
            )
            .optional()
            .map_err(StoreError::Sqlite)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepositoryRecord {
    pub id: String,
    pub root_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RepositoryRefRecord {
    pub id: String,
    pub repository_id: String,
    pub ref_kind: String,
    pub ref_name: Option<String>,
    pub ref_identity: String,
    pub head_oid: Option<String>,
    pub is_current: bool,
    pub last_seen_at: Option<String>,
    pub deleted_at: Option<String>,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
}

impl RepositoryRefRecord {
    pub fn from_snapshot(
        snapshot: &RepositoryRefSnapshot,
        is_current: bool,
        timestamp: &str,
    ) -> Self {
        let identity = ref_identity(
            snapshot.kind,
            snapshot.name.as_deref(),
            snapshot.head_oid.as_deref(),
        );
        Self {
            id: snapshot.id.clone(),
            repository_id: snapshot.repository_id.clone(),
            ref_kind: snapshot.kind.as_str().to_owned(),
            ref_name: snapshot.name.clone(),
            ref_identity: identity,
            head_oid: snapshot.head_oid.clone(),
            is_current,
            last_seen_at: Some(timestamp.to_owned()),
            deleted_at: None,
            created_at: Some(timestamp.to_owned()),
            updated_at: Some(timestamp.to_owned()),
        }
    }

    pub fn local_branch(repository_id: &str, branch: &str, timestamp: &str) -> Self {
        Self {
            id: stable_id(&[
                "repository-ref",
                repository_id,
                RepositoryRefKind::Branch.as_str(),
                branch,
            ]),
            repository_id: repository_id.to_owned(),
            ref_kind: RepositoryRefKind::Branch.as_str().to_owned(),
            ref_name: Some(branch.to_owned()),
            ref_identity: branch.to_owned(),
            head_oid: None,
            is_current: false,
            last_seen_at: Some(timestamp.to_owned()),
            deleted_at: None,
            created_at: Some(timestamp.to_owned()),
            updated_at: Some(timestamp.to_owned()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepositoryRefSyncSummary {
    pub current: RepositoryRefRecord,
    pub deleted_refs: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileRecord {
    pub id: String,
    pub repository_id: String,
    pub path: String,
    pub language: String,
    pub content_hash: String,
    pub index_run_id: String,
    pub parser_version: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileStateRecord {
    pub path: String,
    pub content_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileIndexEventRecord {
    pub id: String,
    pub index_run_id: String,
    pub repository_id: String,
    pub repository_ref_id: Option<String>,
    pub path: String,
    pub old_content_hash: Option<String>,
    pub new_content_hash: Option<String>,
    pub action: String,
    pub reason: String,
    pub status: String,
    pub error_summary: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChunkRecord {
    pub id: String,
    pub file_id: String,
    pub symbol_id: Option<String>,
    pub kind: String,
    pub text_hash: String,
    pub start_line: usize,
    pub end_line: usize,
    pub start_byte: usize,
    pub end_byte: usize,
    pub vector_point_id: Option<String>,
    pub excluded_reason: Option<String>,
    pub index_run_id: String,
    pub parser_version: String,
    pub embedding_model: Option<String>,
    pub embedding_dimension: Option<usize>,
    pub embedded_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SymbolRecord {
    pub id: String,
    pub file_id: String,
    pub parent_symbol_id: Option<String>,
    pub name: String,
    pub qualified_name: String,
    pub kind: String,
    pub signature: Option<String>,
    pub start_line: usize,
    pub end_line: usize,
    pub start_byte: usize,
    pub end_byte: usize,
    pub index_run_id: String,
    pub parser_version: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CallRecord {
    pub id: String,
    pub caller_symbol_id: String,
    pub callee_text: String,
    pub callee_symbol_id: Option<String>,
    pub call_line: usize,
    pub confidence: f32,
    pub resolution_status: String,
    pub index_run_id: String,
    pub parser_version: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SymbolReferenceRecord {
    pub id: String,
    pub file_id: String,
    pub source_symbol_id: Option<String>,
    pub target_symbol_id: Option<String>,
    pub reference_text: String,
    pub reference_kind: String,
    pub line: usize,
    pub confidence: f32,
    pub resolution_status: String,
    pub index_run_id: String,
    pub parser_version: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestRecord {
    pub id: String,
    pub repository_id: String,
    pub file_id: String,
    pub symbol_id: Option<String>,
    pub name: String,
    pub qualified_name: String,
    pub framework: String,
    pub language: String,
    pub path: String,
    pub start_line: usize,
    pub end_line: usize,
    pub start_byte: usize,
    pub end_byte: usize,
    pub index_run_id: String,
    pub parser_version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TestSearchRow {
    pub id: String,
    pub name: String,
    pub qualified_name: String,
    pub framework: String,
    pub language: String,
    pub path: String,
    pub start_line: usize,
    pub end_line: usize,
    pub provenance: EvidenceProvenance,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepositoryStatus {
    pub repository_id: String,
    pub current_ref_id: Option<String>,
    pub current_ref_kind: Option<String>,
    pub current_ref_name: Option<String>,
    pub current_head_oid: Option<String>,
    pub files_indexed: usize,
    pub chunks_indexed: usize,
    pub symbols_indexed: usize,
    pub calls_indexed: usize,
    pub last_indexed_at: Option<String>,
    pub embedding_model: Option<String>,
    pub embedding_dimension: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WatcherStatusRecord {
    pub repository_id: String,
    pub root_path: String,
    pub mode: String,
    pub owner_kind: String,
    pub owner_pid: Option<i32>,
    pub socket_path: Option<String>,
    pub state: String,
    pub started_at: Option<String>,
    pub updated_at: Option<String>,
    pub heartbeat_at: Option<String>,
    pub files_seen: usize,
    pub queued_events: usize,
    pub last_indexed_path: Option<String>,
    pub last_error: Option<String>,
    pub active_layer: Option<String>,
    pub quality_status: Option<String>,
    pub quality_pending_jobs: usize,
    pub quality_running_jobs: usize,
    pub quality_failed_jobs: usize,
    pub quality_stale_jobs: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WatcherClientRecord {
    pub repository_id: String,
    pub client_id: String,
    pub client_kind: String,
    pub pid: Option<i32>,
    pub started_at: Option<String>,
    pub heartbeat_at: Option<String>,
    pub last_seen_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexRunRecord {
    pub id: String,
    pub repository_id: String,
    pub repository_ref_id: Option<String>,
    pub status: String,
    pub embedding_model: String,
    pub embedding_dimension: Option<usize>,
    pub files_seen: usize,
    pub files_indexed: usize,
    pub chunks_embedded: usize,
    pub error_summary: Option<String>,
    pub parser_version: String,
    pub indexer_version: String,
    pub run_kind: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticGenerationRecord {
    pub id: String,
    pub repository_id: String,
    pub fast_model: String,
    pub fast_dimension: usize,
    pub fast_completed_at: String,
    pub quality_model: Option<String>,
    pub quality_dimension: Option<usize>,
    pub quality_status: String,
    pub quality_started_at: Option<String>,
    pub quality_completed_at: Option<String>,
    pub active_layer: String,
    pub files_seen: usize,
    pub embeddable_chunks: usize,
    pub fast_embedded_chunks: usize,
    pub quality_embedded_chunks: usize,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FastEmbeddingManifestRecord {
    pub file_id: String,
    pub chunk_id: String,
    pub content_hash: String,
    pub text_hash: String,
    pub vector_point_id: String,
}

#[derive(Debug, Clone, Copy)]
pub struct FastSemanticGenerationInput<'a> {
    pub repository_id: &'a str,
    pub repository_ref_id: Option<&'a str>,
    pub fast_model: &'a str,
    pub fast_dimension: usize,
    pub vector_table: &'a str,
    pub upserted_embeddings: &'a [FastEmbeddingManifestRecord],
    pub files_seen: usize,
    pub completed_at: &'a str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticRoutingSummary {
    pub repository_id: String,
    pub generation_id: String,
    pub active_layer: SemanticLayer,
    pub quality_status: SemanticLayerStatus,
    pub embeddable_chunks: usize,
    pub fast_embedded_chunks: usize,
    pub quality_embedded_chunks: usize,
    pub fast: SemanticLayerManifestSummary,
    pub quality: Option<SemanticLayerManifestSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticLayerManifestSummary {
    pub semantic_layer: SemanticLayer,
    pub embedding_model: String,
    pub embedding_dimension: usize,
    pub vector_table: String,
    pub current_chunks: usize,
    pub stale_chunks: usize,
    pub blocked_chunks: usize,
    pub failed_chunks: usize,
    pub other_chunks: usize,
    pub total_chunks: usize,
    pub expected_chunks: usize,
    pub is_complete: bool,
}

impl SemanticLayerManifestSummary {
    fn empty(
        semantic_layer: SemanticLayer,
        expected_chunks: usize,
        embedding_model: String,
        embedding_dimension: usize,
        vector_table: String,
    ) -> Self {
        Self {
            semantic_layer,
            embedding_model,
            embedding_dimension,
            vector_table,
            current_chunks: 0,
            stale_chunks: 0,
            blocked_chunks: 0,
            failed_chunks: 0,
            other_chunks: 0,
            total_chunks: 0,
            expected_chunks,
            is_complete: expected_chunks == 0,
        }
    }

    fn from_aggregate(
        semantic_layer: SemanticLayer,
        expected_chunks: usize,
        aggregate: SemanticLayerManifestAggregate,
    ) -> Result<Self> {
        let embedding_dimension =
            usize_count(aggregate.embedding_dimension, "embedding dimension")?;
        let current_chunks = usize_count(aggregate.current_chunks, "current chunk count")?;
        let stale_chunks = usize_count(aggregate.stale_chunks, "stale chunk count")?;
        let blocked_chunks = usize_count(aggregate.blocked_chunks, "blocked chunk count")?;
        let failed_chunks = usize_count(aggregate.failed_chunks, "failed chunk count")?;
        let other_chunks = usize_count(aggregate.other_chunks, "other chunk count")?;
        let total_chunks = usize_count(aggregate.total_chunks, "total chunk count")?;
        let is_complete = current_chunks == expected_chunks
            && total_chunks == expected_chunks
            && stale_chunks == 0
            && blocked_chunks == 0
            && failed_chunks == 0
            && other_chunks == 0;

        Ok(Self {
            semantic_layer,
            embedding_model: aggregate.embedding_model,
            embedding_dimension,
            vector_table: aggregate.vector_table,
            current_chunks,
            stale_chunks,
            blocked_chunks,
            failed_chunks,
            other_chunks,
            total_chunks,
            expected_chunks,
            is_complete,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SemanticLayerManifestAggregate {
    embedding_model: String,
    embedding_dimension: i64,
    vector_table: String,
    current_chunks: i64,
    stale_chunks: i64,
    blocked_chunks: i64,
    failed_chunks: i64,
    other_chunks: i64,
    total_chunks: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChunkEmbeddingRecord {
    pub id: String,
    pub repository_id: String,
    pub file_id: String,
    pub chunk_id: String,
    pub semantic_layer: String,
    pub embedding_model: String,
    pub embedding_dimension: usize,
    pub content_hash: String,
    pub text_hash: String,
    pub vector_table: String,
    pub vector_point_id: String,
    pub generation_id: String,
    pub embedded_at: String,
    pub status: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QualityEmbeddingJobRecord {
    pub id: String,
    pub repository_id: String,
    pub generation_id: String,
    pub chunk_id: String,
    pub file_id: String,
    pub path: String,
    pub content_hash: String,
    pub text_hash: String,
    pub status: String,
    pub attempts: usize,
    pub error_summary: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QualityQueueSummary {
    pub repository_id: String,
    pub generation_id: String,
    pub quality_model: String,
    pub quality_status: SemanticLayerStatus,
    pub queued_jobs: usize,
    pub skipped_stale_jobs: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QualityCarryForwardSummary {
    pub repository_id: String,
    pub generation_id: String,
    pub quality_model: String,
    pub carried_embeddings: usize,
    pub quality_dimension: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QualityJobSourceRow {
    pub job: QualityEmbeddingJobRecord,
    pub current_file_id: Option<String>,
    pub current_content_hash: Option<String>,
    pub language: Option<String>,
    pub chunk_kind: Option<String>,
    pub current_text_hash: Option<String>,
    pub start_line: Option<usize>,
    pub end_line: Option<usize>,
    pub start_byte: Option<usize>,
    pub end_byte: Option<usize>,
    pub excluded_reason: Option<String>,
    pub symbol_id: Option<String>,
    pub symbol_name: Option<String>,
    pub index_run_id: Option<String>,
    pub parser_version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QualityJobCompletion {
    Succeeded {
        embedding: Box<ChunkEmbeddingRecord>,
    },
    Failed {
        error_summary: String,
    },
    SkippedStale,
    SkippedExcluded {
        reason: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QualityGenerationProgress {
    pub repository_id: String,
    pub generation_id: String,
    pub embeddable_chunks: usize,
    pub quality_eligible_chunks: usize,
    pub quality_ineligible_chunks: usize,
    pub quality_embedded_chunks: usize,
    pub pending_jobs: usize,
    pub running_jobs: usize,
    pub succeeded_jobs: usize,
    pub failed_jobs: usize,
    pub skipped_stale_jobs: usize,
    pub skipped_excluded_jobs: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QualityActivationReason {
    QualityComplete,
    GenerationNotLatest,
    QualityModelMissing,
    QualityDimensionMissing,
    NoEmbeddableChunks,
    QualityCoverageIncomplete,
    QualityJobsPending,
    QualityJobsFailed,
    QualityJobsStale,
    QualityBlocked,
    QualityStale,
}

impl QualityActivationReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::QualityComplete => "quality_complete",
            Self::GenerationNotLatest => "generation_not_latest",
            Self::QualityModelMissing => "quality_model_missing",
            Self::QualityDimensionMissing => "quality_dimension_missing",
            Self::NoEmbeddableChunks => "no_embeddable_chunks",
            Self::QualityCoverageIncomplete => "quality_coverage_incomplete",
            Self::QualityJobsPending => "quality_jobs_pending",
            Self::QualityJobsFailed => "quality_jobs_failed",
            Self::QualityJobsStale => "quality_jobs_stale",
            Self::QualityBlocked => "quality_blocked",
            Self::QualityStale => "quality_stale",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QualityActivationSummary {
    pub repository_id: String,
    pub generation_id: String,
    pub previous_status: SemanticLayerStatus,
    pub quality_status: SemanticLayerStatus,
    pub active_layer: SemanticLayer,
    pub reason: QualityActivationReason,
    pub progress: QualityGenerationProgress,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbeddingIndexMetadata {
    pub embedding_model: String,
    pub embedding_dimension: Option<usize>,
    pub chunks_embedded: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageExplorerSummary {
    pub repository_id: String,
    pub sqlite: SqliteStorageSummary,
    pub vector: VectorStorageProjection,
    pub warnings: Vec<StorageHealthRow>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SqliteStorageSummary {
    pub repositories: usize,
    pub files: usize,
    pub chunks: usize,
    pub symbols: usize,
    pub calls: usize,
    pub index_runs: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VectorStorageProjection {
    pub collection_name: String,
    pub embedding_model: String,
    pub embedding_dimension: Option<usize>,
    pub embeddable_chunks: usize,
    pub vector_backed_chunks: usize,
    pub excluded_chunks: usize,
    pub missing_vector_chunks: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpectedVectorPoint {
    pub vector_point_id: String,
    pub chunk_id: String,
    pub path: String,
    pub start_line: usize,
    pub end_line: usize,
    pub text_hash: String,
    pub embedding_model: Option<String>,
    pub embedding_dimension: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageHealthRow {
    pub status: StorageHealthStatus,
    pub label: String,
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorageHealthStatus {
    Ok,
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq)]
pub struct IndexCoverageSummary {
    pub repository_id: String,
    pub files: Vec<FileCoverageRow>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FileCoverageRow {
    pub path: String,
    pub language: String,
    pub chunks: usize,
    pub symbols: usize,
    pub calls: usize,
    pub embeddable_chunks: usize,
    pub vector_backed_chunks: usize,
    pub excluded_chunks: usize,
    pub status: FileCoverageStatus,
    pub detail: FileDetailSummary,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileCoverageStatus {
    Covered,
    MetadataOnly,
    Excluded,
    MissingVector,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FileDetailSummary {
    pub chunks: Vec<FileChunkDetailRow>,
    pub symbols: Vec<FileSymbolDetailRow>,
    pub calls: Vec<FileCallDetailRow>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileChunkDetailRow {
    pub kind: String,
    pub symbol: Option<String>,
    pub start_line: usize,
    pub end_line: usize,
    pub vector_status: ChunkVectorStatus,
    pub excluded_reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChunkVectorStatus {
    VectorBacked,
    MissingVector,
    Excluded,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileSymbolDetailRow {
    pub kind: String,
    pub qualified_name: String,
    pub parent_symbol_id: Option<String>,
    pub start_line: usize,
    pub end_line: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FileCallDetailRow {
    pub caller_symbol: String,
    pub callee_text: String,
    pub call_line: usize,
    pub confidence: f64,
    pub resolution_status: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolOutlineSummary {
    pub repository_id: String,
    pub symbols: Vec<SymbolOutlineRow>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolOutlineRow {
    pub id: String,
    pub parent_symbol_id: Option<String>,
    pub depth: usize,
    pub child_count: usize,
    pub kind: String,
    pub qualified_name: String,
    pub name: String,
    pub path: String,
    pub start_line: usize,
    pub end_line: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CallResolutionSummary {
    pub repository_id: String,
    pub buckets: Vec<CallResolutionBucket>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CallResolutionBucket {
    pub resolution_status: String,
    pub confidence_bucket: ConfidenceBucket,
    pub call_count: usize,
    pub average_confidence: f64,
    pub rows: Vec<CallResolutionEdgeRow>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CallResolutionEdgeRow {
    pub path: String,
    pub caller_symbol: String,
    pub callee_text: String,
    pub call_line: usize,
    pub confidence: f64,
    pub resolution_status: String,
    pub confidence_bucket: ConfidenceBucket,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbeddingCoverageSummary {
    pub repository_id: String,
    pub collection_name: String,
    pub configured_embedding_model: String,
    pub embedding_model: String,
    pub embedding_dimension: Option<usize>,
    pub total_chunks: usize,
    pub embeddable_chunks: usize,
    pub vector_backed_chunks: usize,
    pub missing_vector_chunks: usize,
    pub excluded_chunks: usize,
    pub latest_chunks_embedded: Option<usize>,
    pub exclusion_reasons: Vec<EmbeddingExclusionRow>,
    pub health: Vec<StorageHealthRow>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbeddingExclusionRow {
    pub reason: String,
    pub chunks: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexRunsTimelineSummary {
    pub repository_id: String,
    pub runs: Vec<IndexRunTimelineRow>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexRunTimelineRow {
    pub id: String,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub status: String,
    pub embedding_model: String,
    pub embedding_dimension: Option<usize>,
    pub files_seen: usize,
    pub files_indexed: usize,
    pub chunks_embedded: usize,
    pub error_summary: Option<String>,
    pub run_kind: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SemanticNeighborhoodSummary {
    pub repository_id: String,
    pub collection_name: String,
    pub embedding_model: String,
    pub rows: Vec<SemanticNeighborhoodRow>,
    pub health: Vec<StorageHealthRow>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SemanticNeighborhoodRow {
    pub vector_point_id: String,
    pub path: String,
    pub start_line: usize,
    pub end_line: usize,
    pub symbol_name: Option<String>,
    pub chunk_kind: String,
    pub language: String,
    pub score: Option<f64>,
    pub text_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrossStoreHealthSummary {
    pub repository_id: String,
    pub collection_name: String,
    pub rows: Vec<StorageHealthRow>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ConfidenceBucket {
    High,
    Medium,
    Low,
}

impl ConfidenceBucket {
    pub fn label(self) -> &'static str {
        match self {
            Self::High => "high",
            Self::Medium => "medium",
            Self::Low => "low",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SymbolSearchRow {
    pub id: String,
    pub name: String,
    pub qualified_name: String,
    pub kind: String,
    pub path: String,
    pub start_line: usize,
    pub end_line: usize,
    pub provenance: EvidenceProvenance,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CallSearchRow {
    pub callee_text: String,
    pub call_line: usize,
    pub confidence: f64,
    pub resolution_status: String,
    pub symbol_id: Option<String>,
    pub symbol_name: Option<String>,
    pub symbol_qualified_name: Option<String>,
    pub symbol_kind: Option<String>,
    pub path: Option<String>,
    pub start_line: Option<usize>,
    pub end_line: Option<usize>,
    pub provenance: EvidenceProvenance,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CallPath {
    pub hops: usize,
    pub min_confidence: f64,
    pub terminal_resolution_status: String,
    pub edges: Vec<CallPathEdge>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CallPathEdge {
    pub call_id: String,
    pub caller_symbol_id: String,
    pub caller_symbol_name: String,
    pub caller_symbol_qualified_name: String,
    pub caller_symbol_kind: String,
    pub caller_path: String,
    pub caller_start_line: usize,
    pub caller_end_line: usize,
    pub callee_text: String,
    pub callee_symbol_id: Option<String>,
    pub callee_symbol_name: Option<String>,
    pub callee_symbol_qualified_name: Option<String>,
    pub callee_symbol_kind: Option<String>,
    pub callee_path: Option<String>,
    pub callee_start_line: Option<usize>,
    pub callee_end_line: Option<usize>,
    pub call_line: usize,
    pub confidence: f64,
    pub resolution_status: String,
    pub provenance: EvidenceProvenance,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ContextPack {
    pub format: String,
    pub repository_id: String,
    pub query: String,
    pub focus_symbols: Vec<SymbolSearchRow>,
    pub direct_callers: Vec<CallSearchRow>,
    pub direct_callees: Vec<CallSearchRow>,
    pub files: Vec<String>,
    pub limits: ContextPackLimits,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ContextPackLimits {
    pub max_symbols: usize,
    pub max_callers: usize,
    pub max_callees: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EvidenceProvenance {
    pub content_hash: Option<String>,
    pub index_run_id: Option<String>,
    pub parser_version: Option<String>,
    pub indexed_at: Option<String>,
    pub embedding_model: Option<String>,
    pub embedding_dimension: Option<usize>,
    pub embedded_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileFreshnessSnapshot {
    pub path: String,
    pub content_hash: String,
    pub indexed_at: String,
    pub index_run_id: Option<String>,
    pub parser_version: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum EvidenceFreshness {
    Fresh,
    Stale,
    Deleted,
    Missing,
    Unknown,
}

impl EvidenceFreshness {
    pub fn label(self) -> &'static str {
        match self {
            Self::Fresh => "fresh",
            Self::Stale => "stale",
            Self::Deleted => "deleted",
            Self::Missing => "missing",
            Self::Unknown => "unknown",
        }
    }
}

pub fn freshness_for_hash(
    indexed_content_hash: Option<&str>,
    current_content_hash: Option<&str>,
) -> EvidenceFreshness {
    match (indexed_content_hash, current_content_hash) {
        (Some(indexed), Some(current)) if indexed == current => EvidenceFreshness::Fresh,
        (Some(_), Some(_)) => EvidenceFreshness::Stale,
        (Some(_), None) => EvidenceFreshness::Deleted,
        (None, Some(_)) => EvidenceFreshness::Missing,
        (None, None) => EvidenceFreshness::Unknown,
    }
}

fn env_path(upper: &str, legacy: &str) -> Option<PathBuf> {
    env_value(upper, legacy).map(PathBuf::from)
}

fn env_value(upper: &str, legacy: &str) -> Option<String> {
    env::var(upper).ok().or_else(|| env::var(legacy).ok())
}

#[derive(Debug)]
pub enum StoreError {
    Io(std::io::Error),
    Json(serde_json::Error),
    Sqlite(rusqlite::Error),
    SqliteVecRegistration(symdex_sqlite_vec::SqliteVecRegistrationError),
    InvalidVectorTableName(String),
    InvalidPointId(String),
    InvalidVectorSize(usize),
    InvalidLimit(usize),
    InconsistentVectorDimensions,
    EmbeddingDimensionChanged {
        repository_id: String,
        embedding_model: String,
        previous_dimension: usize,
        current_dimension: usize,
    },
    WriterBusy {
        owner: Option<WriterLeaseInfo>,
    },
    UnexpectedResponse(String),
}

impl Display for StoreError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "filesystem error: {error}"),
            Self::Json(error) => write!(f, "JSON error: {error}"),
            Self::Sqlite(error) => write!(f, "SQLite error: {error}"),
            Self::SqliteVecRegistration(error) => write!(f, "{error}"),
            Self::InvalidVectorTableName(name) => {
                write!(f, "invalid sqlite-vec table name `{name}`")
            }
            Self::InvalidPointId(id) => write!(f, "invalid vector point id source `{id}`"),
            Self::InvalidVectorSize(size) => write!(f, "invalid vector size `{size}`"),
            Self::InvalidLimit(limit) => write!(f, "invalid vector query limit `{limit}`"),
            Self::InconsistentVectorDimensions => {
                write!(f, "vector points have inconsistent vector dimensions")
            }
            Self::EmbeddingDimensionChanged {
                repository_id,
                embedding_model,
                previous_dimension,
                current_dimension,
            } => write!(
                f,
                "embedding dimension changed for repository `{repository_id}` and model `{embedding_model}`: previous={previous_dimension} current={current_dimension}; reset the collection or use a new model name before reindexing"
            ),
            Self::WriterBusy { owner } => {
                write!(f, "{}", writer_busy_message(owner.as_ref()))
            }
            Self::UnexpectedResponse(message) => {
                write!(f, "unexpected vector store response: {message}")
            }
        }
    }
}

pub fn writer_busy_message(owner: Option<&WriterLeaseInfo>) -> String {
    let Some(owner) = owner else {
        return "database writer busy: owner=<unknown>; stop the active writer or wait for it to finish"
            .to_owned();
    };
    format!(
        "database writer busy: owner={} pid={} repo={} operation={}; stop the active writer or wait for it to finish",
        owner.owner_kind,
        owner.pid,
        owner.repo_root.as_deref().unwrap_or("<none>"),
        owner.operation
    )
}

impl std::error::Error for StoreError {}

pub type Result<T> = std::result::Result<T, StoreError>;

fn collect_rows<T>(
    rows: rusqlite::MappedRows<'_, impl FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<T>>,
) -> Result<Vec<T>> {
    let mut values = Vec::new();
    for row in rows {
        values.push(row.map_err(StoreError::Sqlite)?);
    }
    Ok(values)
}

fn ref_identity(kind: RepositoryRefKind, ref_name: Option<&str>, head_oid: Option<&str>) -> String {
    match kind {
        RepositoryRefKind::Branch | RepositoryRefKind::Other => ref_name.unwrap_or("unknown"),
        RepositoryRefKind::Detached => head_oid.unwrap_or("unknown"),
        RepositoryRefKind::NonGit => "working-tree",
    }
    .to_owned()
}

fn upsert_repository_ref_in_transaction(
    transaction: &rusqlite::Transaction<'_>,
    record: &RepositoryRefRecord,
) -> Result<()> {
    transaction
        .execute(
            "INSERT INTO repository_refs (
               id, repository_id, ref_kind, ref_name, ref_identity, head_oid,
               is_current, last_seen_at, deleted_at, created_at, updated_at
             )
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
             ON CONFLICT(repository_id, ref_kind, ref_identity) DO UPDATE SET
               id = excluded.id,
               ref_name = excluded.ref_name,
               head_oid = COALESCE(excluded.head_oid, repository_refs.head_oid),
               is_current = excluded.is_current,
               last_seen_at = excluded.last_seen_at,
               deleted_at = excluded.deleted_at,
               updated_at = excluded.updated_at",
            params![
                record.id,
                record.repository_id,
                record.ref_kind,
                record.ref_name,
                record.ref_identity,
                record.head_oid,
                if record.is_current { 1i64 } else { 0i64 },
                record.last_seen_at,
                record.deleted_at,
                record.created_at,
                record.updated_at,
            ],
        )
        .map_err(StoreError::Sqlite)?;
    Ok(())
}

fn repository_ref_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<RepositoryRefRecord> {
    Ok(RepositoryRefRecord {
        id: row.get(0)?,
        repository_id: row.get(1)?,
        ref_kind: row.get(2)?,
        ref_name: row.get(3)?,
        ref_identity: row.get(4)?,
        head_oid: row.get(5)?,
        is_current: row.get::<_, i64>(6)? != 0,
        last_seen_at: row.get(7)?,
        deleted_at: row.get(8)?,
        created_at: row.get(9)?,
        updated_at: row.get(10)?,
    })
}

fn file_state_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<FileStateRecord> {
    Ok(FileStateRecord {
        path: row.get(0)?,
        content_hash: row.get(1)?,
    })
}

fn usize_count(value: i64, label: &str) -> Result<usize> {
    value
        .try_into()
        .map_err(|_| StoreError::UnexpectedResponse(format!("negative {label}")))
}

#[derive(Debug, Default)]
struct QualityJobStatusCounts {
    pending_jobs: usize,
    running_jobs: usize,
    succeeded_jobs: usize,
    failed_jobs: usize,
    skipped_stale_jobs: usize,
    skipped_excluded_jobs: usize,
}

fn quality_job_status_counts_in_transaction(
    transaction: &rusqlite::Transaction<'_>,
    repository_id: &str,
    generation_id: &str,
) -> Result<QualityJobStatusCounts> {
    let mut counts = QualityJobStatusCounts::default();
    let mut statement = transaction
        .prepare(
            "SELECT jobs.status, COUNT(*)
                             FROM quality_embedding_jobs AS jobs
                            WHERE jobs.repository_id = ?1
                                AND jobs.generation_id = ?2
                                AND EXISTS (
                                        SELECT 1
                                            FROM chunk_embeddings AS fast
                                         WHERE fast.repository_id = jobs.repository_id
                                             AND fast.generation_id = jobs.generation_id
                                             AND fast.file_id = jobs.file_id
                                             AND fast.chunk_id = jobs.chunk_id
                                             AND fast.semantic_layer = 'fast'
                                             AND fast.status = 'current'
                                             AND fast.content_hash = jobs.content_hash
                                             AND fast.text_hash = jobs.text_hash
                                )
                            GROUP BY jobs.status",
        )
        .map_err(StoreError::Sqlite)?;
    let rows = statement
        .query_map(params![repository_id, generation_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })
        .map_err(StoreError::Sqlite)?;
    for row in collect_rows(rows)? {
        let count = usize_count(row.1, "quality job status count")?;
        match row.0.as_str() {
            "pending" => counts.pending_jobs = count,
            "running" => counts.running_jobs = count,
            "succeeded" => counts.succeeded_jobs = count,
            "failed" => counts.failed_jobs = count,
            "skipped_stale" => counts.skipped_stale_jobs = count,
            "skipped_excluded" => counts.skipped_excluded_jobs = count,
            _ => {}
        }
    }
    Ok(counts)
}

fn quality_generation_with_status(
    generation: &SemanticGenerationRecord,
    quality_model: &str,
    quality_status: SemanticLayerStatus,
    updated_at: &str,
) -> SemanticGenerationRecord {
    let mut generation = generation.clone();
    generation.quality_model = Some(quality_model.to_owned());
    generation.quality_dimension = None;
    generation.quality_status = quality_status.as_str().to_owned();
    generation.quality_started_at = None;
    generation.quality_completed_at = None;
    generation.active_layer = SemanticLayer::Fast.as_str().to_owned();
    generation.updated_at = updated_at.to_owned();
    generation
}

fn mark_superseded_quality_jobs_stale_in_transaction(
    transaction: &rusqlite::Transaction<'_>,
    repository_id: &str,
    current_generation_id: &str,
    updated_at: &str,
) -> Result<usize> {
    transaction
        .execute(
            "UPDATE quality_embedding_jobs
             SET status = 'skipped_stale', updated_at = ?3
             WHERE repository_id = ?1
               AND generation_id != ?2
               AND status IN ('pending', 'running')",
            params![repository_id, current_generation_id, updated_at],
        )
        .map_err(StoreError::Sqlite)
}

fn upsert_quality_embedding_jobs_in_transaction(
    transaction: &rusqlite::Transaction<'_>,
    jobs: &[QualityEmbeddingJobRecord],
) -> Result<()> {
    let mut statement = transaction
        .prepare(
            "INSERT INTO quality_embedding_jobs (
               id, repository_id, generation_id, chunk_id, file_id, path,
               content_hash, text_hash, status, attempts, error_summary,
               created_at, updated_at
             )
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
             ON CONFLICT(repository_id, generation_id, chunk_id) DO UPDATE SET
               id = excluded.id,
               file_id = excluded.file_id,
               path = excluded.path,
               content_hash = excluded.content_hash,
               text_hash = excluded.text_hash,
               status = excluded.status,
               attempts = excluded.attempts,
               error_summary = excluded.error_summary,
               updated_at = excluded.updated_at",
        )
        .map_err(StoreError::Sqlite)?;
    for job in jobs {
        statement
            .execute(params![
                job.id,
                job.repository_id,
                job.generation_id,
                job.chunk_id,
                job.file_id,
                job.path,
                job.content_hash,
                job.text_hash,
                job.status,
                job.attempts as i64,
                job.error_summary,
                job.created_at,
                job.updated_at,
            ])
            .map_err(StoreError::Sqlite)?;
    }
    Ok(())
}

fn upsert_semantic_generation_in_transaction(
    transaction: &rusqlite::Transaction<'_>,
    generation: &SemanticGenerationRecord,
) -> Result<()> {
    transaction
        .execute(
            "INSERT INTO semantic_generations (
               id, repository_id, fast_model, fast_dimension, fast_completed_at,
               quality_model, quality_dimension, quality_status, quality_started_at,
               quality_completed_at, active_layer, files_seen, embeddable_chunks,
               fast_embedded_chunks, quality_embedded_chunks, created_at, updated_at
             )
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)
             ON CONFLICT(id) DO UPDATE SET
               repository_id = excluded.repository_id,
               fast_model = excluded.fast_model,
               fast_dimension = excluded.fast_dimension,
               fast_completed_at = excluded.fast_completed_at,
               quality_model = excluded.quality_model,
               quality_dimension = excluded.quality_dimension,
               quality_status = excluded.quality_status,
               quality_started_at = excluded.quality_started_at,
               quality_completed_at = excluded.quality_completed_at,
               active_layer = excluded.active_layer,
               files_seen = excluded.files_seen,
               embeddable_chunks = excluded.embeddable_chunks,
               fast_embedded_chunks = excluded.fast_embedded_chunks,
               quality_embedded_chunks = excluded.quality_embedded_chunks,
               created_at = excluded.created_at,
               updated_at = excluded.updated_at",
            params![
                generation.id,
                generation.repository_id,
                generation.fast_model,
                generation.fast_dimension as i64,
                generation.fast_completed_at,
                generation.quality_model,
                generation
                    .quality_dimension
                    .map(|dimension| dimension as i64),
                generation.quality_status,
                generation.quality_started_at,
                generation.quality_completed_at,
                generation.active_layer,
                generation.files_seen as i64,
                generation.embeddable_chunks as i64,
                generation.fast_embedded_chunks as i64,
                generation.quality_embedded_chunks as i64,
                generation.created_at,
                generation.updated_at,
            ],
        )
        .map_err(StoreError::Sqlite)?;
    Ok(())
}

fn upsert_chunk_embedding_in_transaction(
    transaction: &rusqlite::Transaction<'_>,
    embedding: &ChunkEmbeddingRecord,
) -> Result<()> {
    transaction
        .execute(
            "INSERT INTO chunk_embeddings (
               id, repository_id, file_id, chunk_id, semantic_layer, embedding_model,
               embedding_dimension, content_hash, text_hash, vector_table,
               vector_point_id, vector_store, vector_rowid,
               generation_id, embedded_at, status
             )
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, 'sqlite_vec', ?12, ?13, ?14, ?15)
             ON CONFLICT(chunk_id, semantic_layer, embedding_model, embedding_dimension)
             DO UPDATE SET
               id = excluded.id,
               repository_id = excluded.repository_id,
               file_id = excluded.file_id,
               content_hash = excluded.content_hash,
               text_hash = excluded.text_hash,
               vector_table = excluded.vector_table,
               vector_point_id = excluded.vector_point_id,
               vector_store = excluded.vector_store,
               vector_rowid = excluded.vector_rowid,
               generation_id = excluded.generation_id,
               embedded_at = excluded.embedded_at,
               status = excluded.status",
            params![
                embedding.id,
                embedding.repository_id,
                embedding.file_id,
                embedding.chunk_id,
                embedding.semantic_layer,
                embedding.embedding_model,
                embedding.embedding_dimension as i64,
                embedding.content_hash,
                embedding.text_hash,
                embedding.vector_table,
                embedding.vector_point_id,
                vector_rowid(&embedding.vector_point_id)?,
                embedding.generation_id,
                embedding.embedded_at,
                embedding.status,
            ],
        )
        .map_err(StoreError::Sqlite)?;
    Ok(())
}

fn current_fast_embedding_manifest(
    transaction: &rusqlite::Transaction<'_>,
    repository_id: &str,
    repository_ref_id: Option<&str>,
    fast_model: &str,
    fast_dimension: usize,
    upserted_embeddings: &[FastEmbeddingManifestRecord],
) -> Result<Vec<FastEmbeddingManifestRecord>> {
    use std::collections::BTreeMap;

    let mut manifest = BTreeMap::new();
    if let Some(repository_ref_id) = repository_ref_id {
        let mut statement = transaction
            .prepare(
                "SELECT chunk_embeddings.file_id, chunk_embeddings.chunk_id,
                        files.content_hash, chunks.text_hash, chunk_embeddings.vector_point_id
                 FROM chunk_embeddings
                 JOIN chunks ON chunk_embeddings.chunk_id = chunks.id
                 JOIN files ON chunk_embeddings.file_id = files.id
                           AND chunks.file_id = files.id
                 JOIN ref_files
                   ON ref_files.repository_id = files.repository_id
                  AND ref_files.repository_ref_id = ?4
                  AND ref_files.file_id = files.id
                  AND ref_files.path = files.path
                 WHERE files.repository_id = ?1
                   AND chunk_embeddings.repository_id = ?1
                   AND chunk_embeddings.semantic_layer = 'fast'
                   AND chunk_embeddings.embedding_model = ?2
                   AND chunk_embeddings.embedding_dimension = ?3
                   AND chunk_embeddings.vector_store = 'sqlite_vec'
                   AND chunk_embeddings.status = 'current'
                   AND chunks.excluded_reason IS NULL
                 ORDER BY chunk_embeddings.chunk_id",
            )
            .map_err(StoreError::Sqlite)?;
        let rows = statement
            .query_map(
                params![
                    repository_id,
                    fast_model,
                    fast_dimension as i64,
                    repository_ref_id
                ],
                fast_embedding_manifest_record,
            )
            .map_err(StoreError::Sqlite)?;
        for row in collect_rows(rows)? {
            manifest.insert(row.chunk_id.clone(), row);
        }
    } else {
        let mut statement = transaction
            .prepare(
                "SELECT chunk_embeddings.file_id, chunk_embeddings.chunk_id,
                        files.content_hash, chunks.text_hash, chunk_embeddings.vector_point_id
                 FROM chunk_embeddings
                 JOIN chunks ON chunk_embeddings.chunk_id = chunks.id
                 JOIN files ON chunk_embeddings.file_id = files.id
                           AND chunks.file_id = files.id
                 WHERE files.repository_id = ?1
                   AND chunk_embeddings.repository_id = ?1
                   AND chunk_embeddings.semantic_layer = 'fast'
                   AND chunk_embeddings.embedding_model = ?2
                   AND chunk_embeddings.embedding_dimension = ?3
                   AND chunk_embeddings.vector_store = 'sqlite_vec'
                   AND chunk_embeddings.status = 'current'
                   AND chunks.excluded_reason IS NULL
                 ORDER BY chunk_embeddings.chunk_id",
            )
            .map_err(StoreError::Sqlite)?;
        let rows = statement
            .query_map(
                params![repository_id, fast_model, fast_dimension as i64],
                fast_embedding_manifest_record,
            )
            .map_err(StoreError::Sqlite)?;
        for row in collect_rows(rows)? {
            manifest.insert(row.chunk_id.clone(), row);
        }
    }
    for row in upserted_embeddings {
        manifest.insert(row.chunk_id.clone(), row.clone());
    }
    Ok(manifest.into_values().collect())
}

fn fast_embedding_manifest_record(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<FastEmbeddingManifestRecord> {
    Ok(FastEmbeddingManifestRecord {
        file_id: row.get(0)?,
        chunk_id: row.get(1)?,
        content_hash: row.get(2)?,
        text_hash: row.get(3)?,
        vector_point_id: row.get(4)?,
    })
}

fn fast_semantic_generation_id(
    repository_id: &str,
    fast_model: &str,
    fast_dimension: usize,
    manifest: &[FastEmbeddingManifestRecord],
) -> String {
    let fast_dimension = fast_dimension.to_string();
    let mut parts = vec![
        "semantic-generation".to_owned(),
        repository_id.to_owned(),
        SemanticLayer::Fast.as_str().to_owned(),
        fast_model.to_owned(),
        fast_dimension,
    ];
    for row in manifest {
        parts.push(row.chunk_id.clone());
        parts.push(row.content_hash.clone());
        parts.push(row.text_hash.clone());
    }
    let refs = parts.iter().map(String::as_str).collect::<Vec<_>>();
    stable_id(&refs)
}

fn chunk_embedding_id(
    repository_id: &str,
    generation_id: &str,
    chunk_id: &str,
    semantic_layer: &str,
    embedding_model: &str,
    embedding_dimension: usize,
) -> String {
    let embedding_dimension = embedding_dimension.to_string();
    stable_id(&[
        "chunk-embedding",
        repository_id,
        generation_id,
        chunk_id,
        semantic_layer,
        embedding_model,
        &embedding_dimension,
    ])
}

fn semantic_generation_record(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<SemanticGenerationRecord> {
    Ok(SemanticGenerationRecord {
        id: row.get(0)?,
        repository_id: row.get(1)?,
        fast_model: row.get(2)?,
        fast_dimension: row.get::<_, i64>(3)? as usize,
        fast_completed_at: row.get(4)?,
        quality_model: row.get(5)?,
        quality_dimension: row
            .get::<_, Option<i64>>(6)?
            .map(|dimension| dimension as usize),
        quality_status: row.get(7)?,
        quality_started_at: row.get(8)?,
        quality_completed_at: row.get(9)?,
        active_layer: row.get(10)?,
        files_seen: row.get::<_, i64>(11)? as usize,
        embeddable_chunks: row.get::<_, i64>(12)? as usize,
        fast_embedded_chunks: row.get::<_, i64>(13)? as usize,
        quality_embedded_chunks: row.get::<_, i64>(14)? as usize,
        created_at: row.get(15)?,
        updated_at: row.get(16)?,
    })
}

fn chunk_embedding_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<ChunkEmbeddingRecord> {
    Ok(ChunkEmbeddingRecord {
        id: row.get(0)?,
        repository_id: row.get(1)?,
        file_id: row.get(2)?,
        chunk_id: row.get(3)?,
        semantic_layer: row.get(4)?,
        embedding_model: row.get(5)?,
        embedding_dimension: row.get::<_, i64>(6)? as usize,
        content_hash: row.get(7)?,
        text_hash: row.get(8)?,
        vector_table: row.get(9)?,
        vector_point_id: row.get(10)?,
        generation_id: row.get(11)?,
        embedded_at: row.get(12)?,
        status: row.get(13)?,
    })
}

fn quality_embedding_job_record(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<QualityEmbeddingJobRecord> {
    Ok(QualityEmbeddingJobRecord {
        id: row.get(0)?,
        repository_id: row.get(1)?,
        generation_id: row.get(2)?,
        chunk_id: row.get(3)?,
        file_id: row.get(4)?,
        path: row.get(5)?,
        content_hash: row.get(6)?,
        text_hash: row.get(7)?,
        status: row.get(8)?,
        attempts: row.get::<_, i64>(9)? as usize,
        error_summary: row.get(10)?,
        created_at: row.get(11)?,
        updated_at: row.get(12)?,
    })
}

fn quality_job_source_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<QualityJobSourceRow> {
    Ok(QualityJobSourceRow {
        job: QualityEmbeddingJobRecord {
            id: row.get(0)?,
            repository_id: row.get(1)?,
            generation_id: row.get(2)?,
            chunk_id: row.get(3)?,
            file_id: row.get(4)?,
            path: row.get(5)?,
            content_hash: row.get(6)?,
            text_hash: row.get(7)?,
            status: row.get(8)?,
            attempts: row.get::<_, i64>(9)? as usize,
            error_summary: row.get(10)?,
            created_at: row.get(11)?,
            updated_at: row.get(12)?,
        },
        current_file_id: row.get(13)?,
        current_content_hash: row.get(14)?,
        language: row.get(15)?,
        chunk_kind: row.get(16)?,
        current_text_hash: row.get(17)?,
        start_line: row.get::<_, Option<i64>>(18)?.map(|line| line as usize),
        end_line: row.get::<_, Option<i64>>(19)?.map(|line| line as usize),
        start_byte: row.get::<_, Option<i64>>(20)?.map(|byte| byte as usize),
        end_byte: row.get::<_, Option<i64>>(21)?.map(|byte| byte as usize),
        excluded_reason: row.get(22)?,
        symbol_id: row.get(23)?,
        symbol_name: row.get(24)?,
        index_run_id: row.get(25)?,
        parser_version: row.get(26)?,
    })
}

fn watcher_status_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<WatcherStatusRecord> {
    Ok(WatcherStatusRecord {
        repository_id: row.get(0)?,
        root_path: row.get(1)?,
        mode: row.get(2)?,
        owner_kind: row.get(3)?,
        owner_pid: row.get::<_, Option<i64>>(4)?.map(|pid| pid as i32),
        socket_path: row.get(5)?,
        state: row.get(6)?,
        started_at: row.get(7)?,
        updated_at: row.get(8)?,
        heartbeat_at: row.get(9)?,
        files_seen: row.get::<_, i64>(10)? as usize,
        queued_events: row.get::<_, i64>(11)? as usize,
        last_indexed_path: row.get(12)?,
        last_error: row.get(13)?,
        active_layer: row.get(14)?,
        quality_status: row.get(15)?,
        quality_pending_jobs: row.get::<_, i64>(16)? as usize,
        quality_running_jobs: row.get::<_, i64>(17)? as usize,
        quality_failed_jobs: row.get::<_, i64>(18)? as usize,
        quality_stale_jobs: row.get::<_, i64>(19)? as usize,
    })
}

fn watcher_client_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<WatcherClientRecord> {
    Ok(WatcherClientRecord {
        repository_id: row.get(0)?,
        client_id: row.get(1)?,
        client_kind: row.get(2)?,
        pid: row.get::<_, Option<i64>>(3)?.map(|pid| pid as i32),
        started_at: row.get(4)?,
        heartbeat_at: row.get(5)?,
        last_seen_at: row.get(6)?,
    })
}

fn call_search_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<CallSearchRow> {
    Ok(CallSearchRow {
        callee_text: row.get(0)?,
        call_line: row.get::<_, i64>(1)? as usize,
        confidence: row.get(2)?,
        resolution_status: row.get(3)?,
        symbol_id: row.get(4)?,
        symbol_name: row.get(5)?,
        symbol_qualified_name: row.get(6)?,
        symbol_kind: row.get(7)?,
        path: row.get(8)?,
        start_line: row.get::<_, Option<i64>>(9)?.map(|line| line as usize),
        end_line: row.get::<_, Option<i64>>(10)?.map(|line| line as usize),
        provenance: EvidenceProvenance {
            content_hash: row.get(11)?,
            index_run_id: row.get(12)?,
            parser_version: row.get(13)?,
            indexed_at: row.get(14)?,
            embedding_model: None,
            embedding_dimension: None,
            embedded_at: None,
        },
    })
}

fn test_search_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<TestSearchRow> {
    Ok(TestSearchRow {
        id: row.get(0)?,
        name: row.get(1)?,
        qualified_name: row.get(2)?,
        framework: row.get(3)?,
        language: row.get(4)?,
        path: row.get(5)?,
        start_line: row.get::<_, i64>(6)? as usize,
        end_line: row.get::<_, i64>(7)? as usize,
        provenance: EvidenceProvenance {
            content_hash: row.get(8)?,
            index_run_id: row.get(9)?,
            parser_version: row.get(10)?,
            indexed_at: row.get(11)?,
            embedding_model: None,
            embedding_dimension: None,
            embedded_at: None,
        },
    })
}

fn call_path_edge(row: &rusqlite::Row<'_>) -> rusqlite::Result<CallPathEdge> {
    Ok(CallPathEdge {
        call_id: row.get(0)?,
        callee_text: row.get(1)?,
        call_line: row.get::<_, i64>(2)? as usize,
        confidence: row.get(3)?,
        resolution_status: row.get(4)?,
        caller_symbol_id: row.get(5)?,
        caller_symbol_name: row.get(6)?,
        caller_symbol_qualified_name: row.get(7)?,
        caller_symbol_kind: row.get(8)?,
        caller_path: row.get(9)?,
        caller_start_line: row.get::<_, i64>(10)? as usize,
        caller_end_line: row.get::<_, i64>(11)? as usize,
        callee_symbol_id: row.get(12)?,
        callee_symbol_name: row.get(13)?,
        callee_symbol_qualified_name: row.get(14)?,
        callee_symbol_kind: row.get(15)?,
        callee_path: row.get(16)?,
        callee_start_line: row.get::<_, Option<i64>>(17)?.map(|line| line as usize),
        callee_end_line: row.get::<_, Option<i64>>(18)?.map(|line| line as usize),
        provenance: EvidenceProvenance {
            content_hash: row.get(19)?,
            index_run_id: row.get(20)?,
            parser_version: row.get(21)?,
            indexed_at: row.get(22)?,
            embedding_model: None,
            embedding_dimension: None,
            embedded_at: None,
        },
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SymbolRef {
    id: String,
}

struct TraceContext<'a> {
    target_query: &'a str,
    target_ids: &'a std::collections::BTreeSet<String>,
    edges_by_caller: &'a std::collections::BTreeMap<String, Vec<CallPathEdge>>,
}

fn trace_call_paths(
    current_symbol_id: &str,
    remaining_depth: usize,
    context: &TraceContext<'_>,
    visited: &mut std::collections::BTreeSet<String>,
    stack: &mut Vec<CallPathEdge>,
    paths: &mut Vec<CallPath>,
) {
    if remaining_depth == 0 || paths.len() >= 50 {
        return;
    }
    let Some(edges) = context.edges_by_caller.get(current_symbol_id) else {
        return;
    };
    for edge in edges {
        stack.push(edge.clone());
        if call_edge_reaches_target(edge, context.target_query, context.target_ids) {
            paths.push(call_path_from_edges(stack));
            stack.pop();
            if paths.len() >= 50 {
                return;
            }
            continue;
        }
        if let Some(next_symbol_id) = &edge.callee_symbol_id
            && visited.insert(next_symbol_id.clone())
        {
            trace_call_paths(
                next_symbol_id,
                remaining_depth.saturating_sub(1),
                context,
                visited,
                stack,
                paths,
            );
            visited.remove(next_symbol_id);
        }
        stack.pop();
    }
}

fn trace_reachable_call_paths(
    current_symbol_id: &str,
    remaining_depth: usize,
    edges_by_caller: &std::collections::BTreeMap<String, Vec<CallPathEdge>>,
    visited: &mut std::collections::BTreeSet<String>,
    stack: &mut Vec<CallPathEdge>,
    paths: &mut Vec<CallPath>,
) {
    if remaining_depth == 0 || paths.len() >= 50 {
        return;
    }
    let Some(edges) = edges_by_caller.get(current_symbol_id) else {
        return;
    };
    for edge in edges {
        stack.push(edge.clone());
        // Impact already reports direct callees separately, so this traversal
        // only materializes bounded transitive paths.
        if stack.len() > 1 {
            paths.push(call_path_from_edges(stack));
            if paths.len() >= 50 {
                stack.pop();
                return;
            }
        }
        if let Some(next_symbol_id) = &edge.callee_symbol_id
            && visited.insert(next_symbol_id.clone())
        {
            trace_reachable_call_paths(
                next_symbol_id,
                remaining_depth.saturating_sub(1),
                edges_by_caller,
                visited,
                stack,
                paths,
            );
            visited.remove(next_symbol_id);
        }
        stack.pop();
    }
}

fn call_edge_reaches_target(
    edge: &CallPathEdge,
    target_query: &str,
    target_ids: &std::collections::BTreeSet<String>,
) -> bool {
    edge.callee_symbol_id
        .as_ref()
        .is_some_and(|symbol_id| target_ids.contains(symbol_id))
        || edge.callee_text == target_query
        || edge
            .callee_symbol_name
            .as_ref()
            .is_some_and(|name| name == target_query)
        || edge
            .callee_symbol_qualified_name
            .as_ref()
            .is_some_and(|name| name == target_query)
}

fn call_path_from_edges(edges: &[CallPathEdge]) -> CallPath {
    CallPath {
        hops: edges.len(),
        min_confidence: edges
            .iter()
            .map(|edge| edge.confidence)
            .fold(1.0_f64, f64::min),
        terminal_resolution_status: edges
            .last()
            .map(|edge| edge.resolution_status.clone())
            .unwrap_or_else(|| "unresolved".to_owned()),
        edges: edges.to_vec(),
    }
}

fn embedding_index_metadata(row: &rusqlite::Row<'_>) -> rusqlite::Result<EmbeddingIndexMetadata> {
    Ok(EmbeddingIndexMetadata {
        embedding_model: row.get(0)?,
        embedding_dimension: row
            .get::<_, Option<i64>>(1)?
            .map(|dimension| dimension as usize),
        chunks_embedded: row.get::<_, i64>(2)? as usize,
    })
}

#[derive(Debug, Clone, Copy)]
struct ChunkProjectionCounts {
    embeddable_chunks: usize,
    vector_backed_chunks: usize,
    excluded_chunks: usize,
    missing_vector_chunks: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FileCoverageBaseRow {
    file_id: String,
    path: String,
    language: String,
    chunks: usize,
    symbols: usize,
    calls: usize,
    embeddable_chunks: usize,
    vector_backed_chunks: usize,
    excluded_chunks: usize,
    status: FileCoverageStatus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SymbolOutlineBaseRow {
    id: String,
    parent_symbol_id: Option<String>,
    kind: String,
    qualified_name: String,
    name: String,
    start_line: usize,
    end_line: usize,
    path: String,
}

fn storage_warnings(
    sqlite: &SqliteStorageSummary,
    vector: &VectorStorageProjection,
) -> Vec<StorageHealthRow> {
    let mut rows = Vec::new();
    if sqlite.repositories == 0 {
        rows.push(StorageHealthRow {
            status: StorageHealthStatus::Error,
            label: "repository_missing".to_owned(),
            detail: "SQLite has no repository row for the selected root.".to_owned(),
        });
    }
    if sqlite.index_runs == 0 {
        rows.push(StorageHealthRow {
            status: StorageHealthStatus::Warning,
            label: "no_index_runs".to_owned(),
            detail: "No index run metadata has been recorded yet.".to_owned(),
        });
    }
    if vector.missing_vector_chunks > 0 {
        rows.push(StorageHealthRow {
            status: StorageHealthStatus::Warning,
            label: "missing_vectors".to_owned(),
            detail: format!(
                "{} embeddable chunks do not have vector point IDs.",
                vector.missing_vector_chunks
            ),
        });
    }
    if vector.excluded_chunks > 0 {
        rows.push(StorageHealthRow {
            status: StorageHealthStatus::Warning,
            label: "excluded_chunks".to_owned(),
            detail: format!(
                "{} chunks are intentionally metadata-only.",
                vector.excluded_chunks
            ),
        });
    }
    if rows.is_empty() {
        rows.push(StorageHealthRow {
            status: StorageHealthStatus::Ok,
            label: "coverage_ok".to_owned(),
            detail: "SQLite metadata and vector-backed chunk counts are aligned.".to_owned(),
        });
    }
    rows
}

fn embedding_coverage_health(
    status: &RepositoryStatus,
    projection: &ChunkProjectionCounts,
    latest_embedding: Option<&EmbeddingIndexMetadata>,
    configured_embedding_model: &str,
) -> Vec<StorageHealthRow> {
    let mut rows = Vec::new();
    if projection.missing_vector_chunks > 0 {
        rows.push(StorageHealthRow {
            status: StorageHealthStatus::Warning,
            label: "missing_vectors".to_owned(),
            detail: format!(
                "{} embeddable chunks have no recorded vector point ID.",
                projection.missing_vector_chunks
            ),
        });
    }
    if projection.excluded_chunks > 0 {
        rows.push(StorageHealthRow {
            status: StorageHealthStatus::Warning,
            label: "excluded_chunks".to_owned(),
            detail: format!(
                "{} chunks are intentionally excluded from embeddings.",
                projection.excluded_chunks
            ),
        });
    }
    if projection.embeddable_chunks > 0 && latest_embedding.is_none() {
        rows.push(StorageHealthRow {
            status: StorageHealthStatus::Warning,
            label: "no_embedding_run".to_owned(),
            detail: "No successful semantic index run has recorded model/dimension metadata."
                .to_owned(),
        });
    }
    if let Some(latest) = latest_embedding {
        if latest.embedding_model != configured_embedding_model {
            rows.push(StorageHealthRow {
                status: StorageHealthStatus::Error,
                label: "model_drift".to_owned(),
                detail: format!(
                    "Configured model {configured_embedding_model} differs from latest indexed model {}.",
                    latest.embedding_model
                ),
            });
        }
        if latest.embedding_dimension.is_none() {
            rows.push(StorageHealthRow {
                status: StorageHealthStatus::Error,
                label: "dimension_missing".to_owned(),
                detail: "Latest successful semantic run did not record a vector dimension."
                    .to_owned(),
            });
        }
        if latest.chunks_embedded != projection.vector_backed_chunks {
            rows.push(StorageHealthRow {
                status: StorageHealthStatus::Warning,
                label: "run_count_mismatch".to_owned(),
                detail: format!(
                    "Latest run embedded {} chunks, while SQLite records {} vector-backed chunks.",
                    latest.chunks_embedded, projection.vector_backed_chunks
                ),
            });
        }
    }
    if status.chunks_indexed == 0 {
        rows.push(StorageHealthRow {
            status: StorageHealthStatus::Warning,
            label: "no_chunks".to_owned(),
            detail: "No SQLite chunks are indexed for this repository.".to_owned(),
        });
    }
    rows
}

fn semantic_neighborhood_health(rows: &[SemanticNeighborhoodRow]) -> Vec<StorageHealthRow> {
    if rows.is_empty() {
        return vec![StorageHealthRow {
            status: StorageHealthStatus::Warning,
            label: "no_vector_payloads".to_owned(),
            detail: "No vector-backed chunk metadata is recorded for semantic inspection."
                .to_owned(),
        }];
    }
    vec![StorageHealthRow {
        status: StorageHealthStatus::Ok,
        label: "metadata_only".to_owned(),
        detail: format!(
            "{} vector metadata metadata rows are available without source text.",
            rows.len()
        ),
    }]
}

fn file_coverage_status(
    chunks: usize,
    embeddable_chunks: usize,
    vector_backed_chunks: usize,
    excluded_chunks: usize,
) -> FileCoverageStatus {
    if chunks > 0 && excluded_chunks == chunks {
        FileCoverageStatus::Excluded
    } else if embeddable_chunks > vector_backed_chunks {
        FileCoverageStatus::MissingVector
    } else if embeddable_chunks > 0 && embeddable_chunks == vector_backed_chunks {
        FileCoverageStatus::Covered
    } else {
        FileCoverageStatus::MetadataOnly
    }
}

fn chunk_vector_status(has_vector: bool, excluded_reason: &Option<String>) -> ChunkVectorStatus {
    if excluded_reason.is_some() {
        ChunkVectorStatus::Excluded
    } else if has_vector {
        ChunkVectorStatus::VectorBacked
    } else {
        ChunkVectorStatus::MissingVector
    }
}

fn symbol_outline_rows(bases: Vec<SymbolOutlineBaseRow>) -> Vec<SymbolOutlineRow> {
    use std::collections::BTreeMap;

    let parents: BTreeMap<String, Option<String>> = bases
        .iter()
        .map(|row| (row.id.clone(), row.parent_symbol_id.clone()))
        .collect();
    let mut child_counts: BTreeMap<String, usize> = BTreeMap::new();
    for parent_id in bases.iter().filter_map(|row| row.parent_symbol_id.as_ref()) {
        *child_counts.entry(parent_id.clone()).or_default() += 1;
    }

    bases
        .into_iter()
        .map(|row| {
            let depth = symbol_depth(&row.parent_symbol_id, &parents);
            let child_count = child_counts.get(&row.id).copied().unwrap_or(0);
            SymbolOutlineRow {
                id: row.id,
                parent_symbol_id: row.parent_symbol_id,
                depth,
                child_count,
                kind: row.kind,
                qualified_name: row.qualified_name,
                name: row.name,
                path: row.path,
                start_line: row.start_line,
                end_line: row.end_line,
            }
        })
        .collect()
}

fn symbol_depth(
    parent_symbol_id: &Option<String>,
    parents: &std::collections::BTreeMap<String, Option<String>>,
) -> usize {
    let mut depth = 0;
    let mut current = parent_symbol_id.as_ref();
    while let Some(symbol_id) = current {
        depth += 1;
        if depth >= 16 {
            break;
        }
        current = parents.get(symbol_id).and_then(Option::as_ref);
    }
    depth
}

fn confidence_bucket(confidence: f64) -> ConfidenceBucket {
    if confidence >= 0.85 {
        ConfidenceBucket::High
    } else if confidence >= 0.5 {
        ConfidenceBucket::Medium
    } else {
        ConfidenceBucket::Low
    }
}

fn call_resolution_buckets(rows: Vec<CallResolutionEdgeRow>) -> Vec<CallResolutionBucket> {
    use std::collections::BTreeMap;

    let mut grouped: BTreeMap<(String, ConfidenceBucket), Vec<CallResolutionEdgeRow>> =
        BTreeMap::new();
    for row in rows {
        grouped
            .entry((row.resolution_status.clone(), row.confidence_bucket))
            .or_default()
            .push(row);
    }

    grouped
        .into_iter()
        .map(|((resolution_status, confidence_bucket), rows)| {
            let call_count = rows.len();
            let average_confidence = if call_count == 0 {
                0.0
            } else {
                rows.iter().map(|row| row.confidence).sum::<f64>() / call_count as f64
            };
            CallResolutionBucket {
                resolution_status,
                confidence_bucket,
                call_count,
                average_confidence,
                rows,
            }
        })
        .collect()
}

struct ProvenanceColumn {
    table: &'static str,
    name: &'static str,
    alter_sql: &'static str,
}

const COMPATIBILITY_COLUMNS: &[ProvenanceColumn] = &[
    ProvenanceColumn {
        table: "index_runs",
        name: "repository_ref_id",
        alter_sql: "ALTER TABLE index_runs ADD COLUMN repository_ref_id TEXT",
    },
    ProvenanceColumn {
        table: "index_runs",
        name: "parser_version",
        alter_sql: "ALTER TABLE index_runs ADD COLUMN parser_version TEXT",
    },
    ProvenanceColumn {
        table: "index_runs",
        name: "indexer_version",
        alter_sql: "ALTER TABLE index_runs ADD COLUMN indexer_version TEXT",
    },
    ProvenanceColumn {
        table: "index_runs",
        name: "run_kind",
        alter_sql: "ALTER TABLE index_runs ADD COLUMN run_kind TEXT NOT NULL DEFAULT 'manual'",
    },
    ProvenanceColumn {
        table: "files",
        name: "index_run_id",
        alter_sql: "ALTER TABLE files ADD COLUMN index_run_id TEXT",
    },
    ProvenanceColumn {
        table: "files",
        name: "parser_version",
        alter_sql: "ALTER TABLE files ADD COLUMN parser_version TEXT",
    },
    ProvenanceColumn {
        table: "symbols",
        name: "index_run_id",
        alter_sql: "ALTER TABLE symbols ADD COLUMN index_run_id TEXT",
    },
    ProvenanceColumn {
        table: "symbols",
        name: "parser_version",
        alter_sql: "ALTER TABLE symbols ADD COLUMN parser_version TEXT",
    },
    ProvenanceColumn {
        table: "chunks",
        name: "index_run_id",
        alter_sql: "ALTER TABLE chunks ADD COLUMN index_run_id TEXT",
    },
    ProvenanceColumn {
        table: "chunks",
        name: "parser_version",
        alter_sql: "ALTER TABLE chunks ADD COLUMN parser_version TEXT",
    },
    ProvenanceColumn {
        table: "chunks",
        name: "embedding_model",
        alter_sql: "ALTER TABLE chunks ADD COLUMN embedding_model TEXT",
    },
    ProvenanceColumn {
        table: "chunks",
        name: "embedding_dimension",
        alter_sql: "ALTER TABLE chunks ADD COLUMN embedding_dimension INTEGER",
    },
    ProvenanceColumn {
        table: "chunks",
        name: "embedded_at",
        alter_sql: "ALTER TABLE chunks ADD COLUMN embedded_at TEXT",
    },
    ProvenanceColumn {
        table: "calls",
        name: "index_run_id",
        alter_sql: "ALTER TABLE calls ADD COLUMN index_run_id TEXT",
    },
    ProvenanceColumn {
        table: "calls",
        name: "parser_version",
        alter_sql: "ALTER TABLE calls ADD COLUMN parser_version TEXT",
    },
    ProvenanceColumn {
        table: "chunk_embeddings",
        name: "vector_store",
        alter_sql: "ALTER TABLE chunk_embeddings ADD COLUMN vector_store TEXT NOT NULL DEFAULT 'legacy_qdrant'",
    },
    ProvenanceColumn {
        table: "chunk_embeddings",
        name: "vector_table",
        alter_sql: "ALTER TABLE chunk_embeddings ADD COLUMN vector_table TEXT",
    },
    ProvenanceColumn {
        table: "chunk_embeddings",
        name: "vector_point_id",
        alter_sql: "ALTER TABLE chunk_embeddings ADD COLUMN vector_point_id TEXT",
    },
    ProvenanceColumn {
        table: "chunk_embeddings",
        name: "vector_rowid",
        alter_sql: "ALTER TABLE chunk_embeddings ADD COLUMN vector_rowid INTEGER",
    },
];

const FILES_CONTENT_ADDRESSING_INDEXES: &str = r#"
CREATE INDEX IF NOT EXISTS idx_files_repository_path ON files(repository_id, path);
CREATE UNIQUE INDEX IF NOT EXISTS idx_files_repository_path_hash
    ON files(repository_id, path, content_hash);
"#;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS repositories (
  id TEXT PRIMARY KEY,
  root_path TEXT NOT NULL UNIQUE,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS repository_refs (
    id TEXT PRIMARY KEY,
    repository_id TEXT NOT NULL,
    ref_kind TEXT NOT NULL,
    ref_name TEXT,
    ref_identity TEXT NOT NULL,
    head_oid TEXT,
    is_current INTEGER NOT NULL DEFAULT 0,
    last_seen_at TEXT,
    deleted_at TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE(repository_id, ref_kind, ref_identity),
    CHECK(ref_kind IN ('branch', 'detached', 'other', 'non_git')),
    CHECK(is_current IN (0, 1)),
    FOREIGN KEY(repository_id) REFERENCES repositories(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_repository_refs_current
    ON repository_refs(repository_id, is_current, deleted_at);

CREATE INDEX IF NOT EXISTS idx_repository_refs_branch_name
    ON repository_refs(repository_id, ref_kind, ref_name);

CREATE TABLE IF NOT EXISTS ref_files (
    repository_ref_id TEXT NOT NULL,
    repository_id TEXT NOT NULL,
    path TEXT NOT NULL,
    file_id TEXT NOT NULL,
    indexed_at TEXT NOT NULL,
    index_run_id TEXT,
    PRIMARY KEY(repository_ref_id, path),
    FOREIGN KEY(repository_ref_id) REFERENCES repository_refs(id) ON DELETE CASCADE,
    FOREIGN KEY(repository_id) REFERENCES repositories(id) ON DELETE CASCADE,
    FOREIGN KEY(file_id) REFERENCES files(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_ref_files_repository_path
    ON ref_files(repository_id, path);

CREATE INDEX IF NOT EXISTS idx_ref_files_file
    ON ref_files(file_id);

CREATE TABLE IF NOT EXISTS index_runs (
  id TEXT PRIMARY KEY,
  repository_id TEXT NOT NULL,
    repository_ref_id TEXT,
  started_at TEXT NOT NULL,
  finished_at TEXT,
  status TEXT NOT NULL,
  embedding_model TEXT NOT NULL,
  embedding_dimension INTEGER,
  files_seen INTEGER DEFAULT 0,
  files_indexed INTEGER DEFAULT 0,
  chunks_embedded INTEGER DEFAULT 0,
  error_summary TEXT,
  parser_version TEXT,
  indexer_version TEXT,
    run_kind TEXT NOT NULL DEFAULT 'manual',
    FOREIGN KEY(repository_id) REFERENCES repositories(id) ON DELETE CASCADE,
    FOREIGN KEY(repository_ref_id) REFERENCES repository_refs(id) ON DELETE SET NULL
);

CREATE TABLE IF NOT EXISTS file_index_events (
  id TEXT PRIMARY KEY,
  index_run_id TEXT NOT NULL,
  repository_id TEXT NOT NULL,
  repository_ref_id TEXT,
  path TEXT NOT NULL,
  old_content_hash TEXT,
  new_content_hash TEXT,
  action TEXT NOT NULL,
  reason TEXT NOT NULL,
  status TEXT NOT NULL,
  error_summary TEXT,
  occurred_at TEXT NOT NULL,
  CHECK(action IN ('created', 'updated', 'deleted', 'skipped', 'failed')),
  CHECK(status IN ('success', 'failed', 'skipped')),
  FOREIGN KEY(index_run_id) REFERENCES index_runs(id) ON DELETE CASCADE,
  FOREIGN KEY(repository_id) REFERENCES repositories(id) ON DELETE CASCADE,
  FOREIGN KEY(repository_ref_id) REFERENCES repository_refs(id) ON DELETE SET NULL
);

CREATE TABLE IF NOT EXISTS watchers (
  repository_id TEXT PRIMARY KEY,
  root_path TEXT NOT NULL,
  mode TEXT NOT NULL,
  owner_kind TEXT NOT NULL,
  owner_pid INTEGER,
  socket_path TEXT,
  state TEXT NOT NULL,
  started_at TEXT,
  updated_at TEXT,
  heartbeat_at TEXT,
  files_seen INTEGER NOT NULL DEFAULT 0,
  queued_events INTEGER NOT NULL DEFAULT 0,
  last_indexed_path TEXT,
  last_error TEXT,
  active_layer TEXT,
  quality_status TEXT,
  quality_pending_jobs INTEGER NOT NULL DEFAULT 0,
  quality_running_jobs INTEGER NOT NULL DEFAULT 0,
  quality_failed_jobs INTEGER NOT NULL DEFAULT 0,
  quality_stale_jobs INTEGER NOT NULL DEFAULT 0,
  FOREIGN KEY(repository_id) REFERENCES repositories(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS watcher_clients (
  repository_id TEXT NOT NULL,
  client_id TEXT NOT NULL,
  client_kind TEXT NOT NULL,
  pid INTEGER,
  started_at TEXT NOT NULL,
  heartbeat_at TEXT NOT NULL,
  last_seen_at TEXT NOT NULL,
  PRIMARY KEY(repository_id, client_id),
  FOREIGN KEY(repository_id) REFERENCES repositories(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS files (
  id TEXT PRIMARY KEY,
  repository_id TEXT NOT NULL,
  path TEXT NOT NULL,
  language TEXT NOT NULL,
  content_hash TEXT NOT NULL,
  indexed_at TEXT NOT NULL,
  index_run_id TEXT,
  parser_version TEXT,
  FOREIGN KEY(repository_id) REFERENCES repositories(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS symbols (
  id TEXT PRIMARY KEY,
  file_id TEXT NOT NULL,
  parent_symbol_id TEXT,
  name TEXT NOT NULL,
  qualified_name TEXT,
  kind TEXT NOT NULL,
  signature TEXT,
  start_line INTEGER NOT NULL,
  end_line INTEGER NOT NULL,
  start_byte INTEGER NOT NULL,
  end_byte INTEGER NOT NULL,
  index_run_id TEXT,
  parser_version TEXT,
  FOREIGN KEY(file_id) REFERENCES files(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS chunks (
  id TEXT PRIMARY KEY,
  file_id TEXT NOT NULL,
  symbol_id TEXT,
  kind TEXT NOT NULL,
  text_hash TEXT NOT NULL,
  start_line INTEGER NOT NULL,
  end_line INTEGER NOT NULL,
  start_byte INTEGER NOT NULL,
  end_byte INTEGER NOT NULL,
  vector_point_id TEXT,
  excluded_reason TEXT,
  index_run_id TEXT,
  parser_version TEXT,
  embedding_model TEXT,
  embedding_dimension INTEGER,
  embedded_at TEXT,
  FOREIGN KEY(file_id) REFERENCES files(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS calls (
  id TEXT PRIMARY KEY,
  caller_symbol_id TEXT NOT NULL,
  callee_text TEXT NOT NULL,
  callee_symbol_id TEXT,
  call_line INTEGER NOT NULL,
  confidence REAL NOT NULL,
  resolution_status TEXT NOT NULL,
  index_run_id TEXT,
  parser_version TEXT
);

CREATE TABLE IF NOT EXISTS symbol_references (
    id TEXT PRIMARY KEY,
    file_id TEXT NOT NULL,
    source_symbol_id TEXT,
    target_symbol_id TEXT,
    reference_text TEXT NOT NULL,
    reference_kind TEXT NOT NULL,
    line INTEGER NOT NULL,
    confidence REAL NOT NULL,
    resolution_status TEXT NOT NULL,
    index_run_id TEXT,
    parser_version TEXT,
    CHECK(reference_kind IN (
        'import',
        'type_reference',
        'implementation',
        'attribute',
        'inheritance',
        'decorator',
        'config_link'
    )),
    CHECK(resolution_status IN (
        'resolved_exact',
        'resolved_local_candidate',
        'unresolved',
        'ambiguous'
    )),
    FOREIGN KEY(file_id) REFERENCES files(id) ON DELETE CASCADE,
    FOREIGN KEY(source_symbol_id) REFERENCES symbols(id) ON DELETE CASCADE,
    FOREIGN KEY(target_symbol_id) REFERENCES symbols(id) ON DELETE SET NULL
);

CREATE TABLE IF NOT EXISTS tests (
    id TEXT PRIMARY KEY,
    repository_id TEXT NOT NULL,
    file_id TEXT NOT NULL,
    symbol_id TEXT,
    name TEXT NOT NULL,
    qualified_name TEXT NOT NULL,
    framework TEXT NOT NULL,
    language TEXT NOT NULL,
    path TEXT NOT NULL,
    start_line INTEGER NOT NULL,
    end_line INTEGER NOT NULL,
    start_byte INTEGER NOT NULL,
    end_byte INTEGER NOT NULL,
    index_run_id TEXT,
    parser_version TEXT,
    indexed_at TEXT NOT NULL,
    FOREIGN KEY(repository_id) REFERENCES repositories(id) ON DELETE CASCADE,
    FOREIGN KEY(file_id) REFERENCES files(id) ON DELETE CASCADE,
    FOREIGN KEY(symbol_id) REFERENCES symbols(id) ON DELETE SET NULL
);

CREATE TABLE IF NOT EXISTS semantic_generations (
    id TEXT PRIMARY KEY,
    repository_id TEXT NOT NULL,
    fast_model TEXT NOT NULL,
    fast_dimension INTEGER NOT NULL,
    fast_completed_at TEXT NOT NULL,
    quality_model TEXT,
    quality_dimension INTEGER,
    quality_status TEXT NOT NULL,
    quality_started_at TEXT,
    quality_completed_at TEXT,
    active_layer TEXT NOT NULL,
    files_seen INTEGER NOT NULL,
    embeddable_chunks INTEGER NOT NULL,
    fast_embedded_chunks INTEGER NOT NULL,
    quality_embedded_chunks INTEGER NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    CHECK(quality_status IN (
        'missing',
        'fast_ready',
        'quality_pending',
        'quality_ready',
        'quality_stale',
        'quality_blocked',
        'quality_failed'
    )),
    CHECK(active_layer IN ('fast', 'quality')),
    FOREIGN KEY(repository_id) REFERENCES repositories(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS semantic_generation_refs (
    repository_ref_id TEXT PRIMARY KEY,
    repository_id TEXT NOT NULL,
    generation_id TEXT NOT NULL,
    linked_at TEXT NOT NULL,
    FOREIGN KEY(repository_ref_id) REFERENCES repository_refs(id) ON DELETE CASCADE,
    FOREIGN KEY(repository_id) REFERENCES repositories(id) ON DELETE CASCADE,
    FOREIGN KEY(generation_id) REFERENCES semantic_generations(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS chunk_embeddings (
    id TEXT PRIMARY KEY,
    repository_id TEXT NOT NULL,
    file_id TEXT NOT NULL,
    chunk_id TEXT NOT NULL,
    semantic_layer TEXT NOT NULL,
    embedding_model TEXT NOT NULL,
    embedding_dimension INTEGER NOT NULL,
    content_hash TEXT NOT NULL,
    text_hash TEXT NOT NULL,
    vector_table TEXT NOT NULL,
    vector_point_id TEXT NOT NULL,
    vector_store TEXT NOT NULL DEFAULT 'sqlite_vec',
    vector_rowid INTEGER,
    generation_id TEXT NOT NULL,
    embedded_at TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'current',
    UNIQUE(chunk_id, semantic_layer, embedding_model, embedding_dimension),
    CHECK(semantic_layer IN ('fast', 'quality')),
    CHECK(status IN ('current', 'stale', 'blocked', 'failed')),
    FOREIGN KEY(repository_id) REFERENCES repositories(id) ON DELETE CASCADE,
    FOREIGN KEY(file_id) REFERENCES files(id) ON DELETE CASCADE,
    FOREIGN KEY(chunk_id) REFERENCES chunks(id) ON DELETE CASCADE,
    FOREIGN KEY(generation_id) REFERENCES semantic_generations(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS vector_points (
  vector_store TEXT NOT NULL,
  vector_table TEXT NOT NULL,
  vector_rowid INTEGER NOT NULL,
  vector_point_id TEXT NOT NULL,
  repository_id TEXT NOT NULL,
  file_id TEXT NOT NULL,
  chunk_id TEXT NOT NULL,
  symbol_id TEXT,
  symbol_name TEXT,
  path TEXT NOT NULL,
  language TEXT NOT NULL,
  chunk_kind TEXT NOT NULL,
  start_line INTEGER NOT NULL,
  end_line INTEGER NOT NULL,
  text_hash TEXT NOT NULL,
  parser_version TEXT,
  content_hash TEXT,
  index_run_id TEXT,
  embedding_model TEXT,
  embedding_dimension INTEGER,
  indexed_at TEXT,
  PRIMARY KEY(vector_store, vector_table, vector_point_id),
  UNIQUE(vector_store, vector_table, vector_rowid)
);

CREATE TABLE IF NOT EXISTS quality_embedding_jobs (
    id TEXT PRIMARY KEY,
    repository_id TEXT NOT NULL,
    generation_id TEXT NOT NULL,
    chunk_id TEXT NOT NULL,
    file_id TEXT NOT NULL,
    path TEXT NOT NULL,
    content_hash TEXT NOT NULL,
    text_hash TEXT NOT NULL,
    status TEXT NOT NULL,
    attempts INTEGER NOT NULL DEFAULT 0,
    error_summary TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE(repository_id, generation_id, chunk_id),
    CHECK(status IN ('pending', 'running', 'succeeded', 'failed', 'skipped_stale', 'skipped_excluded')),
    CHECK(attempts >= 0),
    FOREIGN KEY(repository_id) REFERENCES repositories(id) ON DELETE CASCADE,
    FOREIGN KEY(generation_id) REFERENCES semantic_generations(id) ON DELETE CASCADE,
    FOREIGN KEY(chunk_id) REFERENCES chunks(id) ON DELETE CASCADE,
    FOREIGN KEY(file_id) REFERENCES files(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_files_repository_path ON files(repository_id, path);
CREATE UNIQUE INDEX IF NOT EXISTS idx_files_repository_path_hash
    ON files(repository_id, path, content_hash);
CREATE INDEX IF NOT EXISTS idx_chunks_file_id ON chunks(file_id);
CREATE INDEX IF NOT EXISTS idx_symbols_file_id ON symbols(file_id);
CREATE INDEX IF NOT EXISTS idx_symbols_name ON symbols(name);
CREATE INDEX IF NOT EXISTS idx_symbols_qualified_name ON symbols(qualified_name);
CREATE INDEX IF NOT EXISTS idx_calls_caller_symbol_id ON calls(caller_symbol_id);
CREATE INDEX IF NOT EXISTS idx_calls_callee_symbol_id ON calls(callee_symbol_id);
CREATE INDEX IF NOT EXISTS idx_symbol_references_file_kind ON symbol_references(file_id, reference_kind);
CREATE INDEX IF NOT EXISTS idx_symbol_references_source_kind ON symbol_references(source_symbol_id, reference_kind);
CREATE INDEX IF NOT EXISTS idx_symbol_references_target_kind ON symbol_references(target_symbol_id, reference_kind);
CREATE INDEX IF NOT EXISTS idx_symbol_references_kind_status ON symbol_references(reference_kind, resolution_status);
CREATE INDEX IF NOT EXISTS idx_tests_repository_name ON tests(repository_id, name);
CREATE INDEX IF NOT EXISTS idx_tests_repository_qualified_name ON tests(repository_id, qualified_name);
CREATE INDEX IF NOT EXISTS idx_tests_file_id ON tests(file_id);
CREATE INDEX IF NOT EXISTS idx_tests_symbol_id ON tests(symbol_id);
CREATE INDEX IF NOT EXISTS idx_index_runs_repository_status ON index_runs(repository_id, status, finished_at);
CREATE INDEX IF NOT EXISTS idx_index_runs_repository_model_status ON index_runs(repository_id, embedding_model, status, finished_at);
CREATE INDEX IF NOT EXISTS idx_file_index_events_run_path ON file_index_events(index_run_id, path);
CREATE INDEX IF NOT EXISTS idx_file_index_events_repository_path_time ON file_index_events(repository_id, path, occurred_at);
CREATE INDEX IF NOT EXISTS idx_file_index_events_repository_action_status ON file_index_events(repository_id, action, status, occurred_at);
CREATE INDEX IF NOT EXISTS idx_semantic_generations_repository_fast_completed ON semantic_generations(repository_id, fast_completed_at DESC, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_semantic_generation_refs_generation ON semantic_generation_refs(repository_id, generation_id);
CREATE INDEX IF NOT EXISTS idx_chunk_embeddings_repository_layer_model ON chunk_embeddings(repository_id, semantic_layer, embedding_model);
CREATE INDEX IF NOT EXISTS idx_chunk_embeddings_generation_chunk ON chunk_embeddings(generation_id, chunk_id);
CREATE INDEX IF NOT EXISTS idx_chunk_embeddings_repository_generation_layer_status ON chunk_embeddings(repository_id, generation_id, semantic_layer, status);
CREATE INDEX IF NOT EXISTS idx_vector_points_repository_table ON vector_points(repository_id, vector_store, vector_table);
CREATE INDEX IF NOT EXISTS idx_vector_points_chunk ON vector_points(chunk_id);
CREATE INDEX IF NOT EXISTS idx_quality_embedding_jobs_repository_generation_status ON quality_embedding_jobs(repository_id, generation_id, status, updated_at);
CREATE INDEX IF NOT EXISTS idx_watchers_state_heartbeat ON watchers(state, heartbeat_at);
CREATE INDEX IF NOT EXISTS idx_watcher_clients_repository_heartbeat ON watcher_clients(repository_id, heartbeat_at);
"#;

pub fn current_timestamp() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    seconds.to_string()
}

fn timestamp() -> String {
    current_timestamp()
}

fn timestamp_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    use rusqlite::{Connection, params};
    use symdex_core::{
        RepositoryRefKind, RepositoryRefSnapshot, SemanticLayer, SemanticLayerStatus, stable_id,
    };

    use crate::{
        CallRecord, ChunkEmbeddingRecord, ChunkRecord, ChunkVectorStatus, ConfidenceBucket,
        FastEmbeddingManifestRecord, FastSemanticGenerationInput, FileCoverageStatus,
        FileIndexEventRecord, FileRecord, PointPayload, QualityActivationReason,
        QualityEmbeddingJobRecord, QualityJobCompletion, RepositoryRecord,
        SemanticGenerationRecord, SqliteStore, SqliteVectorStore, StorageHealthStatus, StoreConfig,
        StoreError, SymbolRecord, SymbolReferenceRecord, TestRecord, VectorPoint,
        WatcherClientRecord, WatcherStatusRecord, WriterLease, WriterLeaseInfo, WriterLeaseKind,
        WriterLeaseRequest, validate_vector_table_name, vector_point_id, vector_rowid,
        vector_table_name,
    };

    #[test]
    fn collection_name_is_deterministic_and_safe() {
        assert_eq!(
            vector_table_name("Repo-ID_123", "nomic-embed-text:latest"),
            "symdex_repo_id_123_nomic_embed_text_latest"
        );
    }

    #[test]
    fn validates_collection_names() {
        assert!(validate_vector_table_name("symdex_repo_model").is_ok());
        assert!(validate_vector_table_name("").is_err());
        assert!(validate_vector_table_name("../bad").is_err());
    }

    #[test]
    fn vector_point_id_formats_stable_hash_as_uuid() {
        assert_eq!(
            vector_point_id("0123456789abcdeffedcba9876543210").expect("point id should format"),
            "01234567-89ab-cdef-fedc-ba9876543210"
        );
        assert!(vector_point_id("not-hex").is_err());
    }

    #[test]
    fn vector_rowid_is_stable_and_positive() {
        assert_eq!(
            vector_rowid("01234567-89ab-cdef-fedc-ba9876543210").expect("rowid should derive"),
            81_985_529_216_486_895
        );
        assert_eq!(
            vector_rowid("point-chunk-1").expect("arbitrary point ids should derive"),
            vector_rowid("point-chunk-1").expect("arbitrary point ids should derive")
        );
        assert!(vector_rowid("").is_err());
    }

    #[test]
    fn sqlite_vec_health_check_reports_version() {
        let db = TestDb::new("sqlite-vec-health");
        let vector_store = SqliteVectorStore::new(&db.config()).expect("vector store should open");
        let version = vector_store
            .health_check()
            .expect("sqlite-vec should report a version");
        assert!(version.starts_with("v"));
    }

    #[test]
    fn sqlite_open_initializes_new_database_in_wal_mode() {
        let db = TestDb::new("sqlite-new-wal");
        let store = SqliteStore::open(&db.config()).expect("store should open");

        let journal_mode: String = store
            .connection
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .expect("journal mode should be readable");

        assert_eq!(journal_mode, "wal");
    }

    #[test]
    fn sqlite_open_existing_database_does_not_change_journal_mode() {
        let db = TestDb::new("sqlite-existing-journal");
        let config = db.config();
        let connection = Connection::open(&config.sqlite_path).expect("raw database should open");
        connection
            .execute_batch(
                "PRAGMA journal_mode = DELETE;
                 CREATE TABLE existing_table (id INTEGER PRIMARY KEY);",
            )
            .expect("raw database should be initialized");
        drop(connection);

        let store = SqliteStore::open(&config).expect("store should open");
        let journal_mode: String = store
            .connection
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .expect("journal mode should be readable");

        assert_eq!(journal_mode, "delete");
    }

    #[test]
    fn writer_lease_excludes_second_writer_for_same_database() {
        let db = TestDb::new("writer-lease-exclusive");
        let config = db.config();
        let _lease = WriterLease::acquire(
            &config,
            WriterLeaseRequest::new(WriterLeaseKind::ManualIndex, "index")
                .for_repo("repo", "/tmp/repo"),
        )
        .expect("first writer should acquire lease");

        let error = WriterLease::acquire(
            &config,
            WriterLeaseRequest::new(WriterLeaseKind::QualityIndex, "index-quality"),
        )
        .expect_err("second writer should fail");

        match error {
            StoreError::WriterBusy { owner } => {
                let owner = owner.expect("owner metadata should be available");
                assert_eq!(owner.owner_kind, "manual_index");
                assert_eq!(owner.operation, "index");
                assert_eq!(owner.repo_root.as_deref(), Some("/tmp/repo"));
            }
            other => panic!("expected writer busy, got {other}"),
        }
    }

    #[test]
    fn writer_lease_allows_different_database_paths() {
        let first = TestDb::new("writer-lease-first");
        let second = TestDb::new("writer-lease-second");

        let _first_lease = WriterLease::acquire(
            &first.config(),
            WriterLeaseRequest::new(WriterLeaseKind::ManualIndex, "index"),
        )
        .expect("first database should acquire lease");
        let _second_lease = WriterLease::acquire(
            &second.config(),
            WriterLeaseRequest::new(WriterLeaseKind::ManualIndex, "index"),
        )
        .expect("second database should acquire lease independently");
    }

    #[test]
    fn writer_lease_drop_allows_reacquisition_and_replaces_metadata() {
        let db = TestDb::new("writer-lease-reacquire");
        let config = db.config();
        {
            let _lease = WriterLease::acquire(
                &config,
                WriterLeaseRequest::new(WriterLeaseKind::ManualIndex, "index"),
            )
            .expect("first lease should acquire");
        }

        let _lease = WriterLease::acquire(
            &config,
            WriterLeaseRequest::new(WriterLeaseKind::QualityIndex, "index-quality"),
        )
        .expect("lease should reacquire after drop");
        let info = WriterLeaseInfo::read_for(&config)
            .expect("lease metadata should read")
            .expect("lease metadata should exist");

        assert_eq!(info.owner_kind, "quality_index");
        assert_eq!(info.operation, "index-quality");
    }

    #[test]
    fn sync_repository_ref_tracks_current_branch_and_deleted_branches() {
        let db = TestDb::new("repository-ref-sync");
        let store = SqliteStore::open(&db.config()).expect("store should open");
        store.migrate().expect("migration should run");
        store
            .upsert_repository(&RepositoryRecord {
                id: "repo".to_owned(),
                root_path: "/tmp/repo".to_owned(),
            })
            .expect("repository should be stored");

        let first = RepositoryRefSnapshot {
            id: "ref-main".to_owned(),
            repository_id: "repo".to_owned(),
            kind: RepositoryRefKind::Branch,
            name: Some("main".to_owned()),
            head_oid: Some("0123456789abcdef0123456789abcdef01234567".to_owned()),
            local_branches: vec!["main".to_owned(), "feature".to_owned()],
        };
        let summary = store.sync_repository_ref(&first).expect("refs should sync");
        assert_eq!(summary.current.ref_name.as_deref(), Some("main"));
        assert_eq!(summary.deleted_refs, 0);

        let second = RepositoryRefSnapshot {
            id: "ref-main".to_owned(),
            repository_id: "repo".to_owned(),
            kind: RepositoryRefKind::Branch,
            name: Some("main".to_owned()),
            head_oid: Some("fedcba9876543210fedcba9876543210fedcba98".to_owned()),
            local_branches: vec!["main".to_owned()],
        };
        let summary = store
            .sync_repository_ref(&second)
            .expect("refs should sync after branch deletion");
        let current = store
            .current_repository_ref("repo")
            .expect("current ref should load")
            .expect("current ref should exist");

        assert_eq!(summary.deleted_refs, 1);
        assert_eq!(current.ref_name.as_deref(), Some("main"));
        assert_eq!(
            current.head_oid.as_deref(),
            Some("fedcba9876543210fedcba9876543210fedcba98")
        );
    }

    #[test]
    fn ref_files_track_paths_per_repository_ref() {
        let db = TestDb::new("ref-files");
        let mut store = SqliteStore::open(&db.config()).expect("store should open");
        store.migrate().expect("migration should run");
        store
            .upsert_repository(&RepositoryRecord {
                id: "repo".to_owned(),
                root_path: "/tmp/repo".to_owned(),
            })
            .expect("repository should be stored");
        let main = RepositoryRefSnapshot {
            id: stable_id(&["repository-ref", "repo", "branch", "main"]),
            repository_id: "repo".to_owned(),
            kind: RepositoryRefKind::Branch,
            name: Some("main".to_owned()),
            head_oid: Some("0123456789abcdef0123456789abcdef01234567".to_owned()),
            local_branches: vec!["main".to_owned(), "feature".to_owned()],
        };
        let feature = RepositoryRefSnapshot {
            id: stable_id(&["repository-ref", "repo", "branch", "feature"]),
            repository_id: "repo".to_owned(),
            kind: RepositoryRefKind::Branch,
            name: Some("feature".to_owned()),
            head_oid: Some("fedcba9876543210fedcba9876543210fedcba98".to_owned()),
            local_branches: vec!["main".to_owned(), "feature".to_owned()],
        };

        store.sync_repository_ref(&main).expect("main should sync");
        store
            .replace_file_facts_for_ref_with_tests(
                &main.id,
                &sample_file_at("main-file", "src/lib.rs", "hash-main"),
                &[],
                &[],
                &[],
                &[],
            )
            .expect("main file should persist");
        store
            .sync_repository_ref(&feature)
            .expect("feature should sync");
        store
            .replace_file_facts_for_ref_with_tests(
                &feature.id,
                &sample_file_at("feature-file", "src/feature.rs", "hash-feature"),
                &[],
                &[],
                &[],
                &[],
            )
            .expect("feature file should persist");

        assert_eq!(
            store
                .ref_file_paths(&main.id)
                .expect("main paths should load"),
            vec!["src/lib.rs".to_owned()]
        );
        assert_eq!(
            store
                .ref_file_paths(&feature.id)
                .expect("feature paths should load"),
            vec!["src/feature.rs".to_owned()]
        );

        let removed = store
            .remove_missing_ref_files(&feature.id, &[])
            .expect("feature manifest cleanup should run");
        assert_eq!(removed, 1);
        assert_eq!(
            store
                .ref_file_paths(&main.id)
                .expect("main paths should remain"),
            vec!["src/lib.rs".to_owned()]
        );
        assert!(
            store
                .ref_file_paths(&feature.id)
                .expect("feature paths should be empty")
                .is_empty()
        );

        let removed_files = store
            .remove_unreferenced_missing_files("repo", &[])
            .expect("unreferenced file cleanup should run");
        assert_eq!(removed_files, 1);
        let status = store.repository_status("repo").expect("status should load");
        assert_eq!(status.files_indexed, 1);
        assert_eq!(
            store
                .ref_file_paths(&main.id)
                .expect("main paths should still remain"),
            vec!["src/lib.rs".to_owned()]
        );
    }

    #[test]
    fn freshness_snapshots_can_be_scoped_to_repository_ref() {
        let db = TestDb::new("ref-freshness-snapshots");
        let mut store = SqliteStore::open(&db.config()).expect("store should open");
        store.migrate().expect("migration should run");
        store
            .upsert_repository(&RepositoryRecord {
                id: "repo".to_owned(),
                root_path: "/tmp/repo".to_owned(),
            })
            .expect("repository should be stored");
        let main = RepositoryRefSnapshot {
            id: stable_id(&["repository-ref", "repo", "branch", "main"]),
            repository_id: "repo".to_owned(),
            kind: RepositoryRefKind::Branch,
            name: Some("main".to_owned()),
            head_oid: Some("0123456789abcdef0123456789abcdef01234567".to_owned()),
            local_branches: vec!["main".to_owned(), "feature".to_owned()],
        };
        let feature = RepositoryRefSnapshot {
            id: stable_id(&["repository-ref", "repo", "branch", "feature"]),
            repository_id: "repo".to_owned(),
            kind: RepositoryRefKind::Branch,
            name: Some("feature".to_owned()),
            head_oid: Some("fedcba9876543210fedcba9876543210fedcba98".to_owned()),
            local_branches: vec!["main".to_owned(), "feature".to_owned()],
        };
        let old = sample_file_at("old-file", "src/lib.rs", "hash-old");
        let current = sample_file_at("current-file", "src/lib.rs", "hash-current");
        store
            .sync_repository_ref(&feature)
            .expect("feature should sync");
        store
            .replace_file_facts_for_ref_with_tests(&feature.id, &old, &[], &[], &[], &[])
            .expect("old ref snapshot should persist");
        store.sync_repository_ref(&main).expect("main should sync");
        store
            .replace_file_facts_for_ref_with_tests(&main.id, &current, &[], &[], &[], &[])
            .expect("current ref snapshot should persist");

        let mut repository_hashes = store
            .indexed_file_freshness_snapshots("repo")
            .expect("repo snapshots should load")
            .into_iter()
            .map(|row| row.content_hash)
            .collect::<Vec<_>>();
        repository_hashes.sort();
        assert_eq!(
            repository_hashes,
            vec!["hash-current".to_owned(), "hash-old".to_owned()]
        );
        assert_eq!(
            store
                .indexed_file_freshness_snapshots_for_ref("repo", &main.id)
                .expect("ref snapshots should load")
                .into_iter()
                .map(|row| row.content_hash)
                .collect::<Vec<_>>(),
            vec!["hash-current".to_owned()]
        );
    }

    #[test]
    fn scoped_queries_filter_through_ref_files() {
        let db = TestDb::new("ref-scoped-queries");
        let mut store = SqliteStore::open(&db.config()).expect("store should open");
        store.migrate().expect("migration should run");
        store
            .upsert_repository(&RepositoryRecord {
                id: "repo".to_owned(),
                root_path: "/tmp/repo".to_owned(),
            })
            .expect("repository should be stored");
        let main = RepositoryRefSnapshot {
            id: stable_id(&["repository-ref", "repo", "branch", "main"]),
            repository_id: "repo".to_owned(),
            kind: RepositoryRefKind::Branch,
            name: Some("main".to_owned()),
            head_oid: Some("0123456789abcdef0123456789abcdef01234567".to_owned()),
            local_branches: vec!["main".to_owned(), "feature".to_owned()],
        };
        let feature = RepositoryRefSnapshot {
            id: stable_id(&["repository-ref", "repo", "branch", "feature"]),
            repository_id: "repo".to_owned(),
            kind: RepositoryRefKind::Branch,
            name: Some("feature".to_owned()),
            head_oid: Some("fedcba9876543210fedcba9876543210fedcba98".to_owned()),
            local_branches: vec!["main".to_owned(), "feature".to_owned()],
        };

        store.sync_repository_ref(&main).expect("refs should sync");
        let main_symbols = vec![
            sample_symbol_in_file("main-caller", "main-file", "caller", "main::caller"),
            sample_symbol_in_file("main-helper", "main-file", "helper", "main::helper"),
        ];
        let main_calls = vec![sample_call(
            "main-call",
            "main-caller",
            "helper",
            Some("main-helper"),
            4,
        )];
        store
            .replace_file_facts_for_ref_with_tests(
                &main.id,
                &sample_file_at("main-file", "src/main.rs", "hash-main"),
                &main_symbols,
                &[],
                &main_calls,
                &[],
            )
            .expect("main facts should persist");

        store
            .sync_repository_ref(&feature)
            .expect("feature ref should sync");
        let feature_symbols = vec![
            sample_symbol_in_file(
                "feature-caller",
                "feature-file",
                "caller",
                "feature::caller",
            ),
            sample_symbol_in_file(
                "feature-helper",
                "feature-file",
                "helper",
                "feature::helper",
            ),
        ];
        let feature_calls = vec![sample_call(
            "feature-call",
            "feature-caller",
            "helper",
            Some("feature-helper"),
            8,
        )];
        store
            .replace_file_facts_for_ref_with_tests(
                &feature.id,
                &sample_file_at("feature-file", "src/feature.rs", "hash-feature"),
                &feature_symbols,
                &[],
                &feature_calls,
                &[],
            )
            .expect("feature facts should persist");

        assert!(
            store
                .repository_has_ref_file_manifests("repo")
                .expect("manifest check should run")
        );
        let main_symbols = store
            .find_symbols_for_ref("repo", &main.id, "helper")
            .expect("main symbol search should run");
        assert_eq!(main_symbols.len(), 1);
        assert_eq!(main_symbols[0].qualified_name, "main::helper");
        let feature_symbols = store
            .find_symbols_for_ref("repo", &feature.id, "helper")
            .expect("feature symbol search should run");
        assert_eq!(feature_symbols.len(), 1);
        assert_eq!(feature_symbols[0].qualified_name, "feature::helper");

        let main_callers = store
            .callers_for_ref("repo", &main.id, "main::helper")
            .expect("main callers should run");
        assert_eq!(main_callers.len(), 1);
        assert_eq!(
            main_callers[0].symbol_qualified_name.as_deref(),
            Some("main::caller")
        );
        let feature_callers = store
            .callers_for_ref("repo", &feature.id, "feature::helper")
            .expect("feature callers should run");
        assert_eq!(feature_callers.len(), 1);
        assert_eq!(
            feature_callers[0].symbol_qualified_name.as_deref(),
            Some("feature::caller")
        );
        assert!(
            store
                .callers_for_ref("repo", &main.id, "feature::helper")
                .expect("cross-ref callers should run")
                .is_empty()
        );

        let pack = store
            .context_pack_for_ref("repo", &main.id, "helper", 5)
            .expect("scoped context pack should build");
        assert_eq!(pack.focus_symbols[0].qualified_name, "main::helper");
        assert_eq!(pack.direct_callers.len(), 1);
        assert_eq!(pack.files, vec!["src/main.rs".to_owned()]);
        assert!(pack.notes.contains(&"repository_ref_scoped".to_owned()));
    }

    #[test]
    fn ref_scoped_queries_preserve_same_path_different_content_snapshots() {
        let db = TestDb::new("ref-same-path-snapshots");
        let mut store = SqliteStore::open(&db.config()).expect("store should open");
        store.migrate().expect("migration should run");
        store
            .upsert_repository(&RepositoryRecord {
                id: "repo".to_owned(),
                root_path: "/tmp/repo".to_owned(),
            })
            .expect("repository should be stored");
        let main = RepositoryRefSnapshot {
            id: stable_id(&["repository-ref", "repo", "branch", "main"]),
            repository_id: "repo".to_owned(),
            kind: RepositoryRefKind::Branch,
            name: Some("main".to_owned()),
            head_oid: Some("0123456789abcdef0123456789abcdef01234567".to_owned()),
            local_branches: vec!["main".to_owned(), "feature".to_owned()],
        };
        let feature = RepositoryRefSnapshot {
            id: stable_id(&["repository-ref", "repo", "branch", "feature"]),
            repository_id: "repo".to_owned(),
            kind: RepositoryRefKind::Branch,
            name: Some("feature".to_owned()),
            head_oid: Some("fedcba9876543210fedcba9876543210fedcba98".to_owned()),
            local_branches: vec!["main".to_owned(), "feature".to_owned()],
        };

        store
            .sync_repository_ref(&main)
            .expect("main ref should sync");
        let main_symbols = vec![
            sample_symbol_in_file("main-caller", "main-file-hash", "caller", "main::caller"),
            sample_symbol_in_file("main-helper", "main-file-hash", "helper", "main::helper"),
        ];
        store
            .replace_file_facts_for_ref_with_tests(
                &main.id,
                &sample_file_at("main-file-hash", "src/lib.rs", "hash-main"),
                &main_symbols,
                &[],
                &[sample_call(
                    "main-call",
                    "main-caller",
                    "helper",
                    Some("main-helper"),
                    3,
                )],
                &[],
            )
            .expect("main snapshot should persist");

        store
            .sync_repository_ref(&feature)
            .expect("feature ref should sync");
        let feature_symbols = vec![
            sample_symbol_in_file(
                "feature-caller",
                "feature-file-hash",
                "caller",
                "feature::caller",
            ),
            sample_symbol_in_file(
                "feature-helper",
                "feature-file-hash",
                "helper",
                "feature::helper",
            ),
        ];
        store
            .replace_file_facts_for_ref_with_tests(
                &feature.id,
                &sample_file_at("feature-file-hash", "src/lib.rs", "hash-feature"),
                &feature_symbols,
                &[],
                &[sample_call(
                    "feature-call",
                    "feature-caller",
                    "helper",
                    Some("feature-helper"),
                    7,
                )],
                &[],
            )
            .expect("feature snapshot should persist");

        let status = store.repository_status("repo").expect("status should load");
        assert_eq!(status.files_indexed, 2);

        let main_symbols = store
            .find_symbols_for_ref("repo", &main.id, "helper")
            .expect("main symbols should load");
        assert_eq!(main_symbols.len(), 1);
        assert_eq!(main_symbols[0].qualified_name, "main::helper");
        assert_eq!(
            main_symbols[0].provenance.content_hash,
            Some("hash-main".to_owned())
        );

        let feature_symbols = store
            .find_symbols_for_ref("repo", &feature.id, "helper")
            .expect("feature symbols should load");
        assert_eq!(feature_symbols.len(), 1);
        assert_eq!(feature_symbols[0].qualified_name, "feature::helper");
        assert_eq!(
            feature_symbols[0].provenance.content_hash,
            Some("hash-feature".to_owned())
        );

        assert!(
            store
                .callers_for_ref("repo", &main.id, "feature::helper")
                .expect("main should not see feature callers")
                .is_empty()
        );

        store
            .remove_missing_ref_files(&main.id, &[])
            .expect("main manifest should clear");
        let removed = store
            .remove_unreferenced_missing_files("repo", &[])
            .expect("unreferenced main snapshot should be removed");
        assert_eq!(removed, 1);
        let status = store
            .repository_status("repo")
            .expect("status should reload");
        assert_eq!(status.files_indexed, 1);
        assert_eq!(
            store
                .find_symbols_for_ref("repo", &feature.id, "helper")
                .expect("feature snapshot should remain")[0]
                .qualified_name,
            "feature::helper"
        );
    }

    #[test]
    fn sqlite_vec_upserts_queries_scrolls_and_deletes_points() {
        let db = TestDb::new("sqlite-vec-roundtrip");
        let vector_store = SqliteVectorStore::new(&db.config()).expect("vector store should open");
        let table = vector_table_name("repo", "nomic-embed-text");
        vector_store
            .ensure_table(&table, 2)
            .expect("vector table should be created");

        let first = vector_point_id("0123456789abcdeffedcba9876543210")
            .expect("first point id should format");
        let second = vector_point_id("11111111111111112222222222222222")
            .expect("second point id should format");
        vector_store
            .upsert_points(
                &table,
                &[
                    VectorPoint {
                        id: first.clone(),
                        vector: vec![1.0, 0.0],
                        payload: sample_payload(),
                    },
                    VectorPoint {
                        id: second.clone(),
                        vector: vec![0.0, 1.0],
                        payload: PointPayload {
                            chunk_id: "chunk-2".to_owned(),
                            text_hash: "hash-2".to_owned(),
                            ..sample_payload()
                        },
                    },
                ],
            )
            .expect("points should upsert");

        let results = vector_store
            .query_points(&table, vec![1.0, 0.0], 1)
            .expect("query should run");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, first);
        assert_eq!(results[0].payload.path, "src/lib.rs");

        let scrolled = vector_store
            .scroll_points_for_repository(&table, "repo")
            .expect("points should scroll");
        assert_eq!(scrolled.len(), 2);

        vector_store
            .delete_points(&table, &[first.clone(), second])
            .expect("points should delete");
        let scrolled = vector_store
            .scroll_points_for_repository(&table, "repo")
            .expect("empty points should scroll");
        assert!(scrolled.is_empty());
    }

    #[test]
    fn sqlite_vec_queries_filter_through_ref_files() {
        let db = TestDb::new("sqlite-vec-ref-query");
        let mut store = SqliteStore::open(&db.config()).expect("store should open");
        store.migrate().expect("migration should run");
        store
            .upsert_repository(&RepositoryRecord {
                id: "repo".to_owned(),
                root_path: "/tmp/repo".to_owned(),
            })
            .expect("repository should persist");
        let main = RepositoryRefSnapshot {
            id: stable_id(&["repository-ref", "repo", "branch", "main"]),
            repository_id: "repo".to_owned(),
            kind: RepositoryRefKind::Branch,
            name: Some("main".to_owned()),
            head_oid: Some("0123456789abcdef0123456789abcdef01234567".to_owned()),
            local_branches: vec!["main".to_owned(), "feature".to_owned()],
        };
        let feature = RepositoryRefSnapshot {
            id: stable_id(&["repository-ref", "repo", "branch", "feature"]),
            repository_id: "repo".to_owned(),
            kind: RepositoryRefKind::Branch,
            name: Some("feature".to_owned()),
            head_oid: Some("fedcba9876543210fedcba9876543210fedcba98".to_owned()),
            local_branches: vec!["main".to_owned(), "feature".to_owned()],
        };
        store
            .sync_repository_ref(&main)
            .expect("main ref should sync");
        store
            .replace_file_facts_for_ref_with_tests(
                &main.id,
                &sample_file_at("main-file", "src/main.rs", "hash-main"),
                &[],
                &[],
                &[],
                &[],
            )
            .expect("main file should persist");
        store
            .sync_repository_ref(&feature)
            .expect("feature ref should sync");
        store
            .replace_file_facts_for_ref_with_tests(
                &feature.id,
                &sample_file_at("feature-file", "src/feature.rs", "hash-feature"),
                &[],
                &[],
                &[],
                &[],
            )
            .expect("feature file should persist");
        drop(store);

        let vector_store = SqliteVectorStore::new(&db.config()).expect("vector store should open");
        let table = vector_table_name("repo", "nomic-embed-text");
        let main_point = vector_point_id("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
            .expect("main point id should format");
        let feature_point = vector_point_id("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")
            .expect("feature point id should format");
        vector_store
            .upsert_points(
                &table,
                &[
                    VectorPoint {
                        id: main_point.clone(),
                        vector: vec![1.0, 0.0],
                        payload: PointPayload {
                            file_id: "main-file".to_owned(),
                            chunk_id: "main-chunk".to_owned(),
                            path: "src/main.rs".to_owned(),
                            ..sample_payload()
                        },
                    },
                    VectorPoint {
                        id: feature_point.clone(),
                        vector: vec![0.0, 1.0],
                        payload: PointPayload {
                            file_id: "feature-file".to_owned(),
                            chunk_id: "feature-chunk".to_owned(),
                            path: "src/feature.rs".to_owned(),
                            ..sample_payload()
                        },
                    },
                ],
            )
            .expect("points should upsert");

        let main_results = vector_store
            .query_points_for_ref(&table, "repo", &main.id, vec![0.0, 1.0], 5)
            .expect("main scoped query should run");
        assert_eq!(main_results.len(), 1);
        assert_eq!(main_results[0].id, main_point);
        assert_eq!(main_results[0].payload.path, "src/main.rs");

        let feature_results = vector_store
            .query_points_for_ref(&table, "repo", &feature.id, vec![1.0, 0.0], 5)
            .expect("feature scoped query should run");
        assert_eq!(feature_results.len(), 1);
        assert_eq!(feature_results[0].id, feature_point);
        assert_eq!(feature_results[0].payload.path, "src/feature.rs");
    }

    #[test]
    fn freshness_for_hash_labels_evidence_states() {
        assert_eq!(
            crate::freshness_for_hash(Some("same"), Some("same")),
            crate::EvidenceFreshness::Fresh
        );
        assert_eq!(
            crate::freshness_for_hash(Some("old"), Some("new")),
            crate::EvidenceFreshness::Stale
        );
        assert_eq!(
            crate::freshness_for_hash(Some("old"), None),
            crate::EvidenceFreshness::Deleted
        );
        assert_eq!(
            crate::freshness_for_hash(None, Some("new")),
            crate::EvidenceFreshness::Missing
        );
        assert_eq!(
            crate::freshness_for_hash(None, None),
            crate::EvidenceFreshness::Unknown
        );
    }

    #[test]
    fn sqlite_migrates_and_persists_file_chunks() {
        let db = TestDb::new("persist");
        let mut store = SqliteStore::open(&db.config()).expect("store should open");
        store.migrate().expect("migration should run");
        store
            .upsert_repository(&RepositoryRecord {
                id: "repo".to_owned(),
                root_path: "/tmp/repo".to_owned(),
            })
            .expect("repository should persist");

        store
            .replace_file_facts(&sample_file("hash-1"), &[], &[sample_chunk("chunk-1")], &[])
            .expect("file chunks should persist");

        assert!(
            store
                .file_unchanged("repo", "file", "src/lib.rs", "hash-1", "parser")
                .expect("unchanged check should run")
        );
        assert!(
            !store
                .file_unchanged("repo", "file", "src/lib.rs", "hash-1", "next-parser")
                .expect("parser-version check should run")
        );
        let status = store.repository_status("repo").expect("status should load");
        assert_eq!(status.files_indexed, 1);
        assert_eq!(status.chunks_indexed, 1);
        assert!(status.last_indexed_at.is_some());
    }

    #[test]
    fn sqlite_migration_creates_query_indexes() {
        let db = TestDb::new("indexes");
        let store = SqliteStore::open(&db.config()).expect("store should open");
        store.migrate().expect("migration should run");

        let indexes = sqlite_index_names(&store);

        for expected in [
            "idx_files_repository_path",
            "idx_files_repository_path_hash",
            "idx_chunks_file_id",
            "idx_symbols_file_id",
            "idx_symbols_name",
            "idx_symbols_qualified_name",
            "idx_calls_caller_symbol_id",
            "idx_calls_callee_symbol_id",
            "idx_symbol_references_file_kind",
            "idx_symbol_references_source_kind",
            "idx_symbol_references_target_kind",
            "idx_symbol_references_kind_status",
            "idx_tests_repository_name",
            "idx_tests_repository_qualified_name",
            "idx_tests_file_id",
            "idx_tests_symbol_id",
            "idx_index_runs_repository_status",
            "idx_index_runs_repository_model_status",
            "idx_file_index_events_run_path",
            "idx_file_index_events_repository_path_time",
            "idx_file_index_events_repository_action_status",
            "idx_semantic_generations_repository_fast_completed",
            "idx_semantic_generation_refs_generation",
            "idx_chunk_embeddings_repository_layer_model",
            "idx_chunk_embeddings_generation_chunk",
            "idx_chunk_embeddings_repository_generation_layer_status",
            "idx_quality_embedding_jobs_repository_generation_status",
        ] {
            assert!(
                indexes.iter().any(|index| index == expected),
                "missing SQLite index {expected}; found {indexes:?}"
            );
        }
    }

    #[test]
    fn sqlite_migration_removes_legacy_file_path_uniqueness() {
        let db = TestDb::new("content-addressed-files-migration");
        let store = SqliteStore::open(&db.config()).expect("store should open");
        store
            .connection
            .execute_batch(
                "CREATE TABLE repositories (
                   id TEXT PRIMARY KEY,
                   root_path TEXT NOT NULL UNIQUE,
                   created_at TEXT NOT NULL,
                   updated_at TEXT NOT NULL
                 );
                 CREATE TABLE files (
                   id TEXT PRIMARY KEY,
                   repository_id TEXT NOT NULL,
                   path TEXT NOT NULL,
                   language TEXT NOT NULL,
                   content_hash TEXT NOT NULL,
                   indexed_at TEXT NOT NULL,
                   index_run_id TEXT,
                   parser_version TEXT,
                   UNIQUE(repository_id, path),
                   FOREIGN KEY(repository_id) REFERENCES repositories(id) ON DELETE CASCADE
                 );
                 INSERT INTO repositories (id, root_path, created_at, updated_at)
                 VALUES ('repo', '/tmp/repo', '1', '1');
                 INSERT INTO files (
                   id, repository_id, path, language, content_hash, indexed_at,
                   index_run_id, parser_version
                 )
                 VALUES ('old-file', 'repo', 'src/lib.rs', 'rust', 'hash-old', '1', 'run', 'parser');",
            )
            .expect("legacy schema should be installed");

        store.migrate().expect("migration should run");

        store
            .connection
            .execute(
                "INSERT INTO files (
                   id, repository_id, path, language, content_hash, indexed_at,
                   index_run_id, parser_version
                 )
                 VALUES ('new-file', 'repo', 'src/lib.rs', 'rust', 'hash-new', '2', 'run', 'parser')",
                [],
            )
            .expect("same path with different content should insert");
        let count: i64 = store
            .connection
            .query_row(
                "SELECT COUNT(*) FROM files WHERE repository_id = 'repo' AND path = 'src/lib.rs'",
                [],
                |row| row.get(0),
            )
            .expect("file count should load");
        assert_eq!(count, 2);
    }

    #[test]
    fn sqlite_migration_creates_layered_semantic_tables_idempotently() {
        let db = TestDb::new("layered-semantic-tables");
        let store = SqliteStore::open(&db.config()).expect("store should open");
        store.migrate().expect("first migration should run");
        store.migrate().expect("second migration should run");

        let tables = sqlite_table_names(&store);

        for expected in [
            "file_index_events",
            "symbol_references",
            "semantic_generations",
            "semantic_generation_refs",
            "chunk_embeddings",
            "quality_embedding_jobs",
        ] {
            assert!(
                tables.iter().any(|table| table == expected),
                "missing SQLite table {expected}; found {tables:?}"
            );
        }
    }

    #[test]
    fn semantic_generation_refs_route_generations_per_ref() {
        let db = TestDb::new("semantic-generation-refs");
        let mut store = SqliteStore::open(&db.config()).expect("store should open");
        store.migrate().expect("migration should run");
        store
            .upsert_repository(&RepositoryRecord {
                id: "repo".to_owned(),
                root_path: "/tmp/repo".to_owned(),
            })
            .expect("repository should persist");
        let main = RepositoryRefSnapshot {
            id: stable_id(&["repository-ref", "repo", "branch", "main"]),
            repository_id: "repo".to_owned(),
            kind: RepositoryRefKind::Branch,
            name: Some("main".to_owned()),
            head_oid: Some("0123456789abcdef0123456789abcdef01234567".to_owned()),
            local_branches: vec!["main".to_owned(), "feature".to_owned()],
        };
        let feature = RepositoryRefSnapshot {
            id: stable_id(&["repository-ref", "repo", "branch", "feature"]),
            repository_id: "repo".to_owned(),
            kind: RepositoryRefKind::Branch,
            name: Some("feature".to_owned()),
            head_oid: Some("fedcba9876543210fedcba9876543210fedcba98".to_owned()),
            local_branches: vec!["main".to_owned(), "feature".to_owned()],
        };
        store.sync_repository_ref(&main).expect("main should sync");
        store
            .sync_repository_ref(&feature)
            .expect("feature should sync");

        let mut main_generation = sample_semantic_generation();
        main_generation.id = "generation-main".to_owned();
        main_generation.fast_completed_at = "100".to_owned();
        main_generation.created_at = "100".to_owned();
        main_generation.updated_at = "100".to_owned();
        store
            .upsert_semantic_generation(&main_generation)
            .expect("main generation should persist");
        let mut feature_generation = sample_semantic_generation();
        feature_generation.id = "generation-feature".to_owned();
        feature_generation.fast_completed_at = "200".to_owned();
        feature_generation.created_at = "200".to_owned();
        feature_generation.updated_at = "200".to_owned();
        feature_generation.quality_status = "quality_ready".to_owned();
        feature_generation.active_layer = "quality".to_owned();
        store
            .upsert_semantic_generation(&feature_generation)
            .expect("feature generation should persist");

        store
            .link_semantic_generation_to_ref("repo", &main.id, &main_generation.id, "101")
            .expect("main generation should link");
        store
            .link_semantic_generation_to_ref("repo", &feature.id, &feature_generation.id, "201")
            .expect("feature generation should link");

        let repo_latest = store
            .latest_semantic_generation("repo")
            .expect("latest repo generation should load")
            .expect("latest repo generation should exist");
        assert_eq!(repo_latest.id, "generation-feature");
        let main_latest = store
            .latest_semantic_generation_for_ref("repo", &main.id)
            .expect("main ref generation should load")
            .expect("main ref generation should exist");
        assert_eq!(main_latest.id, "generation-main");
        let feature_latest = store
            .latest_semantic_generation_for_ref("repo", &feature.id)
            .expect("feature ref generation should load")
            .expect("feature ref generation should exist");
        assert_eq!(feature_latest.id, "generation-feature");

        let main_routing = store
            .semantic_routing_summary_for_ref("repo", &main.id)
            .expect("main routing should load")
            .expect("main routing should exist");
        assert_eq!(main_routing.generation_id, "generation-main");
        assert_eq!(main_routing.active_layer, SemanticLayer::Fast);
        let feature_routing = store
            .semantic_routing_summary_for_ref("repo", &feature.id)
            .expect("feature routing should load")
            .expect("feature routing should exist");
        assert_eq!(feature_routing.generation_id, "generation-feature");
        assert_eq!(feature_routing.active_layer, SemanticLayer::Quality);
    }

    #[test]
    fn layered_semantic_records_round_trip() {
        let db = TestDb::new("layered-semantic-round-trip");
        let mut store = SqliteStore::open(&db.config()).expect("store should open");
        store.migrate().expect("migration should run");
        store
            .upsert_repository(&RepositoryRecord {
                id: "repo".to_owned(),
                root_path: "/tmp/repo".to_owned(),
            })
            .expect("repository should persist");
        store
            .replace_file_facts(
                &sample_file("content-hash"),
                &[],
                &[sample_chunk("chunk-1")],
                &[],
            )
            .expect("file facts should persist");

        let generation = sample_semantic_generation();
        store
            .upsert_semantic_generation(&generation)
            .expect("generation should persist");
        assert_eq!(
            store
                .latest_semantic_generation("repo")
                .expect("latest generation should load"),
            Some(generation.clone())
        );

        let embedding = sample_chunk_embedding();
        store
            .upsert_chunk_embedding(&embedding)
            .expect("chunk embedding should persist");
        assert_eq!(
            store
                .chunk_embeddings_for_generation("repo", "generation-1", "fast")
                .expect("chunk embeddings should load"),
            vec![embedding]
        );

        let job = sample_quality_embedding_job();
        store
            .upsert_quality_embedding_job(&job)
            .expect("quality job should persist");
        assert_eq!(
            store
                .quality_jobs_by_status("repo", "generation-1", "pending")
                .expect("quality jobs should load"),
            vec![job]
        );

        let status = store.repository_status("repo").expect("status should load");
        assert_eq!(status.files_indexed, 1);
        assert_eq!(status.chunks_indexed, 1);
    }

    #[test]
    fn quality_queue_upserts_jobs_and_marks_generation_pending() {
        let db = TestDb::new("quality-queue-pending");
        let mut store = SqliteStore::open(&db.config()).expect("store should open");
        store.migrate().expect("migration should run");
        store
            .upsert_repository(&RepositoryRecord {
                id: "repo".to_owned(),
                root_path: "/tmp/repo".to_owned(),
            })
            .expect("repository should persist");
        store
            .replace_file_facts(
                &sample_file("content-hash"),
                &[],
                &[sample_chunk("chunk-1")],
                &[],
            )
            .expect("file facts should persist");
        let generation = sample_semantic_generation();
        store
            .upsert_semantic_generation(&generation)
            .expect("generation should persist");
        let mut job = sample_quality_embedding_job();
        job.id = SqliteStore::quality_embedding_job_id("repo", "generation-1", "chunk-1");

        let first = store
            .queue_quality_embedding_jobs(&generation, "mxbai-embed-large", &[job.clone()], "200")
            .expect("quality jobs should queue");
        job.updated_at = "201".to_owned();
        let second = store
            .queue_quality_embedding_jobs(&generation, "mxbai-embed-large", &[job], "201")
            .expect("quality jobs should upsert idempotently");

        assert_eq!(first.queued_jobs, 1);
        assert_eq!(second.queued_jobs, 1);
        assert_eq!(second.skipped_stale_jobs, 0);
        let jobs = store
            .quality_jobs_by_status("repo", "generation-1", "pending")
            .expect("pending jobs should load");
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].updated_at, "201");
        let generation = store
            .latest_semantic_generation("repo")
            .expect("generation should load")
            .expect("generation should exist");
        assert_eq!(generation.quality_status, "quality_pending");
        assert_eq!(
            generation.quality_model.as_deref(),
            Some("mxbai-embed-large")
        );
        assert_eq!(generation.quality_dimension, None);
        assert_eq!(generation.active_layer, "fast");
    }

    #[test]
    fn quality_queue_marks_only_superseded_pending_and_running_jobs_stale() {
        let db = TestDb::new("quality-queue-stale-scope");
        let mut store = SqliteStore::open(&db.config()).expect("store should open");
        store.migrate().expect("migration should run");
        store
            .upsert_repository(&RepositoryRecord {
                id: "repo".to_owned(),
                root_path: "/tmp/repo".to_owned(),
            })
            .expect("repository should persist");
        store
            .replace_file_facts(
                &sample_file("content-hash"),
                &[],
                &[
                    sample_chunk("chunk-1"),
                    sample_chunk("chunk-2"),
                    sample_chunk("chunk-3"),
                    sample_chunk("chunk-4"),
                    sample_chunk("chunk-5"),
                ],
                &[],
            )
            .expect("file facts should persist");
        let current_generation = sample_semantic_generation();
        let old_generation = SemanticGenerationRecord {
            id: "generation-old".to_owned(),
            fast_completed_at: "050".to_owned(),
            created_at: "050".to_owned(),
            updated_at: "050".to_owned(),
            ..sample_semantic_generation()
        };
        store
            .upsert_semantic_generation(&old_generation)
            .expect("old generation should persist");
        store
            .upsert_semantic_generation(&current_generation)
            .expect("current generation should persist");
        for (chunk_id, status) in [
            ("chunk-1", "pending"),
            ("chunk-2", "running"),
            ("chunk-3", "succeeded"),
            ("chunk-4", "failed"),
            ("chunk-5", "skipped_excluded"),
        ] {
            store
                .upsert_quality_embedding_job(&sample_quality_embedding_job_for(
                    "generation-old",
                    chunk_id,
                    status,
                ))
                .expect("old job should persist");
        }

        let skipped = store
            .mark_superseded_quality_jobs_stale("repo", "generation-1", "300")
            .expect("superseded jobs should be marked stale");

        assert_eq!(skipped, 2);
        assert_eq!(
            store
                .quality_jobs_by_status("repo", "generation-old", "skipped_stale")
                .expect("stale jobs should load")
                .len(),
            2
        );
        assert_eq!(
            store
                .quality_jobs_by_status("repo", "generation-old", "succeeded")
                .expect("succeeded jobs should remain")
                .len(),
            1
        );
        assert_eq!(
            store
                .quality_jobs_by_status("repo", "generation-old", "failed")
                .expect("failed jobs should remain")
                .len(),
            1
        );
        assert_eq!(
            store
                .quality_jobs_by_status("repo", "generation-old", "skipped_excluded")
                .expect("excluded jobs should remain")
                .len(),
            1
        );
    }

    #[test]
    fn blocked_quality_generation_records_status_without_pending_jobs() {
        let db = TestDb::new("quality-queue-blocked");
        let mut store = SqliteStore::open(&db.config()).expect("store should open");
        store.migrate().expect("migration should run");
        store
            .upsert_repository(&RepositoryRecord {
                id: "repo".to_owned(),
                root_path: "/tmp/repo".to_owned(),
            })
            .expect("repository should persist");
        store
            .replace_file_facts(
                &sample_file("content-hash"),
                &[],
                &[sample_chunk("chunk-1")],
                &[],
            )
            .expect("file facts should persist");
        let generation = sample_semantic_generation();
        store
            .upsert_semantic_generation(&generation)
            .expect("generation should persist");

        let summary = store
            .mark_quality_generation_blocked(&generation, "mxbai-embed-large", "400")
            .expect("generation should be marked blocked");

        assert_eq!(summary.quality_status, SemanticLayerStatus::QualityBlocked);
        assert_eq!(summary.queued_jobs, 0);
        assert_eq!(
            store
                .quality_jobs_by_status("repo", "generation-1", "pending")
                .expect("pending jobs should load")
                .len(),
            0
        );
        let generation = store
            .latest_semantic_generation("repo")
            .expect("generation should load")
            .expect("generation should exist");
        assert_eq!(generation.quality_status, "quality_blocked");
        assert_eq!(
            generation.quality_model.as_deref(),
            Some("mxbai-embed-large")
        );
        assert_eq!(generation.quality_dimension, None);
        assert_eq!(generation.active_layer, "fast");
    }

    #[test]
    fn quality_worker_claims_bounded_jobs_and_loads_sources() {
        let db = TestDb::new("quality-worker-claim-source");
        let mut store = SqliteStore::open(&db.config()).expect("store should open");
        store.migrate().expect("migration should run");
        store
            .upsert_repository(&RepositoryRecord {
                id: "repo".to_owned(),
                root_path: "/tmp/repo".to_owned(),
            })
            .expect("repository should persist");
        store
            .replace_file_facts(
                &sample_file("content-hash"),
                &[sample_symbol("symbol", "hello", "hello")],
                &[sample_chunk("chunk-1"), sample_chunk("chunk-2")],
                &[],
            )
            .expect("file facts should persist");
        let generation = sample_semantic_generation();
        store
            .upsert_semantic_generation(&generation)
            .expect("generation should persist");
        for chunk_id in ["chunk-1", "chunk-2"] {
            store
                .upsert_quality_embedding_job(&sample_quality_embedding_job_for(
                    "generation-1",
                    chunk_id,
                    "pending",
                ))
                .expect("job should persist");
        }

        let claimed = store
            .claim_quality_embedding_jobs("repo", "generation-1", 1, "500")
            .expect("job should claim");

        assert_eq!(claimed.len(), 1);
        assert_eq!(claimed[0].status, "running");
        assert_eq!(claimed[0].attempts, 1);
        let source_rows = store
            .quality_job_source_rows(&[claimed[0].id.clone()])
            .expect("source row should load");
        assert_eq!(source_rows.len(), 1);
        assert_eq!(source_rows[0].current_file_id.as_deref(), Some("file"));
        assert_eq!(
            source_rows[0].current_text_hash.as_deref(),
            Some("text-chunk-1")
        );
        assert_eq!(source_rows[0].start_byte, Some(0));
        assert_eq!(source_rows[0].end_byte, Some(32));
        assert_eq!(source_rows[0].symbol_name.as_deref(), Some("hello"));
        assert_eq!(
            store
                .quality_jobs_by_status("repo", "generation-1", "pending")
                .expect("pending jobs should load")
                .len(),
            1
        );
    }

    #[test]
    fn quality_worker_completion_updates_embedding_and_failed_generation_progress() {
        let db = TestDb::new("quality-worker-completion-progress");
        let mut store = SqliteStore::open(&db.config()).expect("store should open");
        store.migrate().expect("migration should run");
        store
            .upsert_repository(&RepositoryRecord {
                id: "repo".to_owned(),
                root_path: "/tmp/repo".to_owned(),
            })
            .expect("repository should persist");
        store
            .replace_file_facts(
                &sample_file("content-hash"),
                &[],
                &[sample_chunk("chunk-1"), sample_chunk("chunk-2")],
                &[],
            )
            .expect("file facts should persist");
        let mut generation = sample_semantic_generation();
        generation.embeddable_chunks = 2;
        store
            .upsert_semantic_generation(&generation)
            .expect("generation should persist");
        let mut success_job =
            sample_quality_embedding_job_for("generation-1", "chunk-1", "running");
        success_job.attempts = 1;
        let mut failed_job = sample_quality_embedding_job_for("generation-1", "chunk-2", "running");
        failed_job.attempts = 1;
        store
            .upsert_quality_embedding_job(&success_job)
            .expect("success job should persist");
        store
            .upsert_quality_embedding_job(&failed_job)
            .expect("failed job should persist");
        store
            .upsert_chunk_embedding(&sample_chunk_embedding())
            .expect("fast embedding should persist");
        store
            .upsert_chunk_embedding(&ChunkEmbeddingRecord {
                id: "embedding-2".to_owned(),
                chunk_id: "chunk-2".to_owned(),
                text_hash: "text-chunk-2".to_owned(),
                ..sample_chunk_embedding()
            })
            .expect("second fast embedding should persist");

        store
            .complete_quality_embedding_job(
                &success_job.id,
                QualityJobCompletion::Succeeded {
                    embedding: Box::new(sample_quality_chunk_embedding("current")),
                },
                "600",
            )
            .expect("success should complete");
        store
            .complete_quality_embedding_job(
                &failed_job.id,
                QualityJobCompletion::Failed {
                    error_summary: "service unavailable".to_owned(),
                },
                "601",
            )
            .expect("failure should complete");
        let progress = store
            .refresh_quality_generation_progress("repo", "generation-1", Some(768), "602")
            .expect("progress should refresh");

        assert_eq!(progress.quality_embedded_chunks, 1);
        assert_eq!(progress.succeeded_jobs, 1);
        assert_eq!(progress.failed_jobs, 1);
        assert_eq!(progress.pending_jobs, 0);
        assert_eq!(progress.running_jobs, 0);
        assert_eq!(
            store
                .chunk_embeddings_for_generation("repo", "generation-1", "quality")
                .expect("quality embeddings should load")
                .len(),
            1
        );
        let generation = store
            .latest_semantic_generation("repo")
            .expect("generation should load")
            .expect("generation should exist");
        assert_eq!(generation.quality_status, "quality_failed");
        assert_eq!(generation.quality_dimension, Some(768));
        assert_eq!(generation.quality_embedded_chunks, 1);
        assert_eq!(generation.active_layer, "fast");
    }

    #[test]
    fn quality_generation_progress_counts_current_quality_manifest_rows() {
        let db = TestDb::new("quality-progress-manifest-count");
        let mut store = SqliteStore::open(&db.config()).expect("store should open");
        store.migrate().expect("migration should run");
        persist_repo_with_chunks(&mut store, &[sample_chunk("chunk-1")]);
        let mut generation = sample_semantic_generation();
        generation.quality_embedded_chunks = 1;
        store
            .upsert_semantic_generation(&generation)
            .expect("generation should persist");
        store
            .upsert_quality_embedding_job(&sample_quality_embedding_job_for(
                "generation-1",
                "chunk-1",
                "pending",
            ))
            .expect("pending job should persist");
        store
            .upsert_chunk_embedding(&sample_chunk_embedding())
            .expect("fast embedding should persist");

        let progress = store
            .quality_generation_progress("repo", "generation-1")
            .expect("progress should load");

        assert_eq!(progress.quality_embedded_chunks, 0);
        assert_eq!(progress.pending_jobs, 1);
    }

    #[test]
    fn quality_jobs_for_fast_generation_cover_full_fast_manifest() {
        let db = TestDb::new("quality-jobs-full-fast-manifest");
        let mut store = SqliteStore::open(&db.config()).expect("store should open");
        store.migrate().expect("migration should run");
        persist_repo_with_chunks(
            &mut store,
            &[sample_chunk("chunk-1"), sample_chunk("chunk-2")],
        );
        let mut generation = sample_semantic_generation();
        generation.embeddable_chunks = 2;
        generation.fast_embedded_chunks = 2;
        store
            .upsert_semantic_generation(&generation)
            .expect("generation should persist");
        store
            .upsert_chunk_embedding(&ChunkEmbeddingRecord {
                id: "fast-embedding-1".to_owned(),
                ..sample_chunk_embedding()
            })
            .expect("first fast embedding should persist");
        store
            .upsert_chunk_embedding(&ChunkEmbeddingRecord {
                id: "fast-embedding-2".to_owned(),
                chunk_id: "chunk-2".to_owned(),
                text_hash: "text-chunk-2".to_owned(),
                vector_point_id: "01234567-89ab-cdef-fedc-ba9876543212".to_owned(),
                ..sample_chunk_embedding()
            })
            .expect("second fast embedding should persist");

        let jobs = store
            .quality_embedding_jobs_for_fast_generation(
                "repo",
                "generation-1",
                "mxbai-embed-large",
                "500",
            )
            .expect("quality jobs should load");

        assert_eq!(jobs.len(), 2);
        assert_eq!(jobs[0].chunk_id, "chunk-1");
        assert_eq!(jobs[1].chunk_id, "chunk-2");
        assert!(jobs.iter().all(|job| job.status == "pending"));
        assert!(jobs.iter().all(|job| job.created_at == "500"));
    }

    #[test]
    fn quality_jobs_for_fast_generation_skip_carried_forward_quality_embeddings() {
        let db = TestDb::new("quality-jobs-skip-carried-forward");
        let mut store = SqliteStore::open(&db.config()).expect("store should open");
        store.migrate().expect("migration should run");
        persist_repo_with_chunks(
            &mut store,
            &[sample_chunk("chunk-1"), sample_chunk("chunk-2")],
        );
        store
            .upsert_semantic_generation(&sample_semantic_generation())
            .expect("old generation should persist");
        store
            .upsert_chunk_embedding(&sample_quality_chunk_embedding("current"))
            .expect("old quality embedding should persist");
        let mut generation = sample_semantic_generation();
        generation.id = "generation-2".to_owned();
        generation.fast_completed_at = "200".to_owned();
        generation.created_at = "200".to_owned();
        generation.updated_at = "200".to_owned();
        generation.embeddable_chunks = 2;
        generation.fast_embedded_chunks = 2;
        store
            .upsert_semantic_generation(&generation)
            .expect("new generation should persist");
        store
            .upsert_chunk_embedding(&ChunkEmbeddingRecord {
                id: "fast-generation-2-chunk-1".to_owned(),
                generation_id: "generation-2".to_owned(),
                ..sample_chunk_embedding()
            })
            .expect("first fast embedding should persist");
        store
            .upsert_chunk_embedding(&ChunkEmbeddingRecord {
                id: "fast-generation-2-chunk-2".to_owned(),
                generation_id: "generation-2".to_owned(),
                chunk_id: "chunk-2".to_owned(),
                text_hash: "text-chunk-2".to_owned(),
                vector_point_id: "01234567-89ab-cdef-fedc-ba9876543212".to_owned(),
                ..sample_chunk_embedding()
            })
            .expect("second fast embedding should persist");

        let carried = store
            .carry_forward_quality_embeddings_for_fast_generation(
                "repo",
                "generation-2",
                "mxbai-embed-large",
            )
            .expect("quality embeddings should carry forward");
        let jobs = store
            .quality_embedding_jobs_for_fast_generation(
                "repo",
                "generation-2",
                "mxbai-embed-large",
                "500",
            )
            .expect("quality jobs should load");
        let progress = store
            .quality_generation_progress("repo", "generation-2")
            .expect("progress should load");

        assert_eq!(carried.carried_embeddings, 1);
        assert_eq!(carried.quality_dimension, Some(768));
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].chunk_id, "chunk-2");
        assert_eq!(progress.embeddable_chunks, 2);
        assert_eq!(progress.quality_embedded_chunks, 1);
        assert_eq!(
            store
                .chunk_embeddings_for_generation("repo", "generation-2", "quality")
                .expect("carried quality rows should load")
                .len(),
            1
        );
    }

    #[test]
    fn quality_activation_marks_complete_generation_ready_and_active() {
        let db = TestDb::new("quality-activation-ready");
        let mut store = SqliteStore::open(&db.config()).expect("store should open");
        store.migrate().expect("migration should run");
        persist_repo_with_chunks(&mut store, &[sample_chunk("chunk-1")]);
        store
            .upsert_semantic_generation(&sample_semantic_generation())
            .expect("generation should persist");
        store
            .upsert_chunk_embedding(&sample_quality_chunk_embedding("current"))
            .expect("quality embedding should persist");
        store
            .upsert_chunk_embedding(&sample_chunk_embedding())
            .expect("fast embedding should persist");

        let summary = store
            .refresh_quality_activation("repo", "generation-1", Some(768), "700")
            .expect("activation should refresh");

        assert_eq!(summary.quality_status, SemanticLayerStatus::QualityReady);
        assert_eq!(summary.active_layer, SemanticLayer::Quality);
        assert_eq!(summary.reason, QualityActivationReason::QualityComplete);
        assert_eq!(summary.progress.quality_embedded_chunks, 1);
        let generation = store
            .latest_semantic_generation("repo")
            .expect("generation should load")
            .expect("generation should exist");
        assert_eq!(generation.quality_status, "quality_ready");
        assert_eq!(generation.active_layer, "quality");
        assert_eq!(generation.quality_completed_at, Some("700".to_owned()));
    }

    #[test]
    fn quality_activation_keeps_pending_jobs_on_fast_layer() {
        let db = TestDb::new("quality-activation-pending-jobs");
        let mut store = SqliteStore::open(&db.config()).expect("store should open");
        store.migrate().expect("migration should run");
        persist_repo_with_chunks(&mut store, &[sample_chunk("chunk-1")]);
        store
            .upsert_semantic_generation(&sample_semantic_generation())
            .expect("generation should persist");
        store
            .upsert_quality_embedding_job(&sample_quality_embedding_job_for(
                "generation-1",
                "chunk-1",
                "pending",
            ))
            .expect("job should persist");
        store
            .upsert_chunk_embedding(&sample_chunk_embedding())
            .expect("fast embedding should persist");

        let summary = store
            .refresh_quality_activation("repo", "generation-1", Some(768), "700")
            .expect("activation should refresh");

        assert_eq!(summary.quality_status, SemanticLayerStatus::QualityPending);
        assert_eq!(summary.active_layer, SemanticLayer::Fast);
        assert_eq!(summary.reason, QualityActivationReason::QualityJobsPending);
        assert_eq!(summary.progress.pending_jobs, 1);
        let generation = store
            .latest_semantic_generation("repo")
            .expect("generation should load")
            .expect("generation should exist");
        assert_eq!(generation.quality_status, "quality_pending");
        assert_eq!(generation.active_layer, "fast");
        assert_eq!(generation.quality_completed_at, None);
    }

    #[test]
    fn quality_activation_keeps_incomplete_coverage_on_fast_layer() {
        let db = TestDb::new("quality-activation-incomplete-coverage");
        let mut store = SqliteStore::open(&db.config()).expect("store should open");
        store.migrate().expect("migration should run");
        persist_repo_with_chunks(
            &mut store,
            &[sample_chunk("chunk-1"), sample_chunk("chunk-2")],
        );
        let mut generation = sample_semantic_generation();
        generation.embeddable_chunks = 2;
        store
            .upsert_semantic_generation(&generation)
            .expect("generation should persist");
        store
            .upsert_chunk_embedding(&sample_quality_chunk_embedding("current"))
            .expect("quality embedding should persist");
        store
            .upsert_chunk_embedding(&sample_chunk_embedding())
            .expect("fast embedding should persist");

        let summary = store
            .refresh_quality_activation("repo", "generation-1", Some(768), "700")
            .expect("activation should refresh");

        assert_eq!(summary.quality_status, SemanticLayerStatus::QualityPending);
        assert_eq!(summary.active_layer, SemanticLayer::Fast);
        assert_eq!(
            summary.reason,
            QualityActivationReason::QualityCoverageIncomplete
        );
        assert_eq!(summary.progress.quality_embedded_chunks, 1);
    }

    #[test]
    fn quality_activation_treats_skipped_excluded_jobs_as_complete_coverage() {
        let db = TestDb::new("quality-activation-excluded-coverage");
        let mut store = SqliteStore::open(&db.config()).expect("store should open");
        store.migrate().expect("migration should run");
        persist_repo_with_chunks(
            &mut store,
            &[sample_chunk("chunk-1"), sample_chunk("chunk-2")],
        );
        let mut generation = sample_semantic_generation();
        generation.embeddable_chunks = 2;
        store
            .upsert_semantic_generation(&generation)
            .expect("generation should persist");
        store
            .upsert_chunk_embedding(&sample_quality_chunk_embedding("current"))
            .expect("quality embedding should persist");
        store
            .upsert_chunk_embedding(&sample_chunk_embedding())
            .expect("fast embedding should persist");
        store
            .upsert_chunk_embedding(&ChunkEmbeddingRecord {
                id: "embedding-2".to_owned(),
                chunk_id: "chunk-2".to_owned(),
                text_hash: "text-chunk-2".to_owned(),
                ..sample_chunk_embedding()
            })
            .expect("second fast embedding should persist");
        store
            .upsert_quality_embedding_job(&sample_quality_embedding_job_for(
                "generation-1",
                "chunk-2",
                "skipped_excluded",
            ))
            .expect("excluded job should persist");

        let summary = store
            .refresh_quality_activation("repo", "generation-1", Some(768), "700")
            .expect("activation should refresh");

        assert_eq!(summary.quality_status, SemanticLayerStatus::QualityReady);
        assert_eq!(summary.active_layer, SemanticLayer::Quality);
        assert_eq!(summary.reason, QualityActivationReason::QualityComplete);
        assert_eq!(summary.progress.embeddable_chunks, 2);
        assert_eq!(summary.progress.quality_eligible_chunks, 1);
        assert_eq!(summary.progress.quality_ineligible_chunks, 1);
        assert_eq!(summary.progress.quality_embedded_chunks, 1);

        let routing = store
            .semantic_routing_summary("repo")
            .expect("routing summary should load")
            .expect("routing summary should exist");
        let quality = routing.quality.expect("quality manifest should exist");
        assert_eq!(quality.expected_chunks, 1);
        assert_eq!(quality.current_chunks, 1);
        assert!(quality.is_complete);
    }

    #[test]
    fn quality_activation_marks_failed_jobs_failed_and_uses_fast_layer() {
        let db = TestDb::new("quality-activation-failed-jobs");
        let mut store = SqliteStore::open(&db.config()).expect("store should open");
        store.migrate().expect("migration should run");
        persist_repo_with_chunks(&mut store, &[sample_chunk("chunk-1")]);
        store
            .upsert_semantic_generation(&sample_semantic_generation())
            .expect("generation should persist");
        store
            .upsert_quality_embedding_job(&sample_quality_embedding_job_for(
                "generation-1",
                "chunk-1",
                "failed",
            ))
            .expect("job should persist");
        store
            .upsert_chunk_embedding(&sample_chunk_embedding())
            .expect("fast embedding should persist");

        let summary = store
            .refresh_quality_activation("repo", "generation-1", Some(768), "700")
            .expect("activation should refresh");

        assert_eq!(summary.quality_status, SemanticLayerStatus::QualityFailed);
        assert_eq!(summary.active_layer, SemanticLayer::Fast);
        assert_eq!(summary.reason, QualityActivationReason::QualityJobsFailed);
        assert_eq!(summary.progress.failed_jobs, 1);
    }

    #[test]
    fn quality_activation_keeps_skipped_stale_jobs_pending_and_fast() {
        let db = TestDb::new("quality-activation-stale-jobs");
        let mut store = SqliteStore::open(&db.config()).expect("store should open");
        store.migrate().expect("migration should run");
        persist_repo_with_chunks(&mut store, &[sample_chunk("chunk-1")]);
        store
            .upsert_semantic_generation(&sample_semantic_generation())
            .expect("generation should persist");
        store
            .upsert_chunk_embedding(&sample_quality_chunk_embedding("current"))
            .expect("quality embedding should persist");
        store
            .upsert_chunk_embedding(&sample_chunk_embedding())
            .expect("fast embedding should persist");
        store
            .upsert_quality_embedding_job(&sample_quality_embedding_job_for(
                "generation-1",
                "chunk-1",
                "skipped_stale",
            ))
            .expect("job should persist");

        let summary = store
            .refresh_quality_activation("repo", "generation-1", Some(768), "700")
            .expect("activation should refresh");

        assert_eq!(summary.quality_status, SemanticLayerStatus::QualityPending);
        assert_eq!(summary.active_layer, SemanticLayer::Fast);
        assert_eq!(summary.reason, QualityActivationReason::QualityJobsStale);
        assert_eq!(summary.progress.skipped_stale_jobs, 1);
        let generation = store
            .latest_semantic_generation("repo")
            .expect("generation should load")
            .expect("generation should exist");
        assert_eq!(generation.quality_status, "quality_pending");
        assert_eq!(generation.active_layer, "fast");
    }

    #[test]
    fn quality_activation_preserves_blocked_status_until_complete() {
        let db = TestDb::new("quality-activation-blocked");
        let mut store = SqliteStore::open(&db.config()).expect("store should open");
        store.migrate().expect("migration should run");
        persist_repo_with_chunks(&mut store, &[sample_chunk("chunk-1")]);
        let mut generation = sample_semantic_generation();
        generation.quality_status = "quality_blocked".to_owned();
        store
            .upsert_semantic_generation(&generation)
            .expect("generation should persist");

        let summary = store
            .refresh_quality_activation("repo", "generation-1", Some(768), "700")
            .expect("activation should refresh");

        assert_eq!(summary.quality_status, SemanticLayerStatus::QualityBlocked);
        assert_eq!(summary.active_layer, SemanticLayer::Fast);
        assert_eq!(summary.reason, QualityActivationReason::QualityBlocked);
    }

    #[test]
    fn quality_activation_does_not_activate_superseded_generation() {
        let db = TestDb::new("quality-activation-superseded");
        let mut store = SqliteStore::open(&db.config()).expect("store should open");
        store.migrate().expect("migration should run");
        persist_repo_with_chunks(&mut store, &[sample_chunk("chunk-1")]);
        store
            .upsert_semantic_generation(&sample_semantic_generation())
            .expect("generation should persist");
        let mut latest_generation = sample_semantic_generation();
        latest_generation.id = "generation-2".to_owned();
        latest_generation.created_at = "200".to_owned();
        latest_generation.updated_at = "200".to_owned();
        store
            .upsert_semantic_generation(&latest_generation)
            .expect("latest generation should persist");
        store
            .upsert_chunk_embedding(&sample_quality_chunk_embedding("current"))
            .expect("quality embedding should persist");

        let summary = store
            .refresh_quality_activation("repo", "generation-1", Some(768), "700")
            .expect("activation should refresh");

        assert_eq!(summary.quality_status, SemanticLayerStatus::QualityPending);
        assert_eq!(summary.active_layer, SemanticLayer::Fast);
        assert_eq!(summary.reason, QualityActivationReason::GenerationNotLatest);
    }

    #[test]
    fn unchanged_fast_generation_preserves_ready_quality_activation() {
        let db = TestDb::new("quality-activation-unchanged-fast");
        let mut store = SqliteStore::open(&db.config()).expect("store should open");
        store.migrate().expect("migration should run");
        persist_repo_with_chunks(&mut store, &[sample_chunk("chunk-1")]);
        let fast_manifest = sample_fast_manifest(&["chunk-1"], "content-hash");
        let generation = store
            .record_fast_semantic_generation(FastSemanticGenerationInput {
                repository_id: "repo",
                repository_ref_id: None,
                fast_model: "nomic-embed-text",
                fast_dimension: 768,
                vector_table: "mxbai-embed-large",
                upserted_embeddings: &fast_manifest,
                files_seen: 1,
                completed_at: "100",
            })
            .expect("generation should be recorded");
        let mut quality_generation = generation.clone();
        quality_generation.quality_model = Some("mxbai-embed-large".to_owned());
        quality_generation.quality_dimension = Some(768);
        quality_generation.quality_status = "quality_pending".to_owned();
        store
            .upsert_semantic_generation(&quality_generation)
            .expect("quality generation should persist");
        store
            .upsert_chunk_embedding(&ChunkEmbeddingRecord {
                generation_id: generation.id.clone(),
                ..sample_quality_chunk_embedding("current")
            })
            .expect("quality embedding should persist");
        store
            .refresh_quality_activation("repo", &generation.id, Some(768), "700")
            .expect("quality should activate");

        let unchanged = store
            .record_fast_semantic_generation(FastSemanticGenerationInput {
                repository_id: "repo",
                repository_ref_id: None,
                fast_model: "nomic-embed-text",
                fast_dimension: 768,
                vector_table: "mxbai-embed-large",
                upserted_embeddings: &fast_manifest,
                files_seen: 1,
                completed_at: "800",
            })
            .expect("unchanged generation should be preserved");

        assert_eq!(unchanged.id, generation.id);
        assert_eq!(unchanged.quality_status, "quality_ready");
        assert_eq!(unchanged.active_layer, "quality");
        assert_eq!(unchanged.quality_completed_at, Some("700".to_owned()));
        assert_eq!(unchanged.quality_embedded_chunks, 1);
    }

    #[test]
    fn semantic_routing_summary_is_empty_without_generation() {
        let db = TestDb::new("semantic-routing-empty");
        let store = SqliteStore::open(&db.config()).expect("store should open");
        store.migrate().expect("migration should run");

        assert_eq!(
            store
                .semantic_routing_summary("repo")
                .expect("routing summary should load"),
            None
        );
    }

    #[test]
    fn semantic_routing_summary_reports_fast_ready_manifest() {
        let db = TestDb::new("semantic-routing-fast-ready");
        let mut store = SqliteStore::open(&db.config()).expect("store should open");
        store.migrate().expect("migration should run");
        store
            .upsert_repository(&RepositoryRecord {
                id: "repo".to_owned(),
                root_path: "/tmp/repo".to_owned(),
            })
            .expect("repository should persist");
        store
            .replace_file_facts(
                &sample_file("content-hash"),
                &[],
                &[sample_chunk("chunk-1")],
                &[],
            )
            .expect("file facts should persist");
        let mut generation = sample_semantic_generation();
        generation.quality_model = None;
        generation.quality_dimension = None;
        generation.quality_status = "fast_ready".to_owned();
        store
            .upsert_semantic_generation(&generation)
            .expect("generation should persist");
        store
            .upsert_chunk_embedding(&sample_chunk_embedding())
            .expect("fast embedding should persist");

        let summary = store
            .semantic_routing_summary("repo")
            .expect("routing summary should load")
            .expect("routing summary should exist");

        assert_eq!(summary.active_layer, SemanticLayer::Fast);
        assert_eq!(summary.quality_status, SemanticLayerStatus::FastReady);
        assert_eq!(summary.fast.embedding_model, "nomic-embed-text");
        assert_eq!(summary.fast.current_chunks, 1);
        assert_eq!(summary.fast.expected_chunks, 1);
        assert!(summary.fast.is_complete);
        assert_eq!(summary.quality, None);
    }

    #[test]
    fn semantic_routing_summary_reports_ready_quality_manifest() {
        let db = TestDb::new("semantic-routing-quality-ready");
        let mut store = SqliteStore::open(&db.config()).expect("store should open");
        store.migrate().expect("migration should run");
        store
            .upsert_repository(&RepositoryRecord {
                id: "repo".to_owned(),
                root_path: "/tmp/repo".to_owned(),
            })
            .expect("repository should persist");
        store
            .replace_file_facts(
                &sample_file("content-hash"),
                &[],
                &[sample_chunk("chunk-1")],
                &[],
            )
            .expect("file facts should persist");
        let mut generation = sample_semantic_generation();
        generation.quality_status = "quality_ready".to_owned();
        generation.active_layer = "quality".to_owned();
        generation.quality_embedded_chunks = 1;
        store
            .upsert_semantic_generation(&generation)
            .expect("generation should persist");
        store
            .upsert_chunk_embedding(&sample_chunk_embedding())
            .expect("fast embedding should persist");
        store
            .upsert_chunk_embedding(&sample_quality_chunk_embedding("current"))
            .expect("quality embedding should persist");

        let summary = store
            .semantic_routing_summary("repo")
            .expect("routing summary should load")
            .expect("routing summary should exist");

        let quality = summary.quality.expect("quality summary should exist");
        assert_eq!(summary.active_layer, SemanticLayer::Quality);
        assert_eq!(summary.quality_status, SemanticLayerStatus::QualityReady);
        assert_eq!(quality.embedding_model, "mxbai-embed-large");
        assert_eq!(quality.vector_table, "symdex_repo_nomic_embed_text_v2_moe");
        assert_eq!(quality.current_chunks, 1);
        assert!(quality.is_complete);
    }

    #[test]
    fn semantic_routing_summary_reports_incomplete_quality_manifest() {
        let db = TestDb::new("semantic-routing-quality-incomplete");
        let mut store = SqliteStore::open(&db.config()).expect("store should open");
        store.migrate().expect("migration should run");
        store
            .upsert_repository(&RepositoryRecord {
                id: "repo".to_owned(),
                root_path: "/tmp/repo".to_owned(),
            })
            .expect("repository should persist");
        store
            .replace_file_facts(
                &sample_file("content-hash"),
                &[],
                &[sample_chunk("chunk-1")],
                &[],
            )
            .expect("file facts should persist");
        let mut generation = sample_semantic_generation();
        generation.quality_status = "quality_stale".to_owned();
        generation.quality_embedded_chunks = 1;
        store
            .upsert_semantic_generation(&generation)
            .expect("generation should persist");
        store
            .upsert_chunk_embedding(&sample_chunk_embedding())
            .expect("fast embedding should persist");
        store
            .upsert_chunk_embedding(&sample_quality_chunk_embedding("stale"))
            .expect("quality embedding should persist");

        let summary = store
            .semantic_routing_summary("repo")
            .expect("routing summary should load")
            .expect("routing summary should exist");

        let quality = summary.quality.expect("quality summary should exist");
        assert_eq!(summary.quality_status, SemanticLayerStatus::QualityStale);
        assert_eq!(quality.current_chunks, 0);
        assert_eq!(quality.stale_chunks, 1);
        assert!(!quality.is_complete);
    }

    #[test]
    fn sqlite_records_fast_semantic_generation_from_layered_manifest() {
        let db = TestDb::new("fast-generation-recording");
        let mut store = SqliteStore::open(&db.config()).expect("store should open");
        store.migrate().expect("migration should run");
        store
            .upsert_repository(&RepositoryRecord {
                id: "repo".to_owned(),
                root_path: "/tmp/repo".to_owned(),
            })
            .expect("repository should persist");
        store
            .replace_file_facts(
                &sample_file("content-hash"),
                &[],
                &[sample_chunk("chunk-1"), sample_chunk("chunk-2")],
                &[],
            )
            .expect("file facts should persist");
        let fast_manifest = sample_fast_manifest(&["chunk-1", "chunk-2"], "content-hash");

        let generation = store
            .record_fast_semantic_generation(FastSemanticGenerationInput {
                repository_id: "repo",
                repository_ref_id: None,
                fast_model: "nomic-embed-text",
                fast_dimension: 768,
                vector_table: "symdex_repo_nomic_embed_text",
                upserted_embeddings: &fast_manifest,
                files_seen: 1,
                completed_at: "200",
            })
            .expect("fast generation should persist");

        assert_eq!(generation.quality_status, "fast_ready");
        assert_eq!(generation.active_layer, "fast");
        assert_eq!(generation.fast_model, "nomic-embed-text");
        assert_eq!(generation.fast_dimension, 768);
        assert_eq!(generation.embeddable_chunks, 2);
        assert_eq!(generation.fast_embedded_chunks, 2);
        assert_eq!(generation.quality_embedded_chunks, 0);
        assert_eq!(
            store
                .latest_semantic_generation("repo")
                .expect("latest generation should load")
                .map(|generation| generation.id),
            Some(generation.id.clone())
        );

        let embeddings = store
            .chunk_embeddings_for_generation("repo", &generation.id, "fast")
            .expect("fast manifest should load");
        assert_eq!(embeddings.len(), 2);
        assert!(
            embeddings
                .iter()
                .all(|embedding| embedding.generation_id == generation.id)
        );
        assert!(embeddings.iter().all(|embedding| {
            embedding.semantic_layer == "fast"
                && embedding.embedding_model == "nomic-embed-text"
                && embedding.embedding_dimension == 768
                && embedding.content_hash == "content-hash"
                && embedding.status == "current"
                && !embedding.vector_point_id.is_empty()
        }));
    }

    #[test]
    fn fast_semantic_generation_scopes_carried_embeddings_to_repository_ref() {
        let db = TestDb::new("fast-generation-ref-scoped-manifest");
        let mut store = SqliteStore::open(&db.config()).expect("store should open");
        store.migrate().expect("migration should run");
        store
            .upsert_repository(&RepositoryRecord {
                id: "repo".to_owned(),
                root_path: "/tmp/repo".to_owned(),
            })
            .expect("repository should persist");
        let repository_ref = RepositoryRefSnapshot {
            id: stable_id(&["repository-ref", "repo", "branch", "main"]),
            repository_id: "repo".to_owned(),
            kind: RepositoryRefKind::Branch,
            name: Some("main".to_owned()),
            head_oid: Some("0123456789abcdef0123456789abcdef01234567".to_owned()),
            local_branches: vec!["main".to_owned()],
        };
        store
            .sync_repository_ref(&repository_ref)
            .expect("repository ref should sync");
        store
            .upsert_semantic_generation(&sample_semantic_generation())
            .expect("seed generation should persist");

        let old_file = sample_file_at("old-file", "src/lib.rs", "old-content-hash");
        let mut old_chunk = sample_chunk("old-chunk");
        old_chunk.file_id = old_file.id.clone();
        old_chunk.text_hash = "old-text-hash".to_owned();
        store
            .replace_file_facts_for_ref_with_tests(
                &repository_ref.id,
                &old_file,
                &[],
                &[old_chunk.clone()],
                &[],
                &[],
            )
            .expect("old ref file facts should persist");
        store
            .upsert_chunk_embedding(&ChunkEmbeddingRecord {
                id: "old-fast-embedding".to_owned(),
                file_id: old_file.id.clone(),
                chunk_id: old_chunk.id.clone(),
                content_hash: old_file.content_hash.clone(),
                text_hash: old_chunk.text_hash.clone(),
                vector_point_id: "old-point".to_owned(),
                ..sample_chunk_embedding()
            })
            .expect("old fast embedding should persist");

        let current_file = sample_file_at("current-file", "src/lib.rs", "current-content-hash");
        let mut current_chunk = sample_chunk("current-chunk");
        current_chunk.file_id = current_file.id.clone();
        current_chunk.text_hash = "current-text-hash".to_owned();
        store
            .replace_file_facts_for_ref_with_tests(
                &repository_ref.id,
                &current_file,
                &[],
                &[current_chunk.clone()],
                &[],
                &[],
            )
            .expect("current ref file facts should persist");
        store
            .upsert_chunk_embedding(&ChunkEmbeddingRecord {
                id: "current-fast-embedding".to_owned(),
                file_id: current_file.id.clone(),
                chunk_id: current_chunk.id.clone(),
                content_hash: current_file.content_hash.clone(),
                text_hash: current_chunk.text_hash.clone(),
                vector_point_id: "current-point".to_owned(),
                ..sample_chunk_embedding()
            })
            .expect("current fast embedding should persist");

        let generation = store
            .record_fast_semantic_generation(FastSemanticGenerationInput {
                repository_id: "repo",
                repository_ref_id: Some(&repository_ref.id),
                fast_model: "nomic-embed-text",
                fast_dimension: 768,
                vector_table: "symdex_repo_nomic_embed_text",
                upserted_embeddings: &[],
                files_seen: 1,
                completed_at: "200",
            })
            .expect("ref-scoped generation should persist");

        let embeddings = store
            .chunk_embeddings_for_generation("repo", &generation.id, "fast")
            .expect("fast manifest should load");
        assert_eq!(embeddings.len(), 1);
        assert_eq!(embeddings[0].file_id, current_file.id);
        assert_eq!(embeddings[0].chunk_id, current_chunk.id);
        assert_eq!(generation.embeddable_chunks, 1);
    }

    #[test]
    fn sqlite_fast_semantic_generation_ids_are_manifest_stable() {
        let db = TestDb::new("fast-generation-idempotent");
        let mut store = SqliteStore::open(&db.config()).expect("store should open");
        store.migrate().expect("migration should run");
        store
            .upsert_repository(&RepositoryRecord {
                id: "repo".to_owned(),
                root_path: "/tmp/repo".to_owned(),
            })
            .expect("repository should persist");
        store
            .replace_file_facts(
                &sample_file("content-hash"),
                &[],
                &[sample_chunk("chunk-1")],
                &[],
            )
            .expect("file facts should persist");
        let first_manifest = sample_fast_manifest(&["chunk-1"], "content-hash");

        let first = store
            .record_fast_semantic_generation(FastSemanticGenerationInput {
                repository_id: "repo",
                repository_ref_id: None,
                fast_model: "nomic-embed-text",
                fast_dimension: 768,
                vector_table: "symdex_repo_nomic_embed_text",
                upserted_embeddings: &first_manifest,
                files_seen: 1,
                completed_at: "200",
            })
            .expect("first generation should persist");
        let second = store
            .record_fast_semantic_generation(FastSemanticGenerationInput {
                repository_id: "repo",
                repository_ref_id: None,
                fast_model: "nomic-embed-text",
                fast_dimension: 768,
                vector_table: "symdex_repo_nomic_embed_text",
                upserted_embeddings: &first_manifest,
                files_seen: 1,
                completed_at: "201",
            })
            .expect("same generation should persist idempotently");
        assert_eq!(first.id, second.id);
        assert_eq!(
            store
                .chunk_embeddings_for_generation("repo", &first.id, "fast")
                .expect("fast manifest should load")
                .len(),
            1
        );

        store
            .replace_file_facts(
                &sample_file("content-hash-2"),
                &[],
                &[sample_chunk("chunk-1")],
                &[],
            )
            .expect("changed file facts should persist");
        let changed_manifest = sample_fast_manifest(&["chunk-1"], "content-hash-2");
        let changed = store
            .record_fast_semantic_generation(FastSemanticGenerationInput {
                repository_id: "repo",
                repository_ref_id: None,
                fast_model: "nomic-embed-text",
                fast_dimension: 768,
                vector_table: "symdex_repo_nomic_embed_text",
                upserted_embeddings: &changed_manifest,
                files_seen: 1,
                completed_at: "202",
            })
            .expect("changed generation should persist");

        assert_ne!(first.id, changed.id);
        assert_eq!(
            store
                .latest_semantic_generation("repo")
                .expect("latest generation should load")
                .map(|generation| generation.id),
            Some(changed.id)
        );
    }

    #[test]
    fn sqlite_migration_creates_provenance_columns() {
        let db = TestDb::new("provenance-columns");
        let store = SqliteStore::open(&db.config()).expect("store should open");
        store.migrate().expect("migration should run");

        for (table, column) in [
            ("index_runs", "parser_version"),
            ("index_runs", "indexer_version"),
            ("index_runs", "run_kind"),
            ("files", "index_run_id"),
            ("files", "parser_version"),
            ("symbols", "index_run_id"),
            ("symbols", "parser_version"),
            ("chunks", "index_run_id"),
            ("chunks", "parser_version"),
            ("chunks", "embedding_model"),
            ("chunks", "embedding_dimension"),
            ("chunks", "embedded_at"),
            ("calls", "index_run_id"),
            ("calls", "parser_version"),
            ("tests", "repository_id"),
            ("tests", "symbol_id"),
            ("tests", "qualified_name"),
            ("tests", "framework"),
            ("tests", "parser_version"),
        ] {
            assert!(
                store
                    .column_exists(table, column)
                    .expect("column check should run"),
                "missing {table}.{column}"
            );
        }
    }

    #[test]
    fn sqlite_replaces_chunks_and_removes_deleted_files() {
        let db = TestDb::new("cleanup");
        let mut store = SqliteStore::open(&db.config()).expect("store should open");
        store.migrate().expect("migration should run");
        store
            .upsert_repository(&RepositoryRecord {
                id: "repo".to_owned(),
                root_path: "/tmp/repo".to_owned(),
            })
            .expect("repository should persist");
        store
            .replace_file_facts(&sample_file("hash-1"), &[], &[sample_chunk("chunk-1")], &[])
            .expect("initial chunks should persist");
        store
            .replace_file_facts(
                &sample_file("hash-2"),
                &[],
                &[sample_chunk("chunk-2"), sample_chunk("chunk-3")],
                &[],
            )
            .expect("replacement chunks should persist");

        let status = store.repository_status("repo").expect("status should load");
        assert_eq!(status.files_indexed, 1);
        assert_eq!(status.chunks_indexed, 2);
        assert!(
            !store
                .file_unchanged("repo", "file", "src/lib.rs", "hash-1", "parser")
                .expect("unchanged check should run")
        );

        let removed = store
            .remove_missing_files("repo", &[])
            .expect("cleanup should run");
        assert_eq!(removed, 1);
        let status = store.repository_status("repo").expect("status should load");
        assert_eq!(status.files_indexed, 0);
        assert_eq!(status.chunks_indexed, 0);
    }

    #[test]
    fn sqlite_persists_replaces_and_queries_tests() {
        let db = TestDb::new("tests");
        let mut store = SqliteStore::open(&db.config()).expect("store should open");
        store.migrate().expect("migration should run");
        store
            .upsert_repository(&RepositoryRecord {
                id: "repo".to_owned(),
                root_path: "/tmp/repo".to_owned(),
            })
            .expect("repository should persist");
        let symbols = vec![
            sample_symbol("target-symbol", "target", "target"),
            sample_symbol("test-symbol", "covers_target", "tests::covers_target"),
        ];
        let calls = vec![sample_call(
            "call-test-target",
            "test-symbol",
            "target",
            Some("target-symbol"),
            8,
        )];
        let tests = vec![sample_test("test-1", "test-symbol", "tests::covers_target")];
        store
            .replace_file_facts_with_tests(&sample_file("hash-1"), &symbols, &[], &calls, &tests)
            .expect("test facts should persist");

        let matched = store
            .tests_matching_name("repo", "tests::covers_target")
            .expect("test lookup should run");
        assert_eq!(matched.len(), 1);
        assert_eq!(matched[0].framework, "rust_test");
        let likely = store
            .likely_tests_for_symbol("repo", "target")
            .expect("likely tests should load");
        assert_eq!(likely.len(), 1);
        assert_eq!(likely[0].qualified_name, "tests::covers_target");

        store
            .replace_file_facts_with_tests(&sample_file("hash-2"), &symbols[..1], &[], &[], &[])
            .expect("replacement should remove tests");
        assert!(
            store
                .likely_tests_for_symbol("repo", "target")
                .expect("likely tests should load")
                .is_empty()
        );
    }

    #[test]
    fn sqlite_persists_metadata_only_non_rust_tests_without_likely_claims() {
        let db = TestDb::new("metadata-only-tests");
        let mut store = SqliteStore::open(&db.config()).expect("store should open");
        store.migrate().expect("migration should run");
        store
            .upsert_repository(&RepositoryRecord {
                id: "repo".to_owned(),
                root_path: "/tmp/repo".to_owned(),
            })
            .expect("repository should persist");
        let symbols = vec![sample_symbol("target-symbol", "target", "target")];
        let tests = vec![sample_metadata_test(
            "test-js-inline",
            "inline works",
            "src::math.test::math::inline works",
        )];
        store
            .replace_file_facts_with_tests(&sample_file("hash-1"), &symbols, &[], &[], &tests)
            .expect("metadata-only test facts should persist");

        let matched = store
            .tests_matching_name("repo", "inline works")
            .expect("test lookup should run");
        assert_eq!(matched.len(), 1);
        assert_eq!(matched[0].framework, "jest");
        assert_eq!(matched[0].language, "javascript");
        assert!(
            store
                .likely_tests_for_symbol("repo", "target")
                .expect("likely tests should load")
                .is_empty()
        );
    }

    #[test]
    fn sqlite_collects_layered_vector_point_ids_before_replacement_and_deletion() {
        let db = TestDb::new("vector-point-cleanup");
        let mut store = SqliteStore::open(&db.config()).expect("store should open");
        store.migrate().expect("migration should run");
        store
            .upsert_repository(&RepositoryRecord {
                id: "repo".to_owned(),
                root_path: "/tmp/repo".to_owned(),
            })
            .expect("repository should persist");

        let mut other_file = sample_file("hash-other");
        other_file.id = "other-file".to_owned();
        other_file.path = "src/other.rs".to_owned();
        let mut other_chunk = sample_chunk("other-chunk");
        other_chunk.file_id = "other-file".to_owned();
        other_chunk.vector_point_id = Some("11111111-1111-1111-1111-111111111111".to_owned());

        store
            .replace_file_facts(&sample_file("hash-1"), &[], &[sample_chunk("chunk-1")], &[])
            .expect("file chunks should persist");
        store
            .replace_file_facts(&other_file, &[], &[other_chunk], &[])
            .expect("other file chunks should persist");
        store
            .upsert_semantic_generation(&sample_semantic_generation())
            .expect("generation should persist");
        store
            .upsert_chunk_embedding(&sample_chunk_embedding())
            .expect("lib embedding should persist");
        store
            .upsert_chunk_embedding(&ChunkEmbeddingRecord {
                id: "embedding-other".to_owned(),
                file_id: "other-file".to_owned(),
                chunk_id: "other-chunk".to_owned(),
                content_hash: "hash-other".to_owned(),
                text_hash: "text-other-chunk".to_owned(),
                vector_point_id: "11111111-1111-1111-1111-111111111111".to_owned(),
                ..sample_chunk_embedding()
            })
            .expect("other embedding should persist");

        let replaced = store
            .vector_point_ids_for_latest_generation_layer_paths(
                "repo",
                SemanticLayer::Fast,
                &["src/lib.rs".to_owned()],
            )
            .expect("point ids should load");
        assert_eq!(replaced, vec!["01234567-89ab-cdef-fedc-ba9876543210"]);

        let missing = store
            .vector_point_ids_for_latest_generation_layer_missing_files(
                "repo",
                SemanticLayer::Fast,
                &["src/lib.rs".to_owned()],
            )
            .expect("missing point ids should load");
        assert_eq!(missing, vec!["11111111-1111-1111-1111-111111111111"]);
    }

    #[test]
    fn sqlite_builds_layered_expected_vector_point_manifest() {
        let db = TestDb::new("layered-vector-expected-points");
        let mut store = SqliteStore::open(&db.config()).expect("store should open");
        store.migrate().expect("migration should run");
        store
            .upsert_repository(&RepositoryRecord {
                id: "repo".to_owned(),
                root_path: "/tmp/repo".to_owned(),
            })
            .expect("repository should persist");
        store
            .replace_file_facts(
                &sample_file("content-hash"),
                &[],
                &[sample_chunk("chunk-1")],
                &[],
            )
            .expect("facts should persist");
        store
            .upsert_semantic_generation(&sample_semantic_generation())
            .expect("generation should persist");
        store
            .upsert_chunk_embedding(&sample_chunk_embedding())
            .expect("fast embedding should persist");
        store
            .upsert_chunk_embedding(&sample_quality_chunk_embedding("current"))
            .expect("quality embedding should persist");

        let fast = store
            .expected_vector_points_for_generation_layer(
                "repo",
                "generation-1",
                SemanticLayer::Fast,
            )
            .expect("fast manifest should load");
        let quality = store
            .expected_vector_points_for_generation_layer(
                "repo",
                "generation-1",
                SemanticLayer::Quality,
            )
            .expect("quality manifest should load");

        assert_eq!(fast.len(), 1);
        assert_eq!(fast[0].embedding_model.as_deref(), Some("nomic-embed-text"));
        assert_eq!(quality.len(), 1);
        assert_eq!(
            quality[0].embedding_model.as_deref(),
            Some("mxbai-embed-large")
        );
        assert_eq!(
            quality[0].vector_point_id,
            "01234567-89ab-cdef-fedc-ba9876543211"
        );
    }

    #[test]
    fn sqlite_layered_vector_manifest_uses_current_embeddings_only() {
        let db = TestDb::new("layered-vector-current-only");
        let mut store = SqliteStore::open(&db.config()).expect("store should open");
        store.migrate().expect("migration should run");
        store
            .upsert_repository(&RepositoryRecord {
                id: "repo".to_owned(),
                root_path: "/tmp/repo".to_owned(),
            })
            .expect("repository should persist");
        store
            .replace_file_facts(
                &sample_file("content-hash"),
                &[],
                &[sample_chunk("chunk-1")],
                &[],
            )
            .expect("facts should persist");
        store
            .upsert_semantic_generation(&sample_semantic_generation())
            .expect("generation should persist");
        store
            .upsert_chunk_embedding(&sample_quality_chunk_embedding("stale"))
            .expect("stale quality embedding should persist");

        let quality = store
            .expected_vector_points_for_generation_layer(
                "repo",
                "generation-1",
                SemanticLayer::Quality,
            )
            .expect("quality manifest should load");

        assert!(quality.is_empty());
    }

    #[test]
    fn sqlite_persists_index_provenance_metadata() {
        let db = TestDb::new("provenance-values");
        let mut store = SqliteStore::open(&db.config()).expect("store should open");
        store.migrate().expect("migration should run");
        store
            .upsert_repository(&RepositoryRecord {
                id: "repo".to_owned(),
                root_path: "/tmp/repo".to_owned(),
            })
            .expect("repository should persist");

        let symbols = vec![sample_symbol("symbol", "caller", "caller")];
        let calls = vec![CallRecord {
            id: "call-1".to_owned(),
            caller_symbol_id: "symbol".to_owned(),
            callee_text: "helper".to_owned(),
            callee_symbol_id: None,
            call_line: 4,
            confidence: 0.25,
            resolution_status: "unresolved".to_owned(),
            index_run_id: "run".to_owned(),
            parser_version: "parser".to_owned(),
        }];
        store
            .replace_file_facts(
                &sample_file("hash-1"),
                &symbols,
                &[sample_chunk("chunk-1")],
                &calls,
            )
            .expect("facts should persist");
        store
            .record_index_run(&sample_index_run("nomic-embed-text", 768))
            .expect("index run should persist");

        let file_provenance: (String, String) = store
            .connection
            .query_row(
                "SELECT index_run_id, parser_version FROM files WHERE id = 'file'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("file provenance should load");
        assert_eq!(file_provenance, ("run".to_owned(), "parser".to_owned()));

        let chunk_provenance: (String, String, Option<String>, Option<i64>, Option<String>) = store
            .connection
            .query_row(
                "SELECT index_run_id, parser_version, embedding_model,
                        embedding_dimension, embedded_at
                 FROM chunks
                 WHERE id = 'chunk-1'",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .expect("chunk provenance should load");
        assert_eq!(chunk_provenance.0, "run");
        assert_eq!(chunk_provenance.1, "parser");
        assert_eq!(chunk_provenance.2, None);
        assert_eq!(chunk_provenance.3, None);
        assert_eq!(chunk_provenance.4, None);

        let symbol_provenance: (String, String) = store
            .connection
            .query_row(
                "SELECT index_run_id, parser_version FROM symbols WHERE id = 'symbol'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("symbol provenance should load");
        assert_eq!(symbol_provenance, ("run".to_owned(), "parser".to_owned()));

        let call_provenance: (String, String) = store
            .connection
            .query_row(
                "SELECT index_run_id, parser_version FROM calls WHERE id = 'call-1'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("call provenance should load");
        assert_eq!(call_provenance, ("run".to_owned(), "parser".to_owned()));

        let run_provenance: (String, String, String) = store
            .connection
            .query_row(
                "SELECT parser_version, indexer_version, run_kind FROM index_runs LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("run provenance should load");
        assert_eq!(
            run_provenance,
            (
                "parser".to_owned(),
                "indexer".to_owned(),
                "semantic".to_owned()
            )
        );
    }

    #[test]
    fn sqlite_persists_symbols_and_calls_for_queries() {
        let db = TestDb::new("symbols-calls");
        let mut store = SqliteStore::open(&db.config()).expect("store should open");
        store.migrate().expect("migration should run");
        store
            .upsert_repository(&RepositoryRecord {
                id: "repo".to_owned(),
                root_path: "/tmp/repo".to_owned(),
            })
            .expect("repository should persist");

        let symbols = vec![
            sample_symbol("caller-symbol", "caller", "caller"),
            sample_symbol("callee-symbol", "helper", "helper"),
        ];
        let calls = vec![CallRecord {
            id: "call-1".to_owned(),
            caller_symbol_id: "caller-symbol".to_owned(),
            callee_text: "helper".to_owned(),
            callee_symbol_id: Some("callee-symbol".to_owned()),
            call_line: 4,
            confidence: 1.0,
            resolution_status: "resolved_exact".to_owned(),
            index_run_id: "run".to_owned(),
            parser_version: "parser".to_owned(),
        }];
        store
            .replace_file_facts(
                &sample_file("hash-1"),
                &symbols,
                &[sample_chunk("chunk-1")],
                &calls,
            )
            .expect("facts should persist");

        let status = store.repository_status("repo").expect("status should load");
        assert_eq!(status.symbols_indexed, 2);
        assert_eq!(status.calls_indexed, 1);

        let found = store.find_symbols("repo", "helper").expect("symbol search");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].qualified_name, "helper");
        assert_eq!(found[0].provenance.content_hash.as_deref(), Some("hash-1"));
        assert_eq!(found[0].provenance.index_run_id.as_deref(), Some("run"));

        let callers = store.callers("repo", "helper").expect("callers query");
        assert_eq!(callers.len(), 1);
        assert_eq!(callers[0].symbol_qualified_name.as_deref(), Some("caller"));
        assert_eq!(
            callers[0].provenance.parser_version.as_deref(),
            Some("parser")
        );

        let callees = store.callees("repo", "caller").expect("callees query");
        assert_eq!(callees.len(), 1);
        assert_eq!(callees[0].symbol_qualified_name.as_deref(), Some("helper"));
    }

    #[test]
    fn sqlite_persists_symbol_references() {
        let db = TestDb::new("symbol-references");
        let mut store = SqliteStore::open(&db.config()).expect("store should open");
        store.migrate().expect("migration should run");
        store
            .upsert_repository(&RepositoryRecord {
                id: "repo".to_owned(),
                root_path: "/tmp/repo".to_owned(),
            })
            .expect("repository should persist");

        let symbols = vec![sample_symbol("source-symbol", "run", "run")];
        let references = vec![SymbolReferenceRecord {
            id: "reference-1".to_owned(),
            file_id: "file".to_owned(),
            source_symbol_id: Some("source-symbol".to_owned()),
            target_symbol_id: None,
            reference_text: "use crate::worker::Task;".to_owned(),
            reference_kind: "import".to_owned(),
            line: 2,
            confidence: 0.4,
            resolution_status: "unresolved".to_owned(),
            index_run_id: "run".to_owned(),
            parser_version: "parser".to_owned(),
        }];

        store
            .replace_file_facts_with_references_and_tests(
                &sample_file("hash-1"),
                &symbols,
                &[],
                &[],
                &references,
                &[],
            )
            .expect("symbol references should persist");

        let row: (String, String, i64, f64, String) = store
            .connection
            .query_row(
                "SELECT source_symbol_id, reference_kind, line, confidence, resolution_status
                   FROM symbol_references
                  WHERE id = 'reference-1'",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .expect("symbol reference should load");
        assert_eq!(row.0, "source-symbol");
        assert_eq!(row.1, "import");
        assert_eq!(row.2, 2);
        assert_eq!(row.4, "unresolved");
    }

    #[test]
    fn sqlite_traces_bounded_call_paths_deterministically() {
        let db = TestDb::new("call-paths");
        let mut store = SqliteStore::open(&db.config()).expect("store should open");
        store.migrate().expect("migration should run");
        store
            .upsert_repository(&RepositoryRecord {
                id: "repo".to_owned(),
                root_path: "/tmp/repo".to_owned(),
            })
            .expect("repository should persist");

        let symbols = vec![
            sample_symbol("a-symbol", "a", "crate::a"),
            sample_symbol("b-symbol", "b", "crate::b"),
            sample_symbol("c-symbol", "c", "crate::c"),
        ];
        let calls = vec![
            sample_call("call-a-b", "a-symbol", "b", Some("b-symbol"), 10),
            sample_call("call-b-c", "b-symbol", "c", Some("c-symbol"), 20),
            sample_call("call-a-c", "a-symbol", "c", Some("c-symbol"), 30),
            sample_call("call-c-a", "c-symbol", "a", Some("a-symbol"), 40),
            unresolved_call("call-a-missing", "a-symbol", "missing", 50),
        ];
        store
            .replace_file_facts(&sample_file("hash-1"), &symbols, &[], &calls)
            .expect("calls should persist");

        let shallow = store
            .call_paths("repo", "crate::a", "crate::c", 1)
            .expect("shallow paths should trace");
        assert_eq!(shallow.len(), 1);
        assert_eq!(shallow[0].hops, 1);
        assert_eq!(shallow[0].edges[0].call_id, "call-a-c");

        let paths = store
            .call_paths("repo", "crate::a", "crate::c", 2)
            .expect("paths should trace");
        assert_eq!(paths.len(), 2);
        assert_eq!(paths[0].hops, 2);
        assert_eq!(paths[0].edges[0].call_id, "call-a-b");
        assert_eq!(paths[0].edges[1].call_id, "call-b-c");
        assert_eq!(paths[1].hops, 1);
        assert_eq!(paths[1].edges[0].call_id, "call-a-c");

        let cyclic = store
            .call_paths("repo", "crate::a", "crate::c", 8)
            .expect("cycle-safe paths should trace");
        assert_eq!(cyclic.len(), 2);
    }

    #[test]
    fn sqlite_traces_unresolved_terminal_call_path_by_callee_text() {
        let db = TestDb::new("call-paths-unresolved");
        let mut store = SqliteStore::open(&db.config()).expect("store should open");
        store.migrate().expect("migration should run");
        store
            .upsert_repository(&RepositoryRecord {
                id: "repo".to_owned(),
                root_path: "/tmp/repo".to_owned(),
            })
            .expect("repository should persist");

        let symbols = vec![sample_symbol("a-symbol", "a", "crate::a")];
        let calls = vec![unresolved_call("call-a-missing", "a-symbol", "missing", 10)];
        store
            .replace_file_facts(&sample_file("hash-1"), &symbols, &[], &calls)
            .expect("calls should persist");

        let paths = store
            .call_paths("repo", "crate::a", "missing", 2)
            .expect("unresolved terminal path should trace");

        assert_eq!(paths.len(), 1);
        assert_eq!(paths[0].hops, 1);
        assert_eq!(paths[0].terminal_resolution_status, "unresolved");
        assert_eq!(paths[0].edges[0].callee_symbol_id, None);
        assert_eq!(paths[0].edges[0].callee_text, "missing");
    }

    #[test]
    fn sqlite_traces_ambiguous_terminal_call_path_by_callee_text() {
        let db = TestDb::new("call-paths-ambiguous");
        let mut store = SqliteStore::open(&db.config()).expect("store should open");
        store.migrate().expect("migration should run");
        store
            .upsert_repository(&RepositoryRecord {
                id: "repo".to_owned(),
                root_path: "/tmp/repo".to_owned(),
            })
            .expect("repository should persist");

        let symbols = vec![sample_symbol("a-symbol", "a", "crate::a")];
        let mut ambiguous = unresolved_call("call-a-helper", "a-symbol", "helper", 10);
        ambiguous.resolution_status = "ambiguous".to_owned();
        ambiguous.confidence = 0.5;
        store
            .replace_file_facts(&sample_file("hash-1"), &symbols, &[], &[ambiguous])
            .expect("calls should persist");

        let paths = store
            .call_paths("repo", "crate::a", "helper", 2)
            .expect("ambiguous terminal path should trace");

        assert_eq!(paths.len(), 1);
        assert_eq!(paths[0].hops, 1);
        assert_eq!(paths[0].terminal_resolution_status, "ambiguous");
        assert_eq!(paths[0].edges[0].callee_symbol_id, None);
        assert_eq!(paths[0].edges[0].callee_text, "helper");
    }

    #[test]
    fn sqlite_reports_transitive_impact_paths_deterministically() {
        let db = TestDb::new("transitive-impact-paths");
        let mut store = SqliteStore::open(&db.config()).expect("store should open");
        store.migrate().expect("migration should run");
        store
            .upsert_repository(&RepositoryRecord {
                id: "repo".to_owned(),
                root_path: "/tmp/repo".to_owned(),
            })
            .expect("repository should persist");

        let symbols = vec![
            sample_symbol("a-symbol", "a", "crate::a"),
            sample_symbol("b-symbol", "b", "crate::b"),
            sample_symbol("c-symbol", "c", "crate::c"),
            sample_symbol("d-symbol", "d", "crate::d"),
        ];
        let calls = vec![
            sample_call("call-a-b", "a-symbol", "b", Some("b-symbol"), 10),
            sample_call("call-b-c", "b-symbol", "c", Some("c-symbol"), 20),
            sample_call("call-c-d", "c-symbol", "d", Some("d-symbol"), 30),
        ];
        store
            .replace_file_facts(&sample_file("hash-1"), &symbols, &[], &calls)
            .expect("calls should persist");

        let callers = store
            .transitive_call_paths_to("repo", "crate::c", 3)
            .expect("transitive callers should trace");
        assert_eq!(callers.len(), 1);
        assert_eq!(callers[0].hops, 2);
        assert_eq!(callers[0].edges[0].call_id, "call-a-b");
        assert_eq!(callers[0].edges[1].call_id, "call-b-c");

        let callees = store
            .transitive_call_paths_from("repo", "crate::a", 3)
            .expect("transitive callees should trace");
        assert_eq!(callees.len(), 2);
        assert_eq!(callees[0].hops, 2);
        assert_eq!(callees[0].edges[1].call_id, "call-b-c");
        assert_eq!(callees[1].hops, 3);
        assert_eq!(callees[1].edges[2].call_id, "call-c-d");
    }

    #[test]
    fn sqlite_builds_compact_context_pack() {
        let db = TestDb::new("context-pack");
        let mut store = SqliteStore::open(&db.config()).expect("store should open");
        store.migrate().expect("migration should run");
        store
            .upsert_repository(&RepositoryRecord {
                id: "repo".to_owned(),
                root_path: "/tmp/repo".to_owned(),
            })
            .expect("repository should persist");

        let symbols = vec![
            sample_symbol("caller-symbol", "caller", "caller"),
            sample_symbol("callee-symbol", "helper", "helper"),
        ];
        let calls = vec![CallRecord {
            id: "call-1".to_owned(),
            caller_symbol_id: "caller-symbol".to_owned(),
            callee_text: "helper".to_owned(),
            callee_symbol_id: Some("callee-symbol".to_owned()),
            call_line: 4,
            confidence: 1.0,
            resolution_status: "resolved_exact".to_owned(),
            index_run_id: "run".to_owned(),
            parser_version: "parser".to_owned(),
        }];
        store
            .replace_file_facts(
                &sample_file("hash-1"),
                &symbols,
                &[sample_chunk("chunk-1")],
                &calls,
            )
            .expect("facts should persist");

        let pack = store
            .context_pack("repo", "helper", 5)
            .expect("context pack should build");

        assert_eq!(pack.format, "symdex.context_pack.v1");
        assert_eq!(pack.focus_symbols.len(), 1);
        assert_eq!(pack.direct_callers.len(), 1);
        assert!(pack.direct_callees.is_empty());
        assert_eq!(pack.files, vec!["src/lib.rs"]);
        let json = serde_json::to_value(&pack).expect("pack should serialize");
        assert_eq!(json["notes"][0], "metadata_only_no_source_text");
        assert!(json.get("source_text").is_none());
    }

    #[test]
    fn sqlite_replaces_calls_without_unique_conflict() {
        let db = TestDb::new("replace-calls");
        let mut store = SqliteStore::open(&db.config()).expect("store should open");
        store.migrate().expect("migration should run");
        store
            .upsert_repository(&RepositoryRecord {
                id: "repo".to_owned(),
                root_path: "/tmp/repo".to_owned(),
            })
            .expect("repository should persist");

        let symbols = vec![
            sample_symbol("caller-symbol", "caller", "caller"),
            sample_symbol("callee-symbol", "helper", "helper"),
        ];
        store
            .replace_file_facts(
                &sample_file("hash-1"),
                &symbols,
                &[sample_chunk("chunk-1")],
                &[CallRecord {
                    id: "call-1".to_owned(),
                    caller_symbol_id: "caller-symbol".to_owned(),
                    callee_text: "helper".to_owned(),
                    callee_symbol_id: Some("callee-symbol".to_owned()),
                    call_line: 4,
                    confidence: 1.0,
                    resolution_status: "resolved_exact".to_owned(),
                    index_run_id: "run".to_owned(),
                    parser_version: "parser".to_owned(),
                }],
            )
            .expect("initial calls should persist");

        store
            .replace_file_facts(
                &sample_file("hash-2"),
                &symbols,
                &[sample_chunk("chunk-2")],
                &[CallRecord {
                    id: "call-1".to_owned(),
                    caller_symbol_id: "caller-symbol".to_owned(),
                    callee_text: "helper_updated".to_owned(),
                    callee_symbol_id: Some("callee-symbol".to_owned()),
                    call_line: 5,
                    confidence: 0.9,
                    resolution_status: "resolved_local_candidate".to_owned(),
                    index_run_id: "run".to_owned(),
                    parser_version: "parser".to_owned(),
                }],
            )
            .expect("replacement calls should persist");

        let status = store.repository_status("repo").expect("status should load");
        assert_eq!(status.calls_indexed, 1);

        let callees = store.callees("repo", "caller").expect("callees query");
        assert_eq!(callees.len(), 1);
        assert_eq!(callees[0].callee_text, "helper_updated");
        assert_eq!(callees[0].call_line, 5);
    }

    #[test]
    fn sqlite_records_embedding_runs_and_rejects_dimension_changes() {
        let db = TestDb::new("embedding-runs");
        let store = SqliteStore::open(&db.config()).expect("store should open");
        store.migrate().expect("migration should run");
        store
            .upsert_repository(&RepositoryRecord {
                id: "repo".to_owned(),
                root_path: "/tmp/repo".to_owned(),
            })
            .expect("repository should persist");

        store
            .record_index_run(&sample_index_run("nomic-embed-text", 768))
            .expect("index run should persist");

        let status = store.repository_status("repo").expect("status should load");
        assert_eq!(status.embedding_model.as_deref(), Some("nomic-embed-text"));
        assert_eq!(status.embedding_dimension, Some(768));
        store
            .ensure_embedding_compatible("repo", "nomic-embed-text", 768)
            .expect("same dimension should be compatible");
        let error = store
            .ensure_embedding_compatible("repo", "nomic-embed-text", 1024)
            .expect_err("changed dimension should be rejected");
        assert!(matches!(
            error,
            StoreError::EmbeddingDimensionChanged {
                previous_dimension: 768,
                current_dimension: 1024,
                ..
            }
        ));
        store
            .ensure_embedding_compatible("repo", "different-model", 1024)
            .expect("different model writes to a different collection");
    }

    #[test]
    fn sqlite_builds_storage_explorer_summary_without_source_text() {
        let db = TestDb::new("storage-explorer");
        let mut store = SqliteStore::open(&db.config()).expect("store should open");
        store.migrate().expect("migration should run");
        store
            .upsert_repository(&RepositoryRecord {
                id: "repo".to_owned(),
                root_path: "/tmp/repo".to_owned(),
            })
            .expect("repository should persist");

        let mut missing_vector = sample_chunk("chunk-missing-vector");
        missing_vector.vector_point_id = None;
        let mut excluded = sample_chunk("chunk-excluded");
        excluded.vector_point_id = None;
        excluded.excluded_reason = Some("secret_detected".to_owned());
        store
            .replace_file_facts(
                &sample_file("hash-1"),
                &[sample_symbol("symbol", "add", "crate::add")],
                &[sample_chunk("chunk-vector"), missing_vector, excluded],
                &[],
            )
            .expect("facts should persist");
        upsert_fast_embedding_for_chunk(&store, "chunk-vector", "hash-1", "text-chunk-vector");
        store
            .record_index_run(&sample_index_run("nomic-embed-text", 768))
            .expect("index run should persist");

        let summary = store
            .storage_explorer_summary("repo", "nomic-embed-text")
            .expect("storage summary should build");

        assert_eq!(summary.sqlite.repositories, 1);
        assert_eq!(summary.sqlite.files, 1);
        assert_eq!(summary.sqlite.chunks, 3);
        assert_eq!(summary.sqlite.symbols, 1);
        assert_eq!(summary.sqlite.index_runs, 1);
        assert_eq!(summary.vector.embedding_model, "nomic-embed-text");
        assert_eq!(summary.vector.embedding_dimension, Some(768));
        assert_eq!(summary.vector.embeddable_chunks, 2);
        assert_eq!(summary.vector.vector_backed_chunks, 1);
        assert_eq!(summary.vector.excluded_chunks, 1);
        assert_eq!(summary.vector.missing_vector_chunks, 1);
        assert!(summary.vector.collection_name.starts_with("symdex_repo_"));
        assert!(summary.warnings.iter().any(
            |row| row.status == StorageHealthStatus::Warning && row.label == "missing_vectors"
        ));
        let debug = format!("{summary:?}");
        assert!(!debug.contains("source_text"));
    }

    #[test]
    fn sqlite_builds_embedding_coverage_summary() {
        let db = TestDb::new("embedding-coverage");
        let mut store = SqliteStore::open(&db.config()).expect("store should open");
        store.migrate().expect("migration should run");
        store
            .upsert_repository(&RepositoryRecord {
                id: "repo".to_owned(),
                root_path: "/tmp/repo".to_owned(),
            })
            .expect("repository should persist");

        store
            .replace_file_facts(
                &sample_file("hash-1"),
                &[sample_symbol("symbol", "add", "crate::add")],
                &[
                    sample_chunk("chunk-vector"),
                    missing_vector_chunk("chunk-missing"),
                    excluded_chunk("chunk-secret", "file"),
                ],
                &[],
            )
            .expect("facts should persist");
        upsert_fast_embedding_for_chunk(&store, "chunk-vector", "hash-1", "text-chunk-vector");
        store
            .record_index_run(&sample_index_run("nomic-embed-text", 768))
            .expect("index run should persist");

        let summary = store
            .embedding_coverage_summary("repo", "nomic-embed-text")
            .expect("embedding coverage should load");

        assert_eq!(summary.repository_id, "repo");
        assert_eq!(summary.configured_embedding_model, "nomic-embed-text");
        assert_eq!(summary.embedding_model, "nomic-embed-text");
        assert_eq!(summary.embedding_dimension, Some(768));
        assert_eq!(summary.total_chunks, 3);
        assert_eq!(summary.embeddable_chunks, 2);
        assert_eq!(summary.vector_backed_chunks, 1);
        assert_eq!(summary.missing_vector_chunks, 1);
        assert_eq!(summary.excluded_chunks, 1);
        assert_eq!(summary.latest_chunks_embedded, Some(1));
        assert_eq!(summary.exclusion_reasons.len(), 1);
        assert_eq!(summary.exclusion_reasons[0].reason, "secret_detected");
        assert_eq!(summary.exclusion_reasons[0].chunks, 1);
        assert!(summary.collection_name.starts_with("symdex_repo_"));
        assert!(summary.health.iter().any(
            |row| row.status == StorageHealthStatus::Warning && row.label == "missing_vectors"
        ));
        let debug = format!("{summary:?}");
        assert!(!debug.contains("source_text"));
    }

    #[test]
    fn sqlite_builds_index_runs_timeline_summary() {
        let db = TestDb::new("index-runs-timeline");
        let store = SqliteStore::open(&db.config()).expect("store should open");
        store.migrate().expect("migration should run");
        store
            .upsert_repository(&RepositoryRecord {
                id: "repo".to_owned(),
                root_path: "/tmp/repo".to_owned(),
            })
            .expect("repository should persist");
        insert_index_run_fixture(
            &store,
            IndexRunFixture {
                id: "run-old",
                started_at: "2026-01-01T00:00:00Z",
                finished_at: Some("2026-01-01T00:00:10Z"),
                status: "success",
                model: "nomic-embed-text",
                dimension: Some(768),
                files_seen: 4,
                files_indexed: 3,
                chunks_embedded: 7,
                error_summary: None,
                run_kind: "semantic",
            },
        );
        insert_index_run_fixture(
            &store,
            IndexRunFixture {
                id: "run-new",
                started_at: "2026-01-02T00:00:00Z",
                finished_at: Some("2026-01-02T00:00:04Z"),
                status: "failed",
                model: "nomic-embed-text",
                dimension: Some(768),
                files_seen: 5,
                files_indexed: 2,
                chunks_embedded: 1,
                error_summary: Some("vector store unavailable"),
                run_kind: "watch",
            },
        );

        let summary = store
            .index_runs_timeline_summary("repo")
            .expect("timeline summary should load");

        assert_eq!(summary.repository_id, "repo");
        assert_eq!(summary.runs.len(), 2);
        assert_eq!(summary.runs[0].id, "run-new");
        assert_eq!(summary.runs[0].status, "failed");
        assert_eq!(summary.runs[0].files_seen, 5);
        assert_eq!(summary.runs[0].files_indexed, 2);
        assert_eq!(summary.runs[0].chunks_embedded, 1);
        assert_eq!(summary.runs[0].run_kind, "watch");
        assert_eq!(
            summary.runs[0].error_summary.as_deref(),
            Some("vector store unavailable")
        );
        assert_eq!(summary.runs[1].id, "run-old");
        assert_eq!(summary.runs[1].run_kind, "semantic");
        let debug = format!("{summary:?}");
        assert!(!debug.contains("source_text"));
    }

    #[test]
    fn sqlite_tracks_started_and_finished_index_runs() {
        let db = TestDb::new("index-run-lifecycle");
        let store = SqliteStore::open(&db.config()).expect("store should open");
        store.migrate().expect("migration should run");
        store
            .upsert_repository(&RepositoryRecord {
                id: "repo".to_owned(),
                root_path: "/tmp/repo".to_owned(),
            })
            .expect("repository should persist");

        let mut run = sample_index_run("nomic-embed-text", 768);
        run.id = "run-lifecycle".to_owned();
        run.status = "running".to_owned();
        run.files_seen = 0;
        run.files_indexed = 0;
        run.chunks_embedded = 0;
        store
            .start_index_run(&run)
            .expect("started run should persist");

        let started: (String, Option<String>, i64, i64) = store
            .connection
            .query_row(
                "SELECT status, finished_at, files_seen, chunks_embedded
                 FROM index_runs
                 WHERE id = 'run-lifecycle'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .expect("started run should load");
        assert_eq!(started, ("running".to_owned(), None, 0, 0));

        run.status = "partial".to_owned();
        run.files_seen = 4;
        run.files_indexed = 3;
        run.error_summary = Some("vector store unavailable".to_owned());
        store
            .finish_index_run(&run)
            .expect("finished run should persist");

        let finished: (String, Option<String>, i64, i64, Option<String>) = store
            .connection
            .query_row(
                "SELECT status, finished_at, files_seen, files_indexed, error_summary
                 FROM index_runs
                 WHERE id = 'run-lifecycle'",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .expect("finished run should load");
        assert_eq!(finished.0, "partial");
        assert!(finished.1.is_some());
        assert_eq!(finished.2, 4);
        assert_eq!(finished.3, 3);
        assert_eq!(finished.4.as_deref(), Some("vector store unavailable"));
    }

    #[test]
    fn sqlite_records_per_file_index_events() {
        let db = TestDb::new("file-index-events");
        let mut store = SqliteStore::open(&db.config()).expect("store should open");
        store.migrate().expect("migration should run");
        store
            .upsert_repository(&RepositoryRecord {
                id: "repo".to_owned(),
                root_path: "/tmp/repo".to_owned(),
            })
            .expect("repository should persist");
        let mut run = sample_index_run("offline", 0);
        run.id = "run-events".to_owned();
        store
            .start_index_run(&run)
            .expect("index run should persist");

        store
            .record_file_index_events(&[
                FileIndexEventRecord {
                    id: "event-created".to_owned(),
                    index_run_id: "run-events".to_owned(),
                    repository_id: "repo".to_owned(),
                    repository_ref_id: None,
                    path: "src/lib.rs".to_owned(),
                    old_content_hash: None,
                    new_content_hash: Some("hash-new".to_owned()),
                    action: "created".to_owned(),
                    reason: "new_file".to_owned(),
                    status: "success".to_owned(),
                    error_summary: None,
                },
                FileIndexEventRecord {
                    id: "event-skipped".to_owned(),
                    index_run_id: "run-events".to_owned(),
                    repository_id: "repo".to_owned(),
                    repository_ref_id: None,
                    path: "src/unchanged.rs".to_owned(),
                    old_content_hash: Some("hash-same".to_owned()),
                    new_content_hash: Some("hash-same".to_owned()),
                    action: "skipped".to_owned(),
                    reason: "unchanged_content_hash".to_owned(),
                    status: "skipped".to_owned(),
                    error_summary: None,
                },
            ])
            .expect("file events should persist");

        let rows: Vec<(
            String,
            Option<String>,
            Option<String>,
            String,
            String,
            String,
        )> = store
            .connection
            .prepare(
                "SELECT path, old_content_hash, new_content_hash, action, reason, status
                   FROM file_index_events
                  WHERE index_run_id = 'run-events'
                  ORDER BY path",
            )
            .expect("query should prepare")
            .query_map([], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            })
            .expect("query should run")
            .collect::<rusqlite::Result<Vec<_>>>()
            .expect("rows should load");

        assert_eq!(rows.len(), 2);
        assert_eq!(
            rows[0],
            (
                "src/lib.rs".to_owned(),
                None,
                Some("hash-new".to_owned()),
                "created".to_owned(),
                "new_file".to_owned(),
                "success".to_owned(),
            )
        );
        assert_eq!(rows[1].3, "skipped");
        assert_eq!(rows[1].5, "skipped");
    }

    #[test]
    fn sqlite_persists_watcher_status() {
        let db = TestDb::new("watcher-status");
        let store = SqliteStore::open(&db.config()).expect("store should open");
        store.migrate().expect("migration should run");
        store
            .upsert_repository(&RepositoryRecord {
                id: "repo".to_owned(),
                root_path: "/tmp/repo".to_owned(),
            })
            .expect("repository should persist");
        store
            .upsert_watcher_status(&WatcherStatusRecord {
                repository_id: "repo".to_owned(),
                root_path: "/tmp/repo".to_owned(),
                mode: "semantic".to_owned(),
                owner_kind: "daemon".to_owned(),
                owner_pid: Some(123),
                socket_path: Some("/tmp/repo/.symdex/watch.sock".to_owned()),
                state: "running".to_owned(),
                started_at: Some("1".to_owned()),
                updated_at: Some("1".to_owned()),
                heartbeat_at: Some("1".to_owned()),
                files_seen: 3,
                queued_events: 2,
                last_indexed_path: Some("src/lib.rs".to_owned()),
                last_error: None,
                active_layer: Some("fast".to_owned()),
                quality_status: Some("quality_pending".to_owned()),
                quality_pending_jobs: 1,
                quality_running_jobs: 0,
                quality_failed_jobs: 0,
                quality_stale_jobs: 0,
            })
            .expect("watcher should persist");

        let status = store
            .watcher_status("repo")
            .expect("watcher status should load")
            .expect("watcher should exist");

        assert_eq!(status.state, "running");
        assert_eq!(status.files_seen, 3);
        assert_eq!(status.queued_events, 2);
        assert_eq!(status.last_indexed_path.as_deref(), Some("src/lib.rs"));
    }

    #[test]
    fn sqlite_persists_and_prunes_watcher_clients() {
        let db = TestDb::new("watcher-clients");
        let store = SqliteStore::open(&db.config()).expect("store should open");
        store.migrate().expect("migration should run");
        store
            .upsert_repository(&RepositoryRecord {
                id: "repo".to_owned(),
                root_path: "/tmp/repo".to_owned(),
            })
            .expect("repository should persist");
        store
            .upsert_watcher_client(&WatcherClientRecord {
                repository_id: "repo".to_owned(),
                client_id: "mcp-1".to_owned(),
                client_kind: "mcp".to_owned(),
                pid: Some(123),
                started_at: Some("1".to_owned()),
                heartbeat_at: Some("1".to_owned()),
                last_seen_at: None,
            })
            .expect("client should persist");

        let clients = store.watcher_clients("repo").expect("clients should load");
        assert_eq!(clients.len(), 1);
        assert_eq!(clients[0].client_kind, "mcp");

        store
            .heartbeat_watcher_client("repo", "mcp-1")
            .expect("heartbeat should update");
        let clients = store.watcher_clients("repo").expect("clients should load");
        assert_ne!(clients[0].heartbeat_at.as_deref(), Some("1"));

        let updated = store
            .heartbeat_watcher_client("repo", "missing")
            .expect("missing heartbeat should not fail");
        assert_eq!(updated, 0);

        let removed = store
            .prune_stale_watcher_clients("repo", "999999999999")
            .expect("stale clients should prune");
        assert_eq!(removed, 1);
        assert!(
            store
                .watcher_clients("repo")
                .expect("clients should load")
                .is_empty()
        );
    }

    #[test]
    fn quality_worker_requeues_current_stale_jobs() {
        let db = TestDb::new("quality-worker-requeue-current-stale");
        let mut store = SqliteStore::open(&db.config()).expect("store should open");
        store.migrate().expect("migration should run");
        store
            .upsert_repository(&RepositoryRecord {
                id: "repo".to_owned(),
                root_path: "/tmp/repo".to_owned(),
            })
            .expect("repository should persist");
        store
            .replace_file_facts(
                &sample_file("content-hash"),
                &[sample_symbol("symbol", "hello", "hello")],
                &[sample_chunk("chunk-1")],
                &[],
            )
            .expect("file facts should persist");
        store
            .upsert_semantic_generation(&sample_semantic_generation())
            .expect("generation should persist");
        store
            .upsert_quality_embedding_job(&sample_quality_embedding_job_for(
                "generation-1",
                "chunk-1",
                "skipped_stale",
            ))
            .expect("stale job should persist");
        store
            .upsert_chunk_embedding(&sample_chunk_embedding())
            .expect("fast embedding should persist");

        let requeued = store
            .requeue_current_terminal_quality_embedding_jobs("repo", "generation-1", "600")
            .expect("current stale jobs should requeue");

        assert_eq!(requeued, 1);
        let jobs = store
            .quality_jobs_by_status("repo", "generation-1", "pending")
            .expect("pending jobs should load");
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].error_summary, None);
        assert_eq!(jobs[0].updated_at, "600");
    }

    #[test]
    fn quality_worker_does_not_requeue_mismatched_stale_jobs() {
        let db = TestDb::new("quality-worker-keeps-mismatched-stale");
        let mut store = SqliteStore::open(&db.config()).expect("store should open");
        store.migrate().expect("migration should run");
        store
            .upsert_repository(&RepositoryRecord {
                id: "repo".to_owned(),
                root_path: "/tmp/repo".to_owned(),
            })
            .expect("repository should persist");
        store
            .replace_file_facts(
                &sample_file("new-content-hash"),
                &[sample_symbol("symbol", "hello", "hello")],
                &[sample_chunk("chunk-1")],
                &[],
            )
            .expect("file facts should persist");
        store
            .upsert_semantic_generation(&sample_semantic_generation())
            .expect("generation should persist");
        store
            .upsert_quality_embedding_job(&sample_quality_embedding_job_for(
                "generation-1",
                "chunk-1",
                "skipped_stale",
            ))
            .expect("stale job should persist");

        let requeued = store
            .requeue_current_terminal_quality_embedding_jobs("repo", "generation-1", "600")
            .expect("mismatched stale jobs should be ignored");

        assert_eq!(requeued, 0);
        assert_eq!(
            store
                .quality_jobs_by_status("repo", "generation-1", "skipped_stale")
                .expect("stale jobs should load")
                .len(),
            1
        );
    }

    #[test]
    fn quality_worker_requeues_current_transient_failed_jobs() {
        let db = TestDb::new("quality-worker-requeue-transient-failed");
        let mut store = SqliteStore::open(&db.config()).expect("store should open");
        store.migrate().expect("migration should run");
        store
            .upsert_repository(&RepositoryRecord {
                id: "repo".to_owned(),
                root_path: "/tmp/repo".to_owned(),
            })
            .expect("repository should persist");
        store
            .replace_file_facts(
                &sample_file("content-hash"),
                &[sample_symbol("symbol", "hello", "hello")],
                &[sample_chunk("chunk-1")],
                &[],
            )
            .expect("file facts should persist");
        store
            .upsert_semantic_generation(&sample_semantic_generation())
            .expect("generation should persist");
        let mut job = sample_quality_embedding_job_for("generation-1", "chunk-1", "failed");
        job.error_summary = Some("SQLite error: database is locked".to_owned());
        store
            .upsert_quality_embedding_job(&job)
            .expect("failed job should persist");
        store
            .upsert_chunk_embedding(&sample_chunk_embedding())
            .expect("fast embedding should persist");

        let requeued = store
            .requeue_current_terminal_quality_embedding_jobs("repo", "generation-1", "600")
            .expect("transient failed jobs should requeue");

        assert_eq!(requeued, 1);
        let jobs = store
            .quality_jobs_by_status("repo", "generation-1", "pending")
            .expect("pending jobs should load");
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].error_summary, None);
    }

    #[test]
    fn quality_worker_keeps_non_transient_failed_jobs() {
        let db = TestDb::new("quality-worker-keeps-non-transient-failed");
        let mut store = SqliteStore::open(&db.config()).expect("store should open");
        store.migrate().expect("migration should run");
        store
            .upsert_repository(&RepositoryRecord {
                id: "repo".to_owned(),
                root_path: "/tmp/repo".to_owned(),
            })
            .expect("repository should persist");
        store
            .replace_file_facts(
                &sample_file("content-hash"),
                &[sample_symbol("symbol", "hello", "hello")],
                &[sample_chunk("chunk-1")],
                &[],
            )
            .expect("file facts should persist");
        store
            .upsert_semantic_generation(&sample_semantic_generation())
            .expect("generation should persist");
        let mut job = sample_quality_embedding_job_for("generation-1", "chunk-1", "failed");
        job.error_summary = Some("embedding model returned an invalid vector".to_owned());
        store
            .upsert_quality_embedding_job(&job)
            .expect("failed job should persist");

        let requeued = store
            .requeue_current_terminal_quality_embedding_jobs("repo", "generation-1", "600")
            .expect("non-transient failed jobs should be ignored");

        assert_eq!(requeued, 0);
        assert_eq!(
            store
                .quality_jobs_by_status("repo", "generation-1", "failed")
                .expect("failed jobs should load")
                .len(),
            1
        );
    }

    #[test]
    fn sqlite_builds_semantic_neighborhood_summary_without_source_text() {
        let db = TestDb::new("semantic-neighborhood");
        let mut store = SqliteStore::open(&db.config()).expect("store should open");
        store.migrate().expect("migration should run");
        store
            .upsert_repository(&RepositoryRecord {
                id: "repo".to_owned(),
                root_path: "/tmp/repo".to_owned(),
            })
            .expect("repository should persist");

        let mut vector = sample_chunk("chunk-vector");
        vector.text_hash = "hash-vector".to_owned();
        let mut missing = missing_vector_chunk("chunk-missing");
        missing.text_hash = "hash-missing".to_owned();
        store
            .replace_file_facts(
                &sample_file("hash-1"),
                &[sample_symbol("symbol", "add", "crate::add")],
                &[vector, missing],
                &[],
            )
            .expect("facts should persist");
        upsert_fast_embedding_for_chunk(&store, "chunk-vector", "hash-1", "hash-vector");
        store
            .record_index_run(&sample_index_run("nomic-embed-text", 768))
            .expect("index run should persist");

        let summary = store
            .semantic_neighborhood_summary("repo", "nomic-embed-text")
            .expect("semantic neighborhood should load");

        assert_eq!(summary.repository_id, "repo");
        assert_eq!(summary.embedding_model, "nomic-embed-text");
        assert_eq!(summary.rows.len(), 1);
        assert_eq!(summary.rows[0].path, "src/lib.rs");
        assert_eq!(summary.rows[0].symbol_name.as_deref(), Some("crate::add"));
        assert_eq!(summary.rows[0].chunk_kind, "function");
        assert_eq!(summary.rows[0].language, "rust");
        assert_eq!(summary.rows[0].score, None);
        assert_eq!(summary.rows[0].text_hash, "hash-vector");
        assert!(
            summary
                .health
                .iter()
                .any(|row| row.status == StorageHealthStatus::Ok && row.label == "metadata_only")
        );
        let debug = format!("{summary:?}");
        assert!(!debug.contains("source_text"));
    }

    #[test]
    fn sqlite_builds_cross_store_health_warnings() {
        let db = TestDb::new("cross-store-health");
        let mut store = SqliteStore::open(&db.config()).expect("store should open");
        store.migrate().expect("migration should run");
        store
            .upsert_repository(&RepositoryRecord {
                id: "repo".to_owned(),
                root_path: "/tmp/repo".to_owned(),
            })
            .expect("repository should persist");

        store
            .replace_file_facts(
                &sample_file("hash-1"),
                &[sample_symbol("symbol", "add", "crate::add")],
                &[
                    sample_chunk("chunk-vector"),
                    missing_vector_chunk("chunk-missing"),
                    excluded_chunk("chunk-secret", "file"),
                ],
                &[],
            )
            .expect("facts should persist");
        insert_index_run_fixture(
            &store,
            IndexRunFixture {
                id: "run-drift-768",
                started_at: "2026-01-01T00:00:00Z",
                finished_at: Some("2026-01-01T00:00:10Z"),
                status: "success",
                model: "different-model",
                dimension: Some(768),
                files_seen: 1,
                files_indexed: 1,
                chunks_embedded: 1,
                error_summary: None,
                run_kind: "semantic",
            },
        );
        insert_index_run_fixture(
            &store,
            IndexRunFixture {
                id: "run-drift-1024",
                started_at: "2026-01-02T00:00:00Z",
                finished_at: Some("2026-01-02T00:00:10Z"),
                status: "success",
                model: "different-model",
                dimension: Some(1024),
                files_seen: 1,
                files_indexed: 1,
                chunks_embedded: 1,
                error_summary: None,
                run_kind: "semantic",
            },
        );

        let summary = store
            .cross_store_health_summary("repo", "nomic-embed-text")
            .expect("health summary should load");

        assert_eq!(summary.repository_id, "repo");
        assert!(summary.rows.iter().any(
            |row| row.status == StorageHealthStatus::Warning && row.label == "missing_vectors"
        ));
        assert!(summary.rows.iter().any(
            |row| row.status == StorageHealthStatus::Warning && row.label == "excluded_chunks"
        ));
        assert!(
            summary
                .rows
                .iter()
                .any(|row| row.status == StorageHealthStatus::Error && row.label == "model_drift")
        );
        assert!(
            summary
                .rows
                .iter()
                .any(|row| row.status == StorageHealthStatus::Error
                    && row.label == "dimension_drift")
        );
        let debug = format!("{summary:?}");
        assert!(!debug.contains("source_text"));
    }

    #[test]
    fn sqlite_flags_missing_collection_health() {
        let db = TestDb::new("missing-collection-health");
        let mut store = SqliteStore::open(&db.config()).expect("store should open");
        store.migrate().expect("migration should run");
        store
            .upsert_repository(&RepositoryRecord {
                id: "repo".to_owned(),
                root_path: "/tmp/repo".to_owned(),
            })
            .expect("repository should persist");
        store
            .replace_file_facts(
                &sample_file("hash-1"),
                &[],
                &[missing_vector_chunk("chunk-missing")],
                &[],
            )
            .expect("facts should persist");

        let summary = store
            .cross_store_health_summary("repo", "nomic-embed-text")
            .expect("health summary should load");

        assert!(summary.rows.iter().any(
            |row| row.status == StorageHealthStatus::Error && row.label == "missing_collection"
        ));
    }

    #[test]
    fn sqlite_builds_file_index_coverage_summary() {
        let db = TestDb::new("index-coverage");
        let mut store = SqliteStore::open(&db.config()).expect("store should open");
        store.migrate().expect("migration should run");
        store
            .upsert_repository(&RepositoryRecord {
                id: "repo".to_owned(),
                root_path: "/tmp/repo".to_owned(),
            })
            .expect("repository should persist");

        let symbols = vec![
            sample_symbol("caller-symbol", "caller", "crate::caller"),
            sample_symbol("callee-symbol", "helper", "crate::helper"),
        ];
        store
            .replace_file_facts(
                &sample_file("hash-1"),
                &symbols,
                &[
                    sample_chunk("chunk-vector"),
                    missing_vector_chunk("chunk-missing"),
                ],
                &[CallRecord {
                    id: "call-1".to_owned(),
                    caller_symbol_id: "caller-symbol".to_owned(),
                    callee_text: "helper".to_owned(),
                    callee_symbol_id: Some("callee-symbol".to_owned()),
                    call_line: 4,
                    confidence: 1.0,
                    resolution_status: "resolved_exact".to_owned(),
                    index_run_id: "run".to_owned(),
                    parser_version: "parser".to_owned(),
                }],
            )
            .expect("first file facts should persist");
        upsert_fast_embedding_for_chunk(&store, "chunk-vector", "hash-1", "text-chunk-vector");

        let second_file = FileRecord {
            id: "file-secret".to_owned(),
            repository_id: "repo".to_owned(),
            path: "src/secret.rs".to_owned(),
            language: "rust".to_owned(),
            content_hash: "hash-secret".to_owned(),
            index_run_id: "run".to_owned(),
            parser_version: "parser".to_owned(),
        };
        store
            .replace_file_facts(
                &second_file,
                &[],
                &[excluded_chunk("secret-chunk", "file-secret")],
                &[],
            )
            .expect("second file facts should persist");

        let coverage = store
            .index_coverage_summary("repo")
            .expect("coverage should load");

        assert_eq!(coverage.repository_id, "repo");
        assert_eq!(coverage.files.len(), 2);
        let lib = coverage
            .files
            .iter()
            .find(|file| file.path == "src/lib.rs")
            .expect("lib coverage row");
        assert_eq!(lib.language, "rust");
        assert_eq!(lib.chunks, 2);
        assert_eq!(lib.symbols, 2);
        assert_eq!(lib.calls, 1);
        assert_eq!(lib.embeddable_chunks, 2);
        assert_eq!(lib.vector_backed_chunks, 1);
        assert_eq!(lib.excluded_chunks, 0);
        assert_eq!(lib.status, FileCoverageStatus::MissingVector);
        assert_eq!(lib.detail.chunks.len(), 2);
        assert!(
            lib.detail
                .chunks
                .iter()
                .any(|chunk| chunk.vector_status == ChunkVectorStatus::MissingVector)
        );
        assert_eq!(lib.detail.symbols.len(), 2);
        assert_eq!(lib.detail.calls.len(), 1);
        assert_eq!(lib.detail.calls[0].callee_text, "helper");
        assert_eq!(lib.detail.calls[0].resolution_status, "resolved_exact");

        let secret = coverage
            .files
            .iter()
            .find(|file| file.path == "src/secret.rs")
            .expect("secret coverage row");
        assert_eq!(secret.chunks, 1);
        assert_eq!(secret.embeddable_chunks, 0);
        assert_eq!(secret.excluded_chunks, 1);
        assert_eq!(secret.status, FileCoverageStatus::Excluded);
        assert_eq!(
            secret.detail.chunks[0].vector_status,
            ChunkVectorStatus::Excluded
        );
        assert_eq!(
            secret.detail.chunks[0].excluded_reason.as_deref(),
            Some("secret_detected")
        );
    }

    #[test]
    fn sqlite_builds_symbol_outline_with_parent_depths() {
        let db = TestDb::new("symbol-outline");
        let mut store = SqliteStore::open(&db.config()).expect("store should open");
        store.migrate().expect("migration should run");
        store
            .upsert_repository(&RepositoryRecord {
                id: "repo".to_owned(),
                root_path: "/tmp/repo".to_owned(),
            })
            .expect("repository should persist");

        let mut parent = sample_symbol("parent-symbol", "Service", "crate::Service");
        parent.kind = "struct".to_owned();
        let mut child = sample_symbol("child-symbol", "run", "crate::Service::run");
        child.parent_symbol_id = Some("parent-symbol".to_owned());
        child.start_line = 5;
        child.end_line = 8;
        let mut grandchild = sample_symbol("grandchild-symbol", "inner", "crate::Service::inner");
        grandchild.parent_symbol_id = Some("child-symbol".to_owned());
        grandchild.start_line = 6;
        grandchild.end_line = 7;

        store
            .replace_file_facts(
                &sample_file("hash-1"),
                &[parent, child, grandchild],
                &[],
                &[],
            )
            .expect("symbols should persist");

        let outline = store
            .symbol_outline_summary("repo")
            .expect("outline should load");

        assert_eq!(outline.repository_id, "repo");
        assert_eq!(outline.symbols.len(), 3);
        let parent = outline
            .symbols
            .iter()
            .find(|symbol| symbol.id == "parent-symbol")
            .expect("parent symbol row");
        assert_eq!(parent.depth, 0);
        assert_eq!(parent.child_count, 1);
        assert_eq!(parent.kind, "struct");

        let child = outline
            .symbols
            .iter()
            .find(|symbol| symbol.id == "child-symbol")
            .expect("child symbol row");
        assert_eq!(child.parent_symbol_id.as_deref(), Some("parent-symbol"));
        assert_eq!(child.depth, 1);
        assert_eq!(child.child_count, 1);

        let grandchild = outline
            .symbols
            .iter()
            .find(|symbol| symbol.id == "grandchild-symbol")
            .expect("grandchild symbol row");
        assert_eq!(grandchild.depth, 2);
    }

    #[test]
    fn sqlite_builds_call_resolution_summary_by_status_and_confidence() {
        let db = TestDb::new("call-resolution");
        let mut store = SqliteStore::open(&db.config()).expect("store should open");
        store.migrate().expect("migration should run");
        store
            .upsert_repository(&RepositoryRecord {
                id: "repo".to_owned(),
                root_path: "/tmp/repo".to_owned(),
            })
            .expect("repository should persist");

        let symbols = vec![
            sample_symbol("caller-symbol", "caller", "crate::caller"),
            sample_symbol("callee-symbol", "helper", "crate::helper"),
        ];
        let calls = vec![
            CallRecord {
                id: "call-high".to_owned(),
                caller_symbol_id: "caller-symbol".to_owned(),
                callee_text: "helper".to_owned(),
                callee_symbol_id: Some("callee-symbol".to_owned()),
                call_line: 4,
                confidence: 1.0,
                resolution_status: "resolved_exact".to_owned(),
                index_run_id: "run".to_owned(),
                parser_version: "parser".to_owned(),
            },
            CallRecord {
                id: "call-low".to_owned(),
                caller_symbol_id: "caller-symbol".to_owned(),
                callee_text: "missing".to_owned(),
                callee_symbol_id: None,
                call_line: 7,
                confidence: 0.25,
                resolution_status: "unresolved".to_owned(),
                index_run_id: "run".to_owned(),
                parser_version: "parser".to_owned(),
            },
        ];
        store
            .replace_file_facts(&sample_file("hash-1"), &symbols, &[], &calls)
            .expect("calls should persist");

        let summary = store
            .call_resolution_summary("repo")
            .expect("call resolution summary should load");

        assert_eq!(summary.repository_id, "repo");
        assert_eq!(summary.buckets.len(), 2);
        let resolved = summary
            .buckets
            .iter()
            .find(|bucket| bucket.resolution_status == "resolved_exact")
            .expect("resolved bucket");
        assert_eq!(resolved.confidence_bucket, ConfidenceBucket::High);
        assert_eq!(resolved.call_count, 1);
        assert_eq!(resolved.rows[0].caller_symbol, "crate::caller");
        assert_eq!(resolved.rows[0].callee_text, "helper");

        let unresolved = summary
            .buckets
            .iter()
            .find(|bucket| bucket.resolution_status == "unresolved")
            .expect("unresolved bucket");
        assert_eq!(unresolved.confidence_bucket, ConfidenceBucket::Low);
        assert_eq!(unresolved.call_count, 1);
        assert_eq!(unresolved.rows[0].call_line, 7);
    }

    #[test]
    fn sqlite_vec_health_is_available() {
        if std::env::var("SYMDEX_TEST_SQLITE_VEC").ok().as_deref() != Some("1") {
            return;
        }

        let client = SqliteVectorStore::new(&StoreConfig::from_env()).expect("client should build");
        client
            .health_check()
            .expect("sqlite-vec should be available");
    }

    fn sample_payload() -> PointPayload {
        PointPayload {
            repository_id: "repo".to_owned(),
            file_id: "file".to_owned(),
            chunk_id: "chunk".to_owned(),
            symbol_id: Some("symbol".to_owned()),
            symbol_name: Some("add".to_owned()),
            path: "src/lib.rs".to_owned(),
            language: "rust".to_owned(),
            chunk_kind: "function".to_owned(),
            start_line: 1,
            end_line: 3,
            text_hash: "hash".to_owned(),
            parser_version: Some("parser".to_owned()),
            content_hash: Some("content-hash".to_owned()),
            index_run_id: Some("run".to_owned()),
            embedding_model: Some("nomic-embed-text".to_owned()),
            embedding_dimension: Some(768),
            indexed_at: Some("123".to_owned()),
        }
    }

    fn sample_file(content_hash: &str) -> FileRecord {
        sample_file_at("file", "src/lib.rs", content_hash)
    }

    fn sample_file_at(id: &str, path: &str, content_hash: &str) -> FileRecord {
        FileRecord {
            id: id.to_owned(),
            repository_id: "repo".to_owned(),
            path: path.to_owned(),
            language: "rust".to_owned(),
            content_hash: content_hash.to_owned(),
            index_run_id: "run".to_owned(),
            parser_version: "parser".to_owned(),
        }
    }

    fn sample_chunk(id: &str) -> ChunkRecord {
        ChunkRecord {
            id: id.to_owned(),
            file_id: "file".to_owned(),
            symbol_id: Some("symbol".to_owned()),
            kind: "function".to_owned(),
            text_hash: format!("text-{id}"),
            start_line: 1,
            end_line: 3,
            start_byte: 0,
            end_byte: 32,
            vector_point_id: Some("01234567-89ab-cdef-fedc-ba9876543210".to_owned()),
            excluded_reason: None,
            index_run_id: "run".to_owned(),
            parser_version: "parser".to_owned(),
            embedding_model: None,
            embedding_dimension: None,
            embedded_at: None,
        }
    }

    fn missing_vector_chunk(id: &str) -> ChunkRecord {
        let mut chunk = sample_chunk(id);
        chunk.vector_point_id = None;
        chunk
    }

    fn excluded_chunk(id: &str, file_id: &str) -> ChunkRecord {
        ChunkRecord {
            id: id.to_owned(),
            file_id: file_id.to_owned(),
            symbol_id: None,
            kind: "function".to_owned(),
            text_hash: format!("text-{id}"),
            start_line: 1,
            end_line: 3,
            start_byte: 0,
            end_byte: 32,
            vector_point_id: None,
            excluded_reason: Some("secret_detected".to_owned()),
            index_run_id: "run".to_owned(),
            parser_version: "parser".to_owned(),
            embedding_model: None,
            embedding_dimension: None,
            embedded_at: None,
        }
    }

    fn sample_symbol(id: &str, name: &str, qualified_name: &str) -> SymbolRecord {
        sample_symbol_in_file(id, "file", name, qualified_name)
    }

    fn sample_symbol_in_file(
        id: &str,
        file_id: &str,
        name: &str,
        qualified_name: &str,
    ) -> SymbolRecord {
        SymbolRecord {
            id: id.to_owned(),
            file_id: file_id.to_owned(),
            parent_symbol_id: None,
            name: name.to_owned(),
            qualified_name: qualified_name.to_owned(),
            kind: "function".to_owned(),
            signature: Some(format!("fn {name}()")),
            start_line: 1,
            end_line: 3,
            start_byte: 0,
            end_byte: 32,
            index_run_id: "run".to_owned(),
            parser_version: "parser".to_owned(),
        }
    }

    fn sample_call(
        id: &str,
        caller_symbol_id: &str,
        callee_text: &str,
        callee_symbol_id: Option<&str>,
        call_line: usize,
    ) -> CallRecord {
        CallRecord {
            id: id.to_owned(),
            caller_symbol_id: caller_symbol_id.to_owned(),
            callee_text: callee_text.to_owned(),
            callee_symbol_id: callee_symbol_id.map(str::to_owned),
            call_line,
            confidence: 1.0,
            resolution_status: "resolved_exact".to_owned(),
            index_run_id: "run".to_owned(),
            parser_version: "parser".to_owned(),
        }
    }

    fn sample_test(id: &str, symbol_id: &str, qualified_name: &str) -> TestRecord {
        TestRecord {
            id: id.to_owned(),
            repository_id: "repo".to_owned(),
            file_id: "file".to_owned(),
            symbol_id: Some(symbol_id.to_owned()),
            name: qualified_name
                .rsplit("::")
                .next()
                .unwrap_or(qualified_name)
                .to_owned(),
            qualified_name: qualified_name.to_owned(),
            framework: "rust_test".to_owned(),
            language: "rust".to_owned(),
            path: "src/lib.rs".to_owned(),
            start_line: 6,
            end_line: 10,
            start_byte: 64,
            end_byte: 128,
            index_run_id: "run".to_owned(),
            parser_version: "parser".to_owned(),
        }
    }

    fn sample_metadata_test(id: &str, name: &str, qualified_name: &str) -> TestRecord {
        TestRecord {
            id: id.to_owned(),
            repository_id: "repo".to_owned(),
            file_id: "file".to_owned(),
            symbol_id: None,
            name: name.to_owned(),
            qualified_name: qualified_name.to_owned(),
            framework: "jest".to_owned(),
            language: "javascript".to_owned(),
            path: "src/math.test.js".to_owned(),
            start_line: 10,
            end_line: 10,
            start_byte: 128,
            end_byte: 192,
            index_run_id: "run".to_owned(),
            parser_version: "parser".to_owned(),
        }
    }

    fn unresolved_call(
        id: &str,
        caller_symbol_id: &str,
        callee_text: &str,
        call_line: usize,
    ) -> CallRecord {
        let mut call = sample_call(id, caller_symbol_id, callee_text, None, call_line);
        call.confidence = 0.25;
        call.resolution_status = "unresolved".to_owned();
        call
    }

    fn sample_index_run(model: &str, dimension: usize) -> crate::IndexRunRecord {
        crate::IndexRunRecord {
            id: format!("run-{model}-{dimension}"),
            repository_id: "repo".to_owned(),
            repository_ref_id: None,
            status: "success".to_owned(),
            embedding_model: model.to_owned(),
            embedding_dimension: Some(dimension),
            files_seen: 1,
            files_indexed: 1,
            chunks_embedded: 1,
            error_summary: None,
            parser_version: "parser".to_owned(),
            indexer_version: "indexer".to_owned(),
            run_kind: "semantic".to_owned(),
        }
    }

    fn sample_semantic_generation() -> SemanticGenerationRecord {
        SemanticGenerationRecord {
            id: "generation-1".to_owned(),
            repository_id: "repo".to_owned(),
            fast_model: "nomic-embed-text".to_owned(),
            fast_dimension: 768,
            fast_completed_at: "100".to_owned(),
            quality_model: Some("mxbai-embed-large".to_owned()),
            quality_dimension: Some(768),
            quality_status: "quality_pending".to_owned(),
            quality_started_at: None,
            quality_completed_at: None,
            active_layer: "fast".to_owned(),
            files_seen: 1,
            embeddable_chunks: 1,
            fast_embedded_chunks: 1,
            quality_embedded_chunks: 0,
            created_at: "100".to_owned(),
            updated_at: "100".to_owned(),
        }
    }

    fn persist_repo_with_chunks(store: &mut SqliteStore, chunks: &[ChunkRecord]) {
        store
            .upsert_repository(&RepositoryRecord {
                id: "repo".to_owned(),
                root_path: "/tmp/repo".to_owned(),
            })
            .expect("repository should persist");
        store
            .replace_file_facts(&sample_file("content-hash"), &[], chunks, &[])
            .expect("file facts should persist");
    }

    fn sample_chunk_embedding() -> ChunkEmbeddingRecord {
        ChunkEmbeddingRecord {
            id: "embedding-1".to_owned(),
            repository_id: "repo".to_owned(),
            file_id: "file".to_owned(),
            chunk_id: "chunk-1".to_owned(),
            semantic_layer: "fast".to_owned(),
            embedding_model: "nomic-embed-text".to_owned(),
            embedding_dimension: 768,
            content_hash: "content-hash".to_owned(),
            text_hash: "text-chunk-1".to_owned(),
            vector_table: "symdex_repo_nomic_embed_text".to_owned(),
            vector_point_id: "01234567-89ab-cdef-fedc-ba9876543210".to_owned(),
            generation_id: "generation-1".to_owned(),
            embedded_at: "101".to_owned(),
            status: "current".to_owned(),
        }
    }

    fn sample_fast_manifest(
        chunk_ids: &[&str],
        content_hash: &str,
    ) -> Vec<FastEmbeddingManifestRecord> {
        chunk_ids
            .iter()
            .map(|chunk_id| FastEmbeddingManifestRecord {
                file_id: "file".to_owned(),
                chunk_id: (*chunk_id).to_owned(),
                content_hash: content_hash.to_owned(),
                text_hash: format!("text-{chunk_id}"),
                vector_point_id: format!("point-{chunk_id}"),
            })
            .collect()
    }

    fn upsert_fast_embedding_for_chunk(
        store: &SqliteStore,
        chunk_id: &str,
        content_hash: &str,
        text_hash: &str,
    ) {
        store
            .upsert_semantic_generation(&sample_semantic_generation())
            .expect("generation should persist");
        store
            .upsert_chunk_embedding(&ChunkEmbeddingRecord {
                id: format!("embedding-{chunk_id}"),
                chunk_id: chunk_id.to_owned(),
                content_hash: content_hash.to_owned(),
                text_hash: text_hash.to_owned(),
                ..sample_chunk_embedding()
            })
            .expect("fast embedding should persist");
    }

    fn sample_quality_chunk_embedding(status: &str) -> ChunkEmbeddingRecord {
        ChunkEmbeddingRecord {
            id: format!("quality-embedding-{status}"),
            semantic_layer: "quality".to_owned(),
            embedding_model: "mxbai-embed-large".to_owned(),
            vector_table: "symdex_repo_nomic_embed_text_v2_moe".to_owned(),
            vector_point_id: "01234567-89ab-cdef-fedc-ba9876543211".to_owned(),
            status: status.to_owned(),
            ..sample_chunk_embedding()
        }
    }

    fn sample_quality_embedding_job() -> QualityEmbeddingJobRecord {
        QualityEmbeddingJobRecord {
            id: "quality-job-1".to_owned(),
            repository_id: "repo".to_owned(),
            generation_id: "generation-1".to_owned(),
            chunk_id: "chunk-1".to_owned(),
            file_id: "file".to_owned(),
            path: "src/lib.rs".to_owned(),
            content_hash: "content-hash".to_owned(),
            text_hash: "text-chunk-1".to_owned(),
            status: "pending".to_owned(),
            attempts: 0,
            error_summary: None,
            created_at: "102".to_owned(),
            updated_at: "102".to_owned(),
        }
    }

    fn sample_quality_embedding_job_for(
        generation_id: &str,
        chunk_id: &str,
        status: &str,
    ) -> QualityEmbeddingJobRecord {
        QualityEmbeddingJobRecord {
            id: SqliteStore::quality_embedding_job_id("repo", generation_id, chunk_id),
            generation_id: generation_id.to_owned(),
            chunk_id: chunk_id.to_owned(),
            text_hash: format!("text-{chunk_id}"),
            status: status.to_owned(),
            ..sample_quality_embedding_job()
        }
    }

    struct IndexRunFixture {
        id: &'static str,
        started_at: &'static str,
        finished_at: Option<&'static str>,
        status: &'static str,
        model: &'static str,
        dimension: Option<usize>,
        files_seen: usize,
        files_indexed: usize,
        chunks_embedded: usize,
        error_summary: Option<&'static str>,
        run_kind: &'static str,
    }

    fn insert_index_run_fixture(store: &SqliteStore, fixture: IndexRunFixture) {
        store
            .connection
            .execute(
                "INSERT INTO index_runs (
                   id, repository_id, started_at, finished_at, status, embedding_model,
                   embedding_dimension, files_seen, files_indexed, chunks_embedded,
                                     error_summary, run_kind
                 )
                                 VALUES (?1, 'repo', ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                params![
                    fixture.id,
                    fixture.started_at,
                    fixture.finished_at,
                    fixture.status,
                    fixture.model,
                    fixture.dimension.map(|dimension| dimension as i64),
                    fixture.files_seen as i64,
                    fixture.files_indexed as i64,
                    fixture.chunks_embedded as i64,
                    fixture.error_summary,
                    fixture.run_kind,
                ],
            )
            .expect("index run fixture should insert");
    }

    fn sqlite_index_names(store: &SqliteStore) -> Vec<String> {
        let mut statement = store
            .connection
            .prepare(
                "SELECT name FROM sqlite_master
                 WHERE type = 'index'
                   AND name NOT LIKE 'sqlite_autoindex%'
                 ORDER BY name",
            )
            .expect("index query should prepare");
        let rows = statement
            .query_map([], |row| row.get::<_, String>(0))
            .expect("index query should run");
        rows.map(|row| row.expect("index row should decode"))
            .collect()
    }

    fn sqlite_table_names(store: &SqliteStore) -> Vec<String> {
        let mut statement = store
            .connection
            .prepare(
                "SELECT name FROM sqlite_master
                 WHERE type = 'table'
                   AND name NOT LIKE 'sqlite_%'
                 ORDER BY name",
            )
            .expect("table query should prepare");
        let rows = statement
            .query_map([], |row| row.get::<_, String>(0))
            .expect("table query should run");
        rows.map(|row| row.expect("table row should decode"))
            .collect()
    }

    struct TestDb {
        dir: PathBuf,
    }

    impl TestDb {
        fn new(name: &str) -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock should be after epoch")
                .as_nanos();
            let dir = std::env::temp_dir().join(format!(
                "symdex-store-test-{name}-{}-{nonce}",
                std::process::id()
            ));
            fs::create_dir_all(&dir).expect("test db dir should exist");
            Self { dir }
        }

        fn config(&self) -> StoreConfig {
            StoreConfig {
                sqlite_path: self.dir.join("symdex.sqlite"),
            }
        }
    }

    impl Drop for TestDb {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(Path::new(&self.dir));
        }
    }
}

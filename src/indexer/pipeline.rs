use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use tracing::{info, warn};

use crate::{
    config::{AppConfig, discover_repo_root},
    db,
    indexer::{discovery::discover_files, hashing::hash_file},
    parser::extract::extract_file,
    symbols::model::ExtractedFile,
};

#[derive(Debug, Clone, Copy)]
pub struct IndexOptions {
    pub full: bool,
    pub watch: bool,
}

#[derive(Debug, Clone, Default)]
pub struct IndexSummary {
    pub repo_root: PathBuf,
    pub database_path: PathBuf,
    pub files_scanned: u64,
    pub files_parsed: u64,
    pub files_skipped: u64,
    pub files_deleted: u64,
    pub symbols_indexed: u64,
    pub references_indexed: u64,
    pub imports_indexed: u64,
    pub relationships_indexed: u64,
    pub parse_errors: u64,
    pub watch_requested: bool,
}

pub fn index_repository(path: &Path, options: IndexOptions) -> Result<IndexSummary> {
    let repo_root = discover_repo_root(path).context("invalid repository root")?;
    let config = AppConfig::load(&repo_root)?;
    let mut conn = db::open_database(&repo_root, &config)?;
    let repository_id = db::upsert_repository(&conn, &repo_root, &config.project.name)?;
    let parse_run_id = db::start_parse_run(&conn, repository_id, options.full)?;
    if options.full {
        db::clear_repository_index(&conn, repository_id)?;
    }

    let database_path = config.database_path(&repo_root);
    let discovered = discover_files(&repo_root, &config)?;
    let mut summary = IndexSummary {
        repo_root,
        database_path,
        files_scanned: discovered.len() as u64,
        watch_requested: options.watch,
        ..IndexSummary::default()
    };
    let mut seen_paths = Vec::with_capacity(discovered.len());

    for file in discovered {
        seen_paths.push(file.relative_path.clone());
        let (hash, bytes) = hash_file(&file.absolute_path)?;
        if !options.full
            && db::existing_file_hash(&conn, repository_id, &file.relative_path)?.as_deref()
                == Some(hash.as_str())
        {
            summary.files_skipped += 1;
            continue;
        }

        let content = String::from_utf8_lossy(&bytes).to_string();
        let extracted = match extract_file(&file.relative_path, &content, file.language) {
            Ok(extracted) => extracted,
            Err(error) => {
                warn!(path = %file.relative_path, error = %error, "failed to parse file");
                let mut extracted = ExtractedFile::default();
                extracted
                    .parse_errors
                    .push(crate::symbols::model::ParseErrorRecord {
                        file_id: None,
                        parse_run_id: None,
                        start_line: 1,
                        start_column: 0,
                        end_line: None,
                        end_column: None,
                        message: error.to_string(),
                    });
                extracted
            }
        };

        let tx = conn.transaction()?;
        db::replace_file_index(
            &tx,
            db::FileIndexReplacement {
                repository_id,
                parse_run_id,
                relative_path: &file.relative_path,
                absolute_path: &file.absolute_path,
                language: file.language.id(),
                content_hash: &hash,
                size_bytes: file.size_bytes,
                symbols: &extracted.symbols,
                references: &extracted.references,
                imports: &extracted.imports,
                relationships: &extracted.relationships,
                parse_errors: &extracted.parse_errors,
            },
        )?;
        tx.commit()?;

        summary.files_parsed += 1;
        summary.symbols_indexed += extracted.symbols.len() as u64;
        summary.references_indexed += extracted.references.len() as u64;
        summary.imports_indexed += extracted.imports.len() as u64;
        summary.relationships_indexed += extracted.relationships.len() as u64;
        summary.parse_errors += extracted.parse_errors.len() as u64;
        info!(path = %file.relative_path, "indexed file");
    }

    summary.files_deleted =
        db::mark_deleted_missing_files(&conn, repository_id, &seen_paths)? as u64;
    db::finish_parse_run(
        &conn,
        parse_run_id,
        summary.files_scanned,
        summary.files_parsed,
        summary.files_skipped,
        summary.parse_errors,
    )?;
    Ok(summary)
}

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::{
    cli::{Cli, Command, FilesCommand, SymbolsCommand},
    config::{self, AppConfig, discover_repo_root},
    db::{self, RelationshipDirection},
    indexer::pipeline::{IndexOptions, index_repository},
};

pub fn run(cli: Cli) -> Result<()> {
    match cli.command {
        Command::Init(args) => {
            let root = std::env::current_dir()?;
            config::init_config(&root, args.force)?;
            println!(
                "Initialized Symdex config at {}",
                root.join("symdex.toml").display()
            );
            println!("Runtime data directory: {}", root.join(".symdex").display());
        }
        Command::Index(args) => {
            let summary = index_repository(
                &args.path,
                IndexOptions {
                    full: args.full,
                    watch: args.watch,
                },
            )?;
            println!("Repo path: {}", summary.repo_root.display());
            println!("Database path: {}", summary.database_path.display());
            println!("Files scanned: {}", summary.files_scanned);
            println!("Files parsed: {}", summary.files_parsed);
            println!("Files skipped: {}", summary.files_skipped);
            println!("Files deleted: {}", summary.files_deleted);
            println!("Symbols indexed: {}", summary.symbols_indexed);
            println!("References indexed: {}", summary.references_indexed);
            println!("Imports indexed: {}", summary.imports_indexed);
            println!("Relationships indexed: {}", summary.relationships_indexed);
            println!("Parse errors: {}", summary.parse_errors);
            if summary.watch_requested {
                println!(
                    "Watch mode requested: continuous watching is planned after the MVP; completed one indexing pass."
                );
            }
        }
        Command::Status(args) => {
            let (root, config, conn) = open_context(&args.path)?;
            let status = db::status(&conn, &root, &config.database_path(&root), &config)?;
            println!("Repo path: {}", status.repo_path);
            println!("Database path: {}", status.database_path);
            println!(
                "Last index time: {}",
                status.last_index_time.unwrap_or_else(|| "never".into())
            );
            println!("Files indexed: {}", status.files_indexed);
            println!("Symbols indexed: {}", status.symbols_indexed);
            println!("Relationships indexed: {}", status.relationships_indexed);
            println!("Parse errors: {}", status.parse_errors);
            println!(
                "Supported languages: {}",
                status.supported_languages.join(", ")
            );
        }
        Command::Symbols(args) => {
            let (root, _config, conn, repository_id) = query_context(Path::new("."))?;
            match args.command {
                SymbolsCommand::Find(name) => {
                    let rows = db::find_symbols(&conn, repository_id, &name.symbol_name)?;
                    print_symbols(&rows);
                }
                SymbolsCommand::In(file) => {
                    let path = normalize_cli_path(&root, &file.file);
                    let rows = db::symbols_in_file(&conn, repository_id, &path)?;
                    print_symbols(&rows);
                }
            }
        }
        Command::Refs(args) => {
            let (_root, _config, conn, repository_id) = query_context(Path::new("."))?;
            for row in db::references(&conn, repository_id, &args.symbol_name)? {
                println!(
                    "{}:{}:{} {} -> {}",
                    row.file_path,
                    row.start_line,
                    row.start_column,
                    row.reference_kind,
                    row.referenced_name
                );
            }
        }
        Command::Callers(args) => {
            let (_root, _config, conn, repository_id) = query_context(Path::new("."))?;
            for row in db::relationships(
                &conn,
                repository_id,
                &args.symbol_name,
                RelationshipDirection::Callers,
            )? {
                println!(
                    "{} [{}] {} ({})",
                    row.source_name.unwrap_or_else(|| "<file>".into()),
                    row.confidence,
                    row.source_file,
                    row.evidence.unwrap_or_default()
                );
            }
        }
        Command::Callees(args) => {
            let (_root, _config, conn, repository_id) = query_context(Path::new("."))?;
            for row in db::relationships(
                &conn,
                repository_id,
                &args.symbol_name,
                RelationshipDirection::Callees,
            )? {
                println!(
                    "{} -> {} [{}]",
                    row.source_name.unwrap_or_else(|| "<unknown>".into()),
                    row.target_name
                        .unwrap_or_else(|| row.evidence.unwrap_or_default()),
                    row.confidence
                );
            }
        }
        Command::Imports(args) => {
            let (root, _config, conn, repository_id) = query_context(Path::new("."))?;
            let path = normalize_cli_path(&root, &args.file);
            for row in db::imports(&conn, repository_id, &path)? {
                println!(
                    "{}:{}:{} {}",
                    row.file_path, row.start_line, row.start_column, row.import_text
                );
            }
        }
        Command::Errors(args) => {
            let (root, _config, conn, repository_id) = query_context(Path::new("."))?;
            let file = args
                .file
                .as_ref()
                .map(|path| normalize_cli_path(&root, path));
            for row in db::parse_errors(&conn, repository_id, file.as_deref())? {
                println!(
                    "{}:{}:{} {}",
                    row.file_path, row.start_line, row.start_column, row.message
                );
            }
        }
        Command::Files(args) => match args.command {
            FilesCommand::WithErrors => {
                let (_root, _config, conn, repository_id) = query_context(Path::new("."))?;
                for file in db::files_with_errors(&conn, repository_id)? {
                    println!("{file}");
                }
            }
        },
        Command::Tui(args) => crate::tui::run(args.path)?,
    }
    Ok(())
}

fn open_context(path: &Path) -> Result<(PathBuf, AppConfig, rusqlite::Connection)> {
    let root = discover_repo_root(path)?;
    let config = AppConfig::load(&root)?;
    let conn = db::open_database(&root, &config)?;
    Ok((root, config, conn))
}

fn query_context(path: &Path) -> Result<(PathBuf, AppConfig, rusqlite::Connection, i64)> {
    let (root, config, conn) = open_context(path)?;
    let repository_id = db::repository_id(&conn, &root)?
        .context("repository has not been indexed yet; run `symdex index .`")?;
    Ok((root, config, conn, repository_id))
}

fn normalize_cli_path(root: &Path, path: &Path) -> String {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    };
    absolute
        .strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn print_symbols(rows: &[db::schema::SymbolRow]) {
    if rows.is_empty() {
        println!("No symbols found");
        return;
    }
    for row in rows {
        println!(
            "{} {} {} {}:{}-{} signature={} visibility={} matched_by={}",
            row.name,
            row.kind,
            row.language,
            row.file_path,
            row.start_line,
            row.end_line,
            row.signature.as_deref().unwrap_or(""),
            row.visibility.as_deref().unwrap_or(""),
            row.matched_by,
        );
    }
}

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use symdex_core::{
    CallEdge, CodeChunk, DiscoveryOptions, FileFacts, RepoRoot, Symbol, discover_rust_files,
    index_rust_file,
};
use symdex_embed::{EmbedConfig, OllamaClient};
use symdex_store::{
    CallRecord, ChunkRecord, FileRecord, IndexRunRecord, PointPayload, QdrantClient,
    RepositoryRecord, SqliteStore, StoreConfig, SymbolRecord, VectorPoint, qdrant_collection_name,
    qdrant_point_id, sqlite_parent,
};

fn main() {
    if let Err(error) = run(env::args().skip(1).collect()) {
        eprintln!("error: {error}");
        std::process::exit(1);
    }
}

fn run(args: Vec<String>) -> Result<(), String> {
    let Some(command) = args.first().map(String::as_str) else {
        print_help();
        return Ok(());
    };

    match command {
        "doctor" => doctor(),
        "init" => init(),
        "index" => {
            let index_args = parse_index_args(&args[1..]);
            index(&index_args)
        }
        "index-status" => {
            let repo = args.get(1).map(String::as_str).unwrap_or(".");
            index_status(repo)
        }
        "symbol" => {
            let repo = args.get(1).map(String::as_str).unwrap_or(".");
            let query = args.get(2).map(String::as_str).unwrap_or("");
            symbol(repo, query)
        }
        "callers" => {
            let repo = args.get(1).map(String::as_str).unwrap_or(".");
            let query = args.get(2).map(String::as_str).unwrap_or("");
            callers(repo, query)
        }
        "callees" => {
            let repo = args.get(1).map(String::as_str).unwrap_or(".");
            let query = args.get(2).map(String::as_str).unwrap_or("");
            callees(repo, query)
        }
        "impact" => {
            let repo = args.get(1).map(String::as_str).unwrap_or(".");
            let query = args.get(2).map(String::as_str).unwrap_or("");
            impact(repo, query)
        }
        "search" => {
            let repo = args.get(1).map(String::as_str).unwrap_or(".");
            let query_parts = if args.len() > 2 { &args[2..] } else { &[] };
            search(repo, query_parts)
        }
        "serve-mcp" => serve_mcp(),
        "help" | "--help" | "-h" => {
            print_help();
            Ok(())
        }
        "--version" | "-V" => {
            println!("symdex {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        unknown => Err(format!("unknown command `{unknown}`\n\nrun `symdex help`")),
    }
}

fn doctor() -> Result<(), String> {
    let store = StoreConfig::from_env();
    let embed = EmbedConfig::from_env();
    let cwd = env::current_dir().map_err(|error| format!("read current directory: {error}"))?;

    println!("symdex doctor");
    println!("workspace: {}", cwd.display());
    println!("sqlite: {}", store.sqlite_path.display());
    println!("qdrant: {}", store.qdrant_url);
    println!("ollama: {}", embed.ollama_url);
    println!("embed_model: {}", embed.model);

    match sqlite_parent(&store) {
        Some(parent) => report_writable_dir("sqlite_parent", &parent),
        None => println!("sqlite_parent: skipped (database path has no parent)"),
    }

    report_ollama(&embed);
    report_qdrant(&store);
    Ok(())
}

fn init() -> Result<(), String> {
    let store = StoreConfig::from_env();
    if let Some(parent) = sqlite_parent(&store) {
        fs::create_dir_all(&parent)
            .map_err(|error| format!("create sqlite directory {}: {error}", parent.display()))?;
        println!("created {}", parent.display());
    }
    let sqlite = SqliteStore::open(&store).map_err(|error| error.to_string())?;
    sqlite.migrate().map_err(|error| error.to_string())?;
    println!("initialized symdex local state");
    Ok(())
}

fn index(args: &IndexArgs) -> Result<(), String> {
    let root = RepoRoot::open(&args.repo).map_err(|error| error.to_string())?;
    let store_config = StoreConfig::from_env();
    let mut sqlite = SqliteStore::open(&store_config).map_err(|error| error.to_string())?;
    sqlite.migrate().map_err(|error| error.to_string())?;
    sqlite
        .upsert_repository(&RepositoryRecord {
            id: root.id().to_owned(),
            root_path: root.path().display().to_string(),
        })
        .map_err(|error| error.to_string())?;

    let collection = collect_index_reports(&root, if args.offline { Some(&sqlite) } else { None })?;

    println!("repository_id: {}", root.id());
    println!("repository_root: {}", root.path().display());
    println!("rust_files_seen: {}", collection.files_seen);
    println!(
        "files_skipped_unchanged: {}",
        collection.files_skipped_unchanged
    );
    print_index_reports(&collection.reports);

    persist_structural_index(&mut sqlite, &root, &collection)?;

    if args.offline {
        println!("embedding: skipped (--offline)");
        println!("qdrant: skipped (--offline)");
        return Ok(());
    }

    let embed_config = EmbedConfig::from_env();
    let chunk_texts = chunk_texts(&collection.reports);
    if chunk_texts.is_empty() {
        println!("chunks_embedded: 0");
        println!("qdrant: skipped (no chunks)");
        return Ok(());
    }

    let embed_client =
        OllamaClient::new(embed_config.clone()).map_err(|error| error.to_string())?;
    if !embed_client
        .model_available()
        .map_err(|error| error.to_string())?
    {
        return Err(format!(
            "embedding model `{}` is not available",
            embed_config.model
        ));
    }

    let embeddings = embed_client
        .embed_batch(
            &chunk_texts
                .iter()
                .map(|chunk| chunk.text.clone())
                .collect::<Vec<_>>(),
        )
        .map_err(|error| error.to_string())?;
    let dimension = embeddings.dimension().unwrap_or(0);
    sqlite
        .ensure_embedding_compatible(root.id(), &embed_config.model, dimension)
        .map_err(|error| error.to_string())?;

    let qdrant = QdrantClient::new(&store_config).map_err(|error| error.to_string())?;
    let qdrant_collection = qdrant_collection_name(root.id(), &embed_config.model);
    qdrant
        .ensure_collection(&qdrant_collection, dimension)
        .map_err(|error| error.to_string())?;

    let points = chunk_texts
        .iter()
        .zip(embeddings.embeddings)
        .map(|(chunk, vector)| vector_point(root.id(), chunk, vector))
        .collect::<Result<Vec<_>, _>>()?;
    qdrant
        .upsert_points(&qdrant_collection, &points)
        .map_err(|error| error.to_string())?;
    sqlite
        .record_index_run(&IndexRunRecord {
            repository_id: root.id().to_owned(),
            status: "success".to_owned(),
            embedding_model: embed_config.model.clone(),
            embedding_dimension: Some(dimension),
            files_seen: collection.files_seen,
            files_indexed: collection.reports.len(),
            chunks_embedded: points.len(),
            error_summary: None,
        })
        .map_err(|error| error.to_string())?;

    println!("embedding_model: {}", embed_config.model);
    println!("embedding_dimension: {dimension}");
    println!("qdrant_collection: {qdrant_collection}");
    println!("chunks_embedded: {}", points.len());
    Ok(())
}

fn index_status(repo: &str) -> Result<(), String> {
    let root = RepoRoot::open(repo).map_err(|error| error.to_string())?;
    let store_config = StoreConfig::from_env();
    let sqlite = SqliteStore::open(&store_config).map_err(|error| error.to_string())?;
    sqlite.migrate().map_err(|error| error.to_string())?;
    let status = sqlite
        .repository_status(root.id())
        .map_err(|error| error.to_string())?;

    println!("repository_id: {}", status.repository_id);
    println!("files_indexed: {}", status.files_indexed);
    println!("chunks_indexed: {}", status.chunks_indexed);
    println!("symbols_indexed: {}", status.symbols_indexed);
    println!("calls_indexed: {}", status.calls_indexed);
    println!(
        "embedding_model: {}",
        status.embedding_model.as_deref().unwrap_or("<none>")
    );
    println!(
        "embedding_dimension: {}",
        status
            .embedding_dimension
            .map(|dimension| dimension.to_string())
            .unwrap_or_else(|| "<none>".to_owned())
    );
    println!(
        "last_indexed_at: {}",
        status.last_indexed_at.as_deref().unwrap_or("<never>")
    );
    Ok(())
}

fn symbol(repo: &str, query: &str) -> Result<(), String> {
    if query.is_empty() {
        return Err("symbol requires a symbol query".to_owned());
    }
    let root = RepoRoot::open(repo).map_err(|error| error.to_string())?;
    let sqlite = sqlite_for_read()?;
    let symbols = sqlite
        .find_symbols(root.id(), query)
        .map_err(|error| error.to_string())?;
    println!("symbols: {}", symbols.len());
    for symbol in symbols {
        println!(
            "{} {} {}:{}-{}",
            symbol.kind, symbol.qualified_name, symbol.path, symbol.start_line, symbol.end_line
        );
    }
    Ok(())
}

fn callers(repo: &str, query: &str) -> Result<(), String> {
    if query.is_empty() {
        return Err("callers requires a symbol query".to_owned());
    }
    let root = RepoRoot::open(repo).map_err(|error| error.to_string())?;
    let sqlite = sqlite_for_read()?;
    let rows = sqlite
        .callers(root.id(), query)
        .map_err(|error| error.to_string())?;
    println!("callers: {}", rows.len());
    for row in rows {
        print_call_row(&row);
    }
    Ok(())
}

fn callees(repo: &str, query: &str) -> Result<(), String> {
    if query.is_empty() {
        return Err("callees requires a symbol query".to_owned());
    }
    let root = RepoRoot::open(repo).map_err(|error| error.to_string())?;
    let sqlite = sqlite_for_read()?;
    let rows = sqlite
        .callees(root.id(), query)
        .map_err(|error| error.to_string())?;
    println!("callees: {}", rows.len());
    for row in rows {
        print_call_row(&row);
    }
    Ok(())
}

fn impact(repo: &str, query: &str) -> Result<(), String> {
    if query.is_empty() {
        return Err("impact requires a symbol query".to_owned());
    }
    println!("direct_callers");
    callers(repo, query)?;
    println!("direct_callees");
    callees(repo, query)?;
    Ok(())
}

fn sqlite_for_read() -> Result<SqliteStore, String> {
    let store_config = StoreConfig::from_env();
    let sqlite = SqliteStore::open(&store_config).map_err(|error| error.to_string())?;
    sqlite.migrate().map_err(|error| error.to_string())?;
    Ok(sqlite)
}

fn print_call_row(row: &symdex_store::CallSearchRow) {
    println!(
        "{} {:.2} {}:{}-{} callee={} status={}",
        row.symbol_qualified_name
            .as_deref()
            .unwrap_or("<unresolved>"),
        row.confidence,
        row.path.as_deref().unwrap_or("<unknown>"),
        row.start_line.unwrap_or(0),
        row.end_line.unwrap_or(0),
        row.callee_text,
        row.resolution_status
    );
}

fn search(repo: &str, query_parts: &[String]) -> Result<(), String> {
    let query = query_parts.join(" ");
    if query.trim().is_empty() {
        return Err("search requires a query".to_owned());
    }

    let root = RepoRoot::open(repo).map_err(|error| error.to_string())?;
    let embed_config = EmbedConfig::from_env();
    let embed_client =
        OllamaClient::new(embed_config.clone()).map_err(|error| error.to_string())?;
    let query_embedding = embed_client
        .embed_batch(&[query])
        .map_err(|error| error.to_string())?;
    let Some(vector) = query_embedding.embeddings.into_iter().next() else {
        return Err("embedding query returned no vector".to_owned());
    };

    let store_config = StoreConfig::from_env();
    let qdrant = QdrantClient::new(&store_config).map_err(|error| error.to_string())?;
    let collection = qdrant_collection_name(root.id(), &embed_config.model);
    let results = qdrant
        .query_points(&collection, vector, 10)
        .map_err(|error| error.to_string())?;

    println!("repository_id: {}", root.id());
    println!("qdrant_collection: {collection}");
    println!("results: {}", results.len());
    for result in results {
        let payload = result.payload;
        println!(
            "{:.4} {}:{}-{} {}",
            result.score,
            payload.path,
            payload.start_line,
            payload.end_line,
            payload.symbol_name.as_deref().unwrap_or("<none>")
        );
    }
    Ok(())
}

fn collect_index_reports(
    root: &RepoRoot,
    sqlite: Option<&SqliteStore>,
) -> Result<IndexCollection, String> {
    let files = discover_rust_files(root, &DiscoveryOptions::default())
        .map_err(|error| error.to_string())?;

    let mut reports = Vec::new();
    let mut files_skipped_unchanged = 0usize;
    for file in &files {
        if let Some(sqlite) = sqlite
            && sqlite
                .file_unchanged(
                    root.id(),
                    &file.facts.relative_path,
                    &file.facts.content_hash,
                )
                .map_err(|error| error.to_string())?
        {
            files_skipped_unchanged += 1;
            continue;
        }

        let source = fs::read_to_string(&file.absolute_path)
            .map_err(|error| format!("read {}: {error}", file.absolute_path.display()))?;
        let index = index_rust_file(&file.facts, &source).map_err(|error| error.to_string())?;
        reports.push(IndexReport {
            file: file.facts.clone(),
            chunks: index.chunks,
            symbols: index.symbols,
            calls: index.calls,
            source,
        });
    }
    Ok(IndexCollection {
        files_seen: files.len(),
        files_skipped_unchanged,
        active_paths: files
            .iter()
            .map(|file| file.facts.relative_path.clone())
            .collect(),
        reports,
    })
}

fn print_index_reports(reports: &[IndexReport]) {
    let chunks_seen: usize = reports.iter().map(|report| report.chunks.len()).sum();
    let chunks_excluded_from_embedding: usize = reports
        .iter()
        .flat_map(|report| report.chunks.iter())
        .filter(|chunk| chunk.excluded_reason.is_some())
        .count();
    for report in reports.iter().take(20) {
        println!(
            "{} {} {} chunks={}",
            report.file.relative_path,
            report.file.language.as_str(),
            report.file.content_hash,
            report.chunks.len()
        );
        for chunk in report.chunks.iter().take(5) {
            println!(
                "  chunk {} lines={}-{} symbol={} excluded={}",
                chunk.kind.as_str(),
                chunk.line_range.start,
                chunk.line_range.end,
                chunk.symbol_name.as_deref().unwrap_or("<none>"),
                chunk.excluded_reason.as_deref().unwrap_or("<none>")
            );
        }
    }
    if reports.len() > 20 {
        println!("... {} more files", reports.len() - 20);
    }
    println!("chunks_seen: {chunks_seen}");
    println!("chunks_excluded_from_embedding: {chunks_excluded_from_embedding}");
}

fn chunk_texts(reports: &[IndexReport]) -> Vec<ChunkText<'_>> {
    reports
        .iter()
        .flat_map(|report| {
            report
                .chunks
                .iter()
                .filter(|chunk| chunk.excluded_reason.is_none())
                .map(|chunk| ChunkText {
                    file: &report.file,
                    chunk,
                    text: report.source[chunk.byte_range.start..chunk.byte_range.end].to_owned(),
                })
        })
        .collect()
}

fn vector_point(
    repository_id: &str,
    chunk: &ChunkText<'_>,
    vector: Vec<f32>,
) -> Result<VectorPoint, String> {
    Ok(VectorPoint {
        id: qdrant_point_id(&chunk.chunk.id).map_err(|error| error.to_string())?,
        vector,
        payload: PointPayload {
            repository_id: repository_id.to_owned(),
            file_id: chunk.file.id.clone(),
            chunk_id: chunk.chunk.id.clone(),
            symbol_id: chunk.chunk.symbol_id.clone(),
            symbol_name: chunk.chunk.symbol_name.clone(),
            path: chunk.chunk.relative_path.clone(),
            language: chunk.file.language.as_str().to_owned(),
            chunk_kind: chunk.chunk.kind.as_str().to_owned(),
            start_line: chunk.chunk.line_range.start,
            end_line: chunk.chunk.line_range.end,
            text_hash: chunk.chunk.text_hash.clone(),
        },
    })
}

fn persist_structural_index(
    sqlite: &mut SqliteStore,
    root: &RepoRoot,
    collection: &IndexCollection,
) -> Result<(), String> {
    let mut chunks_persisted = 0usize;
    let mut symbols_persisted = 0usize;
    let mut calls_persisted = 0usize;
    for report in &collection.reports {
        let file = FileRecord {
            id: report.file.id.clone(),
            repository_id: root.id().to_owned(),
            path: report.file.relative_path.clone(),
            language: report.file.language.as_str().to_owned(),
            content_hash: report.file.content_hash.clone(),
        };
        let chunks = report
            .chunks
            .iter()
            .map(chunk_record)
            .collect::<Result<Vec<_>, _>>()?;
        let symbols = report.symbols.iter().map(symbol_record).collect::<Vec<_>>();
        let calls = report.calls.iter().map(call_record).collect::<Vec<_>>();
        chunks_persisted += chunks.len();
        symbols_persisted += symbols.len();
        calls_persisted += calls.len();
        sqlite
            .replace_file_facts(&file, &symbols, &chunks, &calls)
            .map_err(|error| error.to_string())?;
    }

    let files_removed = sqlite
        .remove_missing_files(root.id(), &collection.active_paths)
        .map_err(|error| error.to_string())?;
    println!("sqlite_files_indexed: {}", collection.reports.len());
    println!("sqlite_chunks_indexed: {chunks_persisted}");
    println!("sqlite_symbols_indexed: {symbols_persisted}");
    println!("sqlite_calls_indexed: {calls_persisted}");
    println!("sqlite_files_removed: {files_removed}");
    Ok(())
}

fn chunk_record(chunk: &CodeChunk) -> Result<ChunkRecord, String> {
    Ok(ChunkRecord {
        id: chunk.id.clone(),
        file_id: chunk.file_id.clone(),
        symbol_id: chunk.symbol_id.clone(),
        kind: chunk.kind.as_str().to_owned(),
        text_hash: chunk.text_hash.clone(),
        start_line: chunk.line_range.start,
        end_line: chunk.line_range.end,
        start_byte: chunk.byte_range.start,
        end_byte: chunk.byte_range.end,
        qdrant_point_id: if chunk.excluded_reason.is_none() {
            Some(qdrant_point_id(&chunk.id).map_err(|error| error.to_string())?)
        } else {
            None
        },
        excluded_reason: chunk.excluded_reason.clone(),
    })
}

fn symbol_record(symbol: &Symbol) -> SymbolRecord {
    SymbolRecord {
        id: symbol.id.clone(),
        file_id: symbol.file_id.clone(),
        parent_symbol_id: symbol.parent_symbol_id.clone(),
        name: symbol.name.clone(),
        qualified_name: symbol.qualified_name.clone(),
        kind: symbol.kind.as_str().to_owned(),
        signature: symbol.signature.clone(),
        start_line: symbol.line_range.start,
        end_line: symbol.line_range.end,
        start_byte: symbol.byte_range.start,
        end_byte: symbol.byte_range.end,
    }
}

fn call_record(call: &CallEdge) -> CallRecord {
    CallRecord {
        id: call.id.clone(),
        caller_symbol_id: call.caller_symbol_id.clone(),
        callee_text: call.callee_text.clone(),
        callee_symbol_id: call.callee_symbol_id.clone(),
        call_line: call.call_line,
        confidence: call.confidence,
        resolution_status: call.resolution_status.as_str().to_owned(),
    }
}

fn parse_index_args(args: &[String]) -> IndexArgs {
    let mut repo = ".".to_owned();
    let mut offline = false;
    for arg in args {
        if arg == "--offline" {
            offline = true;
        } else {
            repo = arg.clone();
        }
    }
    IndexArgs { repo, offline }
}

struct IndexCollection {
    files_seen: usize,
    files_skipped_unchanged: usize,
    active_paths: Vec<String>,
    reports: Vec<IndexReport>,
}

struct IndexReport {
    file: FileFacts,
    chunks: Vec<CodeChunk>,
    symbols: Vec<Symbol>,
    calls: Vec<CallEdge>,
    source: String,
}

struct ChunkText<'a> {
    file: &'a FileFacts,
    chunk: &'a CodeChunk,
    text: String,
}

struct IndexArgs {
    repo: String,
    offline: bool,
}

fn serve_mcp() -> Result<(), String> {
    symdex_mcp::serve_stdio()
}

fn report_writable_dir(label: &str, path: &Path) {
    if path.exists() {
        if path.is_dir() {
            println!("{label}: ok ({})", path.display());
        } else {
            println!("{label}: not a directory ({})", path.display());
        }
        return;
    }

    let display_path: PathBuf = path.to_path_buf();
    println!(
        "{label}: missing ({}) - run `symdex init` to create it",
        display_path.display()
    );
}

fn report_ollama(config: &EmbedConfig) {
    let client = match OllamaClient::new(config.clone()) {
        Ok(client) => client,
        Err(error) => {
            println!("ollama_status: error ({error})");
            return;
        }
    };

    match client.model_available() {
        Ok(true) => println!("ollama_model: ok ({})", config.model),
        Ok(false) => {
            println!("ollama_model: missing ({})", config.model);
            return;
        }
        Err(error) => {
            println!("ollama_status: unreachable ({error})");
            return;
        }
    }

    match client.probe_dimension() {
        Ok(dimension) => println!("embedding_dimension: {dimension}"),
        Err(error) => println!("embedding_dimension: unavailable ({error})"),
    }
}

fn report_qdrant(config: &StoreConfig) {
    let client = match QdrantClient::with_timeout(config, std::time::Duration::from_secs(3)) {
        Ok(client) => client,
        Err(error) => {
            println!("qdrant_status: error ({error})");
            return;
        }
    };

    match client.health_check() {
        Ok(()) => println!("qdrant_status: ok"),
        Err(error) => println!("qdrant_status: unreachable ({error})"),
    }
}

fn print_help() {
    println!(
        "symdex {}\n\nUSAGE:\n    symdex <command>\n\nCOMMANDS:\n    init                   Create local symdex state directories\n    doctor                 Print local configuration and diagnostics\n    index [--offline] <repo>  Index Rust chunks and upsert semantic vectors\n    index-status <repo>    Show local SQLite index counts\n    symbol <repo> <query>  Find symbols in the local index\n    callers <repo> <symbol>  Show direct callers\n    callees <repo> <symbol>  Show direct callees\n    impact <repo> <symbol>  Show direct callers and callees\n    search <repo> <query>  Search indexed chunks by semantic similarity\n    serve-mcp              Run the read-only MCP server over stdio\n    help                   Print this help",
        env!("CARGO_PKG_VERSION")
    );
}

#[cfg(test)]
mod tests {
    use symdex_core::{
        ByteRange, ChunkKind, CodeChunk, FileFacts, Language, LineRange, content_hash, stable_id,
    };

    use crate::{IndexReport, chunk_record, chunk_texts};

    #[test]
    fn chunk_texts_skip_secret_excluded_chunks() {
        let file = sample_file();
        let source = "pub fn public() {}\npub fn secret() {}\n".to_owned();
        let public = sample_chunk("public", 0, 18, None);
        let secret = sample_chunk("secret", 18, source.len(), Some("likely_access_token"));
        let report = IndexReport {
            file: file.clone(),
            chunks: vec![public.clone(), secret.clone()],
            symbols: Vec::new(),
            calls: Vec::new(),
            source,
        };

        let reports = [report];
        let chunks = chunk_texts(&reports);

        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].chunk.id, public.id);
        let public_record = chunk_record(&public).expect("public record");
        let secret_record = chunk_record(&secret).expect("secret record");
        assert!(public_record.qdrant_point_id.is_some());
        assert!(secret_record.qdrant_point_id.is_none());
        assert_eq!(
            secret_record.excluded_reason.as_deref(),
            Some("likely_access_token")
        );
    }

    fn sample_file() -> FileFacts {
        FileFacts {
            id: "file-1".to_owned(),
            relative_path: "src/lib.rs".to_owned(),
            language: Language::Rust,
            content_hash: "hash".to_owned(),
        }
    }

    fn sample_chunk(
        name: &str,
        start_byte: usize,
        end_byte: usize,
        excluded_reason: Option<&str>,
    ) -> CodeChunk {
        CodeChunk {
            id: stable_id(&["chunk", name]),
            file_id: "file-1".to_owned(),
            relative_path: "src/lib.rs".to_owned(),
            symbol_id: None,
            symbol_name: Some(name.to_owned()),
            kind: ChunkKind::Function,
            byte_range: ByteRange::new(start_byte, end_byte),
            line_range: LineRange::new(1, 1),
            text_hash: content_hash(name.as_bytes()),
            excluded_reason: excluded_reason.map(str::to_owned),
        }
    }
}

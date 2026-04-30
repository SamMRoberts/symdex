use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use symdex_core::RepoRoot;
use symdex_embed::{EmbedConfig, OllamaClient};
use symdex_index::{EmbeddingSummary, IndexOptions, IndexSummary, run_index};
use symdex_store::{QdrantClient, SqliteStore, StoreConfig, qdrant_collection_name, sqlite_parent};

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
        "context-pack" => {
            let repo = args.get(1).map(String::as_str).unwrap_or(".");
            let query = args.get(2).map(String::as_str).unwrap_or("");
            context_pack(repo, query)
        }
        "search" => {
            let repo = args.get(1).map(String::as_str).unwrap_or(".");
            let query_parts = if args.len() > 2 { &args[2..] } else { &[] };
            search(repo, query_parts)
        }
        "tui" => {
            let repo = args.get(1).map(String::as_str).unwrap_or(".");
            tui(repo)
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
    let summary = run_index(&IndexOptions {
        repo: args.repo.clone(),
        offline: args.offline,
    })?;
    print_index_summary(&summary);
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

fn context_pack(repo: &str, query: &str) -> Result<(), String> {
    if query.is_empty() {
        return Err("context-pack requires a symbol query".to_owned());
    }
    let root = RepoRoot::open(repo).map_err(|error| error.to_string())?;
    let sqlite = sqlite_for_read()?;
    let pack = sqlite
        .context_pack(root.id(), query, 8)
        .map_err(|error| error.to_string())?;
    let json = serde_json::to_string_pretty(&pack).map_err(|error| error.to_string())?;
    println!("{json}");
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

fn print_index_summary(summary: &IndexSummary) {
    println!("repository_id: {}", summary.repository_id);
    println!("repository_root: {}", summary.repository_root);
    println!("rust_files_seen: {}", summary.files_seen);
    println!(
        "files_skipped_unchanged: {}",
        summary.files_skipped_unchanged
    );
    for file in summary.files.iter().take(20) {
        println!(
            "{} {} {} chunks={}",
            file.path,
            file.language,
            file.content_hash,
            file.chunks.len()
        );
        for chunk in file.chunks.iter().take(5) {
            println!(
                "  chunk {} lines={}-{} symbol={} excluded={}",
                chunk.kind,
                chunk.start_line,
                chunk.end_line,
                chunk.symbol.as_deref().unwrap_or("<none>"),
                chunk.excluded_reason.as_deref().unwrap_or("<none>")
            );
        }
    }
    if summary.files.len() > 20 {
        println!("... {} more files", summary.files.len() - 20);
    }
    println!("chunks_seen: {}", summary.chunks_seen);
    println!(
        "chunks_excluded_from_embedding: {}",
        summary.chunks_excluded_from_embedding
    );
    println!("sqlite_files_indexed: {}", summary.sqlite_files_indexed);
    println!("sqlite_chunks_indexed: {}", summary.sqlite_chunks_indexed);
    println!("sqlite_symbols_indexed: {}", summary.sqlite_symbols_indexed);
    println!("sqlite_calls_indexed: {}", summary.sqlite_calls_indexed);
    println!("sqlite_files_removed: {}", summary.sqlite_files_removed);
    match &summary.embedding {
        EmbeddingSummary::SkippedOffline => {
            println!("embedding: skipped (--offline)");
            println!("qdrant: skipped (--offline)");
        }
        EmbeddingSummary::SkippedNoChunks => {
            println!("chunks_embedded: 0");
            println!("qdrant: skipped (no chunks)");
        }
        EmbeddingSummary::Completed {
            model,
            dimension,
            qdrant_collection,
            chunks_embedded,
        } => {
            println!("embedding_model: {model}");
            println!("embedding_dimension: {dimension}");
            println!("qdrant_collection: {qdrant_collection}");
            println!("chunks_embedded: {chunks_embedded}");
        }
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

struct IndexArgs {
    repo: String,
    offline: bool,
}

fn serve_mcp() -> Result<(), String> {
    symdex_mcp::serve_stdio()
}

fn tui(repo: &str) -> Result<(), String> {
    if repo == "--help" || repo == "-h" {
        print!("{}", symdex_tui::help_text());
        return Ok(());
    }
    symdex_tui::run(symdex_tui::TuiOptions {
        repo: repo.to_owned(),
    })
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
        "symdex {}\n\nUSAGE:\n    symdex <command>\n\nCOMMANDS:\n    init                   Create local symdex state directories\n    doctor                 Print local configuration and diagnostics\n    index [--offline] <repo>  Index Rust chunks and upsert semantic vectors\n    index-status <repo>    Show local SQLite index counts\n    symbol <repo> <query>  Find symbols in the local index\n    callers <repo> <symbol>  Show direct callers\n    callees <repo> <symbol>  Show direct callees\n    impact <repo> <symbol>  Show direct callers and callees\n    context-pack <repo> <symbol>  Print compact JSON evidence for editing context\n    search <repo> <query>  Search indexed chunks by semantic similarity\n    tui [repo]             Run the local terminal UI control panel\n    serve-mcp              Run the read-only MCP server over stdio\n    help                   Print this help",
        env!("CARGO_PKG_VERSION")
    );
}

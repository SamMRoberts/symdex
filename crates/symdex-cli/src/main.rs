use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use symdex_core::{
    CodeChunk, DiscoveryOptions, FileFacts, RepoRoot, discover_rust_files, extract_rust_chunks,
};
use symdex_embed::{EmbedConfig, OllamaClient};
use symdex_mcp::tool_names;
use symdex_store::{
    PointPayload, QdrantClient, StoreConfig, VectorPoint, qdrant_collection_name, qdrant_point_id,
    sqlite_parent,
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
        "search" => {
            let repo = args.get(1).map(String::as_str).unwrap_or(".");
            let query_parts = if args.len() > 2 { &args[2..] } else { &[] };
            search(repo, query_parts)
        }
        "serve-mcp" => serve_mcp_preview(),
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
    println!("initialized symdex local state");
    Ok(())
}

fn index(args: &IndexArgs) -> Result<(), String> {
    let root = RepoRoot::open(&args.repo).map_err(|error| error.to_string())?;
    let reports = collect_index_reports(&root)?;

    println!("repository_id: {}", root.id());
    println!("repository_root: {}", root.path().display());
    println!("rust_files_seen: {}", reports.len());
    print_index_reports(&reports);

    if args.offline {
        println!("embedding: skipped (--offline)");
        println!("qdrant: skipped (--offline)");
        println!("sqlite_persistence: skipped (SQLite adapter pending)");
        return Ok(());
    }

    let embed_config = EmbedConfig::from_env();
    let chunk_texts = chunk_texts(&reports);
    if chunk_texts.is_empty() {
        println!("chunks_embedded: 0");
        println!("qdrant: skipped (no chunks)");
        println!("sqlite_persistence: skipped (SQLite adapter pending)");
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

    let store_config = StoreConfig::from_env();
    let qdrant = QdrantClient::new(&store_config).map_err(|error| error.to_string())?;
    let collection = qdrant_collection_name(root.id(), &embed_config.model);
    qdrant
        .ensure_collection(&collection, dimension)
        .map_err(|error| error.to_string())?;

    let points = chunk_texts
        .iter()
        .zip(embeddings.embeddings)
        .map(|(chunk, vector)| vector_point(root.id(), chunk, vector))
        .collect::<Result<Vec<_>, _>>()?;
    qdrant
        .upsert_points(&collection, &points)
        .map_err(|error| error.to_string())?;

    println!("embedding_model: {}", embeddings.model);
    println!("embedding_dimension: {dimension}");
    println!("qdrant_collection: {collection}");
    println!("chunks_embedded: {}", points.len());
    println!("sqlite_persistence: skipped (SQLite adapter pending)");
    Ok(())
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

fn collect_index_reports(root: &RepoRoot) -> Result<Vec<IndexReport>, String> {
    let files = discover_rust_files(root, &DiscoveryOptions::default())
        .map_err(|error| error.to_string())?;

    let mut reports = Vec::new();
    for file in &files {
        let source = fs::read_to_string(&file.absolute_path)
            .map_err(|error| format!("read {}: {error}", file.absolute_path.display()))?;
        let chunks =
            extract_rust_chunks(&file.facts, &source).map_err(|error| error.to_string())?;
        reports.push(IndexReport {
            file: file.facts.clone(),
            chunks,
            source,
        });
    }
    Ok(reports)
}

fn print_index_reports(reports: &[IndexReport]) {
    let chunks_seen: usize = reports.iter().map(|report| report.chunks.len()).sum();
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
                "  chunk {} lines={}-{} symbol={}",
                chunk.kind.as_str(),
                chunk.line_range.start,
                chunk.line_range.end,
                chunk.symbol_name.as_deref().unwrap_or("<none>")
            );
        }
    }
    if reports.len() > 20 {
        println!("... {} more files", reports.len() - 20);
    }
    println!("chunks_seen: {chunks_seen}");
}

fn chunk_texts(reports: &[IndexReport]) -> Vec<ChunkText<'_>> {
    reports
        .iter()
        .flat_map(|report| {
            report.chunks.iter().map(|chunk| ChunkText {
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

struct IndexReport {
    file: FileFacts,
    chunks: Vec<CodeChunk>,
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

fn serve_mcp_preview() -> Result<(), String> {
    println!("MCP server is not implemented yet. Planned read-only tools:");
    for tool in tool_names() {
        println!("- {tool}");
    }
    Ok(())
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
        "symdex {}\n\nUSAGE:\n    symdex <command>\n\nCOMMANDS:\n    init                   Create local symdex state directories\n    doctor                 Print local configuration and diagnostics\n    index [--offline] <repo>  Index Rust chunks and upsert semantic vectors\n    search <repo> <query>  Search indexed chunks by semantic similarity\n    serve-mcp              Preview planned read-only MCP tools\n    help                   Print this help",
        env!("CARGO_PKG_VERSION")
    );
}

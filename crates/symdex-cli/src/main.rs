use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use symdex_core::{DiscoveryOptions, RepoRoot, discover_rust_files};
use symdex_embed::EmbedConfig;
use symdex_mcp::tool_names;
use symdex_store::{StoreConfig, sqlite_parent};

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
            let repo = args.get(1).map(String::as_str).unwrap_or(".");
            index(repo)
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

    println!("network_checks: skipped (local service adapters not implemented yet)");
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

fn index(repo: &str) -> Result<(), String> {
    let root = RepoRoot::open(repo).map_err(|error| error.to_string())?;
    let files = discover_rust_files(&root, &DiscoveryOptions::default())
        .map_err(|error| error.to_string())?;

    println!("repository_id: {}", root.id());
    println!("repository_root: {}", root.path().display());
    println!("rust_files_seen: {}", files.len());
    for file in files.iter().take(20) {
        println!(
            "{} {} {}",
            file.facts.relative_path,
            file.facts.language.as_str(),
            file.facts.content_hash
        );
    }
    if files.len() > 20 {
        println!("... {} more files", files.len() - 20);
    }
    println!("embedding: skipped (offline discovery slice)");
    println!("persistence: skipped (SQLite adapter pending)");
    Ok(())
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

fn print_help() {
    println!(
        "symdex {}\n\nUSAGE:\n    symdex <command>\n\nCOMMANDS:\n    init          Create local symdex state directories\n    doctor        Print local configuration and basic diagnostics\n    index <repo>  Discover Rust files and print deterministic file facts\n    serve-mcp     Preview planned read-only MCP tools\n    help          Print this help",
        env!("CARGO_PKG_VERSION")
    );
}

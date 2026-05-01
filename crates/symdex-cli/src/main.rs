use std::env;
use std::fs;

use symdex_core::RepoRoot;
use symdex_diagnostics::{DiagnosticCheck, DiagnosticReport, DiagnosticState, run_diagnostics};
use symdex_index::{
    ContinuousIndexEvent, ContinuousIndexOptions, EmbeddingSummary, IndexOptions, IndexSummary,
    WatchChangeSet, run_continuous_index, run_index,
};
use symdex_query::{
    CallDirection, CallGraphSummary, CallPathSummary, FreshnessSummary, ImpactSummary,
    run_call_graph, run_call_path, run_context_pack, run_freshness_report, run_impact,
    run_semantic_search, run_symbol_search,
};
use symdex_store::{EvidenceFreshness, SqliteStore, StoreConfig, sqlite_parent};

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
        "staleness" | "freshness" => {
            let repo = args.get(1).map(String::as_str).unwrap_or(".");
            let symbol_query = args.get(2).map(String::as_str);
            staleness(repo, symbol_query)
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
        "call-path" => {
            let repo = args.get(1).map(String::as_str).unwrap_or(".");
            let source = args.get(2).map(String::as_str).unwrap_or("");
            let target = args.get(3).map(String::as_str).unwrap_or("");
            let max_depth = args
                .get(4)
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap_or(4);
            call_path(repo, source, target, max_depth)
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
    print_diagnostic_report(&run_diagnostics()?);
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
    if args.watch {
        return continuous_index(args);
    }
    let summary = run_index(&IndexOptions {
        repo: args.repo.clone(),
        offline: args.offline,
    })?;
    print_index_summary(&summary);
    Ok(())
}

fn continuous_index(args: &IndexArgs) -> Result<(), String> {
    println!(
        "continuous_indexing: on mode={}",
        if args.offline { "offline" } else { "semantic" }
    );
    println!("press Ctrl+C to stop");
    run_continuous_index(
        &ContinuousIndexOptions::new(args.repo.clone(), args.offline),
        print_continuous_index_event,
    )
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

fn staleness(repo: &str, symbol_query: Option<&str>) -> Result<(), String> {
    let summary = run_freshness_report(repo, symbol_query)?;
    print_freshness_summary(&summary);
    Ok(())
}

fn symbol(repo: &str, query: &str) -> Result<(), String> {
    let summary = run_symbol_search(repo, query).map_err(|error| {
        if error.contains("requires a query") {
            "symbol requires a symbol query".to_owned()
        } else {
            error
        }
    })?;
    println!("symbols: {}", summary.symbols.len());
    for symbol in summary.symbols {
        println!(
            "{} {} {}:{}-{}",
            symbol.kind, symbol.qualified_name, symbol.path, symbol.start_line, symbol.end_line
        );
    }
    Ok(())
}

fn callers(repo: &str, query: &str) -> Result<(), String> {
    let summary = run_call_graph(repo, query, CallDirection::Callers).map_err(|error| {
        if error.contains("requires a symbol query") {
            "callers requires a symbol query".to_owned()
        } else {
            error
        }
    })?;
    print_call_graph_summary(&summary);
    Ok(())
}

fn callees(repo: &str, query: &str) -> Result<(), String> {
    let summary = run_call_graph(repo, query, CallDirection::Callees).map_err(|error| {
        if error.contains("requires a symbol query") {
            "callees requires a symbol query".to_owned()
        } else {
            error
        }
    })?;
    print_call_graph_summary(&summary);
    Ok(())
}

fn call_path(repo: &str, source: &str, target: &str, max_depth: usize) -> Result<(), String> {
    let summary = run_call_path(repo, source, target, max_depth).map_err(|error| {
        if error.contains("source symbol query") {
            "call-path requires a source symbol query".to_owned()
        } else if error.contains("target symbol query") {
            "call-path requires a target symbol query".to_owned()
        } else {
            error
        }
    })?;
    print_call_path_summary(&summary);
    Ok(())
}

fn impact(repo: &str, query: &str) -> Result<(), String> {
    let summary = run_impact(repo, query).map_err(|error| {
        if error.contains("requires a symbol query") {
            "impact requires a symbol query".to_owned()
        } else {
            error
        }
    })?;
    print_impact_summary(&summary);
    Ok(())
}

fn print_call_path_summary(summary: &CallPathSummary) {
    println!("repository_id: {}", summary.repository_id);
    println!("source: {}", summary.source_query);
    println!("target: {}", summary.target_query);
    println!("max_depth: {}", summary.max_depth);
    println!("paths: {}", summary.paths.len());
    for (index, path) in summary.paths.iter().enumerate() {
        println!(
            "path {} hops={} min_confidence={:.2} terminal_status={}",
            index + 1,
            path.hops,
            path.min_confidence,
            path.terminal_resolution_status
        );
        for edge in &path.edges {
            println!(
                "  {} -> {} line={} confidence={:.2} status={} {}:{}-{} run={}",
                edge.caller_symbol_qualified_name,
                edge.callee_symbol_qualified_name
                    .as_deref()
                    .unwrap_or(&edge.callee_text),
                edge.call_line,
                edge.confidence,
                edge.resolution_status,
                edge.caller_path,
                edge.caller_start_line,
                edge.caller_end_line,
                edge.provenance.index_run_id.as_deref().unwrap_or("<none>")
            );
        }
    }
}

fn print_impact_summary(summary: &ImpactSummary) {
    println!("direct_callers");
    println!("callers: {}", summary.direct_callers.len());
    for row in &summary.direct_callers {
        print_call_row(row);
    }
    println!("direct_callees");
    println!("callees: {}", summary.direct_callees.len());
    for row in &summary.direct_callees {
        print_call_row(row);
    }
}

fn context_pack(repo: &str, query: &str) -> Result<(), String> {
    let pack = run_context_pack(repo, query, 8).map_err(|error| {
        if error.contains("requires a symbol query") {
            "context-pack requires a symbol query".to_owned()
        } else {
            error
        }
    })?;
    let json = serde_json::to_string_pretty(&pack).map_err(|error| error.to_string())?;
    println!("{json}");
    Ok(())
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

fn print_call_graph_summary(summary: &CallGraphSummary) {
    println!("{}: {}", summary.direction.label(), summary.rows.len());
    for row in &summary.rows {
        print_call_row(row);
    }
}

fn search(repo: &str, query_parts: &[String]) -> Result<(), String> {
    let query = query_parts.join(" ");
    let summary = run_semantic_search(repo, &query, 10).map_err(|error| {
        if error.contains("requires a query") {
            "search requires a query".to_owned()
        } else {
            error
        }
    })?;

    println!("repository_id: {}", summary.repository_id);
    println!("qdrant_collection: {}", summary.qdrant_collection);
    println!("results: {}", summary.results.len());
    for result in summary.results {
        println!(
            "{:.4} {}:{}-{} {} run={}",
            result.score,
            result.path,
            result.start_line,
            result.end_line,
            result.symbol_name.as_deref().unwrap_or("<none>"),
            result
                .provenance
                .index_run_id
                .as_deref()
                .unwrap_or("<none>")
        );
    }
    Ok(())
}

fn print_freshness_summary(summary: &FreshnessSummary) {
    println!("repository_id: {}", summary.repository_id);
    if let Some(query) = &summary.symbol_query {
        println!("symbol_query: {query}");
        println!("focus_symbols: {}", summary.focus_symbols.len());
        println!(
            "context_pack_files: {}",
            summary
                .context_pack
                .as_ref()
                .map(|pack| pack.files.len())
                .unwrap_or(0)
        );
    }
    println!("files: {}", summary.files.len());
    for freshness in [
        EvidenceFreshness::Fresh,
        EvidenceFreshness::Stale,
        EvidenceFreshness::Deleted,
        EvidenceFreshness::Missing,
        EvidenceFreshness::Unknown,
    ] {
        println!("{}: {}", freshness.label(), summary.count(freshness));
    }
    for file in &summary.files {
        println!(
            "{} {} indexed={} current={} run={} parser={} indexed_at={}",
            file.freshness.label(),
            file.path,
            file.indexed_content_hash.as_deref().unwrap_or("<none>"),
            file.current_content_hash.as_deref().unwrap_or("<none>"),
            file.index_run_id.as_deref().unwrap_or("<none>"),
            file.parser_version.as_deref().unwrap_or("<none>"),
            file.indexed_at.as_deref().unwrap_or("<never>")
        );
    }
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

fn print_diagnostic_report(report: &DiagnosticReport) {
    println!("symdex doctor");
    println!("workspace: {}", report.workspace);
    println!("sqlite: {}", report.sqlite_path);
    println!("qdrant: {}", report.qdrant_url);
    println!("ollama: {}", report.ollama_url);
    println!("embed_model: {}", report.embed_model);
    for check in &report.checks {
        print_diagnostic_check(check);
    }
}

fn print_diagnostic_check(check: &DiagnosticCheck) {
    match check.state {
        DiagnosticState::Ok if check.message.is_empty() => {
            println!("{}: ok", check.label);
        }
        DiagnosticState::Ok => {
            println!("{}: ok ({})", check.label, check.message);
        }
        DiagnosticState::Skipped => {
            println!("{}: skipped ({})", check.label, check.message);
        }
        state => {
            println!("{}: {} ({})", check.label, state.as_str(), check.message);
        }
    }
}

fn parse_index_args(args: &[String]) -> IndexArgs {
    let mut repo = ".".to_owned();
    let mut offline = false;
    let mut watch = false;
    for arg in args {
        if arg == "--offline" {
            offline = true;
        } else if arg == "--watch" {
            watch = true;
        } else {
            repo = arg.clone();
        }
    }
    IndexArgs {
        repo,
        offline,
        watch,
    }
}

struct IndexArgs {
    repo: String,
    offline: bool,
    watch: bool,
}

fn print_continuous_index_event(event: ContinuousIndexEvent) {
    match event {
        ContinuousIndexEvent::Started {
            repository_id,
            files_seen,
        } => {
            println!("watch_started repository_id={repository_id} files_seen={files_seen}");
        }
        ContinuousIndexEvent::Idle { .. } => {}
        ContinuousIndexEvent::ChangesPending { changes } => {
            println!("watch_pending {}", continuous_change_summary(&changes));
        }
        ContinuousIndexEvent::ChangesDetected { changes } => {
            println!("watch_changes {}", continuous_change_summary(&changes));
        }
        ContinuousIndexEvent::BatchCompleted { changes, summary } => {
            println!(
                "watch_indexed {} files_indexed={} chunks_indexed={} chunks_embedded={}",
                continuous_change_summary(&changes),
                summary.sqlite_files_indexed,
                summary.sqlite_chunks_indexed,
                chunks_embedded(&summary.embedding)
            );
        }
        ContinuousIndexEvent::BatchFailed { changes, error } => {
            println!(
                "watch_failed {} error={}",
                continuous_change_summary(&changes),
                error
            );
        }
    }
}

fn continuous_change_summary(changes: &WatchChangeSet) -> String {
    let paths = changes.paths().join(",");
    format!(
        "events={} created={} modified={} deleted={} paths={}",
        changes.event_count(),
        changes.created.len(),
        changes.modified.len(),
        changes.deleted.len(),
        if paths.is_empty() { "<none>" } else { &paths }
    )
}

fn chunks_embedded(embedding: &EmbeddingSummary) -> usize {
    match embedding {
        EmbeddingSummary::Completed {
            chunks_embedded, ..
        } => *chunks_embedded,
        EmbeddingSummary::SkippedOffline | EmbeddingSummary::SkippedNoChunks => 0,
    }
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

fn print_help() {
    println!(
        "symdex {}\n\nUSAGE:\n    symdex <command>\n\nCOMMANDS:\n    init                   Create local symdex state directories\n    doctor                 Print local configuration and diagnostics\n    index [--offline] [--watch] <repo>  Index Rust chunks and upsert semantic vectors\n    index-status <repo>    Show local SQLite index counts\n    staleness <repo> [symbol]  Compare indexed evidence hashes with current files\n    symbol <repo> <query>  Find symbols in the local index\n    callers <repo> <symbol>  Show direct callers\n    callees <repo> <symbol>  Show direct callees\n    call-path <repo> <source> <target> [depth]  Trace bounded call paths\n    impact <repo> <symbol>  Show direct callers and callees\n    context-pack <repo> <symbol>  Print compact JSON evidence for editing context\n    search <repo> <query>  Search indexed chunks by semantic similarity\n    tui [repo]             Run the local terminal UI control panel\n    serve-mcp              Run the read-only MCP server over stdio\n    help                   Print this help",
        env!("CARGO_PKG_VERSION")
    );
}

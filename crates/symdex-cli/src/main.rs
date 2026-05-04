use std::env;
use std::fs;
use std::io::Read;

use serde_json::json;
use symdex_core::RepoRoot;
use symdex_diagnostics::{
    DiagnosticCheck, DiagnosticReport, DiagnosticState, run_diagnostics_for_repo,
};
use symdex_index::{
    ContinuousIndexEvent, EmbeddingSummary, IndexOptions, IndexScope, IndexSummary,
    QualityIndexOptions, QualityIndexSummary, RustAnalyzerEnrichmentSummary, WatchChangeSet,
    run_index, run_index_with_existing_writer, run_quality_index_with_existing_writer,
    run_quality_index_with_progress,
};
use symdex_query::{
    CallDirection, CallGraphSummary, CallPathSummary, ContextPackMode, FreshnessSummary,
    ImpactSummary, SemanticStatusLayerSummary, SemanticStatusSummary, VectorVerifyOptions,
    VectorVerifySemanticLayer, VectorVerifySummary, run_call_graph, run_call_path,
    run_context_pack, run_debug_context_pack, run_freshness_report, run_impact,
    run_semantic_search, run_semantic_status, run_symbol_search, run_unified_context_pack,
    run_vector_verify_with_options,
};
use symdex_store::{
    EvidenceFreshness, SqliteStore, SqliteVectorStore, StoreConfig, WriterLease, WriterLeaseKind,
    WriterLeaseRequest, sqlite_parent,
};
use symdex_watch::{WatcherClientKind, WatcherStatus};

fn main() {
    if let Err(error) = run(env::args().skip(1).collect()) {
        eprintln!("error: {error}");
        std::process::exit(1);
    }
}

fn run(args: Vec<String>) -> Result<(), String> {
    let invocation = parse_cli_invocation(args)?;
    let output = invocation.output;
    let args = invocation.args;
    let Some(command) = args.first().map(String::as_str) else {
        print_help();
        return Ok(());
    };

    match command {
        "doctor" => {
            require_text_output(command, output)?;
            doctor(args.get(1).map(String::as_str))
        }
        "init" => {
            require_text_output(command, output)?;
            init()
        }
        "index" => {
            require_text_output(command, output)?;
            let index_args = parse_index_args(&args[1..])?;
            index(&index_args)
        }
        "watch" => {
            require_text_output(command, output)?;
            let watch_args = parse_watch_args(&args[1..])?;
            watch(&watch_args)
        }
        "watch-daemon" => {
            require_text_output(command, output)?;
            let repo = args.get(1).map(String::as_str).unwrap_or(".");
            symdex_watch::run_daemon(repo)
        }
        "index-quality" => {
            require_text_output(command, output)?;
            let repo = args.get(1).map(String::as_str).unwrap_or(".");
            index_quality(repo)
        }
        "index-status" => {
            let repo = args.get(1).map(String::as_str).unwrap_or(".");
            index_status(repo, output)
        }
        "semantic-status" => {
            let repo = args.get(1).map(String::as_str).unwrap_or(".");
            semantic_status(repo, output)
        }
        "staleness" | "freshness" => {
            require_text_output(command, output)?;
            let repo = args.get(1).map(String::as_str).unwrap_or(".");
            let symbol_query = args.get(2).map(String::as_str);
            staleness(repo, symbol_query)
        }
        "vector-verify" => {
            require_text_output(command, output)?;
            let verify_args = parse_vector_maintenance_args(&args[1..])?;
            vector_verify(&verify_args)
        }
        "qdrant-verify" => {
            require_text_output(command, output)?;
            println!("warning: qdrant-verify is deprecated; use vector-verify");
            let verify_args = parse_vector_maintenance_args(&args[1..])?;
            vector_verify(&verify_args)
        }
        "vector-repair" => {
            require_text_output(command, output)?;
            let repair_args = parse_vector_maintenance_args(&args[1..])?;
            vector_repair(&repair_args)
        }
        "qdrant-repair" => {
            require_text_output(command, output)?;
            println!("warning: qdrant-repair is deprecated; use vector-repair");
            let repair_args = parse_vector_maintenance_args(&args[1..])?;
            vector_repair(&repair_args)
        }
        "symbol" => {
            let repo = args.get(1).map(String::as_str).unwrap_or(".");
            let query = args.get(2).map(String::as_str).unwrap_or("");
            symbol(repo, query, output)
        }
        "callers" => {
            let repo = args.get(1).map(String::as_str).unwrap_or(".");
            let query = args.get(2).map(String::as_str).unwrap_or("");
            callers(repo, query, output)
        }
        "callees" => {
            let repo = args.get(1).map(String::as_str).unwrap_or(".");
            let query = args.get(2).map(String::as_str).unwrap_or("");
            callees(repo, query, output)
        }
        "call-path" => {
            let repo = args.get(1).map(String::as_str).unwrap_or(".");
            let source = args.get(2).map(String::as_str).unwrap_or("");
            let target = args.get(3).map(String::as_str).unwrap_or("");
            let max_depth = args
                .get(4)
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap_or(4);
            call_path(repo, source, target, max_depth, output)
        }
        "impact" => {
            let repo = args.get(1).map(String::as_str).unwrap_or(".");
            let query = args.get(2).map(String::as_str).unwrap_or("");
            impact(repo, query, output)
        }
        "context-pack" => {
            let context_args = parse_context_pack_args(&args[1..])?;
            context_pack(
                &context_args.repo,
                &context_args.query,
                context_args.mode,
                output,
            )
        }
        "debug-context" => {
            let repo = args.get(1).map(String::as_str).unwrap_or(".");
            let input_parts = if args.len() > 2 { &args[2..] } else { &[] };
            debug_context(repo, input_parts, output)
        }
        "search" => {
            let repo = args.get(1).map(String::as_str).unwrap_or(".");
            let query_parts = if args.len() > 2 { &args[2..] } else { &[] };
            search(repo, query_parts, output)
        }
        "tui" => {
            require_text_output(command, output)?;
            let repo = args.get(1).map(String::as_str).unwrap_or(".");
            tui(repo)
        }
        "serve-mcp" => {
            require_text_output(command, output)?;
            let serve_args = parse_serve_mcp_args(&args[1..])?;
            serve_mcp(&serve_args)
        }
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct CliInvocation {
    output: OutputMode,
    args: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OutputMode {
    Text,
    Json,
}

fn parse_cli_invocation(args: Vec<String>) -> Result<CliInvocation, String> {
    let mut output = OutputMode::Text;
    let mut remaining = Vec::new();
    let mut iterator = args.into_iter();
    while let Some(arg) = iterator.next() {
        match arg.as_str() {
            "--json" => output = OutputMode::Json,
            "--output" => {
                let Some(value) = iterator.next() else {
                    return Err("--output requires a value".to_owned());
                };
                output = parse_output_value(&value)?;
            }
            value if value.starts_with("--output=") => {
                output = parse_output_value(value.trim_start_matches("--output="))?;
            }
            _ => {
                remaining.push(arg);
                remaining.extend(iterator);
                break;
            }
        }
    }
    Ok(CliInvocation {
        output,
        args: remaining,
    })
}

fn parse_output_value(value: &str) -> Result<OutputMode, String> {
    match value {
        "text" => Ok(OutputMode::Text),
        "json" => Ok(OutputMode::Json),
        other => Err(format!("unsupported output format `{other}`")),
    }
}

fn require_text_output(command: &str, output: OutputMode) -> Result<(), String> {
    if output == OutputMode::Json {
        return Err(format!(
            "{command} does not support MCP evidence JSON output"
        ));
    }
    Ok(())
}

fn doctor(repo: Option<&str>) -> Result<(), String> {
    print_diagnostic_report(&run_diagnostics_for_repo(repo)?);
    Ok(())
}

fn init() -> Result<(), String> {
    let store = StoreConfig::from_env();
    let _writer = WriterLease::acquire(
        &store,
        WriterLeaseRequest::new(WriterLeaseKind::Init, "init"),
    )
    .map_err(|error| error.to_string())?;
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
        scope: args.scope,
    })?;
    print_index_summary(&summary);
    Ok(())
}

fn index_quality(repo: &str) -> Result<(), String> {
    let summary = run_quality_index_with_progress(
        &QualityIndexOptions {
            repo: repo.to_owned(),
        },
        |progress| {
            println!(
                "quality_progress phase={} completed={} total={} message={}",
                progress.phase, progress.completed, progress.total, progress.message
            );
        },
    )?;
    print_quality_index_summary(&summary);
    Ok(())
}

fn continuous_index(args: &IndexArgs) -> Result<(), String> {
    println!(
        "continuous_indexing: on mode={}",
        if args.offline { "offline" } else { "semantic" }
    );
    println!("press Ctrl+C to stop");
    symdex_watch::run_foreground(&args.repo, args.offline, print_continuous_index_event)
}

fn index_status(repo: &str, output: OutputMode) -> Result<(), String> {
    if output == OutputMode::Json {
        return print_mcp_json_tool(symdex_mcp::TOOL_INDEX_STATUS, json!({ "repo": repo }));
    }
    let root = RepoRoot::open(repo).map_err(|error| error.to_string())?;
    let store_config = StoreConfig::from_env();
    let sqlite = SqliteStore::open_read_only(&store_config).map_err(|error| error.to_string())?;
    let status = sqlite
        .repository_status(root.id())
        .map_err(|error| error.to_string())?;

    println!("repository_id: {}", status.repository_id);
    println!(
        "current_ref: {}",
        status.current_ref_name.as_deref().unwrap_or("<none>")
    );
    println!(
        "current_ref_kind: {}",
        status.current_ref_kind.as_deref().unwrap_or("<none>")
    );
    println!(
        "current_ref_id: {}",
        status.current_ref_id.as_deref().unwrap_or("<none>")
    );
    println!(
        "current_head_oid: {}",
        status.current_head_oid.as_deref().unwrap_or("<none>")
    );
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

fn semantic_status(repo: &str, output: OutputMode) -> Result<(), String> {
    let summary = run_semantic_status(repo)?;
    if output == OutputMode::Json {
        let json = serde_json::to_string_pretty(&semantic_status_json(&summary))
            .map_err(|error| error.to_string())?;
        println!("{json}");
        return Ok(());
    }
    print_semantic_status_summary(&summary);
    Ok(())
}

fn print_semantic_status_summary(summary: &SemanticStatusSummary) {
    println!("repository_id: {}", summary.repository_id);
    println!(
        "generation_id: {}",
        summary.generation_id.as_deref().unwrap_or("<none>")
    );
    println!("active_layer: {}", summary.active_layer.as_str());
    println!("quality_status: {}", summary.quality_status.as_str());
    println!("quality_enabled: {}", summary.quality_enabled);
    println!(
        "fallback_reason: {}",
        summary.fallback_reason.as_deref().unwrap_or("<none>")
    );
    println!(
        "latest_quality_error: {}",
        summary.latest_quality_error.as_deref().unwrap_or("<none>")
    );
    print_semantic_status_layer("fast", &summary.fast);
    print_semantic_status_layer("quality", &summary.quality);
    if let Some(progress) = &summary.quality_progress {
        println!("quality_progress:");
        println!("  embeddable_chunks: {}", progress.embeddable_chunks);
        println!(
            "  quality_eligible_chunks: {}",
            progress.quality_eligible_chunks
        );
        println!(
            "  quality_ineligible_chunks: {}",
            progress.quality_ineligible_chunks
        );
        println!(
            "  quality_embedded_chunks: {}",
            progress.quality_embedded_chunks
        );
        println!("  pending_jobs: {}", progress.pending_jobs);
        println!("  running_jobs: {}", progress.running_jobs);
        println!("  succeeded_jobs: {}", progress.succeeded_jobs);
        println!("  failed_jobs: {}", progress.failed_jobs);
        println!("  skipped_stale_jobs: {}", progress.skipped_stale_jobs);
        println!(
            "  skipped_excluded_jobs: {}",
            progress.skipped_excluded_jobs
        );
    } else {
        println!("quality_progress: <none>");
    }
}

fn print_semantic_status_layer(label: &str, layer: &SemanticStatusLayerSummary) {
    println!("{label}_layer:");
    println!("  semantic_layer: {}", layer.semantic_layer.as_str());
    println!("  embedding_model: {}", layer.embedding_model);
    println!(
        "  embedding_dimension: {}",
        layer
            .embedding_dimension
            .map(|dimension| dimension.to_string())
            .unwrap_or_else(|| "<none>".to_owned())
    );
    println!("  vector_store: sqlite_vec");
    println!("  vector_table: {}", layer.vector_table);
    println!("  expected_chunks: {}", layer.expected_chunks);
    println!("  current_chunks: {}", layer.current_chunks);
    println!("  stale_chunks: {}", layer.stale_chunks);
    println!("  blocked_chunks: {}", layer.blocked_chunks);
    println!("  failed_chunks: {}", layer.failed_chunks);
    println!("  other_chunks: {}", layer.other_chunks);
    println!("  total_chunks: {}", layer.total_chunks);
    println!("  is_complete: {}", layer.is_complete);
}

fn semantic_status_json(summary: &SemanticStatusSummary) -> serde_json::Value {
    json!({
        "repository_id": summary.repository_id.as_str(),
        "generation_id": summary.generation_id.as_deref(),
        "active_layer": summary.active_layer.as_str(),
        "quality_status": summary.quality_status.as_str(),
        "quality_enabled": summary.quality_enabled,
        "fallback_reason": summary.fallback_reason.as_deref(),
        "latest_quality_error": summary.latest_quality_error.as_deref(),
        "fast": semantic_status_layer_json(&summary.fast),
        "quality": semantic_status_layer_json(&summary.quality),
        "quality_progress": summary.quality_progress.as_ref().map(|progress| json!({
            "repository_id": progress.repository_id.as_str(),
            "generation_id": progress.generation_id.as_str(),
            "embeddable_chunks": progress.embeddable_chunks,
            "quality_eligible_chunks": progress.quality_eligible_chunks,
            "quality_ineligible_chunks": progress.quality_ineligible_chunks,
            "quality_embedded_chunks": progress.quality_embedded_chunks,
            "pending_jobs": progress.pending_jobs,
            "running_jobs": progress.running_jobs,
            "succeeded_jobs": progress.succeeded_jobs,
            "failed_jobs": progress.failed_jobs,
            "skipped_stale_jobs": progress.skipped_stale_jobs,
            "skipped_excluded_jobs": progress.skipped_excluded_jobs,
        })),
    })
}

fn semantic_status_layer_json(layer: &SemanticStatusLayerSummary) -> serde_json::Value {
    json!({
        "semantic_layer": layer.semantic_layer.as_str(),
        "embedding_model": layer.embedding_model.as_str(),
        "embedding_dimension": layer.embedding_dimension,
        "vector_store": "sqlite_vec",
        "vector_table": layer.vector_table.as_str(),
        "expected_chunks": layer.expected_chunks,
        "current_chunks": layer.current_chunks,
        "stale_chunks": layer.stale_chunks,
        "blocked_chunks": layer.blocked_chunks,
        "failed_chunks": layer.failed_chunks,
        "other_chunks": layer.other_chunks,
        "total_chunks": layer.total_chunks,
        "is_complete": layer.is_complete,
    })
}

fn staleness(repo: &str, symbol_query: Option<&str>) -> Result<(), String> {
    let summary = run_freshness_report(repo, symbol_query)?;
    print_freshness_summary(&summary);
    Ok(())
}

fn watch(args: &WatchArgs) -> Result<(), String> {
    let status = match args.action {
        WatchAction::Start => {
            let attachment = symdex_watch::start_or_attach(&args.repo, WatcherClientKind::Cli)?;
            let status = attachment.status()?;
            drop(attachment);
            status
        }
        WatchAction::Status => symdex_watch::status(&args.repo)?,
        WatchAction::Stop => symdex_watch::stop_daemon(&args.repo)?,
    };
    print_watcher_status(&status);
    Ok(())
}

fn vector_verify(args: &VectorMaintenanceArgs) -> Result<(), String> {
    let summary = run_vector_verify_with_options(
        &args.repo,
        VectorVerifyOptions {
            semantic_layer: args.semantic_layer,
        },
    )?;
    print_vector_verify_summary(&summary);
    Ok(())
}

fn vector_repair(args: &VectorMaintenanceArgs) -> Result<(), String> {
    let root = RepoRoot::open(&args.repo).map_err(|error| error.to_string())?;
    let store_config = StoreConfig::from_env();
    let _writer = WriterLease::acquire(
        &store_config,
        WriterLeaseRequest::new(WriterLeaseKind::VectorRepair, "vector-repair")
            .for_repo(root.id(), root.path().display().to_string()),
    )
    .map_err(|error| error.to_string())?;
    let options = VectorVerifyOptions {
        semantic_layer: args.semantic_layer,
    };
    let before = run_vector_verify_with_options(&args.repo, options)?;
    println!("pre_repair_verify:");
    print_vector_verify_summary(&before);

    let orphaned_points_deleted = delete_orphaned_vector_points(&before)?;
    println!("orphaned_points_deleted: {orphaned_points_deleted}");

    repair_vector_summary(&args.repo, &before)?;

    let after = run_vector_verify_with_options(&args.repo, options)?;
    println!("post_repair_verify:");
    print_vector_verify_summary(&after);
    Ok(())
}

fn repair_vector_summary(repo: &str, summary: &VectorVerifySummary) -> Result<(), String> {
    if !summary.layer_summaries.is_empty() {
        for layer_summary in &summary.layer_summaries {
            repair_vector_summary(repo, layer_summary)?;
        }
        return Ok(());
    }

    let reindex_required = summary.missing_points > 0 || summary.stale_payload_points > 0;
    if !reindex_required {
        println!(
            "semantic_repair layer={} action=skipped",
            summary.semantic_layer
        );
        return Ok(());
    }

    match VectorVerifySemanticLayer::parse(&summary.semantic_layer)? {
        VectorVerifySemanticLayer::Fast => {
            println!("semantic_repair layer=fast action=reindex_started");
            let index_summary = run_index_with_existing_writer(&IndexOptions {
                repo: repo.to_owned(),
                offline: false,
                scope: IndexScope::Full,
            })?;
            print_index_summary(&index_summary);
        }
        VectorVerifySemanticLayer::Quality => {
            println!("semantic_repair layer=quality action=quality_worker_started");
            let quality_summary = run_quality_index_with_existing_writer(&QualityIndexOptions {
                repo: repo.to_owned(),
            })?;
            print_quality_index_summary(&quality_summary);
        }
        VectorVerifySemanticLayer::All => {}
    }
    Ok(())
}

fn delete_orphaned_vector_points(summary: &VectorVerifySummary) -> Result<usize, String> {
    if !summary.layer_summaries.is_empty() {
        return summary
            .layer_summaries
            .iter()
            .try_fold(0usize, |deleted, layer_summary| {
                Ok(deleted + delete_orphaned_vector_points(layer_summary)?)
            });
    }
    if !summary.table_exists || summary.orphaned_point_ids.is_empty() {
        return Ok(0);
    }
    let store_config = StoreConfig::from_env();
    let vector = SqliteVectorStore::new(&store_config).map_err(|error| error.to_string())?;
    vector
        .delete_points(&summary.collection_name, &summary.orphaned_point_ids)
        .map_err(|error| error.to_string())?;
    Ok(summary.orphaned_point_ids.len())
}

fn symbol(repo: &str, query: &str, output: OutputMode) -> Result<(), String> {
    if output == OutputMode::Json {
        return print_mcp_json_tool(
            symdex_mcp::TOOL_FIND_SYMBOL,
            json!({ "repo": repo, "name": query, "limit": 10 }),
        );
    }
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

fn callers(repo: &str, query: &str, output: OutputMode) -> Result<(), String> {
    if output == OutputMode::Json {
        return print_mcp_json_tool(
            symdex_mcp::TOOL_CALLERS,
            json!({ "repo": repo, "symbol": query }),
        );
    }
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

fn callees(repo: &str, query: &str, output: OutputMode) -> Result<(), String> {
    if output == OutputMode::Json {
        return print_mcp_json_tool(
            symdex_mcp::TOOL_CALLEES,
            json!({ "repo": repo, "symbol": query }),
        );
    }
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

fn call_path(
    repo: &str,
    source: &str,
    target: &str,
    max_depth: usize,
    output: OutputMode,
) -> Result<(), String> {
    if output == OutputMode::Json {
        return print_mcp_json_tool(
            symdex_mcp::TOOL_CALL_PATH,
            json!({
                "repo": repo,
                "source": source,
                "target": target,
                "max_depth": max_depth
            }),
        );
    }
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

fn impact(repo: &str, query: &str, output: OutputMode) -> Result<(), String> {
    if output == OutputMode::Json {
        return print_mcp_json_tool(
            symdex_mcp::TOOL_IMPACT,
            json!({ "repo": repo, "symbol": query, "depth": 4 }),
        );
    }
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
    println!("repository_id: {}", summary.repository_id);
    println!("query: {}", summary.query);
    println!("max_depth: {}", summary.max_depth);
    println!("direct_callers");
    println!("callers: {}", summary.direct_callers.len());
    for evidence in &summary.direct_callers {
        print_impact_call_evidence(evidence);
    }
    println!("direct_callees");
    println!("callees: {}", summary.direct_callees.len());
    for evidence in &summary.direct_callees {
        print_impact_call_evidence(evidence);
    }
    println!("transitive_callers: {}", summary.transitive_callers.len());
    for evidence in &summary.transitive_callers {
        print_impact_path_evidence(evidence);
    }
    println!("transitive_callees: {}", summary.transitive_callees.len());
    for evidence in &summary.transitive_callees {
        print_impact_path_evidence(evidence);
    }
    println!("related_files: {}", summary.related_files.len());
    for file in &summary.related_files {
        println!(
            "{} relationships={} freshness={} trust={:.2}/{} reasons={} run={}",
            file.path,
            file.relationship_count,
            file.freshness.label(),
            file.trust.score,
            file.trust.level,
            reason_list(&file.reasons),
            file.provenance
                .as_ref()
                .and_then(|provenance| provenance.index_run_id.as_deref())
                .unwrap_or("<none>")
        );
    }
    println!("tests_likely: {}", summary.tests_likely.len());
    for test in &summary.tests_likely {
        println!("test: {test}");
    }
    for note in &summary.notes {
        println!("note: {note}");
    }
}

fn print_impact_call_evidence(evidence: &symdex_query::ImpactCallEvidence) {
    print_call_row(&evidence.row);
    println!(
        "freshness={} trust={:.2}/{} reasons={}",
        evidence.freshness.label(),
        evidence.trust.score,
        evidence.trust.level,
        reason_list(&evidence.reasons)
    );
}

fn print_impact_path_evidence(evidence: &symdex_query::ImpactPathEvidence) {
    println!(
        "path hops={} min_confidence={:.2} terminal_status={} trust={:.2}/{} reasons={}",
        evidence.path.hops,
        evidence.path.min_confidence,
        evidence.path.terminal_resolution_status,
        evidence.trust.score,
        evidence.trust.level,
        reason_list(&evidence.reasons)
    );
    for (((edge, freshness), trust), reasons) in evidence
        .path
        .edges
        .iter()
        .zip(&evidence.edge_freshness)
        .zip(&evidence.edge_trust)
        .zip(&evidence.edge_reasons)
    {
        println!(
            "  {} -> {} line={} confidence={:.2} status={} freshness={} trust={:.2}/{} reasons={} {}:{}-{} run={}",
            edge.caller_symbol_qualified_name,
            edge.callee_symbol_qualified_name
                .as_deref()
                .unwrap_or(&edge.callee_text),
            edge.call_line,
            edge.confidence,
            edge.resolution_status,
            freshness.label(),
            trust.score,
            trust.level,
            reason_list(reasons),
            edge.caller_path,
            edge.caller_start_line,
            edge.caller_end_line,
            edge.provenance.index_run_id.as_deref().unwrap_or("<none>")
        );
    }
}

fn print_vector_verify_summary(summary: &VectorVerifySummary) {
    print_vector_verify_summary_with_prefix(summary, "");
    for layer_summary in &summary.layer_summaries {
        println!("layer_summary: {}", layer_summary.semantic_layer);
        print_vector_verify_summary_with_prefix(layer_summary, "  ");
    }
}

fn print_vector_verify_summary_with_prefix(summary: &VectorVerifySummary, prefix: &str) {
    println!("{prefix}repository_id: {}", summary.repository_id);
    println!("{prefix}semantic_layer: {}", summary.semantic_layer);
    println!("{prefix}vector_store: sqlite_vec");
    println!("{prefix}vector_table: {}", summary.collection_name);
    println!("{prefix}embedding_model: {}", summary.embedding_model);
    println!("{prefix}table_exists: {}", summary.table_exists);
    println!(
        "{prefix}expected_vector_points: {}",
        summary.expected_vector_points
    );
    println!(
        "{prefix}vector_payload_points: {}",
        summary.vector_payload_points
    );
    println!("{prefix}missing_points: {}", summary.missing_points);
    println!(
        "{prefix}stale_payload_points: {}",
        summary.stale_payload_points
    );
    println!("{prefix}orphaned_points: {}", summary.orphaned_points);
    for row in &summary.rows {
        println!(
            "{prefix}{} {} {}",
            storage_health_status_label(row.status),
            row.label,
            row.detail
        );
    }
}

fn storage_health_status_label(status: symdex_store::StorageHealthStatus) -> &'static str {
    match status {
        symdex_store::StorageHealthStatus::Ok => "ok",
        symdex_store::StorageHealthStatus::Warning => "warning",
        symdex_store::StorageHealthStatus::Error => "error",
    }
}

fn context_pack(
    repo: &str,
    query: &str,
    mode: ContextPackMode,
    output: OutputMode,
) -> Result<(), String> {
    if output == OutputMode::Json {
        return print_mcp_json_tool(
            symdex_mcp::TOOL_CONTEXT_PACK,
            json!({ "repo": repo, "symbol": query, "limit": 8, "mode": mode.label() }),
        );
    }
    let json = match mode {
        ContextPackMode::Structural => {
            let pack = run_context_pack(repo, query, 8).map_err(context_pack_error)?;
            serde_json::to_string_pretty(&pack).map_err(|error| error.to_string())?
        }
        ContextPackMode::Unified => {
            let pack = run_unified_context_pack(repo, query, 8).map_err(context_pack_error)?;
            serde_json::to_string_pretty(&pack).map_err(|error| error.to_string())?
        }
    };
    println!("{json}");
    Ok(())
}

fn context_pack_error(error: String) -> String {
    if error.contains("requires a symbol query") {
        "context-pack requires a symbol query".to_owned()
    } else {
        error
    }
}

fn debug_context(repo: &str, input_parts: &[String], output: OutputMode) -> Result<(), String> {
    let input = runtime_input(input_parts)?;
    if output == OutputMode::Json {
        return print_mcp_json_tool(
            symdex_mcp::TOOL_DEBUG_CONTEXT,
            json!({ "repo": repo, "input": input, "limit": 8 }),
        );
    }
    let pack = run_debug_context_pack(repo, &input, 8)?;
    let json = serde_json::to_string_pretty(&pack).map_err(|error| error.to_string())?;
    println!("{json}");
    Ok(())
}

fn runtime_input(input_parts: &[String]) -> Result<String, String> {
    let Some(first) = input_parts.first() else {
        return Err("debug-context requires runtime failure input or `-` for stdin".to_owned());
    };
    if first == "-" {
        let mut input = String::new();
        std::io::stdin()
            .read_to_string(&mut input)
            .map_err(|error| format!("read runtime failure input from stdin: {error}"))?;
        return Ok(input);
    }
    if input_parts.len() == 1
        && let Ok(input) = fs::read_to_string(first)
    {
        return Ok(input);
    }
    Ok(input_parts.join(" "))
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

fn search(repo: &str, query_parts: &[String], output: OutputMode) -> Result<(), String> {
    let query = query_parts.join(" ");
    if output == OutputMode::Json {
        return print_mcp_json_tool(
            symdex_mcp::TOOL_SEARCH,
            json!({ "repo": repo, "query": query, "limit": 10 }),
        );
    }
    let summary = run_semantic_search(repo, &query, 10).map_err(|error| {
        if error.contains("requires a query") {
            "search requires a query".to_owned()
        } else {
            error
        }
    })?;

    println!("repository_id: {}", summary.repository_id);
    println!("vector_store: sqlite_vec");
    println!("vector_table: {}", summary.vector_table);
    println!("results: {}", summary.results.len());
    for result in summary.results {
        println!(
            "{:.4} {}:{}-{} {} reasons={} run={}",
            result.score,
            result.path,
            result.start_line,
            result.end_line,
            result.symbol_name.as_deref().unwrap_or("<none>"),
            reason_list(&result.reasons),
            result
                .provenance
                .index_run_id
                .as_deref()
                .unwrap_or("<none>")
        );
    }
    Ok(())
}

fn reason_list(reasons: &[String]) -> String {
    if reasons.is_empty() {
        return "<none>".to_owned();
    }
    reasons.join(",")
}

fn print_mcp_json_tool(name: &str, arguments: serde_json::Value) -> Result<(), String> {
    let result = symdex_mcp::evidence_tool_result(name, &arguments)?;
    let json = serde_json::to_string_pretty(&result).map_err(|error| error.to_string())?;
    println!("{json}");
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
    println!("files_seen: {}", summary.files_seen);
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
        for diagnostic in file.parse_diagnostics.iter().take(5) {
            println!(
                "  parse_diagnostic lines={}-{} bytes={}-{} {}",
                diagnostic.start_line,
                diagnostic.end_line,
                diagnostic.start_byte,
                diagnostic.end_byte,
                diagnostic.message
            );
        }
        if file.parse_diagnostics.len() > 5 {
            println!(
                "  ... {} more parse diagnostics",
                file.parse_diagnostics.len() - 5
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
    match &summary.rust_analyzer {
        RustAnalyzerEnrichmentSummary::Disabled { enable_env } => {
            println!("rust_analyzer_enrichment: disabled (set {enable_env}=1 to force)");
        }
        RustAnalyzerEnrichmentSummary::NotReady { command, reason } => {
            println!("rust_analyzer_enrichment: not_ready command={command} reason={reason}");
        }
        RustAnalyzerEnrichmentSummary::SkippedNoRustFiles { command, version } => {
            println!(
                "rust_analyzer_enrichment: skipped_no_rust_files command={command} version={version}"
            );
        }
        RustAnalyzerEnrichmentSummary::Planned {
            command,
            version,
            eligible_files,
            eligible_symbols,
            eligible_calls,
        } => {
            println!(
                "rust_analyzer_enrichment: planned command={command} version={version} files={eligible_files} symbols={eligible_symbols} calls={eligible_calls}"
            );
        }
    }
    match &summary.embedding {
        EmbeddingSummary::SkippedOffline => {
            println!("embedding: skipped (--offline)");
            println!("vector_store: skipped (--offline)");
        }
        EmbeddingSummary::SkippedNoChunks => {
            println!("chunks_embedded: 0");
            println!("vector_store: skipped (no chunks)");
        }
        EmbeddingSummary::Completed {
            model,
            dimension,
            vector_table,
            chunks_embedded,
        } => {
            println!("embedding_model: {model}");
            println!("embedding_dimension: {dimension}");
            println!("vector_store: sqlite_vec");
            println!("vector_table: {vector_table}");
            println!("chunks_embedded: {chunks_embedded}");
        }
    }
}

fn print_quality_index_summary(summary: &QualityIndexSummary) {
    println!("repository_id: {}", summary.repository_id);
    println!("generation_id: {}", summary.generation_id);
    println!("quality_model: {}", summary.quality_model);
    println!(
        "quality_dimension: {}",
        summary
            .quality_dimension
            .map(|dimension| dimension.to_string())
            .unwrap_or_else(|| "<none>".to_owned())
    );
    println!("quality_status: {}", summary.quality_status);
    println!("active_layer: {}", summary.active_layer);
    println!("activation_reason: {}", summary.activation_reason);
    println!("vector_store: sqlite_vec");
    println!("vector_table: {}", summary.vector_table);
    println!("claimed_jobs: {}", summary.claimed_jobs);
    println!("succeeded_jobs: {}", summary.succeeded_jobs);
    println!("failed_jobs: {}", summary.failed_jobs);
    println!("skipped_stale_jobs: {}", summary.skipped_stale_jobs);
    println!("skipped_excluded_jobs: {}", summary.skipped_excluded_jobs);
    println!("remaining_pending_jobs: {}", summary.remaining_pending_jobs);
    println!(
        "progress: embeddable_chunks={} quality_eligible_chunks={} quality_ineligible_chunks={} quality_embedded_chunks={} pending={} running={} succeeded={} failed={} skipped_stale={} skipped_excluded={}",
        summary.progress.embeddable_chunks,
        summary.progress.quality_eligible_chunks,
        summary.progress.quality_ineligible_chunks,
        summary.progress.quality_embedded_chunks,
        summary.progress.pending_jobs,
        summary.progress.running_jobs,
        summary.progress.succeeded_jobs,
        summary.progress.failed_jobs,
        summary.progress.skipped_stale_jobs,
        summary.progress.skipped_excluded_jobs
    );
}

fn print_diagnostic_report(report: &DiagnosticReport) {
    println!("symdex doctor");
    println!("workspace: {}", report.workspace);
    println!("sqlite: {}", report.sqlite_path);
    println!("vector_store: {}", report.vector_store);
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

fn parse_index_args(args: &[String]) -> Result<IndexArgs, String> {
    let mut repo = ".".to_owned();
    let mut offline = false;
    let mut watch = false;
    let mut scope = None;
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        if arg == "--offline" {
            offline = true;
        } else if arg == "--watch" {
            watch = true;
        } else if arg == "--full" {
            set_index_scope(&mut scope, IndexScope::Full)?;
        } else if arg == "--incremental" {
            set_index_scope(&mut scope, IndexScope::Incremental)?;
        } else if arg == "--scope" {
            index += 1;
            let Some(value) = args.get(index) else {
                return Err("--scope requires a value: full or incremental".to_owned());
            };
            set_index_scope(&mut scope, parse_index_scope(value)?)?;
        } else if let Some(value) = arg.strip_prefix("--scope=") {
            set_index_scope(&mut scope, parse_index_scope(value)?)?;
        } else if arg.starts_with("--") {
            return Err(format!("unsupported index option `{arg}`"));
        } else {
            repo = arg.clone();
        }
        index += 1;
    }

    let scope = scope.unwrap_or(if watch || offline {
        IndexScope::Incremental
    } else {
        IndexScope::Full
    });
    if watch && scope == IndexScope::Full {
        return Err(
            "--watch uses incremental indexing; --full is not supported with --watch".to_owned(),
        );
    }

    Ok(IndexArgs {
        repo,
        offline,
        watch,
        scope,
    })
}

fn set_index_scope(current: &mut Option<IndexScope>, next: IndexScope) -> Result<(), String> {
    if let Some(current) = current
        && *current != next
    {
        return Err("--full and --incremental are mutually exclusive".to_owned());
    }
    *current = Some(next);
    Ok(())
}

fn parse_index_scope(value: &str) -> Result<IndexScope, String> {
    match value {
        "full" => Ok(IndexScope::Full),
        "incremental" => Ok(IndexScope::Incremental),
        other => Err(format!(
            "unsupported index scope `{other}`; expected `full` or `incremental`"
        )),
    }
}

fn parse_context_pack_args(args: &[String]) -> Result<ContextPackArgs, String> {
    let mut positional = Vec::new();
    let mut mode = ContextPackMode::Structural;
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        if arg == "--mode" {
            index += 1;
            let Some(value) = args.get(index) else {
                return Err("--mode requires a value".to_owned());
            };
            mode = ContextPackMode::parse(value)?;
        } else if let Some(value) = arg.strip_prefix("--mode=") {
            mode = ContextPackMode::parse(value)?;
        } else if arg.starts_with("--") {
            return Err(format!("unsupported context-pack option `{arg}`"));
        } else {
            positional.push(arg.clone());
        }
        index += 1;
    }

    Ok(ContextPackArgs {
        repo: positional
            .first()
            .cloned()
            .unwrap_or_else(|| ".".to_owned()),
        query: positional.get(1).cloned().unwrap_or_default(),
        mode,
    })
}

fn parse_serve_mcp_args(args: &[String]) -> Result<ServeMcpArgs, String> {
    let mut watch_repo = None;
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        if arg == "--watch" {
            if watch_repo.is_some() {
                return Err("serve-mcp accepts at most one --watch repository".to_owned());
            }
            index += 1;
            let Some(repo) = args.get(index) else {
                return Err("serve-mcp --watch requires a repository path".to_owned());
            };
            if repo.starts_with("--") {
                return Err("serve-mcp --watch requires a repository path".to_owned());
            }
            watch_repo = Some(repo.clone());
        } else if arg == "--offline" {
            return Err(
                "serve-mcp --watch runs semantic indexing; use `symdex index --offline --watch <repo>` for offline watch mode"
                    .to_owned(),
            );
        } else if arg.starts_with("--") {
            return Err(format!("unsupported serve-mcp option `{arg}`"));
        } else {
            return Err(format!("unexpected serve-mcp argument `{arg}`"));
        }
        index += 1;
    }

    Ok(ServeMcpArgs { watch_repo })
}

fn parse_watch_args(args: &[String]) -> Result<WatchArgs, String> {
    let Some(action) = args.first() else {
        return Err("watch requires an action: start, status, or stop".to_owned());
    };
    let action = match action.as_str() {
        "start" => WatchAction::Start,
        "status" => WatchAction::Status,
        "stop" => WatchAction::Stop,
        other => return Err(format!("unsupported watch action `{other}`")),
    };
    let repo = args.get(1).cloned().unwrap_or_else(|| ".".to_owned());
    if args.len() > 2 {
        return Err("watch accepts at most one repository path".to_owned());
    }
    Ok(WatchArgs { action, repo })
}

fn parse_vector_maintenance_args(args: &[String]) -> Result<VectorMaintenanceArgs, String> {
    let mut positional = Vec::new();
    let mut semantic_layer = VectorVerifySemanticLayer::Fast;
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        if arg == "--semantic-layer" {
            index += 1;
            let Some(value) = args.get(index) else {
                return Err("--semantic-layer requires a value".to_owned());
            };
            semantic_layer = VectorVerifySemanticLayer::parse(value)?;
        } else if let Some(value) = arg.strip_prefix("--semantic-layer=") {
            semantic_layer = VectorVerifySemanticLayer::parse(value)?;
        } else if arg == "--all-semantic-layers" {
            semantic_layer = VectorVerifySemanticLayer::All;
        } else if arg.starts_with("--") {
            return Err(format!("unsupported vector maintenance option `{arg}`"));
        } else {
            positional.push(arg.clone());
        }
        index += 1;
    }

    Ok(VectorMaintenanceArgs {
        repo: positional
            .first()
            .cloned()
            .unwrap_or_else(|| ".".to_owned()),
        semantic_layer,
    })
}

#[derive(Debug)]
struct IndexArgs {
    repo: String,
    offline: bool,
    watch: bool,
    scope: IndexScope,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ServeMcpArgs {
    watch_repo: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct WatchArgs {
    action: WatchAction,
    repo: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WatchAction {
    Start,
    Status,
    Stop,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct VectorMaintenanceArgs {
    repo: String,
    semantic_layer: VectorVerifySemanticLayer,
}

struct ContextPackArgs {
    repo: String,
    query: String,
    mode: ContextPackMode,
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
        ContinuousIndexEvent::QualityState { state } => {
            println!("watch_quality_state {}", continuous_quality_summary(&state));
        }
        ContinuousIndexEvent::QualityStarted { state } => {
            println!(
                "watch_quality_started {}",
                continuous_quality_summary(&state)
            );
        }
        ContinuousIndexEvent::QualityProgress { progress } => {
            println!(
                "watch_quality_progress phase={} completed={} total={} message={}",
                progress.phase, progress.completed, progress.total, progress.message
            );
        }
        ContinuousIndexEvent::QualityCompleted { summary } => {
            println!(
                "watch_quality_completed generation_id={} active_layer={} quality_status={} activation_reason={} claimed_jobs={} succeeded_jobs={} failed_jobs={} skipped_stale_jobs={} remaining_pending_jobs={}",
                summary.generation_id,
                summary.active_layer,
                summary.quality_status,
                summary.activation_reason,
                summary.claimed_jobs,
                summary.succeeded_jobs,
                summary.failed_jobs,
                summary.skipped_stale_jobs,
                summary.remaining_pending_jobs
            );
        }
        ContinuousIndexEvent::QualityFailed { state, error } => {
            if let Some(state) = state {
                println!(
                    "watch_quality_failed {} error={}",
                    continuous_quality_summary(&state),
                    error
                );
            } else {
                println!("watch_quality_failed error={error}");
            }
        }
    }
}

fn print_watcher_status(status: &WatcherStatus) {
    println!("watcher_state: {}", status.state);
    println!("repository_id: {}", status.repository_id);
    println!("root_path: {}", status.root_path);
    println!("mode: {}", status.mode);
    println!("owner_kind: {}", status.owner_kind);
    println!(
        "owner_pid: {}",
        status
            .owner_pid
            .map(|pid| pid.to_string())
            .unwrap_or_else(|| "<none>".to_owned())
    );
    println!(
        "socket_path: {}",
        status.socket_path.as_deref().unwrap_or("<none>")
    );
    println!(
        "heartbeat_at: {}",
        status.heartbeat_at.as_deref().unwrap_or("<none>")
    );
    println!("files_seen: {}", status.files_seen);
    println!("queued_events: {}", status.queued_events);
    println!(
        "last_indexed_path: {}",
        status.last_indexed_path.as_deref().unwrap_or("<none>")
    );
    println!(
        "last_error: {}",
        status.last_error.as_deref().unwrap_or("<none>")
    );
    println!(
        "active_layer: {}",
        status.active_layer.as_deref().unwrap_or("<none>")
    );
    println!(
        "quality_status: {}",
        status.quality_status.as_deref().unwrap_or("<none>")
    );
    println!("quality_pending_jobs: {}", status.quality_pending_jobs);
    println!("quality_running_jobs: {}", status.quality_running_jobs);
    println!("quality_failed_jobs: {}", status.quality_failed_jobs);
    println!("quality_stale_jobs: {}", status.quality_stale_jobs);
    println!("attached_clients: {}", status.attached_clients);
    println!(
        "client_kinds: {}",
        if status.client_kinds.is_empty() {
            "<none>".to_owned()
        } else {
            status.client_kinds.join(",")
        }
    );
    println!(
        "shutdown_after_seconds: {}",
        status
            .shutdown_after_seconds
            .map(|seconds| seconds.to_string())
            .unwrap_or_else(|| "<none>".to_owned())
    );
}

fn continuous_quality_summary(state: &symdex_index::ContinuousQualityState) -> String {
    format!(
        "generation_id={} active_layer={} quality_status={} activation_reason={} embeddable_chunks={} quality_eligible_chunks={} quality_ineligible_chunks={} quality_embedded_chunks={} pending_jobs={} running_jobs={} succeeded_jobs={} failed_jobs={} skipped_stale_jobs={} skipped_excluded_jobs={}",
        state.generation_id,
        state.active_layer,
        state.quality_status,
        state.activation_reason.as_deref().unwrap_or("<none>"),
        state.embeddable_chunks,
        state.quality_eligible_chunks,
        state.quality_ineligible_chunks,
        state.quality_embedded_chunks,
        state.pending_jobs,
        state.running_jobs,
        state.succeeded_jobs,
        state.failed_jobs,
        state.skipped_stale_jobs,
        state.skipped_excluded_jobs
    )
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

fn serve_mcp(args: &ServeMcpArgs) -> Result<(), String> {
    if let Some(repo) = &args.watch_repo {
        let attachment = symdex_watch::start_or_attach(repo, WatcherClientKind::Mcp)?;
        let status = attachment.status()?;
        eprintln!(
            "mcp_watch_attached repository_id={} state={} owner_kind={} attached_clients={}",
            status.repository_id, status.state, status.owner_kind, status.attached_clients
        );
        symdex_mcp::serve_stdio_with_attachment(attachment)
    } else {
        symdex_mcp::serve_stdio()
    }
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
        "symdex {}\n\nUSAGE:\n    symdex [--json|--output json] <command>\n\nCOMMANDS:\n    init                   Create local symdex state directories\n    doctor [repo]          Print local configuration, services, and index readiness diagnostics\n    index [--full|--incremental] [--offline] [--watch] <repo>  Index code with explicit full or incremental scope\n    index-quality <repo>  Process queued quality semantic embedding jobs\n    index-status <repo>    Show local SQLite index counts\n    semantic-status <repo>  Show active semantic layer and quality readiness\n    staleness <repo> [symbol]  Compare indexed evidence hashes with current files\n    vector-verify <repo> [--semantic-layer fast|quality|all]  Verify SQLite vector metadata against sqlite-vec rows
    qdrant-verify <repo> [--semantic-layer fast|quality|all]  Deprecated alias for vector-verify\n    vector-repair <repo> [--semantic-layer fast|quality|all]  Repair sqlite-vec orphaned, missing, and stale vector metadata
    qdrant-repair <repo> [--semantic-layer fast|quality|all]  Deprecated alias for vector-repair\n    symbol <repo> <query>  Find symbols in the local index\n    callers <repo> <symbol>  Show direct callers\n    callees <repo> <symbol>  Show direct callees\n    call-path <repo> <source> <target> [depth]  Trace bounded call paths\n    impact <repo> <symbol>  Show direct, transitive, and related-file impact evidence\n    context-pack <repo> <symbol> [--mode structural|unified]  Print compact JSON evidence for editing context\n    debug-context <repo> <runtime-input|file|->  Build debug context from runtime failure input\n    search <repo> <query>  Search indexed chunks by semantic similarity\n    watch start|status|stop <repo>  Manage the single background watcher for a repository\n    tui [repo]             Run the local terminal UI control panel\n    serve-mcp [--watch <repo>]  Run the MCP server, optionally attaching the repository watcher\n    help                   Print this help\n\nINDEX SCOPE:\n    --full reparses all eligible files. --incremental skips unchanged files by content hash.\n\nJSON OUTPUT:\n    --json is supported for semantic-status as plain command JSON. For index-status, search, symbol, callers, callees, call-path, impact, context-pack, and debug-context it prints the same symdex.mcp.evidence.v1 envelope used by MCP structuredContent.",
        env!("CARGO_PKG_VERSION")
    );
}

#[cfg(test)]
mod tests {
    use super::{
        ContextPackMode, OutputMode, VectorVerifySemanticLayer, WatchAction, parse_cli_invocation,
        parse_context_pack_args, parse_index_args, parse_serve_mcp_args,
        parse_vector_maintenance_args, parse_watch_args, require_text_output, semantic_status_json,
    };
    use symdex_core::{SemanticLayer, SemanticLayerStatus};
    use symdex_index::IndexScope;
    use symdex_query::{SemanticStatusLayerSummary, SemanticStatusSummary};
    use symdex_store::QualityGenerationProgress;

    #[test]
    fn cli_invocation_parses_leading_json_flag() {
        let invocation = parse_cli_invocation(vec![
            "--json".to_owned(),
            "symbol".to_owned(),
            ".".to_owned(),
            "run".to_owned(),
        ])
        .expect("json invocation should parse");

        assert_eq!(invocation.output, OutputMode::Json);
        assert_eq!(invocation.args, vec!["symbol", ".", "run"]);
    }

    #[test]
    fn cli_invocation_parses_output_json_flag() {
        let invocation = parse_cli_invocation(vec![
            "--output=json".to_owned(),
            "index-status".to_owned(),
            ".".to_owned(),
        ])
        .expect("output invocation should parse");

        assert_eq!(invocation.output, OutputMode::Json);
        assert_eq!(invocation.args, vec!["index-status", "."]);
    }

    #[test]
    fn cli_invocation_rejects_unknown_output_format() {
        let error = parse_cli_invocation(vec!["--output=xml".to_owned()])
            .expect_err("unknown output should fail");

        assert!(error.contains("unsupported output format"));
    }

    #[test]
    fn index_args_parse_explicit_full_scope() {
        let args = parse_index_args(&["--full".to_owned(), "repo".to_owned()])
            .expect("index args should parse");

        assert_eq!(args.repo, "repo");
        assert_eq!(args.scope, IndexScope::Full);
        assert!(!args.offline);
        assert!(!args.watch);
    }

    #[test]
    fn index_args_parse_offline_incremental_scope() {
        let args = parse_index_args(&[
            "--offline".to_owned(),
            "--incremental".to_owned(),
            "repo".to_owned(),
        ])
        .expect("index args should parse");

        assert_eq!(args.repo, "repo");
        assert_eq!(args.scope, IndexScope::Incremental);
        assert!(args.offline);
    }

    #[test]
    fn index_args_preserve_legacy_defaults() {
        let semantic = parse_index_args(&["repo".to_owned()]).expect("args should parse");
        let offline = parse_index_args(&["--offline".to_owned(), "repo".to_owned()])
            .expect("args should parse");
        let watch = parse_index_args(&["--watch".to_owned(), "repo".to_owned()])
            .expect("args should parse");

        assert_eq!(semantic.scope, IndexScope::Full);
        assert_eq!(offline.scope, IndexScope::Incremental);
        assert_eq!(watch.scope, IndexScope::Incremental);
    }

    #[test]
    fn index_args_reject_conflicting_scopes() {
        let error = parse_index_args(&["--full".to_owned(), "--incremental".to_owned()])
            .expect_err("conflicting scopes should fail");

        assert!(error.contains("mutually exclusive"));
    }

    #[test]
    fn index_args_reject_full_watch() {
        let error = parse_index_args(&["--watch".to_owned(), "--full".to_owned()])
            .expect_err("full watch should fail");

        assert!(error.contains("--watch uses incremental indexing"));
    }

    #[test]
    fn serve_mcp_args_parse_without_watch() {
        let args = parse_serve_mcp_args(&[]).expect("serve args should parse");

        assert_eq!(args.watch_repo, None);
    }

    #[test]
    fn serve_mcp_args_parse_watch_repo() {
        let args = parse_serve_mcp_args(&["--watch".to_owned(), "repo".to_owned()])
            .expect("serve args should parse");

        assert_eq!(args.watch_repo.as_deref(), Some("repo"));
    }

    #[test]
    fn serve_mcp_args_reject_missing_watch_repo() {
        let error =
            parse_serve_mcp_args(&["--watch".to_owned()]).expect_err("missing repo should fail");

        assert!(error.contains("--watch requires a repository path"));
    }

    #[test]
    fn serve_mcp_args_reject_offline_watch() {
        let error = parse_serve_mcp_args(&[
            "--watch".to_owned(),
            "repo".to_owned(),
            "--offline".to_owned(),
        ])
        .expect_err("offline serve watch should fail");

        assert!(error.contains("semantic indexing"));
    }

    #[test]
    fn watch_args_parse_start_status_and_stop() {
        let start =
            parse_watch_args(&["start".to_owned(), "repo".to_owned()]).expect("start should parse");
        let status = parse_watch_args(&["status".to_owned(), "repo".to_owned()])
            .expect("status should parse");
        let stop =
            parse_watch_args(&["stop".to_owned(), "repo".to_owned()]).expect("stop should parse");

        assert_eq!(start.action, WatchAction::Start);
        assert_eq!(status.action, WatchAction::Status);
        assert_eq!(stop.action, WatchAction::Stop);
        assert_eq!(start.repo, "repo");
    }

    #[test]
    fn watch_args_reject_unknown_action() {
        let error =
            parse_watch_args(&["restart".to_owned(), "repo".to_owned()]).expect_err("bad action");

        assert!(error.contains("unsupported watch action"));
    }

    #[test]
    fn context_pack_args_parse_unified_mode() {
        let args = parse_context_pack_args(&[
            "repo".to_owned(),
            "main".to_owned(),
            "--mode".to_owned(),
            "unified".to_owned(),
        ])
        .expect("context-pack args should parse");

        assert_eq!(args.repo, "repo");
        assert_eq!(args.query, "main");
        assert_eq!(args.mode, ContextPackMode::Unified);
    }

    #[test]
    fn context_pack_args_default_to_structural_mode() {
        let args = parse_context_pack_args(&["repo".to_owned(), "main".to_owned()])
            .expect("context-pack args should parse");

        assert_eq!(args.mode, ContextPackMode::Structural);
    }

    #[test]
    fn vector_maintenance_args_parse_semantic_layer_flag() {
        let args = parse_vector_maintenance_args(&[
            "repo".to_owned(),
            "--semantic-layer".to_owned(),
            "quality".to_owned(),
        ])
        .expect("vector args should parse");

        assert_eq!(args.repo, "repo");
        assert_eq!(args.semantic_layer, VectorVerifySemanticLayer::Quality);
    }

    #[test]
    fn vector_maintenance_args_parse_all_alias() {
        let args =
            parse_vector_maintenance_args(&["--semantic-layer=all".to_owned(), "repo".to_owned()])
                .expect("vector args should parse");

        assert_eq!(args.repo, "repo");
        assert_eq!(args.semantic_layer, VectorVerifySemanticLayer::All);
    }

    #[test]
    fn vector_maintenance_args_reject_unknown_option() {
        let error = parse_vector_maintenance_args(&["--bad".to_owned()])
            .expect_err("unknown vector maintenance option should fail");

        assert!(error.contains("unsupported vector maintenance option"));
    }

    #[test]
    fn index_quality_rejects_json_output() {
        let error = require_text_output("index-quality", OutputMode::Json)
            .expect_err("index-quality should be text-only");

        assert!(error.contains("index-quality does not support"));
    }

    #[test]
    fn semantic_status_json_is_metadata_only() {
        let status = sample_semantic_status();
        let value = semantic_status_json(&status);

        assert_eq!(value["repository_id"], "repo");
        assert_eq!(value["active_layer"], "fast");
        assert_eq!(value["quality_status"], "quality_pending");
        assert_eq!(
            value["fallback_reason"],
            "quality_manifest_incomplete_using_fast_layer"
        );
        assert_eq!(value["fast"]["embedding_model"], "fast-model");
        assert_eq!(value["quality_progress"]["pending_jobs"], 1);
        assert!(value.get("source_text").is_none());
    }

    fn sample_semantic_status() -> SemanticStatusSummary {
        SemanticStatusSummary {
            repository_id: "repo".to_owned(),
            generation_id: Some("generation-1".to_owned()),
            active_layer: SemanticLayer::Fast,
            quality_status: SemanticLayerStatus::QualityPending,
            fallback_reason: Some("quality_manifest_incomplete_using_fast_layer".to_owned()),
            fast: sample_semantic_status_layer(SemanticLayer::Fast, "fast-model", true),
            quality: sample_semantic_status_layer(SemanticLayer::Quality, "quality-model", false),
            quality_progress: Some(QualityGenerationProgress {
                repository_id: "repo".to_owned(),
                generation_id: "generation-1".to_owned(),
                embeddable_chunks: 1,
                quality_eligible_chunks: 1,
                quality_ineligible_chunks: 0,
                quality_embedded_chunks: 0,
                pending_jobs: 1,
                running_jobs: 0,
                succeeded_jobs: 0,
                failed_jobs: 0,
                skipped_stale_jobs: 0,
                skipped_excluded_jobs: 0,
            }),
            latest_quality_error: None,
            quality_enabled: true,
        }
    }

    fn sample_semantic_status_layer(
        semantic_layer: SemanticLayer,
        embedding_model: &str,
        is_complete: bool,
    ) -> SemanticStatusLayerSummary {
        SemanticStatusLayerSummary {
            semantic_layer,
            embedding_model: embedding_model.to_owned(),
            embedding_dimension: Some(768),
            vector_table: format!("symdex_repo_{embedding_model}"),
            current_chunks: usize::from(is_complete),
            stale_chunks: usize::from(!is_complete),
            blocked_chunks: 0,
            failed_chunks: 0,
            other_chunks: 0,
            total_chunks: 1,
            expected_chunks: 1,
            is_complete,
        }
    }
}

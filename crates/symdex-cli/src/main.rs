use std::env;
use std::fs;
use std::io::Read;

use serde_json::json;
use symdex_core::RepoRoot;
use symdex_diagnostics::{
    DiagnosticCheck, DiagnosticReport, DiagnosticState, run_diagnostics_for_repo,
};
use symdex_index::{
    ContinuousIndexEvent, ContinuousIndexOptions, EmbeddingSummary, IndexOptions, IndexSummary,
    QualityIndexOptions, QualityIndexSummary, RustAnalyzerEnrichmentSummary, WatchChangeSet,
    run_continuous_index, run_index, run_quality_index, run_quality_index_with_progress,
};
use symdex_query::{
    CallDirection, CallGraphSummary, CallPathSummary, ContextPackMode, FreshnessSummary,
    ImpactSummary, QdrantVerifyOptions, QdrantVerifySemanticLayer, QdrantVerifySummary,
    SemanticStatusLayerSummary, SemanticStatusSummary, run_call_graph, run_call_path,
    run_context_pack, run_debug_context_pack, run_freshness_report, run_impact,
    run_qdrant_verify_with_options, run_semantic_search, run_semantic_status, run_symbol_search,
    run_unified_context_pack,
};
use symdex_store::{EvidenceFreshness, QdrantClient, SqliteStore, StoreConfig, sqlite_parent};

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
            let index_args = parse_index_args(&args[1..]);
            index(&index_args)
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
        "qdrant-verify" => {
            require_text_output(command, output)?;
            let verify_args = parse_qdrant_maintenance_args(&args[1..])?;
            qdrant_verify(&verify_args)
        }
        "qdrant-repair" => {
            require_text_output(command, output)?;
            let repair_args = parse_qdrant_maintenance_args(&args[1..])?;
            qdrant_repair(&repair_args)
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
            serve_mcp()
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
    run_continuous_index(
        &ContinuousIndexOptions::new(args.repo.clone(), args.offline),
        print_continuous_index_event,
    )
}

fn index_status(repo: &str, output: OutputMode) -> Result<(), String> {
    if output == OutputMode::Json {
        return print_mcp_json_tool(symdex_mcp::TOOL_INDEX_STATUS, json!({ "repo": repo }));
    }
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
    println!("  qdrant_collection: {}", layer.qdrant_collection);
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
        "qdrant_collection": layer.qdrant_collection.as_str(),
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

fn qdrant_verify(args: &QdrantMaintenanceArgs) -> Result<(), String> {
    let summary = run_qdrant_verify_with_options(
        &args.repo,
        QdrantVerifyOptions {
            semantic_layer: args.semantic_layer,
        },
    )?;
    print_qdrant_verify_summary(&summary);
    Ok(())
}

fn qdrant_repair(args: &QdrantMaintenanceArgs) -> Result<(), String> {
    let options = QdrantVerifyOptions {
        semantic_layer: args.semantic_layer,
    };
    let before = run_qdrant_verify_with_options(&args.repo, options)?;
    println!("pre_repair_verify:");
    print_qdrant_verify_summary(&before);

    let orphaned_points_deleted = delete_orphaned_qdrant_points(&before)?;
    println!("orphaned_points_deleted: {orphaned_points_deleted}");

    repair_qdrant_summary(&args.repo, &before)?;

    let after = run_qdrant_verify_with_options(&args.repo, options)?;
    println!("post_repair_verify:");
    print_qdrant_verify_summary(&after);
    Ok(())
}

fn repair_qdrant_summary(repo: &str, summary: &QdrantVerifySummary) -> Result<(), String> {
    if !summary.layer_summaries.is_empty() {
        for layer_summary in &summary.layer_summaries {
            repair_qdrant_summary(repo, layer_summary)?;
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

    match QdrantVerifySemanticLayer::parse(&summary.semantic_layer)? {
        QdrantVerifySemanticLayer::Fast => {
            println!("semantic_repair layer=fast action=reindex_started");
            let index_summary = run_index(&IndexOptions {
                repo: repo.to_owned(),
                offline: false,
            })?;
            print_index_summary(&index_summary);
        }
        QdrantVerifySemanticLayer::Quality => {
            println!("semantic_repair layer=quality action=quality_worker_started");
            let quality_summary = run_quality_index(&QualityIndexOptions {
                repo: repo.to_owned(),
            })?;
            print_quality_index_summary(&quality_summary);
        }
        QdrantVerifySemanticLayer::All => {}
    }
    Ok(())
}

fn delete_orphaned_qdrant_points(summary: &QdrantVerifySummary) -> Result<usize, String> {
    if !summary.layer_summaries.is_empty() {
        return summary
            .layer_summaries
            .iter()
            .try_fold(0usize, |deleted, layer_summary| {
                Ok(deleted + delete_orphaned_qdrant_points(layer_summary)?)
            });
    }
    if !summary.collection_exists || summary.orphaned_point_ids.is_empty() {
        return Ok(0);
    }
    let store_config = StoreConfig::from_env();
    let qdrant = QdrantClient::new(&store_config).map_err(|error| error.to_string())?;
    qdrant
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

fn print_qdrant_verify_summary(summary: &QdrantVerifySummary) {
    print_qdrant_verify_summary_with_prefix(summary, "");
    for layer_summary in &summary.layer_summaries {
        println!("layer_summary: {}", layer_summary.semantic_layer);
        print_qdrant_verify_summary_with_prefix(layer_summary, "  ");
    }
}

fn print_qdrant_verify_summary_with_prefix(summary: &QdrantVerifySummary, prefix: &str) {
    println!("{prefix}repository_id: {}", summary.repository_id);
    println!("{prefix}semantic_layer: {}", summary.semantic_layer);
    println!("{prefix}collection: {}", summary.collection_name);
    println!("{prefix}embedding_model: {}", summary.embedding_model);
    println!("{prefix}collection_exists: {}", summary.collection_exists);
    println!(
        "{prefix}expected_vector_points: {}",
        summary.expected_vector_points
    );
    println!(
        "{prefix}qdrant_payload_points: {}",
        summary.qdrant_payload_points
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
    println!("qdrant_collection: {}", summary.qdrant_collection);
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
            println!("rust_analyzer_enrichment: disabled (set {enable_env}=1)");
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
    println!("qdrant_collection: {}", summary.qdrant_collection);
    println!("claimed_jobs: {}", summary.claimed_jobs);
    println!("succeeded_jobs: {}", summary.succeeded_jobs);
    println!("failed_jobs: {}", summary.failed_jobs);
    println!("skipped_stale_jobs: {}", summary.skipped_stale_jobs);
    println!("remaining_pending_jobs: {}", summary.remaining_pending_jobs);
    println!(
        "progress: embeddable_chunks={} quality_embedded_chunks={} pending={} running={} succeeded={} failed={} skipped_stale={} skipped_excluded={}",
        summary.progress.embeddable_chunks,
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

fn parse_qdrant_maintenance_args(args: &[String]) -> Result<QdrantMaintenanceArgs, String> {
    let mut positional = Vec::new();
    let mut semantic_layer = QdrantVerifySemanticLayer::Fast;
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        if arg == "--semantic-layer" {
            index += 1;
            let Some(value) = args.get(index) else {
                return Err("--semantic-layer requires a value".to_owned());
            };
            semantic_layer = QdrantVerifySemanticLayer::parse(value)?;
        } else if let Some(value) = arg.strip_prefix("--semantic-layer=") {
            semantic_layer = QdrantVerifySemanticLayer::parse(value)?;
        } else if arg == "--all-semantic-layers" {
            semantic_layer = QdrantVerifySemanticLayer::All;
        } else if arg.starts_with("--") {
            return Err(format!("unsupported qdrant maintenance option `{arg}`"));
        } else {
            positional.push(arg.clone());
        }
        index += 1;
    }

    Ok(QdrantMaintenanceArgs {
        repo: positional
            .first()
            .cloned()
            .unwrap_or_else(|| ".".to_owned()),
        semantic_layer,
    })
}

struct IndexArgs {
    repo: String,
    offline: bool,
    watch: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct QdrantMaintenanceArgs {
    repo: String,
    semantic_layer: QdrantVerifySemanticLayer,
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

fn continuous_quality_summary(state: &symdex_index::ContinuousQualityState) -> String {
    format!(
        "generation_id={} active_layer={} quality_status={} activation_reason={} embeddable_chunks={} quality_embedded_chunks={} pending_jobs={} running_jobs={} succeeded_jobs={} failed_jobs={} skipped_stale_jobs={} skipped_excluded_jobs={}",
        state.generation_id,
        state.active_layer,
        state.quality_status,
        state.activation_reason.as_deref().unwrap_or("<none>"),
        state.embeddable_chunks,
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
        "symdex {}\n\nUSAGE:\n    symdex [--json|--output json] <command>\n\nCOMMANDS:\n    init                   Create local symdex state directories\n    doctor [repo]          Print local configuration, services, and index readiness diagnostics\n    index [--offline] [--watch] <repo>  Index Rust chunks and upsert semantic vectors\n    index-quality <repo>  Process queued quality semantic embedding jobs\n    index-status <repo>    Show local SQLite index counts\n    semantic-status <repo>  Show active semantic layer and quality readiness\n    staleness <repo> [symbol]  Compare indexed evidence hashes with current files\n    qdrant-verify <repo> [--semantic-layer fast|quality|all]  Verify SQLite vector metadata against Qdrant payloads\n    qdrant-repair <repo> [--semantic-layer fast|quality|all]  Repair Qdrant orphaned, missing, and stale vector metadata\n    symbol <repo> <query>  Find symbols in the local index\n    callers <repo> <symbol>  Show direct callers\n    callees <repo> <symbol>  Show direct callees\n    call-path <repo> <source> <target> [depth]  Trace bounded call paths\n    impact <repo> <symbol>  Show direct, transitive, and related-file impact evidence\n    context-pack <repo> <symbol> [--mode structural|unified]  Print compact JSON evidence for editing context\n    debug-context <repo> <runtime-input|file|->  Build debug context from runtime failure input\n    search <repo> <query>  Search indexed chunks by semantic similarity\n    tui [repo]             Run the local terminal UI control panel\n    serve-mcp              Run the read-only MCP server over stdio\n    help                   Print this help\n\nJSON OUTPUT:\n    --json is supported for semantic-status as plain command JSON. For index-status, search, symbol, callers, callees, call-path, impact, context-pack, and debug-context it prints the same symdex.mcp.evidence.v1 envelope used by MCP structuredContent.",
        env!("CARGO_PKG_VERSION")
    );
}

#[cfg(test)]
mod tests {
    use super::{
        ContextPackMode, OutputMode, QdrantVerifySemanticLayer, parse_cli_invocation,
        parse_context_pack_args, parse_qdrant_maintenance_args, require_text_output,
        semantic_status_json,
    };
    use symdex_core::{SemanticLayer, SemanticLayerStatus};
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
    fn qdrant_maintenance_args_parse_semantic_layer_flag() {
        let args = parse_qdrant_maintenance_args(&[
            "repo".to_owned(),
            "--semantic-layer".to_owned(),
            "quality".to_owned(),
        ])
        .expect("qdrant args should parse");

        assert_eq!(args.repo, "repo");
        assert_eq!(args.semantic_layer, QdrantVerifySemanticLayer::Quality);
    }

    #[test]
    fn qdrant_maintenance_args_parse_all_alias() {
        let args =
            parse_qdrant_maintenance_args(&["--semantic-layer=all".to_owned(), "repo".to_owned()])
                .expect("qdrant args should parse");

        assert_eq!(args.repo, "repo");
        assert_eq!(args.semantic_layer, QdrantVerifySemanticLayer::All);
    }

    #[test]
    fn qdrant_maintenance_args_reject_unknown_option() {
        let error = parse_qdrant_maintenance_args(&["--bad".to_owned()])
            .expect_err("unknown qdrant option should fail");

        assert!(error.contains("unsupported qdrant maintenance option"));
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
            qdrant_collection: format!("symdex_repo_{embedding_model}"),
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

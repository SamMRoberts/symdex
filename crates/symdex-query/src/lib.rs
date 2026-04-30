//! Query orchestration shared by the CLI and TUI.

use symdex_core::RepoRoot;
use symdex_embed::{EmbedConfig, OllamaClient};
use symdex_store::{
    CallSearchRow, ContextPack, QdrantClient, SqliteStore, StorageExplorerSummary, StoreConfig,
    SymbolSearchRow, qdrant_collection_name,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryMode {
    Semantic,
    Symbol,
}

impl QueryMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Semantic => "semantic",
            Self::Symbol => "symbol",
        }
    }

    pub fn toggled(self) -> Self {
        match self {
            Self::Semantic => Self::Symbol,
            Self::Symbol => Self::Semantic,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallDirection {
    Callers,
    Callees,
}

impl CallDirection {
    pub fn label(self) -> &'static str {
        match self {
            Self::Callers => "callers",
            Self::Callees => "callees",
        }
    }

    pub fn toggled(self) -> Self {
        match self {
            Self::Callers => Self::Callees,
            Self::Callees => Self::Callers,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum QueryResult {
    Semantic(SemanticSearchSummary),
    Symbol(SymbolSearchSummary),
}

#[derive(Debug, Clone, PartialEq)]
pub struct SymbolSearchSummary {
    pub repository_id: String,
    pub query: String,
    pub symbols: Vec<SymbolSearchRow>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SemanticSearchSummary {
    pub repository_id: String,
    pub qdrant_collection: String,
    pub query: String,
    pub results: Vec<SemanticSearchResult>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SemanticSearchResult {
    pub score: f64,
    pub path: String,
    pub start_line: usize,
    pub end_line: usize,
    pub symbol_name: Option<String>,
    pub chunk_kind: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CallGraphSummary {
    pub repository_id: String,
    pub query: String,
    pub direction: CallDirection,
    pub rows: Vec<CallSearchRow>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ImpactSummary {
    pub repository_id: String,
    pub query: String,
    pub direct_callers: Vec<CallSearchRow>,
    pub direct_callees: Vec<CallSearchRow>,
}

pub fn run_symbol_search(repo: &str, query: &str) -> Result<SymbolSearchSummary, String> {
    let query = query.trim();
    if query.is_empty() {
        return Err("symbol search requires a query".to_owned());
    }
    let root = RepoRoot::open(repo).map_err(|error| error.to_string())?;
    let sqlite = sqlite_for_read()?;
    let symbols = sqlite
        .find_symbols(root.id(), query)
        .map_err(|error| error.to_string())?;
    Ok(SymbolSearchSummary {
        repository_id: root.id().to_owned(),
        query: query.to_owned(),
        symbols,
    })
}

pub fn run_call_graph(
    repo: &str,
    query: &str,
    direction: CallDirection,
) -> Result<CallGraphSummary, String> {
    let query = query.trim();
    if query.is_empty() {
        return Err("call graph requires a symbol query".to_owned());
    }
    let root = RepoRoot::open(repo).map_err(|error| error.to_string())?;
    let sqlite = sqlite_for_read()?;
    let rows = match direction {
        CallDirection::Callers => sqlite.callers(root.id(), query),
        CallDirection::Callees => sqlite.callees(root.id(), query),
    }
    .map_err(|error| error.to_string())?;

    Ok(CallGraphSummary {
        repository_id: root.id().to_owned(),
        query: query.to_owned(),
        direction,
        rows,
    })
}

pub fn run_impact(repo: &str, query: &str) -> Result<ImpactSummary, String> {
    let query = query.trim();
    if query.is_empty() {
        return Err("impact requires a symbol query".to_owned());
    }
    let root = RepoRoot::open(repo).map_err(|error| error.to_string())?;
    let sqlite = sqlite_for_read()?;
    let direct_callers = sqlite
        .callers(root.id(), query)
        .map_err(|error| error.to_string())?;
    let direct_callees = sqlite
        .callees(root.id(), query)
        .map_err(|error| error.to_string())?;

    Ok(ImpactSummary {
        repository_id: root.id().to_owned(),
        query: query.to_owned(),
        direct_callers,
        direct_callees,
    })
}

pub fn run_context_pack(repo: &str, query: &str, limit: usize) -> Result<ContextPack, String> {
    let query = query.trim();
    if query.is_empty() {
        return Err("context-pack requires a symbol query".to_owned());
    }
    let root = RepoRoot::open(repo).map_err(|error| error.to_string())?;
    let sqlite = sqlite_for_read()?;
    sqlite
        .context_pack(root.id(), query, limit)
        .map_err(|error| error.to_string())
}

pub fn run_storage_explorer(repo: &str) -> Result<StorageExplorerSummary, String> {
    let root = RepoRoot::open(repo).map_err(|error| error.to_string())?;
    let sqlite = sqlite_for_read()?;
    let embed_config = EmbedConfig::from_env();
    sqlite
        .storage_explorer_summary(root.id(), &embed_config.model)
        .map_err(|error| error.to_string())
}

pub fn run_semantic_search(
    repo: &str,
    query: &str,
    limit: usize,
) -> Result<SemanticSearchSummary, String> {
    let query = query.trim();
    if query.is_empty() {
        return Err("semantic search requires a query".to_owned());
    }

    let root = RepoRoot::open(repo).map_err(|error| error.to_string())?;
    let embed_config = EmbedConfig::from_env();
    let embed_client =
        OllamaClient::new(embed_config.clone()).map_err(|error| error.to_string())?;
    let query_embedding = embed_client
        .embed_batch(&[query.to_owned()])
        .map_err(|error| error.to_string())?;
    let Some(vector) = query_embedding.embeddings.into_iter().next() else {
        return Err("embedding query returned no vector".to_owned());
    };

    let store_config = StoreConfig::from_env();
    let qdrant = QdrantClient::new(&store_config).map_err(|error| error.to_string())?;
    let qdrant_collection = qdrant_collection_name(root.id(), &embed_config.model);
    let results = qdrant
        .query_points(&qdrant_collection, vector, limit)
        .map_err(|error| error.to_string())?
        .into_iter()
        .map(|point| SemanticSearchResult {
            score: point.score,
            path: point.payload.path,
            start_line: point.payload.start_line,
            end_line: point.payload.end_line,
            symbol_name: point.payload.symbol_name,
            chunk_kind: point.payload.chunk_kind,
        })
        .collect();

    Ok(SemanticSearchSummary {
        repository_id: root.id().to_owned(),
        qdrant_collection,
        query: query.to_owned(),
        results,
    })
}

fn sqlite_for_read() -> Result<SqliteStore, String> {
    let store_config = StoreConfig::from_env();
    let sqlite = SqliteStore::open(&store_config).map_err(|error| error.to_string())?;
    sqlite.migrate().map_err(|error| error.to_string())?;
    Ok(sqlite)
}

#[cfg(test)]
mod tests {
    use crate::{
        CallDirection, QueryMode, run_call_graph, run_context_pack, run_impact,
        run_semantic_search, run_symbol_search,
    };

    #[test]
    fn query_mode_toggles_between_workbench_modes() {
        assert_eq!(QueryMode::Semantic.toggled(), QueryMode::Symbol);
        assert_eq!(QueryMode::Symbol.toggled(), QueryMode::Semantic);
    }

    #[test]
    fn call_direction_toggles_between_graph_modes() {
        assert_eq!(CallDirection::Callers.toggled(), CallDirection::Callees);
        assert_eq!(CallDirection::Callees.toggled(), CallDirection::Callers);
    }

    #[test]
    fn symbol_search_rejects_empty_query() {
        let error = run_symbol_search(".", " ").expect_err("empty query should fail");
        assert!(error.contains("requires a query"));
    }

    #[test]
    fn semantic_search_rejects_empty_query_before_service_calls() {
        let error = run_semantic_search(".", " ", 10).expect_err("empty query should fail");
        assert!(error.contains("requires a query"));
    }

    #[test]
    fn call_graph_rejects_empty_query() {
        let error =
            run_call_graph(".", " ", CallDirection::Callers).expect_err("empty query should fail");
        assert!(error.contains("requires a symbol query"));
    }

    #[test]
    fn impact_rejects_empty_query() {
        let error = run_impact(".", " ").expect_err("empty query should fail");
        assert!(error.contains("requires a symbol query"));
    }

    #[test]
    fn context_pack_rejects_empty_query() {
        let error = run_context_pack(".", " ", 8).expect_err("empty query should fail");
        assert!(error.contains("requires a symbol query"));
    }
}

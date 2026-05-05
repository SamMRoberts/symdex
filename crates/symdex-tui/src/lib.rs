//! Terminal UI state, rendering, events, and terminal lifecycle.

mod navigation;
mod terminal;

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyCode, KeyEventKind};
pub(crate) use navigation::{IndexMode, ManualIndexRequest, Screen, UiAction, reduce_screen};
use ratatui::Terminal;
use ratatui::backend::Backend;
use ratatui::layout::{Constraint, Direction, Layout, Margin, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    BarChart, Block, Borders, Gauge, List, ListItem, Paragraph, Scrollbar, ScrollbarOrientation,
    ScrollbarState, Tabs, Wrap,
};
use ratatui::widgets::{Cell, Row, Table, TableState};
use symdex_core::{RepoRoot, SemanticLayer, SemanticLayerStatus};
use symdex_diagnostics::{
    DiagnosticCheck, DiagnosticReport, DiagnosticState, run_diagnostics_for_repo,
};
use symdex_embed::{EmbedConfig, LayeredEmbedConfig};
use symdex_index::{
    ContinuousIndexEvent, ContinuousQualityState, EmbeddingSummary, IndexProgress, IndexScope,
    IndexSummary, RustAnalyzerEnrichmentSummary,
};
use symdex_query::{
    CallDirection, CallGraphSummary, CallPathSummary, DebugContextPack, FreshnessSummary,
    ImpactSummary, QueryMode, QueryResult, SemanticSearchSummary, SemanticStatusLayerSummary,
    SemanticStatusSummary, SymbolSearchSummary, run_call_graph, run_call_path, run_call_resolution,
    run_context_pack, run_cross_store_health, run_debug_context_pack, run_embedding_coverage,
    run_freshness_report, run_impact, run_index_coverage, run_index_runs_timeline,
    run_semantic_neighborhood, run_semantic_search, run_semantic_status, run_storage_explorer,
    run_symbol_outline, run_symbol_search,
};
use symdex_store::{
    CallResolutionSummary, ChunkVectorStatus, ConfidenceBucket, ContextPack,
    CrossStoreHealthSummary, EmbeddingCoverageSummary, EvidenceFreshness, EvidenceProvenance,
    FileCoverageStatus, FileDetailSummary, IndexCoverageSummary, IndexRunTimelineRow,
    IndexRunsTimelineSummary, QualityGenerationProgress, RepositoryStatus, SemanticNeighborhoodRow,
    SemanticNeighborhoodSummary, SqliteStorageSummary, SqliteStore, StorageExplorerSummary,
    StorageHealthRow, StorageHealthStatus, StoreConfig, SymbolOutlineSummary,
    VectorStorageProjection, vector_table_name,
};
use symdex_watch::{WatcherAttachment, WatcherClientKind, WatcherStatus};
use symdex_writer::{WriterClient, WriterIndexScope, WriterJob, WriterJobResponse, WriterProgress};
pub use terminal::help_text;
use terminal::{enter_terminal, leave_terminal};

const EVENT_POLL_INTERVAL: Duration = Duration::from_millis(250);
const WATCHER_STATUS_REFRESH_INTERVAL: Duration = Duration::from_secs(1);
const INDEX_STATUS_REFRESH_INTERVAL: Duration = Duration::from_secs(5);

pub struct TuiOptions {
    pub repo: String,
}

pub fn run(options: TuiOptions) -> Result<(), String> {
    let mut app = App::load(&options.repo)?;
    let mut terminal = enter_terminal()?;
    app.start_continuous_index();
    let result = run_app(&mut terminal, app);
    leave_terminal(&mut terminal)?;
    result
}

pub struct App {
    repo_input: String,
    repo_root: String,
    repository_id: String,
    sqlite_path: String,
    vector_store_label: String,
    ollama_url: String,
    embed_model: String,
    status: RepositoryStatus,
    semantic_status: SemanticStatusSummary,
    message: String,
    view: View,
    screen: Screen,
    index_scope: IndexScope,
    last_index_summary: Option<IndexSummary>,
    index_progress: Option<IndexProgress>,
    animation_tick: usize,
    continuous: ContinuousIndexState,
    watcher_attachment: Option<WatcherAttachment>,
    diagnostics: DiagnosticsState,
    diagnostics_selection: usize,
    diagnostics_details_expanded: bool,
    storage: StorageExplorerState,
    query: QueryWorkbenchState,
    graph: GraphBrowserState,
    evidence: EvidenceViewerState,
    last_error: Option<String>,
    last_watcher_status_refresh: Option<Instant>,
    last_index_status_refresh: Option<Instant>,
    index_receiver: Option<Receiver<IndexJobMessage>>,
    continuous_receiver: Option<Receiver<ContinuousIndexMessage>>,
    continuous_stop: Option<Arc<AtomicBool>>,
    diagnostics_receiver: Option<Receiver<Result<DiagnosticReport, String>>>,
    query_receiver: Option<Receiver<Result<QueryResult, String>>>,
    graph_receiver: Option<Receiver<Result<CallGraphSummary, String>>>,
    evidence_receiver: Option<Receiver<Result<EvidenceResult, String>>>,
    status_refresh_receiver: Option<Receiver<StatusRefreshMessage>>,
}

impl App {
    pub fn load(repo: &str) -> Result<Self, String> {
        let root = RepoRoot::open(repo).map_err(|error| error.to_string())?;
        let store_config = StoreConfig::from_env();
        let embed_config = EmbedConfig::from_env();
        let sqlite =
            SqliteStore::open_read_only(&store_config).map_err(|error| error.to_string())?;
        let status = sqlite
            .repository_status(root.id())
            .map_err(|error| error.to_string())?;
        let storage = sqlite
            .storage_explorer_summary(root.id(), &embed_config.model)
            .map_err(|error| error.to_string())?;
        let coverage = sqlite
            .index_coverage_summary(root.id())
            .map_err(|error| error.to_string())?;
        let outline = sqlite
            .symbol_outline_summary(root.id())
            .map_err(|error| error.to_string())?;
        let call_resolution = sqlite
            .call_resolution_summary(root.id())
            .map_err(|error| error.to_string())?;
        let embedding_coverage = sqlite
            .embedding_coverage_summary(root.id(), &embed_config.model)
            .map_err(|error| error.to_string())?;
        let index_runs = sqlite
            .index_runs_timeline_summary(root.id())
            .map_err(|error| error.to_string())?;
        let freshness = run_freshness_report(repo, None)?;
        let semantic_neighborhood = sqlite
            .semantic_neighborhood_summary(root.id(), &embed_config.model)
            .map_err(|error| error.to_string())?;
        let cross_store_health = sqlite
            .cross_store_health_summary(root.id(), &embed_config.model)
            .map_err(|error| error.to_string())?;
        let semantic_status = run_semantic_status(repo)?;
        let now = Instant::now();

        Ok(Self {
            repo_input: repo.to_owned(),
            repo_root: root.path().display().to_string(),
            repository_id: root.id().to_owned(),
            sqlite_path: store_config.sqlite_path.display().to_string(),
            vector_store_label: "sqlite_vec".to_owned(),
            ollama_url: embed_config.ollama_url,
            embed_model: embed_config.model,
            status,
            semantic_status,
            message: "Overview loaded. Press q or Esc to quit.".to_owned(),
            view: View::Overview,
            screen: Screen::Dashboard,
            index_scope: IndexScope::Incremental,
            last_index_summary: None,
            index_progress: None,
            animation_tick: 0,
            continuous: ContinuousIndexState::default(),
            watcher_attachment: None,
            diagnostics: DiagnosticsState::Idle,
            diagnostics_selection: 0,
            diagnostics_details_expanded: false,
            storage: StorageExplorerState::completed(
                storage,
                coverage,
                outline,
                call_resolution,
                embedding_coverage,
                index_runs,
                freshness,
                semantic_neighborhood,
                cross_store_health,
            ),
            query: QueryWorkbenchState::default(),
            graph: GraphBrowserState::default(),
            evidence: EvidenceViewerState::default(),
            last_error: None,
            last_watcher_status_refresh: Some(now),
            last_index_status_refresh: Some(now),
            index_receiver: None,
            continuous_receiver: None,
            continuous_stop: None,
            diagnostics_receiver: None,
            query_receiver: None,
            graph_receiver: None,
            evidence_receiver: None,
            status_refresh_receiver: None,
        })
    }

    pub fn from_status(
        repo_root: impl Into<String>,
        repository_id: impl Into<String>,
        status: RepositoryStatus,
    ) -> Self {
        let repo_root = repo_root.into();
        let repository_id = repository_id.into();
        let storage = storage_summary_from_status(&repository_id, &status);
        let coverage = coverage_summary_from_status(&repository_id, &status);
        let outline = symbol_outline_summary_from_status(&repository_id);
        let call_resolution = call_resolution_summary_from_status(&repository_id);
        let embedding_coverage = embedding_coverage_summary_from_status(&repository_id, &status);
        let index_runs = index_runs_timeline_summary_from_status(&repository_id, &status);
        let freshness = freshness_summary_from_status(&repository_id);
        let semantic_neighborhood =
            semantic_neighborhood_summary_from_status(&repository_id, &status);
        let cross_store_health = cross_store_health_summary_from_status(&repository_id, &status);
        let semantic_status = semantic_status_summary_from_status(&repository_id, &status);
        let now = Instant::now();
        Self {
            repo_input: repo_root.clone(),
            repo_root,
            repository_id,
            sqlite_path: ".symdex/symdex.sqlite".to_owned(),
            vector_store_label: "sqlite_vec".to_owned(),
            ollama_url: "http://localhost:11434".to_owned(),
            embed_model: "nomic-embed-text".to_owned(),
            status,
            semantic_status,
            message: "Overview loaded. Press q or Esc to quit.".to_owned(),
            view: View::Overview,
            screen: Screen::Dashboard,
            index_scope: IndexScope::Incremental,
            last_index_summary: None,
            index_progress: None,
            animation_tick: 0,
            continuous: ContinuousIndexState::default(),
            watcher_attachment: None,
            diagnostics: DiagnosticsState::Idle,
            diagnostics_selection: 0,
            diagnostics_details_expanded: false,
            storage: StorageExplorerState::completed(
                storage,
                coverage,
                outline,
                call_resolution,
                embedding_coverage,
                index_runs,
                freshness,
                semantic_neighborhood,
                cross_store_health,
            ),
            query: QueryWorkbenchState::default(),
            graph: GraphBrowserState::default(),
            evidence: EvidenceViewerState::default(),
            last_error: None,
            last_watcher_status_refresh: Some(now),
            last_index_status_refresh: Some(now),
            index_receiver: None,
            continuous_receiver: None,
            continuous_stop: None,
            diagnostics_receiver: None,
            query_receiver: None,
            graph_receiver: None,
            evidence_receiver: None,
            status_refresh_receiver: None,
        }
    }

    fn index_lines(&self) -> Vec<Line<'_>> {
        let mut lines = vec![
            Line::from(vec![
                Span::styled("Scope: ", Style::new().add_modifier(Modifier::BOLD)),
                status_span(self.index_scope.label(), StatusTone::Info),
                Span::raw(" press Tab"),
            ]),
            Line::from(vec![
                Span::styled("Offline index: ", Style::new().add_modifier(Modifier::BOLD)),
                Span::raw("press o"),
            ]),
            Line::from(vec![
                Span::styled(
                    "Semantic index: ",
                    Style::new().add_modifier(Modifier::BOLD),
                ),
                Span::raw("press s"),
            ]),
            Line::from({
                let mut spans = vec![Span::styled(
                    "Continuous index: ",
                    Style::new().add_modifier(Modifier::BOLD),
                )];
                if let Some(indicator) = self.continuous_activity_span() {
                    spans.push(indicator);
                    spans.push(Span::raw(" "));
                }
                spans.push(status_span(
                    self.continuous.status_label(),
                    self.continuous.status_tone(),
                ));
                spans.push(Span::raw(" press c"));
                spans
            }),
            Line::from(vec![
                Span::styled(
                    "Refresh status: ",
                    Style::new().add_modifier(Modifier::BOLD),
                ),
                Span::raw("press r"),
            ]),
            Line::from(vec![
                Span::styled("Watch: ", Style::new().add_modifier(Modifier::BOLD)),
                Span::raw(self.continuous.summary()),
            ]),
            Line::from({
                let mut spans = vec![Span::styled(
                    "Watch error: ",
                    Style::new().add_modifier(Modifier::BOLD),
                )];
                if let Some(error) = &self.continuous.latest_error {
                    spans.push(status_span("error", StatusTone::Error));
                    spans.push(Span::raw(" "));
                    spans.push(Span::raw(error.as_str()));
                } else {
                    spans.push(status_span("none", StatusTone::Dim));
                }
                spans
            }),
            Line::from(""),
        ];

        match self.screen {
            Screen::Dashboard => {
                lines.push(Line::from(vec![
                    status_span("idle", StatusTone::Dim),
                    Span::raw(" No indexing job is pending."),
                ]));
            }
            Screen::ConfirmIndex(request) => {
                lines.push(Line::from(vec![
                    status_span("confirm", StatusTone::Warning),
                    Span::raw(" "),
                    Span::raw(format!(
                        "Run {} indexing for this repository?",
                        request.label()
                    )),
                ]));
                lines.push(Line::from("Press y to start, n or Esc to cancel."));
            }
            Screen::ConfirmContinuous => {
                lines.push(Line::from(vec![
                    status_span("confirm", StatusTone::Warning),
                    Span::raw(" Enable continuous semantic indexing?"),
                ]));
                lines.push(Line::from(
                    "The TUI will watch changed indexable files until toggled off.",
                ));
                lines.push(Line::from("Press y to start, n or Esc to cancel."));
            }
            Screen::IndexRunning(request) => {
                lines.push(Line::from(vec![
                    status_span("running", StatusTone::Info),
                    Span::raw(" "),
                    Span::raw(format!("{} indexing", request.label())),
                ]));
                lines.push(Line::from("The TUI will update when the job finishes."));
            }
            Screen::IndexCompleted(request) => {
                lines.push(Line::from(vec![
                    status_span("complete", StatusTone::Success),
                    Span::raw(" "),
                    Span::raw(format!("{} indexing", request.label())),
                ]));
                if let Some(summary) = &self.last_index_summary {
                    lines.extend(summary_lines(summary));
                }
            }
            Screen::IndexFailed(request) => {
                lines.push(Line::from(vec![
                    status_span("failed", StatusTone::Error),
                    Span::raw(" "),
                    Span::raw(format!("{} indexing", request.label())),
                ]));
                lines.push(Line::from(
                    self.last_error
                        .as_deref()
                        .unwrap_or("unknown indexing error"),
                ));
            }
        }

        lines
    }

    fn diagnostics_lines(&self) -> Vec<Line<'_>> {
        let mut lines = vec![
            Line::from(vec![
                Span::styled("Run doctor: ", Style::new().add_modifier(Modifier::BOLD)),
                Span::raw("press Enter"),
            ]),
            Line::from(vec![
                Span::styled("Primary tabs: ", Style::new().add_modifier(Modifier::BOLD)),
                Span::raw("press [ or ]"),
            ]),
            Line::from(""),
        ];

        match &self.diagnostics {
            DiagnosticsState::Idle => {
                lines.push(Line::from(vec![
                    status_span("idle", StatusTone::Dim),
                    Span::raw(" Diagnostics have not run in this TUI session."),
                ]));
            }
            DiagnosticsState::Running => {
                lines.push(Line::from(vec![
                    status_span("running", StatusTone::Info),
                    Span::raw(" local diagnostics..."),
                ]));
            }
            DiagnosticsState::Completed(report) => {
                lines.extend(diagnostic_report_lines(report));
            }
            DiagnosticsState::Failed(error) => {
                lines.push(Line::from(vec![
                    status_span("failed", StatusTone::Error),
                    Span::raw(" "),
                    Span::raw(error.as_str()),
                ]));
            }
        }

        lines
    }

    fn query_lines(&self) -> Vec<Line<'_>> {
        let mut lines = vec![
            Line::from(vec![
                Span::styled("Mode: ", Style::new().add_modifier(Modifier::BOLD)),
                Span::raw(format!("{} (Tab toggles)", self.query.mode.label())),
            ]),
            Line::from(vec![
                Span::styled("Query: ", Style::new().add_modifier(Modifier::BOLD)),
                Span::raw(if self.query.input.is_empty() {
                    "<type to search>".to_owned()
                } else {
                    self.query.input.clone()
                }),
            ]),
            Line::from(
                "Enter runs the query. Backspace edits. Esc clears input or leaves the workbench.",
            ),
            Line::from(""),
        ];

        match &self.query.status {
            QueryStatus::Idle => {
                lines.push(Line::from(vec![
                    status_span("idle", StatusTone::Dim),
                    Span::raw(" No query has run in this TUI session."),
                ]));
            }
            QueryStatus::Running => {
                lines.push(Line::from(vec![
                    status_span("running", StatusTone::Info),
                    Span::raw(format!(" {} query...", self.query.mode.label())),
                ]));
            }
            QueryStatus::Completed(result) => {
                lines.extend(query_result_lines(result));
            }
            QueryStatus::Failed(error) => {
                lines.push(Line::from(vec![
                    status_span("failed", StatusTone::Error),
                    Span::raw(" "),
                    Span::raw(error.as_str()),
                ]));
            }
        }

        lines
    }

    fn graph_lines(&self) -> Vec<Line<'_>> {
        let mut lines = vec![
            Line::from(vec![
                Span::styled("Mode: ", Style::new().add_modifier(Modifier::BOLD)),
                Span::raw(format!("{} (Tab toggles)", self.graph.direction.label())),
            ]),
            Line::from(vec![
                Span::styled("Symbol: ", Style::new().add_modifier(Modifier::BOLD)),
                Span::raw(if self.graph.input.is_empty() {
                    "<type symbol name>".to_owned()
                } else {
                    self.graph.input.clone()
                }),
            ]),
            Line::from("Enter runs the lookup. Backspace edits. Esc clears input or leaves graph."),
            Line::from(""),
        ];

        match &self.graph.status {
            GraphStatus::Idle => {
                lines.push(Line::from(vec![
                    status_span("idle", StatusTone::Dim),
                    Span::raw(" No graph lookup has run in this TUI session."),
                ]));
            }
            GraphStatus::Running => {
                lines.push(Line::from(vec![
                    status_span("running", StatusTone::Info),
                    Span::raw(format!(
                        " loading {} for {}...",
                        self.graph.direction.label(),
                        self.graph.input
                    )),
                ]));
            }
            GraphStatus::Completed(summary) => {
                lines.extend(call_graph_lines(summary));
            }
            GraphStatus::Failed(error) => {
                lines.push(Line::from(vec![
                    status_span("failed", StatusTone::Error),
                    Span::raw(" "),
                    Span::raw(error.as_str()),
                ]));
            }
        }

        lines
    }

    fn evidence_lines(&self) -> Vec<Line<'_>> {
        let mut lines = vec![
            Line::from(vec![
                Span::styled("Mode: ", Style::new().add_modifier(Modifier::BOLD)),
                Span::raw(format!("{} (Tab toggles)", self.evidence.mode.label())),
            ]),
            Line::from(vec![
                Span::styled("Input: ", Style::new().add_modifier(Modifier::BOLD)),
                Span::raw(if self.evidence.input.is_empty() {
                    "<symbol, source -> target, or runtime failure>".to_owned()
                } else {
                    self.evidence.input.clone()
                }),
            ]),
            Line::from(
                "Enter runs the lookup. Backspace edits. Esc clears input or leaves viewer.",
            ),
            Line::from(""),
        ];

        match &self.evidence.status {
            EvidenceStatus::Idle => {
                lines.push(Line::from(vec![
                    status_span("idle", StatusTone::Dim),
                    Span::raw(
                        " No impact, call-path, context-pack, or debug-context lookup has run.",
                    ),
                ]));
            }
            EvidenceStatus::Running => {
                lines.push(Line::from(vec![
                    status_span("running", StatusTone::Info),
                    Span::raw(format!(
                        " loading {} for {}...",
                        self.evidence.mode.label(),
                        self.evidence.input
                    )),
                ]));
            }
            EvidenceStatus::Completed(result) => {
                lines.extend(evidence_result_lines(result));
            }
            EvidenceStatus::Failed(error) => {
                lines.push(Line::from(vec![
                    status_span("failed", StatusTone::Error),
                    Span::raw(" "),
                    Span::raw(error.as_str()),
                ]));
            }
        }

        lines
    }

    fn apply_status_refresh(&mut self, snapshot: StatusRefreshSnapshot) {
        self.status = snapshot.status;
        self.semantic_status = snapshot.semantic_status;
        if let Some(explorer) = snapshot.explorer {
            self.storage.explorer = explorer;
        }
        if let Some(coverage) = snapshot.coverage {
            self.storage.coverage = coverage;
        }
        if let Some(outline) = snapshot.outline {
            self.storage.outline = outline;
        }
        if let Some(calls) = snapshot.calls {
            self.storage.calls = calls;
        }
        if let Some(embeddings) = snapshot.embeddings {
            self.storage.embeddings = embeddings;
        }
        if let Some(runs) = snapshot.runs {
            self.storage.runs = runs;
        }
        if let Some(freshness) = snapshot.freshness {
            self.storage.freshness = freshness;
        }
        if let Some(neighborhood) = snapshot.neighborhood {
            self.storage.neighborhood = neighborhood;
        }
        if let Some(health) = snapshot.health {
            self.storage.health = health;
        }
        if matches!(snapshot.scope, StatusRefreshScope::Full) {
            self.storage.selection = 0;
        }
    }

    fn start_status_refresh(
        &mut self,
        scope: StatusRefreshScope,
        success_message: Option<String>,
    ) -> bool {
        if self.status_refresh_receiver.is_some() {
            return false;
        }
        let repo_input = self.repo_input.clone();
        let repository_id = self.repository_id.clone();
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let result = collect_status_refresh(&repo_input, &repository_id, scope).map(Box::new);
            let _ = sender.send(StatusRefreshMessage::Finished {
                result,
                success_message,
            });
        });
        self.status_refresh_receiver = Some(receiver);
        true
    }

    fn diagnostics_row_count(&self) -> usize {
        match &self.diagnostics {
            DiagnosticsState::Completed(report) => report.checks.len(),
            _ => 0,
        }
    }

    fn storage_row_count(&self) -> usize {
        match self.storage.mode {
            StorageMode::Explorer => match &self.storage.explorer {
                StorageStatus::Completed(summary) => storage_row_count(summary),
                StorageStatus::Failed(_) => 1,
            },
            StorageMode::Coverage => match &self.storage.coverage {
                CoverageStatus::Completed(summary) => coverage_row_count(summary),
                CoverageStatus::Failed(_) => 1,
            },
            StorageMode::Outline => match &self.storage.outline {
                OutlineStatus::Completed(summary) => outline_row_count(summary),
                OutlineStatus::Failed(_) => 1,
            },
            StorageMode::Calls => match &self.storage.calls {
                CallResolutionStatus::Completed(summary) => call_resolution_row_count(summary),
                CallResolutionStatus::Failed(_) => 1,
            },
            StorageMode::Embeddings => match &self.storage.embeddings {
                EmbeddingCoverageStatus::Completed(summary) => {
                    embedding_coverage_row_count(summary)
                }
                EmbeddingCoverageStatus::Failed(_) => 1,
            },
            StorageMode::Runs => match &self.storage.runs {
                IndexRunsTimelineStatus::Completed(summary) => {
                    index_runs_timeline_row_count(summary)
                }
                IndexRunsTimelineStatus::Failed(_) => 1,
            },
            StorageMode::Freshness => match &self.storage.freshness {
                FreshnessStatus::Completed(summary) => freshness_row_count(summary),
                FreshnessStatus::Failed(_) => 1,
            },
            StorageMode::Neighborhood => match &self.storage.neighborhood {
                SemanticNeighborhoodStatus::Completed(summary) => {
                    semantic_neighborhood_row_count(summary)
                }
                SemanticNeighborhoodStatus::Failed(_) => 1,
            },
            StorageMode::Health => match &self.storage.health {
                CrossStoreHealthStatus::Completed(summary) => cross_store_health_row_count(summary),
                CrossStoreHealthStatus::Failed(_) => 1,
            },
        }
    }

    fn handle_key(&mut self, code: KeyCode) -> bool {
        match code {
            KeyCode::Tab => {
                self.toggle_active_mode(false);
                return false;
            }
            KeyCode::BackTab => {
                self.toggle_active_mode(true);
                return false;
            }
            KeyCode::Char('[') => {
                self.select_primary_tab(true);
                return false;
            }
            KeyCode::Char(']') => {
                self.select_primary_tab(false);
                return false;
            }
            _ => {}
        }

        if self.view == View::Query {
            return self.handle_query_key(code);
        }
        if self.view == View::Graph {
            return self.handle_graph_key(code);
        }
        if self.view == View::Evidence {
            return self.handle_evidence_key(code);
        }

        match code {
            KeyCode::Char('q') => return true,
            KeyCode::Esc
                if matches!(
                    self.screen,
                    Screen::ConfirmIndex(_) | Screen::ConfirmContinuous
                ) =>
            {
                self.screen = reduce_screen(self.screen, UiAction::Cancel);
                self.message = "Indexing action cancelled before start.".to_owned();
            }
            KeyCode::Esc => return true,
            KeyCode::Up if self.view == View::Storage && self.storage_row_count() > 0 => {
                self.storage.selection =
                    previous_selection(self.storage.selection, self.storage_row_count());
                self.message = "Storage row selection moved.".to_owned();
            }
            KeyCode::Down if self.view == View::Storage && self.storage_row_count() > 0 => {
                self.storage.selection =
                    next_selection(self.storage.selection, self.storage_row_count());
                self.message = "Storage row selection moved.".to_owned();
            }
            KeyCode::Char('f') if self.view == View::Storage => {
                self.request_stale_evidence_repair();
            }
            KeyCode::Up if self.view == View::Diagnostics && self.diagnostics_row_count() > 0 => {
                self.diagnostics_selection =
                    previous_selection(self.diagnostics_selection, self.diagnostics_row_count());
                self.message = "Doctor diagnostics selection moved.".to_owned();
            }
            KeyCode::Down if self.view == View::Diagnostics && self.diagnostics_row_count() > 0 => {
                self.diagnostics_selection =
                    next_selection(self.diagnostics_selection, self.diagnostics_row_count());
                self.message = "Doctor diagnostics selection moved.".to_owned();
            }
            KeyCode::Enter
                if self.view == View::Diagnostics && self.diagnostics_row_count() > 0 =>
            {
                self.diagnostics_details_expanded = !self.diagnostics_details_expanded;
                self.message = if self.diagnostics_details_expanded {
                    "Doctor selected-check details expanded.".to_owned()
                } else {
                    "Doctor selected-check details collapsed.".to_owned()
                };
            }
            KeyCode::Enter if self.view == View::Diagnostics => {
                self.start_diagnostics();
            }
            KeyCode::Char('o') if self.screen.accepts_new_index_request() => {
                let request = self.selected_index_request(IndexMode::Offline);
                self.screen = reduce_screen(self.screen, UiAction::RequestIndex(request));
                self.message = format!("Confirm {} indexing before starting.", request.label());
            }
            KeyCode::Char('s') if self.screen.accepts_new_index_request() => {
                let request = self.selected_index_request(IndexMode::Semantic);
                self.screen = reduce_screen(self.screen, UiAction::RequestIndex(request));
                self.message = format!("Confirm {} indexing before starting.", request.label());
            }
            KeyCode::Char('c') if self.continuous.enabled => {
                self.stop_continuous_index();
            }
            KeyCode::Char('c') if matches!(self.screen, Screen::IndexRunning(_)) => {
                self.message =
                    "Continuous indexing cannot start while a manual index is running.".to_owned();
            }
            KeyCode::Char('c') if self.screen.accepts_new_index_request() => {
                self.screen = reduce_screen(self.screen, UiAction::RequestContinuous);
                self.message = "Confirm continuous indexing before starting.".to_owned();
            }
            KeyCode::Char('y') => {
                if let Screen::ConfirmIndex(request) = self.screen {
                    self.start_index_job(request);
                } else if matches!(self.screen, Screen::ConfirmContinuous) {
                    self.start_continuous_index();
                }
            }
            KeyCode::Char('n')
                if matches!(
                    self.screen,
                    Screen::ConfirmIndex(_) | Screen::ConfirmContinuous
                ) =>
            {
                self.screen = reduce_screen(self.screen, UiAction::Cancel);
                self.message = "Indexing action cancelled before start.".to_owned();
            }
            KeyCode::Enter if self.screen.is_terminal_job_state() => {
                self.screen = reduce_screen(self.screen, UiAction::Dismiss);
                self.message = "Indexing controls ready.".to_owned();
            }
            KeyCode::Char('r') if !matches!(self.screen, Screen::IndexRunning(_)) => {
                self.handle_refresh_key();
            }
            _ => {}
        }
        false
    }

    fn handle_refresh_key(&mut self) {
        self.handle_refresh_key_with(|app| app.start_diagnostics());
    }

    fn handle_refresh_key_with<R>(&mut self, diagnostic_runner: R)
    where
        R: FnOnce(&mut Self),
    {
        if self.view == View::Diagnostics {
            if self.diagnostics_receiver.is_some() {
                self.message = "Doctor diagnostics already running.".to_owned();
            } else {
                diagnostic_runner(self);
            }
            return;
        }

        if self.start_status_refresh(
            StatusRefreshScope::Full,
            Some("Repository and storage status refreshed.".to_owned()),
        ) {
            self.last_index_status_refresh = Some(Instant::now());
            self.message = "Refreshing repository and storage status...".to_owned();
        } else {
            self.message = "Status refresh already running.".to_owned();
        }
    }

    fn tick_animation(&mut self) {
        if self.continuous.is_on() {
            self.animation_tick = self.animation_tick.wrapping_add(1);
        }
    }

    fn watcher_status_refresh_due(&self, now: Instant) -> bool {
        refresh_due(
            self.last_watcher_status_refresh,
            now,
            WATCHER_STATUS_REFRESH_INTERVAL,
        )
    }

    fn index_status_refresh_due(&self, now: Instant) -> bool {
        refresh_due(
            self.last_index_status_refresh,
            now,
            INDEX_STATUS_REFRESH_INTERVAL,
        )
    }

    fn continuous_activity_span(&self) -> Option<Span<'static>> {
        if !self.continuous.is_on() {
            return None;
        }
        Some(Span::styled(
            format!("[{}]", continuous_activity_frame(self.animation_tick)),
            tone_style(self.continuous.status_tone()).add_modifier(Modifier::BOLD),
        ))
    }

    fn selected_index_request(&self, mode: IndexMode) -> ManualIndexRequest {
        ManualIndexRequest {
            mode,
            scope: self.index_scope,
        }
    }

    fn repairable_freshness_count(&self) -> Option<usize> {
        match &self.storage.freshness {
            FreshnessStatus::Completed(summary) => Some(
                summary.count(EvidenceFreshness::Stale)
                    + summary.count(EvidenceFreshness::Deleted)
                    + summary.count(EvidenceFreshness::Missing),
            ),
            FreshnessStatus::Failed(_) => None,
        }
    }

    fn request_stale_evidence_repair(&mut self) {
        if !self.screen.accepts_new_index_request() {
            self.message =
                "Dismiss or finish the current indexing job before fixing stale evidence."
                    .to_owned();
            return;
        }

        let Some(repairable_rows) = self.repairable_freshness_count() else {
            self.storage.mode = StorageMode::Freshness;
            self.storage.selection = 0;
            self.message =
                "Freshness status is unavailable; press r to refresh storage first.".to_owned();
            return;
        };

        self.storage.mode = StorageMode::Freshness;
        self.storage.selection = 0;
        if repairable_rows == 0 {
            self.message = "No stale, deleted, or missing evidence rows need repair.".to_owned();
            return;
        }

        let request = ManualIndexRequest {
            mode: IndexMode::Semantic,
            scope: IndexScope::Incremental,
        };
        self.index_scope = IndexScope::Incremental;
        self.view = View::Indexing;
        self.screen = reduce_screen(self.screen, UiAction::RequestIndex(request));
        self.message = format!(
            "Confirm semantic incremental indexing to repair {repairable_rows} stale evidence rows."
        );
    }

    fn select_primary_tab(&mut self, reverse: bool) {
        self.view = if reverse {
            self.view.previous()
        } else {
            self.view.next()
        };
        self.message = format!("{} tab selected.", self.view.title());
    }

    fn toggle_active_mode(&mut self, reverse: bool) {
        match self.view {
            View::Storage => {
                self.storage.mode = if reverse {
                    self.storage.mode.previous()
                } else {
                    self.storage.mode.toggled()
                };
                self.storage.selection = 0;
                self.message = format!("Storage mode set to {}.", self.storage.mode.label());
            }
            View::Query => {
                self.query.mode = self.query.mode.toggled();
                self.query.status = QueryStatus::Idle;
                self.query.selection = 0;
                self.message = format!("Query mode set to {}.", self.query.mode.label());
            }
            View::Graph => {
                self.graph.direction = self.graph.direction.toggled();
                self.graph.status = GraphStatus::Idle;
                self.graph.selection = 0;
                self.message = format!("Graph mode set to {}.", self.graph.direction.label());
            }
            View::Evidence => {
                self.evidence.mode = self.evidence.mode.toggled();
                self.evidence.status = EvidenceStatus::Idle;
                self.evidence.selection = 0;
                self.message = format!("Evidence mode set to {}.", self.evidence.mode.label());
            }
            View::Overview => {
                self.message = "Overview has no alternate mode.".to_owned();
            }
            View::Indexing => {
                self.index_scope = match self.index_scope {
                    IndexScope::Full => IndexScope::Incremental,
                    IndexScope::Incremental => IndexScope::Full,
                };
                self.message = format!("Index scope set to {}.", self.index_scope.label());
            }
            View::Diagnostics => {
                self.message = "No alternate doctor mode is selected with Tab.".to_owned();
            }
        }
    }

    fn handle_query_key(&mut self, code: KeyCode) -> bool {
        match code {
            KeyCode::Char('q') if self.query.input.is_empty() => return true,
            KeyCode::Up if self.query.result_count() > 0 => {
                self.query.selection =
                    previous_selection(self.query.selection, self.query.result_count());
                self.message = "Query result selection moved.".to_owned();
            }
            KeyCode::Down if self.query.result_count() > 0 => {
                self.query.selection =
                    next_selection(self.query.selection, self.query.result_count());
                self.message = "Query result selection moved.".to_owned();
            }
            KeyCode::Esc if self.query.input.is_empty() => {
                self.view = View::Indexing;
                self.message = "Indexing controls selected.".to_owned();
            }
            KeyCode::Esc => {
                self.query.input.clear();
                self.query.status = QueryStatus::Idle;
                self.query.selection = 0;
                self.message = "Query input cleared.".to_owned();
            }
            KeyCode::Enter => {
                self.start_query();
            }
            KeyCode::Backspace => {
                self.query.input.pop();
                self.query.selection = 0;
            }
            KeyCode::Char(character) => {
                self.query.input.push(character);
                if matches!(self.query.status, QueryStatus::Failed(_)) {
                    self.query.status = QueryStatus::Idle;
                }
                self.query.selection = 0;
            }
            _ => {}
        }
        false
    }

    fn handle_graph_key(&mut self, code: KeyCode) -> bool {
        match code {
            KeyCode::Char('q') if self.graph.input.is_empty() => return true,
            KeyCode::Up if self.graph.result_count() > 0 => {
                self.graph.selection =
                    previous_selection(self.graph.selection, self.graph.result_count());
                self.message = "Graph result selection moved.".to_owned();
            }
            KeyCode::Down if self.graph.result_count() > 0 => {
                self.graph.selection =
                    next_selection(self.graph.selection, self.graph.result_count());
                self.message = "Graph result selection moved.".to_owned();
            }
            KeyCode::Esc if self.graph.input.is_empty() => {
                self.view = View::Indexing;
                self.message = "Indexing controls selected.".to_owned();
            }
            KeyCode::Esc => {
                self.graph.input.clear();
                self.graph.status = GraphStatus::Idle;
                self.graph.selection = 0;
                self.message = "Graph input cleared.".to_owned();
            }
            KeyCode::Enter => {
                self.start_graph_lookup();
            }
            KeyCode::Backspace => {
                self.graph.input.pop();
                self.graph.selection = 0;
            }
            KeyCode::Char(character) => {
                self.graph.input.push(character);
                if matches!(self.graph.status, GraphStatus::Failed(_)) {
                    self.graph.status = GraphStatus::Idle;
                }
                self.graph.selection = 0;
            }
            _ => {}
        }
        false
    }

    fn handle_evidence_key(&mut self, code: KeyCode) -> bool {
        match code {
            KeyCode::Char('q') if self.evidence.input.is_empty() => return true,
            KeyCode::Up if self.evidence.result_count() > 0 => {
                self.evidence.selection =
                    previous_selection(self.evidence.selection, self.evidence.result_count());
                self.message = "Evidence result selection moved.".to_owned();
            }
            KeyCode::Down if self.evidence.result_count() > 0 => {
                self.evidence.selection =
                    next_selection(self.evidence.selection, self.evidence.result_count());
                self.message = "Evidence result selection moved.".to_owned();
            }
            KeyCode::Esc if self.evidence.input.is_empty() => {
                self.view = View::Indexing;
                self.message = "Indexing controls selected.".to_owned();
            }
            KeyCode::Esc => {
                self.evidence.input.clear();
                self.evidence.status = EvidenceStatus::Idle;
                self.evidence.selection = 0;
                self.message = "Evidence input cleared.".to_owned();
            }
            KeyCode::Enter => {
                self.start_evidence_lookup();
            }
            KeyCode::Backspace => {
                self.evidence.input.pop();
                self.evidence.selection = 0;
            }
            KeyCode::Char(character) => {
                self.evidence.input.push(character);
                if matches!(self.evidence.status, EvidenceStatus::Failed(_)) {
                    self.evidence.status = EvidenceStatus::Idle;
                }
                self.evidence.selection = 0;
            }
            _ => {}
        }
        false
    }

    fn start_index_job(&mut self, request: ManualIndexRequest) {
        let repo = self.repo_input.clone();
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let _ = sender.send(IndexJobMessage::Progress(IndexProgress {
                phase: "queue",
                completed: 0,
                total: 1,
                message: "Submitting index job to writer service".to_owned(),
            }));
            let job = WriterJob::Index {
                repo: repo.clone(),
                offline: matches!(request.mode, IndexMode::Offline),
                scope: writer_scope(request.scope),
            };
            let progress_sender = sender.clone();
            let result = WriterClient::from_env()
                .submit_and_wait_with_progress(&job, move |progress| {
                    let _ = progress_sender.send(IndexJobMessage::Progress(
                        index_progress_from_writer(progress),
                    ));
                })
                .and_then(index_summary_from_writer_response)
                .map(Box::new)
                .and_then(|summary| {
                    let (quality, quality_error) =
                        match run_manual_quality_after_index(&repo, request, &sender) {
                            Ok(quality) => (quality.map(Box::new), None),
                            Err(error) => (None, Some(error)),
                        };
                    Ok(Box::new(ManualIndexJobSummary {
                        index: summary,
                        quality,
                        quality_error,
                    }))
                });
            let _ = sender.send(IndexJobMessage::Finished(result));
        });
        self.index_receiver = Some(receiver);
        self.last_index_summary = None;
        self.index_progress = Some(IndexProgress {
            phase: "start",
            completed: 0,
            total: 1,
            message: format!("Starting {} indexing", request.label()),
        });
        self.last_error = None;
        self.screen = reduce_screen(self.screen, UiAction::Confirm);
        self.message = format!("{} indexing started.", request.label());
    }

    fn start_continuous_index(&mut self) {
        match symdex_watch::start_or_attach(&self.repo_input, WatcherClientKind::Tui) {
            Ok(attachment) => {
                let status = attachment
                    .status()
                    .unwrap_or_else(|_| self.continuous_watcher_status_fallback());
                self.watcher_attachment = Some(attachment);
                self.apply_watcher_status(&status);
                self.last_watcher_status_refresh = Some(Instant::now());
                self.message = "Continuous indexing watcher attached.".to_owned();
            }
            Err(error) => {
                self.continuous.enabled = false;
                self.continuous.status = ContinuousIndexStatus::Failed;
                self.continuous.latest_error = Some(error);
                self.message = "Continuous indexing failed.".to_owned();
            }
        }
        self.screen = reduce_screen(self.screen, UiAction::Confirm);
    }

    fn stop_continuous_index(&mut self) {
        let had_attachment = self.watcher_attachment.take().is_some();
        match symdex_watch::stop_daemon(&self.repo_input) {
            Ok(status) => {
                self.apply_watcher_status(&status);
                self.continuous.enabled = false;
                self.continuous.status = ContinuousIndexStatus::Off;
                self.last_watcher_status_refresh = Some(Instant::now());
                self.message = "Continuous indexing stopped.".to_owned();
            }
            Err(error) => {
                if had_attachment {
                    self.continuous.status = ContinuousIndexStatus::Failed;
                    self.continuous.latest_error = Some(error);
                    self.message = "Continuous indexing stop failed.".to_owned();
                } else {
                    self.continuous.enabled = false;
                    self.continuous.status = ContinuousIndexStatus::Off;
                    self.continuous.queued_events = 0;
                    self.message = "Continuous indexing stopped.".to_owned();
                }
            }
        }
        self.continuous_receiver = None;
        self.continuous_stop = None;
    }

    fn continuous_watcher_status_fallback(&self) -> WatcherStatus {
        WatcherStatus {
            repository_id: self.repository_id.clone(),
            root_path: self.repo_root.clone(),
            mode: "semantic".to_owned(),
            owner_kind: "unknown".to_owned(),
            owner_pid: None,
            socket_path: None,
            state: "starting".to_owned(),
            started_at: None,
            updated_at: None,
            heartbeat_at: None,
            files_seen: self.continuous.files_seen,
            queued_events: self.continuous.queued_events,
            last_indexed_path: self.continuous.last_reindexed_file.clone(),
            last_error: None,
            active_layer: self.continuous.active_layer.clone(),
            quality_status: self.continuous.quality_status.clone(),
            quality_pending_jobs: self.continuous.quality_pending_jobs,
            quality_running_jobs: self.continuous.quality_running_jobs,
            quality_failed_jobs: self.continuous.quality_failed_jobs,
            quality_stale_jobs: self.continuous.quality_stale_jobs,
            attached_clients: 1,
            client_kinds: vec!["tui".to_owned()],
            clients: Vec::new(),
            shutdown_after_seconds: None,
        }
    }

    fn start_diagnostics(&mut self) {
        self.start_diagnostics_with(|repo| run_diagnostics_for_repo(Some(&repo)));
    }

    fn start_diagnostics_with<R>(&mut self, runner: R)
    where
        R: FnOnce(String) -> Result<DiagnosticReport, String> + Send + 'static,
    {
        let (sender, receiver) = mpsc::channel();
        let repo = self.repo_root.clone();
        thread::spawn(move || {
            let result = runner(repo);
            let _ = sender.send(result);
        });
        self.view = View::Diagnostics;
        self.diagnostics = DiagnosticsState::Running;
        self.diagnostics_details_expanded = false;
        self.diagnostics_receiver = Some(receiver);
        self.message = "Doctor diagnostics started.".to_owned();
    }

    fn apply_watcher_status(&mut self, status: &WatcherStatus) {
        self.continuous.enabled = matches!(
            status.state.as_str(),
            "starting" | "running" | "pending" | "indexing" | "failed" | "stale"
        );
        self.continuous.status = match status.state.as_str() {
            "starting" => ContinuousIndexStatus::Starting,
            "running" => ContinuousIndexStatus::Watching,
            "pending" => ContinuousIndexStatus::Pending,
            "indexing" => ContinuousIndexStatus::Indexing,
            "failed" | "stale" => ContinuousIndexStatus::Failed,
            _ => ContinuousIndexStatus::Off,
        };
        self.continuous.files_seen = status.files_seen;
        self.continuous.queued_events = status.queued_events;
        self.continuous.last_reindexed_file = status.last_indexed_path.clone();
        self.continuous.latest_error = status.last_error.clone();
        if status.state == "stale" && self.continuous.latest_error.is_none() {
            self.continuous.latest_error = Some("watcher heartbeat stale".to_owned());
        }
        self.continuous.active_layer = status.active_layer.clone();
        self.continuous.quality_status = status.quality_status.clone();
        self.continuous.quality_pending_jobs = status.quality_pending_jobs;
        self.continuous.quality_running_jobs = status.quality_running_jobs;
        self.continuous.quality_failed_jobs = status.quality_failed_jobs;
        self.continuous.quality_stale_jobs = status.quality_stale_jobs;
        self.continuous.attached_clients = status.attached_clients;
        self.continuous.client_kinds = status.client_kinds.clone();
        self.continuous.shutdown_after_seconds = status.shutdown_after_seconds;
    }

    fn start_query(&mut self) {
        let query = self.query.input.trim().to_owned();
        if query.is_empty() {
            self.query.status = QueryStatus::Failed("query is empty".to_owned());
            self.message = "Query failed.".to_owned();
            return;
        }

        let mode = self.query.mode;
        let repo = self.repo_input.clone();
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let result = match mode {
                QueryMode::Semantic => {
                    run_semantic_search(&repo, &query, 10).map(QueryResult::Semantic)
                }
                QueryMode::Symbol => run_symbol_search(&repo, &query).map(QueryResult::Symbol),
            };
            let _ = sender.send(result);
        });
        self.query.status = QueryStatus::Running;
        self.query_receiver = Some(receiver);
        self.message = format!("{} query started.", mode.label());
    }

    fn start_graph_lookup(&mut self) {
        let query = self.graph.input.trim().to_owned();
        if query.is_empty() {
            self.graph.status = GraphStatus::Failed("symbol query is empty".to_owned());
            self.message = "Graph lookup failed.".to_owned();
            return;
        }

        let direction = self.graph.direction;
        let repo = self.repo_input.clone();
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let result = run_call_graph(&repo, &query, direction);
            let _ = sender.send(result);
        });
        self.graph.status = GraphStatus::Running;
        self.graph_receiver = Some(receiver);
        self.message = format!("{} graph lookup started.", direction.label());
    }

    fn start_evidence_lookup(&mut self) {
        let query = self.evidence.input.trim().to_owned();
        if query.is_empty() {
            self.evidence.status = EvidenceStatus::Failed("symbol query is empty".to_owned());
            self.message = "Evidence lookup failed.".to_owned();
            return;
        }

        let mode = self.evidence.mode;
        let repo = self.repo_input.clone();
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let result = match mode {
                EvidenceMode::Impact => run_impact(&repo, &query).map(EvidenceResult::Impact),
                EvidenceMode::CallPath => parse_call_path_input(&query)
                    .and_then(|(source, target)| run_call_path(&repo, &source, &target, 4))
                    .map(EvidenceResult::CallPath),
                EvidenceMode::ContextPack => {
                    run_context_pack(&repo, &query, 8).map(EvidenceResult::ContextPack)
                }
                EvidenceMode::DebugContext => {
                    run_debug_context_pack(&repo, &query, 8).map(EvidenceResult::DebugContext)
                }
            };
            let _ = sender.send(result);
        });
        self.evidence.status = EvidenceStatus::Running;
        self.evidence_receiver = Some(receiver);
        self.message = format!("{} lookup started.", mode.label());
    }

    fn poll_index_job(&mut self) {
        let Some(receiver) = &self.index_receiver else {
            return;
        };
        match receiver.try_recv() {
            Ok(IndexJobMessage::Progress(progress)) => {
                self.index_progress = Some(progress);
            }
            Ok(IndexJobMessage::Finished(Ok(summary))) => {
                self.index_receiver = None;
                let request = self.screen.index_request().unwrap_or(ManualIndexRequest {
                    mode: IndexMode::Offline,
                    scope: self.index_scope,
                });
                self.message = if let Some(error) = &summary.quality_error {
                    self.continuous.latest_quality_error = Some(error.clone());
                    self.semantic_status.latest_quality_error = Some(error.clone());
                    format!(
                        "{} indexing completed, but quality indexing failed: {}",
                        request.label(),
                        error
                    )
                } else if let Some(quality) = &summary.quality {
                    self.continuous.active_layer = Some(quality.active_layer.clone());
                    self.continuous.quality_status = Some(quality.quality_status.clone());
                    self.continuous.activation_reason = Some(quality.activation_reason.clone());
                    self.continuous.quality_pending_jobs = quality.remaining_pending_jobs;
                    self.continuous.quality_running_jobs = 0;
                    self.continuous.quality_failed_jobs = quality.failed_jobs;
                    self.continuous.quality_stale_jobs = quality.skipped_stale_jobs;
                    self.continuous.latest_quality_error = None;
                    format!(
                        "{} indexing completed: {} files indexed; quality {} on {} after {} jobs.",
                        request.label(),
                        summary.index.sqlite_files_indexed,
                        quality.quality_status,
                        quality.active_layer,
                        quality.claimed_jobs
                    )
                } else {
                    format!(
                        "{} indexing completed: {} files indexed, {} chunks indexed.",
                        request.label(),
                        summary.index.sqlite_files_indexed,
                        summary.index.sqlite_chunks_indexed
                    )
                };
                let completed_message = self.message.clone();
                self.last_index_summary = Some(*summary.index);
                self.index_progress = None;
                self.screen = reduce_screen(self.screen, UiAction::JobSucceeded);
                if self
                    .start_status_refresh(StatusRefreshScope::Full, Some(completed_message.clone()))
                {
                    self.last_index_status_refresh = Some(Instant::now());
                } else {
                    self.message = completed_message;
                }
            }
            Ok(IndexJobMessage::Finished(Err(error))) => {
                self.index_receiver = None;
                self.index_progress = None;
                self.last_error = Some(error);
                self.screen = reduce_screen(self.screen, UiAction::JobFailed);
                self.message = "Indexing failed.".to_owned();
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                self.index_receiver = None;
                self.last_error = Some("indexing worker disconnected".to_owned());
                self.screen = reduce_screen(self.screen, UiAction::JobFailed);
                self.message = "Indexing failed.".to_owned();
            }
        }
    }

    fn poll_continuous_index(&mut self) {
        let Some(receiver) = &self.continuous_receiver else {
            let now = Instant::now();
            if self.watcher_status_refresh_due(now) {
                self.last_watcher_status_refresh = Some(now);
                if let Ok(status) = symdex_watch::status(&self.repo_input) {
                    self.apply_watcher_status(&status);
                }
            }
            return;
        };
        match receiver.try_recv() {
            Ok(ContinuousIndexMessage::Event(event)) => {
                self.apply_continuous_event(*event);
            }
            Ok(ContinuousIndexMessage::Stopped) => {
                self.continuous_receiver = None;
                self.continuous_stop = None;
                if self.continuous.enabled {
                    self.continuous.enabled = false;
                    self.continuous.status = ContinuousIndexStatus::Off;
                    self.message = "Continuous indexing stopped.".to_owned();
                }
            }
            Ok(ContinuousIndexMessage::Failed(error)) => {
                self.continuous_receiver = None;
                self.continuous_stop = None;
                self.continuous.enabled = false;
                self.continuous.status = ContinuousIndexStatus::Failed;
                self.continuous.latest_error = Some(error);
                self.message = "Continuous indexing failed.".to_owned();
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                self.continuous_receiver = None;
                self.continuous_stop = None;
                if self.continuous.enabled {
                    self.continuous.enabled = false;
                    self.continuous.status = ContinuousIndexStatus::Failed;
                    self.continuous.latest_error =
                        Some("continuous indexing worker disconnected".to_owned());
                    self.message = "Continuous indexing failed.".to_owned();
                }
            }
        }
    }

    fn refresh_index_status_on_interval(&mut self) {
        if !matches!(self.view, View::Overview | View::Indexing) {
            return;
        }
        if self.index_receiver.is_some() || self.status_refresh_receiver.is_some() {
            return;
        }
        let now = Instant::now();
        if !self.index_status_refresh_due(now) {
            return;
        }
        self.last_index_status_refresh = Some(now);
        let _ = self.start_status_refresh(StatusRefreshScope::Index, None);
    }

    fn poll_status_refresh(&mut self) {
        let Some(receiver) = &self.status_refresh_receiver else {
            return;
        };
        match receiver.try_recv() {
            Ok(StatusRefreshMessage::Finished {
                result,
                success_message,
            }) => {
                self.status_refresh_receiver = None;
                match result {
                    Ok(snapshot) => {
                        self.apply_status_refresh(*snapshot);
                        if let Some(message) = success_message {
                            self.message = message;
                        }
                    }
                    Err(error) => {
                        self.last_error = Some(error.clone());
                        self.message = format!("Refresh failed: {error}");
                    }
                }
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                self.status_refresh_receiver = None;
                self.last_error = Some("status refresh worker disconnected".to_owned());
                self.message = "Refresh failed: status refresh worker disconnected".to_owned();
            }
        }
    }

    fn apply_continuous_event(&mut self, event: ContinuousIndexEvent) {
        match event {
            ContinuousIndexEvent::Started { files_seen, .. } => {
                self.continuous.enabled = true;
                self.continuous.status = ContinuousIndexStatus::Watching;
                self.continuous.files_seen = files_seen;
                self.continuous.latest_error = None;
                self.message = format!("Continuous indexing on: watching {files_seen} files.");
            }
            ContinuousIndexEvent::Idle { files_seen } => {
                self.continuous.files_seen = files_seen;
                if self.continuous.enabled
                    && matches!(
                        self.continuous.status,
                        ContinuousIndexStatus::Starting
                            | ContinuousIndexStatus::Pending
                            | ContinuousIndexStatus::Indexing
                    )
                {
                    self.continuous.status = ContinuousIndexStatus::Watching;
                }
            }
            ContinuousIndexEvent::ChangesPending { changes } => {
                self.continuous.status = ContinuousIndexStatus::Pending;
                self.continuous.queued_events = changes.event_count();
                self.continuous.latest_error = None;
                self.message = format!(
                    "Continuous indexing debounce pending: {} events.",
                    changes.event_count()
                );
            }
            ContinuousIndexEvent::ChangesDetected { changes } => {
                self.continuous.status = ContinuousIndexStatus::Indexing;
                self.continuous.queued_events = changes.event_count();
                self.continuous.latest_error = None;
                self.message = format!(
                    "Continuous indexing {} changed paths.",
                    changes.event_count()
                );
            }
            ContinuousIndexEvent::BatchCompleted { changes, summary } => {
                self.continuous.status = ContinuousIndexStatus::Watching;
                self.continuous.queued_events = 0;
                self.continuous.last_reindexed_file =
                    changes.paths().first().map(|path| (*path).to_owned());
                self.continuous.latest_error = None;
                self.last_index_summary = Some(*summary);
                let completed_message = format!(
                    "Continuous indexing updated {} file events.",
                    changes.event_count()
                );
                if self.start_status_refresh(StatusRefreshScope::Index, Some(completed_message)) {
                    self.last_index_status_refresh = Some(Instant::now());
                }
            }
            ContinuousIndexEvent::BatchFailed { changes, error } => {
                self.continuous.status = ContinuousIndexStatus::Failed;
                self.continuous.queued_events = changes.event_count();
                self.continuous.latest_error = Some(error);
                self.message = "Continuous indexing batch failed.".to_owned();
            }
            ContinuousIndexEvent::QualityState { state } => {
                self.continuous.apply_quality_state(&state);
                self.apply_semantic_quality_state(&state);
                self.message = format!(
                    "Quality state: {} on {}.",
                    state.quality_status, state.active_layer
                );
            }
            ContinuousIndexEvent::QualityStarted { state } => {
                self.continuous.apply_quality_state(&state);
                self.apply_semantic_quality_state(&state);
                self.continuous.latest_quality_error = None;
                self.semantic_status.latest_quality_error = None;
                self.message = format!(
                    "Quality catch-up started: {} pending jobs.",
                    state.pending_jobs
                );
            }
            ContinuousIndexEvent::QualityProgress { progress } => {
                self.message = progress.message;
            }
            ContinuousIndexEvent::QualityCompleted { summary } => {
                self.continuous.active_layer = Some(summary.active_layer.clone());
                self.continuous.quality_status = Some(summary.quality_status.clone());
                self.continuous.activation_reason = Some(summary.activation_reason.clone());
                self.continuous.quality_pending_jobs = summary.progress.pending_jobs;
                self.continuous.quality_running_jobs = summary.progress.running_jobs;
                self.continuous.quality_failed_jobs = summary.progress.failed_jobs;
                self.continuous.quality_stale_jobs = summary.progress.skipped_stale_jobs;
                self.continuous.latest_quality_error = None;
                self.apply_semantic_quality_summary(&summary);
                self.message = format!(
                    "Quality catch-up completed: {} on {}.",
                    summary.quality_status, summary.active_layer
                );
            }
            ContinuousIndexEvent::QualityFailed { state, error } => {
                if let Some(state) = &state {
                    self.continuous.apply_quality_state(state);
                    self.apply_semantic_quality_state(state);
                }
                self.continuous.latest_quality_error = Some(error.clone());
                self.semantic_status.latest_quality_error = Some(error);
                self.message = "Quality catch-up failed.".to_owned();
            }
        }
    }

    fn apply_semantic_quality_state(&mut self, state: &ContinuousQualityState) {
        let Ok(active_layer) = SemanticLayer::parse(&state.active_layer) else {
            return;
        };
        let Ok(quality_status) = SemanticLayerStatus::parse(&state.quality_status) else {
            return;
        };
        let quality_progress = QualityGenerationProgress {
            repository_id: state.repository_id.clone(),
            generation_id: state.generation_id.clone(),
            embeddable_chunks: state.embeddable_chunks,
            quality_eligible_chunks: state.quality_eligible_chunks,
            quality_ineligible_chunks: state.quality_ineligible_chunks,
            quality_embedded_chunks: state.quality_embedded_chunks,
            pending_jobs: state.pending_jobs,
            running_jobs: state.running_jobs,
            succeeded_jobs: state.succeeded_jobs,
            failed_jobs: state.failed_jobs,
            skipped_stale_jobs: state.skipped_stale_jobs,
            skipped_excluded_jobs: state.skipped_excluded_jobs,
        };
        self.semantic_status.generation_id = Some(state.generation_id.clone());
        self.semantic_status.active_layer = active_layer;
        self.semantic_status.quality_status = quality_status;
        self.semantic_status.quality.current_chunks = state.quality_embedded_chunks;
        self.semantic_status.quality.total_chunks = state.quality_embedded_chunks;
        self.semantic_status.quality.expected_chunks = quality_progress.quality_eligible_chunks;
        self.semantic_status.quality.is_complete = quality_progress_is_complete(&quality_progress);
        self.semantic_status.quality_progress = Some(quality_progress);
        self.semantic_status.fallback_reason = semantic_fallback_reason_for_status(
            active_layer,
            quality_status,
            self.semantic_status.quality.is_complete,
        );
    }

    fn apply_semantic_quality_summary(&mut self, summary: &symdex_index::QualityIndexSummary) {
        let Ok(active_layer) = SemanticLayer::parse(&summary.active_layer) else {
            return;
        };
        let Ok(quality_status) = SemanticLayerStatus::parse(&summary.quality_status) else {
            return;
        };
        self.semantic_status.repository_id = summary.repository_id.clone();
        self.semantic_status.generation_id = Some(summary.generation_id.clone());
        self.semantic_status.active_layer = active_layer;
        self.semantic_status.quality_status = quality_status;
        self.semantic_status.quality.embedding_model = summary.quality_model.clone();
        self.semantic_status.quality.embedding_dimension = summary.quality_dimension;
        self.semantic_status.quality.vector_table = summary.vector_table.clone();
        self.semantic_status.quality.current_chunks = summary.progress.quality_embedded_chunks;
        self.semantic_status.quality.total_chunks = summary.progress.quality_embedded_chunks;
        self.semantic_status.quality.expected_chunks = summary.progress.quality_eligible_chunks;
        self.semantic_status.quality.is_complete = quality_progress_is_complete(&summary.progress);
        self.semantic_status.quality_progress = Some(summary.progress.clone());
        self.semantic_status.latest_quality_error = None;
        self.semantic_status.fallback_reason = semantic_fallback_reason_for_status(
            active_layer,
            quality_status,
            self.semantic_status.quality.is_complete,
        );
    }

    fn poll_diagnostics(&mut self) {
        let Some(receiver) = &self.diagnostics_receiver else {
            return;
        };
        match receiver.try_recv() {
            Ok(Ok(report)) => {
                self.diagnostics_receiver = None;
                self.diagnostics = DiagnosticsState::Completed(report);
                self.diagnostics_selection = 0;
                self.diagnostics_details_expanded = false;
                self.message = "Doctor diagnostics completed.".to_owned();
            }
            Ok(Err(error)) => {
                self.diagnostics_receiver = None;
                self.diagnostics = DiagnosticsState::Failed(error);
                self.message = "Doctor diagnostics failed.".to_owned();
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                self.diagnostics_receiver = None;
                self.diagnostics =
                    DiagnosticsState::Failed("diagnostics worker disconnected".to_owned());
                self.message = "Doctor diagnostics failed.".to_owned();
            }
        }
    }

    fn poll_query(&mut self) {
        let Some(receiver) = &self.query_receiver else {
            return;
        };
        match receiver.try_recv() {
            Ok(Ok(result)) => {
                self.query_receiver = None;
                self.query.status = QueryStatus::Completed(result);
                self.query.selection = 0;
                self.message = "Query completed.".to_owned();
            }
            Ok(Err(error)) => {
                self.query_receiver = None;
                self.query.status = QueryStatus::Failed(error);
                self.message = "Query failed.".to_owned();
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                self.query_receiver = None;
                self.query.status = QueryStatus::Failed("query worker disconnected".to_owned());
                self.message = "Query failed.".to_owned();
            }
        }
    }

    fn poll_graph(&mut self) {
        let Some(receiver) = &self.graph_receiver else {
            return;
        };
        match receiver.try_recv() {
            Ok(Ok(summary)) => {
                self.graph_receiver = None;
                self.graph.status = GraphStatus::Completed(summary);
                self.graph.selection = 0;
                self.message = "Graph lookup completed.".to_owned();
            }
            Ok(Err(error)) => {
                self.graph_receiver = None;
                self.graph.status = GraphStatus::Failed(error);
                self.message = "Graph lookup failed.".to_owned();
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                self.graph_receiver = None;
                self.graph.status = GraphStatus::Failed("graph worker disconnected".to_owned());
                self.message = "Graph lookup failed.".to_owned();
            }
        }
    }

    fn poll_evidence(&mut self) {
        let Some(receiver) = &self.evidence_receiver else {
            return;
        };
        match receiver.try_recv() {
            Ok(Ok(result)) => {
                self.evidence_receiver = None;
                self.evidence.status = EvidenceStatus::Completed(result);
                self.evidence.selection = 0;
                self.message = "Evidence lookup completed.".to_owned();
            }
            Ok(Err(error)) => {
                self.evidence_receiver = None;
                self.evidence.status = EvidenceStatus::Failed(error);
                self.message = "Evidence lookup failed.".to_owned();
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                self.evidence_receiver = None;
                self.evidence.status =
                    EvidenceStatus::Failed("evidence worker disconnected".to_owned());
                self.message = "Evidence lookup failed.".to_owned();
            }
        }
    }
}

pub fn render<B: Backend>(terminal: &mut Terminal<B>, app: &App) -> Result<(), String> {
    terminal
        .draw(|frame| {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(3),
                    Constraint::Min(10),
                    Constraint::Length(4),
                ])
                .split(frame.area());

            let tabs = Tabs::new(View::tabs())
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .border_style(tone_style(StatusTone::Dim))
                        .title(Line::from(vec![
                            status_span("symdex", StatusTone::Info),
                            Span::raw(" TUI"),
                        ])),
                )
                .select(app.view.tab_index())
                .style(Style::new().fg(Color::DarkGray))
                .highlight_style(tab_highlight_style());
            frame.render_widget(tabs, chunks[0]);

            if app.view == View::Overview {
                render_overview_panel(frame, chunks[1], app);
            } else if chunks[1].width >= 110 {
                let body_chunks = Layout::default()
                    .direction(Direction::Horizontal)
                    .constraints([Constraint::Length(36), Constraint::Min(60)])
                    .split(chunks[1]);
                render_repository_summary_panel(frame, body_chunks[0], app);
                render_right_panel(frame, body_chunks[1], app);
            } else {
                let summary_height = if app.view == View::Indexing { 4 } else { 6 };
                let body_chunks = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([Constraint::Length(summary_height), Constraint::Min(8)])
                    .split(chunks[1]);
                render_repository_summary_panel(frame, body_chunks[0], app);
                render_right_panel(frame, body_chunks[1], app);
            }

            let mut status_line = Vec::new();
            if let Some(indicator) = app.continuous_activity_span() {
                status_line.push(Span::styled(
                    "ci ",
                    Style::new()
                        .fg(Color::DarkGray)
                        .add_modifier(Modifier::BOLD),
                ));
                status_line.push(indicator);
                status_line.push(Span::raw(" "));
            }
            status_line.extend([
                status_span(app.view.footer_label(), StatusTone::Info),
                Span::raw(" "),
                Span::styled("status: ", Style::new().add_modifier(Modifier::BOLD)),
                Span::raw(app.message.as_str()),
            ]);

            let footer_chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(2), Constraint::Length(2)])
                .split(chunks[2]);

            let keys = Paragraph::new(Line::from(footer_key_spans(app.view)))
                .wrap(Wrap { trim: true })
                .block(
                    Block::default()
                        .borders(Borders::TOP)
                        .border_style(tone_style(StatusTone::Info))
                        .title(Line::from(status_span("Keys", StatusTone::Info))),
                );
            frame.render_widget(keys, footer_chunks[0]);

            let status = Paragraph::new(Line::from(status_line))
                .wrap(Wrap { trim: true })
                .block(
                    Block::default()
                        .borders(Borders::TOP)
                        .border_style(tone_style(StatusTone::Dim))
                        .title(Line::from(status_span("Status", StatusTone::Dim))),
                );
            frame.render_widget(status, footer_chunks[1]);
        })
        .map(|_| ())
        .map_err(|error| error.to_string())
}

fn render_right_panel(frame: &mut ratatui::Frame<'_>, area: Rect, app: &App) {
    match app.view {
        View::Overview => render_overview_panel(frame, area, app),
        View::Storage => render_storage_panel(frame, area, app),
        View::Diagnostics => match &app.diagnostics {
            DiagnosticsState::Completed(report) => {
                render_diagnostics_panel(frame, area, app, report);
            }
            _ => render_line_panel(frame, area, "Doctor Diagnostics", app.diagnostics_lines()),
        },
        View::Query => match &app.query.status {
            QueryStatus::Completed(result) => {
                let chunks = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([Constraint::Length(3), Constraint::Min(5)])
                    .split(area);
                render_mode_bar(
                    frame,
                    chunks[0],
                    "Query Mode",
                    vec![
                        ("mode", app.query.mode.label().to_owned(), StatusTone::Info),
                        (
                            "input",
                            if app.query.input.is_empty() {
                                "<empty>".to_owned()
                            } else {
                                app.query.input.clone()
                            },
                            StatusTone::Dim,
                        ),
                    ],
                );
                render_selectable_table(
                    frame,
                    chunks[1],
                    query_table(result),
                    app.query.selection,
                    query_result_count(result),
                );
            }
            _ => render_line_panel(frame, area, "Query Workbench", app.query_lines()),
        },
        View::Graph => match &app.graph.status {
            GraphStatus::Completed(summary) => {
                let chunks = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([Constraint::Length(3), Constraint::Min(5)])
                    .split(area);
                render_mode_bar(
                    frame,
                    chunks[0],
                    "Calls Mode",
                    vec![
                        (
                            "direction",
                            app.graph.direction.label().to_owned(),
                            StatusTone::Info,
                        ),
                        (
                            "symbol",
                            if app.graph.input.is_empty() {
                                "<empty>".to_owned()
                            } else {
                                app.graph.input.clone()
                            },
                            StatusTone::Dim,
                        ),
                    ],
                );
                render_selectable_table(
                    frame,
                    chunks[1],
                    call_graph_table(summary),
                    app.graph.selection,
                    summary.rows.len(),
                );
            }
            _ => render_line_panel(frame, area, "Symbol/Call Graph", app.graph_lines()),
        },
        View::Evidence => match &app.evidence.status {
            EvidenceStatus::Completed(EvidenceResult::Impact(summary)) => {
                let chunks = evidence_result_chunks(area);
                render_evidence_mode_bar(frame, chunks[0], app);
                render_selectable_table(
                    frame,
                    chunks[1],
                    impact_table(summary),
                    app.evidence.selection,
                    impact_result_count(summary),
                );
            }
            EvidenceStatus::Completed(EvidenceResult::CallPath(summary)) => {
                let chunks = evidence_result_chunks(area);
                render_evidence_mode_bar(frame, chunks[0], app);
                render_selectable_table(
                    frame,
                    chunks[1],
                    call_path_table(summary),
                    app.evidence.selection,
                    call_path_result_count(summary),
                );
            }
            EvidenceStatus::Completed(EvidenceResult::ContextPack(pack)) => {
                let chunks = evidence_result_chunks(area);
                render_evidence_mode_bar(frame, chunks[0], app);
                render_selectable_table(
                    frame,
                    chunks[1],
                    context_pack_table(pack),
                    app.evidence.selection,
                    context_pack_row_count(pack),
                );
            }
            EvidenceStatus::Completed(EvidenceResult::DebugContext(pack)) => {
                let chunks = evidence_result_chunks(area);
                render_evidence_mode_bar(frame, chunks[0], app);
                render_selectable_table(
                    frame,
                    chunks[1],
                    debug_context_table(pack),
                    app.evidence.selection,
                    debug_context_row_count(pack),
                );
            }
            _ => render_line_panel(
                frame,
                area,
                "Impact/Call Path/Context Pack/Debug",
                app.evidence_lines(),
            ),
        },
        View::Indexing => render_index_panel(frame, area, app),
    }
}

fn render_repository_status_panel(frame: &mut ratatui::Frame<'_>, area: Rect, app: &App) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(tone_style(StatusTone::Info))
        .title(Line::from(status_span(
            "Repository Status",
            StatusTone::Info,
        )));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(5),
            Constraint::Length(7),
            Constraint::Min(5),
        ])
        .split(inner);

    let identity = Paragraph::new(vec![
        Line::from(vec![
            status_span("repo", StatusTone::Info),
            Span::raw(" "),
            Span::raw(app.repo_root.as_str()),
        ]),
        Line::from(vec![
            Span::styled("id ", Style::new().fg(Color::DarkGray)),
            Span::raw(app.repository_id.as_str()),
        ]),
        Line::from(vec![
            Span::styled("ref ", Style::new().fg(Color::DarkGray)),
            Span::raw(index_ref_label(&app.status)),
        ]),
        Line::from(vec![
            Span::styled("last indexed ", Style::new().fg(Color::DarkGray)),
            index_freshness_span(app.status.last_indexed_at.as_deref()),
            Span::raw(" "),
            Span::raw(app.status.last_indexed_at.as_deref().unwrap_or("<never>")),
        ]),
    ])
    .wrap(Wrap { trim: true });
    frame.render_widget(identity, chunks[0]);

    frame.render_widget(index_counts_table(app), chunks[1]);
    frame.render_widget(local_services_table(app), chunks[2]);
}

fn render_overview_panel(frame: &mut ratatui::Frame<'_>, area: Rect, app: &App) {
    if area.width >= 110 {
        let chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(42), Constraint::Percentage(58)])
            .split(area);
        render_repository_status_panel(frame, chunks[0], app);
        render_overview_focus_panel(frame, chunks[1], app);
    } else {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(12), Constraint::Min(6)])
            .split(area);
        render_repository_status_panel(frame, chunks[0], app);
        render_overview_focus_panel(frame, chunks[1], app);
    }
}

fn render_repository_summary_panel(frame: &mut ratatui::Frame<'_>, area: Rect, app: &App) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(tone_style(StatusTone::Dim))
        .title(Line::from(status_span("Repo Summary", StatusTone::Info)));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let lines = vec![
        Line::from(vec![
            status_span("repo", StatusTone::Info),
            Span::raw(" "),
            Span::raw(app.repo_root.as_str()),
        ]),
        Line::from(vec![
            Span::styled("indexed ", metadata_style()),
            index_freshness_span(app.status.last_indexed_at.as_deref()),
            Span::raw(" "),
            Span::raw(app.status.last_indexed_at.as_deref().unwrap_or("<never>")),
        ]),
        Line::from(vec![
            Span::styled("coverage ", metadata_style()),
            status_span("files", StatusTone::Info),
            Span::raw(format!(" {}  ", app.status.files_indexed)),
            status_span("chunks", StatusTone::Info),
            Span::raw(format!(" {}  ", app.status.chunks_indexed)),
            status_span("symbols", StatusTone::Info),
            Span::raw(format!(" {}", app.status.symbols_indexed)),
        ]),
        Line::from(vec![
            Span::styled("services ", metadata_style()),
            status_span("sqlite", StatusTone::Success),
            Span::raw(" local  "),
            status_span("vector", StatusTone::Success),
            Span::raw(" local  "),
            status_span("ollama", StatusTone::Success),
            Span::raw(" local"),
        ]),
        Line::from(vec![
            Span::styled("semantic ", metadata_style()),
            status_span(
                app.semantic_status.active_layer.as_str(),
                semantic_status_tone(&app.semantic_status),
            ),
            Span::raw(" quality "),
            status_span(
                app.semantic_status.quality_status.as_str(),
                semantic_quality_tone(&app.semantic_status),
            ),
        ]),
    ];
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }), inner);
}

fn render_overview_focus_panel(frame: &mut ratatui::Frame<'_>, area: Rect, app: &App) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(tone_style(StatusTone::Info))
        .title(Line::from(status_span(
            "Operational Focus",
            StatusTone::Info,
        )));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(5),
            Constraint::Length(6),
            Constraint::Min(4),
        ])
        .split(inner);

    render_mode_bar(
        frame,
        chunks[0],
        "Current State",
        vec![
            (
                "index",
                app.status
                    .last_indexed_at
                    .as_deref()
                    .unwrap_or("never")
                    .to_owned(),
                if app.status.last_indexed_at.is_some() {
                    StatusTone::Success
                } else {
                    StatusTone::Warning
                },
            ),
            (
                "embedding",
                index_embedding(&app.status),
                if app.status.embedding_model.is_some() {
                    StatusTone::Success
                } else {
                    StatusTone::Warning
                },
            ),
            (
                "layer",
                app.semantic_status.active_layer.as_str().to_owned(),
                semantic_status_tone(&app.semantic_status),
            ),
            (
                "quality",
                app.semantic_status.quality_status.as_str().to_owned(),
                semantic_quality_tone(&app.semantic_status),
            ),
            (
                "qjobs",
                semantic_quality_jobs(&app.semantic_status),
                semantic_quality_tone(&app.semantic_status),
            ),
            (
                "watch",
                app.continuous.summary(),
                app.continuous.status_tone(),
            ),
        ],
    );

    let health_rows = vec![
        Row::new(vec![
            Cell::from("SQLite"),
            Cell::from(status_span("local", StatusTone::Success)),
            Cell::from(app.sqlite_path.clone()),
        ]),
        Row::new(vec![
            Cell::from("sqlite-vec"),
            Cell::from(status_span("local", StatusTone::Success)),
            Cell::from(app.vector_store_label.clone()),
        ]),
        Row::new(vec![
            Cell::from("Ollama"),
            Cell::from(status_span("local", StatusTone::Success)),
            Cell::from(app.ollama_url.clone()),
        ]),
    ];
    let health = Table::new(
        health_rows,
        [
            Constraint::Length(10),
            Constraint::Length(10),
            Constraint::Min(16),
        ],
    )
    .header(table_header(["Service", "State", "Target"]))
    .block(panel_block("Local Services", StatusTone::Dim));
    frame.render_widget(health, chunks[1]);

    let view_rows = vec![
        Row::new(vec![
            Cell::from("Index"),
            Cell::from(screen_label(app.screen)),
            Cell::from(status_span("ready", StatusTone::Info)),
        ]),
        Row::new(vec![
            Cell::from("Storage"),
            Cell::from(app.storage.mode.label()),
            Cell::from(status_span("loaded", StatusTone::Success)),
        ]),
        Row::new(vec![
            Cell::from("Semantic"),
            Cell::from(semantic_status_mode(&app.semantic_status)),
            Cell::from(status_span(
                semantic_status_state(&app.semantic_status),
                semantic_status_tone(&app.semantic_status),
            )),
        ]),
        Row::new(vec![
            Cell::from("QJobs"),
            Cell::from(semantic_quality_jobs(&app.semantic_status)),
            Cell::from(status_span(
                semantic_fallback_state(&app.semantic_status),
                semantic_quality_tone(&app.semantic_status),
            )),
        ]),
        Row::new(vec![
            Cell::from("Query"),
            Cell::from(app.query.mode.label()),
            Cell::from(status_span(
                query_status_label(&app.query.status),
                query_status_tone(&app.query.status),
            )),
        ]),
        Row::new(vec![
            Cell::from("Calls"),
            Cell::from(app.graph.direction.label()),
            Cell::from(status_span(
                graph_status_label(&app.graph.status),
                graph_status_tone(&app.graph.status),
            )),
        ]),
        Row::new(vec![
            Cell::from("Impact"),
            Cell::from(app.evidence.mode.label()),
            Cell::from(status_span(
                evidence_status_label(&app.evidence.status),
                evidence_status_tone(&app.evidence.status),
            )),
        ]),
    ];
    let views = Table::new(
        view_rows,
        [
            Constraint::Length(10),
            Constraint::Length(18),
            Constraint::Min(10),
        ],
    )
    .header(table_header(["View", "Mode", "State"]))
    .block(panel_block("Mode Snapshot", StatusTone::Dim));
    frame.render_widget(views, chunks[2]);
}

fn render_storage_panel(frame: &mut ratatui::Frame<'_>, area: Rect, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(5)])
        .split(area);
    let tabs = Tabs::new(StorageMode::tabs())
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("Storage Views"),
        )
        .select(app.storage.mode.tab_index())
        .style(Style::new().fg(Color::DarkGray))
        .highlight_style(Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD));
    frame.render_widget(tabs, chunks[0]);
    render_storage_mode_panel(frame, chunks[1], app);
}

fn render_storage_mode_panel(frame: &mut ratatui::Frame<'_>, area: Rect, app: &App) {
    match app.storage.mode {
        StorageMode::Explorer => match &app.storage.explorer {
            StorageStatus::Completed(summary) => {
                let chunks = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([Constraint::Min(9), Constraint::Length(8)])
                    .split(area);
                render_selectable_table(
                    frame,
                    chunks[0],
                    storage_table(summary),
                    app.storage.selection,
                    storage_row_count(summary),
                );
                frame.render_widget(
                    storage_detail_panel(summary, app.storage.selection),
                    chunks[1],
                );
            }
            StorageStatus::Failed(error) => render_storage_error(frame, area, error),
        },
        StorageMode::Coverage => match &app.storage.coverage {
            CoverageStatus::Completed(summary) => {
                let chunks = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([Constraint::Min(5), Constraint::Length(13)])
                    .split(area);
                render_selectable_table(
                    frame,
                    chunks[0],
                    coverage_table(summary),
                    app.storage.selection,
                    coverage_row_count(summary),
                );
                frame.render_widget(
                    coverage_detail_panel(summary, app.storage.selection),
                    chunks[1],
                );
            }
            CoverageStatus::Failed(error) => render_storage_error(frame, area, error),
        },
        StorageMode::Outline => match &app.storage.outline {
            OutlineStatus::Completed(summary) => {
                let chunks = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([Constraint::Min(7), Constraint::Length(8)])
                    .split(area);
                render_selectable_table(
                    frame,
                    chunks[0],
                    outline_table(summary),
                    app.storage.selection,
                    outline_row_count(summary),
                );
                frame.render_widget(
                    outline_detail_panel(summary, app.storage.selection),
                    chunks[1],
                );
            }
            OutlineStatus::Failed(error) => render_storage_error(frame, area, error),
        },
        StorageMode::Calls => match &app.storage.calls {
            CallResolutionStatus::Completed(summary) => {
                if area.height >= 20 {
                    let chunks = Layout::default()
                        .direction(Direction::Vertical)
                        .constraints([
                            Constraint::Min(7),
                            Constraint::Length(7),
                            Constraint::Length(9),
                        ])
                        .split(area);
                    render_selectable_table(
                        frame,
                        chunks[0],
                        call_resolution_table(summary),
                        app.storage.selection,
                        call_resolution_row_count(summary),
                    );
                    render_call_resolution_barchart(frame, chunks[1], summary);
                    frame.render_widget(
                        call_resolution_detail_panel(summary, app.storage.selection),
                        chunks[2],
                    );
                } else {
                    let chunks = Layout::default()
                        .direction(Direction::Vertical)
                        .constraints([Constraint::Min(7), Constraint::Length(9)])
                        .split(area);
                    render_selectable_table(
                        frame,
                        chunks[0],
                        call_resolution_table(summary),
                        app.storage.selection,
                        call_resolution_row_count(summary),
                    );
                    frame.render_widget(
                        call_resolution_detail_panel(summary, app.storage.selection),
                        chunks[1],
                    );
                }
            }
            CallResolutionStatus::Failed(error) => render_storage_error(frame, area, error),
        },
        StorageMode::Embeddings => match &app.storage.embeddings {
            EmbeddingCoverageStatus::Completed(summary) => {
                if area.height >= 20 {
                    let chunks = Layout::default()
                        .direction(Direction::Vertical)
                        .constraints([
                            Constraint::Min(7),
                            Constraint::Length(7),
                            Constraint::Length(9),
                        ])
                        .split(area);
                    render_selectable_table(
                        frame,
                        chunks[0],
                        embedding_coverage_table(summary),
                        app.storage.selection,
                        embedding_coverage_row_count(summary),
                    );
                    render_embedding_coverage_barchart(frame, chunks[1], summary);
                    frame.render_widget(
                        embedding_coverage_detail_panel(summary, app.storage.selection),
                        chunks[2],
                    );
                } else {
                    let chunks = Layout::default()
                        .direction(Direction::Vertical)
                        .constraints([Constraint::Min(7), Constraint::Length(9)])
                        .split(area);
                    render_selectable_table(
                        frame,
                        chunks[0],
                        embedding_coverage_table(summary),
                        app.storage.selection,
                        embedding_coverage_row_count(summary),
                    );
                    frame.render_widget(
                        embedding_coverage_detail_panel(summary, app.storage.selection),
                        chunks[1],
                    );
                }
            }
            EmbeddingCoverageStatus::Failed(error) => render_storage_error(frame, area, error),
        },
        StorageMode::Runs => match &app.storage.runs {
            IndexRunsTimelineStatus::Completed(summary) => {
                if area.height >= 20 {
                    let chunks = Layout::default()
                        .direction(Direction::Vertical)
                        .constraints([
                            Constraint::Min(7),
                            Constraint::Length(7),
                            Constraint::Length(9),
                        ])
                        .split(area);
                    render_selectable_table(
                        frame,
                        chunks[0],
                        index_runs_timeline_table(summary),
                        app.storage.selection,
                        index_runs_timeline_row_count(summary),
                    );
                    render_index_runs_barchart(frame, chunks[1], summary);
                    frame.render_widget(
                        index_runs_timeline_detail_panel(summary, app.storage.selection),
                        chunks[2],
                    );
                } else {
                    let chunks = Layout::default()
                        .direction(Direction::Vertical)
                        .constraints([Constraint::Min(7), Constraint::Length(9)])
                        .split(area);
                    render_selectable_table(
                        frame,
                        chunks[0],
                        index_runs_timeline_table(summary),
                        app.storage.selection,
                        index_runs_timeline_row_count(summary),
                    );
                    frame.render_widget(
                        index_runs_timeline_detail_panel(summary, app.storage.selection),
                        chunks[1],
                    );
                }
            }
            IndexRunsTimelineStatus::Failed(error) => render_storage_error(frame, area, error),
        },
        StorageMode::Freshness => match &app.storage.freshness {
            FreshnessStatus::Completed(summary) => {
                if area.height >= 20 {
                    let chunks = Layout::default()
                        .direction(Direction::Vertical)
                        .constraints([
                            Constraint::Min(7),
                            Constraint::Length(7),
                            Constraint::Length(9),
                        ])
                        .split(area);
                    render_selectable_table(
                        frame,
                        chunks[0],
                        freshness_table(summary),
                        app.storage.selection,
                        freshness_row_count(summary),
                    );
                    render_freshness_barchart(frame, chunks[1], summary);
                    frame.render_widget(
                        freshness_detail_panel(summary, app.storage.selection),
                        chunks[2],
                    );
                } else {
                    let chunks = Layout::default()
                        .direction(Direction::Vertical)
                        .constraints([Constraint::Min(7), Constraint::Length(9)])
                        .split(area);
                    render_selectable_table(
                        frame,
                        chunks[0],
                        freshness_table(summary),
                        app.storage.selection,
                        freshness_row_count(summary),
                    );
                    frame.render_widget(
                        freshness_detail_panel(summary, app.storage.selection),
                        chunks[1],
                    );
                }
            }
            FreshnessStatus::Failed(error) => render_storage_error(frame, area, error),
        },
        StorageMode::Neighborhood => match &app.storage.neighborhood {
            SemanticNeighborhoodStatus::Completed(summary) => {
                let chunks = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([Constraint::Min(7), Constraint::Length(9)])
                    .split(area);
                render_selectable_table(
                    frame,
                    chunks[0],
                    semantic_neighborhood_table(summary),
                    app.storage.selection,
                    semantic_neighborhood_row_count(summary),
                );
                frame.render_widget(
                    semantic_neighborhood_detail_panel(summary, app.storage.selection),
                    chunks[1],
                );
            }
            SemanticNeighborhoodStatus::Failed(error) => render_storage_error(frame, area, error),
        },
        StorageMode::Health => match &app.storage.health {
            CrossStoreHealthStatus::Completed(summary) => {
                let chunks = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([Constraint::Min(7), Constraint::Length(9)])
                    .split(area);
                render_selectable_table(
                    frame,
                    chunks[0],
                    cross_store_health_table(summary),
                    app.storage.selection,
                    cross_store_health_row_count(summary),
                );
                frame.render_widget(
                    cross_store_health_detail_panel(summary, app.storage.selection),
                    chunks[1],
                );
            }
            CrossStoreHealthStatus::Failed(error) => render_storage_error(frame, area, error),
        },
    }
}

fn render_storage_error(frame: &mut ratatui::Frame<'_>, area: Rect, error: &str) {
    render_line_panel(
        frame,
        area,
        "Storage Explorer",
        vec![Line::from(vec![
            status_span("failed", StatusTone::Error),
            Span::raw(" "),
            Span::raw(error),
        ])],
    );
}

fn render_diagnostics_panel(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    app: &App,
    report: &DiagnosticReport,
) {
    let detail_height = if app.diagnostics_details_expanded {
        8
    } else {
        5
    };
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(6), Constraint::Length(detail_height)])
        .split(area);
    render_selectable_table(
        frame,
        chunks[0],
        diagnostics_table(report),
        app.diagnostics_selection,
        report.checks.len(),
    );
    if let Some(check) = selected_diagnostic_check(report, app.diagnostics_selection) {
        frame.render_widget(
            diagnostic_detail_panel(check, app.diagnostics_details_expanded),
            chunks[1],
        );
    }
}

fn render_index_panel(frame: &mut ratatui::Frame<'_>, area: Rect, app: &App) {
    let (title, tone) = match app.screen {
        Screen::Dashboard => ("Indexing", StatusTone::Info),
        Screen::ConfirmIndex(_) => ("Confirm Indexing", StatusTone::Warning),
        Screen::ConfirmContinuous => ("Confirm Continuous Indexing", StatusTone::Warning),
        Screen::IndexRunning(_) => ("Indexing Running", StatusTone::Info),
        Screen::IndexCompleted(_) => ("Indexing Complete", StatusTone::Success),
        Screen::IndexFailed(_) => ("Indexing Failed", StatusTone::Error),
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(tone_style(tone))
        .title(Line::from(status_span(title, tone)));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if matches!(app.screen, Screen::IndexRunning(_)) {
        if inner.width >= 78 {
            let chunks = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Min(38), Constraint::Length(38)])
                .split(inner);
            let side_chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(15), Constraint::Length(3)])
                .split(chunks[1]);
            let panel = Paragraph::new(app.index_lines()).wrap(Wrap { trim: true });
            frame.render_widget(panel, chunks[0]);
            render_semantic_readiness_gauges(frame, side_chunks[0], &app.semantic_status);
            render_index_progress_gauge(frame, side_chunks[1], app.index_progress.as_ref());
        } else {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Min(5),
                    Constraint::Length(15),
                    Constraint::Length(3),
                ])
                .split(inner);
            let panel = Paragraph::new(app.index_lines()).wrap(Wrap { trim: true });
            frame.render_widget(panel, chunks[0]);
            render_semantic_readiness_gauges(frame, chunks[1], &app.semantic_status);
            render_index_progress_gauge(frame, chunks[2], app.index_progress.as_ref());
        }
    } else {
        if inner.width >= 78 {
            let chunks = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Min(38), Constraint::Length(38)])
                .split(inner);
            let panel = Paragraph::new(app.index_lines()).wrap(Wrap { trim: true });
            frame.render_widget(panel, chunks[0]);
            render_semantic_readiness_gauges(frame, chunks[1], &app.semantic_status);
        } else {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Min(5), Constraint::Length(15)])
                .split(inner);
            let panel = Paragraph::new(app.index_lines()).wrap(Wrap { trim: true });
            frame.render_widget(panel, chunks[0]);
            render_semantic_readiness_gauges(frame, chunks[1], &app.semantic_status);
        }
    }
}

fn render_index_progress_gauge(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    progress: Option<&IndexProgress>,
) {
    let gauge = Gauge::default()
        .block(Block::default().borders(Borders::ALL).title("Progress"))
        .gauge_style(readable_gauge_style(StatusTone::Info))
        .percent(progress_percent(progress))
        .label(progress_label(progress));
    frame.render_widget(gauge, area);
}

fn render_semantic_readiness_gauges(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    summary: &SemanticStatusSummary,
) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Min(7),
        ])
        .split(area);
    let fast_percent = layer_readiness_percent(&summary.fast);
    let quality_percent = layer_readiness_percent(&summary.quality);
    let fast_gauge = Gauge::default()
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("Fast Readiness"),
        )
        .gauge_style(readable_gauge_style(StatusTone::Success))
        .percent(fast_percent)
        .label(layer_readiness_label(
            "fast_ready",
            &summary.fast,
            fast_percent,
        ));
    let quality_gauge = Gauge::default()
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("Quality Readiness"),
        )
        .gauge_style(readable_gauge_style(semantic_quality_tone(summary)))
        .percent(quality_percent)
        .label(layer_readiness_label(
            "quality_ready",
            &summary.quality,
            quality_percent,
        ));
    frame.render_widget(fast_gauge, chunks[0]);
    frame.render_widget(quality_gauge, chunks[1]);
    render_semantic_job_barchart(frame, chunks[2], summary);
}

fn render_semantic_job_barchart(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    summary: &SemanticStatusSummary,
) {
    render_count_barchart(
        frame,
        area,
        "Fast/Quality Job States",
        semantic_job_bars(summary),
        StatusTone::Info,
    );
}

fn render_call_resolution_barchart(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    summary: &CallResolutionSummary,
) {
    render_count_barchart(
        frame,
        area,
        "Call Resolution Counts",
        call_resolution_bars(summary),
        StatusTone::Info,
    );
}

fn render_embedding_coverage_barchart(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    summary: &EmbeddingCoverageSummary,
) {
    render_count_barchart(
        frame,
        area,
        "Embedding Coverage Counts",
        embedding_coverage_bars(summary),
        StatusTone::Info,
    );
}

fn render_index_runs_barchart(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    summary: &IndexRunsTimelineSummary,
) {
    render_count_barchart(
        frame,
        area,
        "Index Run Outcomes",
        index_run_outcome_bars(summary),
        StatusTone::Info,
    );
}

fn render_freshness_barchart(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    summary: &FreshnessSummary,
) {
    render_count_barchart(
        frame,
        area,
        "Freshness States",
        freshness_bars(summary),
        StatusTone::Info,
    );
}

fn render_count_barchart(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    title: &'static str,
    bars: Vec<(&'static str, u64)>,
    tone: StatusTone,
) {
    let max = bars
        .iter()
        .map(|(_, value)| *value)
        .max()
        .unwrap_or(0)
        .max(1);
    let chart = BarChart::default()
        .block(panel_block(title, StatusTone::Dim))
        .data(bars.as_slice())
        .max(max)
        .bar_width(4)
        .bar_gap(1)
        .bar_style(tone_style(tone).add_modifier(Modifier::BOLD))
        .value_style(Style::new().fg(Color::White).add_modifier(Modifier::BOLD))
        .label_style(metadata_style());
    frame.render_widget(chart, area);
}

fn semantic_job_bars(summary: &SemanticStatusSummary) -> Vec<(&'static str, u64)> {
    let fast_pending = summary
        .fast
        .expected_chunks
        .saturating_sub(summary.fast.total_chunks);
    let progress = summary.quality_progress.as_ref();
    vec![
        ("fpen", fast_pending as u64),
        ("ffai", summary.fast.failed_chunks as u64),
        ("fsta", summary.fast.stale_chunks as u64),
        (
            "qpen",
            progress.map(|row| row.pending_jobs).unwrap_or(0) as u64,
        ),
        (
            "qrun",
            progress.map(|row| row.running_jobs).unwrap_or(0) as u64,
        ),
        (
            "qfai",
            progress.map(|row| row.failed_jobs).unwrap_or(0) as u64,
        ),
        (
            "qsta",
            progress.map(|row| row.skipped_stale_jobs).unwrap_or(0) as u64,
        ),
    ]
}

fn call_resolution_bars(summary: &CallResolutionSummary) -> Vec<(&'static str, u64)> {
    let mut resolved = 0;
    let mut unresolved = 0;
    let mut ambiguous = 0;
    let mut other = 0;
    for bucket in &summary.buckets {
        let count = bucket.call_count as u64;
        if bucket.resolution_status.starts_with("resolved") {
            resolved += count;
        } else if bucket.resolution_status == "unresolved" {
            unresolved += count;
        } else if bucket.resolution_status.contains("ambiguous") {
            ambiguous += count;
        } else {
            other += count;
        }
    }
    vec![
        ("res", resolved),
        ("unres", unresolved),
        ("amb", ambiguous),
        ("oth", other),
    ]
}

fn embedding_coverage_bars(summary: &EmbeddingCoverageSummary) -> Vec<(&'static str, u64)> {
    vec![
        ("total", summary.total_chunks as u64),
        ("emb", summary.embeddable_chunks as u64),
        ("vec", summary.vector_backed_chunks as u64),
        ("miss", summary.missing_vector_chunks as u64),
        ("excl", summary.excluded_chunks as u64),
    ]
}

fn index_run_outcome_bars(summary: &IndexRunsTimelineSummary) -> Vec<(&'static str, u64)> {
    let mut success = 0;
    let mut failed = 0;
    let mut running = 0;
    let mut other = 0;
    for run in &summary.runs {
        match run.status.as_str() {
            "success" | "complete" | "completed" => success += 1,
            "failed" | "error" => failed += 1,
            "running" | "started" | "pending" => running += 1,
            _ => other += 1,
        }
    }
    vec![
        ("ok", success),
        ("fail", failed),
        ("run", running),
        ("oth", other),
    ]
}

fn freshness_bars(summary: &FreshnessSummary) -> Vec<(&'static str, u64)> {
    vec![
        ("fresh", summary.count(EvidenceFreshness::Fresh) as u64),
        ("stale", summary.count(EvidenceFreshness::Stale) as u64),
        ("del", summary.count(EvidenceFreshness::Deleted) as u64),
        ("miss", summary.count(EvidenceFreshness::Missing) as u64),
        ("unk", summary.count(EvidenceFreshness::Unknown) as u64),
    ]
}

fn render_line_panel(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    title: &'static str,
    lines: Vec<Line<'_>>,
) {
    let line_count = lines.len();
    let panel = List::new(lines.into_iter().map(ListItem::new).collect::<Vec<_>>())
        .block(panel_block(title, StatusTone::Dim));
    frame.render_widget(panel, area);
    render_scrollbar(frame, area, 0, line_count);
}

fn render_mode_bar(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    title: &'static str,
    items: Vec<(&'static str, String, StatusTone)>,
) {
    let mut spans = Vec::new();
    for (index, (label, value, tone)) in items.into_iter().enumerate() {
        if index > 0 {
            spans.push(Span::styled(" | ", metadata_style()));
        }
        spans.push(Span::styled(
            format!("{label} "),
            Style::new().fg(Color::White).add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(
            value,
            tone_style(tone).add_modifier(Modifier::BOLD),
        ));
    }
    frame.render_widget(
        Paragraph::new(Line::from(spans))
            .wrap(Wrap { trim: true })
            .block(panel_block(title, StatusTone::Info)),
        area,
    );
}

fn evidence_result_chunks(area: Rect) -> std::rc::Rc<[Rect]> {
    Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(5)])
        .split(area)
}

fn render_evidence_mode_bar(frame: &mut ratatui::Frame<'_>, area: Rect, app: &App) {
    render_mode_bar(
        frame,
        area,
        "Impact Mode",
        vec![
            (
                "mode",
                app.evidence.mode.label().to_owned(),
                StatusTone::Info,
            ),
            (
                "input",
                if app.evidence.input.is_empty() {
                    "<empty>".to_owned()
                } else {
                    app.evidence.input.clone()
                },
                StatusTone::Dim,
            ),
        ],
    );
}

fn render_selectable_table(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    table: Table<'_>,
    selection: usize,
    row_count: usize,
) {
    let mut state = TableState::default();
    if row_count > 0 {
        state.select(Some(selection.min(row_count.saturating_sub(1))));
    }
    frame.render_stateful_widget(
        table
            .row_highlight_style(selected_row_style())
            .highlight_symbol("> "),
        area,
        &mut state,
    );
    render_scrollbar(frame, area, selection, row_count);
}

fn render_scrollbar(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    position: usize,
    content_length: usize,
) {
    let viewport_rows = scroll_viewport_rows(area);
    if content_length <= viewport_rows {
        return;
    }
    let mut state = ScrollbarState::new(content_length)
        .position(position.min(content_length.saturating_sub(1)))
        .viewport_content_length(viewport_rows);
    let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
        .thumb_symbol("#")
        .track_symbol(Some("."))
        .begin_symbol(None)
        .end_symbol(None)
        .thumb_style(tone_style(StatusTone::Info))
        .track_style(tone_style(StatusTone::Dim));
    frame.render_stateful_widget(
        scrollbar,
        area.inner(Margin {
            vertical: 1,
            horizontal: 0,
        }),
        &mut state,
    );
}

fn scroll_viewport_rows(area: Rect) -> usize {
    usize::from(area.height.saturating_sub(3).max(1))
}

fn refresh_due(last_refresh: Option<Instant>, now: Instant, interval: Duration) -> bool {
    last_refresh
        .map(|last_refresh| now.duration_since(last_refresh) >= interval)
        .unwrap_or(true)
}

fn collect_status_refresh(
    repo_input: &str,
    repository_id: &str,
    scope: StatusRefreshScope,
) -> Result<StatusRefreshSnapshot, String> {
    let store_config = StoreConfig::from_env();
    let sqlite = SqliteStore::open_read_only(&store_config).map_err(|error| error.to_string())?;
    let status = sqlite
        .repository_status(repository_id)
        .map_err(|error| error.to_string())?;
    let semantic_status = run_semantic_status(repo_input)?;

    Ok(StatusRefreshSnapshot {
        scope,
        status,
        semantic_status,
        explorer: if matches!(scope, StatusRefreshScope::Full) {
            Some(match run_storage_explorer(repo_input) {
                Ok(summary) => StorageStatus::Completed(summary),
                Err(error) => StorageStatus::Failed(error),
            })
        } else {
            None
        },
        coverage: if matches!(scope, StatusRefreshScope::Full) {
            Some(match run_index_coverage(repo_input) {
                Ok(summary) => CoverageStatus::Completed(summary),
                Err(error) => CoverageStatus::Failed(error),
            })
        } else {
            None
        },
        outline: if matches!(scope, StatusRefreshScope::Full) {
            Some(match run_symbol_outline(repo_input) {
                Ok(summary) => OutlineStatus::Completed(summary),
                Err(error) => OutlineStatus::Failed(error),
            })
        } else {
            None
        },
        calls: if matches!(scope, StatusRefreshScope::Full) {
            Some(match run_call_resolution(repo_input) {
                Ok(summary) => CallResolutionStatus::Completed(summary),
                Err(error) => CallResolutionStatus::Failed(error),
            })
        } else {
            None
        },
        embeddings: if matches!(scope, StatusRefreshScope::Full) {
            Some(match run_embedding_coverage(repo_input) {
                Ok(summary) => EmbeddingCoverageStatus::Completed(summary),
                Err(error) => EmbeddingCoverageStatus::Failed(error),
            })
        } else {
            None
        },
        runs: if matches!(scope, StatusRefreshScope::Full) {
            Some(match run_index_runs_timeline(repo_input) {
                Ok(summary) => IndexRunsTimelineStatus::Completed(summary),
                Err(error) => IndexRunsTimelineStatus::Failed(error),
            })
        } else {
            None
        },
        freshness: if matches!(scope, StatusRefreshScope::Full) {
            Some(match run_freshness_report(repo_input, None) {
                Ok(summary) => FreshnessStatus::Completed(Box::new(summary)),
                Err(error) => FreshnessStatus::Failed(error),
            })
        } else {
            None
        },
        neighborhood: if matches!(scope, StatusRefreshScope::Full) {
            Some(match run_semantic_neighborhood(repo_input) {
                Ok(summary) => SemanticNeighborhoodStatus::Completed(summary),
                Err(error) => SemanticNeighborhoodStatus::Failed(error),
            })
        } else {
            None
        },
        health: if matches!(scope, StatusRefreshScope::Full) {
            Some(match run_cross_store_health(repo_input) {
                Ok(summary) => CrossStoreHealthStatus::Completed(summary),
                Err(error) => CrossStoreHealthStatus::Failed(error),
            })
        } else {
            None
        },
    })
}

fn run_app<B: Backend>(terminal: &mut Terminal<B>, app: App) -> Result<(), String> {
    let mut app = app;
    loop {
        app.tick_animation();
        app.poll_index_job();
        app.poll_continuous_index();
        app.refresh_index_status_on_interval();
        app.poll_status_refresh();
        app.poll_diagnostics();
        app.poll_query();
        app.poll_graph();
        app.poll_evidence();
        render(terminal, &app)?;
        if !event::poll(EVENT_POLL_INTERVAL).map_err(|error| error.to_string())? {
            continue;
        }
        let Event::Key(key) = event::read().map_err(|error| error.to_string())? else {
            continue;
        };
        if key.kind == KeyEventKind::Press && app.handle_key(key.code) {
            return Ok(());
        }
    }
}

fn index_embedding(status: &RepositoryStatus) -> String {
    match (&status.embedding_model, status.embedding_dimension) {
        (Some(model), Some(dimension)) => format!("{model} ({dimension})"),
        (Some(model), None) => model.clone(),
        _ => "<none>".to_owned(),
    }
}

fn index_ref_label(status: &RepositoryStatus) -> String {
    match (&status.current_ref_kind, &status.current_ref_name) {
        (Some(kind), Some(name)) => format!("{kind}:{name}"),
        (Some(kind), None) => kind.clone(),
        _ => "<unknown>".to_owned(),
    }
}

fn index_freshness_span(last_indexed_at: Option<&str>) -> Span<'static> {
    if last_indexed_at.is_some() {
        status_span("indexed", StatusTone::Success)
    } else {
        status_span("never", StatusTone::Warning)
    }
}

fn index_embedding_status_span(status: &RepositoryStatus) -> Span<'static> {
    if status.embedding_model.is_some() {
        status_span("ready", StatusTone::Success)
    } else {
        status_span("none", StatusTone::Warning)
    }
}

fn semantic_status_tone(summary: &SemanticStatusSummary) -> StatusTone {
    match summary.quality_status {
        SemanticLayerStatus::QualityBlocked | SemanticLayerStatus::QualityFailed => {
            StatusTone::Error
        }
        SemanticLayerStatus::QualityPending
        | SemanticLayerStatus::QualityStale
        | SemanticLayerStatus::Missing => StatusTone::Warning,
        SemanticLayerStatus::QualityReady if summary.active_layer == SemanticLayer::Quality => {
            StatusTone::Success
        }
        SemanticLayerStatus::QualityReady => StatusTone::Warning,
        SemanticLayerStatus::FastReady => StatusTone::Success,
    }
}

fn semantic_quality_tone(summary: &SemanticStatusSummary) -> StatusTone {
    match summary.quality_status {
        SemanticLayerStatus::QualityReady => StatusTone::Success,
        SemanticLayerStatus::QualityBlocked | SemanticLayerStatus::QualityFailed => {
            StatusTone::Error
        }
        SemanticLayerStatus::FastReady => StatusTone::Dim,
        SemanticLayerStatus::Missing
        | SemanticLayerStatus::QualityPending
        | SemanticLayerStatus::QualityStale => StatusTone::Warning,
    }
}

fn semantic_status_mode(summary: &SemanticStatusSummary) -> String {
    format!(
        "{} {}",
        summary.active_layer.as_str(),
        summary.quality_status.as_str()
    )
}

fn semantic_status_state(summary: &SemanticStatusSummary) -> &'static str {
    if summary.fallback_reason.is_some() {
        "fallback"
    } else if summary.active_layer == SemanticLayer::Quality {
        "quality"
    } else {
        "fast"
    }
}

fn semantic_fallback_state(summary: &SemanticStatusSummary) -> &'static str {
    if summary.latest_quality_error.is_some() {
        "error"
    } else if summary.fallback_reason.is_some() {
        "fallback"
    } else {
        "ready"
    }
}

fn quality_progress_is_complete(progress: &QualityGenerationProgress) -> bool {
    progress.quality_eligible_chunks == progress.quality_embedded_chunks
        && progress.pending_jobs == 0
        && progress.running_jobs == 0
        && progress.failed_jobs == 0
        && progress.skipped_stale_jobs == 0
}

fn semantic_fallback_reason_for_status(
    active_layer: SemanticLayer,
    quality_status: SemanticLayerStatus,
    quality_complete: bool,
) -> Option<String> {
    if active_layer == SemanticLayer::Quality
        && quality_status == SemanticLayerStatus::QualityReady
        && quality_complete
    {
        return None;
    }
    match quality_status {
        SemanticLayerStatus::FastReady => None,
        SemanticLayerStatus::QualityReady => {
            Some("quality_ready_not_active_using_fast_layer".to_owned())
        }
        _ if !quality_complete => Some("quality_manifest_incomplete_using_fast_layer".to_owned()),
        _ => Some(format!(
            "quality_status_{}_using_fast_layer",
            quality_status.as_str()
        )),
    }
}

fn semantic_quality_jobs(summary: &SemanticStatusSummary) -> String {
    match &summary.quality_progress {
        Some(progress) => format!(
            "{}/{}/{}/{}",
            progress.pending_jobs,
            progress.running_jobs,
            progress.failed_jobs,
            progress.skipped_stale_jobs
        ),
        None => "<none>".to_owned(),
    }
}

fn semantic_status_summary_from_status(
    repository_id: &str,
    status: &RepositoryStatus,
) -> SemanticStatusSummary {
    let fast_model = status
        .embedding_model
        .as_deref()
        .unwrap_or("nomic-embed-text")
        .to_owned();
    let generation_id = status
        .embedding_model
        .as_ref()
        .map(|_| "sample-generation".to_owned());
    let quality_status = if status.embedding_model.is_some() {
        SemanticLayerStatus::FastReady
    } else {
        SemanticLayerStatus::Missing
    };
    SemanticStatusSummary {
        repository_id: repository_id.to_owned(),
        generation_id,
        active_layer: SemanticLayer::Fast,
        quality_status,
        fallback_reason: if status.embedding_model.is_some() {
            None
        } else {
            Some("semantic_generation_missing_using_fast_layer".to_owned())
        },
        fast: SemanticStatusLayerSummary {
            semantic_layer: SemanticLayer::Fast,
            embedding_model: fast_model.clone(),
            embedding_dimension: status.embedding_dimension,
            vector_table: vector_table_name(repository_id, &fast_model),
            current_chunks: if status.embedding_model.is_some() {
                status.chunks_indexed
            } else {
                0
            },
            stale_chunks: 0,
            blocked_chunks: 0,
            failed_chunks: 0,
            other_chunks: 0,
            total_chunks: if status.embedding_model.is_some() {
                status.chunks_indexed
            } else {
                0
            },
            expected_chunks: status.chunks_indexed,
            is_complete: status.embedding_model.is_some(),
        },
        quality: SemanticStatusLayerSummary {
            semantic_layer: SemanticLayer::Quality,
            embedding_model: "mxbai-embed-large".to_owned(),
            embedding_dimension: None,
            vector_table: vector_table_name(repository_id, "mxbai-embed-large"),
            current_chunks: 0,
            stale_chunks: 0,
            blocked_chunks: 0,
            failed_chunks: 0,
            other_chunks: 0,
            total_chunks: 0,
            expected_chunks: status.chunks_indexed,
            is_complete: false,
        },
        quality_progress: None,
        latest_quality_error: None,
        quality_enabled: true,
    }
}

fn storage_summary_from_status(
    repository_id: &str,
    status: &RepositoryStatus,
) -> StorageExplorerSummary {
    let embedding_model = status
        .embedding_model
        .as_deref()
        .unwrap_or("nomic-embed-text")
        .to_owned();
    StorageExplorerSummary {
        repository_id: repository_id.to_owned(),
        sqlite: SqliteStorageSummary {
            repositories: 1,
            files: status.files_indexed,
            chunks: status.chunks_indexed,
            symbols: status.symbols_indexed,
            calls: status.calls_indexed,
            index_runs: usize::from(status.embedding_model.is_some()),
        },
        vector: VectorStorageProjection {
            collection_name: vector_table_name(repository_id, &embedding_model),
            embedding_model,
            embedding_dimension: status.embedding_dimension,
            embeddable_chunks: status.chunks_indexed,
            vector_backed_chunks: if status.embedding_model.is_some() {
                status.chunks_indexed
            } else {
                0
            },
            excluded_chunks: 0,
            missing_vector_chunks: if status.embedding_model.is_some() {
                0
            } else {
                status.chunks_indexed
            },
        },
        warnings: vec![StorageHealthRow {
            status: if status.embedding_model.is_some() {
                StorageHealthStatus::Ok
            } else {
                StorageHealthStatus::Warning
            },
            label: if status.embedding_model.is_some() {
                "coverage_ok".to_owned()
            } else {
                "metadata_only".to_owned()
            },
            detail: if status.embedding_model.is_some() {
                "SQLite metadata and vector-backed chunk counts are aligned.".to_owned()
            } else {
                "Sample TUI state has structural metadata but no semantic embedding metadata."
                    .to_owned()
            },
        }],
    }
}

fn coverage_summary_from_status(
    repository_id: &str,
    status: &RepositoryStatus,
) -> IndexCoverageSummary {
    IndexCoverageSummary {
        repository_id: repository_id.to_owned(),
        files: vec![symdex_store::FileCoverageRow {
            path: "<sample>".to_owned(),
            language: "rust".to_owned(),
            chunks: status.chunks_indexed,
            symbols: status.symbols_indexed,
            calls: status.calls_indexed,
            embeddable_chunks: status.chunks_indexed,
            vector_backed_chunks: if status.embedding_model.is_some() {
                status.chunks_indexed
            } else {
                0
            },
            excluded_chunks: 0,
            status: if status.embedding_model.is_some() {
                FileCoverageStatus::Covered
            } else {
                FileCoverageStatus::MissingVector
            },
            detail: FileDetailSummary {
                chunks: Vec::new(),
                symbols: Vec::new(),
                calls: Vec::new(),
            },
        }],
    }
}

fn symbol_outline_summary_from_status(repository_id: &str) -> SymbolOutlineSummary {
    SymbolOutlineSummary {
        repository_id: repository_id.to_owned(),
        symbols: Vec::new(),
    }
}

fn call_resolution_summary_from_status(repository_id: &str) -> CallResolutionSummary {
    CallResolutionSummary {
        repository_id: repository_id.to_owned(),
        buckets: Vec::new(),
    }
}

fn embedding_coverage_summary_from_status(
    repository_id: &str,
    status: &RepositoryStatus,
) -> EmbeddingCoverageSummary {
    let embedding_model = status
        .embedding_model
        .as_deref()
        .unwrap_or("nomic-embed-text")
        .to_owned();
    let vector_backed_chunks = if status.embedding_model.is_some() {
        status.chunks_indexed
    } else {
        0
    };
    let missing_vector_chunks = status.chunks_indexed.saturating_sub(vector_backed_chunks);
    EmbeddingCoverageSummary {
        repository_id: repository_id.to_owned(),
        collection_name: vector_table_name(repository_id, &embedding_model),
        configured_embedding_model: "nomic-embed-text".to_owned(),
        embedding_model,
        embedding_dimension: status.embedding_dimension,
        total_chunks: status.chunks_indexed,
        embeddable_chunks: status.chunks_indexed,
        vector_backed_chunks,
        missing_vector_chunks,
        excluded_chunks: 0,
        latest_chunks_embedded: status
            .embedding_model
            .as_ref()
            .map(|_| vector_backed_chunks),
        exclusion_reasons: Vec::new(),
        health: vec![StorageHealthRow {
            status: if missing_vector_chunks == 0 {
                StorageHealthStatus::Ok
            } else {
                StorageHealthStatus::Warning
            },
            label: if missing_vector_chunks == 0 {
                "embedding_coverage_ok".to_owned()
            } else {
                "missing_vectors".to_owned()
            },
            detail: if missing_vector_chunks == 0 {
                "Sample TUI state has aligned embedding coverage.".to_owned()
            } else {
                format!("{missing_vector_chunks} sample chunks have no vector point metadata.")
            },
        }],
    }
}

fn index_runs_timeline_summary_from_status(
    repository_id: &str,
    status: &RepositoryStatus,
) -> IndexRunsTimelineSummary {
    let runs = status
        .embedding_model
        .as_ref()
        .map(|model| {
            vec![IndexRunTimelineRow {
                id: "sample-run".to_owned(),
                started_at: status
                    .last_indexed_at
                    .clone()
                    .unwrap_or_else(|| "<unknown>".to_owned()),
                finished_at: status.last_indexed_at.clone(),
                status: "success".to_owned(),
                embedding_model: model.clone(),
                embedding_dimension: status.embedding_dimension,
                files_seen: status.files_indexed,
                files_indexed: status.files_indexed,
                chunks_embedded: status.chunks_indexed,
                error_summary: None,
                run_kind: "semantic".to_owned(),
            }]
        })
        .unwrap_or_default();
    IndexRunsTimelineSummary {
        repository_id: repository_id.to_owned(),
        runs,
    }
}

fn freshness_summary_from_status(repository_id: &str) -> FreshnessSummary {
    FreshnessSummary {
        repository_id: repository_id.to_owned(),
        symbol_query: None,
        files: Vec::new(),
        focus_symbols: Vec::new(),
        context_pack: None,
    }
}

fn semantic_neighborhood_summary_from_status(
    repository_id: &str,
    status: &RepositoryStatus,
) -> SemanticNeighborhoodSummary {
    let embedding_model = status
        .embedding_model
        .as_deref()
        .unwrap_or("nomic-embed-text")
        .to_owned();
    let rows = if status.embedding_model.is_some() && status.chunks_indexed > 0 {
        vec![SemanticNeighborhoodRow {
            vector_point_id: "sample-point".to_owned(),
            path: "<sample>".to_owned(),
            start_line: 1,
            end_line: 1,
            symbol_name: None,
            chunk_kind: "metadata".to_owned(),
            language: "rust".to_owned(),
            score: None,
            text_hash: "<sample>".to_owned(),
        }]
    } else {
        Vec::new()
    };
    SemanticNeighborhoodSummary {
        repository_id: repository_id.to_owned(),
        collection_name: vector_table_name(repository_id, &embedding_model),
        embedding_model,
        health: vec![StorageHealthRow {
            status: if rows.is_empty() {
                StorageHealthStatus::Warning
            } else {
                StorageHealthStatus::Ok
            },
            label: if rows.is_empty() {
                "no_vector_payloads".to_owned()
            } else {
                "metadata_only".to_owned()
            },
            detail: if rows.is_empty() {
                "Sample TUI state has no vector-backed payload metadata.".to_owned()
            } else {
                "Sample TUI state has metadata-only semantic payload rows.".to_owned()
            },
        }],
        rows,
    }
}

fn cross_store_health_summary_from_status(
    repository_id: &str,
    status: &RepositoryStatus,
) -> CrossStoreHealthSummary {
    let embedding_model = status
        .embedding_model
        .as_deref()
        .unwrap_or("nomic-embed-text")
        .to_owned();
    let mut rows = Vec::new();
    if status.embedding_model.is_none() && status.chunks_indexed > 0 {
        rows.push(StorageHealthRow {
            status: StorageHealthStatus::Error,
            label: "missing_collection".to_owned(),
            detail: "Sample state has chunks but no semantic collection metadata.".to_owned(),
        });
    }
    if rows.is_empty() {
        rows.push(StorageHealthRow {
            status: StorageHealthStatus::Ok,
            label: "cross_store_ok".to_owned(),
            detail: "Sample SQLite and sqlite-vec projection metadata are aligned.".to_owned(),
        });
    }
    CrossStoreHealthSummary {
        repository_id: repository_id.to_owned(),
        collection_name: vector_table_name(repository_id, &embedding_model),
        rows,
    }
}

fn summary_lines(summary: &IndexSummary) -> Vec<Line<'static>> {
    let mut lines = vec![
        Line::from(format!("Files seen: {}", summary.files_seen)),
        Line::from(format!(
            "Files skipped unchanged: {}",
            summary.files_skipped_unchanged
        )),
        Line::from(format!(
            "SQLite indexed: files={} chunks={} symbols={} calls={}",
            summary.sqlite_files_indexed,
            summary.sqlite_chunks_indexed,
            summary.sqlite_symbols_indexed,
            summary.sqlite_calls_indexed
        )),
        Line::from(format!(
            "Secret-excluded chunks: {}",
            summary.chunks_excluded_from_embedding
        )),
    ];
    match &summary.rust_analyzer {
        RustAnalyzerEnrichmentSummary::Disabled { .. } => {
            lines.push(Line::from("Rust-analyzer enrichment: disabled"));
        }
        RustAnalyzerEnrichmentSummary::NotReady { reason, .. } => {
            lines.push(Line::from(format!(
                "Rust-analyzer enrichment: not ready ({reason})"
            )));
        }
        RustAnalyzerEnrichmentSummary::SkippedNoRustFiles { .. } => {
            lines.push(Line::from(
                "Rust-analyzer enrichment: skipped (no Rust files)",
            ));
        }
        RustAnalyzerEnrichmentSummary::Planned {
            eligible_files,
            eligible_symbols,
            eligible_calls,
            ..
        } => {
            lines.push(Line::from(format!(
                "Rust-analyzer enrichment: planned files={eligible_files} symbols={eligible_symbols} calls={eligible_calls}"
            )));
        }
    }
    match &summary.embedding {
        EmbeddingSummary::SkippedOffline => {
            lines.push(Line::from("Embedding: skipped (--offline)"));
        }
        EmbeddingSummary::SkippedNoChunks => {
            lines.push(Line::from("Embedding: skipped (no chunks)"));
        }
        EmbeddingSummary::Completed {
            model,
            dimension,
            chunks_embedded,
            ..
        } => {
            lines.push(Line::from(format!(
                "Embedding model: {model} ({dimension})"
            )));
            lines.push(Line::from(format!("Chunks embedded: {chunks_embedded}")));
        }
    }
    lines
}

fn diagnostic_report_lines(report: &DiagnosticReport) -> Vec<Line<'_>> {
    let mut lines = vec![
        Line::from(vec![
            Span::styled("Workspace: ", Style::new().add_modifier(Modifier::BOLD)),
            Span::raw(report.workspace.as_str()),
        ]),
        Line::from(vec![
            Span::styled("SQLite: ", Style::new().add_modifier(Modifier::BOLD)),
            Span::raw(report.sqlite_path.as_str()),
        ]),
        Line::from(vec![
            Span::styled("sqlite-vec: ", Style::new().add_modifier(Modifier::BOLD)),
            Span::raw(report.vector_store.as_str()),
        ]),
        Line::from(vec![
            Span::styled("Ollama: ", Style::new().add_modifier(Modifier::BOLD)),
            Span::raw(report.ollama_url.as_str()),
        ]),
        Line::from(vec![
            Span::styled(
                "Embedding model: ",
                Style::new().add_modifier(Modifier::BOLD),
            ),
            Span::raw(report.embed_model.as_str()),
        ]),
        Line::from(""),
    ];
    lines.extend(report.checks.iter().map(diagnostic_check_line));
    lines
}

fn diagnostic_check_line(check: &DiagnosticCheck) -> Line<'_> {
    let (status, tone) = diagnostic_status(check.state);
    let detail = if check.message.is_empty() {
        String::new()
    } else {
        format!(" {}", check.message)
    };
    Line::from(vec![
        Span::styled(
            format!("{}: ", check.label),
            Style::new().add_modifier(Modifier::BOLD),
        ),
        status_span(status, tone),
        Span::raw(detail),
    ])
}

fn query_result_lines(result: &QueryResult) -> Vec<Line<'_>> {
    match result {
        QueryResult::Symbol(summary) => {
            let mut lines = vec![
                Line::from(format!("Symbols: {}", summary.symbols.len())),
                Line::from(format!("Query: {}", summary.query)),
            ];
            if summary.symbols.is_empty() {
                lines.push(Line::from("No symbols matched."));
                return lines;
            }
            lines.extend(summary.symbols.iter().take(10).map(|symbol| {
                Line::from(format!(
                    "{} {} {}:{}-{}",
                    symbol.kind,
                    symbol.qualified_name,
                    symbol.path,
                    symbol.start_line,
                    symbol.end_line
                ))
            }));
            lines
        }
        QueryResult::Semantic(summary) => {
            let mut lines = vec![
                Line::from(format!("Semantic results: {}", summary.results.len())),
                Line::from(format!("Collection: {}", summary.vector_table)),
            ];
            if summary.results.is_empty() {
                lines.push(Line::from("No semantic matches returned."));
                return lines;
            }
            lines.extend(summary.results.iter().take(10).map(|result| {
                Line::from(format!(
                    "{:.4} {}:{}-{} {}",
                    result.score,
                    result.path,
                    result.start_line,
                    result.end_line,
                    result.symbol_name.as_deref().unwrap_or("<none>")
                ))
            }));
            lines
        }
    }
}

fn call_graph_lines(summary: &CallGraphSummary) -> Vec<Line<'_>> {
    let mut lines = vec![
        Line::from(format!(
            "{}: {}",
            summary.direction.label(),
            summary.rows.len()
        )),
        Line::from(format!("Symbol query: {}", summary.query)),
    ];
    if summary.rows.is_empty() {
        lines.push(Line::from("No direct call relationships matched."));
        return lines;
    }

    lines.extend(summary.rows.iter().take(12).map(|row| {
        Line::from(format!(
            "{} conf={:.2} {}:{}-{} callee={} status={}",
            row.symbol_qualified_name
                .as_deref()
                .unwrap_or("<unresolved>"),
            row.confidence,
            row.path.as_deref().unwrap_or("<unknown>"),
            row.start_line.unwrap_or(0),
            row.end_line.unwrap_or(0),
            row.callee_text,
            row.resolution_status
        ))
    }));
    lines
}

fn evidence_result_lines(result: &EvidenceResult) -> Vec<Line<'_>> {
    match result {
        EvidenceResult::Impact(summary) => impact_lines(summary),
        EvidenceResult::CallPath(summary) => call_path_lines(summary),
        EvidenceResult::ContextPack(pack) => context_pack_lines(pack),
        EvidenceResult::DebugContext(pack) => debug_context_lines(pack),
    }
}

fn impact_lines(summary: &ImpactSummary) -> Vec<Line<'_>> {
    let mut lines = vec![
        Line::from(format!("Impact query: {}", summary.query)),
        Line::from(format!("Direct callers: {}", summary.direct_callers.len())),
    ];
    lines.extend(
        summary
            .direct_callers
            .iter()
            .take(5)
            .map(|evidence| compact_call_line(&evidence.row)),
    );
    lines.push(Line::from(format!(
        "Direct callees: {}",
        summary.direct_callees.len()
    )));
    lines.extend(
        summary
            .direct_callees
            .iter()
            .take(5)
            .map(|evidence| compact_call_line(&evidence.row)),
    );
    lines.push(Line::from(format!(
        "Transitive callers: {} Transitive callees: {} Related files: {}",
        summary.transitive_callers.len(),
        summary.transitive_callees.len(),
        summary.related_files.len()
    )));
    if summary.direct_callers.is_empty() && summary.direct_callees.is_empty() {
        lines.push(Line::from("No direct impact relationships matched."));
    }
    lines
}

fn call_path_lines(summary: &CallPathSummary) -> Vec<Line<'_>> {
    let mut lines = vec![
        Line::from(format!("Call path source: {}", summary.source_query)),
        Line::from(format!("Target: {}", summary.target_query)),
        Line::from(format!(
            "Paths: {} max_depth: {}",
            summary.paths.len(),
            summary.max_depth
        )),
    ];
    for path in summary.paths.iter().take(4) {
        lines.push(Line::from(format!(
            "hops={} min_confidence={:.2} terminal_status={}",
            path.hops, path.min_confidence, path.terminal_resolution_status
        )));
    }
    if summary.paths.is_empty() {
        lines.push(Line::from("No bounded call paths matched."));
    }
    lines
}

fn parse_call_path_input(input: &str) -> Result<(String, String), String> {
    if let Some((source, target)) = input.split_once("->") {
        let source = source.trim();
        let target = target.trim();
        if !source.is_empty() && !target.is_empty() {
            return Ok((source.to_owned(), target.to_owned()));
        }
    }
    let mut parts = input.split_whitespace();
    let Some(source) = parts.next() else {
        return Err("call path requires source and target".to_owned());
    };
    let Some(target) = parts.next() else {
        return Err("call path requires source and target".to_owned());
    };
    if parts.next().is_some() {
        return Err("call path input must be 'source -> target'".to_owned());
    }
    Ok((source.to_owned(), target.to_owned()))
}

fn context_pack_lines(pack: &ContextPack) -> Vec<Line<'_>> {
    let mut lines = vec![
        Line::from(format!("Format: {}", pack.format)),
        Line::from(format!("Query: {}", pack.query)),
        Line::from(format!("Focus symbols: {}", pack.focus_symbols.len())),
    ];
    lines.extend(pack.focus_symbols.iter().take(5).map(|symbol| {
        Line::from(format!(
            "{} {} {}:{}-{}",
            symbol.kind, symbol.qualified_name, symbol.path, symbol.start_line, symbol.end_line
        ))
    }));
    lines.push(Line::from(format!(
        "Callers: {} Callees: {}",
        pack.direct_callers.len(),
        pack.direct_callees.len()
    )));
    lines.push(Line::from(format!("Files: {}", pack.files.len())));
    lines.extend(
        pack.files
            .iter()
            .take(6)
            .map(|file| Line::from(file.clone())),
    );
    lines.push(Line::from(format!(
        "Limits: symbols={} callers={} callees={}",
        pack.limits.max_symbols, pack.limits.max_callers, pack.limits.max_callees
    )));
    lines.extend(
        pack.notes
            .iter()
            .map(|note| Line::from(format!("Note: {note}"))),
    );
    lines
}

fn debug_context_lines(pack: &DebugContextPack) -> Vec<Line<'_>> {
    let mut lines = vec![
        Line::from(format!("Format: {}", pack.format)),
        Line::from(format!("Frames: {}", pack.frames.len())),
        Line::from(format!(
            "Call paths between frames: {}",
            pack.call_paths_between_frames.len()
        )),
        Line::from(format!("Likely tests: {}", pack.likely_tests.len())),
    ];
    lines.extend(pack.frames.iter().take(5).map(|frame| {
        let symbol = frame
            .matched_symbols
            .first()
            .map(|symbol| symbol.qualified_name.as_str())
            .or(frame.frame.symbol.as_deref())
            .unwrap_or("<unmapped>");
        Line::from(format!(
            "Frame #{} {} {} freshness={} path={}",
            frame.frame.ordinal,
            if frame.matched {
                "matched"
            } else {
                "unmatched"
            },
            symbol,
            frame.file_freshness.label(),
            frame
                .normalized_path
                .as_deref()
                .or(frame.frame.path.as_deref())
                .unwrap_or("<unknown>")
        ))
    }));
    lines.extend(pack.call_paths_between_frames.iter().take(4).map(|path| {
        Line::from(format!(
            "Path frames {}->{} {} -> {} paths={}",
            path.from_frame,
            path.to_frame,
            path.from_symbol,
            path.to_symbol,
            path.paths.len()
        ))
    }));
    lines.extend(
        pack.likely_tests
            .iter()
            .take(5)
            .map(|test| Line::from(format!("Likely test: {test}"))),
    );
    lines.push(Line::from(format!(
        "Limits: frames={} symbols/frame={} calls/frame={} paths={}",
        pack.limits.max_frames,
        pack.limits.max_symbols_per_frame,
        pack.limits.max_calls_per_frame,
        pack.limits.max_call_paths_between_frames
    )));
    lines.extend(
        pack.notes
            .iter()
            .map(|note| Line::from(format!("Note: {note}"))),
    );
    lines
}

fn compact_call_line(row: &symdex_store::CallSearchRow) -> Line<'_> {
    Line::from(format!(
        "{} conf={:.2} {}:{}-{} callee={} status={}",
        row.symbol_qualified_name
            .as_deref()
            .unwrap_or("<unresolved>"),
        row.confidence,
        row.path.as_deref().unwrap_or("<unknown>"),
        row.start_line.unwrap_or(0),
        row.end_line.unwrap_or(0),
        row.callee_text,
        row.resolution_status
    ))
}

fn diagnostics_table(report: &DiagnosticReport) -> Table<'_> {
    let rows = report.checks.iter().map(|check| {
        let (status, tone) = diagnostic_status(check.state);
        Row::new(vec![
            Cell::from(check.label.as_str()),
            Cell::from(status_span(status, tone)),
            Cell::from(check.message.as_str()),
        ])
    });
    Table::new(
        rows,
        [
            Constraint::Percentage(34),
            Constraint::Length(13),
            Constraint::Percentage(50),
        ],
    )
    .header(table_header(["Check", "State", "Detail"]))
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title(format!("Doctor Diagnostics | {}", report.workspace)),
    )
    .column_spacing(1)
}

fn index_counts_table(app: &App) -> Table<'_> {
    let rows = [
        ("Files indexed", app.status.files_indexed),
        ("Chunks indexed", app.status.chunks_indexed),
        ("Symbols indexed", app.status.symbols_indexed),
        ("Calls indexed", app.status.calls_indexed),
    ]
    .into_iter()
    .map(|(label, value)| {
        Row::new(vec![
            Cell::from(label),
            Cell::from(value.to_string()).style(Style::new().fg(Color::Green)),
        ])
    });

    Table::new(
        rows,
        [Constraint::Percentage(62), Constraint::Percentage(30)],
    )
    .header(table_header(["Index", "Count"]))
    .column_spacing(1)
}

fn local_services_table(app: &App) -> Table<'_> {
    let rows = vec![
        service_row("SQLite", "local", app.sqlite_path.as_str()),
        service_row("sqlite-vec", "local", app.vector_store_label.as_str()),
        service_row("Ollama", "local", app.ollama_url.as_str()),
        Row::new(vec![
            Cell::from("Embedding"),
            Cell::from(index_embedding_status_span(&app.status)),
            Cell::from(index_embedding(&app.status)),
        ]),
        Row::new(vec![
            Cell::from("Semantic"),
            Cell::from(status_span(
                app.semantic_status.active_layer.as_str(),
                semantic_status_tone(&app.semantic_status),
            )),
            Cell::from(format!(
                "model={} quality={} qjobs={}",
                app.embed_model.as_str(),
                app.semantic_status.quality_status.as_str(),
                semantic_quality_jobs(&app.semantic_status)
            )),
        ]),
    ];

    Table::new(
        rows,
        [
            Constraint::Length(9),
            Constraint::Length(7),
            Constraint::Percentage(64),
        ],
    )
    .header(table_header(["Service", "State", "Target"]))
    .column_spacing(1)
}

fn storage_table(summary: &StorageExplorerSummary) -> Table<'_> {
    let rows = storage_rows(summary).into_iter().map(|row| {
        Row::new(vec![
            Cell::from(row.layer),
            Cell::from(row.metric),
            Cell::from(row.value),
            Cell::from(status_span(row.status, row.tone)),
        ])
    });

    Table::new(
        rows,
        [
            Constraint::Length(8),
            Constraint::Percentage(32),
            Constraint::Percentage(42),
            Constraint::Length(14),
        ],
    )
    .header(table_header(["Layer", "Metric", "Value", "Status"]))
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title(format!("Storage Explorer | {}", summary.repository_id)),
    )
    .column_spacing(1)
}

fn storage_detail_panel(summary: &StorageExplorerSummary, selection: usize) -> Paragraph<'_> {
    let rows = storage_rows(summary);
    let row = rows.get(selection.min(rows.len().saturating_sub(1)));
    let mut lines = Vec::new();
    if let Some(row) = row {
        lines.push(Line::from(vec![
            Span::styled("Selected: ", Style::new().add_modifier(Modifier::BOLD)),
            Span::raw(row.layer),
            Span::raw(" / "),
            Span::raw(row.metric),
        ]));
        lines.push(Line::from(vec![
            Span::styled("Status: ", Style::new().add_modifier(Modifier::BOLD)),
            status_span(row.status, row.tone),
        ]));
        lines.push(Line::from(vec![
            Span::styled("Detail: ", Style::new().add_modifier(Modifier::BOLD)),
            Span::raw(row.detail),
        ]));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(vec![Span::styled(
        "Health: ",
        Style::new().add_modifier(Modifier::BOLD),
    )]));
    lines.extend(summary.warnings.iter().take(3).map(|warning| {
        let tone = storage_health_tone(warning.status);
        Line::from(vec![
            status_span(warning.label.as_str(), tone),
            Span::raw(" "),
            Span::raw(warning.detail.as_str()),
        ])
    }));

    Paragraph::new(lines).wrap(Wrap { trim: true }).block(
        Block::default()
            .borders(Borders::ALL)
            .title("Storage Detail"),
    )
}

fn storage_rows(summary: &StorageExplorerSummary) -> Vec<StorageDisplayRow> {
    let sqlite_status = if summary.sqlite.repositories == 0 {
        ("missing", StatusTone::Error)
    } else {
        ("ok", StatusTone::Success)
    };
    let sqlite_vec_status = if summary.vector.missing_vector_chunks > 0 {
        ("missing-vector", StatusTone::Warning)
    } else if summary.vector.vector_backed_chunks > 0 {
        ("covered", StatusTone::Success)
    } else {
        ("metadata-only", StatusTone::Warning)
    };

    vec![
        storage_row(
            "SQLite",
            "repositories",
            summary.sqlite.repositories.to_string(),
            sqlite_status,
            "Repository rows for the selected root.",
        ),
        storage_row(
            "SQLite",
            "files",
            summary.sqlite.files.to_string(),
            ("indexed", StatusTone::Success),
            "Indexed file rows in SQLite.",
        ),
        storage_row(
            "SQLite",
            "chunks",
            summary.sqlite.chunks.to_string(),
            ("indexed", StatusTone::Success),
            "Syntax-aware chunk metadata rows.",
        ),
        storage_row(
            "SQLite",
            "symbols",
            summary.sqlite.symbols.to_string(),
            ("indexed", StatusTone::Success),
            "Extracted symbol rows with line ranges.",
        ),
        storage_row(
            "SQLite",
            "calls",
            summary.sqlite.calls.to_string(),
            ("indexed", StatusTone::Success),
            "Call edge rows with confidence and resolution status.",
        ),
        storage_row(
            "SQLite",
            "index runs",
            summary.sqlite.index_runs.to_string(),
            if summary.sqlite.index_runs == 0 {
                ("missing", StatusTone::Warning)
            } else {
                ("recorded", StatusTone::Success)
            },
            "Historical index run metadata.",
        ),
        storage_row(
            "sqlite-vec",
            "collection",
            summary.vector.collection_name.clone(),
            sqlite_vec_status,
            "Expected local vector collection for the selected repository and model.",
        ),
        storage_row(
            "sqlite-vec",
            "model",
            summary.vector.embedding_model.clone(),
            ("metadata", StatusTone::Info),
            "Embedding model used to derive the collection name.",
        ),
        storage_row(
            "sqlite-vec",
            "dimension",
            summary
                .vector
                .embedding_dimension
                .map(|dimension| dimension.to_string())
                .unwrap_or_else(|| "<unknown>".to_owned()),
            if summary.vector.embedding_dimension.is_some() {
                ("recorded", StatusTone::Success)
            } else {
                ("unknown", StatusTone::Warning)
            },
            "Latest recorded vector dimension for successful semantic indexing.",
        ),
        storage_row(
            "sqlite-vec",
            "embeddable",
            summary.vector.embeddable_chunks.to_string(),
            ("metadata", StatusTone::Info),
            "Chunks eligible for semantic embedding.",
        ),
        storage_row(
            "sqlite-vec",
            "vector-backed",
            summary.vector.vector_backed_chunks.to_string(),
            sqlite_vec_status,
            "SQLite chunks with sqlite-vec point IDs.",
        ),
        storage_row(
            "sqlite-vec",
            "excluded",
            summary.vector.excluded_chunks.to_string(),
            if summary.vector.excluded_chunks == 0 {
                ("none", StatusTone::Success)
            } else {
                ("metadata-only", StatusTone::Warning)
            },
            "Chunks intentionally excluded from embeddings.",
        ),
        storage_row(
            "sqlite-vec",
            "missing vectors",
            summary.vector.missing_vector_chunks.to_string(),
            if summary.vector.missing_vector_chunks == 0 {
                ("ok", StatusTone::Success)
            } else {
                ("warning", StatusTone::Warning)
            },
            "Embeddable chunks without recorded sqlite-vec point IDs.",
        ),
    ]
}

fn storage_row(
    layer: &'static str,
    metric: &'static str,
    value: String,
    status: (&'static str, StatusTone),
    detail: &'static str,
) -> StorageDisplayRow {
    StorageDisplayRow {
        layer,
        metric,
        value,
        status: status.0,
        tone: status.1,
        detail,
    }
}

fn storage_row_count(summary: &StorageExplorerSummary) -> usize {
    storage_rows(summary).len()
}

fn storage_health_tone(status: StorageHealthStatus) -> StatusTone {
    match status {
        StorageHealthStatus::Ok => StatusTone::Success,
        StorageHealthStatus::Warning => StatusTone::Warning,
        StorageHealthStatus::Error => StatusTone::Error,
    }
}

fn coverage_table(summary: &IndexCoverageSummary) -> Table<'_> {
    let rows = summary.files.iter().map(|file| {
        let (label, tone) = file_coverage_status(file.status);
        Row::new(vec![
            Cell::from(file.path.as_str()),
            Cell::from(file.chunks.to_string()),
            Cell::from(file.symbols.to_string()),
            Cell::from(file.calls.to_string()),
            Cell::from(file.vector_backed_chunks.to_string()),
            Cell::from(file.excluded_chunks.to_string()),
            Cell::from(status_span(label, tone)),
        ])
    });

    Table::new(
        rows,
        [
            Constraint::Percentage(34),
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Length(4),
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Length(12),
        ],
    )
    .header(table_header([
        "Path", "Chk", "Sym", "Call", "Vec", "Ex", "Status",
    ]))
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title(format!("Index Coverage | Files: {}", summary.files.len())),
    )
    .column_spacing(1)
}

fn coverage_detail_panel(summary: &IndexCoverageSummary, selection: usize) -> Paragraph<'_> {
    let Some(file) = selected_coverage_row(summary, selection) else {
        return Paragraph::new(vec![Line::from("No indexed files found.")])
            .wrap(Wrap { trim: true })
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("File Coverage"),
            );
    };
    let (label, tone) = file_coverage_status(file.status);
    let mut lines = vec![
        Line::from(vec![
            Span::styled("File: ", Style::new().add_modifier(Modifier::BOLD)),
            Span::raw(file.path.as_str()),
        ]),
        Line::from(vec![
            Span::styled("Status: ", Style::new().add_modifier(Modifier::BOLD)),
            status_span(label, tone),
            Span::raw(" "),
            Span::raw(file_coverage_detail(file)),
        ]),
        Line::from(vec![
            Span::styled("SQLite: ", Style::new().add_modifier(Modifier::BOLD)),
            Span::raw(format!(
                "chunks={} symbols={} calls={}",
                file.chunks, file.symbols, file.calls
            )),
        ]),
        Line::from(vec![
            Span::styled(
                "sqlite-vec projection: ",
                Style::new().add_modifier(Modifier::BOLD),
            ),
            Span::raw(format!(
                "embeddable={} vector-backed={} excluded={}",
                file.embeddable_chunks, file.vector_backed_chunks, file.excluded_chunks
            )),
        ]),
    ];
    lines.push(Line::from(vec![
        Span::styled("Chunks: ", Style::new().add_modifier(Modifier::BOLD)),
        Span::raw(file_chunk_detail_summary(file)),
    ]));
    lines.push(Line::from(vec![
        Span::styled("Symbols: ", Style::new().add_modifier(Modifier::BOLD)),
        Span::raw(file_symbol_detail_summary(file)),
    ]));
    lines.push(Line::from(vec![
        Span::styled("Calls: ", Style::new().add_modifier(Modifier::BOLD)),
        Span::raw(file_call_detail_summary(file)),
    ]));
    Paragraph::new(lines).wrap(Wrap { trim: true }).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(tone_style(tone))
            .title("File Coverage"),
    )
}

fn coverage_row_count(summary: &IndexCoverageSummary) -> usize {
    summary.files.len()
}

fn selected_coverage_row(
    summary: &IndexCoverageSummary,
    selection: usize,
) -> Option<&symdex_store::FileCoverageRow> {
    summary
        .files
        .get(selection.min(summary.files.len().saturating_sub(1)))
}

fn file_coverage_status(status: FileCoverageStatus) -> (&'static str, StatusTone) {
    match status {
        FileCoverageStatus::Covered => ("covered", StatusTone::Success),
        FileCoverageStatus::MetadataOnly => ("metadata-only", StatusTone::Warning),
        FileCoverageStatus::Excluded => ("excluded", StatusTone::Warning),
        FileCoverageStatus::MissingVector => ("missing-vector", StatusTone::Warning),
    }
}

fn file_coverage_detail(file: &symdex_store::FileCoverageRow) -> &'static str {
    match file.status {
        FileCoverageStatus::Covered => "all embeddable chunks have vector metadata.",
        FileCoverageStatus::MetadataOnly => "file has structural metadata only.",
        FileCoverageStatus::Excluded => "all chunks are intentionally excluded from embedding.",
        FileCoverageStatus::MissingVector => {
            "some embeddable chunks are missing recorded vector metadata."
        }
    }
}

fn file_chunk_detail_summary(file: &symdex_store::FileCoverageRow) -> String {
    if file.detail.chunks.is_empty() {
        return "<none>".to_owned();
    }
    file.detail
        .chunks
        .iter()
        .take(2)
        .map(|chunk| {
            let (label, _) = chunk_vector_status(chunk.vector_status);
            let exclusion = chunk
                .excluded_reason
                .as_deref()
                .map(|reason| format!(" reason={reason}"))
                .unwrap_or_default();
            format!(
                "{} {} {}-{} symbol={}{}",
                chunk.kind,
                label,
                chunk.start_line,
                chunk.end_line,
                chunk.symbol.as_deref().unwrap_or("<none>"),
                exclusion
            )
        })
        .collect::<Vec<_>>()
        .join(" | ")
}

fn file_symbol_detail_summary(file: &symdex_store::FileCoverageRow) -> String {
    if file.detail.symbols.is_empty() {
        return "<none>".to_owned();
    }
    file.detail
        .symbols
        .iter()
        .take(2)
        .map(|symbol| {
            format!(
                "{} {} {}-{} parent={}",
                symbol.kind,
                symbol.qualified_name,
                symbol.start_line,
                symbol.end_line,
                symbol.parent_symbol_id.as_deref().unwrap_or("<root>")
            )
        })
        .collect::<Vec<_>>()
        .join(" | ")
}

fn file_call_detail_summary(file: &symdex_store::FileCoverageRow) -> String {
    if file.detail.calls.is_empty() {
        return "<none>".to_owned();
    }
    file.detail
        .calls
        .iter()
        .take(2)
        .map(|call| {
            format!(
                "status={} conf={:.2} line={} {} -> {}",
                call.resolution_status,
                call.confidence,
                call.call_line,
                call.caller_symbol,
                call.callee_text
            )
        })
        .collect::<Vec<_>>()
        .join(" | ")
}

fn chunk_vector_status(status: ChunkVectorStatus) -> (&'static str, StatusTone) {
    match status {
        ChunkVectorStatus::VectorBacked => ("vector-backed", StatusTone::Success),
        ChunkVectorStatus::MissingVector => ("missing-vector", StatusTone::Warning),
        ChunkVectorStatus::Excluded => ("excluded", StatusTone::Warning),
    }
}

fn outline_table(summary: &SymbolOutlineSummary) -> Table<'_> {
    let rows = summary.symbols.iter().map(|symbol| {
        Row::new(vec![
            Cell::from(outline_symbol_label(symbol)),
            Cell::from(symbol.kind.as_str()),
            Cell::from(symbol.path.as_str()),
            Cell::from(line_range(symbol.start_line, symbol.end_line)),
            Cell::from(symbol.child_count.to_string()),
        ])
    });

    Table::new(
        rows,
        [
            Constraint::Percentage(36),
            Constraint::Length(10),
            Constraint::Percentage(28),
            Constraint::Length(9),
            Constraint::Length(5),
        ],
    )
    .header(table_header(["Symbol", "Kind", "Path", "Lines", "Kids"]))
    .block(Block::default().borders(Borders::ALL).title(format!(
        "Symbol Outline | Symbols: {}",
        summary.symbols.len()
    )))
    .column_spacing(1)
}

fn outline_detail_panel(summary: &SymbolOutlineSummary, selection: usize) -> Paragraph<'_> {
    let Some(symbol) = selected_outline_row(summary, selection) else {
        return Paragraph::new(vec![Line::from("No symbols indexed.")])
            .wrap(Wrap { trim: true })
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Symbol Detail"),
            );
    };
    let lines = vec![
        Line::from(vec![
            Span::styled("Symbol: ", Style::new().add_modifier(Modifier::BOLD)),
            Span::raw(symbol.qualified_name.as_str()),
        ]),
        Line::from(vec![
            Span::styled("Kind: ", Style::new().add_modifier(Modifier::BOLD)),
            Span::raw(symbol.kind.as_str()),
            Span::raw(" "),
            Span::styled("Depth: ", Style::new().add_modifier(Modifier::BOLD)),
            Span::raw(symbol.depth.to_string()),
            Span::raw(" "),
            Span::styled("Children: ", Style::new().add_modifier(Modifier::BOLD)),
            Span::raw(symbol.child_count.to_string()),
        ]),
        Line::from(vec![
            Span::styled("Parent: ", Style::new().add_modifier(Modifier::BOLD)),
            Span::raw(symbol.parent_symbol_id.as_deref().unwrap_or("<root>")),
        ]),
        Line::from(vec![
            Span::styled("Location: ", Style::new().add_modifier(Modifier::BOLD)),
            Span::raw(format!(
                "{}:{}-{}",
                symbol.path, symbol.start_line, symbol.end_line
            )),
        ]),
    ];
    Paragraph::new(lines).wrap(Wrap { trim: true }).block(
        Block::default()
            .borders(Borders::ALL)
            .title("Symbol Detail"),
    )
}

fn outline_row_count(summary: &SymbolOutlineSummary) -> usize {
    summary.symbols.len()
}

fn selected_outline_row(
    summary: &SymbolOutlineSummary,
    selection: usize,
) -> Option<&symdex_store::SymbolOutlineRow> {
    summary
        .symbols
        .get(selection.min(summary.symbols.len().saturating_sub(1)))
}

fn outline_symbol_label(symbol: &symdex_store::SymbolOutlineRow) -> String {
    let indent = "  ".repeat(symbol.depth.min(6));
    format!("{indent}{}", symbol.qualified_name)
}

fn call_resolution_table(summary: &CallResolutionSummary) -> Table<'_> {
    let rows = summary.buckets.iter().map(|bucket| {
        let tone =
            call_resolution_tone(bucket.resolution_status.as_str(), bucket.confidence_bucket);
        Row::new(vec![
            Cell::from(status_span(bucket.resolution_status.as_str(), tone)),
            Cell::from(status_span(bucket.confidence_bucket.label(), tone)),
            Cell::from(bucket.call_count.to_string()),
            Cell::from(format!("{:.2}", bucket.average_confidence))
                .style(score_style(bucket.average_confidence)),
        ])
    });

    Table::new(
        rows,
        [
            Constraint::Percentage(42),
            Constraint::Length(10),
            Constraint::Length(7),
            Constraint::Length(8),
        ],
    )
    .header(table_header(["Resolution", "Conf", "Calls", "Avg"]))
    .block(Block::default().borders(Borders::ALL).title(format!(
        "Call Resolution | Buckets: {}",
        summary.buckets.len()
    )))
    .column_spacing(1)
}

fn call_resolution_detail_panel(
    summary: &CallResolutionSummary,
    selection: usize,
) -> Paragraph<'_> {
    let Some(bucket) = selected_call_resolution_bucket(summary, selection) else {
        return Paragraph::new(vec![Line::from("No call edges indexed.")])
            .wrap(Wrap { trim: true })
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Call Bucket Detail"),
            );
    };
    let rows = bucket
        .rows
        .iter()
        .take(3)
        .map(|row| {
            format!(
                "{}:{} {} -> {} conf={:.2} status={}",
                row.path,
                row.call_line,
                row.caller_symbol,
                row.callee_text,
                row.confidence,
                row.resolution_status
            )
        })
        .collect::<Vec<_>>()
        .join(" | ");
    let tone = call_resolution_tone(bucket.resolution_status.as_str(), bucket.confidence_bucket);
    let lines = vec![
        Line::from(vec![
            Span::styled("Resolution: ", Style::new().add_modifier(Modifier::BOLD)),
            status_span(bucket.resolution_status.as_str(), tone),
        ]),
        Line::from(vec![
            Span::styled(
                "Confidence bucket: ",
                Style::new().add_modifier(Modifier::BOLD),
            ),
            status_span(bucket.confidence_bucket.label(), tone),
            Span::raw(format!(
                " count={} avg={:.2}",
                bucket.call_count, bucket.average_confidence
            )),
        ]),
        Line::from(vec![
            Span::styled("Rows: ", Style::new().add_modifier(Modifier::BOLD)),
            Span::raw(if rows.is_empty() {
                "<none>".to_owned()
            } else {
                rows
            }),
        ]),
    ];
    Paragraph::new(lines).wrap(Wrap { trim: true }).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(tone_style(tone))
            .title("Call Bucket Detail"),
    )
}

fn call_resolution_row_count(summary: &CallResolutionSummary) -> usize {
    summary.buckets.len()
}

fn selected_call_resolution_bucket(
    summary: &CallResolutionSummary,
    selection: usize,
) -> Option<&symdex_store::CallResolutionBucket> {
    summary
        .buckets
        .get(selection.min(summary.buckets.len().saturating_sub(1)))
}

fn call_resolution_tone(status: &str, bucket: ConfidenceBucket) -> StatusTone {
    if status == "unresolved" || matches!(bucket, ConfidenceBucket::Low) {
        StatusTone::Warning
    } else if status.starts_with("resolved") && matches!(bucket, ConfidenceBucket::High) {
        StatusTone::Success
    } else {
        StatusTone::Info
    }
}

fn embedding_coverage_table(summary: &EmbeddingCoverageSummary) -> Table<'_> {
    let rows = embedding_coverage_rows(summary).into_iter().map(|row| {
        Row::new(vec![
            Cell::from(row.metric),
            Cell::from(row.value),
            Cell::from(status_span(row.status, row.tone)),
        ])
    });

    Table::new(
        rows,
        [
            Constraint::Percentage(34),
            Constraint::Percentage(42),
            Constraint::Length(16),
        ],
    )
    .header(table_header(["Metric", "Value", "Status"]))
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title(format!("Embedding Coverage | {}", summary.repository_id)),
    )
    .column_spacing(1)
}

fn embedding_coverage_detail_panel(
    summary: &EmbeddingCoverageSummary,
    selection: usize,
) -> Paragraph<'_> {
    let rows = embedding_coverage_rows(summary);
    let row = rows.get(selection.min(rows.len().saturating_sub(1)));
    let mut lines = Vec::new();
    if let Some(row) = row {
        lines.push(Line::from(vec![
            Span::styled("Selected: ", Style::new().add_modifier(Modifier::BOLD)),
            Span::raw(row.metric),
        ]));
        lines.push(Line::from(vec![
            Span::styled("Status: ", Style::new().add_modifier(Modifier::BOLD)),
            status_span(row.status, row.tone),
            Span::raw(" "),
            Span::raw(row.detail),
        ]));
    }
    lines.push(Line::from(vec![
        Span::styled("Collection: ", Style::new().add_modifier(Modifier::BOLD)),
        Span::raw(summary.collection_name.as_str()),
    ]));
    lines.push(Line::from(vec![
        Span::styled("Exclusions: ", Style::new().add_modifier(Modifier::BOLD)),
        Span::raw(embedding_exclusion_summary(summary)),
    ]));
    lines.push(Line::from(vec![
        Span::styled("Health: ", Style::new().add_modifier(Modifier::BOLD)),
        Span::raw(embedding_health_summary(summary)),
    ]));

    let tone = summary
        .health
        .iter()
        .map(|row| storage_health_tone(row.status))
        .find(|tone| matches!(tone, StatusTone::Error | StatusTone::Warning))
        .unwrap_or(StatusTone::Success);
    Paragraph::new(lines).wrap(Wrap { trim: true }).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(tone_style(tone))
            .title("Embedding Detail"),
    )
}

fn embedding_coverage_rows(summary: &EmbeddingCoverageSummary) -> Vec<StorageDisplayRow> {
    let vector_status = if summary.embeddable_chunks == 0 {
        ("metadata-only", StatusTone::Warning)
    } else if summary.missing_vector_chunks == 0 {
        ("covered", StatusTone::Success)
    } else {
        ("missing-vector", StatusTone::Warning)
    };
    let model_status = if summary.embedding_model == summary.configured_embedding_model {
        ("aligned", StatusTone::Success)
    } else {
        ("model-drift", StatusTone::Error)
    };

    vec![
        storage_row(
            "sqlite-vec",
            "total chunks",
            summary.total_chunks.to_string(),
            ("indexed", StatusTone::Info),
            "SQLite chunk rows for the selected repository.",
        ),
        storage_row(
            "sqlite-vec",
            "embeddable",
            summary.embeddable_chunks.to_string(),
            ("eligible", StatusTone::Info),
            "Chunks eligible for semantic embedding.",
        ),
        storage_row(
            "sqlite-vec",
            "vector-backed",
            summary.vector_backed_chunks.to_string(),
            vector_status,
            "Chunks with recorded sqlite-vec point IDs.",
        ),
        storage_row(
            "sqlite-vec",
            "missing vectors",
            summary.missing_vector_chunks.to_string(),
            if summary.missing_vector_chunks == 0 {
                ("ok", StatusTone::Success)
            } else {
                ("missing-vector", StatusTone::Warning)
            },
            "Embeddable chunks without vector point metadata.",
        ),
        storage_row(
            "sqlite-vec",
            "excluded",
            summary.excluded_chunks.to_string(),
            if summary.excluded_chunks == 0 {
                ("none", StatusTone::Success)
            } else {
                ("metadata-only", StatusTone::Warning)
            },
            "Chunks intentionally withheld from embeddings.",
        ),
        storage_row(
            "sqlite-vec",
            "model",
            summary.embedding_model.clone(),
            model_status,
            "Latest indexed embedding model compared with configured model.",
        ),
        storage_row(
            "sqlite-vec",
            "dimension",
            summary
                .embedding_dimension
                .map(|dimension| dimension.to_string())
                .unwrap_or_else(|| "<unknown>".to_owned()),
            if summary.embedding_dimension.is_some() {
                ("recorded", StatusTone::Success)
            } else {
                ("unknown", StatusTone::Warning)
            },
            "Latest recorded vector dimension.",
        ),
        storage_row(
            "sqlite-vec",
            "run embedded",
            summary
                .latest_chunks_embedded
                .map(|chunks| chunks.to_string())
                .unwrap_or_else(|| "<none>".to_owned()),
            if summary.latest_chunks_embedded.is_some() {
                ("recorded", StatusTone::Info)
            } else {
                ("missing", StatusTone::Warning)
            },
            "Chunks embedded by the latest successful semantic index run.",
        ),
    ]
}

fn embedding_coverage_row_count(summary: &EmbeddingCoverageSummary) -> usize {
    embedding_coverage_rows(summary).len()
}

fn embedding_exclusion_summary(summary: &EmbeddingCoverageSummary) -> String {
    if summary.exclusion_reasons.is_empty() {
        return "<none>".to_owned();
    }
    summary
        .exclusion_reasons
        .iter()
        .take(3)
        .map(|row| format!("{}={}", row.reason, row.chunks))
        .collect::<Vec<_>>()
        .join(" | ")
}

fn embedding_health_summary(summary: &EmbeddingCoverageSummary) -> String {
    summary
        .health
        .iter()
        .take(3)
        .map(|row| format!("{}: {}", row.label, row.detail))
        .collect::<Vec<_>>()
        .join(" | ")
}

fn index_runs_timeline_table(summary: &IndexRunsTimelineSummary) -> Table<'_> {
    let rows = summary.runs.iter().map(|run| {
        let tone = index_run_status_tone(run.status.as_str());
        Row::new(vec![
            Cell::from(run.started_at.as_str()),
            Cell::from(run.run_kind.as_str()),
            Cell::from(status_span(run.status.as_str(), tone)),
            Cell::from(run.files_seen.to_string()),
            Cell::from(run.files_indexed.to_string()),
            Cell::from(run.chunks_embedded.to_string()),
            Cell::from(run.embedding_model.as_str()),
            Cell::from(
                run.embedding_dimension
                    .map(|dimension| dimension.to_string())
                    .unwrap_or_else(|| "<unknown>".to_owned()),
            ),
        ])
    });

    Table::new(
        rows,
        [
            Constraint::Percentage(24),
            Constraint::Length(8),
            Constraint::Length(10),
            Constraint::Length(5),
            Constraint::Length(5),
            Constraint::Length(5),
            Constraint::Percentage(26),
            Constraint::Length(8),
        ],
    )
    .header(table_header([
        "Started", "Kind", "Status", "Seen", "Idx", "Emb", "Model", "Dim",
    ]))
    .block(Block::default().borders(Borders::ALL).title(format!(
        "Index Runs Timeline | Runs: {}",
        summary.runs.len()
    )))
    .column_spacing(1)
}

fn index_runs_timeline_detail_panel(
    summary: &IndexRunsTimelineSummary,
    selection: usize,
) -> Paragraph<'_> {
    let Some(run) = selected_index_run(summary, selection) else {
        return Paragraph::new(vec![Line::from("No index runs recorded.")])
            .wrap(Wrap { trim: true })
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Index Run Detail"),
            );
    };
    let tone = index_run_status_tone(run.status.as_str());
    let lines = vec![
        Line::from(vec![
            Span::styled("Run: ", Style::new().add_modifier(Modifier::BOLD)),
            Span::raw(run.id.as_str()),
            Span::raw(" "),
            Span::styled("Kind: ", Style::new().add_modifier(Modifier::BOLD)),
            Span::raw(run.run_kind.as_str()),
            Span::raw(" "),
            status_span(run.status.as_str(), tone),
        ]),
        Line::from(vec![
            Span::styled("Started: ", Style::new().add_modifier(Modifier::BOLD)),
            Span::raw(run.started_at.as_str()),
            Span::raw(" "),
            Span::styled("Finished: ", Style::new().add_modifier(Modifier::BOLD)),
            Span::raw(run.finished_at.as_deref().unwrap_or("<running>")),
        ]),
        Line::from(vec![
            Span::styled("Counts: ", Style::new().add_modifier(Modifier::BOLD)),
            Span::raw(format!(
                "files_seen={} files_indexed={} chunks_embedded={}",
                run.files_seen, run.files_indexed, run.chunks_embedded
            )),
        ]),
        Line::from(vec![
            Span::styled("Embedding: ", Style::new().add_modifier(Modifier::BOLD)),
            Span::raw(format!(
                "{} dim={}",
                run.embedding_model,
                run.embedding_dimension
                    .map(|dimension| dimension.to_string())
                    .unwrap_or_else(|| "<unknown>".to_owned())
            )),
        ]),
        Line::from(vec![
            Span::styled("Error: ", Style::new().add_modifier(Modifier::BOLD)),
            Span::raw(run.error_summary.as_deref().unwrap_or("<none>")),
        ]),
    ];
    Paragraph::new(lines).wrap(Wrap { trim: true }).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(tone_style(tone))
            .title("Index Run Detail"),
    )
}

fn index_runs_timeline_row_count(summary: &IndexRunsTimelineSummary) -> usize {
    summary.runs.len()
}

fn freshness_table(summary: &FreshnessSummary) -> Table<'_> {
    let rows = summary.files.iter().map(|file| {
        let tone = freshness_tone(file.freshness);
        Row::new(vec![
            Cell::from(file.path.as_str()),
            Cell::from(status_span(file.freshness.label(), tone)),
            Cell::from(file.indexed_content_hash.as_deref().unwrap_or("<none>")),
            Cell::from(file.current_content_hash.as_deref().unwrap_or("<none>")),
            Cell::from(file.index_run_id.as_deref().unwrap_or("<none>")),
        ])
    });

    Table::new(
        rows,
        [
            Constraint::Percentage(34),
            Constraint::Length(9),
            Constraint::Percentage(20),
            Constraint::Percentage(20),
            Constraint::Percentage(17),
        ],
    )
    .header(table_header([
        "Path",
        "Fresh",
        "Indexed hash",
        "Current hash",
        "Run",
    ]))
    .block(Block::default().borders(Borders::ALL).title(format!(
        "Evidence Freshness | fresh={} stale={} deleted={} missing={}",
        summary.count(EvidenceFreshness::Fresh),
        summary.count(EvidenceFreshness::Stale),
        summary.count(EvidenceFreshness::Deleted),
        summary.count(EvidenceFreshness::Missing)
    )))
    .column_spacing(1)
}

fn freshness_detail_panel(summary: &FreshnessSummary, selection: usize) -> Paragraph<'_> {
    let Some(file) = summary
        .files
        .get(selection.min(summary.files.len().saturating_sub(1)))
    else {
        return Paragraph::new(vec![Line::from(
            "No freshness rows. Run indexing or refresh the repository.",
        )])
        .wrap(Wrap { trim: true })
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("Freshness Detail"),
        );
    };
    let tone = freshness_tone(file.freshness);
    let lines = vec![
        Line::from(vec![
            Span::styled("File: ", Style::new().add_modifier(Modifier::BOLD)),
            Span::raw(file.path.as_str()),
            Span::raw(" "),
            status_span(file.freshness.label(), tone),
        ]),
        Line::from(vec![
            Span::styled("Hashes: ", Style::new().add_modifier(Modifier::BOLD)),
            Span::raw(format!(
                "indexed={} current={}",
                file.indexed_content_hash.as_deref().unwrap_or("<none>"),
                file.current_content_hash.as_deref().unwrap_or("<none>")
            )),
        ]),
        Line::from(vec![
            Span::styled("Provenance: ", Style::new().add_modifier(Modifier::BOLD)),
            Span::raw(format!(
                "run={} parser={} indexed_at={}",
                file.index_run_id.as_deref().unwrap_or("<none>"),
                file.parser_version.as_deref().unwrap_or("<none>"),
                file.indexed_at.as_deref().unwrap_or("<never>")
            )),
        ]),
    ];
    Paragraph::new(lines).wrap(Wrap { trim: true }).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(tone_style(tone))
            .title("Freshness Detail"),
    )
}

fn freshness_row_count(summary: &FreshnessSummary) -> usize {
    summary.files.len()
}

fn freshness_tone(freshness: EvidenceFreshness) -> StatusTone {
    match freshness {
        EvidenceFreshness::Fresh => StatusTone::Success,
        EvidenceFreshness::Stale | EvidenceFreshness::Missing => StatusTone::Warning,
        EvidenceFreshness::Deleted => StatusTone::Error,
        EvidenceFreshness::Unknown => StatusTone::Info,
    }
}

fn selected_index_run(
    summary: &IndexRunsTimelineSummary,
    selection: usize,
) -> Option<&IndexRunTimelineRow> {
    summary
        .runs
        .get(selection.min(summary.runs.len().saturating_sub(1)))
}

fn index_run_status_tone(status: &str) -> StatusTone {
    match status {
        "success" | "complete" | "completed" => StatusTone::Success,
        "failed" | "error" => StatusTone::Error,
        "running" | "started" | "pending" => StatusTone::Info,
        _ => StatusTone::Warning,
    }
}

fn semantic_neighborhood_table(summary: &SemanticNeighborhoodSummary) -> Table<'_> {
    let rows = summary.rows.iter().map(|row| {
        Row::new(vec![
            Cell::from(row.path.as_str()),
            Cell::from(line_range(row.start_line, row.end_line)),
            Cell::from(row.symbol_name.as_deref().unwrap_or("<none>")),
            Cell::from(row.chunk_kind.as_str()),
            Cell::from(semantic_score_label(row)),
            Cell::from(short_hash(row.text_hash.as_str())),
        ])
    });

    Table::new(
        rows,
        [
            Constraint::Percentage(30),
            Constraint::Length(9),
            Constraint::Percentage(24),
            Constraint::Length(10),
            Constraint::Length(10),
            Constraint::Length(12),
        ],
    )
    .header(table_header([
        "Path",
        "Lines",
        "Symbol",
        "Kind",
        "Score",
        "Text Hash",
    ]))
    .block(Block::default().borders(Borders::ALL).title(format!(
        "Semantic Neighborhood | Payloads: {}",
        summary.rows.len()
    )))
    .column_spacing(1)
}

fn semantic_neighborhood_detail_panel(
    summary: &SemanticNeighborhoodSummary,
    selection: usize,
) -> Paragraph<'_> {
    let Some(row) = selected_semantic_neighborhood_row(summary, selection) else {
        return Paragraph::new(vec![Line::from(
            "No vector-backed sqlite-vec payload metadata recorded.",
        )])
        .wrap(Wrap { trim: true })
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("Semantic Payload Detail"),
        );
    };
    let lines = vec![
        Line::from(vec![
            Span::styled("Collection: ", Style::new().add_modifier(Modifier::BOLD)),
            Span::raw(summary.collection_name.as_str()),
        ]),
        Line::from(vec![
            Span::styled("Location: ", Style::new().add_modifier(Modifier::BOLD)),
            Span::raw(format!("{}:{}-{}", row.path, row.start_line, row.end_line)),
        ]),
        Line::from(vec![
            Span::styled("Payload: ", Style::new().add_modifier(Modifier::BOLD)),
            Span::raw(format!(
                "symbol={} kind={} language={} score={}",
                row.symbol_name.as_deref().unwrap_or("<none>"),
                row.chunk_kind,
                row.language,
                semantic_score_label(row)
            )),
        ]),
        Line::from(vec![
            Span::styled("Point: ", Style::new().add_modifier(Modifier::BOLD)),
            Span::raw(row.vector_point_id.as_str()),
        ]),
        Line::from(vec![
            Span::styled("Text hash: ", Style::new().add_modifier(Modifier::BOLD)),
            Span::raw(row.text_hash.as_str()),
        ]),
        Line::from(vec![
            Span::styled("Health: ", Style::new().add_modifier(Modifier::BOLD)),
            Span::raw(semantic_neighborhood_health_summary(summary)),
        ]),
    ];
    Paragraph::new(lines).wrap(Wrap { trim: true }).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(tone_style(StatusTone::Info))
            .title("Semantic Payload Detail"),
    )
}

fn semantic_neighborhood_row_count(summary: &SemanticNeighborhoodSummary) -> usize {
    summary.rows.len()
}

fn selected_semantic_neighborhood_row(
    summary: &SemanticNeighborhoodSummary,
    selection: usize,
) -> Option<&SemanticNeighborhoodRow> {
    summary
        .rows
        .get(selection.min(summary.rows.len().saturating_sub(1)))
}

fn semantic_score_label(row: &SemanticNeighborhoodRow) -> String {
    row.score
        .map(|score| format!("{score:.3}"))
        .unwrap_or_else(|| "metadata".to_owned())
}

fn semantic_neighborhood_health_summary(summary: &SemanticNeighborhoodSummary) -> String {
    summary
        .health
        .iter()
        .take(3)
        .map(|row| format!("{}: {}", row.label, row.detail))
        .collect::<Vec<_>>()
        .join(" | ")
}

fn short_hash(text_hash: &str) -> String {
    text_hash.chars().take(10).collect()
}

fn cross_store_health_table(summary: &CrossStoreHealthSummary) -> Table<'_> {
    let rows = summary.rows.iter().map(|row| {
        let tone = storage_health_tone(row.status);
        Row::new(vec![
            Cell::from(status_span(row.label.as_str(), tone)),
            Cell::from(storage_health_label(row.status)),
            Cell::from(row.detail.as_str()),
        ])
    });

    Table::new(
        rows,
        [
            Constraint::Percentage(28),
            Constraint::Length(9),
            Constraint::Percentage(58),
        ],
    )
    .header(table_header(["Check", "Status", "Detail"]))
    .block(Block::default().borders(Borders::ALL).title(format!(
        "Cross-Store Health | Checks: {}",
        summary.rows.len()
    )))
    .column_spacing(1)
}

fn cross_store_health_detail_panel(
    summary: &CrossStoreHealthSummary,
    selection: usize,
) -> Paragraph<'_> {
    let Some(row) = selected_cross_store_health_row(summary, selection) else {
        return Paragraph::new(vec![Line::from("No cross-store health checks available.")])
            .wrap(Wrap { trim: true })
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Cross-Store Detail"),
            );
    };
    let tone = storage_health_tone(row.status);
    let lines = vec![
        Line::from(vec![
            Span::styled("Collection: ", Style::new().add_modifier(Modifier::BOLD)),
            Span::raw(summary.collection_name.as_str()),
        ]),
        Line::from(vec![
            Span::styled("Check: ", Style::new().add_modifier(Modifier::BOLD)),
            status_span(row.label.as_str(), tone),
            Span::raw(" "),
            status_span(storage_health_label(row.status), tone),
        ]),
        Line::from(vec![
            Span::styled("Detail: ", Style::new().add_modifier(Modifier::BOLD)),
            Span::raw(row.detail.as_str()),
        ]),
        Line::from(vec![
            Span::styled("Scope: ", Style::new().add_modifier(Modifier::BOLD)),
            Span::raw(
                "missing collections, missing vectors, excluded chunks, model drift, dimension drift",
            ),
        ]),
    ];
    Paragraph::new(lines).wrap(Wrap { trim: true }).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(tone_style(tone))
            .title("Cross-Store Detail"),
    )
}

fn cross_store_health_row_count(summary: &CrossStoreHealthSummary) -> usize {
    summary.rows.len()
}

fn selected_cross_store_health_row(
    summary: &CrossStoreHealthSummary,
    selection: usize,
) -> Option<&StorageHealthRow> {
    summary
        .rows
        .get(selection.min(summary.rows.len().saturating_sub(1)))
}

fn storage_health_label(status: StorageHealthStatus) -> &'static str {
    match status {
        StorageHealthStatus::Ok => "ok",
        StorageHealthStatus::Warning => "warning",
        StorageHealthStatus::Error => "error",
    }
}

fn service_row<'a>(label: &'static str, state: &'static str, target: &'a str) -> Row<'a> {
    Row::new(vec![
        Cell::from(label),
        Cell::from(status_span(state, StatusTone::Info)),
        Cell::from(target),
    ])
}

fn diagnostic_detail_panel(check: &DiagnosticCheck, expanded: bool) -> Paragraph<'_> {
    let (status, tone) = diagnostic_status(check.state);
    let mut lines = vec![
        Line::from(vec![
            Span::styled("Check: ", Style::new().add_modifier(Modifier::BOLD)),
            Span::raw(check.label.as_str()),
        ]),
        Line::from(vec![
            Span::styled("Status: ", Style::new().add_modifier(Modifier::BOLD)),
            status_span(status, tone),
        ]),
        Line::from(vec![
            Span::styled("Detail: ", Style::new().add_modifier(Modifier::BOLD)),
            Span::raw(diagnostic_message(check)),
        ]),
    ];
    if expanded {
        lines.push(Line::from(""));
        lines.push(Line::from(vec![
            Span::styled("Hint: ", Style::new().add_modifier(Modifier::BOLD)),
            Span::raw(diagnostic_hint(check)),
        ]));
    }

    let title = if expanded {
        "Selected Check Details | expanded"
    } else {
        "Selected Check Details"
    };
    Paragraph::new(lines).wrap(Wrap { trim: true }).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(tone_style(tone))
            .title(Line::from(status_span(title, tone))),
    )
}

fn selected_diagnostic_check(
    report: &DiagnosticReport,
    selection: usize,
) -> Option<&DiagnosticCheck> {
    report
        .checks
        .get(selection.min(report.checks.len().saturating_sub(1)))
}

fn diagnostic_message(check: &DiagnosticCheck) -> &str {
    if check.message.is_empty() {
        "<no detail>"
    } else {
        check.message.as_str()
    }
}

fn diagnostic_hint(check: &DiagnosticCheck) -> &'static str {
    match check.state {
        DiagnosticState::Ok => "No action needed.",
        DiagnosticState::Missing => "Create or configure the missing local path or dependency.",
        DiagnosticState::Pending => {
            "Let the local job finish or run the matching catch-up command."
        }
        DiagnosticState::Unreachable => {
            "Start the local service or verify the configured localhost endpoint."
        }
        DiagnosticState::Error => "Review the diagnostic detail and local configuration.",
        DiagnosticState::Skipped => "No action needed unless this check should apply locally.",
    }
}

fn query_table(result: &QueryResult) -> Table<'_> {
    match result {
        QueryResult::Symbol(summary) => symbol_table(summary),
        QueryResult::Semantic(summary) => semantic_table(summary),
    }
}

fn symbol_table(summary: &SymbolSearchSummary) -> Table<'_> {
    let rows = summary.symbols.iter().map(|symbol| {
        Row::new(vec![
            Cell::from(symbol.kind.as_str()),
            Cell::from(symbol.qualified_name.as_str()),
            Cell::from(symbol.path.as_str()),
            Cell::from(line_range(symbol.start_line, symbol.end_line)),
        ])
    });
    Table::new(
        rows,
        [
            Constraint::Length(12),
            Constraint::Percentage(34),
            Constraint::Percentage(34),
            Constraint::Length(12),
        ],
    )
    .header(table_header(["Kind", "Symbol", "Path", "Lines"]))
    .block(Block::default().borders(Borders::ALL).title(format!(
        "Query Workbench | Symbols: {} | Query: {}",
        summary.symbols.len(),
        summary.query
    )))
    .column_spacing(1)
}

fn semantic_table(summary: &SemanticSearchSummary) -> Table<'_> {
    let rows = summary.results.iter().map(|result| {
        Row::new(vec![
            Cell::from(format!("{:.4}", result.score)).style(score_style(result.score)),
            Cell::from(result.path.as_str()),
            Cell::from(line_range(result.start_line, result.end_line)),
            Cell::from(result.chunk_kind.as_str()),
            Cell::from(result.symbol_name.as_deref().unwrap_or("<none>")),
        ])
    });
    Table::new(
        rows,
        [
            Constraint::Length(8),
            Constraint::Percentage(32),
            Constraint::Length(12),
            Constraint::Length(14),
            Constraint::Percentage(24),
        ],
    )
    .header(table_header(["Score", "Path", "Lines", "Kind", "Symbol"]))
    .block(Block::default().borders(Borders::ALL).title(format!(
        "Query Workbench | Semantic results: {} | Collection: {}",
        summary.results.len(),
        summary.vector_table
    )))
    .column_spacing(1)
}

fn call_graph_table(summary: &CallGraphSummary) -> Table<'_> {
    let rows = summary.rows.iter().map(call_row);
    Table::new(
        rows,
        [
            Constraint::Percentage(30),
            Constraint::Length(8),
            Constraint::Percentage(28),
            Constraint::Length(12),
            Constraint::Percentage(18),
            Constraint::Length(16),
        ],
    )
    .header(table_header([
        "Symbol",
        "Conf",
        "Path",
        "Lines",
        "Callee",
        "Resolution",
    ]))
    .block(Block::default().borders(Borders::ALL).title(format!(
        "Symbol/Call Graph | {}: {} | Symbol query: {}",
        summary.direction.label(),
        summary.rows.len(),
        summary.query
    )))
    .column_spacing(1)
}

fn impact_table(summary: &ImpactSummary) -> Table<'_> {
    let caller_rows = summary
        .direct_callers
        .iter()
        .map(|evidence| impact_row("caller", evidence));
    let callee_rows = summary
        .direct_callees
        .iter()
        .map(|evidence| impact_row("callee", evidence));
    Table::new(
        caller_rows.chain(callee_rows),
        [
            Constraint::Length(8),
            Constraint::Percentage(24),
            Constraint::Length(8),
            Constraint::Length(9),
            Constraint::Percentage(22),
            Constraint::Length(12),
            Constraint::Percentage(15),
            Constraint::Length(16),
        ],
    )
    .header(table_header([
        "Edge",
        "Symbol",
        "Conf",
        "Fresh",
        "Path",
        "Lines",
        "Callee",
        "Resolution",
    ]))
    .block(Block::default().borders(Borders::ALL).title(format!(
        "Impact/Call Path/Context Pack | Impact query: {} | Direct callers: {} | Direct callees: {} | Paths: {}",
        summary.query,
        summary.direct_callers.len(),
        summary.direct_callees.len(),
        summary.transitive_callers.len() + summary.transitive_callees.len()
    )))
    .column_spacing(1)
}

fn call_path_table(summary: &CallPathSummary) -> Table<'_> {
    let rows = summary
        .paths
        .iter()
        .enumerate()
        .flat_map(|(path_index, path)| {
            path.edges
                .iter()
                .enumerate()
                .map(move |(edge_index, edge)| {
                    Row::new(vec![
                        Cell::from((path_index + 1).to_string()),
                        Cell::from((edge_index + 1).to_string()),
                        Cell::from(format!(
                            "{} -> {}",
                            edge.caller_symbol_qualified_name,
                            edge.callee_symbol_qualified_name
                                .as_deref()
                                .unwrap_or(&edge.callee_text)
                        )),
                        confidence_cell(edge.confidence),
                        Cell::from(edge.caller_path.as_str()),
                        Cell::from(edge.call_line.to_string()),
                        resolution_cell(edge.resolution_status.as_str()),
                    ])
                })
        });
    Table::new(
        rows,
        [
            Constraint::Length(5),
            Constraint::Length(5),
            Constraint::Percentage(34),
            Constraint::Length(8),
            Constraint::Percentage(28),
            Constraint::Length(8),
            Constraint::Length(18),
        ],
    )
    .header(table_header([
        "Path",
        "Hop",
        "Edge",
        "Conf",
        "File",
        "Line",
        "Resolution",
    ]))
    .block(Block::default().borders(Borders::ALL).title(format!(
        "Impact/Call Path/Context Pack | Call path: {} -> {} | Paths: {} | Max depth: {}",
        summary.source_query,
        summary.target_query,
        summary.paths.len(),
        summary.max_depth
    )))
    .column_spacing(1)
}

fn context_pack_table(pack: &ContextPack) -> Table<'_> {
    let mut entries = vec![
        ("Format".to_owned(), pack.format.clone()),
        ("Query".to_owned(), pack.query.clone()),
        (
            "Focus symbols".to_owned(),
            pack.focus_symbols.len().to_string(),
        ),
        ("Callers".to_owned(), pack.direct_callers.len().to_string()),
        ("Callees".to_owned(), pack.direct_callees.len().to_string()),
        ("Files".to_owned(), pack.files.len().to_string()),
        (
            "Limits".to_owned(),
            format!(
                "symbols={} callers={} callees={}",
                pack.limits.max_symbols, pack.limits.max_callers, pack.limits.max_callees
            ),
        ),
    ];
    entries.extend(
        pack.focus_symbols
            .iter()
            .map(|symbol| ("Symbol".to_owned(), symbol.qualified_name.clone())),
    );
    entries.extend(
        pack.files
            .iter()
            .map(|file| ("File".to_owned(), file.clone())),
    );
    entries.extend(
        pack.notes
            .iter()
            .map(|note| ("Note".to_owned(), note.clone())),
    );
    let rows = entries
        .into_iter()
        .map(|(field, value)| Row::new(vec![Cell::from(field), Cell::from(value)]));

    Table::new(rows, [Constraint::Length(18), Constraint::Percentage(76)])
        .header(table_header(["Field", "Value"]))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("Impact/Call Path/Context Pack | Context Pack Metadata"),
        )
        .column_spacing(1)
}

fn debug_context_table(pack: &DebugContextPack) -> Table<'_> {
    let frame_rows = pack.frames.iter().map(|frame| {
        let symbol = frame
            .matched_symbols
            .first()
            .map(|symbol| symbol.qualified_name.clone())
            .or_else(|| frame.frame.symbol.clone())
            .unwrap_or_else(|| "<unmapped>".to_owned());
        Row::new(vec![
            Cell::from("Frame"),
            Cell::from(format!("#{}", frame.frame.ordinal)),
            Cell::from(format!(
                "{} {} {}",
                frame
                    .normalized_path
                    .as_deref()
                    .or(frame.frame.path.as_deref())
                    .unwrap_or("<unknown>"),
                runtime_line_label(frame.frame.line, frame.frame.column),
                symbol
            )),
            freshness_cell(frame.file_freshness),
            matched_cell(frame.matched),
            Cell::from(provenance_label(frame.file_provenance.as_ref())),
        ])
    });
    let path_rows = pack.call_paths_between_frames.iter().map(|path| {
        let terminal_status = path
            .paths
            .first()
            .map(|path| path.terminal_resolution_status.as_str())
            .unwrap_or("unresolved");
        Row::new(vec![
            Cell::from("Path"),
            Cell::from(format!("{}->{}", path.from_frame, path.to_frame)),
            Cell::from(format!(
                "{} -> {} ({} paths)",
                path.from_symbol,
                path.to_symbol,
                path.paths.len()
            )),
            Cell::from("n/a"),
            resolution_cell(terminal_status),
            Cell::from("call graph"),
        ])
    });
    let test_rows = pack.likely_tests.iter().map(|test| {
        Row::new(vec![
            Cell::from("Test"),
            Cell::from("-"),
            Cell::from(test.clone()),
            Cell::from("unknown"),
            Cell::from("runtime"),
            Cell::from("failure input"),
        ])
    });
    let note_rows = pack.notes.iter().map(|note| {
        Row::new(vec![
            Cell::from("Note"),
            Cell::from("-"),
            Cell::from(note.clone()),
            Cell::from("n/a"),
            Cell::from("metadata"),
            Cell::from(pack.format.clone()),
        ])
    });
    let rows = frame_rows
        .chain(path_rows)
        .chain(test_rows)
        .chain(note_rows);

    Table::new(
        rows,
        [
            Constraint::Length(8),
            Constraint::Length(8),
            Constraint::Percentage(42),
            Constraint::Length(10),
            Constraint::Length(14),
            Constraint::Percentage(22),
        ],
    )
    .header(table_header([
        "Kind",
        "Ref",
        "Evidence",
        "Fresh",
        "Status",
        "Provenance",
    ]))
    .block(Block::default().borders(Borders::ALL).title(format!(
        "Impact/Call Path/Context Pack/Debug | Debug Context | Frames: {} | Paths: {} | Tests: {}",
        pack.frames.len(),
        pack.call_paths_between_frames.len(),
        pack.likely_tests.len()
    )))
    .column_spacing(1)
}

fn call_row(row: &symdex_store::CallSearchRow) -> Row<'_> {
    Row::new(vec![
        Cell::from(
            row.symbol_qualified_name
                .as_deref()
                .unwrap_or("<unresolved>"),
        ),
        confidence_cell(row.confidence),
        Cell::from(row.path.as_deref().unwrap_or("<unknown>")),
        Cell::from(optional_line_range(row.start_line, row.end_line)),
        Cell::from(row.callee_text.as_str()),
        resolution_cell(row.resolution_status.as_str()),
    ])
}

fn impact_row<'a>(edge: &'static str, evidence: &'a symdex_query::ImpactCallEvidence) -> Row<'a> {
    let row = &evidence.row;
    Row::new(vec![
        Cell::from(edge),
        Cell::from(
            row.symbol_qualified_name
                .as_deref()
                .unwrap_or("<unresolved>"),
        ),
        confidence_cell(row.confidence),
        Cell::from(evidence.freshness.label()),
        Cell::from(row.path.as_deref().unwrap_or("<unknown>")),
        Cell::from(optional_line_range(row.start_line, row.end_line)),
        Cell::from(row.callee_text.as_str()),
        resolution_cell(row.resolution_status.as_str()),
    ])
}

fn table_header<const N: usize>(labels: [&'static str; N]) -> Row<'static> {
    Row::new(
        labels
            .into_iter()
            .map(|label| Cell::from(label).style(Style::new().add_modifier(Modifier::BOLD))),
    )
    .style(Style::new().fg(Color::Cyan))
}

fn panel_block(title: &'static str, tone: StatusTone) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_style(tone_style(tone))
        .title(Line::from(status_span(title, tone)))
}

fn metadata_style() -> Style {
    Style::new().fg(Color::DarkGray)
}

fn tab_highlight_style() -> Style {
    Style::new()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
}

fn footer_key_spans(view: View) -> Vec<Span<'static>> {
    let mut spans = vec![Span::styled(
        "keys ",
        Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD),
    )];
    for (index, (key, label)) in view.footer_keys().iter().enumerate() {
        if index > 0 {
            spans.push(Span::styled("  ", metadata_style()));
        }
        spans.push(Span::styled(
            format!("[{key}]"),
            Style::new()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::raw(" "));
        spans.push(Span::styled(*label, Style::new().fg(Color::White)));
    }
    spans
}

fn screen_label(screen: Screen) -> &'static str {
    match screen {
        Screen::Dashboard => "idle",
        Screen::ConfirmIndex(_) | Screen::ConfirmContinuous => "confirm",
        Screen::IndexRunning(_) => "running",
        Screen::IndexCompleted(_) => "complete",
        Screen::IndexFailed(_) => "failed",
    }
}

fn query_status_label(status: &QueryStatus) -> &'static str {
    match status {
        QueryStatus::Idle => "idle",
        QueryStatus::Running => "running",
        QueryStatus::Completed(_) => "complete",
        QueryStatus::Failed(_) => "failed",
    }
}

fn query_status_tone(status: &QueryStatus) -> StatusTone {
    match status {
        QueryStatus::Idle => StatusTone::Dim,
        QueryStatus::Running => StatusTone::Info,
        QueryStatus::Completed(_) => StatusTone::Success,
        QueryStatus::Failed(_) => StatusTone::Error,
    }
}

fn graph_status_label(status: &GraphStatus) -> &'static str {
    match status {
        GraphStatus::Idle => "idle",
        GraphStatus::Running => "running",
        GraphStatus::Completed(_) => "complete",
        GraphStatus::Failed(_) => "failed",
    }
}

fn graph_status_tone(status: &GraphStatus) -> StatusTone {
    match status {
        GraphStatus::Idle => StatusTone::Dim,
        GraphStatus::Running => StatusTone::Info,
        GraphStatus::Completed(_) => StatusTone::Success,
        GraphStatus::Failed(_) => StatusTone::Error,
    }
}

fn evidence_status_label(status: &EvidenceStatus) -> &'static str {
    match status {
        EvidenceStatus::Idle => "idle",
        EvidenceStatus::Running => "running",
        EvidenceStatus::Completed(_) => "complete",
        EvidenceStatus::Failed(_) => "failed",
    }
}

fn evidence_status_tone(status: &EvidenceStatus) -> StatusTone {
    match status {
        EvidenceStatus::Idle => StatusTone::Dim,
        EvidenceStatus::Running => StatusTone::Info,
        EvidenceStatus::Completed(_) => StatusTone::Success,
        EvidenceStatus::Failed(_) => StatusTone::Error,
    }
}

fn diagnostic_status(state: DiagnosticState) -> (&'static str, StatusTone) {
    match state {
        DiagnosticState::Ok => ("ok", StatusTone::Success),
        DiagnosticState::Missing => ("missing", StatusTone::Warning),
        DiagnosticState::Pending => ("pending", StatusTone::Info),
        DiagnosticState::Unreachable => ("unreachable", StatusTone::Error),
        DiagnosticState::Error => ("error", StatusTone::Error),
        DiagnosticState::Skipped => ("skipped", StatusTone::Warning),
    }
}

fn confidence_cell(confidence: f64) -> Cell<'static> {
    Cell::from(format!("{confidence:.2}")).style(score_style(confidence))
}

fn resolution_cell(status: &str) -> Cell<'_> {
    let tone = if status.starts_with("resolved") {
        StatusTone::Success
    } else if status == "unresolved" {
        StatusTone::Warning
    } else {
        StatusTone::Info
    };
    Cell::from(status_span(status, tone))
}

fn score_style(score: f64) -> Style {
    if score >= 0.8 {
        Style::new().fg(Color::Green).add_modifier(Modifier::BOLD)
    } else if score >= 0.5 {
        Style::new().fg(Color::Yellow)
    } else {
        Style::new().fg(Color::DarkGray)
    }
}

fn selected_row_style() -> Style {
    Style::new().bg(Color::Cyan).add_modifier(Modifier::BOLD)
}

fn previous_selection(selection: usize, row_count: usize) -> usize {
    if row_count == 0 {
        0
    } else if selection == 0 {
        row_count - 1
    } else {
        selection - 1
    }
}

fn next_selection(selection: usize, row_count: usize) -> usize {
    if row_count == 0 {
        0
    } else {
        (selection + 1) % row_count
    }
}

fn query_result_count(result: &QueryResult) -> usize {
    match result {
        QueryResult::Symbol(summary) => summary.symbols.len(),
        QueryResult::Semantic(summary) => summary.results.len(),
    }
}

fn impact_result_count(summary: &ImpactSummary) -> usize {
    summary.direct_callers.len() + summary.direct_callees.len()
}

fn call_path_result_count(summary: &CallPathSummary) -> usize {
    summary.paths.iter().map(|path| path.edges.len()).sum()
}

fn context_pack_row_count(pack: &ContextPack) -> usize {
    7 + pack.focus_symbols.len() + pack.files.len() + pack.notes.len()
}

fn debug_context_row_count(pack: &DebugContextPack) -> usize {
    pack.frames.len()
        + pack.call_paths_between_frames.len()
        + pack.likely_tests.len()
        + pack.notes.len()
}

fn progress_percent(progress: Option<&IndexProgress>) -> u16 {
    let Some(progress) = progress else {
        return 0;
    };
    let percent = progress.completed.saturating_mul(100) / progress.total.max(1);
    percent.min(100) as u16
}

fn progress_label(progress: Option<&IndexProgress>) -> String {
    match progress {
        Some(progress) => format!(
            "{} {}/{}",
            progress.phase, progress.completed, progress.total
        ),
        None => "starting 0/1".to_owned(),
    }
}

fn writer_scope(scope: IndexScope) -> WriterIndexScope {
    match scope {
        IndexScope::Full => WriterIndexScope::Full,
        IndexScope::Incremental => WriterIndexScope::Incremental,
    }
}

fn index_progress_from_writer(progress: WriterProgress) -> IndexProgress {
    IndexProgress {
        phase: writer_progress_phase(&progress.phase),
        completed: progress.completed,
        total: progress.total,
        message: progress.message,
    }
}

fn run_manual_quality_after_index(
    repo: &str,
    request: ManualIndexRequest,
    sender: &mpsc::Sender<IndexJobMessage>,
) -> Result<Option<ManualQualityJobSummary>, String> {
    if !matches!(request.mode, IndexMode::Semantic)
        || !LayeredEmbedConfig::from_env().quality_enabled
    {
        return Ok(None);
    }

    let _ = sender.send(IndexJobMessage::Progress(IndexProgress {
        phase: "quality_index",
        completed: 0,
        total: 1,
        message: "Submitting quality index job to writer service".to_owned(),
    }));
    let progress_sender = sender.clone();
    let response = WriterClient::from_env().submit_and_wait_with_progress(
        &WriterJob::IndexQuality {
            repo: repo.to_owned(),
        },
        move |progress| {
            let _ = progress_sender.send(IndexJobMessage::Progress(index_progress_from_writer(
                progress,
            )));
        },
    )?;
    manual_quality_summary_from_writer_response(response).map(Some)
}

fn writer_progress_phase(phase: &str) -> &'static str {
    match phase {
        "open" => "open",
        "discover" => "discover",
        "parse" => "parse",
        "resolve" => "resolve",
        "rust_analyzer" => "rust_analyzer",
        "sqlite" => "sqlite",
        "semantic" => "semantic",
        "quality_queue" => "quality_queue",
        "quality_index" => "quality_index",
        "vector_repair" => "vector_repair",
        "queue" => "queue",
        "start" => "start",
        _ => "writer",
    }
}

fn index_summary_from_writer_response(response: WriterJobResponse) -> Result<IndexSummary, String> {
    if !response.ok {
        return Err(response.message);
    }
    let data = response.data;
    Ok(IndexSummary {
        repository_id: string_field(&data, "repository_id"),
        repository_root: string_field(&data, "repository_root"),
        files_seen: usize_field(&data, "files_seen"),
        files_skipped_unchanged: usize_field(&data, "files_skipped_unchanged"),
        files: Vec::new(),
        chunks_seen: usize_field(&data, "chunks_seen"),
        chunks_excluded_from_embedding: usize_field(&data, "chunks_excluded_from_embedding"),
        sqlite_files_indexed: usize_field(&data, "sqlite_files_indexed"),
        sqlite_chunks_indexed: usize_field(&data, "sqlite_chunks_indexed"),
        sqlite_symbols_indexed: usize_field(&data, "sqlite_symbols_indexed"),
        sqlite_calls_indexed: usize_field(&data, "sqlite_calls_indexed"),
        sqlite_files_removed: usize_field(&data, "sqlite_files_removed"),
        rust_analyzer: RustAnalyzerEnrichmentSummary::Disabled {
            enable_env: "SYMDEX_RUST_ANALYZER".to_owned(),
        },
        embedding: EmbeddingSummary::SkippedNoChunks,
    })
}

fn manual_quality_summary_from_writer_response(
    response: WriterJobResponse,
) -> Result<ManualQualityJobSummary, String> {
    if !response.ok {
        return Err(response.message);
    }
    let data = response.data;
    Ok(ManualQualityJobSummary {
        quality_status: string_field(&data, "quality_status"),
        active_layer: string_field(&data, "active_layer"),
        activation_reason: string_field(&data, "activation_reason"),
        claimed_jobs: usize_field(&data, "claimed_jobs"),
        failed_jobs: usize_field(&data, "failed_jobs"),
        skipped_stale_jobs: usize_field(&data, "skipped_stale_jobs"),
        remaining_pending_jobs: usize_field(&data, "remaining_pending_jobs"),
    })
}

fn string_field(data: &serde_json::Value, key: &str) -> String {
    data.get(key)
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

fn usize_field(data: &serde_json::Value, key: &str) -> usize {
    data.get(key)
        .and_then(serde_json::Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or(0)
}

fn layer_readiness_percent(layer: &SemanticStatusLayerSummary) -> u16 {
    layer_count_percent(layer.current_chunks, layer.expected_chunks)
}

fn layer_count_percent(count: usize, total_chunks: usize) -> u16 {
    if total_chunks == 0 {
        return 0;
    }
    let percent = count.saturating_mul(100) / total_chunks;
    percent.min(100) as u16
}

fn layer_readiness_label(label: &str, layer: &SemanticStatusLayerSummary, percent: u16) -> String {
    format!(
        "{label} {}/{} {percent}%",
        layer.current_chunks, layer.expected_chunks
    )
}

fn line_range(start: usize, end: usize) -> String {
    format!("{start}-{end}")
}

fn optional_line_range(start: Option<usize>, end: Option<usize>) -> String {
    match (start, end) {
        (Some(start), Some(end)) => line_range(start, end),
        _ => "<unknown>".to_owned(),
    }
}

fn runtime_line_label(line: Option<usize>, column: Option<usize>) -> String {
    match (line, column) {
        (Some(line), Some(column)) => format!("{line}:{column}"),
        (Some(line), None) => line.to_string(),
        _ => "-".to_owned(),
    }
}

fn provenance_label(provenance: Option<&EvidenceProvenance>) -> String {
    let Some(provenance) = provenance else {
        return "none".to_owned();
    };
    let run = provenance.index_run_id.as_deref().unwrap_or("run?");
    let parser = provenance.parser_version.as_deref().unwrap_or("parser?");
    let hash = provenance
        .content_hash
        .as_deref()
        .map(short_hash)
        .unwrap_or("hash?".to_owned());
    format!("{run} {parser} {hash}")
}

fn freshness_cell(freshness: EvidenceFreshness) -> Cell<'static> {
    let tone = match freshness {
        EvidenceFreshness::Fresh => StatusTone::Success,
        EvidenceFreshness::Stale | EvidenceFreshness::Deleted | EvidenceFreshness::Missing => {
            StatusTone::Warning
        }
        EvidenceFreshness::Unknown => StatusTone::Dim,
    };
    Cell::from(status_span(freshness.label(), tone))
}

fn matched_cell(matched: bool) -> Cell<'static> {
    if matched {
        Cell::from(status_span("matched", StatusTone::Success))
    } else {
        Cell::from(status_span("unmapped", StatusTone::Warning))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum View {
    Overview,
    Indexing,
    Storage,
    Diagnostics,
    Query,
    Graph,
    Evidence,
}

impl View {
    fn tabs() -> [&'static str; 7] {
        [
            "Overview", "Index", "Storage", "Doctor", "Query", "Calls", "Impact",
        ]
    }

    fn tab_index(self) -> usize {
        match self {
            Self::Overview => 0,
            Self::Indexing => 1,
            Self::Storage => 2,
            Self::Diagnostics => 3,
            Self::Query => 4,
            Self::Graph => 5,
            Self::Evidence => 6,
        }
    }

    fn footer_label(self) -> &'static str {
        match self {
            Self::Overview => "overview",
            Self::Indexing => "index",
            Self::Storage => "storage",
            Self::Diagnostics => "doctor",
            Self::Query => "query",
            Self::Graph => "calls",
            Self::Evidence => "impact",
        }
    }

    fn footer_keys(self) -> &'static [(&'static str, &'static str)] {
        match self {
            Self::Overview => &[("[ ]", "tabs"), ("r", "refresh"), ("q", "quit")],
            Self::Indexing => &[
                ("[ ]", "tabs"),
                ("o", "offline"),
                ("s", "semantic"),
                ("c", "continuous"),
                ("r", "refresh"),
                ("q", "quit"),
            ],
            Self::Storage => &[
                ("[ ]", "tabs"),
                ("Tab", "storage view"),
                ("Up/Down", "select"),
                ("f", "fix stale"),
                ("r", "refresh"),
                ("q", "quit"),
            ],
            Self::Diagnostics => &[
                ("[ ]", "tabs"),
                ("Enter", "run/details"),
                ("Up/Down", "select"),
                ("r", "rerun"),
                ("q", "quit"),
            ],
            Self::Query => &[
                ("[ ]", "tabs"),
                ("Tab", "mode"),
                ("type", "query"),
                ("Enter", "run"),
                ("Up/Down", "select"),
                ("Esc", "clear/back"),
            ],
            Self::Graph => &[
                ("[ ]", "tabs"),
                ("Tab", "direction"),
                ("type", "symbol"),
                ("Enter", "run"),
                ("Up/Down", "select"),
                ("Esc", "clear/back"),
            ],
            Self::Evidence => &[
                ("[ ]", "tabs"),
                ("Tab", "mode"),
                ("type", "input"),
                ("Enter", "run"),
                ("Up/Down", "select"),
                ("Esc", "clear/back"),
            ],
        }
    }

    fn next(self) -> Self {
        match self {
            Self::Overview => Self::Indexing,
            Self::Indexing => Self::Storage,
            Self::Storage => Self::Diagnostics,
            Self::Diagnostics => Self::Query,
            Self::Query => Self::Graph,
            Self::Graph => Self::Evidence,
            Self::Evidence => Self::Overview,
        }
    }

    fn previous(self) -> Self {
        match self {
            Self::Overview => Self::Evidence,
            Self::Indexing => Self::Overview,
            Self::Storage => Self::Indexing,
            Self::Diagnostics => Self::Storage,
            Self::Query => Self::Diagnostics,
            Self::Graph => Self::Query,
            Self::Evidence => Self::Graph,
        }
    }

    fn title(self) -> &'static str {
        match self {
            Self::Overview => "Overview",
            Self::Indexing => "Index",
            Self::Storage => "Storage",
            Self::Diagnostics => "Doctor",
            Self::Query => "Query",
            Self::Graph => "Calls",
            Self::Evidence => "Impact",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StatusTone {
    Success,
    Warning,
    Error,
    Info,
    Dim,
}

fn status_span(label: &str, tone: StatusTone) -> Span<'_> {
    Span::styled(label, tone_style(tone).add_modifier(Modifier::BOLD))
}

fn tone_style(tone: StatusTone) -> Style {
    let color = match tone {
        StatusTone::Success => Color::Green,
        StatusTone::Warning => Color::Yellow,
        StatusTone::Error => Color::Red,
        StatusTone::Info => Color::Cyan,
        StatusTone::Dim => Color::DarkGray,
    };
    Style::new().fg(color)
}

fn readable_gauge_style(tone: StatusTone) -> Style {
    tone_style(tone)
        .bg(Color::DarkGray)
        .add_modifier(Modifier::BOLD)
}

enum DiagnosticsState {
    Idle,
    Running,
    Completed(DiagnosticReport),
    Failed(String),
}

struct StorageExplorerState {
    mode: StorageMode,
    explorer: StorageStatus,
    coverage: CoverageStatus,
    outline: OutlineStatus,
    calls: CallResolutionStatus,
    embeddings: EmbeddingCoverageStatus,
    runs: IndexRunsTimelineStatus,
    freshness: FreshnessStatus,
    neighborhood: SemanticNeighborhoodStatus,
    health: CrossStoreHealthStatus,
    selection: usize,
}

impl StorageExplorerState {
    #[allow(clippy::too_many_arguments)]
    fn completed(
        explorer: StorageExplorerSummary,
        coverage: IndexCoverageSummary,
        outline: SymbolOutlineSummary,
        calls: CallResolutionSummary,
        embeddings: EmbeddingCoverageSummary,
        runs: IndexRunsTimelineSummary,
        freshness: FreshnessSummary,
        neighborhood: SemanticNeighborhoodSummary,
        health: CrossStoreHealthSummary,
    ) -> Self {
        Self {
            mode: StorageMode::Explorer,
            explorer: StorageStatus::Completed(explorer),
            coverage: CoverageStatus::Completed(coverage),
            outline: OutlineStatus::Completed(outline),
            calls: CallResolutionStatus::Completed(calls),
            embeddings: EmbeddingCoverageStatus::Completed(embeddings),
            runs: IndexRunsTimelineStatus::Completed(runs),
            freshness: FreshnessStatus::Completed(Box::new(freshness)),
            neighborhood: SemanticNeighborhoodStatus::Completed(neighborhood),
            health: CrossStoreHealthStatus::Completed(health),
            selection: 0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StorageMode {
    Explorer,
    Coverage,
    Outline,
    Calls,
    Embeddings,
    Runs,
    Freshness,
    Neighborhood,
    Health,
}

impl StorageMode {
    fn tabs() -> [&'static str; 9] {
        [
            "Store", "Files", "Syms", "Calls", "Vecs", "Runs", "Fresh", "Near", "Health",
        ]
    }

    fn tab_index(self) -> usize {
        match self {
            Self::Explorer => 0,
            Self::Coverage => 1,
            Self::Outline => 2,
            Self::Calls => 3,
            Self::Embeddings => 4,
            Self::Runs => 5,
            Self::Freshness => 6,
            Self::Neighborhood => 7,
            Self::Health => 8,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Explorer => "storage overview",
            Self::Coverage => "index coverage",
            Self::Outline => "symbol outline",
            Self::Calls => "call resolution",
            Self::Embeddings => "embedding coverage",
            Self::Runs => "index runs timeline",
            Self::Freshness => "evidence freshness",
            Self::Neighborhood => "semantic neighborhood",
            Self::Health => "cross-store health",
        }
    }

    fn toggled(self) -> Self {
        match self {
            Self::Explorer => Self::Coverage,
            Self::Coverage => Self::Outline,
            Self::Outline => Self::Calls,
            Self::Calls => Self::Embeddings,
            Self::Embeddings => Self::Runs,
            Self::Runs => Self::Freshness,
            Self::Freshness => Self::Neighborhood,
            Self::Neighborhood => Self::Health,
            Self::Health => Self::Explorer,
        }
    }

    fn previous(self) -> Self {
        match self {
            Self::Explorer => Self::Health,
            Self::Coverage => Self::Explorer,
            Self::Outline => Self::Coverage,
            Self::Calls => Self::Outline,
            Self::Embeddings => Self::Calls,
            Self::Runs => Self::Embeddings,
            Self::Freshness => Self::Runs,
            Self::Neighborhood => Self::Freshness,
            Self::Health => Self::Neighborhood,
        }
    }
}

enum StorageStatus {
    Completed(StorageExplorerSummary),
    Failed(String),
}

enum CoverageStatus {
    Completed(IndexCoverageSummary),
    Failed(String),
}

enum OutlineStatus {
    Completed(SymbolOutlineSummary),
    Failed(String),
}

enum CallResolutionStatus {
    Completed(CallResolutionSummary),
    Failed(String),
}

enum EmbeddingCoverageStatus {
    Completed(EmbeddingCoverageSummary),
    Failed(String),
}

enum IndexRunsTimelineStatus {
    Completed(IndexRunsTimelineSummary),
    Failed(String),
}

enum FreshnessStatus {
    Completed(Box<FreshnessSummary>),
    Failed(String),
}

enum SemanticNeighborhoodStatus {
    Completed(SemanticNeighborhoodSummary),
    Failed(String),
}

enum CrossStoreHealthStatus {
    Completed(CrossStoreHealthSummary),
    Failed(String),
}

#[derive(Debug, Clone)]
struct StorageDisplayRow {
    layer: &'static str,
    metric: &'static str,
    value: String,
    status: &'static str,
    tone: StatusTone,
    detail: &'static str,
}

struct QueryWorkbenchState {
    mode: QueryMode,
    input: String,
    status: QueryStatus,
    selection: usize,
}

impl Default for QueryWorkbenchState {
    fn default() -> Self {
        Self {
            mode: QueryMode::Symbol,
            input: String::new(),
            status: QueryStatus::Idle,
            selection: 0,
        }
    }
}

impl QueryWorkbenchState {
    fn result_count(&self) -> usize {
        match &self.status {
            QueryStatus::Completed(result) => query_result_count(result),
            _ => 0,
        }
    }
}

enum QueryStatus {
    Idle,
    Running,
    Completed(QueryResult),
    Failed(String),
}

struct GraphBrowserState {
    direction: CallDirection,
    input: String,
    status: GraphStatus,
    selection: usize,
}

impl Default for GraphBrowserState {
    fn default() -> Self {
        Self {
            direction: CallDirection::Callers,
            input: String::new(),
            status: GraphStatus::Idle,
            selection: 0,
        }
    }
}

impl GraphBrowserState {
    fn result_count(&self) -> usize {
        match &self.status {
            GraphStatus::Completed(summary) => summary.rows.len(),
            _ => 0,
        }
    }
}

enum GraphStatus {
    Idle,
    Running,
    Completed(CallGraphSummary),
    Failed(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EvidenceMode {
    Impact,
    CallPath,
    ContextPack,
    DebugContext,
}

impl EvidenceMode {
    fn label(self) -> &'static str {
        match self {
            Self::Impact => "impact",
            Self::CallPath => "call path",
            Self::ContextPack => "context pack",
            Self::DebugContext => "debug context",
        }
    }

    fn toggled(self) -> Self {
        match self {
            Self::Impact => Self::CallPath,
            Self::CallPath => Self::ContextPack,
            Self::ContextPack => Self::DebugContext,
            Self::DebugContext => Self::Impact,
        }
    }
}

struct EvidenceViewerState {
    mode: EvidenceMode,
    input: String,
    status: EvidenceStatus,
    selection: usize,
}

impl Default for EvidenceViewerState {
    fn default() -> Self {
        Self {
            mode: EvidenceMode::Impact,
            input: String::new(),
            status: EvidenceStatus::Idle,
            selection: 0,
        }
    }
}

impl EvidenceViewerState {
    fn result_count(&self) -> usize {
        match &self.status {
            EvidenceStatus::Completed(EvidenceResult::Impact(summary)) => {
                impact_result_count(summary)
            }
            EvidenceStatus::Completed(EvidenceResult::CallPath(summary)) => {
                call_path_result_count(summary)
            }
            EvidenceStatus::Completed(EvidenceResult::ContextPack(pack)) => {
                context_pack_row_count(pack)
            }
            EvidenceStatus::Completed(EvidenceResult::DebugContext(pack)) => {
                debug_context_row_count(pack)
            }
            _ => 0,
        }
    }
}

#[allow(clippy::large_enum_variant)]
enum EvidenceStatus {
    Idle,
    Running,
    Completed(EvidenceResult),
    Failed(String),
}

enum EvidenceResult {
    Impact(ImpactSummary),
    CallPath(CallPathSummary),
    ContextPack(ContextPack),
    DebugContext(DebugContextPack),
}

enum IndexJobMessage {
    Progress(IndexProgress),
    Finished(Result<Box<ManualIndexJobSummary>, String>),
}

struct ManualIndexJobSummary {
    index: Box<IndexSummary>,
    quality: Option<Box<ManualQualityJobSummary>>,
    quality_error: Option<String>,
}

struct ManualQualityJobSummary {
    quality_status: String,
    active_layer: String,
    activation_reason: String,
    claimed_jobs: usize,
    failed_jobs: usize,
    skipped_stale_jobs: usize,
    remaining_pending_jobs: usize,
}

#[derive(Clone, Copy)]
enum StatusRefreshScope {
    Full,
    Index,
}

struct StatusRefreshSnapshot {
    scope: StatusRefreshScope,
    status: RepositoryStatus,
    semantic_status: SemanticStatusSummary,
    explorer: Option<StorageStatus>,
    coverage: Option<CoverageStatus>,
    outline: Option<OutlineStatus>,
    calls: Option<CallResolutionStatus>,
    embeddings: Option<EmbeddingCoverageStatus>,
    runs: Option<IndexRunsTimelineStatus>,
    freshness: Option<FreshnessStatus>,
    neighborhood: Option<SemanticNeighborhoodStatus>,
    health: Option<CrossStoreHealthStatus>,
}

enum StatusRefreshMessage {
    Finished {
        result: Result<Box<StatusRefreshSnapshot>, String>,
        success_message: Option<String>,
    },
}

#[allow(dead_code)]
enum ContinuousIndexMessage {
    Event(Box<ContinuousIndexEvent>),
    Failed(String),
    Stopped,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ContinuousIndexState {
    enabled: bool,
    status: ContinuousIndexStatus,
    files_seen: usize,
    queued_events: usize,
    last_reindexed_file: Option<String>,
    latest_error: Option<String>,
    active_layer: Option<String>,
    quality_status: Option<String>,
    activation_reason: Option<String>,
    quality_pending_jobs: usize,
    quality_running_jobs: usize,
    quality_failed_jobs: usize,
    quality_stale_jobs: usize,
    latest_quality_error: Option<String>,
    attached_clients: usize,
    client_kinds: Vec<String>,
    shutdown_after_seconds: Option<u64>,
}

impl Default for ContinuousIndexState {
    fn default() -> Self {
        Self {
            enabled: false,
            status: ContinuousIndexStatus::Off,
            files_seen: 0,
            queued_events: 0,
            last_reindexed_file: None,
            latest_error: None,
            active_layer: None,
            quality_status: None,
            activation_reason: None,
            quality_pending_jobs: 0,
            quality_running_jobs: 0,
            quality_failed_jobs: 0,
            quality_stale_jobs: 0,
            latest_quality_error: None,
            attached_clients: 0,
            client_kinds: Vec::new(),
            shutdown_after_seconds: None,
        }
    }
}

impl ContinuousIndexState {
    fn is_on(&self) -> bool {
        self.enabled && !matches!(self.status, ContinuousIndexStatus::Off)
    }

    fn status_label(&self) -> &'static str {
        self.status.label()
    }

    fn status_tone(&self) -> StatusTone {
        self.status.tone()
    }

    fn summary(&self) -> String {
        let client_kinds = if self.client_kinds.is_empty() {
            "<none>".to_owned()
        } else {
            self.client_kinds.join(",")
        };
        let shutdown_after = self
            .shutdown_after_seconds
            .map(|seconds| seconds.to_string())
            .unwrap_or_else(|| "<none>".to_owned());
        let base = format!(
            "state={} files={} queued={} clients={} kinds={} shutdown_after={} last={} err={}",
            self.status.label(),
            self.files_seen,
            self.queued_events,
            self.attached_clients,
            client_kinds,
            shutdown_after,
            self.last_reindexed_file.as_deref().unwrap_or("<none>"),
            self.latest_error.as_deref().unwrap_or("<none>")
        );
        if self.active_layer.is_none()
            && self.quality_status.is_none()
            && self.activation_reason.is_none()
            && self.quality_pending_jobs == 0
            && self.quality_running_jobs == 0
            && self.quality_failed_jobs == 0
            && self.quality_stale_jobs == 0
            && self.latest_quality_error.is_none()
        {
            return base;
        }
        format!(
            "{} layer={} quality={} qjobs={}/{}/{}/{} reason={} qerr={}",
            base,
            self.active_layer.as_deref().unwrap_or("<none>"),
            self.quality_status.as_deref().unwrap_or("<none>"),
            self.quality_pending_jobs,
            self.quality_running_jobs,
            self.quality_failed_jobs,
            self.quality_stale_jobs,
            self.activation_reason.as_deref().unwrap_or("<none>"),
            self.latest_quality_error.as_deref().unwrap_or("<none>")
        )
    }

    fn apply_quality_state(&mut self, state: &ContinuousQualityState) {
        self.active_layer = Some(state.active_layer.clone());
        self.quality_status = Some(state.quality_status.clone());
        self.activation_reason = state.activation_reason.clone();
        self.quality_pending_jobs = state.pending_jobs;
        self.quality_running_jobs = state.running_jobs;
        self.quality_failed_jobs = state.failed_jobs;
        self.quality_stale_jobs = state.skipped_stale_jobs;
    }
}

fn continuous_activity_frame(tick: usize) -> &'static str {
    const FRAMES: [&str; 4] = ["-", "\\", "|", "/"];
    FRAMES[tick % FRAMES.len()]
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ContinuousIndexStatus {
    Off,
    Starting,
    Watching,
    Pending,
    Indexing,
    Failed,
}

impl ContinuousIndexStatus {
    fn label(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Starting => "starting",
            Self::Watching => "on",
            Self::Pending => "pending",
            Self::Indexing => "indexing",
            Self::Failed => "error",
        }
    }

    fn tone(self) -> StatusTone {
        match self {
            Self::Off => StatusTone::Dim,
            Self::Starting | Self::Watching | Self::Indexing => StatusTone::Info,
            Self::Pending => StatusTone::Warning,
            Self::Failed => StatusTone::Error,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use crossterm::event::KeyCode;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::layout::Position;
    use ratatui::style::Color;
    use symdex_core::{SemanticLayer, SemanticLayerStatus};
    use symdex_diagnostics::{DiagnosticCheck, DiagnosticReport, DiagnosticState};
    use symdex_index::IndexScope;
    use symdex_query::{
        CallDirection, CallGraphSummary, CallPathSummary, DebugContextLimits, DebugContextPack,
        DebugFrameCallPath, DebugFrameMatch, EvidenceTrust, FileFreshnessRow, FreshnessSummary,
        ImpactCallEvidence, ImpactSummary, QueryMode, QueryResult, RuntimeFrame,
        SymbolSearchSummary,
    };
    use symdex_store::{
        CallPath, CallPathEdge, CallResolutionBucket, CallResolutionEdgeRow, CallResolutionSummary,
        CallSearchRow, ChunkVectorStatus, ConfidenceBucket, ContextPack, ContextPackLimits,
        CrossStoreHealthSummary, EmbeddingCoverageSummary, EmbeddingExclusionRow,
        EvidenceFreshness, EvidenceProvenance, FileCallDetailRow, FileChunkDetailRow,
        FileCoverageRow, FileCoverageStatus, FileDetailSummary, FileSymbolDetailRow,
        IndexCoverageSummary, IndexRunTimelineRow, IndexRunsTimelineSummary,
        QualityGenerationProgress, RepositoryStatus, SemanticNeighborhoodRow,
        SemanticNeighborhoodSummary, SqliteStorageSummary, StorageExplorerSummary,
        StorageHealthRow, StorageHealthStatus, SymbolOutlineRow, SymbolOutlineSummary,
        SymbolSearchRow, VectorStorageProjection,
    };
    use symdex_writer::WriterJobResponse;

    use crate::{
        App, ContinuousIndexStatus, DiagnosticsState, EvidenceMode, EvidenceResult, EvidenceStatus,
        GraphStatus, INDEX_STATUS_REFRESH_INTERVAL, IndexMode, ManualIndexRequest, QueryStatus,
        Screen, StorageExplorerState, StorageMode, UiAction, View, WATCHER_STATUS_REFRESH_INTERVAL,
        continuous_activity_frame, layer_count_percent, layer_readiness_percent,
        manual_quality_summary_from_writer_response, parse_call_path_input, progress_percent,
        reduce_screen, render,
    };

    #[test]
    fn renders_dashboard_status() {
        let status = RepositoryStatus {
            repository_id: "repo".to_owned(),
            current_ref_id: Some("ref".to_owned()),
            current_ref_kind: Some("branch".to_owned()),
            current_ref_name: Some("main".to_owned()),
            current_head_oid: Some("0123456789abcdef0123456789abcdef01234567".to_owned()),
            files_indexed: 2,
            chunks_indexed: 3,
            symbols_indexed: 4,
            calls_indexed: 5,
            last_indexed_at: Some("123".to_owned()),
            embedding_model: Some("nomic-embed-text".to_owned()),
            embedding_dimension: Some(768),
        };
        let app = App::from_status("/tmp/repo", "repo", status);
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("terminal should build");

        render(&mut terminal, &app).expect("render should succeed");

        let buffer = terminal.backend().buffer();
        let rendered = format!("{buffer:?}");
        assert!(rendered.contains("symdex TUI"));
        assert!(rendered.contains("Repository Status"));
        assert!(rendered.contains("repo"));
        assert!(rendered.contains("indexed"));
        assert!(rendered.contains("Index"));
        assert!(rendered.contains("Count"));
        assert!(rendered.contains("Files indexed"));
        assert!(rendered.contains("Service"));
        assert!(rendered.contains("State"));
        assert!(rendered.contains("SQLite"));
        assert!(rendered.contains("ready"));
        assert!(rendered.contains("Operational Focus"));
        assert!(rendered.contains("Mode Snapshot"));
        assert!(rendered.contains("nomic-embed-text"));
        assert_eq!(cell_fg_for_text(buffer, "ready", None), Some(Color::Green));
    }

    #[test]
    fn renders_dashboard_semantic_status() {
        let app = App::from_status("/tmp/repo", "repo", sample_status());
        let backend = TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).expect("terminal should build");

        render(&mut terminal, &app).expect("render should succeed");

        let buffer = terminal.backend().buffer();
        let rendered = format!("{buffer:?}");
        assert!(rendered.contains("Semantic"));
        assert!(rendered.contains("fast_ready"));
        assert!(rendered.contains("QJobs"));
    }

    #[test]
    fn renders_major_view_tabs_with_active_style() {
        let app = App::from_status("/tmp/repo", "repo", sample_status());
        let backend = TestBackend::new(100, 24);
        let mut terminal = Terminal::new(backend).expect("terminal should build");

        render(&mut terminal, &app).expect("render should succeed");

        let buffer = terminal.backend().buffer();
        let rendered = format!("{buffer:?}");
        assert!(rendered.contains("Overview"));
        assert!(rendered.contains("Index"));
        assert!(rendered.contains("Storage"));
        assert!(rendered.contains("Doctor"));
        assert!(rendered.contains("Query"));
        assert!(rendered.contains("Calls"));
        assert!(rendered.contains("Impact"));
        assert_eq!(
            cell_fg_for_text(buffer, "Overview", Some(1)),
            Some(Color::Cyan)
        );
    }

    #[test]
    fn renders_view_specific_footer_help() {
        let mut app = App::from_status("/tmp/repo", "repo", sample_status());
        app.view = View::Query;
        let backend = TestBackend::new(120, 24);
        let mut terminal = Terminal::new(backend).expect("terminal should build");

        render(&mut terminal, &app).expect("render should succeed");

        let buffer = terminal.backend().buffer();
        let rendered = format!("{buffer:?}");
        assert!(rendered.contains("Keys"));
        assert!(rendered.contains("keys"));
        assert!(rendered.contains("query"));
        assert!(rendered.contains("[Tab]"));
        assert!(rendered.contains("mode"));
        assert!(rendered.contains("[Enter]"));
        assert!(rendered.contains("run"));
        assert!(rendered.contains("Status"));
        assert!(rendered.contains("status:"));
        assert_eq!(cell_fg_for_text(buffer, "Keys", None), Some(Color::Cyan));
    }

    #[test]
    fn tab_toggles_query_mode_without_switching_major_view() {
        let mut app = App::from_status("/tmp/repo", "repo", sample_status());
        app.view = View::Query;
        app.query.mode = QueryMode::Symbol;

        assert!(!app.handle_key(KeyCode::Tab));

        assert_eq!(app.view, View::Query);
        assert_eq!(app.query.mode, QueryMode::Semantic);
        assert_eq!(app.message, "Query mode set to semantic.");
    }

    #[test]
    fn tab_toggles_index_scope_on_indexing_view() {
        let mut app = App::from_status("/tmp/repo", "repo", sample_status());
        app.view = View::Indexing;

        assert_eq!(app.index_scope, IndexScope::Incremental);

        assert!(!app.handle_key(KeyCode::Tab));

        assert_eq!(app.view, View::Indexing);
        assert_eq!(app.index_scope, IndexScope::Full);
        assert_eq!(app.message, "Index scope set to full.");
    }

    #[test]
    fn brackets_switch_primary_tabs_from_text_entry_views() {
        let mut app = App::from_status("/tmp/repo", "repo", sample_status());
        app.view = View::Query;
        app.query.input = "retry".to_owned();

        assert!(!app.handle_key(KeyCode::Char(']')));

        assert_eq!(app.view, View::Graph);
        assert_eq!(app.query.input, "retry");
        assert_eq!(app.message, "Calls tab selected.");

        assert!(!app.handle_key(KeyCode::Char('[')));

        assert_eq!(app.view, View::Query);
        assert_eq!(app.query.input, "retry");
        assert_eq!(app.message, "Query tab selected.");
    }

    #[test]
    fn letter_keys_remain_available_in_text_entry_views() {
        let mut app = App::from_status("/tmp/repo", "repo", sample_status());
        app.view = View::Query;

        assert!(!app.handle_key(KeyCode::Char('g')));

        assert_eq!(app.view, View::Query);
        assert_eq!(app.query.input, "g");
    }

    #[test]
    fn shift_tab_toggles_storage_to_previous_mode() {
        let mut app = App::from_status("/tmp/repo", "repo", sample_status());
        app.view = View::Storage;

        assert!(!app.handle_key(KeyCode::BackTab));

        assert_eq!(app.view, View::Storage);
        assert_eq!(app.storage.mode, StorageMode::Health);
        assert_eq!(app.message, "Storage mode set to cross-store health.");
    }

    #[test]
    fn renders_doctor_footer_enter_details_help() {
        let mut app = App::from_status("/tmp/repo", "repo", sample_status());
        app.view = View::Diagnostics;
        let backend = TestBackend::new(120, 24);
        let mut terminal = Terminal::new(backend).expect("terminal should build");

        render(&mut terminal, &app).expect("render should succeed");

        let rendered = format!("{:?}", terminal.backend().buffer());
        assert!(rendered.contains("[Enter]"));
        assert!(rendered.contains("run/details"));
        assert!(rendered.contains("[r]"));
        assert!(rendered.contains("rerun"));
    }

    #[test]
    fn renders_storage_explorer_metadata_without_source_text() {
        let mut app = App::from_status("/tmp/repo", "repo", sample_status());
        app.view = View::Storage;
        app.storage = StorageExplorerState::completed(
            sample_storage_summary(),
            sample_index_coverage_summary(),
            sample_symbol_outline_summary(),
            sample_call_resolution_summary(),
            sample_embedding_coverage_summary(),
            sample_index_runs_timeline_summary(),
            sample_freshness_summary(),
            sample_semantic_neighborhood_summary(),
            sample_cross_store_health_summary(),
        );
        let backend = TestBackend::new(140, 30);
        let mut terminal = Terminal::new(backend).expect("terminal should build");

        render(&mut terminal, &app).expect("render should succeed");

        let buffer = terminal.backend().buffer();
        let rendered = format!("{buffer:?}");
        assert!(rendered.contains("Storage Views"));
        assert!(rendered.contains("Store"));
        assert!(rendered.contains("Files"));
        assert!(rendered.contains("Syms"));
        assert!(rendered.contains("Vecs"));
        assert!(rendered.contains("Near"));
        assert!(rendered.contains("Storage Explorer"));
        assert!(rendered.contains("SQLite"));
        assert!(rendered.contains("sqlite-vec"));
        assert!(rendered.contains("missing-vector"));
        assert!(rendered.contains("Storage Detail"));
        assert!(rendered.contains("Health"));
        assert!(!rendered.contains("source_text"));
        assert_eq!(
            cell_fg_for_text(buffer, "missing-vector", None),
            Some(Color::Yellow)
        );
        assert_eq!(cell_fg_for_text(buffer, "Store", None), Some(Color::Cyan));
    }

    #[test]
    fn storage_explorer_selection_drives_detail_panel() {
        let mut app = App::from_status("/tmp/repo", "repo", sample_status());
        app.view = View::Storage;
        app.storage = StorageExplorerState::completed(
            sample_storage_summary(),
            sample_index_coverage_summary(),
            sample_symbol_outline_summary(),
            sample_call_resolution_summary(),
            sample_embedding_coverage_summary(),
            sample_index_runs_timeline_summary(),
            sample_freshness_summary(),
            sample_semantic_neighborhood_summary(),
            sample_cross_store_health_summary(),
        );

        assert_eq!(app.storage.selection, 0);
        assert!(!app.handle_key(KeyCode::Down));
        assert_eq!(app.storage.selection, 1);
        assert_eq!(app.message, "Storage row selection moved.");

        let backend = TestBackend::new(140, 24);
        let mut terminal = Terminal::new(backend).expect("terminal should build");
        render(&mut terminal, &app).expect("render should succeed");

        let rendered = format!("{:?}", terminal.backend().buffer());
        assert!(rendered.contains("Selected"));
        assert!(rendered.contains("SQLite / files"));
        assert!(rendered.contains("Indexed file rows in SQLite."));
    }

    #[test]
    fn storage_fix_stale_requests_confirmed_incremental_semantic_index() {
        let mut app = App::from_status("/tmp/repo", "repo", sample_status());
        app.view = View::Storage;
        app.storage = StorageExplorerState::completed(
            sample_storage_summary(),
            sample_index_coverage_summary(),
            sample_symbol_outline_summary(),
            sample_call_resolution_summary(),
            sample_embedding_coverage_summary(),
            sample_index_runs_timeline_summary(),
            sample_freshness_summary(),
            sample_semantic_neighborhood_summary(),
            sample_cross_store_health_summary(),
        );
        app.storage.mode = StorageMode::Explorer;
        app.index_scope = IndexScope::Full;

        assert!(!app.handle_key(KeyCode::Char('f')));

        assert_eq!(app.view, View::Indexing);
        assert_eq!(app.storage.mode, StorageMode::Freshness);
        assert_eq!(app.index_scope, IndexScope::Incremental);
        assert!(matches!(
            app.screen,
            Screen::ConfirmIndex(ManualIndexRequest {
                mode: IndexMode::Semantic,
                scope: IndexScope::Incremental,
            })
        ));
        assert!(
            app.message
                .contains("Confirm semantic incremental indexing")
        );
    }

    #[test]
    fn storage_fix_stale_reports_when_freshness_is_current() {
        let mut app = App::from_status("/tmp/repo", "repo", sample_status());
        app.view = View::Storage;
        let mut freshness = sample_freshness_summary();
        for file in &mut freshness.files {
            file.freshness = EvidenceFreshness::Fresh;
        }
        app.storage = StorageExplorerState::completed(
            sample_storage_summary(),
            sample_index_coverage_summary(),
            sample_symbol_outline_summary(),
            sample_call_resolution_summary(),
            sample_embedding_coverage_summary(),
            sample_index_runs_timeline_summary(),
            freshness,
            sample_semantic_neighborhood_summary(),
            sample_cross_store_health_summary(),
        );

        assert!(!app.handle_key(KeyCode::Char('f')));

        assert_eq!(app.view, View::Storage);
        assert_eq!(app.storage.mode, StorageMode::Freshness);
        assert!(matches!(app.screen, Screen::Dashboard));
        assert_eq!(
            app.message,
            "No stale, deleted, or missing evidence rows need repair."
        );
    }

    #[test]
    fn renders_storage_explorer_at_80x24() {
        let mut app = App::from_status("/tmp/repo", "repo", sample_status());
        app.view = View::Storage;
        app.storage = StorageExplorerState::completed(
            sample_storage_summary(),
            sample_index_coverage_summary(),
            sample_symbol_outline_summary(),
            sample_call_resolution_summary(),
            sample_embedding_coverage_summary(),
            sample_index_runs_timeline_summary(),
            sample_freshness_summary(),
            sample_semantic_neighborhood_summary(),
            sample_cross_store_health_summary(),
        );
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("terminal should build");

        render(&mut terminal, &app).expect("render should succeed");

        let rendered = format!("{:?}", terminal.backend().buffer());
        assert!(rendered.contains("symdex TUI"));
        assert!(rendered.contains("Storage"));
        assert!(rendered.contains("Layer"));
        assert!(rendered.contains("Metric"));
        assert!(rendered.contains("Status"));
    }

    #[test]
    fn storage_tab_toggles_to_index_coverage() {
        let mut app = App::from_status("/tmp/repo", "repo", sample_status());
        app.view = View::Storage;
        app.storage = StorageExplorerState::completed(
            sample_storage_summary(),
            sample_index_coverage_summary(),
            sample_symbol_outline_summary(),
            sample_call_resolution_summary(),
            sample_embedding_coverage_summary(),
            sample_index_runs_timeline_summary(),
            sample_freshness_summary(),
            sample_semantic_neighborhood_summary(),
            sample_cross_store_health_summary(),
        );
        app.storage.selection = 2;

        assert_eq!(app.storage.mode, StorageMode::Explorer);
        assert!(!app.handle_key(KeyCode::Tab));

        assert_eq!(app.storage.mode, StorageMode::Coverage);
        assert_eq!(app.storage.selection, 0);
        assert_eq!(app.message, "Storage mode set to index coverage.");
    }

    #[test]
    fn storage_tab_cycles_to_symbol_outline() {
        let mut app = App::from_status("/tmp/repo", "repo", sample_status());
        app.view = View::Storage;
        app.storage = StorageExplorerState::completed(
            sample_storage_summary(),
            sample_index_coverage_summary(),
            sample_symbol_outline_summary(),
            sample_call_resolution_summary(),
            sample_embedding_coverage_summary(),
            sample_index_runs_timeline_summary(),
            sample_freshness_summary(),
            sample_semantic_neighborhood_summary(),
            sample_cross_store_health_summary(),
        );

        assert!(!app.handle_key(KeyCode::Tab));
        assert_eq!(app.storage.mode, StorageMode::Coverage);
        assert!(!app.handle_key(KeyCode::Tab));

        assert_eq!(app.storage.mode, StorageMode::Outline);
        assert_eq!(app.message, "Storage mode set to symbol outline.");
    }

    #[test]
    fn storage_tab_cycles_to_call_resolution() {
        let mut app = App::from_status("/tmp/repo", "repo", sample_status());
        app.view = View::Storage;
        app.storage = StorageExplorerState::completed(
            sample_storage_summary(),
            sample_index_coverage_summary(),
            sample_symbol_outline_summary(),
            sample_call_resolution_summary(),
            sample_embedding_coverage_summary(),
            sample_index_runs_timeline_summary(),
            sample_freshness_summary(),
            sample_semantic_neighborhood_summary(),
            sample_cross_store_health_summary(),
        );

        assert!(!app.handle_key(KeyCode::Tab));
        assert!(!app.handle_key(KeyCode::Tab));
        assert_eq!(app.storage.mode, StorageMode::Outline);
        assert!(!app.handle_key(KeyCode::Tab));

        assert_eq!(app.storage.mode, StorageMode::Calls);
        assert_eq!(app.message, "Storage mode set to call resolution.");
    }

    #[test]
    fn storage_tab_cycles_to_embedding_coverage() {
        let mut app = App::from_status("/tmp/repo", "repo", sample_status());
        app.view = View::Storage;
        app.storage = StorageExplorerState::completed(
            sample_storage_summary(),
            sample_index_coverage_summary(),
            sample_symbol_outline_summary(),
            sample_call_resolution_summary(),
            sample_embedding_coverage_summary(),
            sample_index_runs_timeline_summary(),
            sample_freshness_summary(),
            sample_semantic_neighborhood_summary(),
            sample_cross_store_health_summary(),
        );

        assert!(!app.handle_key(KeyCode::Tab));
        assert!(!app.handle_key(KeyCode::Tab));
        assert!(!app.handle_key(KeyCode::Tab));
        assert_eq!(app.storage.mode, StorageMode::Calls);
        assert!(!app.handle_key(KeyCode::Tab));

        assert_eq!(app.storage.mode, StorageMode::Embeddings);
        assert_eq!(app.message, "Storage mode set to embedding coverage.");
    }

    #[test]
    fn storage_tab_cycles_to_index_runs_timeline() {
        let mut app = App::from_status("/tmp/repo", "repo", sample_status());
        app.view = View::Storage;
        app.storage = StorageExplorerState::completed(
            sample_storage_summary(),
            sample_index_coverage_summary(),
            sample_symbol_outline_summary(),
            sample_call_resolution_summary(),
            sample_embedding_coverage_summary(),
            sample_index_runs_timeline_summary(),
            sample_freshness_summary(),
            sample_semantic_neighborhood_summary(),
            sample_cross_store_health_summary(),
        );

        assert!(!app.handle_key(KeyCode::Tab));
        assert!(!app.handle_key(KeyCode::Tab));
        assert!(!app.handle_key(KeyCode::Tab));
        assert!(!app.handle_key(KeyCode::Tab));
        assert_eq!(app.storage.mode, StorageMode::Embeddings);
        assert!(!app.handle_key(KeyCode::Tab));

        assert_eq!(app.storage.mode, StorageMode::Runs);
        assert_eq!(app.message, "Storage mode set to index runs timeline.");
    }

    #[test]
    fn storage_tab_cycles_to_semantic_neighborhood() {
        let mut app = App::from_status("/tmp/repo", "repo", sample_status());
        app.view = View::Storage;
        app.storage = StorageExplorerState::completed(
            sample_storage_summary(),
            sample_index_coverage_summary(),
            sample_symbol_outline_summary(),
            sample_call_resolution_summary(),
            sample_embedding_coverage_summary(),
            sample_index_runs_timeline_summary(),
            sample_freshness_summary(),
            sample_semantic_neighborhood_summary(),
            sample_cross_store_health_summary(),
        );

        for _ in 0..7 {
            assert!(!app.handle_key(KeyCode::Tab));
        }

        assert_eq!(app.storage.mode, StorageMode::Neighborhood);
        assert_eq!(app.message, "Storage mode set to semantic neighborhood.");
    }

    #[test]
    fn storage_tab_cycles_to_cross_store_health() {
        let mut app = App::from_status("/tmp/repo", "repo", sample_status());
        app.view = View::Storage;
        app.storage = StorageExplorerState::completed(
            sample_storage_summary(),
            sample_index_coverage_summary(),
            sample_symbol_outline_summary(),
            sample_call_resolution_summary(),
            sample_embedding_coverage_summary(),
            sample_index_runs_timeline_summary(),
            sample_freshness_summary(),
            sample_semantic_neighborhood_summary(),
            sample_cross_store_health_summary(),
        );

        for _ in 0..8 {
            assert!(!app.handle_key(KeyCode::Tab));
        }

        assert_eq!(app.storage.mode, StorageMode::Health);
        assert_eq!(app.message, "Storage mode set to cross-store health.");
    }

    #[test]
    fn renders_index_coverage_file_rows_without_source_text() {
        let mut app = App::from_status("/tmp/repo", "repo", sample_status());
        app.view = View::Storage;
        app.storage = StorageExplorerState::completed(
            sample_storage_summary(),
            sample_index_coverage_summary(),
            sample_symbol_outline_summary(),
            sample_call_resolution_summary(),
            sample_embedding_coverage_summary(),
            sample_index_runs_timeline_summary(),
            sample_freshness_summary(),
            sample_semantic_neighborhood_summary(),
            sample_cross_store_health_summary(),
        );
        app.storage.mode = StorageMode::Coverage;
        let backend = TestBackend::new(150, 32);
        let mut terminal = Terminal::new(backend).expect("terminal should build");

        render(&mut terminal, &app).expect("render should succeed");

        let buffer = terminal.backend().buffer();
        let rendered = format!("{buffer:?}");
        assert!(rendered.contains("Index Coverage"));
        assert!(rendered.contains("Path"));
        assert!(rendered.contains("src/lib.rs"));
        assert!(rendered.contains("missing-vector"));
        assert!(rendered.contains("File Coverage"));
        assert!(rendered.contains("chunks=2"));
        assert!(rendered.contains("vector-backed"));
        assert!(rendered.contains("crate::add"));
        assert!(rendered.contains("resolved_exact"));
        assert!(!rendered.contains("source_text"));
        assert_eq!(
            cell_fg_for_text(buffer, "missing-vector", None),
            Some(Color::Yellow)
        );
    }

    #[test]
    fn index_coverage_selection_drives_file_detail_panel() {
        let mut app = App::from_status("/tmp/repo", "repo", sample_status());
        app.view = View::Storage;
        app.storage = StorageExplorerState::completed(
            sample_storage_summary(),
            sample_index_coverage_summary(),
            sample_symbol_outline_summary(),
            sample_call_resolution_summary(),
            sample_embedding_coverage_summary(),
            sample_index_runs_timeline_summary(),
            sample_freshness_summary(),
            sample_semantic_neighborhood_summary(),
            sample_cross_store_health_summary(),
        );
        app.storage.mode = StorageMode::Coverage;

        assert_eq!(app.storage.selection, 0);
        assert!(!app.handle_key(KeyCode::Down));
        assert_eq!(app.storage.selection, 1);

        let backend = TestBackend::new(150, 24);
        let mut terminal = Terminal::new(backend).expect("terminal should build");
        render(&mut terminal, &app).expect("render should succeed");

        let rendered = format!("{:?}", terminal.backend().buffer());
        assert!(rendered.contains("src/secret.rs"));
        assert!(rendered.contains("excluded"));
        assert!(rendered.contains("reason=secret_detected"));
        assert!(rendered.contains("all chunks are intentionally excluded"));
    }

    #[test]
    fn index_coverage_scrolls_selected_rows_beyond_initial_view_capacity() {
        let mut coverage = sample_index_coverage_summary();
        coverage.files = (0..24)
            .map(|index| FileCoverageRow {
                path: format!("src/file_{index:02}.rs"),
                language: "rust".to_owned(),
                chunks: 1,
                symbols: 1,
                calls: 0,
                embeddable_chunks: 1,
                vector_backed_chunks: 1,
                excluded_chunks: 0,
                status: FileCoverageStatus::Covered,
                detail: FileDetailSummary {
                    chunks: vec![FileChunkDetailRow {
                        kind: "function".to_owned(),
                        symbol: Some(format!("crate::file_{index:02}")),
                        start_line: index + 1,
                        end_line: index + 2,
                        vector_status: ChunkVectorStatus::VectorBacked,
                        excluded_reason: None,
                    }],
                    symbols: vec![FileSymbolDetailRow {
                        kind: "function".to_owned(),
                        qualified_name: format!("crate::file_{index:02}"),
                        parent_symbol_id: None,
                        start_line: index + 1,
                        end_line: index + 2,
                    }],
                    calls: Vec::new(),
                },
            })
            .collect();

        let mut app = App::from_status("/tmp/repo", "repo", sample_status());
        app.view = View::Storage;
        app.storage = StorageExplorerState::completed(
            sample_storage_summary(),
            coverage,
            sample_symbol_outline_summary(),
            sample_call_resolution_summary(),
            sample_embedding_coverage_summary(),
            sample_index_runs_timeline_summary(),
            sample_freshness_summary(),
            sample_semantic_neighborhood_summary(),
            sample_cross_store_health_summary(),
        );
        app.storage.mode = StorageMode::Coverage;

        for _ in 0..18 {
            assert!(!app.handle_key(KeyCode::Down));
        }
        assert_eq!(app.storage.selection, 18);

        let backend = TestBackend::new(150, 28);
        let mut terminal = Terminal::new(backend).expect("terminal should build");
        render(&mut terminal, &app).expect("render should succeed");

        let buffer = terminal.backend().buffer();
        let rendered = format!("{buffer:?}");
        assert!(rendered.contains("src/file_18.rs"));
        assert!(rendered.contains("crate::file_18"));
        assert!(rendered.contains("#"));
        assert_eq!(
            cell_bg_for_text(buffer, "src/file_18.rs", None),
            Some(Color::Cyan)
        );
    }

    #[test]
    fn renders_index_coverage_at_80x24() {
        let mut app = App::from_status("/tmp/repo", "repo", sample_status());
        app.view = View::Storage;
        app.storage = StorageExplorerState::completed(
            sample_storage_summary(),
            sample_index_coverage_summary(),
            sample_symbol_outline_summary(),
            sample_call_resolution_summary(),
            sample_embedding_coverage_summary(),
            sample_index_runs_timeline_summary(),
            sample_freshness_summary(),
            sample_semantic_neighborhood_summary(),
            sample_cross_store_health_summary(),
        );
        app.storage.mode = StorageMode::Coverage;
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("terminal should build");

        render(&mut terminal, &app).expect("render should succeed");

        let rendered = format!("{:?}", terminal.backend().buffer());
        assert!(rendered.contains("symdex TUI"));
        assert!(rendered.contains("Storage"));
        assert!(rendered.contains("Path"));
        assert!(rendered.contains("Status"));
        assert!(rendered.contains("File Coverage"));
    }

    #[test]
    fn renders_symbol_outline_without_source_text() {
        let mut app = App::from_status("/tmp/repo", "repo", sample_status());
        app.view = View::Storage;
        app.storage = StorageExplorerState::completed(
            sample_storage_summary(),
            sample_index_coverage_summary(),
            sample_symbol_outline_summary(),
            sample_call_resolution_summary(),
            sample_embedding_coverage_summary(),
            sample_index_runs_timeline_summary(),
            sample_freshness_summary(),
            sample_semantic_neighborhood_summary(),
            sample_cross_store_health_summary(),
        );
        app.storage.mode = StorageMode::Outline;
        let backend = TestBackend::new(150, 24);
        let mut terminal = Terminal::new(backend).expect("terminal should build");

        render(&mut terminal, &app).expect("render should succeed");

        let rendered = format!("{:?}", terminal.backend().buffer());
        assert!(rendered.contains("Symbol Outline"));
        assert!(rendered.contains("crate::Service"));
        assert!(rendered.contains("crate::Service::run"));
        assert!(rendered.contains("Symbol Detail"));
        assert!(rendered.contains("Children"));
        assert!(!rendered.contains("source_text"));
    }

    #[test]
    fn symbol_outline_selection_drives_detail_panel() {
        let mut app = App::from_status("/tmp/repo", "repo", sample_status());
        app.view = View::Storage;
        app.storage = StorageExplorerState::completed(
            sample_storage_summary(),
            sample_index_coverage_summary(),
            sample_symbol_outline_summary(),
            sample_call_resolution_summary(),
            sample_embedding_coverage_summary(),
            sample_index_runs_timeline_summary(),
            sample_freshness_summary(),
            sample_semantic_neighborhood_summary(),
            sample_cross_store_health_summary(),
        );
        app.storage.mode = StorageMode::Outline;

        assert_eq!(app.storage.selection, 0);
        assert!(!app.handle_key(KeyCode::Down));
        assert_eq!(app.storage.selection, 1);

        let backend = TestBackend::new(150, 24);
        let mut terminal = Terminal::new(backend).expect("terminal should build");
        render(&mut terminal, &app).expect("render should succeed");

        let rendered = format!("{:?}", terminal.backend().buffer());
        assert!(rendered.contains("crate::Service::run"));
        assert!(rendered.contains("Depth"));
        assert!(rendered.contains("parent-symbol"));
        assert!(rendered.contains("src/lib.rs:5-8"));
    }

    #[test]
    fn renders_symbol_outline_at_80x24() {
        let mut app = App::from_status("/tmp/repo", "repo", sample_status());
        app.view = View::Storage;
        app.storage = StorageExplorerState::completed(
            sample_storage_summary(),
            sample_index_coverage_summary(),
            sample_symbol_outline_summary(),
            sample_call_resolution_summary(),
            sample_embedding_coverage_summary(),
            sample_index_runs_timeline_summary(),
            sample_freshness_summary(),
            sample_semantic_neighborhood_summary(),
            sample_cross_store_health_summary(),
        );
        app.storage.mode = StorageMode::Outline;
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("terminal should build");

        render(&mut terminal, &app).expect("render should succeed");

        let rendered = format!("{:?}", terminal.backend().buffer());
        assert!(rendered.contains("symdex TUI"));
        assert!(rendered.contains("Storage"));
        assert!(rendered.contains("Symbol"));
        assert!(rendered.contains("Kind"));
        assert!(rendered.contains("Symbol Detail"));
    }

    #[test]
    fn renders_call_resolution_dashboard_without_source_text() {
        let mut app = App::from_status("/tmp/repo", "repo", sample_status());
        app.view = View::Storage;
        app.storage = StorageExplorerState::completed(
            sample_storage_summary(),
            sample_index_coverage_summary(),
            sample_symbol_outline_summary(),
            sample_call_resolution_summary(),
            sample_embedding_coverage_summary(),
            sample_index_runs_timeline_summary(),
            sample_freshness_summary(),
            sample_semantic_neighborhood_summary(),
            sample_cross_store_health_summary(),
        );
        app.storage.mode = StorageMode::Calls;
        let backend = TestBackend::new(150, 24);
        let mut terminal = Terminal::new(backend).expect("terminal should build");

        render(&mut terminal, &app).expect("render should succeed");

        let buffer = terminal.backend().buffer();
        let rendered = format!("{buffer:?}");
        assert!(rendered.contains("Call Resolution"));
        assert!(rendered.contains("resolved_exact"));
        assert!(rendered.contains("unresolved"));
        assert!(rendered.contains("Call Bucket Detail"));
        assert!(rendered.contains("crate::caller"));
        assert!(rendered.contains("helper"));
        assert!(!rendered.contains("source_text"));
        assert_eq!(
            cell_fg_for_text(buffer, "unresolved", None),
            Some(Color::Yellow)
        );
    }

    #[test]
    fn call_resolution_selection_drives_bucket_detail() {
        let mut app = App::from_status("/tmp/repo", "repo", sample_status());
        app.view = View::Storage;
        app.storage = StorageExplorerState::completed(
            sample_storage_summary(),
            sample_index_coverage_summary(),
            sample_symbol_outline_summary(),
            sample_call_resolution_summary(),
            sample_embedding_coverage_summary(),
            sample_index_runs_timeline_summary(),
            sample_freshness_summary(),
            sample_semantic_neighborhood_summary(),
            sample_cross_store_health_summary(),
        );
        app.storage.mode = StorageMode::Calls;

        assert_eq!(app.storage.selection, 0);
        assert!(!app.handle_key(KeyCode::Down));
        assert_eq!(app.storage.selection, 1);

        let backend = TestBackend::new(150, 24);
        let mut terminal = Terminal::new(backend).expect("terminal should build");
        render(&mut terminal, &app).expect("render should succeed");

        let rendered = format!("{:?}", terminal.backend().buffer());
        assert!(rendered.contains("Confidence bucket"));
        assert!(rendered.contains("low"));
        assert!(rendered.contains("missing"));
        assert!(rendered.contains("src/lib.rs:7"));
    }

    #[test]
    fn renders_call_resolution_at_80x24() {
        let mut app = App::from_status("/tmp/repo", "repo", sample_status());
        app.view = View::Storage;
        app.storage = StorageExplorerState::completed(
            sample_storage_summary(),
            sample_index_coverage_summary(),
            sample_symbol_outline_summary(),
            sample_call_resolution_summary(),
            sample_embedding_coverage_summary(),
            sample_index_runs_timeline_summary(),
            sample_freshness_summary(),
            sample_semantic_neighborhood_summary(),
            sample_cross_store_health_summary(),
        );
        app.storage.mode = StorageMode::Calls;
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("terminal should build");

        render(&mut terminal, &app).expect("render should succeed");

        let rendered = format!("{:?}", terminal.backend().buffer());
        assert!(rendered.contains("symdex TUI"));
        assert!(rendered.contains("Storage"));
        assert!(rendered.contains("Resolution"));
        assert!(rendered.contains("Conf"));
        assert!(rendered.contains("Call Bucket"));
    }

    #[test]
    fn renders_storage_barcharts_at_normal_height_without_source_text() {
        let mut app = App::from_status("/tmp/repo", "repo", sample_status());
        app.view = View::Storage;
        app.storage = StorageExplorerState::completed(
            sample_storage_summary(),
            sample_index_coverage_summary(),
            sample_symbol_outline_summary(),
            sample_call_resolution_summary(),
            sample_embedding_coverage_summary(),
            sample_index_runs_timeline_summary(),
            sample_freshness_summary(),
            sample_semantic_neighborhood_summary(),
            sample_cross_store_health_summary(),
        );

        for (mode, title) in [
            (StorageMode::Calls, "Call Resolution Counts"),
            (StorageMode::Embeddings, "Embedding Coverage Counts"),
            (StorageMode::Runs, "Index Run Outcomes"),
            (StorageMode::Freshness, "Freshness States"),
        ] {
            app.storage.mode = mode;
            let backend = TestBackend::new(150, 34);
            let mut terminal = Terminal::new(backend).expect("terminal should build");
            render(&mut terminal, &app).expect("render should succeed");

            let rendered = format!("{:?}", terminal.backend().buffer());
            assert!(rendered.contains(title));
            assert!(!rendered.contains("source_text"));
        }
    }

    #[test]
    fn renders_embedding_coverage_without_source_text() {
        let mut app = App::from_status("/tmp/repo", "repo", sample_status());
        app.view = View::Storage;
        app.storage = StorageExplorerState::completed(
            sample_storage_summary(),
            sample_index_coverage_summary(),
            sample_symbol_outline_summary(),
            sample_call_resolution_summary(),
            sample_embedding_coverage_summary(),
            sample_index_runs_timeline_summary(),
            sample_freshness_summary(),
            sample_semantic_neighborhood_summary(),
            sample_cross_store_health_summary(),
        );
        app.storage.mode = StorageMode::Embeddings;
        let backend = TestBackend::new(150, 24);
        let mut terminal = Terminal::new(backend).expect("terminal should build");

        render(&mut terminal, &app).expect("render should succeed");

        let buffer = terminal.backend().buffer();
        let rendered = format!("{buffer:?}");
        assert!(rendered.contains("Embedding Coverage"));
        assert!(rendered.contains("vector-backed"));
        assert!(rendered.contains("missing-vector"));
        assert!(rendered.contains("Embedding Detail"));
        assert!(rendered.contains("secret_detected=1"));
        assert!(rendered.contains("missing_vectors"));
        assert!(!rendered.contains("source_text"));
        assert_eq!(
            cell_fg_for_text(buffer, "missing-vector", None),
            Some(Color::Yellow)
        );
    }

    #[test]
    fn embedding_coverage_selection_drives_detail_panel() {
        let mut app = App::from_status("/tmp/repo", "repo", sample_status());
        app.view = View::Storage;
        app.storage = StorageExplorerState::completed(
            sample_storage_summary(),
            sample_index_coverage_summary(),
            sample_symbol_outline_summary(),
            sample_call_resolution_summary(),
            sample_embedding_coverage_summary(),
            sample_index_runs_timeline_summary(),
            sample_freshness_summary(),
            sample_semantic_neighborhood_summary(),
            sample_cross_store_health_summary(),
        );
        app.storage.mode = StorageMode::Embeddings;

        assert_eq!(app.storage.selection, 0);
        assert!(!app.handle_key(KeyCode::Down));
        assert!(!app.handle_key(KeyCode::Down));
        assert_eq!(app.storage.selection, 2);

        let backend = TestBackend::new(150, 24);
        let mut terminal = Terminal::new(backend).expect("terminal should build");
        render(&mut terminal, &app).expect("render should succeed");

        let rendered = format!("{:?}", terminal.backend().buffer());
        assert!(rendered.contains("Selected"));
        assert!(rendered.contains("vector-backed"));
        assert!(rendered.contains("Chunks with recorded sqlite-vec point IDs."));
        assert!(rendered.contains("symdex_repo_nomic_embed_text"));
    }

    #[test]
    fn renders_embedding_coverage_at_80x24() {
        let mut app = App::from_status("/tmp/repo", "repo", sample_status());
        app.view = View::Storage;
        app.storage = StorageExplorerState::completed(
            sample_storage_summary(),
            sample_index_coverage_summary(),
            sample_symbol_outline_summary(),
            sample_call_resolution_summary(),
            sample_embedding_coverage_summary(),
            sample_index_runs_timeline_summary(),
            sample_freshness_summary(),
            sample_semantic_neighborhood_summary(),
            sample_cross_store_health_summary(),
        );
        app.storage.mode = StorageMode::Embeddings;
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("terminal should build");

        render(&mut terminal, &app).expect("render should succeed");

        let rendered = format!("{:?}", terminal.backend().buffer());
        assert!(rendered.contains("symdex TUI"));
        assert!(rendered.contains("Storage"));
        assert!(rendered.contains("Metric"));
        assert!(rendered.contains("Value"));
        assert!(rendered.contains("Embedding"));
    }

    #[test]
    fn renders_index_runs_timeline_without_source_text() {
        let mut app = App::from_status("/tmp/repo", "repo", sample_status());
        app.view = View::Storage;
        app.storage = StorageExplorerState::completed(
            sample_storage_summary(),
            sample_index_coverage_summary(),
            sample_symbol_outline_summary(),
            sample_call_resolution_summary(),
            sample_embedding_coverage_summary(),
            sample_index_runs_timeline_summary(),
            sample_freshness_summary(),
            sample_semantic_neighborhood_summary(),
            sample_cross_store_health_summary(),
        );
        app.storage.mode = StorageMode::Runs;
        let backend = TestBackend::new(150, 24);
        let mut terminal = Terminal::new(backend).expect("terminal should build");

        render(&mut terminal, &app).expect("render should succeed");

        let buffer = terminal.backend().buffer();
        let rendered = format!("{buffer:?}");
        assert!(rendered.contains("Index Runs Timeline"));
        assert!(rendered.contains("2026-01-02T00:00:00Z"));
        assert!(rendered.contains("failed"));
        assert!(rendered.contains("Index Run Detail"));
        assert!(rendered.contains("sqlite-vec unavailable"));
        assert!(!rendered.contains("source_text"));
        assert_eq!(cell_fg_for_text(buffer, "failed", None), Some(Color::Red));
    }

    #[test]
    fn index_runs_timeline_selection_drives_detail_panel() {
        let mut app = App::from_status("/tmp/repo", "repo", sample_status());
        app.view = View::Storage;
        app.storage = StorageExplorerState::completed(
            sample_storage_summary(),
            sample_index_coverage_summary(),
            sample_symbol_outline_summary(),
            sample_call_resolution_summary(),
            sample_embedding_coverage_summary(),
            sample_index_runs_timeline_summary(),
            sample_freshness_summary(),
            sample_semantic_neighborhood_summary(),
            sample_cross_store_health_summary(),
        );
        app.storage.mode = StorageMode::Runs;

        assert_eq!(app.storage.selection, 0);
        assert!(!app.handle_key(KeyCode::Down));
        assert_eq!(app.storage.selection, 1);

        let backend = TestBackend::new(150, 24);
        let mut terminal = Terminal::new(backend).expect("terminal should build");
        render(&mut terminal, &app).expect("render should succeed");

        let rendered = format!("{:?}", terminal.backend().buffer());
        assert!(rendered.contains("run-success"));
        assert!(rendered.contains("files_seen=4"));
        assert!(rendered.contains("chunks_embedded=7"));
        assert!(rendered.contains("Error"));
    }

    #[test]
    fn renders_index_runs_timeline_at_80x24() {
        let mut app = App::from_status("/tmp/repo", "repo", sample_status());
        app.view = View::Storage;
        app.storage = StorageExplorerState::completed(
            sample_storage_summary(),
            sample_index_coverage_summary(),
            sample_symbol_outline_summary(),
            sample_call_resolution_summary(),
            sample_embedding_coverage_summary(),
            sample_index_runs_timeline_summary(),
            sample_freshness_summary(),
            sample_semantic_neighborhood_summary(),
            sample_cross_store_health_summary(),
        );
        app.storage.mode = StorageMode::Runs;
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("terminal should build");

        render(&mut terminal, &app).expect("render should succeed");

        let rendered = format!("{:?}", terminal.backend().buffer());
        assert!(rendered.contains("symdex TUI"));
        assert!(rendered.contains("Storage"));
        assert!(rendered.contains("Started"));
        assert!(rendered.contains("Status"));
        assert!(rendered.contains("Index Run"));
    }

    #[test]
    fn renders_freshness_provenance_panel() {
        let mut app = App::from_status("/tmp/repo", "repo", sample_status());
        app.view = View::Storage;
        app.storage = StorageExplorerState::completed(
            sample_storage_summary(),
            sample_index_coverage_summary(),
            sample_symbol_outline_summary(),
            sample_call_resolution_summary(),
            sample_embedding_coverage_summary(),
            sample_index_runs_timeline_summary(),
            sample_freshness_summary(),
            sample_semantic_neighborhood_summary(),
            sample_cross_store_health_summary(),
        );
        app.storage.mode = StorageMode::Freshness;
        app.storage.selection = 1;
        let backend = TestBackend::new(120, 24);
        let mut terminal = Terminal::new(backend).expect("terminal should build");

        render(&mut terminal, &app).expect("render should succeed");

        let rendered = format!("{:?}", terminal.backend().buffer());
        assert!(rendered.contains("Evidence Freshness"));
        assert!(rendered.contains("src/stale.rs"));
        assert!(rendered.contains("Provenance"));
        assert!(rendered.contains("run-success"));
        assert!(!rendered.contains("source_text"));
    }

    #[test]
    fn renders_semantic_neighborhood_without_source_text() {
        let mut app = App::from_status("/tmp/repo", "repo", sample_status());
        app.view = View::Storage;
        app.storage = StorageExplorerState::completed(
            sample_storage_summary(),
            sample_index_coverage_summary(),
            sample_symbol_outline_summary(),
            sample_call_resolution_summary(),
            sample_embedding_coverage_summary(),
            sample_index_runs_timeline_summary(),
            sample_freshness_summary(),
            sample_semantic_neighborhood_summary(),
            sample_cross_store_health_summary(),
        );
        app.storage.mode = StorageMode::Neighborhood;
        let backend = TestBackend::new(150, 24);
        let mut terminal = Terminal::new(backend).expect("terminal should build");

        render(&mut terminal, &app).expect("render should succeed");

        let rendered = format!("{:?}", terminal.backend().buffer());
        assert!(rendered.contains("Semantic Neighborhood"));
        assert!(rendered.contains("src/lib.rs"));
        assert!(rendered.contains("crate::add"));
        assert!(rendered.contains("metadata"));
        assert!(rendered.contains("Semantic Payload Detail"));
        assert!(rendered.contains("hash-vector"));
        assert!(!rendered.contains("source_text"));
    }

    #[test]
    fn semantic_neighborhood_selection_drives_detail_panel() {
        let mut app = App::from_status("/tmp/repo", "repo", sample_status());
        app.view = View::Storage;
        app.storage = StorageExplorerState::completed(
            sample_storage_summary(),
            sample_index_coverage_summary(),
            sample_symbol_outline_summary(),
            sample_call_resolution_summary(),
            sample_embedding_coverage_summary(),
            sample_index_runs_timeline_summary(),
            sample_freshness_summary(),
            sample_semantic_neighborhood_summary(),
            sample_cross_store_health_summary(),
        );
        app.storage.mode = StorageMode::Neighborhood;

        assert_eq!(app.storage.selection, 0);
        assert!(!app.handle_key(KeyCode::Down));
        assert_eq!(app.storage.selection, 1);

        let backend = TestBackend::new(150, 24);
        let mut terminal = Terminal::new(backend).expect("terminal should build");
        render(&mut terminal, &app).expect("render should succeed");

        let rendered = format!("{:?}", terminal.backend().buffer());
        assert!(rendered.contains("src/worker.rs:10-18"));
        assert!(rendered.contains("crate::worker"));
        assert!(rendered.contains("point-worker"));
        assert!(rendered.contains("hash-worker"));
    }

    #[test]
    fn renders_semantic_neighborhood_at_80x24() {
        let mut app = App::from_status("/tmp/repo", "repo", sample_status());
        app.view = View::Storage;
        app.storage = StorageExplorerState::completed(
            sample_storage_summary(),
            sample_index_coverage_summary(),
            sample_symbol_outline_summary(),
            sample_call_resolution_summary(),
            sample_embedding_coverage_summary(),
            sample_index_runs_timeline_summary(),
            sample_freshness_summary(),
            sample_semantic_neighborhood_summary(),
            sample_cross_store_health_summary(),
        );
        app.storage.mode = StorageMode::Neighborhood;
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("terminal should build");

        render(&mut terminal, &app).expect("render should succeed");

        let rendered = format!("{:?}", terminal.backend().buffer());
        assert!(rendered.contains("symdex TUI"));
        assert!(rendered.contains("Storage"));
        assert!(rendered.contains("Semantic"));
        assert!(rendered.contains("Payload"));
    }

    #[test]
    fn renders_cross_store_health_warnings_without_source_text() {
        let mut app = App::from_status("/tmp/repo", "repo", sample_status());
        app.view = View::Storage;
        app.storage = StorageExplorerState::completed(
            sample_storage_summary(),
            sample_index_coverage_summary(),
            sample_symbol_outline_summary(),
            sample_call_resolution_summary(),
            sample_embedding_coverage_summary(),
            sample_index_runs_timeline_summary(),
            sample_freshness_summary(),
            sample_semantic_neighborhood_summary(),
            sample_cross_store_health_summary(),
        );
        app.storage.mode = StorageMode::Health;
        let backend = TestBackend::new(150, 24);
        let mut terminal = Terminal::new(backend).expect("terminal should build");

        render(&mut terminal, &app).expect("render should succeed");

        let buffer = terminal.backend().buffer();
        let rendered = format!("{buffer:?}");
        assert!(rendered.contains("Cross-Store Health"));
        assert!(rendered.contains("missing_vectors"));
        assert!(rendered.contains("excluded_chunks"));
        assert!(rendered.contains("model_drift"));
        assert!(rendered.contains("dimension_drift"));
        assert!(rendered.contains("Cross-Store Detail"));
        assert!(!rendered.contains("source_text"));
        assert_eq!(
            cell_fg_for_text(buffer, "model_drift", None),
            Some(Color::Red)
        );
    }

    #[test]
    fn cross_store_health_selection_drives_detail_panel() {
        let mut app = App::from_status("/tmp/repo", "repo", sample_status());
        app.view = View::Storage;
        app.storage = StorageExplorerState::completed(
            sample_storage_summary(),
            sample_index_coverage_summary(),
            sample_symbol_outline_summary(),
            sample_call_resolution_summary(),
            sample_embedding_coverage_summary(),
            sample_index_runs_timeline_summary(),
            sample_freshness_summary(),
            sample_semantic_neighborhood_summary(),
            sample_cross_store_health_summary(),
        );
        app.storage.mode = StorageMode::Health;

        assert!(!app.handle_key(KeyCode::Down));
        assert_eq!(app.storage.selection, 1);

        let backend = TestBackend::new(150, 24);
        let mut terminal = Terminal::new(backend).expect("terminal should build");
        render(&mut terminal, &app).expect("render should succeed");

        let rendered = format!("{:?}", terminal.backend().buffer());
        assert!(rendered.contains("excluded_chunks"));
        assert!(rendered.contains("intentionally excluded"));
        assert!(rendered.contains("missing collections"));
    }

    #[test]
    fn renders_cross_store_health_at_80x24() {
        let mut app = App::from_status("/tmp/repo", "repo", sample_status());
        app.view = View::Storage;
        app.storage = StorageExplorerState::completed(
            sample_storage_summary(),
            sample_index_coverage_summary(),
            sample_symbol_outline_summary(),
            sample_call_resolution_summary(),
            sample_embedding_coverage_summary(),
            sample_index_runs_timeline_summary(),
            sample_freshness_summary(),
            sample_semantic_neighborhood_summary(),
            sample_cross_store_health_summary(),
        );
        app.storage.mode = StorageMode::Health;
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("terminal should build");

        render(&mut terminal, &app).expect("render should succeed");

        let rendered = format!("{:?}", terminal.backend().buffer());
        assert!(rendered.contains("symdex TUI"));
        assert!(rendered.contains("Storage"));
        assert!(rendered.contains("Health"));
        assert!(rendered.contains("Check"));
        assert!(rendered.contains("Detail"));
    }

    #[test]
    fn renders_status_labels_with_semantic_color() {
        let mut app = App::from_status("/tmp/repo", "repo", sample_status());
        app.view = View::Query;
        app.query.status = QueryStatus::Failed("bad query".to_owned());
        let backend = TestBackend::new(100, 24);
        let mut terminal = Terminal::new(backend).expect("terminal should build");

        render(&mut terminal, &app).expect("render should succeed");

        assert_eq!(
            cell_fg_for_text(terminal.backend().buffer(), "failed", None),
            Some(Color::Red)
        );
    }

    #[test]
    fn renders_index_confirmation_panel_with_warning_style() {
        let mut app = App::from_status("/tmp/repo", "repo", sample_status());
        app.view = View::Indexing;
        app.screen = Screen::ConfirmIndex(ManualIndexRequest {
            mode: IndexMode::Semantic,
            scope: IndexScope::Full,
        });
        let backend = TestBackend::new(100, 28);
        let mut terminal = Terminal::new(backend).expect("terminal should build");

        render(&mut terminal, &app).expect("render should succeed");

        let buffer = terminal.backend().buffer();
        let rendered = format!("{buffer:?}");
        assert!(rendered.contains("Confirm Indexing"));
        assert!(rendered.contains("Run semantic full indexing"));
        assert!(rendered.contains("Press y to start"));
        assert_eq!(
            cell_fg_for_text(buffer, "Confirm Indexing", None),
            Some(Color::Yellow)
        );
    }

    #[test]
    fn continuous_indexing_toggle_requires_confirmation() {
        let mut app = App::from_status("/tmp/repo", "repo", sample_status());

        assert!(!app.handle_key(KeyCode::Char('c')));
        assert_eq!(app.screen, Screen::ConfirmContinuous);
        assert_eq!(app.message, "Confirm continuous indexing before starting.");

        assert!(!app.handle_key(KeyCode::Char('n')));
        assert_eq!(app.screen, Screen::Dashboard);
        assert_eq!(app.message, "Indexing action cancelled before start.");
    }

    #[test]
    fn continuous_indexing_toggle_stops_when_enabled() {
        let repo = std::env::temp_dir().join(format!("symdex-tui-stop-{}", std::process::id()));
        std::fs::create_dir_all(&repo).expect("temp repo should be created");
        let mut app = App::from_status(repo.display().to_string(), "repo", sample_status());
        app.continuous.enabled = true;
        app.continuous.status = ContinuousIndexStatus::Watching;
        app.continuous.queued_events = 2;

        assert!(!app.handle_key(KeyCode::Char('c')));

        assert!(!app.continuous.enabled);
        assert_eq!(app.continuous.status, ContinuousIndexStatus::Off);
        assert_eq!(app.continuous.queued_events, 0);
        assert_eq!(app.message, "Continuous indexing stopped.");
    }

    #[test]
    fn continuous_indexing_events_update_status() {
        let mut app = App::from_status("/tmp/repo", "repo", sample_status());
        app.apply_continuous_event(symdex_index::ContinuousIndexEvent::Started {
            repository_id: "repo".to_owned(),
            files_seen: 3,
        });
        assert!(app.continuous.enabled);
        assert_eq!(app.continuous.status, ContinuousIndexStatus::Watching);
        assert_eq!(app.continuous.files_seen, 3);

        let changes = symdex_index::WatchChangeSet {
            created: vec!["src/new.rs".to_owned()],
            modified: vec!["src/lib.rs".to_owned()],
            deleted: Vec::new(),
        };
        app.apply_continuous_event(symdex_index::ContinuousIndexEvent::ChangesPending {
            changes: changes.clone(),
        });
        assert_eq!(app.continuous.status, ContinuousIndexStatus::Pending);
        assert_eq!(app.continuous.queued_events, 2);
        assert_eq!(
            app.message,
            "Continuous indexing debounce pending: 2 events."
        );

        app.apply_continuous_event(symdex_index::ContinuousIndexEvent::BatchFailed {
            changes,
            error: "ollama unavailable".to_owned(),
        });
        assert_eq!(app.continuous.status, ContinuousIndexStatus::Failed);
        assert_eq!(app.continuous.queued_events, 2);
        assert_eq!(
            app.continuous.latest_error.as_deref(),
            Some("ollama unavailable")
        );
        assert_eq!(app.message, "Continuous indexing batch failed.");
    }

    #[test]
    fn continuous_indexing_quality_events_update_status() {
        let mut app = App::from_status("/tmp/repo", "repo", sample_status());
        let state = symdex_index::ContinuousQualityState {
            repository_id: "repo".to_owned(),
            generation_id: "generation-1".to_owned(),
            active_layer: "fast".to_owned(),
            quality_status: "quality_pending".to_owned(),
            activation_reason: None,
            embeddable_chunks: 2,
            quality_eligible_chunks: 2,
            quality_ineligible_chunks: 0,
            quality_embedded_chunks: 1,
            pending_jobs: 1,
            running_jobs: 0,
            succeeded_jobs: 1,
            failed_jobs: 0,
            skipped_stale_jobs: 0,
            skipped_excluded_jobs: 0,
        };

        app.apply_continuous_event(symdex_index::ContinuousIndexEvent::QualityState {
            state: state.clone(),
        });
        assert_eq!(app.continuous.active_layer.as_deref(), Some("fast"));
        assert_eq!(
            app.continuous.quality_status.as_deref(),
            Some("quality_pending")
        );
        assert_eq!(app.continuous.quality_pending_jobs, 1);
        assert_eq!(app.semantic_status.active_layer, SemanticLayer::Fast);
        assert_eq!(
            app.semantic_status.quality_status,
            SemanticLayerStatus::QualityPending
        );
        assert_eq!(
            app.semantic_status.fallback_reason.as_deref(),
            Some("quality_manifest_incomplete_using_fast_layer")
        );
        assert_eq!(app.message, "Quality state: quality_pending on fast.");

        app.apply_continuous_event(symdex_index::ContinuousIndexEvent::QualityCompleted {
            summary: Box::new(symdex_index::QualityIndexSummary {
                repository_id: "repo".to_owned(),
                generation_id: "generation-1".to_owned(),
                quality_model: "mxbai-embed-large".to_owned(),
                quality_dimension: Some(768),
                vector_table: "symdex_repo_nomic_embed_text_v2_moe".to_owned(),
                claimed_jobs: 1,
                succeeded_jobs: 1,
                failed_jobs: 0,
                skipped_stale_jobs: 0,
                skipped_excluded_jobs: 0,
                remaining_pending_jobs: 0,
                quality_status: "quality_ready".to_owned(),
                active_layer: "quality".to_owned(),
                activation_reason: "quality_complete".to_owned(),
                progress: QualityGenerationProgress {
                    repository_id: "repo".to_owned(),
                    generation_id: "generation-1".to_owned(),
                    embeddable_chunks: 2,
                    quality_eligible_chunks: 2,
                    quality_ineligible_chunks: 0,
                    quality_embedded_chunks: 2,
                    pending_jobs: 0,
                    running_jobs: 0,
                    succeeded_jobs: 2,
                    failed_jobs: 0,
                    skipped_stale_jobs: 0,
                    skipped_excluded_jobs: 0,
                },
            }),
        });
        assert_eq!(app.continuous.active_layer.as_deref(), Some("quality"));
        assert_eq!(
            app.continuous.quality_status.as_deref(),
            Some("quality_ready")
        );
        assert_eq!(
            app.continuous.activation_reason.as_deref(),
            Some("quality_complete")
        );
        assert_eq!(app.continuous.quality_pending_jobs, 0);
        assert_eq!(app.semantic_status.active_layer, SemanticLayer::Quality);
        assert_eq!(
            app.semantic_status.quality_status,
            SemanticLayerStatus::QualityReady
        );
        assert_eq!(app.semantic_status.fallback_reason, None);
        assert_eq!(
            app.semantic_status
                .quality_progress
                .as_ref()
                .map(|progress| progress.pending_jobs),
            Some(0)
        );
        assert_eq!(
            app.message,
            "Quality catch-up completed: quality_ready on quality."
        );

        app.apply_continuous_event(symdex_index::ContinuousIndexEvent::QualityFailed {
            state: Some(state),
            error: "quality model unavailable".to_owned(),
        });
        assert_eq!(
            app.continuous.latest_quality_error.as_deref(),
            Some("quality model unavailable")
        );
        assert_eq!(
            app.semantic_status.latest_quality_error.as_deref(),
            Some("quality model unavailable")
        );
        assert_eq!(app.message, "Quality catch-up failed.");
    }

    #[test]
    fn renders_continuous_indexing_status() {
        let mut app = App::from_status("/tmp/repo", "repo", sample_status());
        app.view = View::Indexing;
        app.animation_tick = 2;
        app.continuous.enabled = true;
        app.continuous.status = ContinuousIndexStatus::Pending;
        app.continuous.files_seen = 3;
        app.continuous.queued_events = 2;
        app.continuous.last_reindexed_file = Some("src/lib.rs".to_owned());
        app.continuous.latest_error = Some("ollama unavailable".to_owned());
        app.continuous.active_layer = Some("fast".to_owned());
        app.continuous.quality_status = Some("quality_pending".to_owned());
        app.continuous.quality_pending_jobs = 3;
        app.continuous.activation_reason = Some("quality_jobs_pending".to_owned());
        let backend = TestBackend::new(120, 24);
        let mut terminal = Terminal::new(backend).expect("terminal should build");

        render(&mut terminal, &app).expect("render should succeed");

        let buffer = terminal.backend().buffer();
        let rendered = format!("{buffer:?}");
        assert!(rendered.contains("Continuous index"));
        assert!(rendered.contains("[|]"));
        assert!(rendered.contains("ci"));
        assert!(rendered.contains("pending"));
        assert!(rendered.contains("queued=2"));
        assert!(rendered.contains("src/lib.rs"));
        assert!(rendered.contains("quality_pending"));
        assert!(rendered.contains("qjobs=3/0/0/0"));
        assert!(rendered.contains("ollama unavailable"));
        assert_eq!(
            cell_fg_for_text(buffer, "pending", None),
            Some(Color::Yellow)
        );
    }

    #[test]
    fn index_status_refresh_uses_interval() {
        let mut app = App::from_status("/tmp/repo", "repo", sample_status());
        let now = Instant::now();

        assert_eq!(INDEX_STATUS_REFRESH_INTERVAL, Duration::from_secs(5));

        app.last_index_status_refresh = None;
        assert!(app.index_status_refresh_due(now));

        app.last_index_status_refresh = Some(now - INDEX_STATUS_REFRESH_INTERVAL);
        assert!(app.index_status_refresh_due(now));

        app.last_index_status_refresh = Some(now);
        assert!(!app.index_status_refresh_due(now));
    }

    #[test]
    fn index_status_refresh_skips_non_index_views() {
        let mut app = App::from_status("/tmp/repo", "repo", sample_status());
        app.view = View::Storage;
        app.last_index_status_refresh = None;

        app.refresh_index_status_on_interval();

        assert!(app.status_refresh_receiver.is_none());
        assert!(app.last_index_status_refresh.is_none());
    }

    #[test]
    fn watcher_status_refresh_uses_shorter_interval() {
        let mut app = App::from_status("/tmp/repo", "repo", sample_status());
        let now = Instant::now();

        app.last_watcher_status_refresh = None;
        assert!(app.watcher_status_refresh_due(now));

        app.last_watcher_status_refresh = Some(now - WATCHER_STATUS_REFRESH_INTERVAL);
        assert!(app.watcher_status_refresh_due(now));

        app.last_watcher_status_refresh = Some(now);
        assert!(!app.watcher_status_refresh_due(now));
    }

    #[test]
    fn renders_index_semantic_readiness_gauges() {
        let mut app = App::from_status("/tmp/repo", "repo", sample_status());
        app.view = View::Indexing;
        app.semantic_status.fast.current_chunks = 2;
        app.semantic_status.fast.expected_chunks = 4;
        app.semantic_status.fast.total_chunks = 3;
        app.semantic_status.fast.stale_chunks = 1;
        app.semantic_status.quality.current_chunks = 1;
        app.semantic_status.quality.expected_chunks = 4;
        app.semantic_status.quality_status = SemanticLayerStatus::QualityPending;
        app.semantic_status.quality_progress = Some(QualityGenerationProgress {
            repository_id: "repo".to_owned(),
            generation_id: "generation-1".to_owned(),
            embeddable_chunks: 4,
            quality_eligible_chunks: 4,
            quality_ineligible_chunks: 0,
            quality_embedded_chunks: 1,
            pending_jobs: 2,
            running_jobs: 1,
            succeeded_jobs: 1,
            failed_jobs: 0,
            skipped_stale_jobs: 1,
            skipped_excluded_jobs: 0,
        });
        let backend = TestBackend::new(100, 28);
        let mut terminal = Terminal::new(backend).expect("terminal should build");

        render(&mut terminal, &app).expect("render should succeed");

        let rendered = format!("{:?}", terminal.backend().buffer());
        assert!(rendered.contains("Fast Readiness"));
        assert!(rendered.contains("Quality Readiness"));
        assert!(rendered.contains("fast_ready 2/4 50%"));
        assert!(rendered.contains("quality_ready 1/4 25%"));
        assert!(rendered.contains("Fast/Quality Job States"));
        assert!(rendered.contains("fpen"));
        assert!(rendered.contains("qpen"));
        assert!(rendered.contains("qrun"));
        assert!(rendered.contains("qsta"));
        let buffer = terminal.backend().buffer();
        assert_ne!(
            cell_fg_for_text(buffer, "fast_ready 2/4 50%", None),
            Some(Color::White)
        );
        assert_ne!(
            cell_bg_for_text(buffer, "fast_ready 2/4 50%", None),
            Some(Color::Black)
        );
        assert_ne!(
            cell_fg_for_text(buffer, "quality_ready 1/4 25%", None),
            Some(Color::White)
        );
        assert_ne!(
            cell_bg_for_text(buffer, "quality_ready 1/4 25%", None),
            Some(Color::Black)
        );
    }

    #[test]
    fn continuous_indexing_animation_advances_only_when_on() {
        let mut app = App::from_status("/tmp/repo", "repo", sample_status());

        app.tick_animation();
        assert_eq!(app.animation_tick, 0);

        app.continuous.enabled = true;
        app.continuous.status = ContinuousIndexStatus::Watching;
        app.tick_animation();
        app.tick_animation();

        assert_eq!(app.animation_tick, 2);
        assert_eq!(continuous_activity_frame(app.animation_tick), "|");
    }

    #[test]
    fn renders_running_index_progress_gauge() {
        let mut app = App::from_status("/tmp/repo", "repo", sample_status());
        app.view = View::Indexing;
        app.screen = Screen::IndexRunning(ManualIndexRequest {
            mode: IndexMode::Semantic,
            scope: IndexScope::Incremental,
        });
        app.index_progress = Some(symdex_index::IndexProgress {
            phase: "parse",
            completed: 2,
            total: 4,
            message: "Parsed src/lib.rs".to_owned(),
        });
        let backend = TestBackend::new(100, 24);
        let mut terminal = Terminal::new(backend).expect("terminal should build");

        render(&mut terminal, &app).expect("render should succeed");

        let buffer = terminal.backend().buffer();
        let rendered = format!("{buffer:?}");
        assert!(rendered.contains("Indexing Running"));
        assert!(rendered.contains("Progress"));
        assert!(rendered.contains("parse 2/4"));
        assert_ne!(
            cell_fg_for_text(buffer, "parse 2/4", None),
            Some(Color::White)
        );
        assert_ne!(
            cell_bg_for_text(buffer, "parse 2/4", None),
            Some(Color::Black)
        );
        assert_eq!(
            cell_fg_for_text(buffer, "Indexing Running", None),
            Some(Color::Cyan)
        );
    }

    #[test]
    fn progress_percent_clamps_to_complete() {
        let progress = symdex_index::IndexProgress {
            phase: "vector",
            completed: 6,
            total: 5,
            message: "Upserted vector points".to_owned(),
        };

        assert_eq!(progress_percent(Some(&progress)), 100);
    }

    #[test]
    fn manual_quality_summary_parses_writer_response() {
        let response = WriterJobResponse::ok_with_data(
            "quality index completed",
            serde_json::json!({
                "quality_status": "quality_ready",
                "active_layer": "quality",
                "activation_reason": "quality_complete",
                "claimed_jobs": 4,
                "failed_jobs": 1,
                "skipped_stale_jobs": 2,
                "remaining_pending_jobs": 3,
            }),
        );

        let summary = manual_quality_summary_from_writer_response(response)
            .expect("quality writer response should parse");

        assert_eq!(summary.quality_status, "quality_ready");
        assert_eq!(summary.active_layer, "quality");
        assert_eq!(summary.activation_reason, "quality_complete");
        assert_eq!(summary.claimed_jobs, 4);
        assert_eq!(summary.failed_jobs, 1);
        assert_eq!(summary.skipped_stale_jobs, 2);
        assert_eq!(summary.remaining_pending_jobs, 3);
    }

    #[test]
    fn layer_readiness_percent_uses_ready_chunks_over_expected_chunks() {
        let app = App::from_status("/tmp/repo", "repo", sample_status());
        let mut layer = app.semantic_status.fast.clone();
        layer.expected_chunks = 8;
        layer.current_chunks = 3;

        assert_eq!(layer_readiness_percent(&layer), 37);

        layer.current_chunks = 10;
        assert_eq!(layer_readiness_percent(&layer), 100);

        layer.expected_chunks = 0;
        assert_eq!(layer_readiness_percent(&layer), 0);
    }

    #[test]
    fn layer_count_percent_clamps_to_total_chunks() {
        assert_eq!(layer_count_percent(2, 4), 50);
        assert_eq!(layer_count_percent(1, 4), 25);
        assert_eq!(layer_count_percent(5, 4), 100);
        assert_eq!(layer_count_percent(1, 0), 0);
    }

    #[test]
    fn renders_doctor_diagnostics() {
        let mut app = App::from_status("/tmp/repo", "repo", sample_status());
        app.view = View::Diagnostics;
        app.diagnostics = DiagnosticsState::Completed(sample_diagnostic_report());
        let backend = TestBackend::new(100, 24);
        let mut terminal = Terminal::new(backend).expect("terminal should build");

        render(&mut terminal, &app).expect("render should succeed");

        let buffer = terminal.backend().buffer();
        let rendered = format!("{buffer:?}");
        assert!(rendered.contains("Doctor Diagnostics"));
        assert!(rendered.contains("sqlite_parent"));
        assert!(rendered.contains("sqlite_vec_status"));
        assert!(rendered.contains("unreachable"));
        assert!(rendered.contains("Selected Check Details"));
    }

    #[test]
    fn doctor_selection_updates_selected_check_details() {
        let mut app = App::from_status("/tmp/repo", "repo", sample_status());
        app.view = View::Diagnostics;
        app.diagnostics = DiagnosticsState::Completed(sample_diagnostic_report());

        assert_eq!(app.diagnostics_selection, 0);
        assert!(!app.handle_key(KeyCode::Down));
        assert_eq!(app.diagnostics_selection, 1);
        assert!(!app.handle_key(KeyCode::Enter));

        let backend = TestBackend::new(120, 24);
        let mut terminal = Terminal::new(backend).expect("terminal should build");
        render(&mut terminal, &app).expect("render should succeed");

        let rendered = format!("{:?}", terminal.backend().buffer());
        assert!(rendered.contains("Selected Check Details"));
        assert!(rendered.contains("sqlite_vec_status"));
        assert!(rendered.contains("connection refused"));
        assert!(rendered.contains("Start the local service"));
    }

    #[test]
    fn doctor_enter_toggles_selected_check_details_expansion() {
        let mut app = App::from_status("/tmp/repo", "repo", sample_status());
        app.view = View::Diagnostics;
        app.diagnostics = DiagnosticsState::Completed(sample_diagnostic_report());

        assert!(!app.diagnostics_details_expanded);
        assert!(!app.handle_key(KeyCode::Enter));
        assert!(app.diagnostics_details_expanded);
        assert_eq!(app.message, "Doctor selected-check details expanded.");
        assert!(!app.handle_key(KeyCode::Enter));
        assert!(!app.diagnostics_details_expanded);
        assert_eq!(app.message, "Doctor selected-check details collapsed.");
    }

    #[test]
    fn doctor_diagnostics_pass_loaded_repo_root() {
        let mut app = App::from_status("/tmp/repo", "repo", sample_status());
        app.start_diagnostics_with(|repo| {
            if repo == "/tmp/repo" {
                Ok(sample_diagnostic_report())
            } else {
                Err(format!("unexpected repo path: {repo}"))
            }
        });

        for _ in 0..1000 {
            app.poll_diagnostics();
            if matches!(app.diagnostics, DiagnosticsState::Completed(_)) {
                break;
            }
            std::thread::yield_now();
        }

        match &app.diagnostics {
            DiagnosticsState::Completed(report) => {
                assert_eq!(report.workspace, "/tmp/repo");
            }
            DiagnosticsState::Failed(error) => panic!("diagnostics failed: {error}"),
            DiagnosticsState::Idle | DiagnosticsState::Running => {
                panic!("diagnostics did not complete")
            }
        }
    }

    #[test]
    fn doctor_refresh_reruns_completed_diagnostics() {
        let mut app = App::from_status("/tmp/repo", "repo", sample_status());
        app.view = View::Diagnostics;
        app.diagnostics = DiagnosticsState::Completed(sample_diagnostic_report());
        app.diagnostics_details_expanded = true;

        app.handle_refresh_key_with(|app| {
            app.start_diagnostics_with(|repo| {
                if repo == "/tmp/repo" {
                    Ok(sample_diagnostic_report())
                } else {
                    Err(format!("unexpected repo path: {repo}"))
                }
            });
        });

        assert!(matches!(app.diagnostics, DiagnosticsState::Running));
        assert!(!app.diagnostics_details_expanded);
        assert_eq!(app.message, "Doctor diagnostics started.");
    }

    #[test]
    fn doctor_refresh_does_not_start_duplicate_run() {
        let mut app = App::from_status("/tmp/repo", "repo", sample_status());
        app.view = View::Diagnostics;
        app.start_diagnostics_with(|_| Ok(sample_diagnostic_report()));

        app.handle_refresh_key_with(|_| panic!("diagnostics should already be running"));

        assert!(matches!(app.diagnostics, DiagnosticsState::Running));
        assert_eq!(app.message, "Doctor diagnostics already running.");
    }

    #[test]
    fn renders_query_workbench_symbol_results() {
        let mut app = App::from_status("/tmp/repo", "repo", sample_status());
        app.view = View::Query;
        app.query.mode = QueryMode::Symbol;
        app.query.input = "add".to_owned();
        app.query.status = QueryStatus::Completed(QueryResult::Symbol(SymbolSearchSummary {
            repository_id: "repo".to_owned(),
            query: "add".to_owned(),
            symbols: vec![symdex_store::SymbolSearchRow {
                id: "symbol-1".to_owned(),
                name: "add".to_owned(),
                qualified_name: "crate::add".to_owned(),
                kind: "function".to_owned(),
                path: "src/lib.rs".to_owned(),
                start_line: 1,
                end_line: 3,
                provenance: sample_provenance(),
            }],
        }));
        let backend = TestBackend::new(100, 24);
        let mut terminal = Terminal::new(backend).expect("terminal should build");

        render(&mut terminal, &app).expect("render should succeed");

        let buffer = terminal.backend().buffer();
        let rendered = format!("{buffer:?}");
        assert!(rendered.contains("Query Workbench"));
        assert!(rendered.contains("Symbols"));
        assert!(rendered.contains("Kind"));
        assert!(rendered.contains("Lines"));
        assert!(rendered.contains("crate::add"));
        assert!(rendered.contains("src/lib.rs"));
        assert_eq!(cell_fg_for_text(buffer, "Kind", None), Some(Color::Cyan));
    }

    #[test]
    fn query_workbench_accepts_input_and_backspace() {
        let mut app = App::from_status("/tmp/repo", "repo", sample_status());
        app.view = View::Query;

        assert!(!app.handle_query_key(KeyCode::Char('a')));
        assert!(!app.handle_query_key(KeyCode::Char('d')));
        assert!(!app.handle_query_key(KeyCode::Char('d')));
        assert_eq!(app.query.input, "add");

        assert!(!app.handle_query_key(KeyCode::Backspace));
        assert_eq!(app.query.input, "ad");
    }

    #[test]
    fn query_workbench_toggles_modes() {
        let mut app = App::from_status("/tmp/repo", "repo", sample_status());
        app.view = View::Query;
        assert_eq!(app.query.mode, QueryMode::Symbol);

        assert!(!app.handle_key(KeyCode::Tab));
        assert_eq!(app.view, View::Query);
        assert_eq!(app.query.mode, QueryMode::Semantic);
    }

    #[test]
    fn query_workbench_moves_result_selection() {
        let mut app = App::from_status("/tmp/repo", "repo", sample_status());
        app.view = View::Query;
        app.query.status = QueryStatus::Completed(QueryResult::Symbol(SymbolSearchSummary {
            repository_id: "repo".to_owned(),
            query: "add".to_owned(),
            symbols: vec![
                sample_symbol("symbol-1", "crate::add", 1, 3),
                sample_symbol("symbol-2", "crate::subtract", 5, 8),
            ],
        }));

        assert_eq!(app.query.selection, 0);
        assert!(!app.handle_query_key(KeyCode::Down));
        assert_eq!(app.query.selection, 1);
        assert!(!app.handle_query_key(KeyCode::Down));
        assert_eq!(app.query.selection, 0);
        assert!(!app.handle_query_key(KeyCode::Up));
        assert_eq!(app.query.selection, 1);
    }

    #[test]
    fn query_workbench_scrolls_selected_rows_beyond_initial_view_capacity() {
        let mut app = App::from_status("/tmp/repo", "repo", sample_status());
        app.view = View::Query;
        app.query.status = QueryStatus::Completed(QueryResult::Symbol(SymbolSearchSummary {
            repository_id: "repo".to_owned(),
            query: "symbol".to_owned(),
            symbols: (0..24)
                .map(|index| {
                    sample_symbol(
                        format!("symbol-{index}"),
                        format!("crate::symbol_{index:02}"),
                        index + 1,
                        index + 2,
                    )
                })
                .collect(),
        }));

        for _ in 0..18 {
            assert!(!app.handle_query_key(KeyCode::Down));
        }
        assert_eq!(app.query.selection, 18);

        let backend = TestBackend::new(140, 24);
        let mut terminal = Terminal::new(backend).expect("terminal should build");
        render(&mut terminal, &app).expect("render should succeed");

        let buffer = terminal.backend().buffer();
        let rendered = format!("{buffer:?}");
        assert!(rendered.contains("crate::symbol_18"));
        assert_eq!(
            cell_bg_for_text(buffer, "crate::symbol_18", None),
            Some(Color::Cyan)
        );
    }

    #[test]
    fn renders_selected_result_row_highlight() {
        let mut app = App::from_status("/tmp/repo", "repo", sample_status());
        app.view = View::Query;
        app.query.selection = 1;
        app.query.status = QueryStatus::Completed(QueryResult::Symbol(SymbolSearchSummary {
            repository_id: "repo".to_owned(),
            query: "add".to_owned(),
            symbols: vec![
                sample_symbol("symbol-1", "crate::add", 1, 3),
                sample_symbol("symbol-2", "crate::subtract", 5, 8),
            ],
        }));
        let backend = TestBackend::new(140, 24);
        let mut terminal = Terminal::new(backend).expect("terminal should build");

        render(&mut terminal, &app).expect("render should succeed");

        assert_eq!(
            cell_bg_for_text(terminal.backend().buffer(), "crate::subtract", None),
            Some(Color::Cyan)
        );
    }

    #[test]
    fn renders_query_results_at_80x24() {
        let mut app = App::from_status("/tmp/repo", "repo", sample_status());
        app.view = View::Query;
        app.query.status = QueryStatus::Completed(QueryResult::Symbol(SymbolSearchSummary {
            repository_id: "repo".to_owned(),
            query: "add".to_owned(),
            symbols: vec![
                sample_symbol("symbol-1", "crate::add", 1, 3),
                sample_symbol("symbol-2", "crate::subtract", 5, 8),
            ],
        }));
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("terminal should build");

        render(&mut terminal, &app).expect("render should succeed");

        let rendered = format!("{:?}", terminal.backend().buffer());
        assert!(rendered.contains("symdex TUI"));
        assert!(rendered.contains("Query"));
        assert!(rendered.contains("Kind"));
        assert!(rendered.contains("Lines"));
        assert!(rendered.contains("Status"));
    }

    #[test]
    fn renders_symbol_call_graph_results() {
        let mut app = App::from_status("/tmp/repo", "repo", sample_status());
        app.view = View::Graph;
        app.graph.direction = CallDirection::Callers;
        app.graph.input = "add".to_owned();
        app.graph.status = GraphStatus::Completed(CallGraphSummary {
            repository_id: "repo".to_owned(),
            query: "add".to_owned(),
            direction: CallDirection::Callers,
            rows: vec![sample_call_row()],
        });
        let backend = TestBackend::new(180, 24);
        let mut terminal = Terminal::new(backend).expect("terminal should build");

        render(&mut terminal, &app).expect("render should succeed");

        let buffer = terminal.backend().buffer();
        let rendered = format!("{buffer:?}");
        assert!(rendered.contains("Symbol/Call Graph"));
        assert!(rendered.contains("callers"));
        assert!(rendered.contains("Conf"));
        assert!(rendered.contains("Resolution"));
        assert!(rendered.contains("crate::caller"));
        assert!(rendered.contains("resolved_exact"));
        assert_eq!(
            cell_fg_for_text(buffer, "resolved_exact", None),
            Some(Color::Green)
        );
    }

    #[test]
    fn renders_call_graph_results_at_80x24() {
        let mut app = App::from_status("/tmp/repo", "repo", sample_status());
        app.view = View::Graph;
        app.graph.direction = CallDirection::Callers;
        app.graph.status = GraphStatus::Completed(CallGraphSummary {
            repository_id: "repo".to_owned(),
            query: "add".to_owned(),
            direction: CallDirection::Callers,
            rows: vec![sample_call_row()],
        });
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("terminal should build");

        render(&mut terminal, &app).expect("render should succeed");

        let rendered = format!("{:?}", terminal.backend().buffer());
        assert!(rendered.contains("symdex TUI"));
        assert!(rendered.contains("Calls"));
        assert!(rendered.contains("Conf"));
        assert!(rendered.contains("Lines"));
        assert!(rendered.contains("Status"));
    }

    #[test]
    fn graph_browser_accepts_input_and_backspace() {
        let mut app = App::from_status("/tmp/repo", "repo", sample_status());
        app.view = View::Graph;

        assert!(!app.handle_graph_key(KeyCode::Char('a')));
        assert!(!app.handle_graph_key(KeyCode::Char('d')));
        assert!(!app.handle_graph_key(KeyCode::Char('d')));
        assert_eq!(app.graph.input, "add");

        assert!(!app.handle_graph_key(KeyCode::Backspace));
        assert_eq!(app.graph.input, "ad");
    }

    #[test]
    fn graph_browser_toggles_call_direction() {
        let mut app = App::from_status("/tmp/repo", "repo", sample_status());
        app.view = View::Graph;
        assert_eq!(app.graph.direction, CallDirection::Callers);

        assert!(!app.handle_key(KeyCode::Tab));
        assert_eq!(app.view, View::Graph);
        assert_eq!(app.graph.direction, CallDirection::Callees);
    }

    #[test]
    fn renders_impact_results() {
        let mut app = App::from_status("/tmp/repo", "repo", sample_status());
        app.view = View::Evidence;
        app.evidence.mode = EvidenceMode::Impact;
        app.evidence.input = "add".to_owned();
        app.evidence.status = EvidenceStatus::Completed(EvidenceResult::Impact(ImpactSummary {
            repository_id: "repo".to_owned(),
            query: "add".to_owned(),
            max_depth: 4,
            direct_callers: vec![sample_impact_call_evidence()],
            direct_callees: Vec::new(),
            transitive_callers: Vec::new(),
            transitive_callees: Vec::new(),
            related_files: Vec::new(),
            external_dependencies: Vec::new(),
            tests_likely: Vec::new(),
            notes: vec![
                "likely_tests_unavailable_until_test_discovery_mapping_is_indexed".to_owned(),
            ],
        }));
        let backend = TestBackend::new(180, 24);
        let mut terminal = Terminal::new(backend).expect("terminal should build");

        render(&mut terminal, &app).expect("render should succeed");

        let buffer = terminal.backend().buffer();
        let rendered = format!("{buffer:?}");
        assert!(rendered.contains("Impact/Call Path/Context Pack"));
        assert!(rendered.contains("Impact query"));
        assert!(rendered.contains("Direct callers"));
        assert!(rendered.contains("Edge"));
        assert!(rendered.contains("Resolution"));
        assert!(rendered.contains("resolved_exact"));
        assert!(rendered.contains("fresh"));
    }

    #[test]
    fn renders_call_path_results() {
        let mut app = App::from_status("/tmp/repo", "repo", sample_status());
        app.view = View::Evidence;
        app.evidence.mode = EvidenceMode::CallPath;
        app.evidence.input = "crate::caller -> crate::add".to_owned();
        app.evidence.status =
            EvidenceStatus::Completed(EvidenceResult::CallPath(sample_call_path_summary()));
        let backend = TestBackend::new(180, 24);
        let mut terminal = Terminal::new(backend).expect("terminal should build");

        render(&mut terminal, &app).expect("render should succeed");

        let rendered = format!("{:?}", terminal.backend().buffer());
        assert!(rendered.contains("Call path"));
        assert!(rendered.contains("crate::caller"));
        assert!(rendered.contains("crate::add"));
        assert!(rendered.contains("resolved_exact"));
    }

    #[test]
    fn renders_context_pack_metadata() {
        let mut app = App::from_status("/tmp/repo", "repo", sample_status());
        app.view = View::Evidence;
        app.evidence.mode = EvidenceMode::ContextPack;
        app.evidence.input = "add".to_owned();
        app.evidence.status =
            EvidenceStatus::Completed(EvidenceResult::ContextPack(sample_context_pack()));
        let backend = TestBackend::new(180, 24);
        let mut terminal = Terminal::new(backend).expect("terminal should build");

        render(&mut terminal, &app).expect("render should succeed");

        let buffer = terminal.backend().buffer();
        let rendered = format!("{buffer:?}");
        assert!(rendered.contains("symdex.context_pack.v1"));
        assert!(rendered.contains("Focus symbols"));
        assert!(rendered.contains("metadata_only_no_source_text"));
        assert!(rendered.contains("src/lib.rs"));
    }

    #[test]
    fn renders_context_pack_metadata_at_80x24() {
        let mut app = App::from_status("/tmp/repo", "repo", sample_status());
        app.view = View::Evidence;
        app.evidence.mode = EvidenceMode::ContextPack;
        app.evidence.status =
            EvidenceStatus::Completed(EvidenceResult::ContextPack(sample_context_pack()));
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("terminal should build");

        render(&mut terminal, &app).expect("render should succeed");

        let rendered = format!("{:?}", terminal.backend().buffer());
        assert!(rendered.contains("symdex TUI"));
        assert!(rendered.contains("Impact"));
        assert!(rendered.contains("Field"));
        assert!(rendered.contains("Value"));
        assert!(rendered.contains("Status"));
    }

    #[test]
    fn renders_debug_context_pack_metadata() {
        let mut app = App::from_status("/tmp/repo", "repo", sample_status());
        app.view = View::Evidence;
        app.evidence.mode = EvidenceMode::DebugContext;
        app.evidence.input = "thread 'main' panicked at src/lib.rs:7:5".to_owned();
        app.evidence.status =
            EvidenceStatus::Completed(EvidenceResult::DebugContext(sample_debug_context_pack()));
        let backend = TestBackend::new(180, 24);
        let mut terminal = Terminal::new(backend).expect("terminal should build");

        render(&mut terminal, &app).expect("render should succeed");

        let rendered = format!("{:?}", terminal.backend().buffer());
        assert!(rendered.contains("Debug Context"));
        assert!(rendered.contains("Frame"));
        assert!(rendered.contains("Path"));
        assert!(rendered.contains("Test"));
        assert!(rendered.contains("fresh"));
        assert!(rendered.contains("matched"));
        assert!(rendered.contains("crate::caller"));
        assert!(rendered.contains("test_add"));
        assert!(rendered.contains("run"));
    }

    #[test]
    fn renders_debug_context_pack_at_80x24() {
        let mut app = App::from_status("/tmp/repo", "repo", sample_status());
        app.view = View::Evidence;
        app.evidence.mode = EvidenceMode::DebugContext;
        app.evidence.status =
            EvidenceStatus::Completed(EvidenceResult::DebugContext(sample_debug_context_pack()));
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("terminal should build");

        render(&mut terminal, &app).expect("render should succeed");

        let rendered = format!("{:?}", terminal.backend().buffer());
        assert!(rendered.contains("symdex TUI"));
        assert!(rendered.contains("Debug"));
        assert!(rendered.contains("Kind"));
        assert!(rendered.contains("Fresh"));
        assert!(rendered.contains("Status"));
    }

    #[test]
    fn evidence_viewer_accepts_input_and_toggles_modes() {
        let mut app = App::from_status("/tmp/repo", "repo", sample_status());
        app.view = View::Evidence;
        assert_eq!(app.evidence.mode, EvidenceMode::Impact);

        assert!(!app.handle_evidence_key(KeyCode::Char('a')));
        assert!(!app.handle_evidence_key(KeyCode::Char('d')));
        assert!(!app.handle_evidence_key(KeyCode::Char('d')));
        assert_eq!(app.evidence.input, "add");

        assert!(!app.handle_key(KeyCode::Tab));
        assert_eq!(app.view, View::Evidence);
        assert_eq!(app.evidence.mode, EvidenceMode::CallPath);

        assert!(!app.handle_key(KeyCode::Tab));
        assert_eq!(app.view, View::Evidence);
        assert_eq!(app.evidence.mode, EvidenceMode::ContextPack);

        assert!(!app.handle_key(KeyCode::Tab));
        assert_eq!(app.view, View::Evidence);
        assert_eq!(app.evidence.mode, EvidenceMode::DebugContext);

        assert!(!app.handle_evidence_key(KeyCode::Backspace));
        assert_eq!(app.evidence.input, "ad");
    }

    #[test]
    fn call_path_input_accepts_spaced_arrow_separator() {
        let (source, target) =
            parse_call_path_input("crate::caller -> crate::add").expect("input should parse");

        assert_eq!(source, "crate::caller");
        assert_eq!(target, "crate::add");
    }

    #[test]
    fn reducer_requires_confirmation_before_indexing() {
        let request = ManualIndexRequest {
            mode: IndexMode::Offline,
            scope: IndexScope::Incremental,
        };
        let screen = reduce_screen(Screen::Dashboard, UiAction::RequestIndex(request));
        assert_eq!(screen, Screen::ConfirmIndex(request));

        let screen = reduce_screen(screen, UiAction::Confirm);
        assert_eq!(screen, Screen::IndexRunning(request));
    }

    #[test]
    fn reducer_cancels_pending_index_without_running() {
        let request = ManualIndexRequest {
            mode: IndexMode::Semantic,
            scope: IndexScope::Full,
        };
        let screen = reduce_screen(Screen::Dashboard, UiAction::RequestIndex(request));
        let screen = reduce_screen(screen, UiAction::Cancel);

        assert_eq!(screen, Screen::Dashboard);
    }

    #[test]
    fn reducer_tracks_index_completion_and_dismissal() {
        let request = ManualIndexRequest {
            mode: IndexMode::Offline,
            scope: IndexScope::Incremental,
        };
        let screen = reduce_screen(Screen::Dashboard, UiAction::RequestIndex(request));
        let screen = reduce_screen(screen, UiAction::Confirm);
        let screen = reduce_screen(screen, UiAction::JobSucceeded);
        assert_eq!(screen, Screen::IndexCompleted(request));

        let screen = reduce_screen(screen, UiAction::Dismiss);
        assert_eq!(screen, Screen::Dashboard);
    }

    fn sample_status() -> RepositoryStatus {
        RepositoryStatus {
            repository_id: "repo".to_owned(),
            current_ref_id: Some("ref".to_owned()),
            current_ref_kind: Some("branch".to_owned()),
            current_ref_name: Some("main".to_owned()),
            current_head_oid: Some("0123456789abcdef0123456789abcdef01234567".to_owned()),
            files_indexed: 2,
            chunks_indexed: 3,
            symbols_indexed: 4,
            calls_indexed: 5,
            last_indexed_at: Some("123".to_owned()),
            embedding_model: Some("nomic-embed-text".to_owned()),
            embedding_dimension: Some(768),
        }
    }

    fn sample_storage_summary() -> StorageExplorerSummary {
        StorageExplorerSummary {
            repository_id: "repo".to_owned(),
            sqlite: SqliteStorageSummary {
                repositories: 1,
                files: 2,
                chunks: 4,
                symbols: 3,
                calls: 1,
                index_runs: 1,
            },
            vector: VectorStorageProjection {
                collection_name: "symdex_repo_nomic_embed_text".to_owned(),
                embedding_model: "nomic-embed-text".to_owned(),
                embedding_dimension: Some(768),
                embeddable_chunks: 3,
                vector_backed_chunks: 2,
                excluded_chunks: 1,
                missing_vector_chunks: 1,
            },
            warnings: vec![
                StorageHealthRow {
                    status: StorageHealthStatus::Warning,
                    label: "missing_vectors".to_owned(),
                    detail: "1 embeddable chunks do not have sqlite-vec point IDs.".to_owned(),
                },
                StorageHealthRow {
                    status: StorageHealthStatus::Warning,
                    label: "excluded_chunks".to_owned(),
                    detail: "1 chunks are intentionally metadata-only.".to_owned(),
                },
            ],
        }
    }

    fn sample_index_coverage_summary() -> IndexCoverageSummary {
        IndexCoverageSummary {
            repository_id: "repo".to_owned(),
            files: vec![
                FileCoverageRow {
                    path: "src/lib.rs".to_owned(),
                    language: "rust".to_owned(),
                    chunks: 2,
                    symbols: 2,
                    calls: 1,
                    embeddable_chunks: 2,
                    vector_backed_chunks: 1,
                    excluded_chunks: 0,
                    status: FileCoverageStatus::MissingVector,
                    detail: FileDetailSummary {
                        chunks: vec![
                            FileChunkDetailRow {
                                kind: "function".to_owned(),
                                symbol: Some("crate::add".to_owned()),
                                start_line: 1,
                                end_line: 3,
                                vector_status: ChunkVectorStatus::VectorBacked,
                                excluded_reason: None,
                            },
                            FileChunkDetailRow {
                                kind: "function".to_owned(),
                                symbol: Some("crate::helper".to_owned()),
                                start_line: 5,
                                end_line: 8,
                                vector_status: ChunkVectorStatus::MissingVector,
                                excluded_reason: None,
                            },
                        ],
                        symbols: vec![FileSymbolDetailRow {
                            kind: "function".to_owned(),
                            qualified_name: "crate::add".to_owned(),
                            parent_symbol_id: None,
                            start_line: 1,
                            end_line: 3,
                        }],
                        calls: vec![FileCallDetailRow {
                            caller_symbol: "crate::add".to_owned(),
                            callee_text: "helper".to_owned(),
                            call_line: 2,
                            confidence: 1.0,
                            resolution_status: "resolved_exact".to_owned(),
                        }],
                    },
                },
                FileCoverageRow {
                    path: "src/secret.rs".to_owned(),
                    language: "rust".to_owned(),
                    chunks: 1,
                    symbols: 0,
                    calls: 0,
                    embeddable_chunks: 0,
                    vector_backed_chunks: 0,
                    excluded_chunks: 1,
                    status: FileCoverageStatus::Excluded,
                    detail: FileDetailSummary {
                        chunks: vec![FileChunkDetailRow {
                            kind: "function".to_owned(),
                            symbol: None,
                            start_line: 1,
                            end_line: 4,
                            vector_status: ChunkVectorStatus::Excluded,
                            excluded_reason: Some("secret_detected".to_owned()),
                        }],
                        symbols: Vec::new(),
                        calls: Vec::new(),
                    },
                },
            ],
        }
    }

    fn sample_symbol_outline_summary() -> SymbolOutlineSummary {
        SymbolOutlineSummary {
            repository_id: "repo".to_owned(),
            symbols: vec![
                SymbolOutlineRow {
                    id: "parent-symbol".to_owned(),
                    parent_symbol_id: None,
                    depth: 0,
                    child_count: 1,
                    kind: "struct".to_owned(),
                    qualified_name: "crate::Service".to_owned(),
                    name: "Service".to_owned(),
                    path: "src/lib.rs".to_owned(),
                    start_line: 1,
                    end_line: 10,
                },
                SymbolOutlineRow {
                    id: "child-symbol".to_owned(),
                    parent_symbol_id: Some("parent-symbol".to_owned()),
                    depth: 1,
                    child_count: 0,
                    kind: "function".to_owned(),
                    qualified_name: "crate::Service::run".to_owned(),
                    name: "run".to_owned(),
                    path: "src/lib.rs".to_owned(),
                    start_line: 5,
                    end_line: 8,
                },
            ],
        }
    }

    fn sample_call_resolution_summary() -> CallResolutionSummary {
        CallResolutionSummary {
            repository_id: "repo".to_owned(),
            buckets: vec![
                CallResolutionBucket {
                    resolution_status: "resolved_exact".to_owned(),
                    confidence_bucket: ConfidenceBucket::High,
                    call_count: 1,
                    average_confidence: 1.0,
                    rows: vec![CallResolutionEdgeRow {
                        path: "src/lib.rs".to_owned(),
                        caller_symbol: "crate::caller".to_owned(),
                        callee_text: "helper".to_owned(),
                        call_line: 4,
                        confidence: 1.0,
                        resolution_status: "resolved_exact".to_owned(),
                        confidence_bucket: ConfidenceBucket::High,
                    }],
                },
                CallResolutionBucket {
                    resolution_status: "unresolved".to_owned(),
                    confidence_bucket: ConfidenceBucket::Low,
                    call_count: 1,
                    average_confidence: 0.25,
                    rows: vec![CallResolutionEdgeRow {
                        path: "src/lib.rs".to_owned(),
                        caller_symbol: "crate::caller".to_owned(),
                        callee_text: "missing".to_owned(),
                        call_line: 7,
                        confidence: 0.25,
                        resolution_status: "unresolved".to_owned(),
                        confidence_bucket: ConfidenceBucket::Low,
                    }],
                },
            ],
        }
    }

    fn sample_embedding_coverage_summary() -> EmbeddingCoverageSummary {
        EmbeddingCoverageSummary {
            repository_id: "repo".to_owned(),
            collection_name: "symdex_repo_nomic_embed_text".to_owned(),
            configured_embedding_model: "nomic-embed-text".to_owned(),
            embedding_model: "nomic-embed-text".to_owned(),
            embedding_dimension: Some(768),
            total_chunks: 3,
            embeddable_chunks: 2,
            vector_backed_chunks: 1,
            missing_vector_chunks: 1,
            excluded_chunks: 1,
            latest_chunks_embedded: Some(1),
            exclusion_reasons: vec![EmbeddingExclusionRow {
                reason: "secret_detected".to_owned(),
                chunks: 1,
            }],
            health: vec![
                StorageHealthRow {
                    status: StorageHealthStatus::Warning,
                    label: "missing_vectors".to_owned(),
                    detail: "1 embeddable chunk has no recorded sqlite-vec point ID.".to_owned(),
                },
                StorageHealthRow {
                    status: StorageHealthStatus::Warning,
                    label: "excluded_chunks".to_owned(),
                    detail: "1 chunk is intentionally excluded from embeddings.".to_owned(),
                },
            ],
        }
    }

    fn sample_index_runs_timeline_summary() -> IndexRunsTimelineSummary {
        IndexRunsTimelineSummary {
            repository_id: "repo".to_owned(),
            runs: vec![
                IndexRunTimelineRow {
                    id: "run-failed".to_owned(),
                    started_at: "2026-01-02T00:00:00Z".to_owned(),
                    finished_at: Some("2026-01-02T00:00:04Z".to_owned()),
                    status: "failed".to_owned(),
                    embedding_model: "nomic-embed-text".to_owned(),
                    embedding_dimension: Some(768),
                    files_seen: 5,
                    files_indexed: 2,
                    chunks_embedded: 1,
                    error_summary: Some("sqlite-vec unavailable".to_owned()),
                    run_kind: "watch".to_owned(),
                },
                IndexRunTimelineRow {
                    id: "run-success".to_owned(),
                    started_at: "2026-01-01T00:00:00Z".to_owned(),
                    finished_at: Some("2026-01-01T00:00:10Z".to_owned()),
                    status: "success".to_owned(),
                    embedding_model: "nomic-embed-text".to_owned(),
                    embedding_dimension: Some(768),
                    files_seen: 4,
                    files_indexed: 3,
                    chunks_embedded: 7,
                    error_summary: None,
                    run_kind: "semantic".to_owned(),
                },
            ],
        }
    }

    fn sample_freshness_summary() -> FreshnessSummary {
        FreshnessSummary {
            repository_id: "repo".to_owned(),
            symbol_query: None,
            files: vec![
                FileFreshnessRow {
                    path: "src/lib.rs".to_owned(),
                    freshness: EvidenceFreshness::Fresh,
                    indexed_content_hash: Some("hash-1".to_owned()),
                    current_content_hash: Some("hash-1".to_owned()),
                    indexed_at: Some("2026-01-01T00:00:00Z".to_owned()),
                    index_run_id: Some("run-success".to_owned()),
                    parser_version: Some("tree-sitter-rust".to_owned()),
                },
                FileFreshnessRow {
                    path: "src/stale.rs".to_owned(),
                    freshness: EvidenceFreshness::Stale,
                    indexed_content_hash: Some("old".to_owned()),
                    current_content_hash: Some("new".to_owned()),
                    indexed_at: Some("2026-01-01T00:00:00Z".to_owned()),
                    index_run_id: Some("run-success".to_owned()),
                    parser_version: Some("tree-sitter-rust".to_owned()),
                },
            ],
            focus_symbols: Vec::new(),
            context_pack: None,
        }
    }

    fn sample_semantic_neighborhood_summary() -> SemanticNeighborhoodSummary {
        SemanticNeighborhoodSummary {
            repository_id: "repo".to_owned(),
            collection_name: "symdex_repo_nomic_embed_text".to_owned(),
            embedding_model: "nomic-embed-text".to_owned(),
            rows: vec![
                SemanticNeighborhoodRow {
                    vector_point_id: "point-add".to_owned(),
                    path: "src/lib.rs".to_owned(),
                    start_line: 1,
                    end_line: 3,
                    symbol_name: Some("crate::add".to_owned()),
                    chunk_kind: "function".to_owned(),
                    language: "rust".to_owned(),
                    score: None,
                    text_hash: "hash-vector".to_owned(),
                },
                SemanticNeighborhoodRow {
                    vector_point_id: "point-worker".to_owned(),
                    path: "src/worker.rs".to_owned(),
                    start_line: 10,
                    end_line: 18,
                    symbol_name: Some("crate::worker".to_owned()),
                    chunk_kind: "function".to_owned(),
                    language: "rust".to_owned(),
                    score: None,
                    text_hash: "hash-worker".to_owned(),
                },
            ],
            health: vec![StorageHealthRow {
                status: StorageHealthStatus::Ok,
                label: "metadata_only".to_owned(),
                detail: "2 sqlite-vec payload metadata rows are available without source text."
                    .to_owned(),
            }],
        }
    }

    fn sample_cross_store_health_summary() -> CrossStoreHealthSummary {
        CrossStoreHealthSummary {
            repository_id: "repo".to_owned(),
            collection_name: "symdex_repo_nomic_embed_text".to_owned(),
            rows: vec![
                StorageHealthRow {
                    status: StorageHealthStatus::Warning,
                    label: "missing_vectors".to_owned(),
                    detail: "1 embeddable chunk is missing a recorded sqlite-vec point ID.".to_owned(),
                },
                StorageHealthRow {
                    status: StorageHealthStatus::Warning,
                    label: "excluded_chunks".to_owned(),
                    detail: "1 chunk is intentionally excluded from semantic embedding."
                        .to_owned(),
                },
                StorageHealthRow {
                    status: StorageHealthStatus::Error,
                    label: "model_drift".to_owned(),
                    detail: "Configured model nomic-embed-text differs from latest indexed model different-model.".to_owned(),
                },
                StorageHealthRow {
                    status: StorageHealthStatus::Error,
                    label: "dimension_drift".to_owned(),
                    detail: "Successful runs recorded multiple dimensions: 768, 1024.".to_owned(),
                },
            ],
        }
    }

    fn sample_diagnostic_report() -> DiagnosticReport {
        DiagnosticReport {
            workspace: "/tmp/repo".to_owned(),
            sqlite_path: ".symdex/symdex.sqlite".to_owned(),
            vector_store: "sqlite_vec".to_owned(),
            ollama_url: "http://localhost:11434".to_owned(),
            embed_model: "nomic-embed-text".to_owned(),
            checks: vec![
                DiagnosticCheck {
                    label: "sqlite_parent".to_owned(),
                    state: DiagnosticState::Ok,
                    message: ".symdex".to_owned(),
                },
                DiagnosticCheck {
                    label: "sqlite_vec_status".to_owned(),
                    state: DiagnosticState::Unreachable,
                    message: "connection refused".to_owned(),
                },
            ],
        }
    }

    fn sample_call_row() -> CallSearchRow {
        CallSearchRow {
            callee_text: "add".to_owned(),
            call_line: 7,
            confidence: 1.0,
            resolution_status: "resolved_exact".to_owned(),
            symbol_id: Some("symbol-1".to_owned()),
            symbol_name: Some("caller".to_owned()),
            symbol_qualified_name: Some("crate::caller".to_owned()),
            symbol_kind: Some("function".to_owned()),
            path: Some("src/lib.rs".to_owned()),
            start_line: Some(5),
            end_line: Some(8),
            provenance: sample_provenance(),
        }
    }

    fn sample_impact_call_evidence() -> ImpactCallEvidence {
        ImpactCallEvidence {
            row: sample_call_row(),
            freshness: EvidenceFreshness::Fresh,
            trust: sample_trust(),
            reasons: vec!["relationship:direct_caller".to_owned()],
        }
    }

    fn sample_call_path_summary() -> CallPathSummary {
        CallPathSummary {
            repository_id: "repo".to_owned(),
            source_query: "crate::caller".to_owned(),
            target_query: "crate::add".to_owned(),
            max_depth: 4,
            paths: vec![CallPath {
                hops: 1,
                min_confidence: 1.0,
                terminal_resolution_status: "resolved_exact".to_owned(),
                edges: vec![sample_call_path_edge()],
            }],
        }
    }

    fn sample_call_path_edge() -> CallPathEdge {
        CallPathEdge {
            call_id: "call-1".to_owned(),
            caller_symbol_id: "symbol-caller".to_owned(),
            caller_symbol_name: "caller".to_owned(),
            caller_symbol_qualified_name: "crate::caller".to_owned(),
            caller_symbol_kind: "function".to_owned(),
            caller_path: "src/lib.rs".to_owned(),
            caller_start_line: 5,
            caller_end_line: 8,
            callee_text: "add".to_owned(),
            callee_symbol_id: Some("symbol-add".to_owned()),
            callee_symbol_name: Some("add".to_owned()),
            callee_symbol_qualified_name: Some("crate::add".to_owned()),
            callee_symbol_kind: Some("function".to_owned()),
            callee_path: Some("src/lib.rs".to_owned()),
            callee_start_line: Some(1),
            callee_end_line: Some(3),
            call_line: 7,
            confidence: 1.0,
            resolution_status: "resolved_exact".to_owned(),
            provenance: sample_provenance(),
        }
    }

    fn sample_symbol(
        id: impl Into<String>,
        qualified_name: impl Into<String>,
        start_line: usize,
        end_line: usize,
    ) -> SymbolSearchRow {
        let qualified_name = qualified_name.into();
        SymbolSearchRow {
            id: id.into(),
            name: qualified_name
                .rsplit("::")
                .next()
                .unwrap_or(qualified_name.as_str())
                .to_owned(),
            qualified_name,
            kind: "function".to_owned(),
            path: "src/lib.rs".to_owned(),
            start_line,
            end_line,
            provenance: sample_provenance(),
        }
    }

    fn sample_context_pack() -> ContextPack {
        ContextPack {
            format: "symdex.context_pack.v1".to_owned(),
            repository_id: "repo".to_owned(),
            query: "add".to_owned(),
            focus_symbols: vec![SymbolSearchRow {
                id: "symbol-1".to_owned(),
                name: "add".to_owned(),
                qualified_name: "crate::add".to_owned(),
                kind: "function".to_owned(),
                path: "src/lib.rs".to_owned(),
                start_line: 1,
                end_line: 3,
                provenance: sample_provenance(),
            }],
            direct_callers: vec![sample_call_row()],
            direct_callees: Vec::new(),
            files: vec!["src/lib.rs".to_owned()],
            limits: ContextPackLimits {
                max_symbols: 8,
                max_callers: 8,
                max_callees: 8,
            },
            notes: vec!["metadata_only_no_source_text".to_owned()],
        }
    }

    fn sample_debug_context_pack() -> DebugContextPack {
        DebugContextPack {
            format: "symdex.debug_context.v1".to_owned(),
            repository_id: "repo".to_owned(),
            runtime_observation: None,
            frames: vec![
                DebugFrameMatch {
                    frame: RuntimeFrame {
                        ordinal: 0,
                        raw: "thread 'main' panicked at src/lib.rs:7:5".to_owned(),
                        symbol: Some("caller".to_owned()),
                        path: Some("src/lib.rs".to_owned()),
                        line: Some(7),
                        column: Some(5),
                    },
                    normalized_path: Some("src/lib.rs".to_owned()),
                    file_freshness: EvidenceFreshness::Fresh,
                    file_provenance: Some(sample_provenance()),
                    trust: sample_trust(),
                    reasons: vec!["symbols_at_runtime_location".to_owned()],
                    matched_symbols: vec![SymbolSearchRow {
                        id: "symbol-caller".to_owned(),
                        name: "caller".to_owned(),
                        qualified_name: "crate::caller".to_owned(),
                        kind: "function".to_owned(),
                        path: "src/lib.rs".to_owned(),
                        start_line: 5,
                        end_line: 8,
                        provenance: sample_provenance(),
                    }],
                    calls_at_line: vec![CallPath {
                        hops: 1,
                        min_confidence: 1.0,
                        terminal_resolution_status: "resolved_exact".to_owned(),
                        edges: vec![sample_call_path_edge()],
                    }],
                    matched: true,
                },
                DebugFrameMatch {
                    frame: RuntimeFrame {
                        ordinal: 1,
                        raw: "at src/lib.rs:1:1".to_owned(),
                        symbol: Some("add".to_owned()),
                        path: Some("src/lib.rs".to_owned()),
                        line: Some(1),
                        column: Some(1),
                    },
                    normalized_path: Some("src/lib.rs".to_owned()),
                    file_freshness: EvidenceFreshness::Stale,
                    file_provenance: Some(sample_provenance()),
                    trust: sample_trust(),
                    reasons: vec!["symbols_at_runtime_location".to_owned()],
                    matched_symbols: vec![sample_symbol(
                        "symbol-add".to_owned(),
                        "crate::add".to_owned(),
                        1,
                        3,
                    )],
                    calls_at_line: Vec::new(),
                    matched: true,
                },
            ],
            call_paths_between_frames: vec![DebugFrameCallPath {
                from_frame: 0,
                to_frame: 1,
                from_symbol: "crate::caller".to_owned(),
                to_symbol: "crate::add".to_owned(),
                paths: sample_call_path_summary().paths,
            }],
            likely_tests: vec!["test_add".to_owned()],
            limits: DebugContextLimits {
                max_frames: 8,
                max_symbols_per_frame: 8,
                max_calls_per_frame: 8,
                max_call_paths_between_frames: 8,
            },
            notes: vec!["metadata_only_no_source_text".to_owned()],
        }
    }

    fn sample_provenance() -> EvidenceProvenance {
        EvidenceProvenance {
            content_hash: Some("content-hash".to_owned()),
            index_run_id: Some("run".to_owned()),
            parser_version: Some("parser".to_owned()),
            indexed_at: Some("2026-04-30T00:00:00Z".to_owned()),
            embedding_model: None,
            embedding_dimension: None,
            embedded_at: None,
        }
    }

    fn sample_trust() -> EvidenceTrust {
        EvidenceTrust {
            score: 1.0,
            level: "high".to_owned(),
            factors: vec!["freshness:fresh".to_owned()],
        }
    }

    fn cell_fg_for_text(
        buffer: &ratatui::buffer::Buffer,
        text: &str,
        row: Option<u16>,
    ) -> Option<Color> {
        cell_color_for_text(buffer, text, row, |cell| cell.fg)
    }

    fn cell_bg_for_text(
        buffer: &ratatui::buffer::Buffer,
        text: &str,
        row: Option<u16>,
    ) -> Option<Color> {
        cell_color_for_text(buffer, text, row, |cell| cell.bg)
    }

    fn cell_color_for_text(
        buffer: &ratatui::buffer::Buffer,
        text: &str,
        row: Option<u16>,
        color: impl Fn(&ratatui::buffer::Cell) -> Color,
    ) -> Option<Color> {
        let y_start = row.unwrap_or(buffer.area.y);
        let y_end = row
            .map(|value| value.saturating_add(1))
            .unwrap_or(buffer.area.y + buffer.area.height);
        for y in y_start..y_end {
            for x in buffer.area.x..buffer.area.x + buffer.area.width {
                if text_starts_at(buffer, text, x, y) {
                    return buffer.cell(Position { x, y }).map(&color);
                }
            }
        }
        None
    }

    fn text_starts_at(buffer: &ratatui::buffer::Buffer, text: &str, x: u16, y: u16) -> bool {
        for (offset, expected) in text.chars().enumerate() {
            let Some(cell) = buffer.cell(Position {
                x: x + offset as u16,
                y,
            }) else {
                return false;
            };
            if cell.symbol() != expected.to_string() {
                return false;
            }
        }
        true
    }
}

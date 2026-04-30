//! Terminal UI state, rendering, events, and terminal lifecycle.

use std::io::{self, Stdout};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::{Backend, CrosstermBackend};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Gauge, List, ListItem, Paragraph, Tabs, Wrap};
use ratatui::widgets::{Cell, Row, Table, TableState};
use symdex_core::RepoRoot;
use symdex_diagnostics::{DiagnosticCheck, DiagnosticReport, DiagnosticState, run_diagnostics};
use symdex_embed::EmbedConfig;
use symdex_index::{
    EmbeddingSummary, IndexOptions, IndexProgress, IndexSummary, run_index_with_progress,
};
use symdex_query::{
    CallDirection, CallGraphSummary, ImpactSummary, QueryMode, QueryResult, SemanticSearchSummary,
    SymbolSearchSummary, run_call_graph, run_context_pack, run_impact, run_semantic_search,
    run_symbol_search,
};
use symdex_store::{ContextPack, RepositoryStatus, SqliteStore, StoreConfig};

pub struct TuiOptions {
    pub repo: String,
}

pub fn run(options: TuiOptions) -> Result<(), String> {
    let app = App::load(&options.repo)?;
    let mut terminal = enter_terminal()?;
    let result = run_app(&mut terminal, app);
    leave_terminal(&mut terminal)?;
    result
}

pub fn help_text() -> &'static str {
    "USAGE:\n    symdex tui [repo]\n\nStarts the local terminal UI control panel.\n\nKEYS:\n    i         Show indexing controls\n    d         Run doctor diagnostics\n    w         Show query workbench\n    g         Show symbol/call graph browser\n    p         Show impact/context-pack viewer\n    Tab       Switch to the next tab view\n    Shift+Tab Switch to the previous tab view\n    F2        Toggle mode in query, graph, and impact/context views\n    Up/Down   Move selected result row\n    Enter     Run lookup, toggle Doctor details, or dismiss a completed job\n    o         Confirm offline indexing\n    s         Confirm semantic indexing\n    r         Refresh repository status\n    y / n     Confirm or cancel a pending job\n    q / Esc   Quit or cancel\n"
}

pub struct App {
    repo_input: String,
    repo_root: String,
    repository_id: String,
    sqlite_path: String,
    qdrant_url: String,
    ollama_url: String,
    embed_model: String,
    status: RepositoryStatus,
    message: String,
    view: View,
    screen: Screen,
    last_index_summary: Option<IndexSummary>,
    index_progress: Option<IndexProgress>,
    diagnostics: DiagnosticsState,
    diagnostics_selection: usize,
    diagnostics_details_expanded: bool,
    query: QueryWorkbenchState,
    graph: GraphBrowserState,
    evidence: EvidenceViewerState,
    last_error: Option<String>,
    index_receiver: Option<Receiver<IndexJobMessage>>,
    diagnostics_receiver: Option<Receiver<Result<DiagnosticReport, String>>>,
    query_receiver: Option<Receiver<Result<QueryResult, String>>>,
    graph_receiver: Option<Receiver<Result<CallGraphSummary, String>>>,
    evidence_receiver: Option<Receiver<Result<EvidenceResult, String>>>,
}

impl App {
    pub fn load(repo: &str) -> Result<Self, String> {
        let root = RepoRoot::open(repo).map_err(|error| error.to_string())?;
        let store_config = StoreConfig::from_env();
        let embed_config = EmbedConfig::from_env();
        let sqlite = SqliteStore::open(&store_config).map_err(|error| error.to_string())?;
        sqlite.migrate().map_err(|error| error.to_string())?;
        let status = sqlite
            .repository_status(root.id())
            .map_err(|error| error.to_string())?;

        Ok(Self {
            repo_input: repo.to_owned(),
            repo_root: root.path().display().to_string(),
            repository_id: root.id().to_owned(),
            sqlite_path: store_config.sqlite_path.display().to_string(),
            qdrant_url: store_config.qdrant_url,
            ollama_url: embed_config.ollama_url,
            embed_model: embed_config.model,
            status,
            message: "Dashboard loaded. Press q or Esc to quit.".to_owned(),
            view: View::Indexing,
            screen: Screen::Dashboard,
            last_index_summary: None,
            index_progress: None,
            diagnostics: DiagnosticsState::Idle,
            diagnostics_selection: 0,
            diagnostics_details_expanded: false,
            query: QueryWorkbenchState::default(),
            graph: GraphBrowserState::default(),
            evidence: EvidenceViewerState::default(),
            last_error: None,
            index_receiver: None,
            diagnostics_receiver: None,
            query_receiver: None,
            graph_receiver: None,
            evidence_receiver: None,
        })
    }

    pub fn from_status(
        repo_root: impl Into<String>,
        repository_id: impl Into<String>,
        status: RepositoryStatus,
    ) -> Self {
        let repo_root = repo_root.into();
        Self {
            repo_input: repo_root.clone(),
            repo_root,
            repository_id: repository_id.into(),
            sqlite_path: ".symdex/symdex.sqlite".to_owned(),
            qdrant_url: "http://localhost:6333".to_owned(),
            ollama_url: "http://localhost:11434".to_owned(),
            embed_model: "nomic-embed-text".to_owned(),
            status,
            message: "Dashboard loaded. Press q or Esc to quit.".to_owned(),
            view: View::Indexing,
            screen: Screen::Dashboard,
            last_index_summary: None,
            index_progress: None,
            diagnostics: DiagnosticsState::Idle,
            diagnostics_selection: 0,
            diagnostics_details_expanded: false,
            query: QueryWorkbenchState::default(),
            graph: GraphBrowserState::default(),
            evidence: EvidenceViewerState::default(),
            last_error: None,
            index_receiver: None,
            diagnostics_receiver: None,
            query_receiver: None,
            graph_receiver: None,
            evidence_receiver: None,
        }
    }

    fn index_lines(&self) -> Vec<Line<'_>> {
        let mut lines = vec![
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
            Line::from(vec![
                Span::styled(
                    "Refresh status: ",
                    Style::new().add_modifier(Modifier::BOLD),
                ),
                Span::raw("press r"),
            ]),
            Line::from(""),
        ];

        match self.screen {
            Screen::Dashboard => {
                lines.push(Line::from(vec![
                    status_span("idle", StatusTone::Dim),
                    Span::raw(" No indexing job is pending."),
                ]));
            }
            Screen::ConfirmIndex(mode) => {
                lines.push(Line::from(vec![
                    status_span("confirm", StatusTone::Warning),
                    Span::raw(" "),
                    Span::raw(format!(
                        "Run {} indexing for this repository?",
                        mode.label()
                    )),
                ]));
                lines.push(Line::from("Press y to start, n or Esc to cancel."));
            }
            Screen::IndexRunning(mode) => {
                lines.push(Line::from(vec![
                    status_span("running", StatusTone::Info),
                    Span::raw(" "),
                    Span::raw(format!("{} indexing", mode.label())),
                ]));
                lines.push(Line::from("The TUI will update when the job finishes."));
            }
            Screen::IndexCompleted(mode) => {
                lines.push(Line::from(vec![
                    status_span("complete", StatusTone::Success),
                    Span::raw(" "),
                    Span::raw(format!("{} indexing", mode.label())),
                ]));
                if let Some(summary) = &self.last_index_summary {
                    lines.extend(summary_lines(summary));
                }
            }
            Screen::IndexFailed(mode) => {
                lines.push(Line::from(vec![
                    status_span("failed", StatusTone::Error),
                    Span::raw(" "),
                    Span::raw(format!("{} indexing", mode.label())),
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
                Span::raw("press d"),
            ]),
            Line::from(vec![
                Span::styled(
                    "Index controls: ",
                    Style::new().add_modifier(Modifier::BOLD),
                ),
                Span::raw("press i"),
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
                Span::raw(format!("{} (F2 toggles)", self.query.mode.label())),
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
                Span::raw(format!("{} (F2 toggles)", self.graph.direction.label())),
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
                Span::raw(format!("{} (F2 toggles)", self.evidence.mode.label())),
            ]),
            Line::from(vec![
                Span::styled("Symbol: ", Style::new().add_modifier(Modifier::BOLD)),
                Span::raw(if self.evidence.input.is_empty() {
                    "<type symbol name>".to_owned()
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
                    Span::raw(" No impact or context-pack lookup has run."),
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

    fn refresh_status(&mut self) -> Result<(), String> {
        let store_config = StoreConfig::from_env();
        let sqlite = SqliteStore::open(&store_config).map_err(|error| error.to_string())?;
        sqlite.migrate().map_err(|error| error.to_string())?;
        self.status = sqlite
            .repository_status(&self.repository_id)
            .map_err(|error| error.to_string())?;
        self.message = "Repository status refreshed.".to_owned();
        Ok(())
    }

    fn diagnostics_row_count(&self) -> usize {
        match &self.diagnostics {
            DiagnosticsState::Completed(report) => report.checks.len(),
            _ => 0,
        }
    }

    fn handle_key(&mut self, code: KeyCode) -> bool {
        match code {
            KeyCode::Tab => {
                self.view = self.view.next();
                self.message = format!("{} view selected.", self.view.label());
                return false;
            }
            KeyCode::BackTab => {
                self.view = self.view.previous();
                self.message = format!("{} view selected.", self.view.label());
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
            KeyCode::Esc if matches!(self.screen, Screen::ConfirmIndex(_)) => {
                self.screen = reduce_screen(self.screen, UiAction::Cancel);
                self.message = "Indexing cancelled before start.".to_owned();
            }
            KeyCode::Esc => return true,
            KeyCode::Char('i') => {
                self.view = View::Indexing;
                self.message = "Indexing controls selected.".to_owned();
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
            KeyCode::Char('d') => {
                self.start_diagnostics();
            }
            KeyCode::Char('w') => {
                self.view = View::Query;
                self.message = "Query workbench selected.".to_owned();
            }
            KeyCode::Char('g') => {
                self.view = View::Graph;
                self.message = "Symbol/call graph browser selected.".to_owned();
            }
            KeyCode::Char('p') => {
                self.view = View::Evidence;
                self.message = "Impact/context-pack viewer selected.".to_owned();
            }
            KeyCode::Char('o') if self.screen.accepts_new_index_request() => {
                self.screen =
                    reduce_screen(self.screen, UiAction::RequestIndex(IndexMode::Offline));
                self.message = "Confirm offline indexing before starting.".to_owned();
            }
            KeyCode::Char('s') if self.screen.accepts_new_index_request() => {
                self.screen =
                    reduce_screen(self.screen, UiAction::RequestIndex(IndexMode::Semantic));
                self.message = "Confirm semantic indexing before starting.".to_owned();
            }
            KeyCode::Char('y') => {
                if let Screen::ConfirmIndex(mode) = self.screen {
                    self.start_index_job(mode);
                }
            }
            KeyCode::Char('n') if matches!(self.screen, Screen::ConfirmIndex(_)) => {
                self.screen = reduce_screen(self.screen, UiAction::Cancel);
                self.message = "Indexing cancelled before start.".to_owned();
            }
            KeyCode::Enter if self.screen.is_terminal_job_state() => {
                self.screen = reduce_screen(self.screen, UiAction::Dismiss);
                self.message = "Dashboard loaded. Press q or Esc to quit.".to_owned();
            }
            KeyCode::Char('r') if !matches!(self.screen, Screen::IndexRunning(_)) => {
                if let Err(error) = self.refresh_status() {
                    self.message = format!("Refresh failed: {error}");
                }
            }
            _ => {}
        }
        false
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
            KeyCode::F(2) => {
                self.query.mode = self.query.mode.toggled();
                self.query.status = QueryStatus::Idle;
                self.query.selection = 0;
                self.message = format!("Query mode set to {}.", self.query.mode.label());
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
            KeyCode::F(2) => {
                self.graph.direction = self.graph.direction.toggled();
                self.graph.status = GraphStatus::Idle;
                self.graph.selection = 0;
                self.message = format!("Graph mode set to {}.", self.graph.direction.label());
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
            KeyCode::F(2) => {
                self.evidence.mode = self.evidence.mode.toggled();
                self.evidence.status = EvidenceStatus::Idle;
                self.evidence.selection = 0;
                self.message = format!("Evidence mode set to {}.", self.evidence.mode.label());
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

    fn start_index_job(&mut self, mode: IndexMode) {
        let repo = self.repo_input.clone();
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let progress_sender = sender.clone();
            let result = run_index_with_progress(
                &IndexOptions {
                    repo,
                    offline: matches!(mode, IndexMode::Offline),
                },
                move |progress| {
                    let _ = progress_sender.send(IndexJobMessage::Progress(progress));
                },
            );
            let _ = sender.send(IndexJobMessage::Finished(result));
        });
        self.index_receiver = Some(receiver);
        self.last_index_summary = None;
        self.index_progress = Some(IndexProgress {
            phase: "start",
            completed: 0,
            total: 1,
            message: format!("Starting {} indexing", mode.label()),
        });
        self.last_error = None;
        self.screen = reduce_screen(self.screen, UiAction::Confirm);
        self.message = format!("{} indexing started.", mode.label());
    }

    fn start_diagnostics(&mut self) {
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let result = run_diagnostics();
            let _ = sender.send(result);
        });
        self.view = View::Diagnostics;
        self.diagnostics = DiagnosticsState::Running;
        self.diagnostics_details_expanded = false;
        self.diagnostics_receiver = Some(receiver);
        self.message = "Doctor diagnostics started.".to_owned();
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
                EvidenceMode::ContextPack => {
                    run_context_pack(&repo, &query, 8).map(EvidenceResult::ContextPack)
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
                let mode = self.screen.index_mode().unwrap_or(IndexMode::Offline);
                self.message = format!(
                    "{} indexing completed: {} files indexed, {} chunks indexed.",
                    mode.label(),
                    summary.sqlite_files_indexed,
                    summary.sqlite_chunks_indexed
                );
                let completed_message = self.message.clone();
                self.last_index_summary = Some(summary);
                self.index_progress = None;
                self.screen = reduce_screen(self.screen, UiAction::JobSucceeded);
                if let Err(error) = self.refresh_status() {
                    self.message =
                        format!("Indexing completed, but status refresh failed: {error}");
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
                    Constraint::Length(3),
                ])
                .split(frame.area());

            let tabs = Tabs::new(View::tabs())
                .block(Block::default().borders(Borders::ALL).title("symdex TUI"))
                .select(app.view.tab_index())
                .style(Style::new().fg(Color::DarkGray))
                .highlight_style(Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD));
            frame.render_widget(tabs, chunks[0]);

            let body_chunks = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(45), Constraint::Percentage(55)])
                .split(chunks[1]);

            render_repository_status_panel(frame, body_chunks[0], app);

            render_right_panel(frame, body_chunks[1], app);

            let footer = Paragraph::new(Line::from(vec![
                status_span(app.view.footer_label(), StatusTone::Info),
                Span::raw(" "),
                Span::raw(app.view.footer_help()),
                Span::raw(" | "),
                Span::styled("status: ", Style::new().add_modifier(Modifier::BOLD)),
                Span::raw(app.message.as_str()),
            ]))
            .wrap(Wrap { trim: true })
            .block(Block::default().borders(Borders::ALL).title("Status"));
            frame.render_widget(footer, chunks[2]);
        })
        .map(|_| ())
        .map_err(|error| error.to_string())
}

fn render_right_panel(frame: &mut ratatui::Frame<'_>, area: Rect, app: &App) {
    match app.view {
        View::Diagnostics => match &app.diagnostics {
            DiagnosticsState::Completed(report) => {
                render_diagnostics_panel(frame, area, app, report);
            }
            _ => render_line_panel(frame, area, "Doctor Diagnostics", app.diagnostics_lines()),
        },
        View::Query => match &app.query.status {
            QueryStatus::Completed(result) => {
                render_selectable_table(
                    frame,
                    area,
                    query_table(result),
                    app.query.selection,
                    query_result_count(result),
                );
            }
            _ => render_line_panel(frame, area, "Query Workbench", app.query_lines()),
        },
        View::Graph => match &app.graph.status {
            GraphStatus::Completed(summary) => {
                render_selectable_table(
                    frame,
                    area,
                    call_graph_table(summary),
                    app.graph.selection,
                    summary.rows.len().min(12),
                );
            }
            _ => render_line_panel(frame, area, "Symbol/Call Graph", app.graph_lines()),
        },
        View::Evidence => match &app.evidence.status {
            EvidenceStatus::Completed(EvidenceResult::Impact(summary)) => {
                render_selectable_table(
                    frame,
                    area,
                    impact_table(summary),
                    app.evidence.selection,
                    impact_result_count(summary),
                );
            }
            EvidenceStatus::Completed(EvidenceResult::ContextPack(pack)) => {
                render_selectable_table(
                    frame,
                    area,
                    context_pack_table(pack),
                    app.evidence.selection,
                    context_pack_row_count(pack),
                );
            }
            _ => render_line_panel(frame, area, "Impact/Context Pack", app.evidence_lines()),
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
            Constraint::Length(4),
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
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(5), Constraint::Length(3)])
            .split(inner);
        let panel = Paragraph::new(app.index_lines()).wrap(Wrap { trim: true });
        frame.render_widget(panel, chunks[0]);
        let progress = app.index_progress.as_ref();
        let gauge = Gauge::default()
            .block(Block::default().borders(Borders::ALL).title("Progress"))
            .gauge_style(tone_style(StatusTone::Info).add_modifier(Modifier::BOLD))
            .percent(progress_percent(progress))
            .label(progress_label(progress));
        frame.render_widget(gauge, chunks[1]);
    } else {
        let panel = Paragraph::new(app.index_lines()).wrap(Wrap { trim: true });
        frame.render_widget(panel, inner);
    }
}

fn render_line_panel(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    title: &'static str,
    lines: Vec<Line<'_>>,
) {
    let panel = List::new(lines.into_iter().map(ListItem::new).collect::<Vec<_>>())
        .block(Block::default().borders(Borders::ALL).title(title));
    frame.render_widget(panel, area);
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
}

fn run_app<B: Backend>(terminal: &mut Terminal<B>, app: App) -> Result<(), String> {
    let mut app = app;
    loop {
        app.poll_index_job();
        app.poll_diagnostics();
        app.poll_query();
        app.poll_graph();
        app.poll_evidence();
        render(terminal, &app)?;
        if !event::poll(Duration::from_millis(250)).map_err(|error| error.to_string())? {
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

fn enter_terminal() -> Result<Terminal<CrosstermBackend<Stdout>>, String> {
    enable_raw_mode().map_err(|error| error.to_string())?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen).map_err(|error| {
        let _ = disable_raw_mode();
        error.to_string()
    })?;
    Terminal::new(CrosstermBackend::new(stdout)).map_err(|error| {
        let _ = disable_raw_mode();
        error.to_string()
    })
}

fn leave_terminal(terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> Result<(), String> {
    disable_raw_mode().map_err(|error| error.to_string())?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen).map_err(|error| error.to_string())?;
    terminal.show_cursor().map_err(|error| error.to_string())
}

fn index_embedding(status: &RepositoryStatus) -> String {
    match (&status.embedding_model, status.embedding_dimension) {
        (Some(model), Some(dimension)) => format!("{model} ({dimension})"),
        (Some(model), None) => model.clone(),
        _ => "<none>".to_owned(),
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
            Span::styled("Qdrant: ", Style::new().add_modifier(Modifier::BOLD)),
            Span::raw(report.qdrant_url.as_str()),
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
                Line::from(format!("Collection: {}", summary.qdrant_collection)),
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
        EvidenceResult::ContextPack(pack) => context_pack_lines(pack),
    }
}

fn impact_lines(summary: &ImpactSummary) -> Vec<Line<'_>> {
    let mut lines = vec![
        Line::from(format!("Impact query: {}", summary.query)),
        Line::from(format!("Direct callers: {}", summary.direct_callers.len())),
    ];
    lines.extend(summary.direct_callers.iter().take(5).map(compact_call_line));
    lines.push(Line::from(format!(
        "Direct callees: {}",
        summary.direct_callees.len()
    )));
    lines.extend(summary.direct_callees.iter().take(5).map(compact_call_line));
    if summary.direct_callers.is_empty() && summary.direct_callees.is_empty() {
        lines.push(Line::from("No direct impact relationships matched."));
    }
    lines
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
        service_row("Qdrant", "local", app.qdrant_url.as_str()),
        service_row("Ollama", "local", app.ollama_url.as_str()),
        Row::new(vec![
            Cell::from("Embedding"),
            Cell::from(index_embedding_status_span(&app.status)),
            Cell::from(index_embedding(&app.status)),
        ]),
        Row::new(vec![
            Cell::from("Model"),
            Cell::from(status_span("cfg", StatusTone::Info)),
            Cell::from(app.embed_model.as_str()),
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
    let rows = summary.symbols.iter().take(12).map(|symbol| {
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
    let rows = summary.results.iter().take(12).map(|result| {
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
        summary.qdrant_collection
    )))
    .column_spacing(1)
}

fn call_graph_table(summary: &CallGraphSummary) -> Table<'_> {
    let rows = summary.rows.iter().take(12).map(call_row);
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
        .take(6)
        .map(|row| impact_row("caller", row));
    let callee_rows = summary
        .direct_callees
        .iter()
        .take(6)
        .map(|row| impact_row("callee", row));
    Table::new(
        caller_rows.chain(callee_rows),
        [
            Constraint::Length(8),
            Constraint::Percentage(28),
            Constraint::Length(8),
            Constraint::Percentage(24),
            Constraint::Length(12),
            Constraint::Percentage(18),
            Constraint::Length(16),
        ],
    )
    .header(table_header([
        "Edge",
        "Symbol",
        "Conf",
        "Path",
        "Lines",
        "Callee",
        "Resolution",
    ]))
    .block(Block::default().borders(Borders::ALL).title(format!(
        "Impact/Context Pack | Impact query: {} | Direct callers: {} | Direct callees: {}",
        summary.query,
        summary.direct_callers.len(),
        summary.direct_callees.len()
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
            .take(4)
            .map(|symbol| ("Symbol".to_owned(), symbol.qualified_name.clone())),
    );
    entries.extend(
        pack.files
            .iter()
            .take(4)
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
                .title("Impact/Context Pack | Context Pack Metadata"),
        )
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

fn impact_row<'a>(edge: &'static str, row: &'a symdex_store::CallSearchRow) -> Row<'a> {
    Row::new(vec![
        Cell::from(edge),
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

fn table_header<const N: usize>(labels: [&'static str; N]) -> Row<'static> {
    Row::new(
        labels
            .into_iter()
            .map(|label| Cell::from(label).style(Style::new().add_modifier(Modifier::BOLD))),
    )
    .style(Style::new().fg(Color::Cyan))
}

fn diagnostic_status(state: DiagnosticState) -> (&'static str, StatusTone) {
    match state {
        DiagnosticState::Ok => ("ok", StatusTone::Success),
        DiagnosticState::Missing => ("missing", StatusTone::Warning),
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
        QueryResult::Symbol(summary) => summary.symbols.len().min(12),
        QueryResult::Semantic(summary) => summary.results.len().min(12),
    }
}

fn impact_result_count(summary: &ImpactSummary) -> usize {
    summary.direct_callers.len().min(6) + summary.direct_callees.len().min(6)
}

fn context_pack_row_count(pack: &ContextPack) -> usize {
    7 + pack.focus_symbols.iter().take(4).count()
        + pack.files.iter().take(4).count()
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

fn line_range(start: usize, end: usize) -> String {
    format!("{start}-{end}")
}

fn optional_line_range(start: Option<usize>, end: Option<usize>) -> String {
    match (start, end) {
        (Some(start), Some(end)) => line_range(start, end),
        _ => "<unknown>".to_owned(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum View {
    Indexing,
    Diagnostics,
    Query,
    Graph,
    Evidence,
}

impl View {
    fn tabs() -> [&'static str; 5] {
        ["Index", "Doctor", "Query", "Calls", "Impact"]
    }

    fn tab_index(self) -> usize {
        match self {
            Self::Indexing => 0,
            Self::Diagnostics => 1,
            Self::Query => 2,
            Self::Graph => 3,
            Self::Evidence => 4,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Indexing => "Indexing",
            Self::Diagnostics => "Doctor Diagnostics",
            Self::Query => "Query Workbench",
            Self::Graph => "Symbol/Call Graph",
            Self::Evidence => "Impact/Context Pack",
        }
    }

    fn next(self) -> Self {
        match self {
            Self::Indexing => Self::Diagnostics,
            Self::Diagnostics => Self::Query,
            Self::Query => Self::Graph,
            Self::Graph => Self::Evidence,
            Self::Evidence => Self::Indexing,
        }
    }

    fn previous(self) -> Self {
        match self {
            Self::Indexing => Self::Evidence,
            Self::Diagnostics => Self::Indexing,
            Self::Query => Self::Diagnostics,
            Self::Graph => Self::Query,
            Self::Evidence => Self::Graph,
        }
    }

    fn footer_label(self) -> &'static str {
        match self {
            Self::Indexing => "index",
            Self::Diagnostics => "doctor",
            Self::Query => "query",
            Self::Graph => "calls",
            Self::Evidence => "impact",
        }
    }

    fn footer_help(self) -> &'static str {
        match self {
            Self::Indexing => {
                "Tab next view | o offline | s semantic | d doctor | w query | g calls | p impact | r refresh | q quit"
            }
            Self::Diagnostics => {
                "Tab next view | Up/Down select | Enter details | d rerun | i index | w query | g calls | p impact | q quit"
            }
            Self::Query => {
                "Tab next view | type query | Up/Down select | F2 mode | Enter run | Esc clear/back | q quit"
            }
            Self::Graph => {
                "Tab next view | type symbol | Up/Down select | F2 callers/callees | Enter run | Esc clear/back | q quit"
            }
            Self::Evidence => {
                "Tab next view | type symbol | Up/Down select | F2 impact/context | Enter run | Esc clear/back | q quit"
            }
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

enum DiagnosticsState {
    Idle,
    Running,
    Completed(DiagnosticReport),
    Failed(String),
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
            GraphStatus::Completed(summary) => summary.rows.len().min(12),
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
    ContextPack,
}

impl EvidenceMode {
    fn label(self) -> &'static str {
        match self {
            Self::Impact => "impact",
            Self::ContextPack => "context pack",
        }
    }

    fn toggled(self) -> Self {
        match self {
            Self::Impact => Self::ContextPack,
            Self::ContextPack => Self::Impact,
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
            EvidenceStatus::Completed(EvidenceResult::ContextPack(pack)) => {
                context_pack_row_count(pack)
            }
            _ => 0,
        }
    }
}

enum EvidenceStatus {
    Idle,
    Running,
    Completed(EvidenceResult),
    Failed(String),
}

enum EvidenceResult {
    Impact(ImpactSummary),
    ContextPack(ContextPack),
}

enum IndexJobMessage {
    Progress(IndexProgress),
    Finished(Result<IndexSummary, String>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IndexMode {
    Offline,
    Semantic,
}

impl IndexMode {
    fn label(self) -> &'static str {
        match self {
            Self::Offline => "offline",
            Self::Semantic => "semantic",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Screen {
    Dashboard,
    ConfirmIndex(IndexMode),
    IndexRunning(IndexMode),
    IndexCompleted(IndexMode),
    IndexFailed(IndexMode),
}

impl Screen {
    fn accepts_new_index_request(self) -> bool {
        matches!(
            self,
            Self::Dashboard | Self::IndexCompleted(_) | Self::IndexFailed(_)
        )
    }

    fn is_terminal_job_state(self) -> bool {
        matches!(self, Self::IndexCompleted(_) | Self::IndexFailed(_))
    }

    fn index_mode(self) -> Option<IndexMode> {
        match self {
            Self::ConfirmIndex(mode)
            | Self::IndexRunning(mode)
            | Self::IndexCompleted(mode)
            | Self::IndexFailed(mode) => Some(mode),
            Self::Dashboard => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UiAction {
    RequestIndex(IndexMode),
    Confirm,
    Cancel,
    JobSucceeded,
    JobFailed,
    Dismiss,
}

fn reduce_screen(screen: Screen, action: UiAction) -> Screen {
    match (screen, action) {
        (screen, UiAction::RequestIndex(mode)) if screen.accepts_new_index_request() => {
            Screen::ConfirmIndex(mode)
        }
        (Screen::ConfirmIndex(mode), UiAction::Confirm) => Screen::IndexRunning(mode),
        (Screen::ConfirmIndex(_), UiAction::Cancel) => Screen::Dashboard,
        (Screen::IndexRunning(mode), UiAction::JobSucceeded) => Screen::IndexCompleted(mode),
        (Screen::IndexRunning(mode), UiAction::JobFailed) => Screen::IndexFailed(mode),
        (screen, UiAction::Dismiss) if screen.is_terminal_job_state() => Screen::Dashboard,
        (screen, _) => screen,
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::KeyCode;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::layout::Position;
    use ratatui::style::Color;
    use symdex_diagnostics::{DiagnosticCheck, DiagnosticReport, DiagnosticState};
    use symdex_query::{
        CallDirection, CallGraphSummary, ImpactSummary, QueryMode, QueryResult, SymbolSearchSummary,
    };
    use symdex_store::{
        CallSearchRow, ContextPack, ContextPackLimits, RepositoryStatus, SymbolSearchRow,
    };

    use crate::{
        App, DiagnosticsState, EvidenceMode, EvidenceResult, EvidenceStatus, GraphStatus,
        IndexMode, QueryStatus, Screen, UiAction, View, progress_percent, reduce_screen, render,
    };

    #[test]
    fn renders_dashboard_status() {
        let status = RepositoryStatus {
            repository_id: "repo".to_owned(),
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
        assert!(rendered.contains("Indexing"));
        assert!(rendered.contains("nomic-embed-text"));
        assert_eq!(cell_fg_for_text(buffer, "ready", None), Some(Color::Green));
    }

    #[test]
    fn renders_major_view_tabs_with_active_style() {
        let app = App::from_status("/tmp/repo", "repo", sample_status());
        let backend = TestBackend::new(100, 24);
        let mut terminal = Terminal::new(backend).expect("terminal should build");

        render(&mut terminal, &app).expect("render should succeed");

        let buffer = terminal.backend().buffer();
        let rendered = format!("{buffer:?}");
        assert!(rendered.contains("Index"));
        assert!(rendered.contains("Doctor"));
        assert!(rendered.contains("Query"));
        assert!(rendered.contains("Calls"));
        assert!(rendered.contains("Impact"));
        assert_eq!(
            cell_fg_for_text(buffer, "Index", Some(1)),
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

        let rendered = format!("{:?}", terminal.backend().buffer());
        assert!(rendered.contains("query"));
        assert!(rendered.contains("Tab next view"));
        assert!(rendered.contains("F2 mode"));
        assert!(rendered.contains("Enter run"));
    }

    #[test]
    fn tab_switches_major_views_from_query_without_toggling_mode() {
        let mut app = App::from_status("/tmp/repo", "repo", sample_status());
        app.view = View::Query;
        app.query.mode = QueryMode::Symbol;

        assert!(!app.handle_key(KeyCode::Tab));

        assert_eq!(app.view, View::Graph);
        assert_eq!(app.query.mode, QueryMode::Symbol);
        assert_eq!(app.message, "Symbol/Call Graph view selected.");
    }

    #[test]
    fn shift_tab_switches_to_previous_major_view() {
        let mut app = App::from_status("/tmp/repo", "repo", sample_status());
        app.view = View::Query;

        assert!(!app.handle_key(KeyCode::BackTab));

        assert_eq!(app.view, View::Diagnostics);
        assert_eq!(app.message, "Doctor Diagnostics view selected.");
    }

    #[test]
    fn renders_doctor_footer_enter_details_help() {
        let mut app = App::from_status("/tmp/repo", "repo", sample_status());
        app.view = View::Diagnostics;
        let backend = TestBackend::new(120, 24);
        let mut terminal = Terminal::new(backend).expect("terminal should build");

        render(&mut terminal, &app).expect("render should succeed");

        let rendered = format!("{:?}", terminal.backend().buffer());
        assert!(rendered.contains("Enter details"));
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
        app.screen = Screen::ConfirmIndex(IndexMode::Semantic);
        let backend = TestBackend::new(100, 24);
        let mut terminal = Terminal::new(backend).expect("terminal should build");

        render(&mut terminal, &app).expect("render should succeed");

        let buffer = terminal.backend().buffer();
        let rendered = format!("{buffer:?}");
        assert!(rendered.contains("Confirm Indexing"));
        assert!(rendered.contains("Run semantic indexing"));
        assert!(rendered.contains("Press y to start"));
        assert_eq!(
            cell_fg_for_text(buffer, "Confirm Indexing", None),
            Some(Color::Yellow)
        );
    }

    #[test]
    fn renders_running_index_progress_gauge() {
        let mut app = App::from_status("/tmp/repo", "repo", sample_status());
        app.screen = Screen::IndexRunning(IndexMode::Semantic);
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
        assert_eq!(
            cell_fg_for_text(buffer, "Indexing Running", None),
            Some(Color::Cyan)
        );
    }

    #[test]
    fn progress_percent_clamps_to_complete() {
        let progress = symdex_index::IndexProgress {
            phase: "qdrant",
            completed: 6,
            total: 5,
            message: "Upserted vector points".to_owned(),
        };

        assert_eq!(progress_percent(Some(&progress)), 100);
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
        assert!(rendered.contains("qdrant_status"));
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
        assert!(rendered.contains("qdrant_status"));
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

        assert!(!app.handle_query_key(KeyCode::F(2)));
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

        assert!(!app.handle_graph_key(KeyCode::F(2)));
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
            direct_callers: vec![sample_call_row()],
            direct_callees: Vec::new(),
        }));
        let backend = TestBackend::new(180, 24);
        let mut terminal = Terminal::new(backend).expect("terminal should build");

        render(&mut terminal, &app).expect("render should succeed");

        let buffer = terminal.backend().buffer();
        let rendered = format!("{buffer:?}");
        assert!(rendered.contains("Impact/Context Pack"));
        assert!(rendered.contains("Impact query"));
        assert!(rendered.contains("Direct callers"));
        assert!(rendered.contains("Edge"));
        assert!(rendered.contains("Resolution"));
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
    fn evidence_viewer_accepts_input_and_toggles_modes() {
        let mut app = App::from_status("/tmp/repo", "repo", sample_status());
        app.view = View::Evidence;
        assert_eq!(app.evidence.mode, EvidenceMode::Impact);

        assert!(!app.handle_evidence_key(KeyCode::Char('a')));
        assert!(!app.handle_evidence_key(KeyCode::Char('d')));
        assert!(!app.handle_evidence_key(KeyCode::Char('d')));
        assert_eq!(app.evidence.input, "add");

        assert!(!app.handle_evidence_key(KeyCode::F(2)));
        assert_eq!(app.evidence.mode, EvidenceMode::ContextPack);

        assert!(!app.handle_evidence_key(KeyCode::Backspace));
        assert_eq!(app.evidence.input, "ad");
    }

    #[test]
    fn reducer_requires_confirmation_before_indexing() {
        let screen = reduce_screen(
            Screen::Dashboard,
            UiAction::RequestIndex(IndexMode::Offline),
        );
        assert_eq!(screen, Screen::ConfirmIndex(IndexMode::Offline));

        let screen = reduce_screen(screen, UiAction::Confirm);
        assert_eq!(screen, Screen::IndexRunning(IndexMode::Offline));
    }

    #[test]
    fn reducer_cancels_pending_index_without_running() {
        let screen = reduce_screen(
            Screen::Dashboard,
            UiAction::RequestIndex(IndexMode::Semantic),
        );
        let screen = reduce_screen(screen, UiAction::Cancel);

        assert_eq!(screen, Screen::Dashboard);
    }

    #[test]
    fn reducer_tracks_index_completion_and_dismissal() {
        let screen = reduce_screen(
            Screen::Dashboard,
            UiAction::RequestIndex(IndexMode::Offline),
        );
        let screen = reduce_screen(screen, UiAction::Confirm);
        let screen = reduce_screen(screen, UiAction::JobSucceeded);
        assert_eq!(screen, Screen::IndexCompleted(IndexMode::Offline));

        let screen = reduce_screen(screen, UiAction::Dismiss);
        assert_eq!(screen, Screen::Dashboard);
    }

    fn sample_status() -> RepositoryStatus {
        RepositoryStatus {
            repository_id: "repo".to_owned(),
            files_indexed: 2,
            chunks_indexed: 3,
            symbols_indexed: 4,
            calls_indexed: 5,
            last_indexed_at: Some("123".to_owned()),
            embedding_model: Some("nomic-embed-text".to_owned()),
            embedding_dimension: Some(768),
        }
    }

    fn sample_diagnostic_report() -> DiagnosticReport {
        DiagnosticReport {
            workspace: "/tmp/repo".to_owned(),
            sqlite_path: ".symdex/symdex.sqlite".to_owned(),
            qdrant_url: "http://localhost:6333".to_owned(),
            ollama_url: "http://localhost:11434".to_owned(),
            embed_model: "nomic-embed-text".to_owned(),
            checks: vec![
                DiagnosticCheck {
                    label: "sqlite_parent".to_owned(),
                    state: DiagnosticState::Ok,
                    message: ".symdex".to_owned(),
                },
                DiagnosticCheck {
                    label: "qdrant_status".to_owned(),
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

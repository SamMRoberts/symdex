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
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph, Wrap};
use symdex_core::RepoRoot;
use symdex_embed::EmbedConfig;
use symdex_index::{EmbeddingSummary, IndexOptions, IndexSummary, run_index};
use symdex_store::{RepositoryStatus, SqliteStore, StoreConfig};

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
    "USAGE:\n    symdex tui [repo]\n\nStarts the local terminal UI control panel.\n\nKEYS:\n    o         Confirm offline indexing\n    s         Confirm semantic indexing\n    r         Refresh repository status\n    y / n     Confirm or cancel a pending job\n    Enter     Dismiss a completed or failed job\n    q / Esc   Quit or cancel\n"
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
    screen: Screen,
    last_index_summary: Option<IndexSummary>,
    last_error: Option<String>,
    index_receiver: Option<Receiver<Result<IndexSummary, String>>>,
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
            screen: Screen::Dashboard,
            last_index_summary: None,
            last_error: None,
            index_receiver: None,
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
            screen: Screen::Dashboard,
            last_index_summary: None,
            last_error: None,
            index_receiver: None,
        }
    }

    fn lines(&self) -> Vec<Line<'_>> {
        vec![
            Line::from(vec![
                Span::styled("Repository: ", Style::new().add_modifier(Modifier::BOLD)),
                Span::raw(self.repo_root.as_str()),
            ]),
            Line::from(vec![
                Span::styled("Repository ID: ", Style::new().add_modifier(Modifier::BOLD)),
                Span::raw(self.repository_id.as_str()),
            ]),
            Line::from(""),
            Line::from(vec![
                Span::styled("SQLite: ", Style::new().add_modifier(Modifier::BOLD)),
                Span::raw(self.sqlite_path.as_str()),
            ]),
            Line::from(vec![
                Span::styled("Qdrant: ", Style::new().add_modifier(Modifier::BOLD)),
                Span::raw(self.qdrant_url.as_str()),
            ]),
            Line::from(vec![
                Span::styled("Ollama: ", Style::new().add_modifier(Modifier::BOLD)),
                Span::raw(self.ollama_url.as_str()),
            ]),
            Line::from(vec![
                Span::styled(
                    "Embedding model: ",
                    Style::new().add_modifier(Modifier::BOLD),
                ),
                Span::raw(self.embed_model.as_str()),
            ]),
            Line::from(""),
            Line::from(vec![
                Span::styled("Files indexed: ", Style::new().add_modifier(Modifier::BOLD)),
                Span::raw(self.status.files_indexed.to_string()),
            ]),
            Line::from(vec![
                Span::styled(
                    "Chunks indexed: ",
                    Style::new().add_modifier(Modifier::BOLD),
                ),
                Span::raw(self.status.chunks_indexed.to_string()),
            ]),
            Line::from(vec![
                Span::styled(
                    "Symbols indexed: ",
                    Style::new().add_modifier(Modifier::BOLD),
                ),
                Span::raw(self.status.symbols_indexed.to_string()),
            ]),
            Line::from(vec![
                Span::styled("Calls indexed: ", Style::new().add_modifier(Modifier::BOLD)),
                Span::raw(self.status.calls_indexed.to_string()),
            ]),
            Line::from(vec![
                Span::styled(
                    "Index embedding: ",
                    Style::new().add_modifier(Modifier::BOLD),
                ),
                Span::raw(index_embedding(&self.status)),
            ]),
            Line::from(vec![
                Span::styled("Last indexed: ", Style::new().add_modifier(Modifier::BOLD)),
                Span::raw(self.status.last_indexed_at.as_deref().unwrap_or("<never>")),
            ]),
        ]
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
                lines.push(Line::from("No indexing job is pending."));
            }
            Screen::ConfirmIndex(mode) => {
                lines.push(Line::from(vec![
                    Span::styled("Confirm: ", Style::new().add_modifier(Modifier::BOLD)),
                    Span::raw(format!(
                        "Run {} indexing for this repository?",
                        mode.label()
                    )),
                ]));
                lines.push(Line::from("Press y to start, n or Esc to cancel."));
            }
            Screen::IndexRunning(mode) => {
                lines.push(Line::from(vec![
                    Span::styled("Running: ", Style::new().add_modifier(Modifier::BOLD)),
                    Span::raw(format!("{} indexing", mode.label())),
                ]));
                lines.push(Line::from("The TUI will update when the job finishes."));
            }
            Screen::IndexCompleted(mode) => {
                lines.push(Line::from(vec![
                    Span::styled("Completed: ", Style::new().add_modifier(Modifier::BOLD)),
                    Span::raw(format!("{} indexing", mode.label())),
                ]));
                if let Some(summary) = &self.last_index_summary {
                    lines.extend(summary_lines(summary));
                }
            }
            Screen::IndexFailed(mode) => {
                lines.push(Line::from(vec![
                    Span::styled("Failed: ", Style::new().add_modifier(Modifier::BOLD)),
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

    fn handle_key(&mut self, code: KeyCode) -> bool {
        match code {
            KeyCode::Char('q') => return true,
            KeyCode::Esc if matches!(self.screen, Screen::ConfirmIndex(_)) => {
                self.screen = reduce_screen(self.screen, UiAction::Cancel);
                self.message = "Indexing cancelled before start.".to_owned();
            }
            KeyCode::Esc => return true,
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

    fn start_index_job(&mut self, mode: IndexMode) {
        let repo = self.repo_input.clone();
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let result = run_index(&IndexOptions {
                repo,
                offline: matches!(mode, IndexMode::Offline),
            });
            let _ = sender.send(result);
        });
        self.index_receiver = Some(receiver);
        self.last_index_summary = None;
        self.last_error = None;
        self.screen = reduce_screen(self.screen, UiAction::Confirm);
        self.message = format!("{} indexing started.", mode.label());
    }

    fn poll_index_job(&mut self) {
        let Some(receiver) = &self.index_receiver else {
            return;
        };
        match receiver.try_recv() {
            Ok(Ok(summary)) => {
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
                self.screen = reduce_screen(self.screen, UiAction::JobSucceeded);
                if let Err(error) = self.refresh_status() {
                    self.message =
                        format!("Indexing completed, but status refresh failed: {error}");
                } else {
                    self.message = completed_message;
                }
            }
            Ok(Err(error)) => {
                self.index_receiver = None;
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

            let title = Paragraph::new("symdex TUI")
                .block(Block::default().borders(Borders::ALL).title("Dashboard"));
            frame.render_widget(title, chunks[0]);

            let body_chunks = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(58), Constraint::Percentage(42)])
                .split(chunks[1]);

            let status = List::new(
                app.lines()
                    .into_iter()
                    .map(ListItem::new)
                    .collect::<Vec<_>>(),
            )
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Repository Status"),
            );
            frame.render_widget(status, body_chunks[0]);

            let indexing = List::new(
                app.index_lines()
                    .into_iter()
                    .map(ListItem::new)
                    .collect::<Vec<_>>(),
            )
            .block(Block::default().borders(Borders::ALL).title("Indexing"));
            frame.render_widget(indexing, body_chunks[1]);

            let footer = Paragraph::new(app.message.as_str())
                .wrap(Wrap { trim: true })
                .block(Block::default().borders(Borders::ALL).title("Status"));
            frame.render_widget(footer, chunks[2]);
        })
        .map(|_| ())
        .map_err(|error| error.to_string())
}

fn run_app<B: Backend>(terminal: &mut Terminal<B>, app: App) -> Result<(), String> {
    let mut app = app;
    loop {
        app.poll_index_job();
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
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use symdex_store::RepositoryStatus;

    use crate::{App, IndexMode, Screen, UiAction, reduce_screen, render};

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
        assert!(rendered.contains("Files indexed"));
        assert!(rendered.contains("Indexing"));
        assert!(rendered.contains("nomic-embed-text"));
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
}

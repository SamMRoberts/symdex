//! Terminal UI state, rendering, events, and terminal lifecycle.

use std::io::{self, Stdout};
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
    "USAGE:\n    symdex tui [repo]\n\nStarts the local terminal UI control panel.\n\nKEYS:\n    q / Esc    Quit\n"
}

#[derive(Debug, Clone)]
pub struct App {
    repo_root: String,
    repository_id: String,
    sqlite_path: String,
    qdrant_url: String,
    ollama_url: String,
    embed_model: String,
    status: RepositoryStatus,
    message: String,
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
            repo_root: root.path().display().to_string(),
            repository_id: root.id().to_owned(),
            sqlite_path: store_config.sqlite_path.display().to_string(),
            qdrant_url: store_config.qdrant_url,
            ollama_url: embed_config.ollama_url,
            embed_model: embed_config.model,
            status,
            message: "Dashboard loaded. Press q or Esc to quit.".to_owned(),
        })
    }

    pub fn from_status(
        repo_root: impl Into<String>,
        repository_id: impl Into<String>,
        status: RepositoryStatus,
    ) -> Self {
        Self {
            repo_root: repo_root.into(),
            repository_id: repository_id.into(),
            sqlite_path: ".symdex/symdex.sqlite".to_owned(),
            qdrant_url: "http://localhost:6333".to_owned(),
            ollama_url: "http://localhost:11434".to_owned(),
            embed_model: "nomic-embed-text".to_owned(),
            status,
            message: "Dashboard loaded. Press q or Esc to quit.".to_owned(),
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

            let body = List::new(
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
            frame.render_widget(body, chunks[1]);

            let footer = Paragraph::new(app.message.as_str())
                .wrap(Wrap { trim: true })
                .block(Block::default().borders(Borders::ALL).title("Status"));
            frame.render_widget(footer, chunks[2]);
        })
        .map(|_| ())
        .map_err(|error| error.to_string())
}

fn run_app<B: Backend>(terminal: &mut Terminal<B>, app: App) -> Result<(), String> {
    loop {
        render(terminal, &app)?;
        if !event::poll(Duration::from_millis(250)).map_err(|error| error.to_string())? {
            continue;
        }
        let Event::Key(key) = event::read().map_err(|error| error.to_string())? else {
            continue;
        };
        if key.kind == KeyEventKind::Press && matches!(key.code, KeyCode::Char('q') | KeyCode::Esc)
        {
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

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use symdex_store::RepositoryStatus;

    use crate::{App, render};

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
        assert!(rendered.contains("nomic-embed-text"));
    }
}

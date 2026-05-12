use std::{path::PathBuf, time::Duration};

use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{Terminal, backend::CrosstermBackend};

use crate::{
    config::{AppConfig, discover_repo_root},
    db,
};

pub mod app;
pub mod views;

use app::View;

pub fn run(path: PathBuf) -> Result<()> {
    let root = discover_repo_root(&path)?;
    let config = AppConfig::load(&root)?;
    let conn = db::open_database(&root, &config)?;
    let status = db::status(&conn, &root, &config.database_path(&root), &config)?;
    let app = app::App::new(status);

    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    let result = run_loop(&mut terminal, app);
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    result
}

fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    mut app: app::App,
) -> Result<()> {
    loop {
        terminal.draw(|frame| views::draw(frame, &app))?;
        if event::poll(Duration::from_millis(250))?
            && let Event::Key(key) = event::read()?
        {
            match key.code {
                KeyCode::Char('q') => break,
                KeyCode::Char('?') => app.show_help(),
                KeyCode::Tab | KeyCode::Down => app.next_view(),
                KeyCode::BackTab | KeyCode::Up => app.previous_view(),
                KeyCode::Char('/') => {
                    app.select_view(View::Search);
                    app.message = "Search is available from the CLI with `symdex symbols find <name>` in the MVP".into();
                }
                KeyCode::Char('r') => {
                    app.message = "Run `symdex index .` to re-index from the MVP TUI".into()
                }
                KeyCode::Char('f') => app.select_view(View::Files),
                KeyCode::Char('s') => app.select_view(View::Symbols),
                KeyCode::Char('d') => app.select_view(View::SymbolDetail),
                KeyCode::Char('v') => app.select_view(View::References),
                KeyCode::Char('c') => app.select_view(View::CallersCallees),
                KeyCode::Char('i') => app.select_view(View::Imports),
                KeyCode::Char('e') => app.select_view(View::ParseErrors),
                KeyCode::Esc => app.select_view(View::Dashboard),
                KeyCode::Enter => app.message = format!("Opened {}", app.selected_view.label()),
                _ => {}
            }
        }
    }
    Ok(())
}

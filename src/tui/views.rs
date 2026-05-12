use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph, Wrap},
};

use crate::tui::app::App;
use crate::tui::app::View;

pub fn draw(frame: &mut Frame<'_>, app: &App) {
    let root = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(8), Constraint::Length(3)])
        .split(frame.area());
    let panes = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(24),
            Constraint::Percentage(46),
            Constraint::Percentage(34),
        ])
        .split(root[0]);

    let nav = List::new(
        View::ALL
            .into_iter()
            .map(|view| {
                let style = if view == app.selected_view {
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                };
                ListItem::new(Line::from(Span::styled(view.label(), style)))
            })
            .collect::<Vec<_>>(),
    )
    .block(Block::default().title("Views").borders(Borders::ALL));
    frame.render_widget(nav, panes[0]);

    let status = &app.status;
    let center = Paragraph::new(vec![
        Line::from(format!("Repo path: {}", status.repo_path)),
        Line::from(format!("Database path: {}", status.database_path)),
        Line::from(format!(
            "Last index time: {}",
            status
                .last_index_time
                .clone()
                .unwrap_or_else(|| "never".into())
        )),
        Line::from(format!("Files indexed: {}", status.files_indexed)),
        Line::from(format!("Symbols indexed: {}", status.symbols_indexed)),
        Line::from(format!(
            "Relationships indexed: {}",
            status.relationships_indexed
        )),
        Line::from(format!("Parse errors: {}", status.parse_errors)),
        Line::from(format!(
            "Languages: {}",
            status.supported_languages.join(", ")
        )),
    ])
    .block(
        Block::default()
            .title(app.selected_view.label())
            .borders(Borders::ALL),
    )
    .wrap(Wrap { trim: true });
    frame.render_widget(center, panes[1]);

    let detail = Paragraph::new(detail_lines(app))
        .block(Block::default().title("Details").borders(Borders::ALL))
        .wrap(Wrap { trim: true });
    frame.render_widget(detail, panes[2]);

    let bottom = Paragraph::new(app.message.as_str())
        .block(Block::default().title("Status").borders(Borders::ALL));
    frame.render_widget(bottom, root[1]);
}

fn detail_lines(app: &App) -> Vec<Line<'static>> {
    match app.selected_view {
        View::Dashboard => vec![
            Line::from("Local index dashboard"),
            Line::from("Shows repository, database, status counts, and supported languages."),
            Line::from("Use tab/down and shift-tab/up to move between views."),
        ],
        View::Files => vec![
            Line::from("Files view"),
            Line::from("Lists active indexed files and highlights files with parse errors."),
            Line::from("CLI equivalent: symdex files with-errors"),
        ],
        View::Symbols => vec![
            Line::from("Symbols view"),
            Line::from("Surfaces symbol name, kind, language, file path, and line range."),
            Line::from("CLI equivalent: symdex symbols find <name>"),
        ],
        View::SymbolDetail => vec![
            Line::from("Symbol detail view"),
            Line::from(
                app.selected_symbol_detail
                    .clone()
                    .unwrap_or_else(|| "No symbol selected in the dashboard MVP.".into()),
            ),
            Line::from("Use CLI output for full source-backed evidence."),
        ],
        View::References => vec![
            Line::from("References view"),
            Line::from("Shows known reference locations and reference kind."),
            Line::from("CLI equivalent: symdex refs <symbol-name>"),
        ],
        View::CallersCallees => vec![
            Line::from("Callers/Callees view"),
            Line::from("Shows call relationships with confidence and evidence."),
            Line::from("CLI equivalents: symdex callers <name>, symdex callees <name>"),
        ],
        View::Imports => vec![
            Line::from("Imports view"),
            Line::from("Shows file-level import text and source locations."),
            Line::from("CLI equivalent: symdex imports <file>"),
        ],
        View::ParseErrors => vec![
            Line::from("Parse errors view"),
            Line::from("Shows syntax or parser failures without stopping the index."),
            Line::from("CLI equivalent: symdex errors"),
        ],
        View::Search => vec![
            Line::from("Search view"),
            Line::from(if app.search_text.is_empty() {
                "No search text entered in the dashboard MVP.".into()
            } else {
                format!("Search text: {}", app.search_text)
            }),
            Line::from("CLI equivalent: symdex symbols find <name>"),
        ],
    }
}

use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph, Wrap},
};

use crate::tui::app::App;

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

    let views = [
        "Dashboard",
        "Files",
        "Symbols",
        "Symbol detail",
        "References",
        "Callers/Callees",
        "Imports",
        "Parse errors",
        "Search",
    ];
    let nav = List::new(
        views
            .into_iter()
            .map(|view| {
                let style = if view == app.selected_view {
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                };
                ListItem::new(Line::from(Span::styled(view, style)))
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
            .title(app.selected_view.as_str())
            .borders(Borders::ALL),
    )
    .wrap(Wrap { trim: true });
    frame.render_widget(center, panes[1]);

    let detail = Paragraph::new(vec![
        Line::from("MVP dashboard"),
        Line::from("Use CLI commands for detailed lists:"),
        Line::from("symdex symbols find <name>"),
        Line::from("symdex refs <name>"),
        Line::from("symdex imports <file>"),
        Line::from("symdex errors"),
    ])
    .block(Block::default().title("Details").borders(Borders::ALL))
    .wrap(Wrap { trim: true });
    frame.render_widget(detail, panes[2]);

    let bottom = Paragraph::new(app.message.as_str())
        .block(Block::default().title("Status").borders(Borders::ALL));
    frame.render_widget(bottom, root[1]);
}

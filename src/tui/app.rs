use crate::db::schema::IndexStatus;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    Dashboard,
    Files,
    Symbols,
    SymbolDetail,
    References,
    CallersCallees,
    Imports,
    ParseErrors,
    Search,
}

impl View {
    pub const ALL: [Self; 9] = [
        Self::Dashboard,
        Self::Files,
        Self::Symbols,
        Self::SymbolDetail,
        Self::References,
        Self::CallersCallees,
        Self::Imports,
        Self::ParseErrors,
        Self::Search,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Dashboard => "Dashboard",
            Self::Files => "Files",
            Self::Symbols => "Symbols",
            Self::SymbolDetail => "Symbol detail",
            Self::References => "References",
            Self::CallersCallees => "Callers/Callees",
            Self::Imports => "Imports",
            Self::ParseErrors => "Parse errors",
            Self::Search => "Search",
        }
    }
}

#[derive(Debug, Clone)]
pub struct App {
    pub status: IndexStatus,
    pub selected_view: View,
    pub search_text: String,
    pub selected_symbol_detail: Option<String>,
    pub message: String,
}

impl App {
    pub fn new(status: IndexStatus) -> Self {
        Self {
            status,
            selected_view: View::Dashboard,
            search_text: String::new(),
            selected_symbol_detail: None,
            message: "Press ? for help, q to quit".into(),
        }
    }

    pub fn select_view(&mut self, view: View) {
        self.selected_view = view;
    }

    pub fn next_view(&mut self) {
        let current = View::ALL
            .iter()
            .position(|view| *view == self.selected_view)
            .unwrap_or(0);
        self.selected_view = View::ALL[(current + 1) % View::ALL.len()];
    }

    pub fn previous_view(&mut self) {
        let current = View::ALL
            .iter()
            .position(|view| *view == self.selected_view)
            .unwrap_or(0);
        let previous = if current == 0 {
            View::ALL.len() - 1
        } else {
            current - 1
        };
        self.selected_view = View::ALL[previous];
    }

    pub fn show_help(&mut self) {
        self.message = "q quit | tab/down next | shift-tab/up previous | / search | f files | s symbols | d detail | v refs | c callers/callees | i imports | e errors".into();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_on_dashboard_with_status_message() {
        let app = App::new(status());

        assert_eq!(app.selected_view, View::Dashboard);
        assert!(app.message.contains("Press ?"));
        assert_eq!(View::ALL.len(), 9);
    }

    #[test]
    fn cycles_views_and_updates_help_message() {
        let mut app = App::new(status());

        app.next_view();
        assert_eq!(app.selected_view, View::Files);
        app.previous_view();
        assert_eq!(app.selected_view, View::Dashboard);
        app.show_help();
        assert!(app.message.contains("callers/callees"));
    }

    fn status() -> IndexStatus {
        IndexStatus {
            repo_path: "/repo".into(),
            database_path: "/repo/.symdex/index.db".into(),
            last_index_time: None,
            files_indexed: 0,
            symbols_indexed: 0,
            relationships_indexed: 0,
            parse_errors: 0,
            supported_languages: vec!["rust".into()],
        }
    }
}

use std::io::{self, Stdout};

use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

pub type TuiTerminal = Terminal<CrosstermBackend<Stdout>>;

pub fn help_text() -> &'static str {
    "USAGE:\n    symdex tui [repo]\n\nStarts the local terminal UI control panel.\n\nKEYS:\n    [         Switch to the previous primary tab\n    ]         Switch to the next primary tab\n    Tab       Switch to the next mode in the active view, or full/incremental scope on Indexing\n    Shift+Tab Switch to the previous mode in the active view, or full/incremental scope on Indexing\n    Up/Down   Move selected result row\n    Enter     Run lookup, run Doctor, toggle Doctor details, or dismiss a completed job\n    o         Confirm offline indexing with the selected scope\n    s         Confirm semantic indexing with the selected scope\n    c         Toggle continuous indexing\n    r         Refresh repository and storage status\n    y / n     Confirm or cancel a pending job\n    q / Esc   Quit or cancel\n"
}

pub fn enter_terminal() -> Result<TuiTerminal, String> {
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

pub fn leave_terminal(terminal: &mut TuiTerminal) -> Result<(), String> {
    disable_raw_mode().map_err(|error| error.to_string())?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen).map_err(|error| error.to_string())?;
    terminal.show_cursor().map_err(|error| error.to_string())
}

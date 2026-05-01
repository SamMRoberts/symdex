#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IndexMode {
    Offline,
    Semantic,
}

impl IndexMode {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Offline => "offline",
            Self::Semantic => "semantic",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Screen {
    Dashboard,
    ConfirmIndex(IndexMode),
    ConfirmContinuous,
    IndexRunning(IndexMode),
    IndexCompleted(IndexMode),
    IndexFailed(IndexMode),
}

impl Screen {
    pub(crate) fn accepts_new_index_request(self) -> bool {
        matches!(
            self,
            Self::Dashboard | Self::IndexCompleted(_) | Self::IndexFailed(_)
        )
    }

    pub(crate) fn is_terminal_job_state(self) -> bool {
        matches!(self, Self::IndexCompleted(_) | Self::IndexFailed(_))
    }

    pub(crate) fn index_mode(self) -> Option<IndexMode> {
        match self {
            Self::ConfirmIndex(mode)
            | Self::IndexRunning(mode)
            | Self::IndexCompleted(mode)
            | Self::IndexFailed(mode) => Some(mode),
            Self::Dashboard | Self::ConfirmContinuous => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UiAction {
    RequestIndex(IndexMode),
    RequestContinuous,
    Confirm,
    Cancel,
    JobSucceeded,
    JobFailed,
    Dismiss,
}

pub(crate) fn reduce_screen(screen: Screen, action: UiAction) -> Screen {
    match (screen, action) {
        (screen, UiAction::RequestIndex(mode)) if screen.accepts_new_index_request() => {
            Screen::ConfirmIndex(mode)
        }
        (screen, UiAction::RequestContinuous) if screen.accepts_new_index_request() => {
            Screen::ConfirmContinuous
        }
        (Screen::ConfirmIndex(mode), UiAction::Confirm) => Screen::IndexRunning(mode),
        (Screen::ConfirmContinuous, UiAction::Confirm) => Screen::Dashboard,
        (Screen::ConfirmIndex(_) | Screen::ConfirmContinuous, UiAction::Cancel) => {
            Screen::Dashboard
        }
        (Screen::IndexRunning(mode), UiAction::JobSucceeded) => Screen::IndexCompleted(mode),
        (Screen::IndexRunning(mode), UiAction::JobFailed) => Screen::IndexFailed(mode),
        (screen, UiAction::Dismiss) if screen.is_terminal_job_state() => Screen::Dashboard,
        (screen, _) => screen,
    }
}

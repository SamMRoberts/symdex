use symdex_index::IndexScope;

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
pub(crate) struct ManualIndexRequest {
    pub(crate) mode: IndexMode,
    pub(crate) scope: IndexScope,
}

impl ManualIndexRequest {
    pub(crate) fn label(self) -> String {
        format!("{} {}", self.mode.label(), self.scope.label())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Screen {
    Dashboard,
    ConfirmIndex(ManualIndexRequest),
    ConfirmContinuous,
    IndexRunning(ManualIndexRequest),
    IndexCompleted(ManualIndexRequest),
    IndexFailed(ManualIndexRequest),
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

    pub(crate) fn index_request(self) -> Option<ManualIndexRequest> {
        match self {
            Self::ConfirmIndex(request)
            | Self::IndexRunning(request)
            | Self::IndexCompleted(request)
            | Self::IndexFailed(request) => Some(request),
            Self::Dashboard | Self::ConfirmContinuous => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UiAction {
    RequestIndex(ManualIndexRequest),
    RequestContinuous,
    Confirm,
    Cancel,
    JobSucceeded,
    JobFailed,
    Dismiss,
}

pub(crate) fn reduce_screen(screen: Screen, action: UiAction) -> Screen {
    match (screen, action) {
        (screen, UiAction::RequestIndex(request)) if screen.accepts_new_index_request() => {
            Screen::ConfirmIndex(request)
        }
        (screen, UiAction::RequestContinuous) if screen.accepts_new_index_request() => {
            Screen::ConfirmContinuous
        }
        (Screen::ConfirmIndex(request), UiAction::Confirm) => Screen::IndexRunning(request),
        (Screen::ConfirmContinuous, UiAction::Confirm) => Screen::Dashboard,
        (Screen::ConfirmIndex(_) | Screen::ConfirmContinuous, UiAction::Cancel) => {
            Screen::Dashboard
        }
        (Screen::IndexRunning(request), UiAction::JobSucceeded) => Screen::IndexCompleted(request),
        (Screen::IndexRunning(request), UiAction::JobFailed) => Screen::IndexFailed(request),
        (screen, UiAction::Dismiss) if screen.is_terminal_job_state() => Screen::Dashboard,
        (screen, _) => screen,
    }
}

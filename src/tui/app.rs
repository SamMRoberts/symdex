use crate::db::schema::IndexStatus;

#[derive(Debug, Clone)]
pub struct App {
    pub status: IndexStatus,
    pub selected_view: String,
    pub message: String,
}

impl App {
    pub fn new(status: IndexStatus) -> Self {
        Self {
            status,
            selected_view: "Dashboard".into(),
            message: "Press ? for help, q to quit".into(),
        }
    }
}

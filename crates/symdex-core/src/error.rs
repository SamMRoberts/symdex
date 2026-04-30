use std::fmt::{Display, Formatter};
use std::path::PathBuf;

pub type Result<T> = std::result::Result<T, CoreError>;

#[derive(Debug)]
pub enum CoreError {
    Io {
        action: &'static str,
        path: PathBuf,
        source: std::io::Error,
    },
    EmptyPath,
    PathOutsideRepo {
        root: PathBuf,
        path: PathBuf,
    },
    NonUtf8Path {
        path: PathBuf,
    },
}

impl CoreError {
    pub(crate) fn io(
        action: &'static str,
        path: impl Into<PathBuf>,
        source: std::io::Error,
    ) -> Self {
        Self::Io {
            action,
            path: path.into(),
            source,
        }
    }
}

impl Display for CoreError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io {
                action,
                path,
                source,
            } => write!(f, "failed to {action} {}: {source}", path.display()),
            Self::EmptyPath => write!(f, "path must not be empty"),
            Self::PathOutsideRepo { root, path } => write!(
                f,
                "path {} is outside repository root {}",
                path.display(),
                root.display()
            ),
            Self::NonUtf8Path { path } => write!(f, "path is not valid UTF-8: {}", path.display()),
        }
    }
}

impl std::error::Error for CoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::EmptyPath | Self::PathOutsideRepo { .. } | Self::NonUtf8Path { .. } => None,
        }
    }
}

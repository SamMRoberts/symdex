use std::path::{Component, Path, PathBuf};

use crate::{CoreError, Result, stable_id};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoRoot {
    canonical: PathBuf,
    id: String,
}

impl RepoRoot {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if path.as_os_str().is_empty() {
            return Err(CoreError::EmptyPath);
        }

        let canonical = path
            .canonicalize()
            .map_err(|source| CoreError::io("canonicalize repository root", path, source))?;
        let root_text = canonical.to_string_lossy();
        let id = stable_id(&["repository", &root_text]);
        Ok(Self { canonical, id })
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn path(&self) -> &Path {
        &self.canonical
    }

    pub fn normalize_existing_path(&self, path: impl AsRef<Path>) -> Result<NormalizedRepoPath> {
        let path = path.as_ref();
        let canonical = path
            .canonicalize()
            .map_err(|source| CoreError::io("canonicalize repository path", path, source))?;

        if !canonical.starts_with(&self.canonical) {
            return Err(CoreError::PathOutsideRepo {
                root: self.canonical.clone(),
                path: canonical,
            });
        }

        let relative =
            canonical
                .strip_prefix(&self.canonical)
                .map_err(|_| CoreError::PathOutsideRepo {
                    root: self.canonical.clone(),
                    path: canonical.clone(),
                })?;

        NormalizedRepoPath::from_relative_path(relative)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct NormalizedRepoPath(String);

impl NormalizedRepoPath {
    pub fn new(path: impl AsRef<str>) -> Result<Self> {
        let path = path.as_ref();
        if path.is_empty() {
            return Err(CoreError::EmptyPath);
        }
        let normalized = path.replace('\\', "/");
        if normalized.starts_with('/') {
            return Err(CoreError::PathOutsideRepo {
                root: PathBuf::new(),
                path: PathBuf::from(normalized),
            });
        }
        if normalized
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
        {
            return Err(CoreError::PathOutsideRepo {
                root: PathBuf::new(),
                path: PathBuf::from(normalized),
            });
        }
        Ok(Self(normalized))
    }

    pub(crate) fn from_relative_path(path: &Path) -> Result<Self> {
        let mut parts = Vec::new();
        for component in path.components() {
            match component {
                Component::Normal(part) => {
                    let part = part.to_str().ok_or_else(|| CoreError::NonUtf8Path {
                        path: path.to_path_buf(),
                    })?;
                    parts.push(part);
                }
                Component::CurDir => {}
                Component::Prefix(_) | Component::RootDir | Component::ParentDir => {
                    return Err(CoreError::PathOutsideRepo {
                        root: PathBuf::new(),
                        path: path.to_path_buf(),
                    });
                }
            }
        }
        Self::new(parts.join("/"))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::NormalizedRepoPath;

    #[test]
    fn normalized_paths_use_forward_slashes() {
        let path = NormalizedRepoPath::new("src\\main.rs").expect("path should normalize");
        assert_eq!(path.as_str(), "src/main.rs");
    }

    #[test]
    fn normalized_paths_reject_parent_segments() {
        assert!(NormalizedRepoPath::new("../secret.rs").is_err());
        assert!(NormalizedRepoPath::new("src/../secret.rs").is_err());
        assert!(NormalizedRepoPath::new("/abs/path.rs").is_err());
    }
}

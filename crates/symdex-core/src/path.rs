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
        if !canonical.is_dir() {
            return Err(CoreError::RepositoryRootNotDirectory { path: canonical });
        }
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
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    use crate::CoreError;

    use super::{NormalizedRepoPath, RepoRoot};

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

    #[test]
    fn repo_root_rejects_file_paths() {
        let path = temp_path("file-root");
        fs::write(&path, "not a directory").expect("test file should be written");

        let error = RepoRoot::open(&path).expect_err("file roots should be rejected");

        assert!(matches!(
            error,
            CoreError::RepositoryRootNotDirectory { .. }
        ));
        let _ = fs::remove_file(path);
    }

    #[cfg(unix)]
    #[test]
    fn normalize_existing_path_rejects_symlink_escape() {
        let root_path = temp_path("repo-root");
        let outside_path = temp_path("outside-root");
        fs::create_dir_all(root_path.join("src")).expect("repo should be created");
        fs::create_dir_all(&outside_path).expect("outside dir should be created");
        fs::write(outside_path.join("secret.rs"), "pub fn secret() {}\n")
            .expect("outside file should be written");
        std::os::unix::fs::symlink(
            outside_path.join("secret.rs"),
            root_path.join("src/link.rs"),
        )
        .expect("symlink should be created");
        let root = RepoRoot::open(&root_path).expect("repo root should open");

        let error = root
            .normalize_existing_path(root_path.join("src/link.rs"))
            .expect_err("symlink escapes should be rejected");

        assert!(matches!(error, CoreError::PathOutsideRepo { .. }));
        let _ = fs::remove_dir_all(root_path);
        let _ = fs::remove_dir_all(outside_path);
    }

    fn temp_path(name: &str) -> std::path::PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should be after epoch")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "symdex-path-test-{name}-{}-{nonce}",
            std::process::id()
        ))
    }
}

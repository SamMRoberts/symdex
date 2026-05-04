use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use crate::{CoreError, RepoRoot, Result, stable_id};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepositoryRefKind {
    Branch,
    Detached,
    Other,
    NonGit,
}

impl RepositoryRefKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Branch => "branch",
            Self::Detached => "detached",
            Self::Other => "other",
            Self::NonGit => "non_git",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepositoryRefSnapshot {
    pub id: String,
    pub repository_id: String,
    pub kind: RepositoryRefKind,
    pub name: Option<String>,
    pub head_oid: Option<String>,
    pub local_branches: Vec<String>,
}

impl RepositoryRefSnapshot {
    pub fn detect(root: &RepoRoot) -> Result<Self> {
        let Some(git_dir) = git_dir(root.path())? else {
            return Ok(non_git_snapshot(root));
        };
        let head = read_optional_to_string(&git_dir.join("HEAD"))?;
        let Some(head) = head.map(|value| value.trim().to_owned()) else {
            return Ok(non_git_snapshot(root));
        };
        let local_branches = local_branches(&git_dir)?;

        if let Some(reference) = head.strip_prefix("ref: ") {
            let (kind, name) = classify_reference(reference);
            let head_oid = read_ref_oid(&git_dir, reference)?;
            let id = ref_id(root.id(), kind, name.as_deref().unwrap_or(reference));
            return Ok(Self {
                id,
                repository_id: root.id().to_owned(),
                kind,
                name: Some(name.unwrap_or_else(|| reference.to_owned())),
                head_oid,
                local_branches,
            });
        }

        if is_hex_oid(&head) {
            let id = ref_id(root.id(), RepositoryRefKind::Detached, &head);
            return Ok(Self {
                id,
                repository_id: root.id().to_owned(),
                kind: RepositoryRefKind::Detached,
                name: None,
                head_oid: Some(head),
                local_branches,
            });
        }

        Ok(Self {
            id: ref_id(root.id(), RepositoryRefKind::Other, &head),
            repository_id: root.id().to_owned(),
            kind: RepositoryRefKind::Other,
            name: Some(head),
            head_oid: None,
            local_branches,
        })
    }
}

fn non_git_snapshot(root: &RepoRoot) -> RepositoryRefSnapshot {
    RepositoryRefSnapshot {
        id: ref_id(root.id(), RepositoryRefKind::NonGit, "working-tree"),
        repository_id: root.id().to_owned(),
        kind: RepositoryRefKind::NonGit,
        name: Some("working-tree".to_owned()),
        head_oid: None,
        local_branches: Vec::new(),
    }
}

fn ref_id(repository_id: &str, kind: RepositoryRefKind, identity: &str) -> String {
    stable_id(&["repository-ref", repository_id, kind.as_str(), identity])
}

fn git_dir(root: &Path) -> Result<Option<PathBuf>> {
    let dot_git = root.join(".git");
    let metadata = match fs::symlink_metadata(&dot_git) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(CoreError::io("read .git metadata", dot_git, source)),
    };
    if metadata.is_dir() {
        return Ok(Some(dot_git));
    }
    if !metadata.is_file() {
        return Ok(None);
    }
    let contents = fs::read_to_string(&dot_git)
        .map_err(|source| CoreError::io("read .git file", &dot_git, source))?;
    let Some(gitdir) = contents.trim().strip_prefix("gitdir: ") else {
        return Ok(None);
    };
    let path = PathBuf::from(gitdir);
    if path.is_absolute() {
        Ok(Some(path))
    } else {
        Ok(Some(root.join(path)))
    }
}

fn classify_reference(reference: &str) -> (RepositoryRefKind, Option<String>) {
    if let Some(branch) = reference.strip_prefix("refs/heads/") {
        (RepositoryRefKind::Branch, Some(branch.to_owned()))
    } else {
        (RepositoryRefKind::Other, Some(reference.to_owned()))
    }
}

fn read_ref_oid(git_dir: &Path, reference: &str) -> Result<Option<String>> {
    let loose = read_optional_to_string(&git_dir.join(reference))?;
    if let Some(value) = loose.map(|value| value.trim().to_owned())
        && is_hex_oid(&value)
    {
        return Ok(Some(value));
    }
    packed_ref_oid(git_dir, reference)
}

fn packed_ref_oid(git_dir: &Path, reference: &str) -> Result<Option<String>> {
    let Some(contents) = read_optional_to_string(&git_dir.join("packed-refs"))? else {
        return Ok(None);
    };
    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with('^') {
            continue;
        }
        let mut parts = line.split_whitespace();
        let Some(oid) = parts.next() else {
            continue;
        };
        if parts.next() == Some(reference) && is_hex_oid(oid) {
            return Ok(Some(oid.to_owned()));
        }
    }
    Ok(None)
}

fn local_branches(git_dir: &Path) -> Result<Vec<String>> {
    let mut branches = BTreeSet::new();
    let refs_heads = git_dir.join("refs").join("heads");
    collect_loose_branches(&refs_heads, &refs_heads, &mut branches)?;
    if let Some(contents) = read_optional_to_string(&git_dir.join("packed-refs"))? {
        for line in contents.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with('^') {
                continue;
            }
            let mut parts = line.split_whitespace();
            let _oid = parts.next();
            if let Some(reference) = parts
                .next()
                .and_then(|part| part.strip_prefix("refs/heads/"))
            {
                branches.insert(reference.to_owned());
            }
        }
    }
    Ok(branches.into_iter().collect())
}

fn collect_loose_branches(root: &Path, dir: &Path, branches: &mut BTreeSet<String>) -> Result<()> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(source) => return Err(CoreError::io("read git branch refs", dir, source)),
    };
    for entry in entries {
        let entry = entry.map_err(|source| CoreError::io("read git branch ref", dir, source))?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|source| CoreError::io("read git branch ref type", &path, source))?;
        if file_type.is_dir() {
            collect_loose_branches(root, &path, branches)?;
        } else if file_type.is_file() {
            let relative = path
                .strip_prefix(root)
                .map_err(|_| CoreError::PathOutsideRepo {
                    root: root.to_path_buf(),
                    path: path.clone(),
                })?;
            if let Some(name) = relative.to_str() {
                branches.insert(name.replace('\\', "/"));
            }
        }
    }
    Ok(())
}

fn read_optional_to_string(path: &Path) -> Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(value) => Ok(Some(value)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(CoreError::io("read git metadata", path, source)),
    }
}

fn is_hex_oid(value: &str) -> bool {
    value.len() >= 40 && value.chars().all(|character| character.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::{RepositoryRefKind, RepositoryRefSnapshot};
    use crate::RepoRoot;

    #[test]
    fn detects_non_git_repository() {
        let root_path = temp_dir("non_git");
        fs::create_dir_all(&root_path).expect("temp repo should be created");
        let root = RepoRoot::open(&root_path).expect("repo root should open");

        let snapshot = RepositoryRefSnapshot::detect(&root).expect("snapshot should resolve");

        assert_eq!(snapshot.kind, RepositoryRefKind::NonGit);
        assert_eq!(snapshot.name.as_deref(), Some("working-tree"));
        assert!(snapshot.local_branches.is_empty());
    }

    #[test]
    fn detects_attached_local_branch() {
        let root_path = temp_dir("branch");
        let git_dir = root_path.join(".git");
        fs::create_dir_all(git_dir.join("refs/heads/feature")).expect("refs should be created");
        fs::write(git_dir.join("HEAD"), "ref: refs/heads/feature/topic\n")
            .expect("HEAD should be written");
        fs::write(
            git_dir.join("refs/heads/feature/topic"),
            "0123456789abcdef0123456789abcdef01234567\n",
        )
        .expect("branch ref should be written");
        let root = RepoRoot::open(&root_path).expect("repo root should open");

        let snapshot = RepositoryRefSnapshot::detect(&root).expect("snapshot should resolve");

        assert_eq!(snapshot.kind, RepositoryRefKind::Branch);
        assert_eq!(snapshot.name.as_deref(), Some("feature/topic"));
        assert_eq!(
            snapshot.head_oid.as_deref(),
            Some("0123456789abcdef0123456789abcdef01234567")
        );
        assert_eq!(snapshot.local_branches, vec!["feature/topic".to_owned()]);
    }

    #[test]
    fn detects_detached_head() {
        let root_path = temp_dir("detached");
        let git_dir = root_path.join(".git");
        fs::create_dir_all(&git_dir).expect("git dir should be created");
        fs::write(
            git_dir.join("HEAD"),
            "fedcba9876543210fedcba9876543210fedcba98\n",
        )
        .expect("HEAD should be written");
        let root = RepoRoot::open(&root_path).expect("repo root should open");

        let snapshot = RepositoryRefSnapshot::detect(&root).expect("snapshot should resolve");

        assert_eq!(snapshot.kind, RepositoryRefKind::Detached);
        assert_eq!(
            snapshot.head_oid.as_deref(),
            Some("fedcba9876543210fedcba9876543210fedcba98")
        );
    }

    fn temp_dir(label: &str) -> std::path::PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time should be available")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("symdex_git_ref_{label}_{unique}"));
        fs::create_dir_all(&path).expect("temp dir should be created");
        path
    }
}

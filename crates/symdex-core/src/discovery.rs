use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use crate::{CoreError, FileFacts, Language, RepoRoot, Result, content_hash, stable_id};

const BUILT_IN_EXCLUDED_DIRS: &[&str] = &[
    ".git",
    ".hg",
    ".svn",
    ".direnv",
    "target",
    "node_modules",
    "vendor",
    ".symdex",
    "qdrant_storage",
];

#[derive(Debug, Clone)]
pub struct DiscoveryOptions {
    pub respect_gitignore: bool,
}

impl Default for DiscoveryOptions {
    fn default() -> Self {
        Self {
            respect_gitignore: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredFile {
    pub facts: FileFacts,
    pub absolute_path: PathBuf,
}

pub fn discover_rust_files(
    root: &RepoRoot,
    options: &DiscoveryOptions,
) -> Result<Vec<DiscoveredFile>> {
    let ignore_rules = if options.respect_gitignore {
        IgnoreRules::load(root.path())?
    } else {
        IgnoreRules::default()
    };
    let mut files = Vec::new();
    visit_dir(root, root.path(), &ignore_rules, &mut files)?;
    files.sort_by(|left, right| left.facts.relative_path.cmp(&right.facts.relative_path));
    Ok(files)
}

fn visit_dir(
    root: &RepoRoot,
    dir: &Path,
    ignore_rules: &IgnoreRules,
    files: &mut Vec<DiscoveredFile>,
) -> Result<()> {
    let ignore_rules = ignore_rules.extend_from_gitignore(root, dir)?;
    let entries =
        fs::read_dir(dir).map_err(|source| CoreError::io("read directory", dir, source))?;
    for entry in entries {
        let entry = entry.map_err(|source| CoreError::io("read directory entry", dir, source))?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|source| CoreError::io("read file type", &path, source))?;

        if file_type.is_symlink() {
            continue;
        }

        if file_type.is_dir() {
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if BUILT_IN_EXCLUDED_DIRS.contains(&name) {
                continue;
            }
            let relative = root.normalize_existing_path(&path)?;
            if ignore_rules.matches_dir(relative.as_str()) {
                continue;
            }
            visit_dir(root, &path, &ignore_rules, files)?;
            continue;
        }

        if !file_type.is_file() || path.extension().and_then(|ext| ext.to_str()) != Some("rs") {
            continue;
        }

        let relative = root.normalize_existing_path(&path)?;
        if ignore_rules.matches_file(relative.as_str()) {
            continue;
        }

        let bytes = fs::read(&path).map_err(|source| CoreError::io("read file", &path, source))?;
        let hash = content_hash(&bytes);
        let relative_path = relative.as_str().to_owned();
        let file_id = stable_id(&[root.id(), &relative_path]);
        files.push(DiscoveredFile {
            facts: FileFacts {
                id: file_id,
                relative_path,
                language: Language::Rust,
                content_hash: hash,
            },
            absolute_path: path,
        });
    }
    Ok(())
}

#[derive(Debug, Clone, Default)]
struct IgnoreRules {
    exact_paths: BTreeSet<String>,
    dir_prefixes: BTreeSet<String>,
    names: BTreeSet<ScopedNameRule>,
}

impl IgnoreRules {
    fn load(root: &Path) -> Result<Self> {
        let root = RepoRoot::open(root)?;
        Self::default().extend_from_gitignore(&root, root.path())
    }

    fn extend_from_gitignore(&self, root: &RepoRoot, dir: &Path) -> Result<Self> {
        let path = dir.join(".gitignore");
        let contents = match fs::read_to_string(&path) {
            Ok(contents) => contents,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(self.clone());
            }
            Err(source) => return Err(CoreError::io("read .gitignore", path, source)),
        };

        let mut rules = self.clone();
        let base_prefix = scoped_base_prefix(root, dir)?;
        for raw_line in contents.lines() {
            let line = raw_line.trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with('!') {
                continue;
            }

            let normalized = line.trim_start_matches('/').replace('\\', "/");
            if normalized.is_empty() || normalized.contains('*') {
                continue;
            }

            if let Some(dir) = normalized.strip_suffix('/') {
                rules.dir_prefixes.insert(format!("{base_prefix}{dir}/"));
            } else if normalized.contains('/') {
                rules
                    .exact_paths
                    .insert(format!("{base_prefix}{normalized}"));
            } else {
                rules.names.insert(ScopedNameRule {
                    base_prefix: base_prefix.clone(),
                    name: normalized,
                });
            }
        }
        Ok(rules)
    }

    fn matches_dir(&self, relative_path: &str) -> bool {
        let path = format!("{relative_path}/");
        self.names.iter().any(|rule| rule.matches(relative_path))
            || self
                .dir_prefixes
                .iter()
                .any(|prefix| path.starts_with(prefix))
            || self.exact_paths.contains(relative_path)
    }

    fn matches_file(&self, relative_path: &str) -> bool {
        self.exact_paths.contains(relative_path)
            || self
                .dir_prefixes
                .iter()
                .any(|prefix| relative_path.starts_with(prefix))
            || self.names.iter().any(|rule| rule.matches(relative_path))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct ScopedNameRule {
    base_prefix: String,
    name: String,
}

impl ScopedNameRule {
    fn matches(&self, relative_path: &str) -> bool {
        relative_path.starts_with(&self.base_prefix)
            && relative_path.rsplit('/').next() == Some(self.name.as_str())
    }
}

fn scoped_base_prefix(root: &RepoRoot, dir: &Path) -> Result<String> {
    if dir == root.path() {
        return Ok(String::new());
    }
    let relative = root.normalize_existing_path(dir)?;
    Ok(format!("{}/", relative.as_str()))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    use crate::{DiscoveryOptions, RepoRoot, discover_rust_files};

    #[test]
    fn discovers_rust_files_in_stable_order() {
        let repo = TestRepo::new("stable-order");
        repo.write("src/lib.rs", "pub fn lib() {}\n");
        repo.write("src/main.rs", "fn main() {}\n");
        repo.write("README.md", "# ignored\n");

        let root = RepoRoot::open(repo.path()).expect("repo root should open");
        let files = discover_rust_files(&root, &DiscoveryOptions::default())
            .expect("discovery should succeed");

        let paths: Vec<_> = files
            .iter()
            .map(|file| file.facts.relative_path.as_str())
            .collect();
        assert_eq!(paths, vec!["src/lib.rs", "src/main.rs"]);
        assert_ne!(files[0].facts.id, files[1].facts.id);
    }

    #[test]
    fn applies_builtin_and_gitignore_excludes() {
        let repo = TestRepo::new("ignore");
        repo.write(".gitignore", "ignored.rs\nignored_dir/\n");
        repo.write("src/lib.rs", "pub fn lib() {}\n");
        repo.write("ignored.rs", "pub fn ignored() {}\n");
        repo.write("ignored_dir/mod.rs", "pub fn ignored() {}\n");
        repo.write("target/debug/build.rs", "pub fn ignored() {}\n");

        let root = RepoRoot::open(repo.path()).expect("repo root should open");
        let files = discover_rust_files(&root, &DiscoveryOptions::default())
            .expect("discovery should succeed");

        let paths: Vec<_> = files
            .iter()
            .map(|file| file.facts.relative_path.as_str())
            .collect();
        assert_eq!(paths, vec!["src/lib.rs"]);
    }

    #[test]
    fn applies_nested_gitignore_excludes_from_parent_index() {
        let repo = TestRepo::new("nested-ignore");
        repo.write("src/lib.rs", "pub fn lib() {}\n");
        repo.write("nested/.gitignore", "ignored.rs\nignored_dir/\n");
        repo.write("nested/visible.rs", "pub fn visible() {}\n");
        repo.write("nested/ignored.rs", "pub fn ignored() {}\n");
        repo.write("nested/ignored_dir/mod.rs", "pub fn ignored() {}\n");

        let root = RepoRoot::open(repo.path()).expect("repo root should open");
        let files = discover_rust_files(&root, &DiscoveryOptions::default())
            .expect("discovery should succeed");

        let paths: Vec<_> = files
            .iter()
            .map(|file| file.facts.relative_path.as_str())
            .collect();
        assert_eq!(paths, vec!["nested/visible.rs", "src/lib.rs"]);
    }

    #[cfg(unix)]
    #[test]
    fn skips_symlinked_files_and_directories() {
        let repo = TestRepo::new("symlink-skip");
        let outside = TestRepo::new("symlink-outside");
        repo.write("src/lib.rs", "pub fn lib() {}\n");
        outside.write("outside.rs", "pub fn outside_file() {}\n");
        outside.write("dir/mod.rs", "pub fn outside_dir() {}\n");
        std::os::unix::fs::symlink(
            outside.path().join("outside.rs"),
            repo.path().join("link.rs"),
        )
        .expect("file symlink should be created");
        std::os::unix::fs::symlink(outside.path().join("dir"), repo.path().join("linked_dir"))
            .expect("directory symlink should be created");

        let root = RepoRoot::open(repo.path()).expect("repo root should open");
        let files = discover_rust_files(&root, &DiscoveryOptions::default())
            .expect("discovery should succeed");

        let paths: Vec<_> = files
            .iter()
            .map(|file| file.facts.relative_path.as_str())
            .collect();
        assert_eq!(paths, vec!["src/lib.rs"]);
    }

    struct TestRepo {
        path: PathBuf,
    }

    impl TestRepo {
        fn new(name: &str) -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock should be after epoch")
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "symdex-core-test-{name}-{}-{nonce}",
                std::process::id()
            ));
            fs::create_dir_all(&path).expect("test repo should be created");
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }

        fn write(&self, relative_path: &str, contents: &str) {
            let path = self.path.join(relative_path);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).expect("parent directory should be created");
            }
            fs::write(path, contents).expect("test file should be written");
        }
    }

    impl Drop for TestRepo {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

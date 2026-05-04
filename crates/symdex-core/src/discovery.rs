use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use globset::{GlobBuilder, GlobMatcher};

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

pub fn discover_indexable_files(
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

pub fn discover_rust_files(
    root: &RepoRoot,
    options: &DiscoveryOptions,
) -> Result<Vec<DiscoveredFile>> {
    discover_indexable_files(root, options).map(|files| {
        files
            .into_iter()
            .filter(|file| file.facts.language == Language::Rust)
            .collect()
    })
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
            root.normalize_existing_path(&path)?;
            visit_dir(root, &path, &ignore_rules, files)?;
            continue;
        }

        let Some(language) = path
            .extension()
            .and_then(|ext| ext.to_str())
            .and_then(Language::from_extension)
        else {
            continue;
        };
        if !file_type.is_file() {
            continue;
        }

        let relative = root.normalize_existing_path(&path)?;
        if ignore_rules.matches_file(relative.as_str()) {
            continue;
        }

        let bytes = fs::read(&path).map_err(|source| CoreError::io("read file", &path, source))?;
        let hash = content_hash(&bytes);
        let relative_path = relative.as_str().to_owned();
        let file_id = stable_id(&[root.id(), &relative_path, &hash]);
        files.push(DiscoveredFile {
            facts: FileFacts {
                id: file_id,
                relative_path,
                language,
                content_hash: hash,
            },
            absolute_path: path,
        });
    }
    Ok(())
}

#[derive(Debug, Clone, Default)]
struct IgnoreRules {
    rules: Vec<IgnoreRule>,
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
            if let Some(rule) = IgnoreRule::parse(&base_prefix, raw_line, &path)? {
                rules.rules.push(rule);
            }
        }
        Ok(rules)
    }

    fn matches_file(&self, relative_path: &str) -> bool {
        let mut ignored = false;
        for rule in &self.rules {
            if rule.matches(relative_path, false) {
                ignored = !rule.negated;
            }
        }
        ignored
    }
}

#[derive(Debug, Clone)]
struct IgnoreRule {
    base_prefix: String,
    negated: bool,
    directory_only: bool,
    basename_only: bool,
    matcher: GlobMatcher,
}

impl IgnoreRule {
    fn parse(base_prefix: &str, raw_line: &str, gitignore_path: &Path) -> Result<Option<Self>> {
        let Some((negated, mut pattern)) = parse_gitignore_line(raw_line) else {
            return Ok(None);
        };
        let directory_only = pattern.ends_with('/');
        while pattern.ends_with('/') {
            pattern.pop();
        }
        while pattern.starts_with('/') {
            pattern.remove(0);
        }
        pattern = pattern.replace('\\', "/");
        if pattern.is_empty() {
            return Ok(None);
        }

        let basename_only = !pattern.contains('/');
        let matcher_pattern = if basename_only {
            pattern
        } else {
            format!("{base_prefix}{pattern}")
        };
        let matcher = GlobBuilder::new(&matcher_pattern)
            .literal_separator(true)
            .backslash_escape(true)
            .build()
            .map_err(|error| {
                CoreError::io(
                    "parse .gitignore pattern",
                    gitignore_path,
                    io::Error::new(io::ErrorKind::InvalidData, error.to_string()),
                )
            })?
            .compile_matcher();

        Ok(Some(Self {
            base_prefix: base_prefix.to_owned(),
            negated,
            directory_only,
            basename_only,
            matcher,
        }))
    }

    fn matches(&self, relative_path: &str, is_dir: bool) -> bool {
        if !self.base_prefix.is_empty() && !relative_path.starts_with(&self.base_prefix) {
            return false;
        }
        if self.basename_only {
            return self.matches_basename(relative_path, is_dir);
        }
        self.matcher.is_match(relative_path)
            || ancestor_paths(relative_path).any(|ancestor| self.matcher.is_match(ancestor))
    }

    fn matches_basename(&self, relative_path: &str, is_dir: bool) -> bool {
        let scoped = if self.base_prefix.is_empty() {
            relative_path
        } else {
            relative_path
                .strip_prefix(&self.base_prefix)
                .unwrap_or(relative_path)
        };
        let mut components = scoped.split('/').collect::<Vec<_>>();
        if self.directory_only && !is_dir {
            components.pop();
        }
        components
            .into_iter()
            .any(|component| self.matcher.is_match(component))
    }
}

fn parse_gitignore_line(raw_line: &str) -> Option<(bool, String)> {
    let line = raw_line.trim();
    if line.is_empty() {
        return None;
    }
    if let Some(pattern) = line.strip_prefix(r"\#") {
        return Some((false, format!("#{pattern}")));
    }
    if let Some(pattern) = line.strip_prefix(r"\!") {
        return Some((false, format!("!{pattern}")));
    }
    if line.starts_with('#') {
        return None;
    }
    if let Some(pattern) = line.strip_prefix('!') {
        return Some((true, pattern.trim().to_owned()));
    }
    Some((false, line.to_owned()))
}

fn ancestor_paths(relative_path: &str) -> impl Iterator<Item = &str> {
    relative_path
        .match_indices('/')
        .map(|(index, _)| &relative_path[..index])
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
    use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
    use std::time::{SystemTime, UNIX_EPOCH};

    use crate::{
        DiscoveryOptions, Language, RepoRoot, discover_indexable_files, discover_rust_files,
    };

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
    fn discovers_active_language_files_in_stable_order() {
        let repo = TestRepo::new("active-languages");
        repo.write("src/lib.rs", "pub fn lib() {}\n");
        repo.write("src/Program.cs", "class Program { void Run() {} }\n");
        repo.write("web/app.jsx", "function App() { return null; }\n");
        repo.write("web/util.ts", "export function util(): void {}\n");
        repo.write("README.md", "# ignored\n");

        let root = RepoRoot::open(repo.path()).expect("repo root should open");
        let files = discover_indexable_files(&root, &DiscoveryOptions::default())
            .expect("discovery should succeed");

        let paths: Vec<_> = files
            .iter()
            .map(|file| (file.facts.relative_path.as_str(), file.facts.language))
            .collect();
        assert_eq!(
            paths,
            vec![
                ("src/Program.cs", Language::CSharp),
                ("src/lib.rs", Language::Rust),
                ("web/app.jsx", Language::JavaScript),
                ("web/util.ts", Language::TypeScript),
            ]
        );
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

    #[test]
    fn applies_gitignore_globs_and_negation_in_order() {
        let repo = TestRepo::new("glob-negation");
        repo.write(
            ".gitignore",
            "*.generated.rs\n**/*.generated.ts\nsrc/file?.rs\nsrc/class[0-9].rs\n!src/keep.generated.rs\n!web/keep.generated.ts\n",
        );
        repo.write("src/lib.rs", "pub fn lib() {}\n");
        repo.write("src/skip.generated.rs", "pub fn skipped() {}\n");
        repo.write("src/keep.generated.rs", "pub fn kept() {}\n");
        repo.write(
            "nested/skip.generated.ts",
            "export function skipped(): void {}\n",
        );
        repo.write("web/keep.generated.ts", "export function kept(): void {}\n");
        repo.write("src/file1.rs", "pub fn skipped() {}\n");
        repo.write("src/file10.rs", "pub fn visible() {}\n");
        repo.write("src/class7.rs", "pub fn skipped() {}\n");
        repo.write("src/classx.rs", "pub fn visible() {}\n");

        let root = RepoRoot::open(repo.path()).expect("repo root should open");
        let files = discover_indexable_files(&root, &DiscoveryOptions::default())
            .expect("discovery should succeed");

        let paths: Vec<_> = files
            .iter()
            .map(|file| file.facts.relative_path.as_str())
            .collect();
        assert_eq!(
            paths,
            vec![
                "src/classx.rs",
                "src/file10.rs",
                "src/keep.generated.rs",
                "src/lib.rs",
                "web/keep.generated.ts",
            ]
        );
    }

    #[test]
    fn applies_directory_rules_with_descendant_negation() {
        let repo = TestRepo::new("directory-negation");
        repo.write(".gitignore", "generated/\n!generated/keep.rs\n");
        repo.write("src/lib.rs", "pub fn lib() {}\n");
        repo.write("generated/drop.rs", "pub fn dropped() {}\n");
        repo.write("generated/nested/drop.rs", "pub fn dropped() {}\n");
        repo.write("generated/keep.rs", "pub fn kept() {}\n");

        let root = RepoRoot::open(repo.path()).expect("repo root should open");
        let files = discover_rust_files(&root, &DiscoveryOptions::default())
            .expect("discovery should succeed");

        let paths: Vec<_> = files
            .iter()
            .map(|file| file.facts.relative_path.as_str())
            .collect();
        assert_eq!(paths, vec!["generated/keep.rs", "src/lib.rs"]);
    }

    #[test]
    fn scopes_nested_gitignore_globs_to_nested_directories() {
        let repo = TestRepo::new("nested-glob-scope");
        repo.write("src/lib.rs", "pub fn lib() {}\n");
        repo.write("other/ignored.rs", "pub fn visible() {}\n");
        repo.write(
            "nested/.gitignore",
            "ignored.rs\nignored_dir/\n*.generated.rs\n!keep.generated.rs\n",
        );
        repo.write("nested/visible.rs", "pub fn visible() {}\n");
        repo.write("nested/sub/ignored.rs", "pub fn ignored() {}\n");
        repo.write("nested/sub/ignored_dir/mod.rs", "pub fn ignored() {}\n");
        repo.write("nested/drop.generated.rs", "pub fn ignored() {}\n");
        repo.write("nested/keep.generated.rs", "pub fn kept() {}\n");

        let root = RepoRoot::open(repo.path()).expect("repo root should open");
        let files = discover_rust_files(&root, &DiscoveryOptions::default())
            .expect("discovery should succeed");

        let paths: Vec<_> = files
            .iter()
            .map(|file| file.facts.relative_path.as_str())
            .collect();
        assert_eq!(
            paths,
            vec![
                "nested/keep.generated.rs",
                "nested/visible.rs",
                "other/ignored.rs",
                "src/lib.rs",
            ]
        );
    }

    #[test]
    fn builtin_excludes_cannot_be_reincluded_by_gitignore_negation() {
        let repo = TestRepo::new("builtin-hard-excludes");
        repo.write(".gitignore", "!target/keep.rs\n!node_modules/keep.ts\n");
        repo.write("src/lib.rs", "pub fn lib() {}\n");
        repo.write("target/keep.rs", "pub fn ignored() {}\n");
        repo.write(
            "node_modules/keep.ts",
            "export function ignored(): void {}\n",
        );

        let root = RepoRoot::open(repo.path()).expect("repo root should open");
        let files = discover_indexable_files(&root, &DiscoveryOptions::default())
            .expect("discovery should succeed");

        let paths: Vec<_> = files
            .iter()
            .map(|file| file.facts.relative_path.as_str())
            .collect();
        assert_eq!(paths, vec!["src/lib.rs"]);
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

    static NEXT_TEST_REPO_ID: AtomicU64 = AtomicU64::new(0);

    impl TestRepo {
        fn new(name: &str) -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock should be after epoch")
                .as_nanos();
            let id = NEXT_TEST_REPO_ID.fetch_add(1, AtomicOrdering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "symdex-core-test-{name}-{}-{nonce}-{id}",
                std::process::id(),
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

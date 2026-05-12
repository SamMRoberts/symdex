use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    pub project: ProjectConfig,
    pub index: IndexConfig,
    pub parser: ParserConfig,
    pub storage: StorageConfig,
    pub search: SearchConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ProjectConfig {
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct IndexConfig {
    pub include: Vec<String>,
    pub exclude: Vec<String>,
    pub max_file_size_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ParserConfig {
    pub languages: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct StorageConfig {
    pub database_path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SearchConfig {
    pub enable_fts: bool,
}

impl Default for ProjectConfig {
    fn default() -> Self {
        Self {
            name: "symdex-project".to_string(),
        }
    }
}

impl Default for IndexConfig {
    fn default() -> Self {
        Self {
            include: vec!["src/**".to_string(), "tests/**".to_string()],
            exclude: vec![
                ".git/**".to_string(),
                "target/**".to_string(),
                "node_modules/**".to_string(),
                "dist/**".to_string(),
                "build/**".to_string(),
                ".symdex/**".to_string(),
            ],
            max_file_size_bytes: 1_048_576,
        }
    }
}

impl Default for ParserConfig {
    fn default() -> Self {
        Self {
            languages: vec![
                "rust".into(),
                "typescript".into(),
                "javascript".into(),
                "python".into(),
            ],
        }
    }
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            database_path: PathBuf::from(".symdex/index.db"),
        }
    }
}

impl Default for SearchConfig {
    fn default() -> Self {
        Self { enable_fts: true }
    }
}

impl AppConfig {
    pub fn load(repo_root: &Path) -> Result<Self> {
        let mut value = toml::Value::try_from(Self::default())?;
        merge_file(&mut value, &repo_root.join("symdex.toml"))?;
        merge_file(&mut value, &repo_root.join("symdex.local.toml"))?;
        let mut config: Self = value.try_into()?;
        if config.project.name == "symdex-project" {
            config.project.name = repo_root
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("project")
                .to_string();
        }
        Ok(config)
    }

    pub fn database_path(&self, repo_root: &Path) -> PathBuf {
        if self.storage.database_path.is_absolute() {
            self.storage.database_path.clone()
        } else {
            repo_root.join(&self.storage.database_path)
        }
    }
}

pub fn init_config(repo_root: &Path, force: bool) -> Result<()> {
    fs::create_dir_all(repo_root.join(".symdex")).context("cannot create .symdex directory")?;
    let config_path = repo_root.join("symdex.toml");
    if config_path.exists() && !force {
        bail!("symdex.toml already exists; pass --force to overwrite it");
    }
    let mut config = AppConfig::default();
    config.project.name = repo_root
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("project")
        .to_string();
    fs::write(&config_path, toml::to_string_pretty(&config)?)
        .context("failed to write symdex.toml")?;
    Ok(())
}

pub fn discover_repo_root(path: &Path) -> Result<PathBuf> {
    let mut current = if path.exists() {
        path.canonicalize()?
    } else {
        path.to_path_buf()
    };
    if current.is_file() {
        current = current
            .parent()
            .context("file has no parent directory")?
            .to_path_buf();
    }
    for candidate in current.ancestors() {
        if candidate.join("symdex.toml").exists() || candidate.join(".git").exists() {
            return Ok(candidate.to_path_buf());
        }
    }
    Ok(current)
}

fn merge_file(base: &mut toml::Value, path: &Path) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    let parsed: toml::Value = fs::read_to_string(path)
        .with_context(|| format!("failed to read {}", path.display()))?
        .parse()
        .with_context(|| format!("failed to parse {}", path.display()))?;
    merge_value(base, parsed);
    Ok(())
}

fn merge_value(base: &mut toml::Value, overlay: toml::Value) {
    match (base, overlay) {
        (toml::Value::Table(base_table), toml::Value::Table(overlay_table)) => {
            for (key, value) in overlay_table {
                match base_table.get_mut(&key) {
                    Some(existing) => merge_value(existing, value),
                    None => {
                        base_table.insert(key, value);
                    }
                }
            }
        }
        (base_slot, overlay_value) => *base_slot = overlay_value,
    }
}

use std::path::{Path, PathBuf};

use anyhow::Result;
use globset::{Glob, GlobSet, GlobSetBuilder};
use ignore::WalkBuilder;

use crate::{
    config::AppConfig,
    parser::languages::{self, LanguageKind},
};

#[derive(Debug, Clone)]
pub struct DiscoveredFile {
    pub absolute_path: PathBuf,
    pub relative_path: String,
    pub language: LanguageKind,
    pub size_bytes: u64,
}

pub fn discover_files(repo_root: &Path, config: &AppConfig) -> Result<Vec<DiscoveredFile>> {
    let include = build_globs(&config.index.include)?;
    let exclude = build_globs(&config.index.exclude)?;
    let mut files = Vec::new();
    let walker = WalkBuilder::new(repo_root)
        .standard_filters(true)
        .hidden(false)
        .build();
    for entry in walker {
        let entry = entry?;
        if !entry
            .file_type()
            .map(|kind| kind.is_file())
            .unwrap_or(false)
        {
            continue;
        }
        let path = entry.path();
        let relative = path.strip_prefix(repo_root).unwrap_or(path);
        let relative_text = relative.to_string_lossy().replace('\\', "/");
        if !include.is_empty() && !include.is_match(&relative_text) {
            continue;
        }
        if exclude.is_match(&relative_text) {
            continue;
        }
        let metadata = entry.metadata()?;
        if metadata.len() > config.index.max_file_size_bytes {
            continue;
        }
        let Some(language) = languages::detect(path) else {
            continue;
        };
        if !languages::allowed(language, &config.parser.languages) {
            continue;
        }
        files.push(DiscoveredFile {
            absolute_path: path.to_path_buf(),
            relative_path: relative_text,
            language,
            size_bytes: metadata.len(),
        });
    }
    files.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    Ok(files)
}

fn build_globs(patterns: &[String]) -> Result<GlobSet> {
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        builder.add(Glob::new(pattern)?);
    }
    Ok(builder.build()?)
}

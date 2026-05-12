use std::path::Path;

use anyhow::{Result, bail};
use tree_sitter::Language;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LanguageKind {
    Rust,
    TypeScript,
    JavaScript,
    Python,
}

impl LanguageKind {
    pub fn id(self) -> &'static str {
        match self {
            Self::Rust => "rust",
            Self::TypeScript => "typescript",
            Self::JavaScript => "javascript",
            Self::Python => "python",
        }
    }

    pub fn tree_sitter_language(self) -> Language {
        match self {
            Self::Rust => tree_sitter_rust::LANGUAGE.into(),
            Self::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            Self::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
            Self::Python => tree_sitter_python::LANGUAGE.into(),
        }
    }
}

pub fn detect(path: &Path) -> Option<LanguageKind> {
    match path.extension().and_then(|extension| extension.to_str()) {
        Some("rs") => Some(LanguageKind::Rust),
        Some("ts") | Some("tsx") => Some(LanguageKind::TypeScript),
        Some("js") | Some("jsx") | Some("mjs") | Some("cjs") => Some(LanguageKind::JavaScript),
        Some("py") => Some(LanguageKind::Python),
        _ => None,
    }
}

pub fn allowed(language: LanguageKind, configured: &[String]) -> bool {
    configured.iter().any(|item| item == language.id())
}

pub fn parse_language(value: &str) -> Result<LanguageKind> {
    match value {
        "rust" => Ok(LanguageKind::Rust),
        "typescript" => Ok(LanguageKind::TypeScript),
        "javascript" => Ok(LanguageKind::JavaScript),
        "python" => Ok(LanguageKind::Python),
        other => bail!("unsupported language: {other}"),
    }
}

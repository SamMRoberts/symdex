use tree_sitter::{Node, Parser};

use crate::{
    ByteRange, ChunkKind, CodeChunk, CoreError, FileFacts, LineRange, Result, content_hash,
    stable_id,
};

pub fn extract_rust_chunks(file: &FileFacts, source: &str) -> Result<Vec<CodeChunk>> {
    let mut parser = Parser::new();
    let language = tree_sitter_rust::LANGUAGE.into();
    parser
        .set_language(&language)
        .map_err(|error| CoreError::ParserLanguage {
            message: error.to_string(),
        })?;

    let tree = parser
        .parse(source, None)
        .ok_or_else(|| CoreError::ParseFailed {
            path: file.relative_path.clone(),
        })?;
    if tree.root_node().has_error() {
        return Err(CoreError::ParseFailed {
            path: file.relative_path.clone(),
        });
    }

    let mut chunks = Vec::new();
    collect_function_chunks(tree.root_node(), file, source, &mut chunks);
    chunks.sort_by_key(|chunk| (chunk.byte_range.start, chunk.byte_range.end));

    if chunks.is_empty() && !source.trim().is_empty() {
        chunks.push(file_fallback_chunk(file, source));
    }

    Ok(chunks)
}

fn collect_function_chunks(
    node: Node<'_>,
    file: &FileFacts,
    source: &str,
    chunks: &mut Vec<CodeChunk>,
) {
    if node.kind() == "function_item" {
        chunks.push(function_chunk(node, file, source));
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_function_chunks(child, file, source, chunks);
    }
}

fn function_chunk(node: Node<'_>, file: &FileFacts, source: &str) -> CodeChunk {
    let kind = if has_ancestor_kind(node, "impl_item") {
        ChunkKind::Method
    } else {
        ChunkKind::Function
    };
    let symbol_name = node
        .child_by_field_name("name")
        .and_then(|name| name.utf8_text(source.as_bytes()).ok())
        .map(str::to_owned);
    chunk_for_node(node, file, source, kind, symbol_name)
}

fn file_fallback_chunk(file: &FileFacts, source: &str) -> CodeChunk {
    let text_hash = content_hash(source.as_bytes());
    let end_line = source.lines().count().max(1);
    CodeChunk {
        id: stable_id(&[
            &file.id,
            ChunkKind::FileFallback.as_str(),
            "0",
            &source.len().to_string(),
            &text_hash,
        ]),
        file_id: file.id.clone(),
        relative_path: file.relative_path.clone(),
        symbol_id: None,
        symbol_name: None,
        kind: ChunkKind::FileFallback,
        byte_range: ByteRange::new(0, source.len()),
        line_range: LineRange::new(1, end_line),
        text_hash,
    }
}

fn chunk_for_node(
    node: Node<'_>,
    file: &FileFacts,
    source: &str,
    kind: ChunkKind,
    symbol_name: Option<String>,
) -> CodeChunk {
    let start_byte = node.start_byte();
    let end_byte = node.end_byte();
    let text_hash = content_hash(&source.as_bytes()[start_byte..end_byte]);
    let symbol_id = symbol_name.as_ref().map(|name| {
        stable_id(&[
            &file.id,
            kind.as_str(),
            name,
            &start_byte.to_string(),
            &text_hash,
        ])
    });

    CodeChunk {
        id: stable_id(&[
            &file.id,
            kind.as_str(),
            &start_byte.to_string(),
            &end_byte.to_string(),
            &text_hash,
        ]),
        file_id: file.id.clone(),
        relative_path: file.relative_path.clone(),
        symbol_id,
        symbol_name,
        kind,
        byte_range: ByteRange::new(start_byte, end_byte),
        line_range: LineRange::new(node.start_position().row + 1, node.end_position().row + 1),
        text_hash,
    }
}

fn has_ancestor_kind(node: Node<'_>, kind: &str) -> bool {
    let mut parent = node.parent();
    while let Some(current) = parent {
        if current.kind() == kind {
            return true;
        }
        parent = current.parent();
    }
    false
}

#[cfg(test)]
mod tests {
    use crate::{ChunkKind, FileFacts, Language, extract_rust_chunks};

    fn file() -> FileFacts {
        FileFacts {
            id: "file-1".to_owned(),
            relative_path: "src/lib.rs".to_owned(),
            language: Language::Rust,
            content_hash: "hash".to_owned(),
        }
    }

    #[test]
    fn extracts_functions_and_methods_with_line_ranges() {
        let source = r#"pub fn free_function() -> i32 {
    42
}

struct Counter;

impl Counter {
    pub fn increment(&self) {
    }
}
"#;

        let chunks = extract_rust_chunks(&file(), source).expect("chunks should parse");

        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].kind, ChunkKind::Function);
        assert_eq!(chunks[0].symbol_name.as_deref(), Some("free_function"));
        assert_eq!(chunks[0].line_range.start, 1);
        assert_eq!(chunks[0].line_range.end, 3);
        assert_eq!(chunks[1].kind, ChunkKind::Method);
        assert_eq!(chunks[1].symbol_name.as_deref(), Some("increment"));
        assert_eq!(chunks[1].line_range.start, 8);
    }

    #[test]
    fn emits_file_fallback_when_no_function_chunks_exist() {
        let source = "pub struct OnlyData;\n";

        let chunks = extract_rust_chunks(&file(), source).expect("chunks should parse");

        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].kind, ChunkKind::FileFallback);
        assert_eq!(chunks[0].line_range.start, 1);
        assert_eq!(chunks[0].line_range.end, 1);
    }

    #[test]
    fn chunk_ids_are_stable() {
        let source = "pub fn stable() {}\n";

        let first = extract_rust_chunks(&file(), source).expect("first parse should succeed");
        let second = extract_rust_chunks(&file(), source).expect("second parse should succeed");

        assert_eq!(first[0].id, second[0].id);
        assert_eq!(first[0].symbol_id, second[0].symbol_id);
    }

    #[test]
    fn syntax_errors_fail_closed() {
        let source = "pub fn broken( {}\n";

        let error = extract_rust_chunks(&file(), source).expect_err("syntax errors should fail");

        assert!(error.to_string().contains("failed to parse Rust file"));
    }
}

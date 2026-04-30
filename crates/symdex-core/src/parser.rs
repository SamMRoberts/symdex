use tree_sitter::{Node, Parser};

use crate::{
    ByteRange, CallEdge, ChunkKind, CodeChunk, CoreError, FileFacts, LineRange, ResolutionStatus,
    Result, RustFileIndex, Symbol, SymbolKind, content_hash, stable_id,
};

pub fn extract_rust_chunks(file: &FileFacts, source: &str) -> Result<Vec<CodeChunk>> {
    Ok(index_rust_file(file, source)?.chunks)
}

pub fn index_rust_file(file: &FileFacts, source: &str) -> Result<RustFileIndex> {
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

    let mut functions = Vec::new();
    collect_function_nodes(tree.root_node(), &mut functions);

    let mut chunks = Vec::new();
    let mut symbols = Vec::new();
    for function in &functions {
        let (chunk, symbol) = function_facts(*function, file, source);
        chunks.push(chunk);
        symbols.push(symbol);
    }

    if chunks.is_empty() && !source.trim().is_empty() {
        chunks.push(file_fallback_chunk(file, source));
    }

    let mut calls = Vec::new();
    for (function, symbol) in functions.iter().zip(symbols.iter()) {
        collect_call_edges(*function, symbol, &symbols, source, &mut calls);
    }

    chunks.sort_by_key(|chunk| (chunk.byte_range.start, chunk.byte_range.end));
    symbols.sort_by_key(|symbol| (symbol.byte_range.start, symbol.byte_range.end));
    calls.sort_by_key(|call| (call.call_line, call.callee_text.clone()));

    Ok(RustFileIndex {
        chunks,
        symbols,
        calls,
    })
}

fn collect_function_nodes<'tree>(node: Node<'tree>, functions: &mut Vec<Node<'tree>>) {
    if node.kind() == "function_item" {
        functions.push(node);
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_function_nodes(child, functions);
    }
}

fn function_facts(node: Node<'_>, file: &FileFacts, source: &str) -> (CodeChunk, Symbol) {
    let symbol_kind = if has_ancestor_kind(node, "impl_item") {
        SymbolKind::Method
    } else {
        SymbolKind::Function
    };
    let chunk_kind = if symbol_kind == SymbolKind::Method {
        ChunkKind::Method
    } else {
        ChunkKind::Function
    };
    let symbol_name = node
        .child_by_field_name("name")
        .and_then(|name| name.utf8_text(source.as_bytes()).ok())
        .unwrap_or("<anonymous>")
        .to_owned();
    let qualified_name = qualified_name(file, node, source, &symbol_name, symbol_kind);
    let symbol = symbol_for_node(
        node,
        file,
        source,
        symbol_kind,
        symbol_name.clone(),
        qualified_name,
    );
    let chunk = chunk_for_node(
        node,
        file,
        source,
        chunk_kind,
        Some(symbol_name),
        Some(symbol.id.clone()),
    );
    (chunk, symbol)
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
    symbol_id: Option<String>,
) -> CodeChunk {
    let start_byte = node.start_byte();
    let end_byte = node.end_byte();
    let text_hash = content_hash(&source.as_bytes()[start_byte..end_byte]);
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

fn symbol_for_node(
    node: Node<'_>,
    file: &FileFacts,
    source: &str,
    kind: SymbolKind,
    name: String,
    qualified_name: String,
) -> Symbol {
    let start_byte = node.start_byte();
    let end_byte = node.end_byte();
    let signature = signature_text(node, source);
    let signature_hash = signature
        .as_ref()
        .map(|signature| content_hash(signature.as_bytes()))
        .unwrap_or_else(|| content_hash(name.as_bytes()));
    Symbol {
        id: stable_id(&[
            &file.id,
            kind.as_str(),
            &qualified_name,
            &start_byte.to_string(),
            &signature_hash,
        ]),
        file_id: file.id.clone(),
        parent_symbol_id: None,
        name,
        qualified_name,
        kind,
        signature,
        byte_range: ByteRange::new(start_byte, end_byte),
        line_range: LineRange::new(node.start_position().row + 1, node.end_position().row + 1),
    }
}

fn collect_call_edges(
    function: Node<'_>,
    caller: &Symbol,
    symbols: &[Symbol],
    source: &str,
    calls: &mut Vec<CallEdge>,
) {
    let mut cursor = function.walk();
    for child in function.children(&mut cursor) {
        collect_call_edges_from_node(child, caller, symbols, source, calls);
    }
}

fn collect_call_edges_from_node(
    node: Node<'_>,
    caller: &Symbol,
    symbols: &[Symbol],
    source: &str,
    calls: &mut Vec<CallEdge>,
) {
    if node.kind() == "call_expression"
        && let Some(callee_text) = callee_text(node, source)
    {
        let (callee_symbol_id, resolution_status, confidence) =
            resolve_callee(&callee_text, symbols);
        let call_line = node.start_position().row + 1;
        calls.push(CallEdge {
            id: stable_id(&[
                &caller.id,
                &callee_text,
                &call_line.to_string(),
                &node.start_byte().to_string(),
            ]),
            caller_symbol_id: caller.id.clone(),
            callee_text,
            callee_symbol_id,
            call_line,
            confidence,
            resolution_status,
        });
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_call_edges_from_node(child, caller, symbols, source, calls);
    }
}

fn resolve_callee(
    callee_text: &str,
    symbols: &[Symbol],
) -> (Option<String>, ResolutionStatus, f32) {
    let exact: Vec<&Symbol> = symbols
        .iter()
        .filter(|symbol| symbol.qualified_name == callee_text)
        .collect();
    if exact.len() == 1 {
        return (
            Some(exact[0].id.clone()),
            ResolutionStatus::ResolvedExact,
            1.0,
        );
    }
    if exact.len() > 1 {
        return (None, ResolutionStatus::Ambiguous, 0.2);
    }

    let suffix = callee_text.rsplit("::").next().unwrap_or(callee_text);
    let candidates: Vec<&Symbol> = symbols
        .iter()
        .filter(|symbol| symbol.name == suffix || symbol.qualified_name.ends_with(callee_text))
        .collect();
    match candidates.as_slice() {
        [symbol] => (
            Some(symbol.id.clone()),
            ResolutionStatus::ResolvedLocalCandidate,
            0.75,
        ),
        [] => (None, ResolutionStatus::Unresolved, 0.25),
        _ => (None, ResolutionStatus::Ambiguous, 0.2),
    }
}

fn callee_text(call: Node<'_>, source: &str) -> Option<String> {
    let function = call.child_by_field_name("function")?;
    Some(match function.kind() {
        "identifier" => node_text(function, source)?.to_owned(),
        "scoped_identifier" => node_text(function, source)?.replace(' ', ""),
        "field_expression" => function
            .child_by_field_name("field")
            .and_then(|field| node_text(field, source))
            .unwrap_or_else(|| node_text(function, source).unwrap_or(""))
            .to_owned(),
        _ => node_text(function, source)?.replace(' ', ""),
    })
}

fn qualified_name(
    file: &FileFacts,
    node: Node<'_>,
    source: &str,
    name: &str,
    kind: SymbolKind,
) -> String {
    let mut parts = module_parts(&file.relative_path);
    if kind == SymbolKind::Method
        && let Some(impl_type) = impl_type_name(node, source)
    {
        parts.push(impl_type);
    }
    parts.push(name.to_owned());
    parts.join("::")
}

fn module_parts(relative_path: &str) -> Vec<String> {
    let trimmed = relative_path.strip_suffix(".rs").unwrap_or(relative_path);
    let mut parts: Vec<String> = trimmed
        .split('/')
        .filter(|part| *part != "src" && *part != "lib" && *part != "main" && *part != "mod")
        .map(str::to_owned)
        .collect();
    if parts.last().is_some_and(|part| part == "mod") {
        parts.pop();
    }
    parts
}

fn impl_type_name(node: Node<'_>, source: &str) -> Option<String> {
    let mut parent = node.parent();
    while let Some(current) = parent {
        if current.kind() == "impl_item" {
            if let Some(type_node) = current.child_by_field_name("type") {
                return node_text(type_node, source).map(|text| text.replace(' ', ""));
            }
            let mut cursor = current.walk();
            for child in current.children(&mut cursor) {
                if child.kind().contains("type") {
                    return node_text(child, source).map(|text| text.replace(' ', ""));
                }
            }
            return None;
        }
        parent = current.parent();
    }
    None
}

fn signature_text(node: Node<'_>, source: &str) -> Option<String> {
    let body = node.child_by_field_name("body")?;
    let signature = &source[node.start_byte()..body.start_byte()];
    Some(signature.trim().to_owned())
}

fn node_text<'a>(node: Node<'_>, source: &'a str) -> Option<&'a str> {
    node.utf8_text(source.as_bytes()).ok()
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
    use crate::{
        ChunkKind, FileFacts, Language, ResolutionStatus, SymbolKind, extract_rust_chunks,
        index_rust_file,
    };

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

    #[test]
    fn extracts_qualified_symbols() {
        let source = r#"struct Counter;

impl Counter {
    pub fn increment(&self) {}
}

pub fn free_function() {}
"#;

        let index = index_rust_file(&file(), source).expect("index should parse");

        assert_eq!(index.symbols.len(), 2);
        assert_eq!(index.symbols[0].kind, SymbolKind::Method);
        assert_eq!(index.symbols[0].qualified_name, "Counter::increment");
        assert_eq!(index.symbols[1].kind, SymbolKind::Function);
        assert_eq!(index.symbols[1].qualified_name, "free_function");
        assert!(
            index.symbols[1]
                .signature
                .as_deref()
                .is_some_and(|sig| sig.contains("pub fn free_function"))
        );
    }

    #[test]
    fn extracts_resolved_and_unresolved_calls() {
        let source = r#"pub fn helper() {}

pub fn caller() {
    helper();
    external::thing();
}
"#;

        let index = index_rust_file(&file(), source).expect("index should parse");

        assert_eq!(index.calls.len(), 2);
        let helper_call = index
            .calls
            .iter()
            .find(|call| call.callee_text == "helper")
            .expect("helper call should exist");
        assert_eq!(
            helper_call.resolution_status,
            ResolutionStatus::ResolvedExact
        );
        assert!(helper_call.callee_symbol_id.is_some());

        let external_call = index
            .calls
            .iter()
            .find(|call| call.callee_text == "external::thing")
            .expect("external call should exist");
        assert_eq!(
            external_call.resolution_status,
            ResolutionStatus::Unresolved
        );
        assert!(external_call.callee_symbol_id.is_none());
    }
}

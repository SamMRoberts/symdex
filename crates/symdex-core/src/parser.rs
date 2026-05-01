use tree_sitter::{Node, Parser};

use crate::{
    ByteRange, CallEdge, ChunkKind, CodeChunk, CoreError, FileFacts, Language, LineRange,
    ParseDiagnostic, ResolutionStatus, Result, SourceFileIndex, Symbol, SymbolKind, content_hash,
    secret_exclusion_reason, stable_id,
};

pub fn extract_chunks(file: &FileFacts, source: &str) -> Result<Vec<CodeChunk>> {
    Ok(index_source_file(file, source)?.chunks)
}

pub fn extract_rust_chunks(file: &FileFacts, source: &str) -> Result<Vec<CodeChunk>> {
    extract_chunks(file, source)
}

pub fn index_rust_file(file: &FileFacts, source: &str) -> Result<SourceFileIndex> {
    index_source_file(file, source)
}

pub fn index_source_file(file: &FileFacts, source: &str) -> Result<SourceFileIndex> {
    let mut parser = Parser::new();
    set_parser_language(&mut parser, file.language, is_tsx_path(&file.relative_path))?;

    let tree = parser
        .parse(source, None)
        .ok_or_else(|| CoreError::ParseFailed {
            path: file.relative_path.clone(),
        })?;
    let parse_diagnostics = collect_parse_diagnostics(tree.root_node());

    let mut functions = Vec::new();
    collect_function_nodes(file.language, tree.root_node(), &mut functions);

    let mut chunks = Vec::new();
    let mut symbols = Vec::new();
    for function in &functions {
        let (chunk, symbol) = function_facts(function.node, file, source, function.symbol_kind);
        chunks.push(chunk);
        symbols.push(symbol);
    }

    if chunks.is_empty() && !source.trim().is_empty() {
        chunks.push(file_fallback_chunk(file, source));
    }

    let mut calls = Vec::new();
    for (function, symbol) in functions.iter().zip(symbols.iter()) {
        collect_call_edges(
            function.node,
            file.language,
            symbol,
            &symbols,
            source,
            &mut calls,
        );
    }

    chunks.sort_by_key(|chunk| (chunk.byte_range.start, chunk.byte_range.end));
    symbols.sort_by_key(|symbol| (symbol.byte_range.start, symbol.byte_range.end));
    calls.sort_by_key(|call| (call.call_line, call.callee_text.clone()));

    Ok(SourceFileIndex {
        chunks,
        symbols,
        calls,
        parse_diagnostics,
    })
}

fn collect_parse_diagnostics(root: Node<'_>) -> Vec<ParseDiagnostic> {
    let mut diagnostics = Vec::new();
    collect_parse_diagnostics_from_node(root, &mut diagnostics);
    diagnostics.sort_by_key(|diagnostic| {
        (
            diagnostic.byte_range.start,
            diagnostic.byte_range.end,
            diagnostic.message.clone(),
        )
    });
    diagnostics
}

fn collect_parse_diagnostics_from_node(node: Node<'_>, diagnostics: &mut Vec<ParseDiagnostic>) {
    if node.is_error() || node.is_missing() {
        diagnostics.push(ParseDiagnostic {
            byte_range: ByteRange::new(node.start_byte(), node.end_byte()),
            line_range: LineRange::new(node.start_position().row + 1, node.end_position().row + 1),
            message: parse_diagnostic_message(node),
        });
    }

    if !node.has_error() {
        return;
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_parse_diagnostics_from_node(child, diagnostics);
    }
}

fn parse_diagnostic_message(node: Node<'_>) -> String {
    if node.is_missing() {
        format!("tree-sitter missing `{}`", node.kind())
    } else {
        format!("tree-sitter parse error `{}`", node.kind())
    }
}

fn set_parser_language(parser: &mut Parser, language: Language, tsx: bool) -> Result<()> {
    let grammar = match language {
        Language::CSharp => tree_sitter_c_sharp::LANGUAGE.into(),
        Language::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
        Language::Rust => tree_sitter_rust::LANGUAGE.into(),
        Language::TypeScript if tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
        Language::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
    };
    parser
        .set_language(&grammar)
        .map_err(|error| CoreError::ParserLanguage {
            message: error.to_string(),
        })
}

#[derive(Debug, Clone, Copy)]
struct FunctionNode<'tree> {
    node: Node<'tree>,
    symbol_kind: SymbolKind,
}

fn collect_function_nodes<'tree>(
    language: Language,
    node: Node<'tree>,
    functions: &mut Vec<FunctionNode<'tree>>,
) {
    if let Some(symbol_kind) = function_symbol_kind(language, node) {
        functions.push(FunctionNode { node, symbol_kind });
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_function_nodes(language, child, functions);
    }
}

fn function_symbol_kind(language: Language, node: Node<'_>) -> Option<SymbolKind> {
    match language {
        Language::Rust if node.kind() == "function_item" => {
            if has_ancestor_kind(node, "impl_item") {
                Some(SymbolKind::Method)
            } else {
                Some(SymbolKind::Function)
            }
        }
        Language::CSharp => match node.kind() {
            "method_declaration"
            | "constructor_declaration"
            | "destructor_declaration"
            | "operator_declaration"
            | "conversion_operator_declaration" => Some(SymbolKind::Method),
            "local_function_statement" => Some(SymbolKind::Function),
            _ => None,
        },
        Language::JavaScript | Language::TypeScript => match node.kind() {
            "function_declaration" | "generator_function_declaration" => Some(SymbolKind::Function),
            "method_definition"
            | "method_signature"
            | "abstract_method_signature"
            | "generator_method" => Some(SymbolKind::Method),
            "public_field_definition" if value_is_function_like(node) => Some(SymbolKind::Method),
            "variable_declarator" if value_is_function_like(node) => Some(SymbolKind::Function),
            _ => None,
        },
        _ => None,
    }
}

fn function_facts(
    node: Node<'_>,
    file: &FileFacts,
    source: &str,
    symbol_kind: SymbolKind,
) -> (CodeChunk, Symbol) {
    let chunk_kind = if symbol_kind == SymbolKind::Method {
        ChunkKind::Method
    } else {
        ChunkKind::Function
    };
    let symbol_name = symbol_name(node, source);
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
    let excluded_reason = secret_exclusion_reason(&file.relative_path, source).map(str::to_owned);
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
        excluded_reason,
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
    let text = &source[start_byte..end_byte];
    let text_hash = content_hash(text.as_bytes());
    let excluded_reason = secret_exclusion_reason(&file.relative_path, text).map(str::to_owned);
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
        excluded_reason,
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
    language: Language,
    caller: &Symbol,
    symbols: &[Symbol],
    source: &str,
    calls: &mut Vec<CallEdge>,
) {
    let mut cursor = function.walk();
    for child in function.children(&mut cursor) {
        collect_call_edges_from_node(child, language, caller, symbols, source, calls);
    }
}

fn collect_call_edges_from_node(
    node: Node<'_>,
    language: Language,
    caller: &Symbol,
    symbols: &[Symbol],
    source: &str,
    calls: &mut Vec<CallEdge>,
) {
    if is_call_expression(language, node)
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
        collect_call_edges_from_node(child, language, caller, symbols, source, calls);
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

    let suffix = symbol_suffix(callee_text);
    let candidates: Vec<&Symbol> = symbols
        .iter()
        .filter(|symbol| {
            symbol.name == suffix
                || symbol.qualified_name.ends_with(callee_text)
                || symbol_suffix(&symbol.qualified_name) == suffix
        })
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
    let function = call
        .child_by_field_name("function")
        .or_else(|| call.named_child(0))?;
    Some(match function.kind() {
        "identifier" | "property_identifier" => node_text(function, source)?.to_owned(),
        "scoped_identifier" => node_text(function, source)?.replace(' ', ""),
        "field_expression" => function
            .child_by_field_name("field")
            .and_then(|field| node_text(field, source))
            .unwrap_or_else(|| node_text(function, source).unwrap_or(""))
            .to_owned(),
        "member_expression" | "member_access_expression" | "qualified_name" => {
            clean_expression_text(node_text(function, source)?)
        }
        _ => node_text(function, source)?.replace(' ', ""),
    })
}

fn qualified_name(
    file: &FileFacts,
    node: Node<'_>,
    source: &str,
    name: &str,
    _kind: SymbolKind,
) -> String {
    let mut parts = module_parts(&file.relative_path);
    parts.extend(container_parts(file.language, node, source));
    parts.push(name.to_owned());
    parts.join("::")
}

fn module_parts(relative_path: &str) -> Vec<String> {
    let trimmed = strip_supported_extension(relative_path);
    let mut parts: Vec<String> = trimmed
        .split('/')
        .filter(|part| {
            !matches!(
                *part,
                "src" | "lib" | "main" | "mod" | "index" | "Program" | "program"
            )
        })
        .map(str::to_owned)
        .collect();
    if parts.last().is_some_and(|part| part == "mod") {
        parts.pop();
    }
    parts
}

fn container_parts(language: Language, node: Node<'_>, source: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut parent = node.parent();
    while let Some(current) = parent {
        match language {
            Language::Rust if current.kind() == "impl_item" => {
                if let Some(type_name) = impl_type_name(current, source) {
                    parts.push(type_name);
                }
            }
            Language::CSharp
                if matches!(
                    current.kind(),
                    "namespace_declaration"
                        | "file_scoped_namespace_declaration"
                        | "class_declaration"
                        | "struct_declaration"
                        | "interface_declaration"
                        | "record_declaration"
                ) =>
            {
                if let Some(name) = named_node_text(current, source) {
                    parts.push(clean_expression_text(name));
                }
            }
            Language::JavaScript | Language::TypeScript
                if matches!(
                    current.kind(),
                    "class_declaration" | "class" | "abstract_class_declaration"
                ) =>
            {
                if let Some(name) = named_node_text(current, source) {
                    parts.push(clean_expression_text(name));
                }
            }
            _ => {}
        }
        parent = current.parent();
    }
    parts.reverse();
    parts
}

fn impl_type_name(node: Node<'_>, source: &str) -> Option<String> {
    if let Some(type_node) = node.child_by_field_name("type") {
        return node_text(type_node, source).map(clean_expression_text);
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind().contains("type") {
            return node_text(child, source).map(clean_expression_text);
        }
    }
    None
}

fn signature_text(node: Node<'_>, source: &str) -> Option<String> {
    let end = node
        .child_by_field_name("body")
        .or_else(|| node.child_by_field_name("value"))
        .map(|child| child.start_byte())
        .unwrap_or_else(|| {
            node.named_child(0)
                .map(|child| child.end_byte())
                .unwrap_or_else(|| node.end_byte())
        });
    let signature = &source[node.start_byte()..end.min(node.end_byte())];
    Some(signature.trim().to_owned()).filter(|signature| !signature.is_empty())
}

fn node_text<'a>(node: Node<'_>, source: &'a str) -> Option<&'a str> {
    node.utf8_text(source.as_bytes()).ok()
}

fn named_node_text<'a>(node: Node<'_>, source: &'a str) -> Option<&'a str> {
    node.child_by_field_name("name")
        .and_then(|name| node_text(name, source))
}

fn symbol_name(node: Node<'_>, source: &str) -> String {
    named_node_text(node, source)
        .or_else(|| {
            node.parent()
                .filter(|parent| parent.kind() == "variable_declarator")
                .and_then(|parent| named_node_text(parent, source))
        })
        .or_else(|| {
            node.child_by_field_name("property")
                .and_then(|property| node_text(property, source))
        })
        .map(clean_expression_text)
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "<anonymous>".to_owned())
}

fn value_is_function_like(node: Node<'_>) -> bool {
    let Some(value) = node.child_by_field_name("value") else {
        return false;
    };
    matches!(
        value.kind(),
        "arrow_function" | "function" | "function_expression" | "generator_function"
    )
}

fn is_call_expression(language: Language, node: Node<'_>) -> bool {
    match language {
        Language::CSharp => node.kind() == "invocation_expression",
        Language::JavaScript | Language::Rust | Language::TypeScript => {
            node.kind() == "call_expression"
        }
    }
}

fn symbol_suffix(text: &str) -> &str {
    text.rsplit([':', '.', '#'])
        .find(|part| !part.is_empty())
        .unwrap_or(text)
}

fn clean_expression_text(text: &str) -> String {
    text.split_whitespace().collect::<String>()
}

fn strip_supported_extension(relative_path: &str) -> &str {
    for suffix in [
        ".tsx", ".mts", ".cts", ".jsx", ".mjs", ".cjs", ".rs", ".cs", ".ts", ".js",
    ] {
        if let Some(stripped) = relative_path.strip_suffix(suffix) {
            return stripped;
        }
    }
    relative_path
}

fn is_tsx_path(relative_path: &str) -> bool {
    relative_path.ends_with(".tsx")
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
        ChunkKind, FileFacts, Language, ResolutionStatus, SymbolKind, extract_chunks,
        extract_rust_chunks, index_rust_file, index_source_file,
    };

    fn file() -> FileFacts {
        file_with_language("src/lib.rs", Language::Rust)
    }

    fn file_with_language(relative_path: &str, language: Language) -> FileFacts {
        FileFacts {
            id: "file-1".to_owned(),
            relative_path: relative_path.to_owned(),
            language,
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
    fn marks_sensitive_chunks_as_excluded_from_embedding() {
        let source =
            "pub fn token() {\n    let api_key = \"abcdefghijklmnopqrstuvwxyz123456\";\n}\n";

        let chunks = extract_rust_chunks(&file(), source).expect("chunks should parse");

        assert_eq!(chunks.len(), 1);
        assert_eq!(
            chunks[0].excluded_reason.as_deref(),
            Some("likely_credential_assignment")
        );
    }

    #[test]
    fn syntax_errors_emit_partial_index_with_diagnostics() {
        let source = "pub fn broken( {}\n";

        let index = index_rust_file(&file(), source).expect("syntax errors should index partially");

        assert!(!index.chunks.is_empty());
        assert!(!index.parse_diagnostics.is_empty());
        assert!(
            index
                .parse_diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message.contains("tree-sitter"))
        );
    }

    #[test]
    fn partial_parse_preserves_valid_functions_around_errors() {
        let source = r#"pub fn before() {}

pub fn broken( {}

pub fn after() {}
"#;

        let index = index_rust_file(&file(), source).expect("partial parse should succeed");

        assert!(!index.parse_diagnostics.is_empty());
        assert!(index.symbols.iter().any(|symbol| symbol.name == "before"));
        assert!(index.symbols.iter().any(|symbol| symbol.name == "after"));
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

    #[test]
    fn indexes_csharp_methods_and_invocations() {
        let source = r#"namespace Demo;

class Runner {
    void Helper() {}

    void Run() {
        Helper();
        Console.WriteLine("hi");
    }
}
"#;

        let file = file_with_language("src/Runner.cs", Language::CSharp);
        let index = index_source_file(&file, source).expect("C# should parse");

        assert_eq!(index.symbols.len(), 2);
        assert!(
            index
                .symbols
                .iter()
                .any(|symbol| symbol.qualified_name.ends_with("Runner::Run"))
        );
        let helper_call = index
            .calls
            .iter()
            .find(|call| call.callee_text == "Helper")
            .expect("helper invocation should be captured");
        assert_eq!(
            helper_call.resolution_status,
            ResolutionStatus::ResolvedLocalCandidate
        );
        assert!(helper_call.callee_symbol_id.is_some());
        assert!(
            index
                .calls
                .iter()
                .any(|call| call.callee_text == "Console.WriteLine"
                    && call.resolution_status == ResolutionStatus::Unresolved)
        );
    }

    #[test]
    fn indexes_javascript_functions_methods_and_calls() {
        let source = r#"function helper() {}

class Runner {
  run() {
    helper();
    service.execute();
  }
}
"#;

        let file = file_with_language("web/app.js", Language::JavaScript);
        let index = index_source_file(&file, source).expect("JavaScript should parse");

        assert!(
            index
                .symbols
                .iter()
                .any(|symbol| symbol.name == "helper" && symbol.kind == SymbolKind::Function)
        );
        assert!(
            index
                .symbols
                .iter()
                .any(|symbol| symbol.qualified_name.ends_with("Runner::run")
                    && symbol.kind == SymbolKind::Method)
        );
        assert!(index.calls.iter().any(|call| call.callee_text == "helper"
            && call.resolution_status == ResolutionStatus::ResolvedLocalCandidate));
        assert!(
            index
                .calls
                .iter()
                .any(|call| call.callee_text == "service.execute"
                    && call.resolution_status == ResolutionStatus::Unresolved)
        );
    }

    #[test]
    fn indexes_typescript_functions_and_arrow_declarators() {
        let source = r#"export function typed(input: string): string {
  return helper(input);
}

const helper = (value: string): string => value.trim();
"#;

        let file = file_with_language("web/util.ts", Language::TypeScript);
        let index = index_source_file(&file, source).expect("TypeScript should parse");

        assert!(
            index
                .symbols
                .iter()
                .any(|symbol| symbol.name == "typed" && symbol.kind == SymbolKind::Function)
        );
        assert!(
            index
                .symbols
                .iter()
                .any(|symbol| symbol.name == "helper" && symbol.kind == SymbolKind::Function)
        );
        assert!(index.calls.iter().any(|call| call.callee_text == "helper"
            && call.resolution_status == ResolutionStatus::ResolvedLocalCandidate));

        let chunks = extract_chunks(&file, source).expect("chunks should parse");
        assert_eq!(chunks.len(), 2);
    }

    #[test]
    fn indexes_active_language_fixtures() {
        for (fixture, relative_path, language, expected_symbols) in [
            (
                "csharp_basic/Program.cs",
                "Program.cs",
                Language::CSharp,
                vec!["Helper", "Run"],
            ),
            (
                "javascript_basic/src/app.js",
                "src/app.js",
                Language::JavaScript,
                vec!["helper", "run"],
            ),
            (
                "typescript_basic/src/util.ts",
                "src/util.ts",
                Language::TypeScript,
                vec!["typed", "helper"],
            ),
        ] {
            let source = std::fs::read_to_string(
                std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../../tests/fixtures")
                    .join(fixture),
            )
            .expect("fixture should be readable");
            let file = file_with_language(relative_path, language);
            let index = index_source_file(&file, &source).expect("fixture should parse");
            for expected in expected_symbols {
                assert!(
                    index.symbols.iter().any(|symbol| symbol.name == expected),
                    "{fixture} should index symbol {expected}"
                );
            }
            assert!(
                !index.chunks.is_empty(),
                "{fixture} should emit at least one chunk"
            );
            assert!(
                !index.calls.is_empty(),
                "{fixture} should emit at least one call edge"
            );
        }
    }
}

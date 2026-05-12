use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use tree_sitter::{Node, Parser, Point};

use crate::{
    parser::languages::LanguageKind,
    symbols::{
        model::{
            ExtractedFile, ImportRecord, ParseErrorRecord, ReferenceRecord, RelationshipRecord,
            SymbolRecord,
        },
        relationships::{CALLS, CONTAINS, IMPORTS, NAME_MATCH, STRUCTURAL},
    },
};

pub fn extract_file(
    relative_path: &str,
    content: &str,
    language: LanguageKind,
) -> Result<ExtractedFile> {
    let mut parser = Parser::new();
    parser.set_language(&language.tree_sitter_language())?;
    let tree = parser
        .parse(content, None)
        .context("Tree-sitter parser unavailable")?;
    let mut extracted = ExtractedFile::default();
    let mut stack = Vec::new();
    walk(
        tree.root_node(),
        relative_path,
        content,
        language,
        &mut extracted,
        &mut stack,
    );
    collect_parse_errors(tree.root_node(), &mut extracted.parse_errors);
    Ok(extracted)
}

fn walk(
    node: Node<'_>,
    relative_path: &str,
    content: &str,
    language: LanguageKind,
    extracted: &mut ExtractedFile,
    stack: &mut Vec<usize>,
) {
    if let Some(import) = import_record(node, content, language) {
        if let Some(source_index) = stack.last().copied() {
            extracted.relationships.push(RelationshipRecord {
                source_index: Some(source_index),
                target_name: import
                    .imported_symbol
                    .clone()
                    .or(import.imported_path.clone()),
                source_symbol_id: None,
                target_symbol_id: None,
                source_file_id: None,
                target_file_id: None,
                relationship_kind: IMPORTS.into(),
                confidence: NAME_MATCH.into(),
                evidence: Some(format!("{} imports {}", relative_path, import.import_text)),
            });
        }
        extracted.imports.push(import);
    }

    if let Some((name, start, end)) = call_reference(node, content) {
        extracted.references.push(ReferenceRecord {
            file_id: None,
            symbol_id: None,
            referenced_name: name.clone(),
            reference_kind: "call".into(),
            start_line: line(start),
            start_column: start.column as u32,
            end_line: line(end),
            end_column: end.column as u32,
        });
        if let Some(source_index) = stack.last().copied() {
            let source_name = extracted.symbols[source_index].name.clone();
            extracted.relationships.push(RelationshipRecord {
                source_index: Some(source_index),
                target_name: Some(name.clone()),
                source_symbol_id: None,
                target_symbol_id: None,
                source_file_id: None,
                target_file_id: None,
                relationship_kind: CALLS.into(),
                confidence: NAME_MATCH.into(),
                evidence: Some(format!(
                    "{source_name} calls {name} at {relative_path}:{}",
                    line(start)
                )),
            });
        }
    }

    let current_symbol = symbol_record(
        node,
        relative_path,
        content,
        language,
        stack.last().copied(),
    );
    if let Some(symbol) = current_symbol {
        let symbol_index = extracted.symbols.len();
        if let Some(parent_index) = symbol.parent_index {
            extracted.relationships.push(RelationshipRecord {
                source_index: Some(parent_index),
                target_name: Some(symbol.name.clone()),
                source_symbol_id: None,
                target_symbol_id: None,
                source_file_id: None,
                target_file_id: None,
                relationship_kind: CONTAINS.into(),
                confidence: STRUCTURAL.into(),
                evidence: Some(format!(
                    "{} contains {}",
                    extracted.symbols[parent_index].name, symbol.name
                )),
            });
        }
        extracted.symbols.push(symbol);
        stack.push(symbol_index);
        walk_children(node, relative_path, content, language, extracted, stack);
        stack.pop();
        return;
    }

    walk_children(node, relative_path, content, language, extracted, stack);
}

fn walk_children(
    node: Node<'_>,
    relative_path: &str,
    content: &str,
    language: LanguageKind,
    extracted: &mut ExtractedFile,
    stack: &mut Vec<usize>,
) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk(child, relative_path, content, language, extracted, stack);
    }
}

fn symbol_record(
    node: Node<'_>,
    relative_path: &str,
    content: &str,
    language: LanguageKind,
    parent_index: Option<usize>,
) -> Option<SymbolRecord> {
    let kind = symbol_kind(node, language)?;
    let name = node_name(node, content)?;
    let signature = signature(node, content);
    let visibility = visibility(&name, node, content, language);
    let start = node.start_position();
    let end = node.end_position();
    let symbol_hash = symbol_hash(
        relative_path,
        &name,
        &kind,
        node.start_byte(),
        node.end_byte(),
        signature.as_deref(),
    );
    Some(SymbolRecord {
        id: None,
        file_id: None,
        parent_index,
        parent_symbol_id: None,
        name,
        kind,
        language: language.id().into(),
        signature,
        visibility,
        start_line: line(start),
        start_column: start.column as u32,
        end_line: line(end),
        end_column: end.column as u32,
        start_byte: node.start_byte() as u32,
        end_byte: node.end_byte() as u32,
        symbol_hash,
    })
}

fn symbol_kind(node: Node<'_>, language: LanguageKind) -> Option<String> {
    let node_kind = node.kind();
    let kind = match language {
        LanguageKind::Rust => match node_kind {
            "function_item" if has_ancestor(node, "impl_item") => "method",
            "function_item" => "function",
            "struct_item" => "struct",
            "enum_item" => "enum",
            "trait_item" => "trait",
            "mod_item" => "module",
            "const_item" => "constant",
            "static_item" => "static",
            "type_item" => "type_alias",
            "macro_definition" => "macro",
            _ => return None,
        },
        LanguageKind::TypeScript => match node_kind {
            "function_declaration" | "generator_function_declaration" => "function",
            "method_definition" | "method_signature" => "method",
            "class_declaration" => "class",
            "interface_declaration" => "interface",
            "type_alias_declaration" => "type_alias",
            "internal_module" | "module" => "module",
            "lexical_declaration" | "variable_declarator" => "variable",
            "public_field_definition" | "property_signature" => "property",
            _ => return None,
        },
        LanguageKind::JavaScript => match node_kind {
            "function_declaration" | "generator_function_declaration" => "function",
            "method_definition" => "method",
            "class_declaration" => "class",
            "lexical_declaration" | "variable_declarator" => "variable",
            "public_field_definition" => "property",
            _ => return None,
        },
        LanguageKind::Python => match node_kind {
            "function_definition" => "function",
            "class_definition" => "class",
            _ => return None,
        },
    };
    Some(kind.into())
}

fn node_name(node: Node<'_>, content: &str) -> Option<String> {
    if let Some(name) = node.child_by_field_name("name") {
        return text(name, content).map(normalize_reference_name);
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if matches!(
            child.kind(),
            "identifier" | "type_identifier" | "property_identifier"
        ) {
            return text(child, content).map(normalize_reference_name);
        }
        if child.kind() == "variable_declarator"
            && let Some(name) = child.child_by_field_name("name")
        {
            return text(name, content).map(normalize_reference_name);
        }
    }
    None
}

fn import_record(node: Node<'_>, content: &str, language: LanguageKind) -> Option<ImportRecord> {
    let is_import = match language {
        LanguageKind::Rust => matches!(
            node.kind(),
            "use_declaration" | "extern_crate_declaration" | "mod_item"
        ),
        LanguageKind::TypeScript | LanguageKind::JavaScript => {
            matches!(node.kind(), "import_statement" | "export_statement")
        }
        LanguageKind::Python => matches!(node.kind(), "import_statement" | "import_from_statement"),
    };
    if !is_import {
        return None;
    }
    let import_text = text(node, content)?;
    let start = node.start_position();
    Some(ImportRecord {
        file_id: None,
        imported_path: import_path(node, content).or_else(|| Some(import_text.clone())),
        imported_symbol: node_name(node, content),
        import_text,
        start_line: line(start),
        start_column: start.column as u32,
    })
}

fn import_path(node: Node<'_>, content: &str) -> Option<String> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if matches!(
            child.kind(),
            "string" | "string_fragment" | "scoped_identifier" | "dotted_name"
        ) {
            return text(child, content).map(|value| value.trim_matches(['\'', '"']).to_string());
        }
    }
    None
}

fn call_reference(node: Node<'_>, content: &str) -> Option<(String, Point, Point)> {
    if node.kind() != "call_expression" {
        return None;
    }
    let function = node
        .child_by_field_name("function")
        .or_else(|| node.named_child(0))?;
    let name = text(function, content).map(normalize_reference_name)?;
    if name.is_empty() {
        return None;
    }
    Some((name, function.start_position(), function.end_position()))
}

fn collect_parse_errors(node: Node<'_>, errors: &mut Vec<ParseErrorRecord>) {
    if node.is_error() || node.is_missing() || node.kind() == "ERROR" {
        let start = node.start_position();
        let end = node.end_position();
        errors.push(ParseErrorRecord {
            file_id: None,
            parse_run_id: None,
            start_line: line(start),
            start_column: start.column as u32,
            end_line: Some(line(end)),
            end_column: Some(end.column as u32),
            message: format!("Tree-sitter parse error: {}", node.kind()),
        });
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_parse_errors(child, errors);
    }
}

fn text(node: Node<'_>, content: &str) -> Option<String> {
    node.utf8_text(content.as_bytes())
        .ok()
        .map(|value| value.trim().to_string())
}

fn signature(node: Node<'_>, content: &str) -> Option<String> {
    let raw = text(node, content)?;
    let first_line = raw.lines().next().unwrap_or_default().trim();
    if first_line.is_empty() {
        None
    } else {
        Some(
            first_line
                .trim_end_matches('{')
                .trim_end_matches(':')
                .trim()
                .chars()
                .take(240)
                .collect(),
        )
    }
}

fn visibility(name: &str, node: Node<'_>, content: &str, language: LanguageKind) -> Option<String> {
    let source = text(node, content).unwrap_or_default();
    match language {
        LanguageKind::Rust if source.trim_start().starts_with("pub") => Some("public".into()),
        LanguageKind::TypeScript | LanguageKind::JavaScript
            if source.trim_start().starts_with("export") =>
        {
            Some("public".into())
        }
        LanguageKind::TypeScript | LanguageKind::JavaScript if source.contains("private ") => {
            Some("private".into())
        }
        LanguageKind::Python if name.starts_with('_') => Some("private".into()),
        _ => None,
    }
}

fn normalize_reference_name(value: String) -> String {
    value
        .trim_matches(['\'', '"'])
        .split(['.', ':', '<', '(', '['])
        .next_back()
        .unwrap_or("")
        .trim_matches(':')
        .trim()
        .to_string()
}

fn has_ancestor(node: Node<'_>, kind: &str) -> bool {
    let mut current = node.parent();
    while let Some(parent) = current {
        if parent.kind() == kind {
            return true;
        }
        current = parent.parent();
    }
    false
}

fn line(point: Point) -> u32 {
    point.row as u32 + 1
}

fn symbol_hash(
    relative_path: &str,
    name: &str,
    kind: &str,
    start_byte: usize,
    end_byte: usize,
    signature: Option<&str>,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(relative_path.as_bytes());
    hasher.update(b"\0");
    hasher.update(name.as_bytes());
    hasher.update(b"\0");
    hasher.update(kind.as_bytes());
    hasher.update(b"\0");
    hasher.update(start_byte.to_string().as_bytes());
    hasher.update(b"\0");
    hasher.update(end_byte.to_string().as_bytes());
    hasher.update(b"\0");
    hasher.update(signature.unwrap_or_default().as_bytes());
    format!("{:x}", hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_rust_function() {
        let extracted = extract_file(
            "src/lib.rs",
            "pub fn parse_config() { read_file(); }",
            LanguageKind::Rust,
        )
        .unwrap();
        assert!(
            extracted
                .symbols
                .iter()
                .any(|symbol| symbol.name == "parse_config")
        );
        assert!(
            extracted
                .references
                .iter()
                .any(|reference| reference.referenced_name == "read_file")
        );
    }
}

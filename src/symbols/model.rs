#[derive(Debug, Clone)]
pub struct SymbolRecord {
    pub id: Option<i64>,
    pub file_id: Option<i64>,
    pub parent_index: Option<usize>,
    pub parent_symbol_id: Option<i64>,
    pub name: String,
    pub kind: String,
    pub language: String,
    pub signature: Option<String>,
    pub visibility: Option<String>,
    pub start_line: u32,
    pub start_column: u32,
    pub end_line: u32,
    pub end_column: u32,
    pub start_byte: u32,
    pub end_byte: u32,
    pub symbol_hash: String,
}

#[derive(Debug, Clone)]
pub struct ReferenceRecord {
    pub file_id: Option<i64>,
    pub symbol_id: Option<i64>,
    pub referenced_name: String,
    pub reference_kind: String,
    pub start_line: u32,
    pub start_column: u32,
    pub end_line: u32,
    pub end_column: u32,
}

#[derive(Debug, Clone)]
pub struct ImportRecord {
    pub file_id: Option<i64>,
    pub import_text: String,
    pub imported_path: Option<String>,
    pub imported_symbol: Option<String>,
    pub start_line: u32,
    pub start_column: u32,
}

#[derive(Debug, Clone)]
pub struct RelationshipRecord {
    pub source_index: Option<usize>,
    pub target_name: Option<String>,
    pub source_symbol_id: Option<i64>,
    pub target_symbol_id: Option<i64>,
    pub source_file_id: Option<i64>,
    pub target_file_id: Option<i64>,
    pub relationship_kind: String,
    pub confidence: String,
    pub evidence: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ParseErrorRecord {
    pub file_id: Option<i64>,
    pub parse_run_id: Option<i64>,
    pub start_line: u32,
    pub start_column: u32,
    pub end_line: Option<u32>,
    pub end_column: Option<u32>,
    pub message: String,
}

#[derive(Debug, Clone, Default)]
pub struct ExtractedFile {
    pub symbols: Vec<SymbolRecord>,
    pub references: Vec<ReferenceRecord>,
    pub imports: Vec<ImportRecord>,
    pub relationships: Vec<RelationshipRecord>,
    pub parse_errors: Vec<ParseErrorRecord>,
}

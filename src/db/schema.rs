#[derive(Debug, Clone)]
pub struct IndexStatus {
    pub repo_path: String,
    pub database_path: String,
    pub last_index_time: Option<String>,
    pub files_indexed: i64,
    pub symbols_indexed: i64,
    pub relationships_indexed: i64,
    pub parse_errors: i64,
    pub supported_languages: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct SymbolRow {
    pub id: i64,
    pub name: String,
    pub kind: String,
    pub language: String,
    pub file_path: String,
    pub start_line: i64,
    pub end_line: i64,
    pub signature: Option<String>,
    pub visibility: Option<String>,
    pub matched_by: String,
}

#[derive(Debug, Clone)]
pub struct ReferenceRow {
    pub referenced_name: String,
    pub reference_kind: String,
    pub file_path: String,
    pub start_line: i64,
    pub start_column: i64,
    pub symbol_name: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ImportRow {
    pub file_path: String,
    pub import_text: String,
    pub imported_path: Option<String>,
    pub imported_symbol: Option<String>,
    pub start_line: i64,
    pub start_column: i64,
}

#[derive(Debug, Clone)]
pub struct ErrorRow {
    pub file_path: String,
    pub start_line: i64,
    pub start_column: i64,
    pub message: String,
}

#[derive(Debug, Clone)]
pub struct RelationshipRow {
    pub source_name: Option<String>,
    pub target_name: Option<String>,
    pub source_file: String,
    pub target_file: Option<String>,
    pub relationship_kind: String,
    pub confidence: String,
    pub evidence: Option<String>,
}

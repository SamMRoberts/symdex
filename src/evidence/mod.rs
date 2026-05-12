#[derive(Debug, Clone)]
pub struct Evidence {
    pub claim_type: String,
    pub file_path: String,
    pub line_range: Option<(u32, u32)>,
    pub symbol_name: Option<String>,
    pub relationship_kind: Option<String>,
    pub source_table: String,
    pub source_record_id: Option<i64>,
}

impl Evidence {
    pub fn symbol(
        file_path: impl Into<String>,
        symbol_name: impl Into<String>,
        start: u32,
        end: u32,
    ) -> Self {
        Self {
            claim_type: "symbol_exists".into(),
            file_path: file_path.into(),
            line_range: Some((start, end)),
            symbol_name: Some(symbol_name.into()),
            relationship_kind: None,
            source_table: "symbols".into(),
            source_record_id: None,
        }
    }
}

//! Read-only MCP tool contract boundary.

pub const TOOL_SEARCH: &str = "symdex.search";
pub const TOOL_FIND_SYMBOL: &str = "symdex.find_symbol";
pub const TOOL_CALLERS: &str = "symdex.callers";
pub const TOOL_CALLEES: &str = "symdex.callees";
pub const TOOL_IMPACT: &str = "symdex.impact";

pub fn tool_names() -> [&'static str; 5] {
    [
        TOOL_SEARCH,
        TOOL_FIND_SYMBOL,
        TOOL_CALLERS,
        TOOL_CALLEES,
        TOOL_IMPACT,
    ]
}

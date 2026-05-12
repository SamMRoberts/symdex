# Parser Pipeline

The MVP detects language by extension, loads the matching Tree-sitter grammar, parses the file, records syntax errors, extracts symbols/imports/call references, and persists generated records in SQLite.
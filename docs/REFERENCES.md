# External References

Verified sources for the chosen stack.

## Parsers

- tree-sitter Rust bindings: https://docs.rs/tree-sitter
- tree-sitter project: https://github.com/tree-sitter/tree-sitter
- tree-sitter C# grammar: https://github.com/tree-sitter/tree-sitter-c-sharp
- tree-sitter JavaScript grammar: https://github.com/tree-sitter/tree-sitter-javascript
- tree-sitter TypeScript grammar: https://github.com/tree-sitter/tree-sitter-typescript

## Vector database

- Qdrant docs: https://qdrant.tech/documentation/
- Qdrant local quickstart: https://qdrant.tech/documentation/quickstart/
- Qdrant create collection API: https://api.qdrant.tech/api-reference/collections/create-collection
- Qdrant upsert points API: https://api.qdrant.tech/api-reference/points/upsert-points
- Qdrant query points API: https://api.qdrant.tech/api-reference/search/query-points
- Qdrant Rust client: https://docs.rs/qdrant-client

## Local embeddings

- Ollama API docs: https://docs.ollama.com/api
- Ollama embeddings docs: https://docs.ollama.com/capabilities/embeddings
- nomic-embed-text model: https://ollama.com/library/nomic-embed-text

## MCP

- MCP docs: https://modelcontextprotocol.io/
- MCP server tools spec: https://modelcontextprotocol.io/specification/2025-06-18/server/tools
- Rust SDK / rmcp: https://docs.rs/rmcp

## Terminal UI

- ratatui docs: https://docs.rs/ratatui
- crossterm docs: https://docs.rs/crossterm

## Maintenance rule

When updating crate versions, MCP behavior, TUI behavior, embedding models, or Qdrant collection behavior, re-check the relevant upstream docs and update this file if assumptions changed.

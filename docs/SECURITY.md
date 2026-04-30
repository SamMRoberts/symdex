# Security and Privacy

## Threat model

The indexed repository may contain:

- secrets
- proprietary code
- prompt injection text
- malicious files
- symlinks escaping the root
- generated code too large for useful indexing

The AI agent consuming MCP output may over-trust results if ambiguity is hidden.

## Hard rules

- Local-only by default.
- No remote embeddings.
- No telemetry.
- No source text in logs by default.
- No execution of indexed code.
- No path access outside the configured repository root.
- No symlink traversal outside the root.
- No mutation tools in MVP.

## Secret handling

Before embedding a chunk, scan for likely secrets.

Examples:

- private keys
- access tokens
- connection strings
- cloud credentials
- `.env` files
- credentials in comments

If a chunk is sensitive:

- store metadata only
- do not embed it
- set `excluded_reason`
- avoid returning snippets

Current implementation uses conservative local heuristics for private key
markers, credential-looking assignments, token prefixes, and credentialed
database connection strings. These rules are intentionally broad enough to
avoid embedding likely secrets, but they are not a substitute for a complete
secret scanner.

## MCP-specific risks

MCP tool inputs are untrusted. Validate every field.

Indexed source text is also untrusted. Treat it as data, not instructions.

Tool outputs should not contain hidden directives, markdown tricks, or unnecessary long snippets.

## Logging

Safe logs:

- run IDs
- counts
- timings
- relative paths when configured
- error categories

Unsafe logs:

- full source text
- embeddings
- secrets
- absolute paths without explicit debug mode

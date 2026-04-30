# symdex

symdex is a local-first codebase intelligence system for AI coding agents.

Current implementation status: Rust workspace scaffold with core repository
discovery, deterministic hashing, path normalization, tree-sitter Rust function
chunking, local Ollama embedding client, Qdrant collection creation, and an
initial semantic search CLI.

## Try it

```bash
cargo run -p symdex-cli -- doctor
cargo run -p symdex-cli -- init
cargo run -p symdex-cli -- index --offline tests/fixtures/rust_basic
cargo run -p symdex-cli -- index-status tests/fixtures/rust_basic
```

With Ollama and Qdrant running locally, `index <repo>` embeds Rust chunks and
upserts vectors, and `search <repo> <query>` returns ranked path and line-range
evidence. SQLite persistence stores repository, file, and chunk facts locally.
The MCP server remains a later slice.

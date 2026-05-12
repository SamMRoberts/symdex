# Project Discovery

Symdex resolves the repository root from `symdex.toml`, then `.git`, then the provided path. File discovery uses the `ignore` crate plus configured include/exclude globsets and skips files over the configured size limit.
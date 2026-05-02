# Symdex — Deep Dive Analysis Report

## Executive Summary

Symdex is a remarkably well-designed local-first codebase intelligence backend.
Its architecture is clean, its privacy model is principled, and its evidence
contracts are stable. Phases 1–15 are largely complete and the backlog reflects
real maturity. The primary opportunity now is bridging the gap between *evidence
metadata* and *actionable debugging intelligence* — the thing agents and chat
tools actually need when they're trying to fix code.

---

## What Is Good

### 1. Architecture and Crate Boundaries

The workspace layout is excellent. Nine crates with explicit dependency rules, no
circular dependencies, and a clear inward dependency flow:
`core → store/embed → index/query → cli/tui/mcp`. The contracts are enforced at
compile time — MCP cannot start indexing, TUI cannot shell out, core knows
nothing about Qdrant. This is production-quality crate discipline that most
greenfield projects never achieve.

The `symdex-core` crate is notably clean: pure domain logic with no I/O
dependencies. The `Language` enum plus `SymbolKind`, `ChunkKind`,
`ResolutionStatus`, and `CallEdge` form a minimal but expressive domain model.
The evidence contract schema constants living in `symdex-core` and shared by
CLI, TUI, and MCP is a smart design — a single source of truth prevents drift.

### 2. Evidence Contract Design

The `symdex.mcp.evidence.v1` envelope is designed exactly right for multi-agent
reuse:

- Versioned schema identifier
- Local-only / read-only flags baked into the envelope, not just the
  documentation
- `freshness`, `trust`, `reasons`, and `provenance` fields on every evidence row
- Compact output suitable for LLM context windows
- Source text deliberately omitted by default

The trust scoring model (0.0–1.0 heuristic over freshness + provenance
completeness + confidence + index metadata) is a useful ordering aid for agents,
even if it is not a correctness proof. The `reasons` tags
(`semantic_vector_match`, `relationship:direct_caller`,
`bounded_transitive_call_path`) are particularly valuable: they give agents a
machine-readable explanation of *why* a result was returned without requiring
source text.

### 3. Incremental and Continuous Indexing

The content-hash skip logic is correct and efficient. The conservative design —
same ignore rules, same path-boundary checks, same secret detection, same
hashing — for both manual and continuous indexing means the continuous path
cannot silently behave differently. The debounce and coalescing of filesystem
events before scheduling reindex work is the right approach for busy editors.
Index run summaries recording `run_kind` (manual vs. watch) give the TUI a clean
audit trail.

### 4. Call Graph — Conservative and Honest

The decision to preserve `unresolved` and `ambiguous` call edges as labeled
graph facts rather than dropping them is architecturally correct and rare. Most
tools either resolve confidently (and lie) or drop ambiguous edges (and lose
evidence). The four-state `ResolutionStatus` (`resolved_exact`,
`resolved_local_candidate`, `unresolved`, `ambiguous`) and the confidence scalar
give downstream agents and humans the information they need to decide how much to
trust a call edge.

The cross-file resolution pass for Rust — normalizing `crate::` prefixes, use
aliases, grouped imports, `self::`/`super::` calls from caller scope — is
impressively thorough without claiming type inference it does not do. The macro
invocation preservation as unresolved edges with diagnostics is exactly right.

### 5. Debug Context Pack

The `symdex_debug_context` MCP tool and `debug-context` CLI command are the most
directly useful features for troubleshooting. The input parser handles all the
common Rust runtime output shapes: `cargo test`, `RUST_BACKTRACE=1`,
`RUST_BACKTRACE=full`, panic-hook locations, `anyhow` cause chains (without
treating them as stack frames), `tracing` metadata lines, and async stack-like
output. The frame-to-SQLite join (files → symbols covering the line → calls
recorded at that line) is the right join, and the test-name mapping for indexed
Rust tests adds immediate value for CI failures.

### 6. Security and Privacy Model

No remote calls by default. No source text in embeddings, Qdrant payloads, logs,
or MCP responses. Path boundary enforcement in both discovery and MCP input
validation. Secret detection before embedding. Symlink escape rejection. Treating
indexed source text as untrusted data (not instructions) in the MCP section of
the security doc is a real insight. The `excluded_reason` column in `chunks`
persisting the *reason* a chunk was excluded (not just a boolean) is useful for
TUI inspection.

### 7. TUI Design

The nested-tab storage visualization architecture is sophisticated: SQLite as
structural source of truth, Qdrant as semantic projection, cross-store health
view surfacing mismatches. The two-container footer (shortcut hints above, status
messages below) is the right separation. The confirmation flow for long-running
jobs and the animated continuous-indexing indicator solve real UX problems. Using
`ratatui` test backends for render assertions is the correct testing approach for
TUI code.

### 8. Test Coverage Philosophy

The test pyramid is well-structured. Keeping Qdrant and Ollama checks behind
opt-in environment flags means CI is fast and reproducible without service
dependencies. The fixture-based approach for language parsers is correct: tiny,
purpose-built fixtures that test specific behaviors rather than large
repositories. The explicit list in `TESTING.md` of exactly what must be tested
(including the less obvious cases like `self.method()` resolution, cross-file
`super::` calls, async stack lines, and `anyhow` cause chains) is a strong
signal that this was designed for long-term maintainability.

---

## What Is Almost There

### 1. Call Resolution Quality Gap for Multi-Language

Rust call resolution is detailed and well-thought-out. C#, JavaScript, and
TypeScript are described as "conservative" — syntax-aware function/method chunks,
symbols, and call-like references, but not whole-language type inference. In
practice, for JS/TS especially, this means call resolution quality drops
significantly for:

- Dynamic dispatch (`.call()`, `.apply()`, `Proxy`, event handlers assigned to
  callbacks)
- Import aliasing beyond simple named imports
- Class inheritance chains (`super.method()` in subclasses)
- Module re-exports (`export { foo as bar } from './mod'`)

This is correctly acknowledged but not yet addressed. The gap matters because a
large portion of real debugging questions involve TS/JS codebases. The same
cross-file resolution pass that exists for Rust is missing for C#/JS/TS.

### 2. `symdex_context_pack` Is Structural Only

The context pack format (`symdex.context_pack.v1`) returns focus symbols, direct
callers, direct callees, and involved files. But it explicitly notes
`direct_relationships_only` and does not combine semantic search results. A
coding agent preparing to edit a function needs *both* the structural
neighborhood (who calls this, who it calls) and the semantically similar code
(other functions that do related things). The tool hints you should call
`symdex_search` separately to combine, but that is extra round-trips and manual
work for the agent. The two evidence types should be available in a single
enriched context pack.

### 3. Optional rust-analyzer Integration Is Planned But Not Wired

The readiness diagnostics exist and the enrichment planning (reporting eligible
file/symbol/call counts) is implemented. But the actual symbol and call fact
application from rust-analyzer output is marked as a remaining P2 item. This is
the right deferral — applying rust-analyzer facts correctly is complex — but it
means Rust call resolution quality currently tops out at the conservative local
heuristic level, missing trait implementations, generic instantiations, and
closure captures.

### 4. Impact Analysis Test Discovery Is Conservative Across Languages

`tests_likely` in the impact output lists indexed tests that directly call the
queried symbol through resolved call edges. Rust test attributes, C# NUnit,
xUnit, and MSTest attributes, and JavaScript/TypeScript Jest, Vitest, and Mocha
test calls are now persisted as test facts. JS/TS inline callback tests remain
metadata-only unless a named callback can be linked unambiguously to an indexed
symbol, so they are searchable but do not overclaim likely-test call coverage.
For a tool designed to help debug failing tests across languages, the remaining
gap is richer runtime parsing and stronger non-Rust call resolution rather than
the table write path itself.

### 5. The `staleness` Command Is Underexposed in MCP

The CLI has a `staleness` command that compares indexed content hashes against
current files and reports fresh/stale/deleted/missing/unknown states per-file or
scoped to a symbol's context. This is exactly the kind of information a coding
agent needs before deciding whether to trust returned evidence. But there is no
corresponding `symdex_staleness` MCP tool — the staleness information only
surfaces as fields embedded in other tool responses. An agent checking "is my
index fresh enough to trust before I start editing?" has no direct tool to call.

### 6. Config Model Is Environment-Variable Only

Configuration is entirely via environment variables (`SYMDEX_DB_PATH`,
`SYMDEX_QDRANT_URL`, `SYMDEX_OLLAMA_URL`, `SYMDEX_EMBED_MODEL`). This is fine
for single-repo local use, but multi-repo workflows — which cross-agent reuse
requires — need per-repo config or a config file so different repos can have
different embedding models or SQLite paths without re-setting env vars. The
backlog notes this as P3 but it blocks practical multi-repo agent setups.

### 7. Structured Logging Is Missing

The backlog flags this as P3. Currently there is no `tracing` integration, no
structured log output, and no metrics. For a tool running in continuous indexing
mode in the background while agents work, invisible failures are a serious
operational problem. An agent that calls `symdex_index_status` and gets back zero
chunks has no way to know whether the index failed silently or just has not run
yet.

### 8. Glob Pattern and Negation Support in `.gitignore`

This gap has been closed. Discovery now applies ordered, scoped, glob-aware
`.gitignore` rules with `*`, `**`, `?`, character classes, directory rules,
basename rules, nested scope, and `!` negation while preserving built-in hard
excludes and symlink boundary checks.

---

## New Features to Implement

The following are prioritized by the combination of user value for
debugging/troubleshooting and architectural fit with the existing local-first
design.

### Priority 1 — Enriched Context Pack (Unified Structural + Semantic Evidence)

**What:** Add a `unified` mode to `symdex_context_pack` (and the CLI equivalent)
that runs both the structural context pack query and a semantic search for the
target symbol in a single call, merging and deduplicating results. The merged
output should annotate each result with its evidence source (`structural`,
`semantic`, or `both`).

**Why:** This is the single biggest improvement to agent integration. Right now
agents need two round-trips and manual merging. The merged pack gives a coding
agent the complete picture of a function: what calls it, what it calls, and what
other code in the repository is semantically related. This is the minimal viable
"pre-edit briefing" for an agent.

**Fit:** `symdex-query` already has both `run_context_pack` and semantic search.
The merge is a new query function in `symdex-query` that the MCP tool calls. No
schema changes needed.

### Priority 2 — `symdex_staleness_check` MCP Tool

**What:** A new `symdex_staleness_check` tool that accepts a `repo` and optional
`symbol` or list of `paths`, and returns the freshness state for each indexed
file in scope: `fresh`, `stale`, `deleted`, `missing`, or `unknown`, along with
the indexed content hash and current content hash for stale files.

**Why:** Agents currently have no way to explicitly ask "should I trust the index
for this file?" before requesting a context pack or impact analysis. The evidence
rows include freshness labels, but an agent that wants to decide whether to
trigger reindexing first has no direct tool. A staleness check tool enables the
agent workflow: check staleness → request reindex if needed → then query.

**Fit:** The `staleness` CLI command already implements this logic. The MCP tool
is a thin wrapper following the same pattern as `symdex_index_status`.

### Priority 3 — `symdex_request_reindex` Write Tool (Scoped)

**What:** A single write-capable MCP tool that accepts a `repo` and optionally a
list of `paths` (or `force: true` for a full reindex), triggers offline
structural reindexing, and returns an `index_run_id` plus a status. It must not
trigger semantic (Qdrant/Ollama) indexing without explicit `semantic: true` from
a trusted caller.

**Why:** The current design is read-only MCP by rule. But the most common agent
debugging workflow is: *detect stale index → fix → re-query*. Without a reindex
tool, agents are stuck waiting for the user to manually reindex or for continuous
indexing to catch up. The `symdex.debug_context.v1` tool is much more useful
when the agent can guarantee the index is fresh.

**Design constraints (from existing security model):**

- Scope to a specific repo root (existing boundary enforcement applies)
- No source execution, no path escape, no remote calls — same rules as manual
  indexing
- Offline structural indexing only by default; semantic requires explicit opt-in
- Requires a new design doc before implementation per the existing rule
- Tool response must include the index run ID for freshness correlation in
  follow-up queries

### Priority 4 — Multi-Language Test Discovery (Implemented)

**What:** Extend the `DiscoveredTest` and test-discovery pipeline to cover:

- **C#**: NUnit `[Test]`/`[TestCase]`, xUnit `[Fact]`/`[Theory]`, MSTest
  `[TestMethod]`
- **TypeScript/JavaScript**: Jest `test()`/`it()`/`describe()` blocks, Vitest
  equivalents, Mocha `it()`/`describe()`

Store discovered tests in the `tests` table with the same schema (language slug,
framework, qualified name, optional symbol linkage, byte/line ranges,
provenance). `symdex_impact` surfaces `tests_likely` for any language when a
stored test has direct resolved call evidence.

**Why:** A large fraction of real debugging workflows start with "this test is
failing." If the agent cannot map the failing test name to indexed symbols and
call edges, the debug context pack is much less useful for JS/TS and C#
codebases.

**Fit:** The `tests` table schema is language-neutral. The tree-sitter grammars
for C#, JS, and TS are wired. The implemented path preserves metadata-only rows
for callback tests that cannot be linked safely.

### Priority 5 — Stack Trace Parser for C#, JS/TS, and Python (Partial)

**What:** Extend the runtime-to-source parser in `symdex_debug_context` to
handle:

- **C# stack frames**: `at Namespace.Class.Method(Type param) in Path.cs:line N`
- **Node.js/V8 stack frames**: `at Object.method (file.js:line:col)` and
  `at async function (file.ts:line:col)` from compiled TS
- **Basic Python** (future, but low-hanging fruit):
  `File "path.py", line N, in function_name`

**Why:** The current parser is excellent for Rust but blind to the most common
runtimes in web and enterprise codebases. A C# developer pasting an
`ApplicationException` stack trace into an agent gets back no indexed evidence.
This is a gap that directly limits usefulness for debugging multi-language
codebases.

**Fit:** The runtime parser is self-contained in `symdex-core`. Each language
adds a new regex/pattern set following the existing Rust parser structure. The
downstream join logic (frames → indexed files/symbols/calls) is language-agnostic
and requires no changes.

### Priority 6 — `symdex_explain_change` Tool (Pre-Edit Safety Check)

**What:** A new MCP tool that accepts a `repo` and a proposed change
specification — a list of `{ path, start_line, end_line, description }` tuples —
and returns a compact pre-edit safety report: which symbols are affected, who
calls them (direct + transitive), which tests likely cover them, whether the
evidence is fresh, and a trust score for the completeness of the analysis.

**Why:** This is the highest-value agent integration feature not yet designed.
When GitHub Copilot or Claude prepares to edit code, it currently guesses at
impact. A `symdex_explain_change` tool gives the agent deterministic,
evidence-grounded answers to "what will this change affect?" before writing a
single line. This directly addresses the original product question from
`APP_SPEC.md`: *"Which code is relevant to this requested change? What files and
tests are likely affected?"*

**Design:**

- Input: repo root + list of change targets (path + line range)
- Implementation: union of `impact` results for all symbols intersecting the
  specified line ranges, then deduplicate and rank by trust score
- Output: compact impact report with freshness, trust, and reason tags
- No source text, read-only, follows all existing MCP security rules
- Requires a design doc before implementation

### Priority 7 — Glob Pattern `.gitignore` Support

**What:** Implement glob pattern matching and negation rules in the `.gitignore`
discovery filter. Specifically: `**` matching across path segments, `?`
single-character wildcard, `[pattern]` character classes, and negation `!rule`
to un-exclude paths.

**Why:** This is a correctness gap. Real repositories with generated code,
vendored dependencies, or intentionally excluded test fixtures rely on
glob-style ignore rules. Until this is fixed, the index either includes files it
should not (wrong evidence) or excludes files it should index (missing evidence).
Both outcomes undermine trust in the tool's results.

**Fit:** The discovery logic is in `symdex-core/src/discovery.rs`. A glob
library (`globset` is the idiomatic Rust choice) can replace the current
hand-rolled simple rule matching without changing any downstream contracts.

### Priority 8 — `symdex_semantic_neighborhood` MCP Tool

**What:** A new MCP tool that accepts a `repo`, a `chunk_id` or `symbol`, and a
`limit`, and returns the N nearest semantic neighbors from Qdrant — the code
chunks most similar to the given function/method, ranked by vector similarity.
Output includes path, line range, symbol name, chunk kind, score, freshness, and
provenance. No source text.

**Why:** Semantic search answers "what code is related to this query string?" but
agents need "what code is similar to *this specific function*?" when they are
trying to understand whether a pattern is duplicated elsewhere, find related
implementations to compare against, or locate the origin of a pattern they want
to refactor. The TUI already has a semantic neighborhood view using Qdrant
metadata — this is the MCP-exposed equivalent.

**Fit:** The Qdrant query infrastructure and the `qdrant_collection_name`
function already exist. This tool adds a new query path where the vector comes
from an existing Qdrant point (looked up by `chunk_id`) rather than from
embedding a text query.

### Priority 9 — Cross-Repo Context (Future Architecture)

**What:** Extend the `RepoRoot` concept to support a "workspace" or "multi-repo"
configuration where a single symdex instance can answer cross-repository
questions: "which other indexed repositories define a function with this
signature?" or "where else in my monorepo does this pattern appear?"

**Why:** Many real-world debugging problems span repositories — a shared library
version mismatch, a microservice API contract, a shared type definition.
Single-repo limitation is the correct MVP constraint, but the architecture should
plan for this now rather than needing a structural refactor later.

**Design notes:**

- Keep repository isolation as the default
- A multi-repo query adds a "search across all indexed repos" mode to
  `symdex_search` and `symdex_find_symbol`
- The `repository_id` filtering in Qdrant payloads already makes this possible
  at the query level — removing the filter enables cross-repo search
- Privacy: multi-repo queries require explicit opt-in; never mix results across
  repos silently

---

## Summary Table

| Area | Status | Priority |
|---|---|---|
| Core architecture and crate boundaries | ✅ Excellent | — |
| Evidence contracts (MCP envelope, versioning) | ✅ Excellent | — |
| Rust indexing + call resolution | ✅ Strong | — |
| Debug context pack (Rust) | ✅ Strong | — |
| Continuous indexing | ✅ Good | — |
| TUI storage visualization | ✅ Good | — |
| Secret detection | ✅ Adequate | — |
| C#/JS/TS call resolution depth | 🟡 Conservative only | P2 |
| Context pack (structural only) | 🟡 Needs semantic merge | P1 |
| rust-analyzer fact application | 🟡 Planned, not wired | P2 |
| Multi-language test discovery | ✅ Implemented conservatively | — |
| `staleness_check` MCP tool | 🟡 Exists in CLI, not MCP | P1 |
| `.gitignore` glob patterns | ✅ Implemented | — |
| Structured logging / metrics | 🔴 Missing | P3 |
| Config file / per-repo config | 🔴 Env-var only | P3 |
| Unified context pack (struct+semantic) | 🆕 New | P1 |
| `symdex_request_reindex` write tool | 🆕 New | P1 |
| Stack trace parsing for C#/Node | 🆕 New | P2 |
| `symdex_explain_change` (pre-edit safety) | 🆕 New | P1 |
| `symdex_semantic_neighborhood` MCP tool | 🆕 New | P2 |
| Cross-repo / multi-repo context | 🆕 Future | P3 |

---

## Recommended Next Session Order

1. **Unified context pack** — highest immediate value for agent integration, no
   schema changes, pure query layer
2. **`symdex_staleness_check` MCP tool** — thin wrapper over existing CLI logic,
   closes a real agent workflow gap
3. **`.gitignore` glob patterns** — correctness fix, affects every repository
   that uses patterns
4. **C#/Node.js stack trace parsing** — pure parser addition, no schema changes
5. **Design doc for `symdex_request_reindex`** — needs design before code, but
   should be next write-capable tool
6. **Design doc for `symdex_explain_change`** — most powerful future agent
   integration feature

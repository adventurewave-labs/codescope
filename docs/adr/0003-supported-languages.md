# ADR-0003: Supported Languages (v1)

**Status:** Accepted

**Date:** 2026-05-28

## Context

`codescope`'s value scales with how many of its users' repositories it can actually index. But every supported language adds a grammar to bundle, a set of extraction queries to author and maintain, and a surface for resolution bugs. The PRD's wedge is AI agent power users reached through the MCP ecosystem, so language priority should follow where those agents do the most work and where tree-sitter grammars are most mature and stable.

The roadmap also sequences this: the Phase 0 spike is Rust-only to prove the speed claim, TypeScript/JavaScript and Python land in the Phase 1 MVP/launch for reach, and Go arrives in Phase 2.

## Decision

Support four languages in v1: **Rust, TypeScript/JavaScript, Python, and Go.** Each is integrated through its mature tree-sitter grammar plus a set of `codescope`-authored tree-sitter **query files (`.scm`)** that capture that language's definitions (functions, structs/classes, traits/interfaces, types, modules) and structural edges (calls, references, imports, contains, defines).

TypeScript and JavaScript are treated as one language family (shared grammar lineage, shared extraction queries with TS-specific additions). Languages roll out per the roadmap: Rust first (spike), TS/JS + Python at launch, Go in Phase 2.

## Consequences

**Positive**
- These four cover the overwhelming majority of AI-agent users while keeping the maintained grammar/query set small.
- All four have mature, well-tested tree-sitter grammars and strong tree-sitter-graph support, lowering extraction risk.
- `.scm`-driven extraction means a fifth language is mostly a new grammar plus a query file, not new engine code.

**Negative / tradeoffs**
- Dynamic languages (Python, JavaScript) have weaker static resolvability; Tier 1 heuristic resolution will be less precise there, which is why every result is confidence-labeled and Tier 2 SCIP is planned (ADR-0004).
- Notable absences (Java, C/C++, C#, Ruby, PHP) will disappoint some users until later phases.
- The TS/JS family carries dialect breadth (JSX/TSX, decorators, module systems) that the queries must handle, raising maintenance cost for that one family.

## Alternatives Considered

- **Rust-only for v1:** simplest and lowest-risk, and great for Rust-community credibility, but the PRD explicitly wants TS/Python *in the launch* for adoption. Kept only as the Phase 0 spike.
- **Maximal coverage (10+ languages day one):** broad appeal but spreads authoring/maintenance thin and dilutes resolution quality, undermining the "structurally precise by default" promise. Rejected.
- **Include Java/C++ in v1 instead of Go:** large user bases, but Go is simpler to resolve, has a clean grammar, and aligns with the agent/backend user base; Java/C++ deferred.

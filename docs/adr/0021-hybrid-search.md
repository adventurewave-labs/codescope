# ADR-0021: Hybrid Natural-Language Symbol Search (BM25F + PageRank, RRF)

**Status:** Accepted. This is the first tier of ADR-0012's optional semantic layer.

**Date:** 2026-09-26

## Context

Every query surface so far assumed the agent already knew a name. The most
common agent question, though, is "where is the code that does X?". The usual
answers are grep, which misses renamed or abbreviated identifiers, or
embeddings, which need a model, a download or a network call and ship
multi-hundred-MB dependencies. ADR-0012 kept semantics optional and opt-in.
There is a strong zero-dependency middle ground: lexical retrieval tuned for
code, fused with the structural signal we already compute.

## Decision

- **Docs are extracted.** Each symbol gets its leading doc comment or
  docstring:
  - Comment siblings directly above the definition. Rust attributes,
    decorators and annotations are skipped, and a blank line breaks the
    attachment.
  - Python docstrings.
  - The file's module doc (Rust `//!`, Python module docstring, Go package
    comment), stored on the file's module symbol.
  - Comment markers are stripped and the text is capped at 320 chars. Schema
    version bumped to 3.
- **Tokenization**
  - camelCase, snake_case, acronym runs (`HTTPServer` → `http server`) and
    digit boundaries are split.
  - Terms are lowercased and stopwords (English plus code keywords) dropped.
  - A consistency-first suffix stemmer maps `parse`, `parses`, `parsing` and
    `parsed` to the same stem.
- **BM25F** (k1 = 1.2, b = 0.75) over weighted fields:

  | Field | Weight |
  |---|---|
  | name | 4 |
  | owner | 2 |
  | doc | 1.5 |
  | signature (minus the name) | 1 |
  | path | 0.75 |
  | file module doc | 0.3 |

  Two adjustments:
  - **Coverage bonus:** up to ×1.5 for matching more distinct query terms, so
    one rare term can't dominate.
  - **Test code is down-weighted** (×0.4) unless the query mentions tests.
- **Synonyms.** About 25 code-vocabulary groups (remove/delete/purge,
  get/fetch/load, resolve/bind, reindex/rebuild, …). They are stemmed at load
  and weighted 0.5 relative to the original terms.
- **Fusion.** Reciprocal rank fusion (k = 60) of the BM25 ranking (w = 1.0) and
  the PageRank ranking restricted to the lexical hits (w = 0.35). Rank fusion
  needs no calibration between the two scores; centrality only reorders
  lexical matches and never adds non-matches.
- **Filters.** Structural filters (`kind:`, `lang:`, `file:`, `owner:`,
  `calls:`, `returns:`) can be mixed into the query text.
- **Surfaces**
  - `codescope find "<query>"` and MCP `cs_find`.
  - Results carry `score` (top = 1.0), `matched` terms and `doc`.
  - MCP caches the index per `LiveGraph` generation, rebuilding only after a
    refresh changed the graph.
- The `semantic` cargo feature stays reserved for an embedding tier. It would
  fuse into the same RRF as a third ranking.

## Evaluation

`tests/find_relevance.rs` runs 16 behavior-phrased queries against codescope's
own source. Most queries don't contain the target's name, e.g. "turn a unified
diff into changed line ranges" → `parse_unified_diff`.

| Configuration | MRR | R@1 | R@5 |
|---|---|---|---|
| BM25F + synonyms + RRF | 0.644 | 0.56 | 0.75 |
| + file-module docs, test down-weight, 4 synonym groups | **0.693** | 0.56 | **0.88** |

The test fails if MRR < 0.6 or R@5 < 0.8. The one remaining miss
(`resolve_one`) has a doc comment that describes its return value, not its
purpose, which is a limit of lexical retrieval an embedding tier would address.

Spot checks on external repos:

| Query | Repo | Result |
|---|---|---|
| "reconnect to the server after connection loss" | hiredis | `redisReconnect` #1 |
| "define a route for GET requests" | sinatra | `Base::get` #1 |
| "register a custom type adapter" | gson | `GsonBuilder::registerTypeAdapter` #2 |

Cost of `find` in the CLI, including load, freshness check and index build
(parallel): 115 ms on gson (4.5k symbols) and 690 ms on dotnet/samples (25k
symbols).

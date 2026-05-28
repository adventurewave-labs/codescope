# ADR-0012: Optional Semantic Layer

**Status:** Accepted

**Date:** 2026-05-28

## Context

The incumbent MCP code-indexers are embeddings-first: they lean on vector similarity plus a separate vector DB (ChromaDB, Milvus) and return *approximate* matches. `codescope`'s entire differentiation is the opposite — structural precision by default, from AST + symbol resolution. But there are genuine queries that structure alone answers poorly: "find code that does X conceptually," fuzzy natural-language search over a repo, or ranking candidates when no exact structural match exists. We want to serve those cases without becoming the thing we are differentiating against.

## Decision

Provide an **optional semantic layer**: local embeddings computed via **ONNX (the `ort` crate)**, stored **alongside the graph** in the same embedded store. It is **strictly additive and OFF by default**, gated behind a Cargo **feature flag `semantic`**. It is **queried only when a structural query is insufficient** (no/low structural matches), never as the primary path.

Crucially, embeddings are **not the foundation**: the structural graph is the source of truth, and semantic results are a fallback/augmentation that is clearly distinguishable from precise structural answers.

## Consequences

**Positive**
- Preserves the precision-first identity and the single-binary, no-external-DB promise: embeddings live in the same local store, run locally via ONNX, and require no vector DB or cloud.
- Builds with `--features semantic` only; the default binary stays lean, fast to compile, and free of ML dependencies for users who never need fuzzy search.
- Gives a graceful answer to "conceptual" queries without compromising the structural core, and keeps `codescope` ahead of embeddings-only tools on both precision and footprint.

**Negative / tradeoffs**
- An optional feature is an extra build configuration to test and document; CI must cover both `default` and `--features semantic`.
- Bundling/loading an ONNX model adds binary size and a model-provenance/licensing consideration when the feature is enabled.
- Two retrieval modes (structural vs. semantic) require clear result labeling so agents never mistake an approximate match for a precise one.

## Alternatives Considered

- **Embeddings-first architecture (incumbent shape):** maximizes fuzzy recall but sacrifices precision and the install/footprint advantage; it is exactly what we differentiate against. Rejected as the foundation.
- **No semantic layer at all:** simplest and purest, but leaves genuinely conceptual queries unanswerable. Rejected in favor of an off-by-default option.
- **Calling a remote embeddings API:** avoids local model weight, but breaks local-first/private operation and the zero-cloud promise. Rejected.
- **On by default behind config:** would slow the common path and dilute the precision-first message. Rejected; opt-in only.

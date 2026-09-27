# ADR-0018: Owner- and Receiver-Aware Heuristic Resolution

**Status:** Accepted (refines ADR-0004 Tier 1)

**Date:** 2026-09-26

## Context

Tier-1 resolution bound every call by bare name, preferring same-file matches.
Dogfooding showed the cost: `items.push(x)` on a `Vec` bound to the repo's
`Budget::push`, `Vec::new()` to whichever `new` sat in the same file, and Rust
`impl` methods were indistinguishable from free functions. Callers/blast-radius
answers were polluted with confident-looking false edges.

## Decision

**Extraction**
- Rust `impl [Trait for] Type` blocks are scopes: inner fns become `Method`s with
  `owner = Type`, container = the type's symbol when declared in the same file.
  Trait method signatures are extracted as methods of the trait.
- Python/JS/TS functions lexically inside a class, and Go methods (receiver
  type), get `kind = Method` and an `owner`.
- `impl Trait for Type { fn m }` emits `m —Defines→ Trait::m`.
- Every call site records its **qualifier** (receiver / path head).
- **Local receiver-type inference:** a variable qualifier is replaced by its
  type when evident in the enclosing definition — parameters/annotations
  (`g: &CodeGraph`, Go `(s *Server)`) and constructors (`Budget::new(…)`,
  `Repo(…)`, `new Client(…)`, `&Server{}`, `NewServer(…)`).

**Scoring** (best wins; ties counted into `Edge.alternatives`)

| Evidence | Score |
|---|---|
| owner match (`self.m` in `T` → `T::m`; `T::m`; inferred receiver type) | +16 |
| module qualifier (`store::open` → `store.rs`, Go package dir) | +8 |
| same file | +4 |
| caller's file imports the candidate's module (dir-only: +2) | +3 |
| bare call → free function | +2 |
| base | +1 |

**Abstention.** A call through an un-typed variable binds only with ≥ module
evidence; a call through a *typed* receiver whose type owns none of the
candidates (e.g. `Vec`, `String`) stays unbound. Unbound is better than wrong:
it is honestly "external".

**Surfaces.** `SymbolView.owner`, `ambiguity` on reference hits,
`Owner::name` / `module::name` query targets, `owner:` (alias `in:`) structural
filter, `repo_summary.resolution` stats, blast-radius/diff-impact follow
`Defines` (changing a trait method reaches its impls).

**Index compatibility.** Records carry new fields; the store keys compatibility
on `schema + crate version` and rebuilds on mismatch instead of failing to
decode or silently serving stale extraction.

## Consequences

- On codescope itself, `callers Budget::push` went from 17 results (11 false,
  via `Vec::push`) to exactly the 5 true callers.
- Recall drops for calls through fields (`self.store.put()`) whose type isn't
  locally evident; struct-field type tracking is the natural next step, with
  SCIP (Tier 2) as the precise ceiling.

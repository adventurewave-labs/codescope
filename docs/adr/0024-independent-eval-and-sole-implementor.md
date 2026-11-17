# ADR-0024: Independent eval corpus + sole-implementor resolution

- Status: Accepted
- Date: 2026-11-17
- Supersedes: none (extends ADR-0018 / ADR-0022 resolution scoring)

## Context

ADR-0022 established the accuracy eval (`tests/eval/`, `scripts/eval`) and
published P 1.00 / R 0.97. An independent reproduction — a fresh 11-fixture,
77-site corpus authored outside the repo (`@gt` annotations frozen before any
tool run, scored through the CLI binary only) — confirmed precision but
measured **R 0.88** on shapes the in-repo corpus does not contain:

- **Interface-typed receivers produced no call edge at all.** In their own
  fixtures every dispatch receiver is concrete-typed (`new Repo()`,
  `*Store`, `Engine*`). With a receiver typed as an interface — TS field
  `gateway: Gateway`, Go interface value `m Manifester`, C++ `Actuator*` —
  the typed-receiver rule (ADR-0018) abstained because the interface owns no
  extracted method (TS/Go/C++ interface members are not extracted) and no
  candidate matched its owner. Rust (`dyn Shape`) and Java (`Stock`) were
  unaffected: they extract the declared trait/interface method, so the owner
  rule binds the declaration.
- **Ruby paren-less calls inside binary expressions were not extracted.**
  `total * tax_rate` — the query captured bare identifiers only in statement
  positions; binary operands fell through.
- Their own corpus, re-run from a clean checkout, already carried two
  misses of the same receiver family (`a.st.Put`, `Store::open().put()`),
  absorbed into the published 0.97.
- A guard probe found `Math.round(x)` binding to an unrelated in-repo
  ownerless `round`: the typed-receiver abstention only protected calls
  whose candidates had owners.

## Decision

1. **Sole-implementor fallback.** For a `Calls` edge whose qualifier is a
   typed receiver (uppercase, non-self) and whose best candidate scores
   below the owner-match threshold: if the qualifier names a type declared
   in this repo that owns no callable with this name, and **exactly one**
   in-repo owner provides the method, bind to it (`Heuristic`). Two or more
   implementors stay unbound — that is genuine dynamic dispatch the
   heuristics must not guess. External type names (`Math`, `Vec`) never
   qualify: the type must itself be a repo symbol.
2. **Ruby binary operands.** The right operand of a `binary` expression is
   captured as a call site. The left operand is not (it is usually the
   accumulator/variable). Bare captures that name a parameter, an assigned
   local (`x = …` on either side), or a block parameter of the enclosing def
   are dropped at extraction: they are reads, and a same-name in-repo method
   would otherwise capture them as false call edges.
3. **External-type evidence floor.** An uppercase qualifier naming no
   in-repo symbol is now subject to the same `MIN_RECEIVER_SCORE` floor as
   variable receivers, closing the ownerless-candidate capture hole.
4. **The corpus ships.** `tests/eval-independent/` + `tests/resolution_eval_independent.rs`
   (gates P 0.95 / R 0.95) keep the externally-authored fixtures in CI;
   `scripts/eval` runs both corpora and writes `docs/EVAL.md` /
   `docs/EVAL-INDEPENDENT.md`.

## Consequences

- Independent corpus: 77 sites, **P 0.98 / R 1.00** (was 0.97/0.88 on
  0.3.0). The single remaining FP is a convention site: Java binds
  `Stock::pick` to the interface *declaration* (matching the declared-type
  convention the rust fixture encodes via `Shape::area`), while the corpus
  annotates the executable target (`LocalStock::pick`). Both are defensible;
  the site is kept as a documented reminder, not normalized away.
- The original corpus is unchanged: 85 sites, P 1.00 / R 0.97 (its two
  pre-existing receiver misses are unrelated to this decision and remain
  tracked as misses in `docs/EVAL.md`).
- Extraction filters binary right-operand captures against the enclosing
  def's parameters, assigned locals, and block parameters; names outside
  those sets that are still plain reads (e.g. shadowed in deeper blocks)
  produce unbound edges, which resolution refuses to bind without evidence.
- Future work: record `implements`/`extends` edges at extraction time so
  the fallback can require a declared subtype relation instead of sole-owner
  ambiguity; extract TS/Go/C++ interface member declarations so the
  declared-type convention applies everywhere.

## Verification

- `cargo test` — original eval, independent eval, unit tests for sole
  implementor (binds / multi-impl abstains / external never captures) and
  ruby binary extraction.
- The external CLI reproduction harness lives outside the repo
  (`codescope-eval/independent-2026-11-17/` in the rig workspace): frozen
  corpus, Python CLI-only scorer, per-run result JSONs.

# ADR-0022: Accuracy Eval Harness & Release Pipeline

**Status:** Accepted (extends ADR-0014)

**Date:** 2026-09-26

## Context

Loops 0–4 changed resolution heavily (owners, receivers, imports, implicit
receivers, abstention). Before this ADR, "better" was argued from anecdotes and
aggregate bound-rates on real repos. A bound-rate can rise while precision
falls. Users also had to build from source.

## Decision

**1. Golden call-edge fixtures** in `tests/eval/<language>/`, one small
multi-file project per language (10). Call sites carry inline annotations
next to the code they describe:

```text
s.put();        // @eval put=Store::put                (must bind here)
store::open();  // @eval open=open@src/store.rs        (qualified name + file)
v.len();        // @eval len=-                          (external: must stay unbound)
```

The fixtures deliberately include traps:
- same-named methods on different types across files;
- module- vs. type-qualified calls;
- `self`/`this` and implicit-receiver calls;
- class fields as receivers;
- trait objects;
- instantiation without an explicit constructor;
- std-lib calls that must not bind to a same-named repo symbol.

**2. Scoring** (`tests/resolution_eval.rs`):

| Outcome | Meaning |
|---|---|
| TP | bound to the expected symbol |
| FP | wrong symbol, or bound when external |
| FN | in-repo target left unbound or not extracted |
| TN | external call correctly left unbound |

It reports per-language and overall precision, recall, F1 and accuracy, and
lists every miss. **Regression gates** (precision ≥ 0.97, recall ≥ 0.92) run
in plain `cargo test`, so CI enforces them.

**3. `scripts/eval`** runs the resolution eval and the `find` relevance eval
(ADR-0021) in release mode and writes `docs/EVAL.md`. `--check` prints only.

**4. Release workflow** (`.github/workflows/release.yml`), triggered by a
`v*` tag or manual dispatch:
- `verify` runs fmt, clippy (`-D warnings`), tests and the eval gates.
- `build` produces `--locked` release binaries for
  `x86_64/aarch64-unknown-linux-musl` (static), `aarch64/x86_64-apple-darwin`
  and `x86_64-pc-windows-msvc`. Each binary is smoke-tested (`--version`,
  `index`, `find`) and packaged (tar.gz/zip) with a `.sha256`.
- `publish` creates or updates the GitHub Release via the preinstalled `gh`
  CLI.
- The workflow uses first-party actions only, matching `ci.yml`.

## Results

Writing an honest suite surfaced gaps on its first run: 85 call sites scored
precision 1.00, recall **0.78**. Four general fixes (not per-fixture tweaks)
followed:

1. Capitalized calls with no callable candidate bind to the class or struct:
   `Repo()`, `new Repo()`, Ruby `Repo.new`.
2. JS/TS `new X()` call sites are extracted.
3. Receiver-type inference falls back to enclosing containers, so class field
   declarations count.
4. `&dyn Trait` / `impl Trait` annotations resolve to the trait. A module
   qualifier also prefers the module's free functions over its same-named
   methods.

After the fixes: **precision 1.00, recall 0.97, F1 0.98**. Real repos
improved in the same direction: gson ambiguous calls went from 289 to 175, fmt
from 1,595 to 901, and more calls were bound.

Two known misses remain, left in the suite on purpose:
- the return type of a chained call (`Store::open().put()`);
- Go struct-field types (`a.st.Put()`).

Both are the next resolution steps.

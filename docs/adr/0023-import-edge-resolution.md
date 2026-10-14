# ADR-0023: Import-edge resolution

- Status: Accepted
- Date: 2026-10-14
- Supersedes: none (extends ADR-0004 / ADR-0018 resolution scoring)

## Context

`deps` (and `cs_dependency_graph`) advertises cycle detection, but the DFS ran
over an adjacency built only from import edges whose `to` end had resolved to a
symbol. In practice almost nothing qualified:

- C/C++ `#include "b.h"` produced `to_name = "b.h"`; the generic name
  normalizer takes the last `.`-segment, so it looked for a symbol named `h`.
- TS/JS relative imports `"./b"` normalized to `/b` (the leading `.` is split
  as a qualifier dot).
- In polyglot repos, a bare module import (`import b` in Python) could tie
  between same-named modules in other languages and bind cross-language
  (`py/a.py`'s import binding the module of `cpp/b.hpp`).
- Conversely, external qualified paths (`std::process::Command`) could be
  captured by an in-repo namesake (`enum Command`), which contradicts the
  resolver's stated "abstain rather than guess" posture.

Result: `cycles` was effectively Python-only, and import edges were the least
trustworthy part of the graph.

## Decision

Import edges get their own resolution path, separate from call-edge scoring:

1. **File-naming imports** — the import string carries a code extension
   (`"b.h"`, `"./b.ts"`). It binds the module symbol of the file with that
   exact basename (same-directory files win ties). `#include <stdio.h>` stays
   unbound unless a `stdio.h` exists in the repo.
2. **Module-naming imports** — `b`, `./b`, `app.store.Repo`,
   `crate::fresh::LiveGraph`. Two guards:
   - **Language families.** Candidates outside the caller's language family
     are never considered (JS⇄TS and C⇄C++ are one family each; a Python
     import can never mean a C++ header).
   - **In-repo evidence for qualified paths.** A multi-segment path binds only
     when a parent segment names an in-repo module or directory (or is
     `crate`/`self`/`super`). `std::process::Command` and `java.util.List`
     therefore stay unbound; `crate::fresh::LiveGraph` and `app.store.Repo`
     bind. Relative imports (`./x`, `../x`) are in-repo by construction.

Import edges prefer module-kind targets (+6 in scoring); same-directory
candidates get +4.

## Consequences

- Cycle detection now works for C/C++ header guards and JS/TS circular
  imports, the two cases it was advertised for, and keeps working for
  Python/Ruby module cycles.
- Polyglot repos no longer cross-bind imports between languages.
- External qualified imports stay unbound instead of capturing in-repo
  namesakes; `summary`'s "call resolution" bound-count on codescope's own
  source went 629/2950 → 660/3052 (bound, honest edges only).
- `find` relevance improved as a side effect (MRR 0.693 → 0.730): resolved
  import edges feed import-aware scoring and centrality.
- The 85-site call-resolution eval is unchanged (1.00 / 0.97 / 0.98) — call
  edges take the pre-existing scoring path untouched.
- Known limits: Rust glob imports (`use a::{b, c}`) stay unbound (the
  normalized name is a brace expression); Go package imports bind only when a
  file stem matches the package name.

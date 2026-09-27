# ADR-0019: Java, C, C++, C#, Ruby

**Status:** Accepted (extends ADR-0003)

**Date:** 2026-09-26

## Decision

Add five tree-sitter grammars. All are pinned to **ABI 14**, the maximum that
tree-sitter 0.24 accepts:

| Language | Crate | Extensions |
|---|---|---|
| Java | `tree-sitter-java 0.23.5` | `.java` |
| C | `tree-sitter-c 0.23.4` | `.c` |
| C++ | `tree-sitter-cpp 0.23.4` | `.cc .cpp .cxx .c++ .hh .hpp .hxx .h++ .ipp .h` |
| C# | `tree-sitter-c-sharp =0.23.1` | `.cs` |
| Ruby | `tree-sitter-ruby 0.23.1` | `.rb .rake` |

- **C# is pinned to `=0.23.1`.** Version 0.23.5 ships ABI 15, which tree-sitter 0.24
  rejects with an "Incompatible language version" error. The
  `every_language_query_compiles` test guards against this happening again.
- **`.h` is parsed as C++.** The C++ grammar accepts almost every C header; the
  C grammar fails on C++ headers.
- **Methods get owners**
  - Java/C# class/record/struct/interface members.
  - C++ in-class definitions, plus out-of-line `Foo::bar` definitions (owner taken from the qualifier scope).
  - Ruby methods inside a `class` or `module`.
- **Constructors.** `new Widget()` binds to the `Widget` constructor.
- **Imports.** `import`, `#include`, `using`, `require`/`require_relative`.
- **Receiver inference** now also handles type-before-name declarations:
  `Repo r`, `final Store s =`, `List<Foo> xs`, `Widget *w`.
- **Implicit receivers.** In Java, C#, C++ and Ruby, a bare `f()` inside a method
  of `T` scores +16 toward `T::f`. Rust, Python, JS/TS and Go keep preferring free
  functions, because in those languages a bare call is never a method.
- **Ruby paren-less calls.** A bare identifier in statement position is treated
  as a call.
- **Overloads are not ambiguity.** Ties between candidates with the same owner
  and file are not counted in `alternatives`.
- **Robustness**
  - Each language's query compiles lazily and independently. A broken grammar or
    query disables that one language (logged), not the whole indexer.
  - The index-compatibility key now includes a fingerprint of all query sources,
    so editing a `.scm` file re-extracts every file.

## Validation (real repos, release build, 4-core container)

| Repo | Language | LOC | Index time | Symbols | Unambiguous-bound calls |
|---|---|---|---|---|---|
| redis/hiredis | C | 18k | 167 ms | 806 | 48% |
| google/gson | Java | 57k | 257 ms | 4,502 | 36% (was 24% before the overload fix) |
| sinatra/sinatra | Ruby | 24k | 182 ms | 1,312 | 21% |
| fmtlib/fmt | C++ | 70k | 537 ms | 5,942 | 32% |
| dotnet/samples | C# | 263k | 2.3 s | 25,772 | 18% |

"Unbound" mostly means calls into standard or third-party libraries, which
codescope deliberately leaves unbound (ADR-0018).

# ADR-0016: Diff-Driven Change Impact

**Status:** Accepted

**Date:** 2026-09-26

## Context

After an agent edits code, the questions are: *what did I actually change, what
can that break, and which tests should I run?* `cs_blast_radius` answers the
middle question for one named target, but agents work in diffs, not symbol names.

## Decision

- `diff::parse_unified_diff` turns any unified diff (any context size) into
  new-side changed line ranges per file; pure deletions attribute to the line at
  the deletion point; deleted files are ignored.
- `diff::git_changes(root, base)` runs `git diff --unified=0 <base>` (default
  `HEAD`) plus `git ls-files --others --exclude-standard` (untracked files count
  as fully changed).
- `diff::diff_impact` maps ranges to the **innermost** overlapping symbols, runs
  incoming BFS over `Calls | References | Imports`, and splits the reached set into
  **impacted tests** (path conventions, `test_*`/`Test*` names, Rust `mod tests`)
  and **impacted production symbols**, plus a `risk` summary with counts.
- Budget priority: changed symbols → tests → impacted.
- Surfaces: CLI `codescope diff-impact [--base REF]` (re-indexes first so spans
  match the working tree), MCP `cs_diff_impact` (`base`, or a raw `diff` string).

## Consequences

- Agents get a "tests to run" list and PR-review-style impact in one call.
- Test detection is heuristic; frameworks with other conventions may be missed.
- Precision tracks resolution quality (ADR-0004).

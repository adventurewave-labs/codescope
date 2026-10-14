#!/usr/bin/env python3
"""Realistic synthetic-repo generator (docs/BENCHMARKS.md methodology).

Functions with doc comments, input validation, loops and ~9 cross-file call
sites each (21 lines per function, ~21 LOC/symbol) — closer to real Rust
than the dense corpus.

Usage: gen_realistic.py [out_dir] [n_files] [n_fns]
Defaults reproduce a ~1M-LOC corpus: 1,400 files x 39 fns.
"""
import os
import sys


def gen(out: str, nfiles: int, nfns: int) -> None:
    os.makedirs(os.path.join(out, "src"), exist_ok=True)
    for fi in range(nfiles):
        lines = []
        for j in range(nfns):
            lines.append(f"/// Documented handler number {j} in module {fi}.")
            lines.append(
                f"pub fn op{fi}_{j}(input: u32, flag: bool) -> Result<u32, String> {{"
            )
            lines.append(f"    if input > {1000 + j} {{")
            lines.append('        return Err(format!("input too large: {input}"));')
            lines.append("    }")
            lines.append("    let mut total: u32 = input;")
            for k in range(9):
                tf = (fi + k * 17 + j) % nfiles
                tj = (j + k * 3) % nfns
                lines.append(f"    total = total.wrapping_add(crate::m{tf}::op{tf}_{tj}(total, flag)?);")
            lines.append("    for _i in 0..3 {")
            lines.append(f"        total = total.wrapping_mul({1 + j});")
            lines.append("    }")
            lines.append("    Ok(total)")
            lines.append("}")
            lines.append("")
        with open(os.path.join(out, "src", f"m{fi}.rs"), "w") as fh:
            fh.write("\n".join(lines))
    with open(os.path.join(out, "src", "lib.rs"), "w") as fh:
        fh.write("\n".join(f"pub mod m{i};" for i in range(nfiles)) + "\n")


if __name__ == "__main__":
    out = sys.argv[1] if len(sys.argv) > 1 else "/tmp/perf1m"
    nfiles = int(sys.argv[2]) if len(sys.argv) > 2 else 1400
    nfns = int(sys.argv[3]) if len(sys.argv) > 3 else 39
    gen(out, nfiles, nfns)
    loc = 0
    for dirpath, _, files in os.walk(out):
        for f in files:
            with open(os.path.join(dirpath, f)) as fh:
                loc += sum(1 for _ in fh)
    print(f"{out}: {nfiles + 1} files, {loc:,} LOC")

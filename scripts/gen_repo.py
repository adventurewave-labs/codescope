#!/usr/bin/env python3
"""Dense synthetic-repo generator (docs/BENCHMARKS.md methodology).

N files x M functions, every function 4 lines with one cross-file call —
maximizes symbol/edge density per byte (a storage worst case).

Usage: gen_repo.py [out_dir] [n_files] [n_fns]
Defaults reproduce the 100k-LOC dense corpus: 500 files x 50 fns.
"""
import os
import sys


def gen(out: str, nfiles: int, nfns: int) -> None:
    os.makedirs(os.path.join(out, "src"), exist_ok=True)
    for fi in range(nfiles):
        lines = []
        for j in range(nfns):
            tgt = (fi + j) % nfiles
            lines.append(f"pub fn f{fi}_{j}(x: u32) -> u32 {{")
            lines.append(f"    let y = x + {j};")
            lines.append(f"    crate::m{tgt}::f{tgt}_{(j * 7) % nfns}(y)")
            lines.append("}")
            lines.append("")
        with open(os.path.join(out, "src", f"m{fi}.rs"), "w") as fh:
            fh.write("\n".join(lines))
    with open(os.path.join(out, "src", "lib.rs"), "w") as fh:
        fh.write("\n".join(f"pub mod m{i};" for i in range(nfiles)) + "\n")


if __name__ == "__main__":
    out = sys.argv[1] if len(sys.argv) > 1 else "/tmp/perf100k"
    nfiles = int(sys.argv[2]) if len(sys.argv) > 2 else 500
    nfns = int(sys.argv[3]) if len(sys.argv) > 3 else 50
    gen(out, nfiles, nfns)
    loc = 0
    for dirpath, _, files in os.walk(out):
        for f in files:
            with open(os.path.join(dirpath, f)) as fh:
                loc += sum(1 for _ in fh)
    print(f"{out}: {nfiles + 1} files, {loc:,} LOC")

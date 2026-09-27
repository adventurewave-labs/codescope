//! Call-resolution accuracy eval (ADR-0022).
//!
//! Every fixture under `tests/eval/<language>/` is a tiny multi-file project
//! whose call sites carry inline golden annotations:
//!
//! ```text
//! s.put(); // @eval put=Store::put          → must bind to Store::put
//! store::open(); // @eval open=open@src/store.rs → name + file
//! v.len(); // @eval len=-                    → external: must stay unbound
//! ```
//!
//! Scoring per annotation: TP = bound to the expected symbol; FP = bound to a
//! wrong symbol, or bound when it should stay external; FN = unbound (or not
//! extracted) when an in-repo target was expected; TN = correctly unbound.
//!
//! `scripts/eval` runs this and writes the report to `docs/EVAL.md`.

use codescope::domain::{CodeGraph, EdgeKind};
use codescope::{extract, resolve, walker};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;

#[derive(Default, Clone, Copy)]
struct Counts {
    tp: usize,
    fp: usize,
    fn_: usize,
    tn: usize,
}

impl Counts {
    fn add(&mut self, o: Counts) {
        self.tp += o.tp;
        self.fp += o.fp;
        self.fn_ += o.fn_;
        self.tn += o.tn;
    }
    fn precision(&self) -> f64 {
        ratio(self.tp, self.tp + self.fp)
    }
    fn recall(&self) -> f64 {
        ratio(self.tp, self.tp + self.fn_)
    }
    fn accuracy(&self) -> f64 {
        ratio(self.tp + self.tn, self.tp + self.tn + self.fp + self.fn_)
    }
    fn f1(&self) -> f64 {
        let (p, r) = (self.precision(), self.recall());
        if p + r == 0.0 {
            0.0
        } else {
            2.0 * p * r / (p + r)
        }
    }
}

fn ratio(a: usize, b: usize) -> f64 {
    if b == 0 {
        1.0
    } else {
        a as f64 / b as f64
    }
}

fn last_seg(s: &str) -> &str {
    s.rsplit("::")
        .next()
        .and_then(|x| x.rsplit('.').next())
        .unwrap_or(s)
}

fn build(dir: &Path) -> CodeGraph {
    let mut g = CodeGraph::new();
    for f in walker::walk(dir) {
        let src = std::fs::read_to_string(&f.abs_path).unwrap();
        g.upsert_file(extract::extract(f.language, &f.rel_path, &src, 0));
    }
    g.reindex();
    resolve::resolve(&mut g);
    g
}

/// Evaluate one fixture; returns counts and human-readable misses.
fn eval_fixture(dir: &Path) -> (Counts, Vec<String>) {
    let g = build(dir);
    let mut c = Counts::default();
    let mut misses = Vec::new();
    for f in walker::walk(dir) {
        let src = std::fs::read_to_string(&f.abs_path).unwrap();
        for (i, line) in src.lines().enumerate() {
            let Some(spec) = line.split("@eval ").nth(1) else {
                continue;
            };
            let lineno = i as u32 + 1;
            for item in spec.split_whitespace() {
                let (callee, expected) = item.split_once('=').expect("callee=expected");
                // Bound target(s) of matching call edges on this line.
                let bound: Vec<Option<(String, String)>> = g
                    .edges()
                    .iter()
                    .filter(|e| e.kind == EdgeKind::Calls && e.line == lineno)
                    .filter(|e| last_seg(&e.to_name) == callee)
                    .filter(|e| g.symbol(e.from).is_some_and(|s| s.file == f.rel_path))
                    .map(|e| {
                        e.to.and_then(|t| g.symbol(t))
                            .map(|s| (s.qualified_name(), s.file.clone()))
                    })
                    .collect();
                let got = bound.iter().flatten().next().cloned();
                let extracted = !bound.is_empty();
                let loc = format!("{}:{lineno} {callee}", f.rel_path);
                match (expected, got) {
                    ("-", None) => c.tn += 1,
                    ("-", Some((q, file))) => {
                        c.fp += 1;
                        misses.push(format!("FP {loc}: expected external, bound to {q}@{file}"));
                    }
                    (exp, None) => {
                        c.fn_ += 1;
                        let why = if extracted {
                            "unbound"
                        } else {
                            "call not extracted"
                        };
                        misses.push(format!("FN {loc}: expected {exp}, {why}"));
                    }
                    (exp, Some((q, file))) => {
                        let (eq, ef) = exp
                            .split_once('@')
                            .map_or((exp, None), |(a, b)| (a, Some(b)));
                        if q == eq && ef.map_or(true, |ef| ef == file) {
                            c.tp += 1;
                        } else {
                            c.fp += 1;
                            misses.push(format!("FP {loc}: expected {exp}, bound to {q}@{file}"));
                        }
                    }
                }
            }
        }
    }
    (c, misses)
}

#[test]
fn resolution_accuracy() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/eval");
    let mut langs: BTreeMap<String, (Counts, Vec<String>)> = BTreeMap::new();
    for entry in std::fs::read_dir(&root).unwrap() {
        let dir = entry.unwrap().path();
        if dir.is_dir() {
            let name = dir.file_name().unwrap().to_string_lossy().into_owned();
            langs.insert(name, eval_fixture(&dir));
        }
    }

    let mut total = Counts::default();
    let mut report = String::new();
    writeln!(
        report,
        "| Language | Call sites | Precision | Recall | F1 | Accuracy |"
    )
    .unwrap();
    writeln!(report, "|---|---|---|---|---|---|").unwrap();
    for (lang, (c, _)) in &langs {
        total.add(*c);
        writeln!(
            report,
            "| {lang} | {} | {:.2} | {:.2} | {:.2} | {:.2} |",
            c.tp + c.fp + c.fn_ + c.tn,
            c.precision(),
            c.recall(),
            c.f1(),
            c.accuracy()
        )
        .unwrap();
    }
    writeln!(
        report,
        "| **all** | **{}** | **{:.2}** | **{:.2}** | **{:.2}** | **{:.2}** |",
        total.tp + total.fp + total.fn_ + total.tn,
        total.precision(),
        total.recall(),
        total.f1(),
        total.accuracy()
    )
    .unwrap();
    let misses: Vec<&String> = langs.values().flat_map(|(_, m)| m).collect();
    if !misses.is_empty() {
        writeln!(report, "\nMisses:\n").unwrap();
        for m in &misses {
            writeln!(report, "- `{m}`").unwrap();
        }
    }
    println!("{report}");
    if let Ok(path) = std::env::var("CODESCOPE_EVAL_REPORT") {
        let doc = format!(
            "# Call-resolution accuracy\n\nGenerated by `scripts/eval` from `tests/resolution_eval.rs` \
             over the annotated fixtures in `tests/eval/` (ADR-0022). TP = bound to the \
             expected symbol; FP = wrong symbol, or bound when the call is external; FN = \
             in-repo target left unbound.\n\n{report}"
        );
        std::fs::write(path, doc).unwrap();
    }

    // Regression gates (set just under the measured baseline).
    assert!(
        total.precision() >= PRECISION_GATE,
        "precision regressed\n{report}"
    );
    assert!(total.recall() >= RECALL_GATE, "recall regressed\n{report}");
}

const PRECISION_GATE: f64 = 0.97;
const RECALL_GATE: f64 = 0.92;

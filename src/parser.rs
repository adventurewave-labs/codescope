//! Parsing context: wraps tree-sitter (ADR-0002).
//!
//! Provides a per-language [`tree_sitter::Language`] and a thin parse entry
//! point. Parsers are cheap to create; we make one per file during the parallel
//! parse so there is no shared mutable state across rayon workers (ADR-0007).

use crate::domain::Language;
use tree_sitter::{Parser, Tree};

/// Return the compiled tree-sitter grammar for a language.
pub fn ts_language(lang: Language) -> tree_sitter::Language {
    match lang {
        Language::Rust => tree_sitter_rust::LANGUAGE.into(),
        Language::Python => tree_sitter_python::LANGUAGE.into(),
        Language::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
        Language::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        Language::Go => tree_sitter_go::LANGUAGE.into(),
    }
}

/// Parse `source` for the given language. Tree-sitter recovers from errors, so
/// this returns a (possibly partial) tree even for broken/in-progress code
/// (ADR-0013).
pub fn parse(lang: Language, source: &str) -> Option<Tree> {
    let mut parser = Parser::new();
    parser.set_language(&ts_language(lang)).ok()?;
    parser.parse(source, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_all_languages() {
        assert!(parse(Language::Rust, "fn main() {}").is_some());
        assert!(parse(Language::Python, "def f():\n    pass").is_some());
        assert!(parse(Language::JavaScript, "function f() {}").is_some());
        assert!(parse(Language::TypeScript, "function f(): void {}").is_some());
        assert!(parse(Language::Go, "package main\nfunc main() {}").is_some());
    }

    #[test]
    fn recovers_from_broken_code() {
        // Missing closing brace — tree-sitter should still return a tree.
        let tree = parse(Language::Rust, "fn main() { let x = ").unwrap();
        assert!(tree.root_node().has_error());
    }
}

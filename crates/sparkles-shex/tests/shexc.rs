//! The ShExC lexer and parser over the shexTest schemas: every positive schema parses,
//! every negative-syntax schema fails, and the lexer gives back every file's text.
//!
//! The suite comes from `SPARKLES_SHEX_TESTS` or Apache Jena's copy next to the
//! workspace (see `shextest.rs`); the tests are skipped without it.

use sparkles_shex::Schema;
use sparkles_shex::shexc::lexer::{TokenKind, lex};
use std::path::{Path, PathBuf};

fn suite_dir() -> Option<PathBuf> {
    let p = std::env::var("SPARKLES_SHEX_TESTS")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../../../apache/jena/jena-shex/src/test/files/spec")
        });
    p.exists().then_some(p)
}

/// The `.shex` files of `dir`, sorted.
fn shex_files(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.extension().is_some_and(|x| x == "shex"))
                .collect()
        })
        .unwrap_or_default();
    files.sort();
    files
}

/// The positive schemas: `syntax/` in Jena's layout, `schemas/` upstream.
fn positive_dir(suite: &Path) -> PathBuf {
    let syntax = suite.join("syntax");
    if syntax.is_dir() {
        syntax
    } else {
        suite.join("schemas")
    }
}

fn name(p: &Path) -> String {
    p.file_name().unwrap().to_string_lossy().into_owned()
}

#[test]
fn syntax_parses() {
    let Some(suite) = suite_dir() else {
        eprintln!("shexTest suite not found (set SPARKLES_SHEX_TESTS); skipped");
        return;
    };
    let files = shex_files(&positive_dir(&suite));
    assert!(!files.is_empty(), "no schemas in {}", suite.display());
    let mut failures = Vec::new();
    for f in &files {
        let text = std::fs::read_to_string(f).unwrap();
        if let Err(e) = Schema::parse_shexc(&text, None) {
            failures.push(format!("{}: {e}", name(f)));
        }
    }
    eprintln!(
        "syntax: {}/{} parse",
        files.len() - failures.len(),
        files.len()
    );
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn negative_syntax_fails() {
    let Some(suite) = suite_dir() else {
        eprintln!("shexTest suite not found (set SPARKLES_SHEX_TESTS); skipped");
        return;
    };
    let files = shex_files(&suite.join("negativeSyntax"));
    assert!(
        !files.is_empty(),
        "no negative schemas in {}",
        suite.display()
    );
    let mut accepted = Vec::new();
    for f in &files {
        let text = std::fs::read_to_string(f).unwrap();
        if Schema::parse_shexc(&text, None).is_ok() {
            accepted.push(name(f));
        }
    }
    eprintln!(
        "negativeSyntax: {}/{} rejected",
        files.len() - accepted.len(),
        files.len()
    );
    assert!(accepted.is_empty(), "accepted: {accepted:?}");
}

#[test]
fn lexer_is_lossless() {
    let Some(suite) = suite_dir() else {
        eprintln!("shexTest suite not found (set SPARKLES_SHEX_TESTS); skipped");
        return;
    };
    let mut n = 0;
    for dir in ["syntax", "schemas", "negativeSyntax", "negativeStructure"] {
        for f in shex_files(&suite.join(dir)) {
            let text = std::fs::read_to_string(&f).unwrap();
            let toks = lex(&text);
            let joined: String = toks.iter().map(|t| t.text(&text)).collect();
            assert_eq!(
                joined,
                text.trim_start_matches('\u{feff}'),
                "{}",
                f.display()
            );
            assert_eq!(toks.last().map(|t| t.kind), Some(TokenKind::Eof));
            n += 1;
        }
    }
    eprintln!("lexed {n} files losslessly");
}

/// The suite's schemas that import others (circular imports included) close with their
/// imports read from files next to them, and those the validation tests use compile.
#[test]
fn imports_close() {
    let Some(suite) = suite_dir() else {
        eprintln!("shexTest suite not found (set SPARKLES_SHEX_TESTS); skipped");
        return;
    };
    let manifest =
        std::fs::read_to_string(suite.join("validation/manifest.ttl")).unwrap_or_default();
    let mut n = 0;
    let mut failures = Vec::new();
    for f in shex_files(&suite.join("schemas")) {
        let text = std::fs::read_to_string(&f).unwrap();
        let base = sparkles_shex::resolve::file_url(&f);
        let schema = Schema::parse_shexc(&text, Some(&base)).unwrap();
        if schema.imports.is_empty() {
            continue;
        }
        n += 1;
        let resolver = sparkles_shex::FileResolver::default();
        if let Err(e) = sparkles_shex::resolve::close(&schema, &resolver) {
            failures.push(format!("{}: {e}", name(&f)));
        }
        // (some imported schemas reference labels only their importers declare)
        let used = manifest.contains(&format!("schemas/{}>", name(&f)));
        if used && let Err(e) = sparkles_shex::compile(&schema, &resolver) {
            failures.push(format!("{}: compile: {e}", name(&f)));
        }
    }
    eprintln!("imports: {}/{n} schemas close", n - failures.len());
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

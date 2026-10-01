//! The lossless Turtle and TriG syntax tree on the W3C suites and the SHACL files, and on
//! one snapshot per construct.
//!
//! - Every input the reference parser (oxttl) accepts parses to a tree that holds each
//!   significant token exactly once, in order: the tree's parser accepts at least what
//!   the reference parser accepts.
//! - `tests/cst/<name>.ttl` and `.trig` parse to the outline in `<name>.tree`
//!   (`SPARKLES_FMT_BLESS=1` writes the outlines).
//!
//! The suites come from `SPARKLES_RDF_TESTS_DIR` (or next to `SPARKLES_W3C_DIR`) and
//! `SPARKLES_SHACL_TESTS`; those tests are skipped without them.

mod corpus;

use corpus::rdf;
use sparkles_fmt::Language;
use sparkles_fmt::check::graph;
use sparkles_fmt::lex::{LexMode, TokenKind, lex};
use sparkles_fmt::tree::{Element, NodeId, Tree};
use sparkles_fmt::turtle::parse::parse;
use std::path::{Path, PathBuf};

/// The significant tokens of the tree, in tree order.
fn tree_tokens(t: &Tree<'_>, n: NodeId, out: &mut Vec<u32>) {
    for e in t.children(n) {
        match *e {
            Element::Node(c) => tree_tokens(t, c, out),
            Element::Token(id) => out.push(id.0),
        }
    }
}

/// The tree holds every significant token once, in order, and its tokens concatenate to
/// the input.
fn lossless(name: &str, text: &str, t: &Tree<'_>) -> Result<(), String> {
    let mut seen = Vec::new();
    tree_tokens(t, t.root(), &mut seen);
    let significant: Vec<u32> = t
        .tokens
        .iter()
        .enumerate()
        .filter(|(_, tok)| !tok.kind.is_trivia() && tok.kind != TokenKind::Eof)
        .map(|(i, _)| i as u32)
        .collect();
    if seen != significant {
        return Err(format!("{name}: tokens missing from the tree"));
    }
    let joined: String = t.tokens.iter().map(|tok| tok.text(text)).collect();
    if joined != text.trim_start_matches('\u{feff}') {
        return Err(format!("{name}: the tokens are not the input"));
    }
    Ok(())
}

/// Parse `text` as Turtle or TriG and check the tree.
fn parses(name: &str, text: &str, trig: bool) -> Result<(), String> {
    let t = parse(text, lex(text, LexMode::Turtle), trig).map_err(|e| format!("{name}: {e}"))?;
    lossless(name, text, &t)
}

#[test]
fn accepts_what_the_reference_accepts() {
    let mut failures = Vec::new();
    let mut accepted = 0;
    if let Some(dir) = rdf::suite_dir() {
        for lang in [Language::Turtle, Language::TriG] {
            for case in rdf::cases(&dir, lang) {
                let Ok(text) = std::fs::read_to_string(&case.path) else {
                    continue;
                };
                if graph::parse(&text, lang).is_err() {
                    continue;
                }
                accepted += 1;
                if let Err(e) = parses(&case.rel, &text, lang == Language::TriG) {
                    failures.push(e);
                }
            }
        }
    } else {
        eprintln!("W3C RDF suite not found (set SPARKLES_RDF_TESTS_DIR): skipped");
    }
    if let Some(dir) = rdf::shacl_dir() {
        for (rel, path) in rdf::shacl_files(&dir) {
            let text = std::fs::read_to_string(&path).unwrap();
            accepted += 1;
            if let Err(e) = parses(&rel, &text, false) {
                failures.push(e);
            }
        }
    } else {
        eprintln!("SHACL tests not found (set SPARKLES_SHACL_TESTS): skipped");
    }
    eprintln!(
        "Turtle and TriG syntax trees: {accepted} inputs, {} failures",
        failures.len()
    );
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

fn bless() -> bool {
    std::env::var_os("SPARKLES_FMT_BLESS").is_some_and(|v| !v.is_empty() && v != "0")
}

#[test]
fn snapshots() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/cst");
    let mut inputs: Vec<PathBuf> = std::fs::read_dir(&dir)
        .expect("tests/cst")
        .flatten()
        .map(|e| e.path())
        .filter(|p| matches!(p.extension().and_then(|e| e.to_str()), Some("ttl" | "trig")))
        .collect();
    inputs.sort();
    assert!(!inputs.is_empty());
    let mut failures = Vec::new();
    for input in &inputs {
        let name = input.file_name().unwrap().to_string_lossy().to_string();
        let text = std::fs::read_to_string(input).unwrap();
        let trig = name.ends_with(".trig");
        let lang = match trig {
            false => Language::Turtle,
            true => Language::TriG,
        };
        // every snapshot is valid
        if let Err(e) = graph::parse(&text, lang) {
            panic!("{name}: not valid: {e}");
        }
        let tree = match parse(&text, lex(&text, LexMode::Turtle), trig) {
            Ok(t) => t,
            Err(e) => {
                failures.push(format!("{name}: {e}"));
                continue;
            }
        };
        if let Err(e) = lossless(&name, &text, &tree) {
            failures.push(e);
            continue;
        }
        let dump = tree.dump();
        let expected = input.with_extension("tree");
        if bless() {
            std::fs::write(&expected, &dump).unwrap();
            continue;
        }
        match std::fs::read_to_string(&expected) {
            Ok(want) if want == dump => {}
            Ok(_) => failures.push(format!(
                "{name}: the tree differs from {} (SPARKLES_FMT_BLESS=1 rewrites it):\n{dump}",
                expected.display()
            )),
            Err(e) => failures.push(format!("{}: {e}", expected.display())),
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

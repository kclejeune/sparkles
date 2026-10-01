//! The lossless SPARQL syntax tree on the W3C suites and on one snapshot per construct.
//!
//! - Every query and update the reference parser accepts parses to a tree that holds
//!   each significant token exactly once, in order.
//! - The tree's parser accepts exactly what the reference parser accepts, on every
//!   positive and negative syntax test, except the divergences listed in [`DIVERGENCES`]
//!   with their reasons.
//! - `tests/cst/<name>.rq` and `.ru` parse to the outline in `<name>.tree`
//!   (`SPARKLES_FMT_BLESS=1` writes the outlines).
//!
//! The suites come from `SPARKLES_W3C_DIR` (as in `tests/w3c.rs`); those tests are
//! skipped without it.

use sparkles_fmt::lex::{LexMode, lex};
use sparkles_fmt::sparql::Unit;
use sparkles_fmt::sparql::parse::parse;
use sparkles_fmt::tree::{Element, NodeId, Tree};
use std::path::{Path, PathBuf};

/// Files where the two parsers disagree, relative to the suite directory, with the
/// reason. The syntax tree's parser accepts these negative tests: it checks the grammar
/// and the reference parser's checks of blank node labels and ground data, but not the
/// scoping of variables, which needs the algebra.
const DIVERGENCES: &[(&str, &str)] = &[
    ("sparql11/aggregates/agg08.rq", SCOPE),
    ("sparql11/aggregates/agg09.rq", SCOPE),
    ("sparql11/aggregates/agg10.rq", SCOPE),
    ("sparql11/aggregates/agg11.rq", SCOPE),
    ("sparql11/aggregates/agg12.rq", SCOPE),
    ("sparql11/grouping/group06.rq", SCOPE),
    ("sparql11/grouping/group07.rq", SCOPE),
    ("sparql11/syntax-query/syn-bad-01.rq", SCOPE),
    ("sparql11/syntax-query/syn-bad-02.rq", SCOPE),
    ("sparql11/syntax-query/syn-bad-03.rq", SCOPE),
    ("sparql11/syntax-query/syntax-BINDscope6.rq", SCOPE),
    ("sparql11/syntax-query/syntax-BINDscope7.rq", SCOPE),
    ("sparql11/syntax-query/syntax-BINDscope8.rq", SCOPE),
    ("sparql11/syntax-query/syntax-SELECTscope2.rq", SCOPE),
    ("sparql12/syntax/group-by-scope-bad-1.rq", SCOPE),
    ("sparql12/syntax/group-by-scope-bad-2.rq", SCOPE),
    ("sparql12/syntax/group-by-scope-bad-3.rq", SCOPE),
];

const SCOPE: &str =
    "variable scope (SPARQL 1.2 §18.2.1, §11.4): checked while building the algebra";

fn suite_dir() -> Option<PathBuf> {
    let p = std::env::var("SPARKLES_W3C_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../../../apache/jena/jena-arq/testing/rdf-tests-cg/sparql")
        });
    p.exists().then_some(p)
}

/// Every `.rq` and `.ru` file under `dir`, sorted.
fn sparql_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if matches!(p.extension().and_then(|e| e.to_str()), Some("rq" | "ru")) {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

fn unit_of(path: &Path) -> Unit {
    match path.extension().and_then(|e| e.to_str()) {
        Some("ru") => Unit::Update,
        _ => Unit::Query,
    }
}

/// The reference parser's verdict, with the file's URL as the base IRI (as the engine's
/// suite runs it).
fn reference_accepts(path: &Path, text: &str, unit: Unit) -> bool {
    let base = format!("file://{}", path.display());
    let p = spargebra::SparqlParser::new()
        .with_base_iri(&base)
        .expect("a valid base IRI");
    match unit {
        Unit::Query => p.parse_query(text).is_ok(),
        Unit::Update => p.parse_update(text).is_ok(),
    }
}

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
fn assert_lossless(name: &str, text: &str, t: &Tree<'_>) {
    let mut seen = Vec::new();
    tree_tokens(t, t.root(), &mut seen);
    let significant: Vec<u32> = t
        .tokens
        .iter()
        .enumerate()
        .filter(|(_, tok)| !tok.kind.is_trivia() && tok.kind != sparkles_fmt::lex::TokenKind::Eof)
        .map(|(i, _)| i as u32)
        .collect();
    assert_eq!(seen, significant, "{name}: tokens missing from the tree");
    let joined: String = t.tokens.iter().map(|tok| tok.text(text)).collect();
    assert_eq!(joined, text.trim_start_matches('\u{feff}'), "{name}");
}

#[test]
fn accepts_what_the_reference_accepts() {
    let Some(dir) = suite_dir() else {
        eprintln!("W3C suite not found (set SPARKLES_W3C_DIR): skipped");
        return;
    };
    let mut divergences = Vec::new();
    let (mut accepted, mut rejected) = (0, 0);
    for path in sparql_files(&dir) {
        // a few negative tests are not UTF-8
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let rel = path
            .strip_prefix(&dir)
            .unwrap_or(&path)
            .display()
            .to_string();
        let unit = unit_of(&path);
        let reference = reference_accepts(&path, &text, unit);
        let tokens = lex(&text, LexMode::Sparql);
        let cst = parse(&text, tokens, unit);
        if let Ok(t) = &cst {
            assert_lossless(&rel, &text, t);
        }
        match (reference, &cst) {
            (true, Ok(_)) => accepted += 1,
            (false, Err(_)) => rejected += 1,
            (true, Err(e)) => divergences.push(format!("{rel}: rejected: {e}")),
            (false, Ok(_)) => divergences.push(format!("{rel}: accepted")),
        }
    }
    eprintln!(
        "W3C SPARQL: {accepted} accepted and {rejected} rejected by both, {} divergences",
        divergences.len()
    );
    // only over-acceptance is ever listed: rejecting what the reference accepts would
    // refuse valid input
    let listed = |d: &str| {
        DIVERGENCES
            .iter()
            .any(|(f, _)| d == format!("{f}: accepted"))
    };
    let unexpected: Vec<&str> = divergences
        .iter()
        .map(String::as_str)
        .filter(|d| !listed(d))
        .collect();
    let stale: Vec<&str> = DIVERGENCES
        .iter()
        .map(|(f, _)| *f)
        .filter(|f| !divergences.iter().any(|d| d.starts_with(&format!("{f}:"))))
        .collect();
    assert!(
        unexpected.is_empty() && stale.is_empty(),
        "unexpected divergences:\n{}\nlisted but agreeing: {stale:?}",
        unexpected.join("\n")
    );
}

/// Edge cases where the reference parser is more lenient than the grammar, or checks more
/// than the grammar: both parsers agree on each.
#[test]
fn edge_cases_agree() {
    const P: &str = "PREFIX : <http://example.org/> ";
    let cases: &[(&str, Unit, bool)] = &[
        // quad blocks need no `.` between statements
        ("INSERT DATA { :a :b :c :d :e :f }", Unit::Update, true),
        ("INSERT DATA { :a :b :c . . :d :e :f }", Unit::Update, false),
        (
            "INSERT DATA { GRAPH :g { :a :b :c :d :e :f } }",
            Unit::Update,
            false,
        ),
        // a lone `;`, and none after the last operation
        (";", Unit::Update, true),
        ("CLEAR ALL ;", Unit::Update, true),
        ("CLEAR ALL ; ;", Unit::Update, false),
        ("CLEAR ALL CLEAR ALL", Unit::Update, false),
        // a template of a lone `.`
        ("CONSTRUCT { . } WHERE {}", Unit::Query, true),
        ("CONSTRUCT WHERE { . }", Unit::Query, false),
        // triples statements need a `.` between them, other elements may have one
        ("SELECT * { :a :b :c :d :e :f }", Unit::Query, false),
        (
            "SELECT * { :a :b :c OPTIONAL {} . :d :e :f . }",
            Unit::Query,
            true,
        ),
        ("SELECT * { :a :b :c . . }", Unit::Query, false),
        ("SELECT * { . }", Unit::Query, false),
        ("SELECT * { :a :b :c ; ; :d :e }", Unit::Query, true),
        // a comment makes `[ ]` and `( )` two tokens, which no rule accepts
        ("SELECT * { :a :b [ #c\n ] }", Unit::Query, false),
        ("SELECT * { :a :b ( #c\n ) }", Unit::Query, false),
        (
            "SELECT * { VALUES ( #c\n ) { ( #c\n ) } }",
            Unit::Query,
            true,
        ),
        // VALUES rows match the variables
        ("SELECT * { VALUES (?a ?b) { (1) } }", Unit::Query, false),
        ("SELECT * { VALUES (?a $a) { (1 2) } }", Unit::Query, false),
        ("SELECT * { VALUES ?a { ?b } }", Unit::Query, false),
        // reifiers and annotations follow plain predicates only
        ("SELECT * { :a ^:b :c ~ :r }", Unit::Query, true),
        ("SELECT * { :a (:b) :c {| :d :e |} }", Unit::Query, true),
        ("SELECT * { :a ?v :c {| :d :e |} }", Unit::Query, true),
        ("SELECT * { :a :b+ :c ~ :r }", Unit::Query, false),
        ("SELECT * { :a !:b :c {| :d :e |} }", Unit::Query, false),
        (
            "SELECT * { :a :b :c ; :p/:q :d {| :d :e |} }",
            Unit::Query,
            false,
        ),
        // a blank node label in two basic graph patterns
        (
            "SELECT * { _:a :b :c OPTIONAL { _:a :d :e } }",
            Unit::Query,
            false,
        ),
        (
            "SELECT * { _:a :b :c FILTER(true) _:a :d :e }",
            Unit::Query,
            true,
        ),
        (
            "CONSTRUCT { _:a :b :c } WHERE { _:a :b :c }",
            Unit::Query,
            true,
        ),
        (
            "INSERT DATA { _:a :b :c } ; INSERT DATA { _:a :b :c }",
            Unit::Update,
            false,
        ),
        (
            "INSERT { _:a :b :c } WHERE { _:a :b :c }",
            Unit::Update,
            true,
        ),
        // ground data
        ("INSERT DATA { ?s :b :c }", Unit::Update, false),
        ("INSERT DATA { :a :b [ :c :d ] }", Unit::Update, true),
        ("DELETE DATA { :a :b [] }", Unit::Update, false),
        ("DELETE WHERE { :a :b ( 1 ) }", Unit::Update, false),
        (
            "DELETE { :a :b :c {| :d :e |} } WHERE {}",
            Unit::Update,
            false,
        ),
        (
            "DELETE { :a :b :c ~ :r {| :d :e |} } WHERE {}",
            Unit::Update,
            true,
        ),
        (
            "DELETE { << :a :b :c >> :d :e } WHERE {}",
            Unit::Update,
            false,
        ),
        (
            "DELETE { << :a :b :c ~ :r >> :d :e } WHERE {}",
            Unit::Update,
            true,
        ),
        // escapes and directions
        ("SELECT * { :a :b \"\\uD83C\" }", Unit::Query, false),
        ("SELECT * { :a :b \"\\q\" }", Unit::Query, false),
        ("SELECT * { :a :b \"x\"@en--rtl }", Unit::Query, true),
        ("SELECT * { :a :b \"x\"@en--up }", Unit::Query, false),
        ("SELECT * { :a :b <\\u0061> }", Unit::Query, true),
        // LIMIT and OFFSET in either order, once each
        ("SELECT * {} OFFSET 1 LIMIT 1", Unit::Query, true),
        ("SELECT * {} LIMIT 1 LIMIT 1", Unit::Query, false),
        // keywords match case-insensitively, `a` only in lowercase
        ("select * where { ?s a ?o } limit 1", Unit::Query, true),
        ("SELECT * { ?s A ?o }", Unit::Query, false),
        ("SELECT * {} GROUP BY", Unit::Query, false),
        ("SELECT {}", Unit::Query, false),
        ("PREFIX : <http://example.org/>", Unit::Query, false),
    ];
    let mut failures = Vec::new();
    for &(body, unit, valid) in cases {
        let text = format!("{P}{body}");
        let reference = reference_accepts(Path::new("/edge"), &text, unit);
        let cst = parse(&text, lex(&text, LexMode::Sparql), unit);
        if reference != valid || cst.is_ok() != valid {
            failures.push(format!(
                "{body:?}: expected {valid}, reference {reference}, tree {cst:?}",
                cst = cst.map(|_| ())
            ));
        }
    }
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
        .filter(|p| matches!(p.extension().and_then(|e| e.to_str()), Some("rq" | "ru")))
        .collect();
    inputs.sort();
    assert!(!inputs.is_empty());
    let mut failures = Vec::new();
    for input in &inputs {
        let name = input.file_name().unwrap().to_string_lossy().to_string();
        let text = std::fs::read_to_string(input).unwrap();
        let unit = unit_of(input);
        // every snapshot is valid SPARQL
        assert!(reference_accepts(input, &text, unit), "{name}: not valid");
        let tree = match parse(&text, lex(&text, LexMode::Sparql), unit) {
            Ok(t) => t,
            Err(e) => {
                failures.push(format!("{name}: {e}"));
                continue;
            }
        };
        assert_lossless(&name, &text, &tree);
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

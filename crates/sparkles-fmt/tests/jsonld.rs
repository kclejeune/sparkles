//! JSON-LD over a corpus of real documents. Every document formats (or is a syntax error
//! of the reference parser), passes the JSON check, and formats to itself, under the
//! defaults, with `sort` and at width 40; where `oxjsonld` reads the input without
//! fetching anything, the input's and the output's datasets are isomorphic. With `sort`,
//! shuffling the members of every object whose order the rules decide, and re-spacing
//! the document, gives the same output.
//!
//! The corpus (each part skipped when absent):
//! - the JSON and JSON-LD files of the Jena checkout: `jena-arq/testing/RIOT` (`jsonld`,
//!   `jsonld11`, the stream manager and reader tests), the W3C SPARQL suite's frame and
//!   JSON results (`.srj`), the ShEx suite's manifests and ShExJ schemas;
//! - Oxigraph's `testsuite/oxigraph-tests/jsonld` (`SPARKLES_OXIGRAPH_DIR`, else the
//!   sibling checkout);
//! - every Turtle and TriG evaluation result of the W3C RDF suites, written as JSON-LD by
//!   `oxjsonld`;
//! - with `SPARKLES_JSONLD_TESTS=<json-ld-api checkout>/tests`, every `.jsonld` and
//!   `.json` file under it.
//!
//! Documents expected to fail are listed in `tests/fmt-known-failures.txt` as
//! `jsonld:<name>` with a reason; listed ones that pass are reported.
//!
//! `mise run fmt:jsonld-compare` (not part of `ci`) compares the golden outputs with
//! oxfmt, which follows Prettier's JSON conventions. Of the 11 outputs, 3 differ, each by
//! a deliberate rule: an array holding an object stays one element per line where
//! Prettier keeps `[{ "@id": "x" }]` on one line; a broken array of numbers takes one
//! number per line where Prettier fills lines; number lexemes stay as written where oxfmt
//! rewrites `1.50` and `1E3`.

mod corpus;

use oxrdf::Quad;
use sparkles_fmt::check::graph::isomorphic;
use sparkles_fmt::check::json::{json_equivalent, json_reference};
use sparkles_fmt::jsonld::print::Table;
use sparkles_fmt::syntax::NodeKind;
use sparkles_fmt::tree::{NodeId, Tree};
use sparkles_fmt::{Check, FormatError, Language, Options, format};
use std::path::{Path, PathBuf};

/// One document: a name (`jena:…`, `oxigraph:…`, `w3c:…`, `rdf:…`) and its text.
struct Doc {
    name: String,
    text: String,
}

/// The Jena checkout: four levels above `SPARKLES_W3C_DIR`, else the sibling checkout.
fn jena_dir() -> Option<PathBuf> {
    let p = match std::env::var("SPARKLES_W3C_DIR") {
        Ok(sparql) => Path::new(&sparql).join("../../../.."),
        Err(_) => Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../../apache/jena"),
    };
    p.join("jena-arq").exists().then_some(p)
}

/// The Oxigraph checkout: `SPARKLES_OXIGRAPH_DIR`, else next to the Jena one or the
/// sibling checkout.
fn oxigraph_dir() -> Option<PathBuf> {
    let candidates = match std::env::var("SPARKLES_OXIGRAPH_DIR") {
        Ok(p) => vec![PathBuf::from(p)],
        Err(_) => vec![
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../../oxigraph/oxigraph"),
            jena_dir()
                .map(|j| j.join("../../oxigraph/oxigraph"))
                .unwrap_or_default(),
        ],
    };
    candidates
        .into_iter()
        .find(|p| p.join("testsuite").exists())
}

/// The files under `dir` with one of `exts`, sorted.
fn files(dir: &Path, exts: &[&str]) -> Vec<PathBuf> {
    fn walk(dir: &Path, f: &mut dyn FnMut(&Path)) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for p in rd.flatten().map(|e| e.path()) {
            if p.is_dir() {
                walk(&p, f);
            } else {
                f(&p);
            }
        }
    }
    let mut v = Vec::new();
    walk(dir, &mut |p| {
        if p.extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| exts.contains(&e))
        {
            v.push(p.to_path_buf());
        }
    });
    v.sort();
    v
}

/// The documents under `root`'s `dirs` with `exts`, named `prefix:` and their path under
/// `root`.
fn docs_under(root: &Path, dirs: &[&str], exts: &[&str], prefix: &str, out: &mut Vec<Doc>) {
    for d in dirs {
        for p in files(&root.join(d), exts) {
            let Ok(text) = std::fs::read_to_string(&p) else {
                continue;
            };
            let rel = p.strip_prefix(root).unwrap_or(&p).to_string_lossy();
            out.push(Doc {
                name: format!("{prefix}:{}", rel.replace('\\', "/")),
                text,
            });
        }
    }
}

/// Every Turtle and TriG evaluation result as JSON-LD (results with RDF 1.2 triple terms,
/// which JSON-LD 1.1 cannot write, are left out).
fn generated() -> Vec<Doc> {
    let Some(dir) = corpus::rdf::suite_dir() else {
        return Vec::new();
    };
    let mut results: Vec<(String, PathBuf)> = Vec::new();
    for lang in [Language::Turtle, Language::TriG] {
        for c in corpus::rdf::cases(&dir, lang) {
            if let Some(r) = c.result
                && !results.iter().any(|(_, p)| *p == r)
            {
                results.push((c.rel, r));
            }
        }
    }
    corpus::par_map(&results, |(rel, path)| {
        let text = std::fs::read_to_string(path).ok()?;
        let quads: Vec<Quad> = oxttl::NQuadsParser::new()
            .for_slice(&text)
            .collect::<Result<_, _>>()
            .ok()?;
        let mut w = oxjsonld::JsonLdSerializer::new()
            .with_prefix("rdf", "http://www.w3.org/1999/02/22-rdf-syntax-ns#")
            .ok()?
            .with_prefix("xsd", "http://www.w3.org/2001/XMLSchema#")
            .ok()?
            .for_writer(Vec::new());
        for q in &quads {
            w.serialize_quad(q).ok()?;
        }
        let text = String::from_utf8(w.finish().ok()?).ok()?;
        Some(Doc {
            name: format!("rdf:{rel}"),
            text,
        })
    })
    .into_iter()
    .flatten()
    .collect()
}

fn corpus_docs() -> Vec<Doc> {
    let mut docs = Vec::new();
    if let Some(jena) = jena_dir() {
        docs_under(
            &jena,
            &[
                "jena-arq/testing/RIOT/jsonld",
                "jena-arq/testing/RIOT/jsonld11",
                "jena-arq/testing/RIOT/StreamManager",
                "jena-arq/testing/RIOT/Reader",
                "jena-arq/testing/rdf-tests-cg/sparql",
                "jena-shex/src/test/files/spec",
            ],
            &["jsonld", "jsonld11", "srj", "json"],
            "jena",
            &mut docs,
        );
    }
    if let Some(ox) = oxigraph_dir() {
        docs_under(
            &ox,
            &["testsuite/oxigraph-tests/jsonld"],
            &["jsonld"],
            "oxigraph",
            &mut docs,
        );
    }
    if let Ok(w3c) = std::env::var("SPARKLES_JSONLD_TESTS") {
        docs_under(
            Path::new(&w3c),
            &[""],
            &["jsonld", "json"],
            "w3c",
            &mut docs,
        );
    }
    docs.extend(generated());
    docs
}

/// The dataset of a JSON-LD document read without fetching anything (relative IRIs
/// against a synthetic base); `None` when `oxjsonld` refuses it.
fn dataset(text: &str) -> Option<Vec<Quad>> {
    oxjsonld::JsonLdParser::new()
        .with_base_iri("http://sparkles-fmt.invalid/base/")
        .ok()?
        .for_slice(text)
        .collect::<Result<_, _>>()
        .ok()
}

/// A small deterministic generator (splitmix64).
struct Rng(u64);

impl Rng {
    fn new(seed: &str, round: u64) -> Rng {
        let mut h = 0xcbf2_9ce4_8422_2325u64 ^ round;
        for b in seed.bytes() {
            h = (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3);
        }
        Rng(h)
    }

    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

/// `text` with the members of every object whose order the rules fully decide under
/// `sort` (all but data under `@value`, and objects with two unknown `@` keys, which keep
/// their source order) shuffled, and random whitespace between the tokens.
fn shuffled(text: &str, rng: &mut Rng) -> String {
    let tokens = sparkles_fmt::jsonld::lex::lex(text);
    let tree = sparkles_fmt::jsonld::parse::parse(text, tokens).expect("a JSON document");
    let mut out = String::new();
    if let Some(v) = tree.child_nodes(tree.root()).next() {
        emit(&tree, v, Table::Node, rng, &mut out);
    }
    out
}

fn space(rng: &mut Rng, out: &mut String) {
    const WS: [&str; 5] = ["", " ", "\n", "\t", "  \r\n  "];
    out.push_str(WS[rng.below(WS.len())]);
}

fn emit(tree: &Tree<'_>, n: NodeId, table: Table, rng: &mut Rng, out: &mut String) {
    match tree.kind(n) {
        NodeKind::JsonObject => {
            let mut members: Vec<(NodeId, String)> = tree
                .child_nodes(n)
                .map(|m| {
                    let key = tree.token_text(tree.first_token(m).unwrap());
                    (m, sparkles_fmt::jsonld::print::unescape(key).into_owned())
                })
                .collect();
            let unknown = members.iter().filter(|(_, k)| table.rank(k).0 == 1).count();
            if table != Table::Literal && unknown <= 1 {
                for i in (1..members.len()).rev() {
                    members.swap(i, rng.below(i + 1));
                }
            }
            out.push('{');
            for (i, (m, key)) in members.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                space(rng, out);
                out.push_str(tree.token_text(tree.first_token(*m).unwrap()));
                space(rng, out);
                out.push(':');
                space(rng, out);
                let value = tree.child_nodes(*m).next().unwrap();
                emit(tree, value, table.child(key), rng, out);
                space(rng, out);
            }
            out.push('}');
        }
        NodeKind::JsonArray => {
            out.push('[');
            for (i, item) in tree.child_nodes(n).enumerate() {
                if i > 0 {
                    out.push(',');
                }
                space(rng, out);
                emit(tree, item, table, rng, out);
                space(rng, out);
            }
            out.push(']');
        }
        _ => out.push_str(tree.text(n)),
    }
}

/// What is wrong with one document, if anything; `Ok(false)` for a syntax error of the
/// reference parser, `Ok(true)` when it formatted.
fn check(doc: &Doc) -> Result<bool, String> {
    let defaults = Options::default();
    let sorted = Options {
        sort: true,
        ..Options::default()
    };
    let narrow = Options {
        line_width: 40,
        ..Options::default()
    };
    let input = dataset(&doc.text);
    let mut sorted_out = String::new();
    for (label, opts) in [("defaults", &defaults), ("sort", &sorted), ("w40", &narrow)] {
        let out = match format(&doc.text, Language::JsonLd, opts) {
            Ok(f) => f.text,
            Err(FormatError::Syntax { .. }) if label == "defaults" => return Ok(false),
            Err(e) => return Err(format!("{label}: {e}")),
        };
        match format(&out, Language::JsonLd, opts) {
            Ok(f) if f.text == out => {}
            Ok(_) => return Err(format!("{label}: the output formats differently")),
            Err(e) => return Err(format!("{label}: formatting the output: {e}")),
        }
        let r = json_reference(&doc.text).map_err(|e| e.to_string())?;
        json_equivalent(&r, &out).map_err(|e| format!("{label}: {e}"))?;
        if let Some(a) = &input {
            match dataset(&out) {
                Some(b) if isomorphic(a, &b) => {}
                Some(_) => return Err(format!("{label}: the dataset differs")),
                None => return Err(format!("{label}: oxjsonld refuses the output")),
            }
        }
        if label == "sort" {
            sorted_out = out;
        }
    }
    for round in 0..2 {
        let text = shuffled(&doc.text, &mut Rng::new(&doc.name, round));
        match format(&text, Language::JsonLd, &sorted) {
            Ok(f) if f.text == sorted_out => {}
            Ok(f) => {
                return Err(format!(
                    "a shuffled copy formats differently\n--- shuffled ---\n{text}\n--- got ---\n{}--- expected ---\n{sorted_out}",
                    f.text
                ));
            }
            Err(e) => return Err(format!("a shuffled copy: {e}")),
        }
    }
    Ok(true)
}

#[test]
fn corpus_formats_checks_and_converges() {
    let docs = corpus_docs();
    if docs.is_empty() {
        eprintln!("no JSON-LD corpus (set SPARKLES_W3C_DIR); skipped");
        return;
    }
    let known = corpus::fmt_known_failures();
    let results = corpus::par_map(&docs, check);
    let (mut formatted, mut syntax, mut rdf, mut known_hit) = (0, 0, 0, 0);
    let mut failures = Vec::new();
    let mut now_passing = Vec::new();
    for (doc, result) in docs.iter().zip(&results) {
        let key = format!("jsonld:{}", doc.name);
        let listed = known.contains_key(&key);
        match result {
            Ok(true) if listed => now_passing.push(key),
            Ok(true) => {
                formatted += 1;
                if dataset(&doc.text).is_some() {
                    rdf += 1;
                }
            }
            Ok(false) => syntax += 1,
            Err(_) if listed => known_hit += 1,
            Err(e) => failures.push(format!("{}: {e}", doc.name)),
        }
    }
    eprintln!(
        "JSON-LD corpus: {} documents, {formatted} formatted ({rdf} with their datasets \
         compared), {syntax} syntax errors, {known_hit} known failures, {} new failures",
        docs.len(),
        failures.len()
    );
    if !now_passing.is_empty() {
        eprintln!(
            "now passing (remove from tests/fmt-known-failures.txt):\n  {}",
            now_passing.join("\n  ")
        );
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

// ------------------------------------------------------------------ the pipeline ------

fn fmt(text: &str) -> Result<String, FormatError> {
    format(text, Language::JsonLd, &Options::default()).map(|f| f.text)
}

#[test]
fn scalars_and_empty_containers() {
    for (input, output) in [
        ("\"x\"", "\"x\"\n"),
        (" 42 ", "42\n"),
        ("null", "null\n"),
        ("\u{feff}{ }", "{}\n"),
        ("[\r\n]", "[]\n"),
        ("[[], {}]", "[\n  [],\n  {}\n]\n"),
        ("{\"a\":{\"b\":{}}}", "{ \"a\": { \"b\": {} } }\n"),
    ] {
        assert_eq!(fmt(input).unwrap(), output, "{input:?}");
    }
}

#[test]
fn line_breaks_and_long_values() {
    let long = "x".repeat(120);
    let out = fmt(&format!("{{\"@id\":\"{long}\"}}")).unwrap();
    assert_eq!(out, format!("{{\n  \"@id\": \"{long}\"\n}}\n"));
    // CRLF input, LF output
    assert_eq!(
        fmt("{\r\n\"a\": 1,\r\n\"b\": 2\r\n}\r\n").unwrap(),
        "{\n  \"a\": 1,\n  \"b\": 2\n}\n"
    );
    // indent-width
    let opts = Options {
        indent_width: 4,
        ..Options::default()
    };
    assert_eq!(
        format("{\"a\": [{\"b\": 1}]}", Language::JsonLd, &opts)
            .unwrap()
            .text,
        "{\n    \"a\": [\n        { \"b\": 1 }\n    ]\n}\n"
    );
}

#[test]
fn errors() {
    let e = fmt("{\"@id\": \"a\",\n \"@id\": \"b\"}").unwrap_err();
    assert!(
        matches!(&e, FormatError::Syntax { line: 2, column: 2, message, .. } if message == "duplicate key \"@id\""),
        "{e:?}"
    );
    let e = fmt("{\n  /* note */ \"a\": 1\n}").unwrap_err();
    assert!(
        matches!(&e, FormatError::Syntax { line: 2, column: 3, message, .. } if message == "comments are not allowed in JSON"),
        "{e:?}"
    );
    assert!(matches!(
        fmt("{\"a\": 1,}"),
        Err(FormatError::Syntax { .. })
    ));
    assert!(matches!(fmt(""), Err(FormatError::Syntax { .. })));
    // `# sparkles-fmt: ignore-file` is a comment, so not JSON either
    assert!(matches!(
        fmt("# sparkles-fmt: ignore-file\n{}"),
        Err(FormatError::Syntax { .. })
    ));
}

#[test]
fn deep_nesting() {
    let depth = sparkles_fmt::check::json::MAX_DEPTH;
    let doc = |d: usize| format!("{}1{}", "{\"a\":[".repeat(d / 2), "]}".repeat(d / 2));
    let out = fmt(&doc(depth)).unwrap();
    assert_eq!(fmt(&out).unwrap(), out);
    assert!(matches!(
        fmt(&doc(depth + 2)),
        Err(FormatError::Unsupported { .. })
    ));
}

#[test]
fn no_option_warnings_and_the_cursor_follows_its_token() {
    let opts = Options {
        sort: true,
        cursor: Some(6),
        ..Options::default()
    };
    let text = "{\"b\": 1, \"a\": 2, \"@id\": \"x\"}";
    let f = format(text, Language::JsonLd, &opts).unwrap();
    assert!(f.warnings.is_empty(), "{:?}", f.warnings);
    assert_eq!(f.text, "{\n  \"@id\": \"x\",\n  \"a\": 2,\n  \"b\": 1\n}\n");
    // the cursor was on `1`, the value of "b"
    let at = f.cursor.unwrap();
    assert_eq!(&f.text[at..at + 1], "1");
}

#[test]
fn a_changed_document_is_refused() {
    let r = json_reference("{\"a\": [1, 2]}").unwrap();
    assert_eq!(
        json_equivalent(&r, "{\"a\": [2, 1]}"),
        Err(FormatError::Unsafe {
            check: Check::Graph
        })
    );
}

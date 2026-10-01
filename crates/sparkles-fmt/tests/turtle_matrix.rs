//! The configuration matrix of Turtle and TriG: the safety checks and idempotence hold
//! under every option combination, not only the defaults.
//!
//! - every golden input of `tests/golden/{turtle,trig}` under the matrix of the keys that
//!   act on Turtle: `directive-style` × `type-shorthand` × `compact-iris` ×
//!   `quote-style` × `prefix-groups` (none, or `[["rdf", "rdfs", "xsd", "owl"]]`) × line
//!   width (40, 100), 64 combinations. `sort`, `prune-prefixes` and `turtle-layout` join
//!   when they act on Turtle;
//! - with `SPARKLES_FMT_MATRIX=1`, every positive W3C Turtle and TriG test and every SHACL
//!   file under the whole matrix (slow). The suites under the defaults and with every key
//!   flipped are `w3c_turtle.rs`.
//!
//! The documents go through the pipeline with the language set (`check::run` with
//! [`Turtle`]), as `format()` does for `.ttl` and `.trig` files.

mod corpus;

use sparkles_fmt::turtle::Turtle;
use sparkles_fmt::{DirectiveStyle, Language, Options, QuoteStyle, check, options};
use std::path::Path;

/// A named document and whether it is TriG.
type Doc = (String, String, bool);
/// A labelled option set.
type OptionSet = (String, Options);

/// The Turtle matrix, each combination with a label for failure messages.
fn turtle_matrix() -> Vec<OptionSet> {
    let mut v = Vec::new();
    for bits in 0u32..64 {
        let bit = |n: u32| bits & (1 << n) != 0;
        let o = Options {
            directive_style: match bit(0) {
                true => DirectiveStyle::Turtle,
                false => DirectiveStyle::Sparql,
            },
            type_shorthand: !bit(1),
            compact_iris: !bit(2),
            quote_style: match bit(3) {
                true => QuoteStyle::Preserve,
                false => QuoteStyle::Double,
            },
            prefix_groups: match bit(4) {
                true => vec![
                    ["rdf", "rdfs", "xsd", "owl"]
                        .iter()
                        .map(|s| s.to_string())
                        .collect(),
                ],
                false => Vec::new(),
            },
            line_width: if bit(5) { 40 } else { 100 },
            ..Options::default()
        };
        options::validate(&o).expect("valid options");
        let label = format!(
            "directive-style={:?} type-shorthand={} compact-iris={} quote-style={:?} \
             prefix-groups={:?} line-width={}",
            o.directive_style,
            o.type_shorthand,
            o.compact_iris,
            o.quote_style,
            o.prefix_groups,
            o.line_width
        );
        v.push((label, o));
    }
    v
}

/// The golden inputs of `tests/golden/{turtle,trig}`.
fn golden_inputs() -> Vec<Doc> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden");
    let mut v = Vec::new();
    for (lang, trig) in [("turtle", false), ("trig", true)] {
        for e in std::fs::read_dir(root.join(lang))
            .into_iter()
            .flatten()
            .flatten()
        {
            let p = e.path();
            let Some(file) = p.file_name().and_then(|f| f.to_str()) else {
                continue;
            };
            let Some((name, _)) = file.split_once(".in.") else {
                continue;
            };
            if let Ok(text) = std::fs::read_to_string(&p) {
                v.push((format!("golden/{lang}/{name}"), text, trig));
            }
        }
    }
    v.sort();
    v
}

/// `None` when `text` formats under `opts` to a fixpoint; else why not.
fn check(text: &str, trig: bool, opts: &Options) -> Option<String> {
    let lang = Turtle { trig };
    let out = match check::run(&lang, text, opts) {
        Ok(out) => out.text,
        Err(e) => return Some(e.to_string()),
    };
    match check::run(&lang, &out, opts) {
        Ok(again) if again.text == out => None,
        Ok(again) => Some(format!(
            "not a fixpoint\n--- first ---\n{out}--- second ---\n{}",
            again.text
        )),
        Err(e) => Some(format!("formatting the output: {e}")),
    }
}

/// Run every document under every option set; fail with the first few failures.
fn run(what: &str, docs: &[Doc], matrix: &[OptionSet]) {
    let jobs: Vec<(&Doc, &OptionSet)> = docs
        .iter()
        .flat_map(|d| matrix.iter().map(move |m| (d, m)))
        .collect();
    let failures: Vec<String> = corpus::par_map(&jobs, |((name, text, trig), (label, opts))| {
        check(text, *trig, opts).map(|e| format!("{name} [{label}]: {e}"))
    })
    .into_iter()
    .flatten()
    .collect();
    eprintln!(
        "{what}: {} documents × {} option sets, {} failures",
        docs.len(),
        matrix.len(),
        failures.len()
    );
    assert!(
        failures.is_empty(),
        "{} failures, the first ones:\n\n{}",
        failures.len(),
        failures
            .iter()
            .take(20)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n\n")
    );
}

#[test]
fn golden_inputs_under_the_matrix() {
    let docs = golden_inputs();
    assert!(docs.len() >= 5, "{} golden inputs", docs.len());
    let matrix = turtle_matrix();
    assert_eq!(matrix.len(), 64);
    run(
        "Turtle and TriG golden inputs under the matrix",
        &docs,
        &matrix,
    );
}

#[test]
fn corpus_under_the_full_matrix() {
    if std::env::var("SPARKLES_FMT_MATRIX").as_deref() != Ok("1") {
        eprintln!("set SPARKLES_FMT_MATRIX=1 to run the Turtle corpus under the whole matrix");
        return;
    }
    let known = corpus::fmt_known_failures();
    let mut docs: Vec<Doc> = Vec::new();
    if let Some(dir) = corpus::rdf::suite_dir() {
        for lang in [Language::Turtle, Language::TriG] {
            for c in corpus::rdf::cases(&dir, lang) {
                if !c.kind.is_positive() || known.contains_key(&c.rel) {
                    continue;
                }
                if let Ok(text) = std::fs::read_to_string(&c.path) {
                    docs.push((c.rel, text, lang == Language::TriG));
                }
            }
        }
    }
    if let Some(dir) = corpus::rdf::shacl_dir() {
        for (rel, path) in corpus::rdf::shacl_files(&dir) {
            let key = format!("shacl:{rel}");
            if known.contains_key(&key) {
                continue;
            }
            if let Ok(text) = std::fs::read_to_string(&path) {
                docs.push((key, text, false));
            }
        }
    }
    if docs.is_empty() {
        eprintln!("W3C RDF suite and SHACL tests not found: skipped");
        return;
    }
    run(
        "Turtle and TriG corpus under the matrix",
        &docs,
        &turtle_matrix(),
    );
}

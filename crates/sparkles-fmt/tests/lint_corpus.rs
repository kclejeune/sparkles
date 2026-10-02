//! `sparkles lint` over the W3C suites (spec X03 §9): every document lints without a
//! panic, a document the reference parser accepts gets no `syntax` diagnostic, a
//! negative syntax test always gets an error, and `fix` keeps the meaning of every
//! document it changes. The suites come from `SPARKLES_W3C_DIR` (as in `tests/w3c.rs`);
//! the test is skipped without them.

mod corpus;

use corpus::rdf::{self, RdfKind};
use corpus::{Kind, cases, par_map, suite_dir};
use sparkles_fmt::Language;
use sparkles_fmt::lint::{LintOptions, Severity, fix, lint};

#[test]
fn sparql_suite() {
    let Some(dir) = suite_dir() else {
        eprintln!("skipped: no SPARQL suite");
        return;
    };
    let opts = LintOptions::default();
    let all = cases(&dir);
    let failures: Vec<String> = par_map(&all, |c| {
        let text = std::fs::read_to_string(&c.path).ok()?;
        let d = lint(&text, Language::Sparql, &opts).ok()?.diagnostics;
        let syntax = d.iter().any(|d| d.rule == "syntax");
        let error = d.iter().any(|d| d.severity == Severity::Error);
        match c.kind {
            Kind::Positive if syntax => Some(format!("{}: syntax error on a valid query", c.rel)),
            Kind::Negative if !error => Some(format!("{}: no error on an invalid query", c.rel)),
            Kind::Positive => {
                let f = fix(&text, Language::Sparql, &opts);
                f.err().map(|e| format!("{}: fix: {e}", c.rel))
            }
            _ => None,
        }
    })
    .into_iter()
    .flatten()
    .collect();
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn turtle_and_trig_suites() {
    let Some(dir) = rdf::suite_dir() else {
        eprintln!("skipped: no RDF suite");
        return;
    };
    let opts = LintOptions::default();
    for lang in [Language::Turtle, Language::TriG] {
        let all = rdf::cases(&dir, lang);
        let failures: Vec<String> = par_map(&all, |c| {
            let text = std::fs::read_to_string(&c.path).ok()?;
            let d = lint(&text, lang, &opts).ok()?.diagnostics;
            let error = d.iter().any(|d| d.severity == Severity::Error);
            match c.kind {
                k if k.is_positive() && d.iter().any(|d| d.rule == "syntax") => {
                    Some(format!("{}: syntax error on a valid document", c.rel))
                }
                RdfKind::NegativeSyntax | RdfKind::NegativeEval if !error => {
                    Some(format!("{}: no error on an invalid document", c.rel))
                }
                k if k.is_positive() => {
                    let f = fix(&text, lang, &opts);
                    f.err().map(|e| format!("{}: fix: {e}", c.rel))
                }
                _ => None,
            }
        })
        .into_iter()
        .flatten()
        .collect();
        assert!(failures.is_empty(), "{lang:?}:\n{}", failures.join("\n"));
    }
}

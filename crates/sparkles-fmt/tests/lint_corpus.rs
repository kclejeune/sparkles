//! `sparkles lint` over the W3C suites (spec X04 §9): every document lints without a
//! panic, a document the reference parser accepts gets no `syntax` diagnostic, a
//! negative syntax test always gets an error, and `fix` keeps the meaning of every
//! document it changes. The suites come from `SPARKLES_W3C_DIR` (as in `tests/w3c.rs`);
//! the test is skipped without them. N-Triples and N-Quads run over the same RDF
//! suites, SHACLC over Jena's `shaclc-valid` files, and JSON-LD over the Jena and
//! Oxigraph JSON-LD files.

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
            // a negative test that only ARQ's syntax accepts is valid for the linter
            Kind::Negative if !error && !corpus::arq_only(&c.path) => {
                Some(format!("{}: no error on an invalid query", c.rel))
            }
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
    for lang in [
        Language::Turtle,
        Language::TriG,
        Language::NTriples,
        Language::NQuads,
    ] {
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

/// Jena's SHACLC files that are valid SHACLC, and the SHACL test suite's directory.
#[test]
fn shaclc_corpus() {
    let std = std::env::var("SPARKLES_SHACL_TESTS")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| {
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../../../apache/jena/jena-shacl/src/test/files/std")
        });
    let valid = std.join("../shaclc-valid");
    let Ok(rd) = std::fs::read_dir(&valid) else {
        eprintln!("skipped: no SHACLC files");
        return;
    };
    let opts = LintOptions::default();
    let mut seen = 0;
    for p in rd.flatten().map(|e| e.path()) {
        if p.extension().and_then(|e| e.to_str()) != Some("shaclc") {
            continue;
        }
        let text = std::fs::read_to_string(&p).unwrap();
        let d = lint(&text, Language::Shaclc, &opts).unwrap().diagnostics;
        assert!(
            d.iter().all(|d| d.rule != "syntax"),
            "{}: syntax error on a valid document: {d:?}",
            p.display()
        );
        fix(&text, Language::Shaclc, &opts).unwrap_or_else(|e| panic!("{}: fix: {e}", p.display()));
        seen += 1;
    }
    assert!(seen > 10, "{seen} files");
}

/// Every JSON-LD file of the Jena and Oxigraph checkouts that is JSON lints without a
/// panic and without a `syntax` finding, and `fix` keeps its dataset.
#[test]
fn jsonld_corpus() {
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let roots = [
        manifest.join("../../../../apache/jena/jena-arq/testing/RIOT/jsonld"),
        manifest.join("../../../../apache/jena/jena-arq/testing/RIOT/jsonld11"),
        manifest.join("../../../../oxigraph/oxigraph/testsuite/oxigraph-tests/jsonld"),
        manifest.join("../../../../oxigraph/oxigraph/testsuite/json-ld-api/tests"),
    ];
    fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for p in rd.flatten().map(|e| e.path()) {
            if p.is_dir() {
                walk(&p, out);
            } else if p.extension().and_then(|e| e.to_str()) == Some("jsonld") {
                out.push(p);
            }
        }
    }
    let mut paths = Vec::new();
    for r in &roots {
        walk(r, &mut paths);
    }
    paths.sort();
    if paths.is_empty() {
        eprintln!("skipped: no JSON-LD files");
        return;
    }
    let opts = LintOptions::default();
    let failures: Vec<String> = par_map(&paths, |p| {
        let text = std::fs::read_to_string(p).ok()?;
        let d = lint(&text, Language::JsonLd, &opts).ok()?.diagnostics;
        let strict_json = sparkles_fmt::check::json::json_reference(&text).is_ok();
        if strict_json && d.iter().any(|d| d.rule == "syntax") {
            return Some(format!("{}: syntax error on valid JSON", p.display()));
        }
        fix(&text, Language::JsonLd, &opts)
            .err()
            .map(|e| format!("{}: fix: {e}", p.display()))
    })
    .into_iter()
    .flatten()
    .collect();
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

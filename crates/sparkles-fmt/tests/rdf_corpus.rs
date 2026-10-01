//! The W3C RDF test suites and the SHACL test files against the reference parsers of the
//! RDF formats (oxttl) and the graph comparison the safety check uses, before any of them
//! formats: every positive input parses, every negative one is a syntax error, every
//! evaluation input parses to a graph isomorphic to its expected result, and every
//! canonical form test's result is a valid document with the same graph. The formatter's
//! own suites over these corpora (format, check, fixpoint) build on the same
//! classification ([`corpus::rdf`]).
//!
//! Inputs where oxttl itself disagrees with the suite are listed with a reason in
//! `tests/fmt-known-failures.txt` under `reference:` keys; listed ones that pass are
//! reported so the list can shrink.

mod corpus;

use corpus::rdf::{self, RdfCase, RdfKind};
use oxrdf::{GraphName, Quad};
use sparkles_fmt::check::graph::{isomorphic, parse};
use sparkles_fmt::{FormatError, Language};

/// The quads of an evaluation input with the base IRI its expected result assumes (the
/// formatter's own parse uses a synthetic one).
fn parse_at(text: &str, lang: Language, base: &str) -> Result<Vec<Quad>, String> {
    let e = |e: oxttl::TurtleSyntaxError| e.to_string();
    match lang {
        Language::TriG => oxttl::TriGParser::new()
            .with_base_iri(base)
            .map_err(|e| e.to_string())?
            .for_slice(text)
            .collect::<Result<_, _>>()
            .map_err(e),
        _ => oxttl::TurtleParser::new()
            .with_base_iri(base)
            .map_err(|e| e.to_string())?
            .for_slice(text)
            .map(|t| t.map(|t| t.in_graph(GraphName::DefaultGraph)))
            .collect::<Result<_, _>>()
            .map_err(e),
    }
}

/// What is wrong with one case, if anything.
fn check(case: &RdfCase) -> Option<String> {
    let text = match std::fs::read(&case.path).map(String::from_utf8) {
        Ok(Ok(t)) => t,
        // not UTF-8 is a syntax error of every RDF syntax
        Ok(Err(_)) if !case.kind.is_positive() => return None,
        Ok(Err(_)) => return Some("not UTF-8".into()),
        Err(e) => return Some(e.to_string()),
    };
    let parsed = parse(&text, case.language);
    match (case.kind.is_positive(), parsed) {
        (true, Err(e)) => Some(format!("rejected: {e}")),
        (false, Ok(_)) => Some("accepted a negative test".into()),
        (false, Err(FormatError::Syntax { .. })) => None,
        (false, Err(e)) => Some(format!("not a syntax error: {e}")),
        (true, Ok(quads)) => {
            let result = case.result.as_ref()?;
            let quads = match (&case.base, case.kind) {
                (Some(base), RdfKind::Eval) => match parse_at(&text, case.language, base) {
                    Ok(q) => q,
                    Err(e) => return Some(format!("rejected at its base: {e}")),
                },
                _ => quads,
            };
            let lang = match case.language {
                Language::TriG | Language::NQuads => Language::NQuads,
                _ => Language::NTriples,
            };
            let expected = std::fs::read_to_string(result)
                .map_err(|e| e.to_string())
                .and_then(|t| parse(&t, lang).map_err(|e| e.to_string()));
            match expected {
                Err(e) => Some(format!("the expected result: {e}")),
                Ok(exp) if !isomorphic(&quads, &exp) => {
                    Some("not isomorphic to the expected result".into())
                }
                Ok(_) => None,
            }
        }
    }
}

#[test]
fn reference_parsers_agree_with_the_rdf_suites() {
    let Some(dir) = rdf::suite_dir() else {
        eprintln!("W3C RDF suite not found (set SPARKLES_RDF_TESTS_DIR): skipped");
        return;
    };
    let known = corpus::fmt_known_failures();
    let mut failures = Vec::new();
    let mut fixed = Vec::new();
    for lang in [
        Language::Turtle,
        Language::TriG,
        Language::NTriples,
        Language::NQuads,
    ] {
        let cases = rdf::cases(&dir, lang);
        let count = |k: RdfKind| cases.iter().filter(|c| c.kind == k).count();
        eprintln!(
            "{}: {} cases ({} positive syntax, {} negative syntax, {} evaluation, {} negative evaluation, {} canonical form)",
            lang.name(),
            cases.len(),
            count(RdfKind::PositiveSyntax),
            count(RdfKind::NegativeSyntax),
            count(RdfKind::Eval),
            count(RdfKind::NegativeEval),
            count(RdfKind::C14n),
        );
        assert!(
            cases.iter().any(|c| c.kind.is_positive())
                && cases.iter().any(|c| !c.kind.is_positive()),
            "{}: no positive or no negative tests under {}",
            lang.name(),
            dir.display()
        );
        let results = corpus::par_map(&cases, check);
        for (case, problem) in cases.iter().zip(results) {
            let key = format!("reference:{}", case.rel);
            match (problem, known.contains_key(&key)) {
                (Some(p), false) => failures.push(format!("{}: {p}", case.rel)),
                (None, true) => fixed.push(key),
                _ => {}
            }
        }
    }
    if !fixed.is_empty() {
        eprintln!(
            "passing now (remove from fmt-known-failures.txt): {}",
            fixed.join(", ")
        );
    }
    assert!(
        failures.is_empty(),
        "{} reference parse disagreements:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn shacl_test_files_are_turtle() {
    let Some(dir) = rdf::shacl_dir() else {
        eprintln!("SHACL tests not found (set SPARKLES_SHACL_TESTS): skipped");
        return;
    };
    let files = rdf::shacl_files(&dir);
    assert!(!files.is_empty(), "no .ttl files under {}", dir.display());
    let failures: Vec<String> = files
        .iter()
        .filter_map(|(rel, path)| {
            let text = std::fs::read_to_string(path).ok()?;
            parse(&text, Language::Turtle)
                .err()
                .map(|e| format!("{rel}: {e}"))
        })
        .collect();
    eprintln!("SHACL: {} Turtle files", files.len());
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

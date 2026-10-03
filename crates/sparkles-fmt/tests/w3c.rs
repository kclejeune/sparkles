//! The W3C SPARQL test suites (sparql10, sparql11, sparql12) through the formatter,
//! classified by their manifests:
//!
//! - every positive syntax test and every query and update of an evaluation test formats,
//!   passes the safety checks and is a fixpoint (`format(out) == out`);
//! - every negative syntax test is a syntax error;
//! - every other `.rq`/`.ru` file formats or is a syntax error;
//! - nothing panics.
//!
//! Exceptions are listed with a reason in `tests/fmt-known-failures.txt`; listed files
//! that pass are reported so the list can shrink. Negative tests the reference parser
//! wrongly accepts are taken from the engine's `w3c-known-failures.txt`.
//!
//! Set `SPARKLES_W3C_DIR` to the `rdf-tests-cg/sparql` directory (as for the engine's
//! suite); without it the default sibling checkout is used, and the tests are skipped
//! when that is absent.

mod corpus;

use corpus::{Case, Kind};
use sparkles_fmt::check::{sparql_equivalent, sparql_reference};
use sparkles_fmt::lex::{LexMode, lex};
use sparkles_fmt::{FormatError, Language, Options, format};
use std::panic::{AssertUnwindSafe, catch_unwind};

fn suite() -> Option<Vec<Case>> {
    let Some(dir) = corpus::suite_dir() else {
        eprintln!("W3C suite not found (set SPARKLES_W3C_DIR): skipped");
        return None;
    };
    let cases = corpus::cases(&dir);
    assert!(!cases.is_empty(), "no queries under {}", dir.display());
    Some(cases)
}

/// What became of one file.
enum Outcome {
    /// formatted; whether the output differs from the input
    Formatted {
        changed: bool,
    },
    Syntax,
    /// not UTF-8 (a few negative tests)
    NotText,
    Failed(String),
}

/// Format `text`, then its output, under `opts`: the output must be a fixpoint.
pub fn format_twice(text: &str, opts: &Options) -> Result<bool, FormatError> {
    let out = format(text, Language::Sparql, opts)?;
    let again = format(&out.text, Language::Sparql, opts)?;
    if again.text != out.text {
        return Err(FormatError::Unsafe {
            check: sparkles_fmt::Check::Idempotence,
        });
    }
    Ok(out.changed)
}

fn run(case: &Case) -> Outcome {
    let Ok(text) = std::fs::read_to_string(&case.path) else {
        return Outcome::NotText;
    };
    match catch_unwind(AssertUnwindSafe(|| {
        format_twice(&text, &Options::default())
    })) {
        Ok(Ok(changed)) => Outcome::Formatted { changed },
        Ok(Err(FormatError::Syntax { .. })) => Outcome::Syntax,
        Ok(Err(e)) => Outcome::Failed(e.to_string()),
        Err(_) => Outcome::Failed("panicked".to_string()),
    }
}

/// A negative test that only ARQ's syntax accepts, such as `constructwhere06`
/// (`CONSTRUCT WHERE { GRAPH … }`). The formatter takes ARQ's syntax, as the engine does by
/// default, and the engine's W3C harness likewise holds negative tests to strict SPARQL.
fn arq_only(case: &Case) -> bool {
    let Ok(text) = std::fs::read_to_string(&case.path) else {
        return false;
    };
    // the tests use relative IRIs, resolved against the file's URL
    let base = format!("file://{}", case.path.display());
    let Ok(arq) = spargebra::SparqlParser::new().with_base_iri(&base) else {
        return false;
    };
    let strict = arq.clone().with_arq_syntax(false);
    let parses = |p: &spargebra::SparqlParser| {
        p.clone().parse_query(&text).is_ok() || p.clone().parse_update(&text).is_ok()
    };
    !parses(&strict) && parses(&arq)
}

#[test]
fn corpus_by_manifest() {
    let Some(cases) = suite() else { return };
    let known = corpus::fmt_known_failures();
    let w3c_known = corpus::w3c_known_failures();
    let outcomes = corpus::par_map(&cases, run);

    let mut failures = Vec::new();
    let mut now_passing = Vec::new();
    let count = |kind: Kind| cases.iter().filter(|c| c.kind == kind).count();
    let (positives, negatives, unlisted) = (
        count(Kind::Positive),
        count(Kind::Negative),
        count(Kind::Unlisted),
    );
    let (mut formatted, mut changed, mut rejected, mut syntax_ok, mut known_hit) = (0, 0, 0, 0, 0);
    for (case, outcome) in cases.iter().zip(&outcomes) {
        let listed = known.contains_key(&case.rel);
        let failure = match (case.kind, outcome) {
            (_, Outcome::Failed(e)) if e == "panicked" => Some("panicked".to_string()),
            (Kind::Positive | Kind::Unlisted, Outcome::Formatted { changed: c }) => {
                formatted += 1;
                changed += usize::from(*c);
                None
            }
            (Kind::Positive, Outcome::Syntax) => Some("rejected as a syntax error".to_string()),
            (Kind::Positive, Outcome::NotText) => Some("not UTF-8".to_string()),
            (Kind::Unlisted, Outcome::Syntax | Outcome::NotText) => {
                syntax_ok += 1;
                None
            }
            (Kind::Negative, Outcome::Syntax | Outcome::NotText) => {
                rejected += 1;
                None
            }
            (Kind::Negative, Outcome::Formatted { .. }) => {
                if case.ids.iter().any(|id| w3c_known.contains(id)) || arq_only(case) {
                    rejected += 1;
                    None
                } else {
                    Some("a negative syntax test was accepted".to_string())
                }
            }
            (_, Outcome::Failed(e)) => Some(e.clone()),
        };
        match failure {
            Some(_) if listed => known_hit += 1,
            Some(e) => failures.push(format!("{}: {e}", case.rel)),
            None if listed => now_passing.push(case.rel.clone()),
            None => {}
        }
    }
    eprintln!(
        "W3C SPARQL through the formatter: {} files\n  \
         positive {positives}, negative {negatives}, in no manifest {unlisted}\n  \
         {formatted} formatted ({changed} changed), {rejected} negative tests rejected, \
         {syntax_ok} unlisted syntax errors, {known_hit} known failures, {} new failures",
        cases.len(),
        failures.len()
    );
    if !now_passing.is_empty() {
        eprintln!(
            "now passing (remove from tests/fmt-known-failures.txt):\n  {}",
            now_passing.join("\n  ")
        );
    }
    assert!(
        positives > 1000 && negatives > 150,
        "the manifests were not read: {positives} positive, {negatives} negative"
    );
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn negative_tests_include_the_codepoint_escapes() {
    let Some(cases) = suite() else { return };
    for n in 1..=4 {
        let rel = format!("sparql12/codepoint-escapes/codepoint-esc-0{n}-bad.rq");
        let case = cases.iter().find(|c| c.rel == rel).expect(&rel);
        assert_eq!(case.kind, Kind::Negative, "{rel}");
        let text = std::fs::read_to_string(&case.path).unwrap();
        assert!(
            matches!(
                format(&text, Language::Sparql, &Options::default()),
                Err(FormatError::Syntax { .. })
            ),
            "{rel}"
        );
    }
}

/// Two parses of one document make up different blank nodes and variables; the algebra
/// check must still find them equal. Run on every positive file, so the canonical form
/// is known to cover everything spargebra makes up.
#[test]
fn every_parse_equals_itself() {
    let Some(cases) = suite() else { return };
    let positive: Vec<&Case> = cases.iter().filter(|c| c.kind == Kind::Positive).collect();
    let results = corpus::par_map(&positive, |c| {
        let text = std::fs::read_to_string(&c.path).ok()?;
        let r = match sparql_reference(&text, &lex(&text, LexMode::Sparql)) {
            Ok(r) => r,
            Err(e) => return Some(format!("{}: {e}", c.rel)),
        };
        sparql_equivalent(&r, &text)
            .err()
            .map(|e| format!("{}: {e}", c.rel))
    });
    let failures: Vec<String> = results.into_iter().flatten().collect();
    eprintln!(
        "algebra self-comparison: {} positive files, {} differ",
        positive.len(),
        failures.len()
    );
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

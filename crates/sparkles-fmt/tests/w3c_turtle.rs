//! The W3C Turtle and TriG test suites (`rdf-tests-cg/rdf`, RDF 1.1 and 1.2) and the SHACL
//! test files through the formatter, classified by their manifests:
//!
//! - every positive syntax test and every evaluation input formats, passes the safety
//!   checks (graph, comments, idempotence) and is a fixpoint (`format(out) == out`),
//!   under the default options and with every style key that acts on Turtle flipped;
//! - every negative syntax and negative evaluation test is a positioned syntax error;
//! - every SHACL shapes or data graph formats the same way;
//! - a comment after any token of those inputs keeps formatting safe (comment sweeps);
//! - nothing panics.
//!
//! Exceptions are listed with a reason in `tests/fmt-known-failures.txt` (the path under
//! the RDF suite directory, or `shacl:<path>` under the SHACL one); listed files that pass
//! are reported so the list can shrink.
//!
//! The suites come from `SPARKLES_RDF_TESTS_DIR` (or the `rdf` directory next to
//! `SPARKLES_W3C_DIR`) and `SPARKLES_SHACL_TESTS`; each test is skipped when its corpus
//! is absent.

mod corpus;

use corpus::rdf::{self, RdfCase};
use sparkles_fmt::check;
use sparkles_fmt::lex::{LexMode, TokenKind, lex};
use sparkles_fmt::turtle::Turtle;
use sparkles_fmt::{Check, DirectiveStyle, FormatError, Language, Options, QuoteStyle};
use std::panic::{AssertUnwindSafe, catch_unwind};

/// Every style key that acts on Turtle and TriG, away from its default.
fn flipped() -> Options {
    Options {
        line_width: 40,
        indent_width: 4,
        directive_style: DirectiveStyle::Turtle,
        prefix_groups: vec![
            vec!["rdf".into(), "rdfs".into(), "xsd".into(), "owl".into()],
            vec!["sh".into(), "".into()],
        ],
        type_shorthand: false,
        compact_iris: false,
        quote_style: QuoteStyle::Preserve,
        ..Options::default()
    }
}

/// Format `text`, then its output: the output must be a fixpoint. Whether it changed.
fn format_twice(text: &str, trig: bool, opts: &Options) -> Result<bool, FormatError> {
    let lang = Turtle { trig };
    let out = check::run(&lang, text, opts)?;
    let again = check::run(&lang, &out.text, opts)?;
    if again.text != out.text {
        return Err(FormatError::Unsafe {
            check: Check::Idempotence,
        });
    }
    Ok(out.changed)
}

/// What became of one input.
enum Outcome {
    /// formatted under both option sets; whether the default output differs from the
    /// input
    Formatted {
        changed: bool,
    },
    /// a syntax error with a position
    Syntax,
    /// not UTF-8 (a few negative tests)
    NotText,
    Failed(String),
}

fn run(text: Result<String, std::io::Error>, trig: bool) -> Outcome {
    let Ok(text) = text else {
        return Outcome::NotText;
    };
    let result = catch_unwind(AssertUnwindSafe(|| {
        let changed = format_twice(&text, trig, &Options::default())?;
        format_twice(&text, trig, &flipped()).map_err(|e| match e {
            FormatError::Syntax { .. } => e,
            e => FormatError::Unsupported {
                message: format!("with every key flipped: {e}"),
                line: 0,
                column: 0,
            },
        })?;
        Ok::<bool, FormatError>(changed)
    }));
    match result {
        Ok(Ok(changed)) => Outcome::Formatted { changed },
        Ok(Err(FormatError::Syntax { line, column, .. })) if line > 0 && column > 0 => {
            Outcome::Syntax
        }
        Ok(Err(e @ FormatError::Syntax { .. })) => Outcome::Failed(format!("unpositioned {e}")),
        Ok(Err(e)) => Outcome::Failed(e.to_string()),
        Err(_) => Outcome::Failed("panicked".to_string()),
    }
}

fn read(path: &std::path::Path) -> Result<String, std::io::Error> {
    std::fs::read(path).and_then(|b| {
        String::from_utf8(b).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    })
}

#[test]
fn turtle_and_trig_suites() {
    let Some(dir) = rdf::suite_dir() else {
        eprintln!(
            "W3C RDF suite not found (set SPARKLES_RDF_TESTS_DIR or SPARKLES_W3C_DIR): skipped"
        );
        return;
    };
    let known = corpus::fmt_known_failures();
    let mut failures = Vec::new();
    let mut now_passing = Vec::new();
    for lang in [Language::Turtle, Language::TriG] {
        let cases: Vec<RdfCase> = rdf::cases(&dir, lang);
        let outcomes = corpus::par_map(&cases, |c| run(read(&c.path), lang == Language::TriG));
        let (mut positives, mut negatives, mut formatted, mut changed, mut rejected, mut known_hit) =
            (0, 0, 0, 0, 0, 0);
        for (case, outcome) in cases.iter().zip(&outcomes) {
            let positive = case.kind.is_positive();
            match positive {
                true => positives += 1,
                false => negatives += 1,
            }
            let failure = match (positive, outcome) {
                (_, Outcome::Failed(e)) => Some(e.clone()),
                (true, Outcome::Formatted { changed: c }) => {
                    formatted += 1;
                    changed += usize::from(*c);
                    None
                }
                (true, Outcome::Syntax) => Some("rejected as a syntax error".to_string()),
                (true, Outcome::NotText) => Some("not UTF-8".to_string()),
                (false, Outcome::Syntax | Outcome::NotText) => {
                    rejected += 1;
                    None
                }
                (false, Outcome::Formatted { .. }) => {
                    Some("a negative test was accepted".to_string())
                }
            };
            let listed = known.contains_key(&case.rel);
            match failure {
                Some(_) if listed => known_hit += 1,
                Some(e) => failures.push(format!("{}: {e}", case.rel)),
                None if listed => now_passing.push(case.rel.clone()),
                None => {}
            }
        }
        eprintln!(
            "W3C {} through the formatter: {} cases (positive {positives}, negative {negatives}); \
             {formatted} formatted ({changed} changed), {rejected} negative tests rejected, \
             {known_hit} known failures",
            lang.display_name(),
            cases.len(),
        );
        assert!(
            positives > 100 && negatives > 30,
            "{}: the manifests were not read: {positives} positive, {negatives} negative",
            lang.name()
        );
    }
    if !now_passing.is_empty() {
        eprintln!(
            "now passing (remove from tests/fmt-known-failures.txt):\n  {}",
            now_passing.join("\n  ")
        );
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn shacl_files() {
    let Some(dir) = rdf::shacl_dir() else {
        eprintln!("SHACL tests not found (set SPARKLES_SHACL_TESTS): skipped");
        return;
    };
    let known = corpus::fmt_known_failures();
    let files = rdf::shacl_files(&dir);
    let outcomes = corpus::par_map(&files, |(_, path)| run(read(path), false));
    let mut failures = Vec::new();
    let mut now_passing = Vec::new();
    let (mut formatted, mut changed) = (0, 0);
    for ((rel, _), outcome) in files.iter().zip(&outcomes) {
        let key = format!("shacl:{rel}");
        let failure = match outcome {
            Outcome::Formatted { changed: c } => {
                formatted += 1;
                changed += usize::from(*c);
                None
            }
            Outcome::Syntax => Some("rejected as a syntax error".to_string()),
            Outcome::NotText => Some("not UTF-8".to_string()),
            Outcome::Failed(e) => Some(e.clone()),
        };
        match (failure, known.contains_key(&key)) {
            (Some(e), false) => failures.push(format!("{rel}: {e}")),
            (None, true) => now_passing.push(key),
            _ => {}
        }
    }
    eprintln!(
        "SHACL through the formatter: {} files, {formatted} formatted ({changed} changed)",
        files.len()
    );
    if !now_passing.is_empty() {
        eprintln!(
            "now passing (remove from tests/fmt-known-failures.txt):\n  {}",
            now_passing.join("\n  ")
        );
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// The printer copies language tags as written: the graph check cannot see a change of
/// their case, since oxttl lowercases tags when parsing.
#[test]
fn language_tags_are_copied() {
    let text = "<http://e/s> <http://e/p> \"x\"@EN-gb, 'y'@en--rtl .\n";
    let out = check::run(&Turtle { trig: false }, text, &Options::default()).unwrap();
    assert_eq!(
        out.text,
        "<http://e/s>\n  <http://e/p> \"x\"@EN-gb, \"y\"@en--rtl ;\n.\n"
    );
}

/// A comment after one significant token: the input with it either formats (a fixpoint,
/// every check passed) or is a syntax error. The number of inputs that formatted, or the
/// first failure.
fn sweep(name: &str, text: &str, trig: bool, step: usize) -> Result<usize, String> {
    let tokens = lex(text, LexMode::Turtle);
    let ends: Vec<usize> = tokens
        .iter()
        .filter(|t| !t.kind.is_trivia() && t.kind != TokenKind::Eof)
        .map(|t| t.end())
        .collect();
    let mut formatted = 0;
    for &end in ends.iter().step_by(step) {
        for comment in [" # c\n", "\n# c\n"] {
            let mut s = text.to_string();
            s.insert_str(end, comment);
            match catch_unwind(AssertUnwindSafe(|| {
                format_twice(&s, trig, &Options::default())
            })) {
                Ok(Ok(_)) => formatted += 1,
                Ok(Err(FormatError::Syntax { .. })) => {}
                Ok(Err(e)) => return Err(format!("{name}: {comment:?} at byte {end}: {e}")),
                Err(_) => return Err(format!("{name}: {comment:?} at byte {end}: panicked")),
            }
        }
    }
    Ok(formatted)
}

/// Comment sweeps: a trailing comment, and a comment on a line of its own, after every
/// significant token of the golden inputs and the W3C inputs (every few tokens of the
/// SHACL files): no comment is lost or moves twice.
#[test]
fn comment_sweeps() {
    let mut inputs: Vec<(String, String, bool, usize)> = Vec::new();
    let golden = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden");
    for (dir, trig) in [("turtle", false), ("trig", true)] {
        for e in std::fs::read_dir(golden.join(dir)).unwrap().flatten() {
            let p = e.path();
            if p.to_string_lossy().contains(".in.") {
                inputs.push((p.display().to_string(), read(&p).unwrap(), trig, 1));
            }
        }
    }
    let known = corpus::fmt_known_failures();
    if let Some(dir) = rdf::suite_dir() {
        for lang in [Language::Turtle, Language::TriG] {
            for c in rdf::cases(&dir, lang) {
                if !c.kind.is_positive() || known.contains_key(&c.rel) {
                    continue;
                }
                if let Ok(text) = read(&c.path) {
                    inputs.push((c.rel, text, lang == Language::TriG, 1));
                }
            }
        }
    }
    if let Some(dir) = rdf::shacl_dir() {
        for (rel, path) in rdf::shacl_files(&dir) {
            if let Ok(text) = read(&path) {
                inputs.push((format!("shacl:{rel}"), text, false, 13));
            }
        }
    }
    let results = corpus::par_map(&inputs, |(name, text, trig, step)| {
        sweep(name, text, *trig, *step)
    });
    let formatted: usize = results.iter().flatten().sum();
    let failures: Vec<String> = results.into_iter().filter_map(Result::err).collect();
    eprintln!(
        "comment sweeps: {} inputs, {formatted} documents with a comment formatted, {} failures",
        inputs.len(),
        failures.len()
    );
    assert!(
        formatted > 1000,
        "the sweeps formatted only {formatted} documents"
    );
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

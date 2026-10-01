//! The W3C N-Triples and N-Quads suites (RDF 1.1 and 1.2) through the formatter: every
//! positive input formats, keeps its quads (blank node labels included) and its
//! comments, and is a fixpoint, by default, sorted and canonicalized, in memory and
//! streamed; every negative input is a syntax error at the position oxttl gives for the
//! whole document; every canonical form test's input formats to its expected result
//! byte for byte (comments aside, which the canonical form has none of). The N-Triples
//! and N-Quads results of the Turtle and TriG evaluation tests are positive inputs too.
//!
//! Documented exceptions are listed in `tests/fmt-known-failures.txt` under the path
//! below the suite directory; listed ones that pass are reported so the list can shrink.

mod corpus;

use corpus::rdf::{self, RdfCase, RdfKind};
use sparkles_fmt::check::{comments, graph};
use sparkles_fmt::lex::LexMode;
use sparkles_fmt::lines::scan::{LineKind, scan};
use sparkles_fmt::{FormatError, Language, LinesConfig, Options, format, format_lines};
use std::path::PathBuf;

/// One input to check.
struct Input {
    path: PathBuf,
    rel: String,
    lang: Language,
    kind: RdfKind,
    /// a canonical form test's expected output
    c14n: Option<PathBuf>,
}

fn opts(sort: bool, canonicalize: bool) -> Options {
    Options {
        sort,
        canonicalize,
        ..Options::default()
    }
}

/// The text without its comments and blank lines (what the canonical form keeps).
fn without_comments(text: &str) -> String {
    text.lines()
        .filter_map(|l| {
            let s = scan(l);
            (s.kind == LineKind::Statement).then(|| format!("{}\n", &l[s.body]))
        })
        .collect()
}

/// What is wrong with formatting `text` under `o`, if anything.
fn check_formatting(text: &str, lang: Language, o: &Options) -> Result<String, String> {
    let f = format(text, lang, o).map_err(|e| format!("rejected: {e}"))?;
    let input = graph::parse(text, lang).map_err(|e| format!("the reference parse: {e}"))?;
    let output = graph::parse(&f.text, lang).map_err(|e| format!("the output: {e}"))?;
    if !o.sort && !o.canonicalize && input != output {
        return Err("the output's quads differ".into());
    }
    if !graph::isomorphic(&input, &output) {
        return Err("the output's dataset differs".into());
    }
    if o.canonicalize {
        if f.text.lines().any(|l| scan(l).comment.is_some()) {
            return Err("a comment survived canonicalization".into());
        }
    } else {
        comments::same(text, &f.text, LexMode::Turtle).map_err(|e| e.to_string())?;
    }
    if f.changed != (f.text != text) {
        return Err("`changed` is wrong".into());
    }
    match format(&f.text, lang, o) {
        Ok(again) if again.text == f.text && !again.changed => {}
        Ok(_) => return Err("not a fixpoint".into()),
        Err(e) => return Err(format!("formatting the output: {e}")),
    }
    // streamed in small pieces, the same output
    let mut streamed = Vec::new();
    let cfg = LinesConfig {
        sort_memory: 512,
        threads: 2,
        ..LinesConfig::default()
    };
    let stats = format_lines(text.as_bytes(), &mut streamed, lang, o, &cfg)
        .map_err(|e| format!("streamed: {e}"))?;
    if streamed != f.text.as_bytes() || stats.changed != f.changed {
        return Err("streaming gives another output".into());
    }
    Ok(f.text)
}

/// What is wrong with one input, if anything.
fn check(input: &Input) -> Option<String> {
    let text = match std::fs::read(&input.path).map(String::from_utf8) {
        Ok(Ok(t)) => t,
        Ok(Err(_)) if !input.kind.is_positive() => return None,
        Ok(Err(_)) => return Some("not UTF-8".into()),
        Err(e) => return Some(e.to_string()),
    };
    if !input.kind.is_positive() {
        let reference = graph::parse(&text, input.lang);
        return match (format(&text, input.lang, &Options::default()), reference) {
            (Ok(_), _) => Some("accepted a negative test".into()),
            (
                Err(FormatError::Syntax { line, column, .. }),
                Err(FormatError::Syntax {
                    line: l2,
                    column: c2,
                    ..
                }),
            ) if (line, column) == (l2, c2) => None,
            (Err(e), r) => Some(format!("{e:?}, while the whole document gives {r:?}")),
        };
    }
    let mut problems = Vec::new();
    let mut default_out = String::new();
    for (name, o) in [
        ("default", opts(false, false)),
        ("sort", opts(true, false)),
        ("canonicalize", opts(false, true)),
    ] {
        match check_formatting(&text, input.lang, &o) {
            Ok(out) if name == "default" => default_out = out,
            Ok(_) => {}
            Err(e) => problems.push(format!("{name}: {e}")),
        }
    }
    if let Some(result) = &input.c14n
        && problems.is_empty()
    {
        let expected = std::fs::read_to_string(result).unwrap_or_default();
        if without_comments(&default_out) != expected {
            problems.push(format!(
                "not the canonical form\n--- got ---\n{default_out}--- expected ---\n{expected}"
            ));
        }
        if !text.contains('#') && default_out != expected {
            problems.push("comment-free, yet not the canonical form byte for byte".into());
        }
    }
    (!problems.is_empty()).then(|| problems.join("; "))
}

#[test]
fn line_formats_format_the_rdf_suites() {
    let Some(dir) = rdf::suite_dir() else {
        eprintln!("W3C RDF suite not found (set SPARKLES_RDF_TESTS_DIR): skipped");
        return;
    };
    let mut inputs = Vec::new();
    for lang in [Language::NTriples, Language::NQuads] {
        for c in rdf::cases(&dir, lang) {
            let c14n = (c.kind == RdfKind::C14n)
                .then(|| c.result.clone())
                .flatten();
            inputs.push(Input {
                path: c.path,
                rel: c.rel,
                lang,
                kind: c.kind,
                c14n,
            });
        }
    }
    // the expected results of the Turtle and TriG evaluation tests
    let suite = std::fs::canonicalize(&dir).unwrap_or_else(|_| dir.clone());
    for lang in [Language::Turtle, Language::TriG] {
        for RdfCase { result, kind, .. } in rdf::cases(&dir, lang) {
            let Some(result) = result.filter(|_| kind == RdfKind::Eval) else {
                continue;
            };
            let lang = match result.extension().and_then(|e| e.to_str()) {
                Some("nq") => Language::NQuads,
                _ => Language::NTriples,
            };
            let rel = result
                .strip_prefix(&suite)
                .unwrap_or(&result)
                .to_string_lossy()
                .replace('\\', "/");
            if inputs.iter().any(|i: &Input| i.path == result) {
                continue;
            }
            inputs.push(Input {
                path: result,
                rel,
                lang,
                kind: RdfKind::PositiveSyntax,
                c14n: None,
            });
        }
    }
    let count = |l: Language, k: fn(RdfKind) -> bool| {
        inputs.iter().filter(|i| i.lang == l && k(i.kind)).count()
    };
    for l in [Language::NTriples, Language::NQuads] {
        eprintln!(
            "{}: {} positive ({} canonical form), {} negative",
            l.name(),
            count(l, RdfKind::is_positive),
            inputs
                .iter()
                .filter(|i| i.lang == l && i.c14n.is_some())
                .count(),
            count(l, |k| !k.is_positive()),
        );
    }
    assert!(
        inputs.iter().any(|i| i.c14n.is_some()),
        "no canonical form tests"
    );
    let known = corpus::fmt_known_failures();
    let results = corpus::par_map(&inputs, check);
    let mut failures = Vec::new();
    let mut fixed = Vec::new();
    for (input, problem) in inputs.iter().zip(results) {
        match (problem, known.contains_key(&input.rel)) {
            (Some(p), false) => failures.push(format!("{}: {p}", input.rel)),
            (None, true) => fixed.push(input.rel.clone()),
            _ => {}
        }
    }
    if !fixed.is_empty() {
        eprintln!(
            "passing now (remove from fmt-known-failures.txt): {}",
            fixed.join(", ")
        );
    }
    eprintln!("{} inputs, {} failures", inputs.len(), failures.len());
    assert!(
        failures.is_empty(),
        "{} failures:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

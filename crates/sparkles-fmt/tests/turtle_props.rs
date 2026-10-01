//! Property tests of Turtle and TriG over their corpus (the golden inputs, and when the
//! suites are present every positive W3C Turtle and TriG test and every SHACL file that
//! formats under the defaults and is not a documented failure):
//!
//! - **comment injection:** comments inserted at random token boundaries, with or without
//!   blank lines around them, all survive, and the output is a fixpoint;
//! - **layout independence:** re-spacing the input does not change the output;
//! - **random options:** the checks and idempotence hold under any valid options;
//! - **convergence:** `directive-style` fully determines the directives and graph blocks,
//!   so formatting under one value and then the other equals formatting under the second
//!   directly.
//!
//! Turtle is not switched on in `format()` yet, so the documents go through the pipeline
//! directly (`check::run`). `PROPTEST_CASES` sets the number of cases (default 64).

mod corpus;

use proptest::prelude::*;
use proptest::sample::Index;
use sparkles_fmt::lex::{LexMode, TokenKind, lex};
use sparkles_fmt::turtle::Turtle;
use sparkles_fmt::{
    DirectiveStyle, FormatError, Options, QuoteStyle, TurtleLayout, check, options,
};
use std::path::Path;

/// A named document and whether it is TriG.
type Doc = (String, String, bool);

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

/// The documents the properties draw from: those that format under the defaults.
fn documents() -> &'static [Doc] {
    static DOCS: std::sync::OnceLock<Vec<Doc>> = std::sync::OnceLock::new();
    DOCS.get_or_init(|| {
        let known = corpus::fmt_known_failures();
        let mut v = golden_inputs();
        if let Some(dir) = corpus::rdf::suite_dir() {
            for lang in [sparkles_fmt::Language::Turtle, sparkles_fmt::Language::TriG] {
                for c in corpus::rdf::cases(&dir, lang) {
                    if !c.kind.is_positive() || known.contains_key(&c.rel) {
                        continue;
                    }
                    if let Ok(text) = std::fs::read_to_string(&c.path) {
                        v.push((c.rel, text, lang == sparkles_fmt::Language::TriG));
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
                    v.push((key, text, false));
                }
            }
        }
        let ok = corpus::par_map(&v, |(_, text, trig)| {
            check::run(&Turtle { trig: *trig }, text, &Options::default()).is_ok()
        });
        v.into_iter()
            .zip(ok)
            .filter(|(_, ok)| *ok)
            .map(|(d, _)| d)
            .collect()
    })
}

fn config() -> ProptestConfig {
    let cases = std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(64);
    ProptestConfig {
        cases,
        failure_persistence: None,
        ..ProptestConfig::default()
    }
}

fn pick(i: &Index) -> &'static Doc {
    let docs = documents();
    &docs[i.index(docs.len())]
}

fn format(text: &str, trig: bool, opts: &Options) -> Result<String, FormatError> {
    check::run(&Turtle { trig }, text, opts).map(|f| f.text)
}

/// Format and format again: the text, or why it failed.
fn fixpoint(text: &str, trig: bool, opts: &Options) -> Result<String, String> {
    let out = format(text, trig, opts).map_err(|e| e.to_string())?;
    let again = format(&out, trig, opts).map_err(|e| format!("again: {e}"))?;
    if again != out {
        return Err(format!(
            "not a fixpoint\n--- first ---\n{out}--- second ---\n{again}"
        ));
    }
    Ok(out)
}

/// The labels the text declares (`@prefix` or `PREFIX`), in order, each once.
fn declared_labels(text: &str) -> Vec<String> {
    let sig: Vec<_> = lex(text, LexMode::Turtle)
        .into_iter()
        .filter(|t| !t.kind.is_trivia())
        .collect();
    let mut labels: Vec<String> = Vec::new();
    for w in sig.windows(2) {
        let directive = w[0].text(text);
        if (directive == "@prefix" || directive.eq_ignore_ascii_case("prefix"))
            && w[1].kind == TokenKind::PnameNs
        {
            let l = w[1].text(text).trim_end_matches(':').to_string();
            if !labels.contains(&l) {
                labels.push(l);
            }
        }
    }
    labels
}

/// The seeds of a random [`Options`]: widths, the boolean and enumerated keys, and a
/// group (0..3, or 3 for none) for each declared label.
type OptionSeed = (u16, u8, [bool; 8], Vec<u8>);

fn option_seed() -> impl Strategy<Value = OptionSeed> {
    (
        40u16..=400,
        1u8..=8,
        any::<[bool; 8]>(),
        prop::collection::vec(0u8..4, 1..12),
    )
}

fn options_from(seed: &OptionSeed, text: &str) -> Options {
    let (line_width, indent_width, b, group_seeds) = seed;
    let mut groups: Vec<Vec<String>> = vec![Vec::new(); 3];
    for (i, l) in declared_labels(text).into_iter().enumerate() {
        let g = group_seeds[i % group_seeds.len()] as usize;
        if g < 3 {
            groups[g].push(l);
        }
    }
    groups.retain(|g| !g.is_empty());
    let o = Options {
        line_width: *line_width,
        indent_width: *indent_width,
        sort: b[0],
        prune_prefixes: b[1],
        directive_style: match b[2] {
            true => DirectiveStyle::Turtle,
            false => DirectiveStyle::Sparql,
        },
        type_shorthand: b[3],
        compact_iris: b[4],
        quote_style: match b[5] {
            true => QuoteStyle::Preserve,
            false => QuoteStyle::Double,
        },
        turtle_layout: match b[6] {
            true => TurtleLayout::Conventional,
            false => TurtleLayout::Diff,
        },
        align_values: b[7],
        prefix_groups: groups,
        ..Options::default()
    };
    options::validate(&o).expect("valid options");
    o
}

fn marker(n: usize) -> String {
    format!("# sparkles-prop-c{n}")
}

/// `text` with marker comments after the significant tokens `spots` picks (modulo their
/// number; 0 is the very start), each with a blank line before it (`1`), after it (`2`),
/// both (`3`) or neither (`0`) when it starts a line of its own (an odd spot), else on
/// the token's line.
fn inject(text: &str, spots: &[(usize, u8)]) -> (String, usize) {
    let mut ends: Vec<usize> = vec![0];
    ends.extend(
        lex(text, LexMode::Turtle)
            .into_iter()
            .filter(|t| !t.kind.is_trivia() && t.kind != TokenKind::Eof)
            .map(|t| t.end()),
    );
    let mut at: Vec<(usize, u8)> = spots
        .iter()
        .map(|&(s, blanks)| (ends[s % ends.len()], blanks))
        .collect();
    at.sort_unstable();
    at.dedup_by_key(|a| a.0);
    let mut out = String::with_capacity(text.len() + 32 * at.len());
    let mut prev = 0;
    for (n, &(pos, mode)) in at.iter().enumerate() {
        out.push_str(&text[prev..pos]);
        let own_line = pos == 0 || mode & 4 != 0;
        if pos > 0 {
            out.push(if own_line { '\n' } else { ' ' });
        }
        if own_line && mode & 1 != 0 {
            out.push('\n');
        }
        out.push_str(&marker(n));
        out.push('\n');
        if own_line && mode & 2 != 0 {
            out.push('\n');
        }
        prev = pos;
    }
    out.push_str(&text[prev..]);
    (out, at.len())
}

/// `text` re-spaced: whitespace within lines changes width, single line breaks become
/// spaces or line breaks with any indentation, blank lines stay blank lines. Whitespace
/// next to a comment or in the header is kept, since it decides what the comment belongs
/// to.
fn respace(text: &str, seeds: &[u8]) -> String {
    let toks = lex(text, LexMode::Turtle);
    let first_sig = toks
        .iter()
        .position(|t| !t.kind.is_trivia())
        .unwrap_or(toks.len());
    let mut out = String::with_capacity(text.len() + 16);
    let mut k = 0;
    let mut seed = || {
        k += 1;
        seeds[k % seeds.len()]
    };
    for (i, t) in toks.iter().enumerate() {
        let s = t.text(text);
        let near_comment = (i > 0 && toks[i - 1].kind == TokenKind::Comment)
            || toks
                .get(i + 1)
                .is_some_and(|n| n.kind == TokenKind::Comment);
        if t.kind != TokenKind::Whitespace || i < first_sig || near_comment {
            out.push_str(s);
            continue;
        }
        let indent = " ".repeat(seed() as usize % 5);
        match s.matches('\n').count() {
            0 => out.push_str([" ", "  ", "\t", "   "][seed() as usize % 4]),
            1 if seed() % 2 == 0 => out.push(' '),
            1 => {
                out.push('\n');
                out.push_str(&indent);
            }
            _ => {
                out.push_str("\n\n");
                out.push_str(&indent);
            }
        }
    }
    out
}

proptest! {
    #![proptest_config(config())]

    #[test]
    fn injected_comments_survive(
        i in any::<Index>(),
        spots in prop::collection::vec((any::<usize>(), 0u8..8), 1..6),
    ) {
        let (name, text, trig) = pick(&i);
        let (injected, n) = inject(text, &spots);
        let out = match fixpoint(&injected, *trig, &Options::default()) {
            Ok(out) => out,
            Err(e) => return Err(TestCaseError::fail(format!("{name}: {e}\n--- input ---\n{injected}"))),
        };
        for c in 0..n {
            prop_assert_eq!(out.matches(&marker(c)).count(), 1, "{}: {} lost\n{}", name, marker(c), out);
        }
    }

    #[test]
    fn random_options_pass_the_checks(i in any::<Index>(), seed in option_seed()) {
        let (name, text, trig) = pick(&i);
        let opts = options_from(&seed, text);
        if let Err(e) = fixpoint(text, *trig, &opts) {
            return Err(TestCaseError::fail(format!("{name} under {opts:?}: {e}")));
        }
    }

    #[test]
    fn directive_style_converges(i in any::<Index>(), seed in option_seed()) {
        let (name, text, trig) = pick(&i);
        let a = options_from(&seed, text);
        let mut b = a.clone();
        b.directive_style = match a.directive_style {
            DirectiveStyle::Sparql => DirectiveStyle::Turtle,
            DirectiveStyle::Turtle => DirectiveStyle::Sparql,
        };
        let via_a = format(text, *trig, &a).and_then(|f| format(&f, *trig, &b));
        let direct = format(text, *trig, &b);
        prop_assert_eq!(via_a, direct, "{} under {:?}", name, b);
    }

    #[test]
    fn layout_does_not_matter(i in any::<Index>(), seeds in prop::collection::vec(any::<u8>(), 1..32)) {
        let (name, text, trig) = pick(&i);
        // a node kept as written keeps its layout by design
        prop_assume!(!text.contains("sparkles-fmt: ignore"));
        let respaced = respace(text, &seeds);
        let a = format(text, *trig, &Options::default());
        let b = format(&respaced, *trig, &Options::default());
        prop_assert_eq!(a, b, "{}\n--- re-spaced input ---\n{}", name, respaced);
    }
}

#[test]
fn the_corpus_is_there() {
    let docs = documents();
    assert!(docs.len() >= 5, "{} documents", docs.len());
    eprintln!("Turtle and TriG property corpus: {} documents", docs.len());
}

#[test]
fn injection_and_respacing_keep_the_tokens() {
    let text = "@prefix ex: <http://e/> .\nex:s ex:p \"a b\" ; # c\n  ex:q (1 2) .\n";
    let (injected, n) = inject(text, &[(0, 0), (3, 0), (5, 7)]);
    assert_eq!(n, 3);
    assert!(
        injected.starts_with("# sparkles-prop-c0\n@prefix"),
        "{injected}"
    );
    assert!(
        injected.contains("<http://e/> # sparkles-prop-c1\n"),
        "{injected}"
    );
    assert!(
        injected.contains("ex:s\n\n# sparkles-prop-c2\n\n"),
        "{injected}"
    );
    let respaced = respace(text, &[1, 2, 3, 7]);
    let sig = |t: &str| -> Vec<String> {
        lex(t, LexMode::Turtle)
            .into_iter()
            .filter(|t| t.kind != TokenKind::Whitespace)
            .map(|tok| tok.text(t).to_string())
            .collect()
    };
    assert_eq!(sig(&respaced), sig(text));
}

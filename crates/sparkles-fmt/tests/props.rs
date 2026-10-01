//! Property tests over the corpus (the golden inputs, and every positive W3C file when the
//! suite is present):
//!
//! - **comment injection:** comments inserted at random token boundaries all survive;
//! - **random options:** the checks and idempotence hold under any valid options;
//! - **convergence:** for the keys that fully determine their construct
//!   (`operator-position`, `directive-style`, `align-values`), formatting under one value
//!   and then another equals formatting under the second directly;
//! - **layout independence:** re-spacing the input does not change the output.
//!
//! `PROPTEST_CASES` sets the number of cases (default 64).

mod corpus;

use proptest::prelude::*;
use proptest::sample::Index;
use sparkles_fmt::lex::{LexMode, TokenKind, lex};
use sparkles_fmt::{
    DirectiveStyle, FormatError, Language, OperatorPosition, Options, QuoteStyle, TurtleLayout,
    format, options,
};

fn config() -> ProptestConfig {
    let cases = std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(64);
    ProptestConfig {
        cases,
        // the failure message names the corpus file and the minimal input; no
        // regression files in the source tree
        failure_persistence: None,
        ..ProptestConfig::default()
    }
}

fn pick(i: &Index) -> &'static (String, String) {
    let texts = corpus::positive_texts();
    &texts[i.index(texts.len())]
}

/// The labels the text declares with `PREFIX`, in order, each once.
fn declared_labels(text: &str) -> Vec<String> {
    let sig: Vec<_> = lex(text, LexMode::Sparql)
        .into_iter()
        .filter(|t| !t.kind.is_trivia())
        .collect();
    let mut labels: Vec<String> = Vec::new();
    for w in sig.windows(2) {
        if w[0].kind == TokenKind::Word
            && w[0].text(text).eq_ignore_ascii_case("prefix")
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
type OptionSeed = (u16, u8, [bool; 9], Vec<u8>);

fn option_seed() -> impl Strategy<Value = OptionSeed> {
    (
        40u16..=400,
        1u8..=8,
        any::<[bool; 9]>(),
        prop::collection::vec(0u8..4, 1..12),
    )
}

fn options_from(seed: &OptionSeed, text: &str) -> Options {
    let (line_width, indent_width, b, group_seeds) = seed;
    let labels = declared_labels(text);
    let mut groups: Vec<Vec<String>> = vec![Vec::new(); 3];
    for (i, l) in labels.into_iter().enumerate() {
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
        directive_style: if b[2] {
            DirectiveStyle::Turtle
        } else {
            DirectiveStyle::Sparql
        },
        type_shorthand: b[3],
        compact_iris: b[4],
        quote_style: if b[5] {
            QuoteStyle::Preserve
        } else {
            QuoteStyle::Double
        },
        operator_position: if b[6] {
            OperatorPosition::Trailing
        } else {
            OperatorPosition::Leading
        },
        turtle_layout: if b[7] {
            TurtleLayout::Conventional
        } else {
            TurtleLayout::Diff
        },
        align_values: b[8],
        prefix_groups: groups,
        ..Options::default()
    };
    options::validate(&o).expect("valid options");
    o
}

/// Format and format again: the text, or why it failed.
fn fixpoint(text: &str, opts: &Options) -> Result<String, String> {
    let out = format(text, Language::Sparql, opts).map_err(|e| e.to_string())?;
    let again = format(&out.text, Language::Sparql, opts).map_err(|e| format!("again: {e}"))?;
    if again.text != out.text {
        return Err(format!(
            "not a fixpoint\n--- first ---\n{}--- second ---\n{}",
            out.text, again.text
        ));
    }
    Ok(out.text)
}

/// A marker comment no corpus file contains.
fn marker(n: usize) -> String {
    format!("# sparkles-prop-c{n}")
}

/// `text` with marker comments after the significant tokens `spots` picks (modulo their
/// number; 0 is the very start).
fn inject(text: &str, spots: &[usize]) -> (String, usize) {
    let mut ends: Vec<usize> = vec![0];
    ends.extend(
        lex(text, LexMode::Sparql)
            .into_iter()
            .filter(|t| !t.kind.is_trivia() && t.kind != TokenKind::Eof)
            .map(|t| t.end()),
    );
    let mut at: Vec<usize> = spots.iter().map(|s| ends[s % ends.len()]).collect();
    at.sort_unstable();
    at.dedup();
    let mut out = String::with_capacity(text.len() + 32 * at.len());
    let mut prev = 0;
    for (n, &pos) in at.iter().enumerate() {
        out.push_str(&text[prev..pos]);
        if pos > 0 {
            out.push(' ');
        }
        out.push_str(&marker(n));
        out.push('\n');
        prev = pos;
    }
    out.push_str(&text[prev..]);
    (out, at.len())
}

/// `text` re-spaced: whitespace within lines changes width, single line breaks become
/// spaces or line breaks with any indentation, blank lines stay blank lines. Whitespace
/// next to a comment or in the header is kept, since it decides what the comment
/// belongs to.
fn respace(text: &str, seeds: &[u8]) -> String {
    let toks = lex(text, LexMode::Sparql);
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
        spots in prop::collection::vec(any::<usize>(), 1..6),
    ) {
        let (name, text) = pick(&i);
        let (injected, n) = inject(text, &spots);
        let out = match format(&injected, Language::Sparql, &Options::default()) {
            Ok(out) => out.text,
            // a comment inside a token sequence the grammar keeps together
            Err(FormatError::Syntax { .. }) => return Ok(()),
            Err(e) => return Err(TestCaseError::fail(format!("{name}: {e}\n{injected}"))),
        };
        for c in 0..n {
            prop_assert_eq!(out.matches(&marker(c)).count(), 1, "{}: {} lost\n{}", name, marker(c), out);
        }
        let again = fixpoint(&out, &Options::default());
        prop_assert!(again.as_deref() == Ok(out.as_str()), "{}: {:?}", name, again);
    }

    #[test]
    fn random_options_pass_the_checks(i in any::<Index>(), seed in option_seed()) {
        let (name, text) = pick(&i);
        let opts = options_from(&seed, text);
        if let Err(e) = fixpoint(text, &opts) {
            return Err(TestCaseError::fail(format!("{name} under {opts:?}: {e}")));
        }
    }

    #[test]
    fn construct_keys_converge(
        i in any::<Index>(),
        seed in option_seed(),
        flip in any::<[bool; 3]>(),
    ) {
        let (name, text) = pick(&i);
        let a = options_from(&seed, text);
        let mut b = a.clone();
        if flip[0] {
            b.operator_position = match a.operator_position {
                OperatorPosition::Leading => OperatorPosition::Trailing,
                OperatorPosition::Trailing => OperatorPosition::Leading,
            };
        }
        if flip[1] {
            b.directive_style = match a.directive_style {
                DirectiveStyle::Sparql => DirectiveStyle::Turtle,
                DirectiveStyle::Turtle => DirectiveStyle::Sparql,
            };
        }
        if flip[2] {
            b.align_values = !a.align_values;
        }
        let via_a = format(text, Language::Sparql, &a)
            .and_then(|f| format(&f.text, Language::Sparql, &b))
            .map(|f| f.text);
        let direct = format(text, Language::Sparql, &b).map(|f| f.text);
        prop_assert_eq!(via_a, direct, "{}", name);
    }

    #[test]
    fn layout_does_not_matter(i in any::<Index>(), seeds in prop::collection::vec(any::<u8>(), 1..32)) {
        let (name, text) = pick(&i);
        // a node kept as written keeps its layout by design
        prop_assume!(!text.contains("sparkles-fmt: ignore"));
        let respaced = respace(text, &seeds);
        let a = format(text, Language::Sparql, &Options::default()).map(|f| f.text);
        let b = format(&respaced, Language::Sparql, &Options::default()).map(|f| f.text);
        prop_assert_eq!(a, b, "{}\n--- re-spaced input ---\n{}", name, respaced);
    }
}

#[test]
fn injection_and_respacing_keep_the_meaning() {
    // the helpers themselves: markers land between tokens, re-spacing keeps the tokens
    let text = "PREFIX ex: <http://e/>\nSELECT ?s # c\nWHERE {\n\n  ?s ex:p \"a b\" .\n}\n";
    let (injected, n) = inject(text, &[0, 3]);
    assert_eq!(n, 2);
    assert!(
        injected.starts_with("# sparkles-prop-c0\nPREFIX"),
        "{injected}"
    );
    assert!(
        injected.contains("<http://e/> # sparkles-prop-c1\n"),
        "{injected}"
    );
    let respaced = respace(text, &[1, 2, 3, 7]);
    let sig = |t: &str| -> Vec<String> {
        lex(t, LexMode::Sparql)
            .into_iter()
            .filter(|t| !matches!(t.kind, TokenKind::Whitespace))
            .map(|tok| tok.text(t).to_string())
            .collect()
    };
    assert_eq!(sig(text), sig(&respaced));
    assert!(respaced.contains("# c\n"), "{respaced}");
    assert!(respaced.contains("\n\n"), "{respaced}");
    assert_eq!(
        declared_labels("prefix : <x:> PREFIX ex: <y:> PREFIX ex: <z:>"),
        ["", "ex"]
    );
}

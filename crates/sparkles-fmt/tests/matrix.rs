//! The configuration matrix: the safety checks and idempotence hold under every option
//! combination, not only the defaults.
//!
//! - every golden input under the SPARQL matrix: `type-shorthand` × `compact-iris` ×
//!   `quote-style` × `operator-position` × `prune-prefixes` × `align-values` ×
//!   `prefix-groups` (none, or `[["rdf", "rdfs", "xsd", "owl"]]`) × line width (40,
//!   100), 256 combinations. Keys SPARQL ignores stay out;
//! - the W3C corpus with every key flipped from its default;
//! - with `SPARKLES_FMT_MATRIX=1`, the W3C corpus under the whole SPARQL matrix (slow).

mod corpus;

use sparkles_fmt::{
    DirectiveStyle, Language, OperatorPosition, Options, QuoteStyle, TurtleLayout, format, options,
};

/// The SPARQL matrix, each combination with a label for failure messages.
fn sparql_matrix() -> Vec<OptionSet> {
    let mut v = Vec::new();
    for bits in 0u32..256 {
        let bit = |n: u32| bits & (1 << n) != 0;
        let o = Options {
            type_shorthand: !bit(0),
            compact_iris: !bit(1),
            quote_style: if bit(2) {
                QuoteStyle::Preserve
            } else {
                QuoteStyle::Double
            },
            operator_position: if bit(3) {
                OperatorPosition::Trailing
            } else {
                OperatorPosition::Leading
            },
            prefix_groups: if bit(4) { w3c_groups() } else { Vec::new() },
            line_width: if bit(5) { 40 } else { 100 },
            prune_prefixes: bit(6),
            align_values: bit(7),
            ..Options::default()
        };
        options::validate(&o).expect("valid options");
        let label = format!(
            "type-shorthand={} compact-iris={} quote-style={:?} operator-position={:?} \
             prefix-groups={:?} line-width={} prune-prefixes={} align-values={}",
            o.type_shorthand,
            o.compact_iris,
            o.quote_style,
            o.operator_position,
            o.prefix_groups,
            o.line_width,
            o.prune_prefixes,
            o.align_values
        );
        v.push((label, o));
    }
    v
}

fn w3c_groups() -> Vec<Vec<String>> {
    vec![
        ["rdf", "rdfs", "xsd", "owl"]
            .iter()
            .map(|s| s.to_string())
            .collect(),
    ]
}

/// Every key away from its default.
fn every_key_flipped() -> Options {
    let o = Options {
        line_width: 40,
        indent_width: 4,
        sort: true,
        prune_prefixes: true,
        directive_style: DirectiveStyle::Turtle,
        prefix_groups: w3c_groups(),
        type_shorthand: false,
        compact_iris: false,
        quote_style: QuoteStyle::Preserve,
        operator_position: OperatorPosition::Trailing,
        turtle_layout: TurtleLayout::Conventional,
        align_values: true,
        ..Options::default()
    };
    options::validate(&o).expect("valid options");
    o
}

/// `None` when `text` formats under `opts` to a fixpoint; else why not.
fn check(text: &str, opts: &Options) -> Option<String> {
    let out = match format(text, Language::Sparql, opts) {
        Ok(out) => out.text,
        Err(e) => return Some(e.to_string()),
    };
    match format(&out, Language::Sparql, opts) {
        Ok(again) if again.text == out => None,
        Ok(again) => Some(format!(
            "not a fixpoint\n--- first ---\n{out}--- second ---\n{}",
            again.text
        )),
        Err(e) => Some(format!("formatting the output: {e}")),
    }
}

/// A named document, and a labelled option set.
type Doc = (String, String);
type OptionSet = (String, Options);

/// Run every document under every option set; fail with the first few failures.
fn run(what: &str, docs: &[Doc], matrix: &[OptionSet]) {
    let jobs: Vec<(&Doc, &OptionSet)> = docs
        .iter()
        .flat_map(|d| matrix.iter().map(move |m| (d, m)))
        .collect();
    let failures: Vec<String> = corpus::par_map(&jobs, |((name, text), (label, opts))| {
        check(text, opts).map(|e| format!("{name} [{label}]: {e}"))
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
    let docs = corpus::golden_inputs();
    assert!(!docs.is_empty(), "no golden inputs");
    let matrix = sparql_matrix();
    assert_eq!(matrix.len(), 256);
    run("golden inputs under the SPARQL matrix", &docs, &matrix);
}

#[test]
fn corpus_with_every_key_flipped() {
    if corpus::suite_dir().is_none() {
        eprintln!("W3C suite not found (set SPARKLES_W3C_DIR): skipped");
        return;
    }
    run(
        "W3C corpus with every key flipped",
        corpus::positive_texts(),
        &[("every key flipped".to_string(), every_key_flipped())],
    );
}

#[test]
fn corpus_under_the_full_matrix() {
    if std::env::var("SPARKLES_FMT_MATRIX").as_deref() != Ok("1") {
        eprintln!("set SPARKLES_FMT_MATRIX=1 to run the W3C corpus under the whole matrix");
        return;
    }
    if corpus::suite_dir().is_none() {
        eprintln!("W3C suite not found (set SPARKLES_W3C_DIR): skipped");
        return;
    }
    run(
        "W3C corpus under the SPARQL matrix",
        corpus::positive_texts(),
        &sparql_matrix(),
    );
}

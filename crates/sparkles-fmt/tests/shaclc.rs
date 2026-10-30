//! The SHACLC formatter against its corpus: G03's test files (Jena's `shaclc-valid` pairs
//! and syntax tests, from `SPARKLES_SHACL_TESTS` or the sibling Jena checkout, skipped when
//! absent), the documents of the G03 spec and of the SHACL 1.2 compact syntax draft's
//! examples, and the shapes graphs of the SHACL test suite written by the reader's own
//! writer.
//!
//! Each document formats under every option set of a matrix, and the output
//!
//! - parses with the `sparkles-shaclc` reader to a shapes graph isomorphic to the
//!   input's,
//! - keeps every comment, and
//! - formats to itself.
//!
//! A comment after any token and on a line of its own before any token keeps formatting
//! safe too (the sweeps).

use oxrdf::dataset::CanonicalizationAlgorithm;
use oxrdf::{Dataset, GraphName, Quad};
use sparkles_fmt::lex::{LexMode, TokenKind, lex};
use sparkles_fmt::{FormatError, Language, Options, QuoteStyle, format};
use sparkles_shaclc::lex::{LexKind, lex_lossless};
use std::path::{Path, PathBuf};

const BASE: &str = "http://tests.example/base/";

/// Documents of the G03 spec and the examples of the SHACL 1.2 compact syntax draft,
/// each with the constructs of the grammar: directives, shapes and shape classes, every
/// kind of constraint, paths, counts, arrays, `|` chains, nested shapes and comments.
pub const DOCS: &[(&str, &str)] = &[
    (
        "note-example",
        r#"BASE <http://example.com/ns>
IMPORTS <http://example.com/person-ontology>
PREFIX ex: <http://example.com/ns#>

shape ex:PersonShape -> ex:Person {
    closed=true ignoredProperties=[rdf:type] .
    ex:ssn xsd:string [0..1] pattern="^\\d{3}-\\d{2}-\\d{4}$" .
    ex:worksFor IRI [0..*] @ex:CompanyShape .
    ex:address { ex:city xsd:string [1..1] . ex:zip xsd:string . } [0..1] .
}

shapeClass ex:CompanyShape {
    ex:name xsd:string [1..1] .
}
"#,
    ),
    (
        "paths-and-chains",
        r#"PREFIX ex: <http://example.com/ns#>
shape ex:S {
    ^ex:parent/ex:child* [0..*] .
    (ex:a|ex:b)+/ex:c? IRI|Literal .
    ex:p xsd:string|xsd:integer|ex:K [1..1] maxLength=5|!minLength=2 .
    ex:q !class=ex:Bad @ex:Good|{ ex:r IRI . } .
    !nodeKind=sh:Literal|hasValue=ex:x .
}
"#,
    ),
    (
        "lists",
        r#"PREFIX ex: <http://ex.org/>
shape ex:S {
    memberShape=ex:M .
    ex:speakers IRI [1..1] memberShape=ex:Speaker maxListLength=10 minListLength=1 uniqueMembers=true .
    ex:d in=[1 2.5 "t"@en 'q' -3 1e3 true FALSE ex:i <http://ex.org/j>] .
}
"#,
    ),
    (
        "comments",
        r#"# header one
# header two

# detached block

# leading block
BASE <http://ex.org/> # after base
PREFIX ex: <http://ex.org/> # after prefix
# before shape
shape ex:S -> ex:C { # after the brace
    # before p
    ex:p xsd:string # after type
        [1..1] # after count
        . # after dot

    # detached in body

    # leading of q
    ex:q IRI . # q trailing
    closed=true # after closed
        ignoredProperties=[ex:a # after a
            ex:b # after b
        ] .
    # dangling in body
}
# dangling at the end
"#,
    ),
    (
        "empty-and-minimal",
        "shape <http://ex.org/s> {}\nshapeClass <http://ex.org/c> { }\nshape <http://ex.org/e> -> <http://ex.org/f> {\n}\n",
    ),
    (
        "relative-iris-and-escapes",
        "PREFIX ex: <http://x/>\nshape <s> -> <c> { <p>/<q> <t> [1..1] pattern='a\\'b' message=\"\"\"long\nstring\"\"\" . ex:no\\.pe xsd:string . }\nPREFIX ex: <http://y/>\nshape ex:t {}\n",
    ),
    (
        "keywords-in-any-case",
        "base <http://ex.org/>\nprefix ex: <http://ex.org/>\nimports <http://ex.org/o>\nSHAPE ex:S { closed=TRUE . }\nSHAPECLASS ex:C { ex:p IRI . }\n",
    ),
];

/// The SHACL test suite's `std` directory, if present.
fn std_dir() -> Option<PathBuf> {
    let p = std::env::var("SPARKLES_SHACL_TESTS")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../../../apache/jena/jena-shacl/src/test/files/std")
        });
    p.exists().then_some(p)
}

/// Jena's SHACLC test files that are valid SHACLC.
fn jena_corpus() -> Vec<(String, String)> {
    let Some(std) = std_dir() else {
        eprintln!("the SHACL test suite is absent: only the in-tree documents run");
        return Vec::new();
    };
    let mut out = Vec::new();
    let valid = std.join("../shaclc-valid");
    let mut names: Vec<_> = std::fs::read_dir(&valid)
        .map(|d| d.flatten().map(|e| e.path()).collect())
        .unwrap_or_default();
    names.sort();
    for p in names {
        if p.extension().and_then(|e| e.to_str()) == Some("shaclc") {
            let name = format!("jena/{}", p.file_name().unwrap().to_string_lossy());
            out.push((name, std::fs::read_to_string(&p).unwrap()));
        }
    }
    for name in ["nodeParams.shc", "propertyParams.shc"] {
        let p = std.join("../local/shaclc-syntax").join(name);
        if let Ok(text) = std::fs::read_to_string(&p) {
            out.push((format!("jena/{name}"), text));
        }
    }
    out
}

fn corpus() -> Vec<(String, String)> {
    let mut v: Vec<(String, String)> = DOCS
        .iter()
        .map(|(n, t)| (format!("doc/{n}"), t.to_string()))
        .collect();
    v.extend(jena_corpus());
    v
}

/// The option sets every document formats under, with their names.
fn option_sets() -> Vec<(String, Options)> {
    let mut v = Vec::new();
    for bits in 0u32..32 {
        let bit = |n: u32| bits & (1 << n) != 0;
        let o = Options {
            line_width: if bit(0) { 40 } else { 100 },
            prune_prefixes: bit(1),
            align_values: bit(2),
            compact_iris: !bit(3),
            quote_style: if bit(4) {
                QuoteStyle::Preserve
            } else {
                QuoteStyle::Double
            },
            ..Options::default()
        };
        v.push((
            format!(
                "width={} prune={} align={} compact-iris={} quote={:?}",
                o.line_width, o.prune_prefixes, o.align_values, o.compact_iris, o.quote_style
            ),
            o,
        ));
    }
    v
}

fn quads(text: &str) -> Vec<Quad> {
    sparkles_shaclc::parse(text, Some(BASE))
        .unwrap_or_else(|e| panic!("{e}\n{text}"))
        .graph
        .iter()
        .map(|t| t.into_owned().in_graph(GraphName::DefaultGraph))
        .collect()
}

fn canonical(text: &str) -> Dataset {
    let mut d: Dataset = quads(text).into_iter().collect();
    d.canonicalize(CanonicalizationAlgorithm::Unstable);
    d
}

fn comments(text: &str) -> Vec<String> {
    let mut v: Vec<String> = lex_lossless(text)
        .unwrap()
        .into_iter()
        .filter(|l| l.kind == LexKind::Comment)
        .map(|l| text[l.start..l.end].trim_end().to_string())
        .collect();
    v.sort();
    v
}

/// Format `text`, then check the output against the input.
fn check(name: &str, text: &str, opts: &Options) -> Result<String, String> {
    let out = format(text, Language::Shaclc, opts)
        .map_err(|e| format!("{name}: {e}"))?
        .text;
    if canonical(text) != canonical(&out) {
        return Err(format!("{name}: the shapes graph differs\n{out}"));
    }
    if comments(text) != comments(&out) {
        return Err(format!("{name}: a comment was lost\n{out}"));
    }
    let again = format(&out, Language::Shaclc, opts).map_err(|e| format!("{name}: {e}"))?;
    if again.text != out {
        return Err(format!(
            "{name}: not a fixpoint\n--- first\n{out}--- second\n{}",
            again.text
        ));
    }
    Ok(out)
}

fn report(failures: Vec<String>, checked: usize) {
    assert!(
        failures.is_empty(),
        "{} of {checked} checks failed:\n\n{}",
        failures.len(),
        failures.join("\n\n")
    );
}

#[test]
fn the_corpus_formats_under_every_option_set() {
    let mut failures = Vec::new();
    let mut checked = 0;
    for (name, text) in corpus() {
        for (label, opts) in option_sets() {
            checked += 1;
            if let Err(e) = check(&format!("{name} [{label}]"), &text, &opts) {
                failures.push(e);
            }
        }
    }
    report(failures, checked);
}

/// Significant token ends of `text`.
fn token_ends(text: &str) -> Vec<usize> {
    // a comment cannot come between `@` and the IRI reference it takes
    lex(text, LexMode::Shaclc)
        .into_iter()
        .filter(|t| !t.kind.is_trivia() && !matches!(t.kind, TokenKind::Eof | TokenKind::At))
        .map(|t| t.end())
        .collect()
}

fn sweep(inject: impl Fn(&str, usize) -> String, opts: &Options) {
    let mut failures = Vec::new();
    let mut checked = 0;
    for (name, text) in corpus() {
        for end in token_ends(&text) {
            checked += 1;
            let injected = inject(&text, end);
            if let Err(e) = check(&format!("{name} + comment at {end}"), &injected, opts) {
                failures.push(e);
            }
        }
    }
    report(failures, checked);
}

fn sweep_options() -> Vec<Options> {
    vec![
        Options::default(),
        Options {
            line_width: 40,
            prune_prefixes: true,
            align_values: true,
            ..Options::default()
        },
    ]
}

#[test]
fn a_comment_after_any_token_is_kept() {
    for opts in sweep_options() {
        sweep(
            |t, end| format!("{} # swept\n{}", &t[..end], &t[end..]),
            &opts,
        );
    }
}

#[test]
fn a_comment_on_its_own_line_after_any_token_is_kept() {
    for opts in sweep_options() {
        sweep(
            |t, end| format!("{}\n# swept\n{}", &t[..end], &t[end..]),
            &opts,
        );
    }
}

#[test]
fn a_comment_block_with_blank_lines_after_any_token_is_kept() {
    for opts in sweep_options() {
        sweep(
            |t, end| format!("{}\n\n# swept\n\n{}", &t[..end], &t[end..]),
            &opts,
        );
    }
}

#[test]
fn the_reader_writes_shapes_the_formatter_reformats() {
    // the shapes graphs the SHACLC test pairs expect (Turtle), which SHACLC can express
    let Some(std) = std_dir() else { return };
    let mut files: Vec<PathBuf> = std::fs::read_dir(std.join("../shaclc-valid"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("ttl"))
        .collect();
    files.sort();
    let mut written = 0;
    let mut failures = Vec::new();
    for p in files {
        let Ok(text) = std::fs::read_to_string(&p) else {
            continue;
        };
        let mut parser = oxttl::TurtleParser::new()
            .with_base_iri(BASE)
            .unwrap()
            .for_slice(text.as_bytes());
        let mut graph = oxrdf::Graph::new();
        for t in parser.by_ref() {
            match t {
                Ok(t) => {
                    graph.insert(&t);
                }
                Err(_) => break,
            }
        }
        let prefixes: Vec<(String, String)> = parser
            .prefixes()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect();
        let Ok(shaclc) = sparkles_shaclc::write(&graph, &prefixes) else {
            continue;
        };
        written += 1;
        let name = p.display().to_string();
        for opts in sweep_options() {
            if let Err(e) = check(&name, &shaclc, &opts) {
                failures.push(e);
            }
        }
    }
    eprintln!("{written} shapes graphs written as SHACLC and reformatted");
    report(failures, written);
    assert!(
        written >= 20,
        "only {written} shapes graphs could be written"
    );
}

#[test]
fn a_syntax_error_is_the_readers_with_its_position() {
    let e = format(
        "shape <s> {\n  <p> <t>\n}\n",
        Language::Shaclc,
        &Options::default(),
    )
    .unwrap_err();
    match e {
        FormatError::Syntax {
            line,
            column,
            offset,
            ..
        } => {
            assert_eq!((line, column), (3, 1));
            assert_eq!(offset, "shape <s> {\n  <p> <t>\n".len());
        }
        other => panic!("{other:?}"),
    }
    // a lexical error is a positioned syntax error too
    let e = format(
        "shape <s> { <p> \"open",
        Language::Shaclc,
        &Options::default(),
    )
    .unwrap_err();
    assert!(matches!(e, FormatError::Syntax { line: 1, .. }), "{e:?}");
}

// ------------------------------------------------------------ generated documents ----

mod generated {
    use super::*;
    use proptest::prelude::*;

    fn iri() -> impl Strategy<Value = String> {
        prop::sample::select(vec![
            "ex:a",
            "ex:b-c",
            ":d",
            "<http://example.com/ns#full>",
            "<rel>",
            "xsd:string",
            "ex:",
        ])
        .prop_map(String::from)
    }

    fn path() -> impl Strategy<Value = String> {
        let leaf = prop_oneof![
            iri(),
            iri().prop_map(|i| format!("^{i}")),
            iri().prop_map(|i| format!("{i}*")),
        ];
        leaf.prop_recursive(2, 6, 2, |inner| {
            prop_oneof![
                (inner.clone(), inner.clone()).prop_map(|(a, b)| format!("{a}/{b}")),
                (inner.clone(), inner.clone()).prop_map(|(a, b)| format!("({a}|{b})+")),
                inner.prop_map(|a| format!("({a})?")),
            ]
        })
    }

    fn value() -> impl Strategy<Value = String> {
        prop_oneof![
            iri(),
            Just("\"text\"".to_string()),
            Just("'it\\'s'@en".to_string()),
            Just("5".to_string()),
            Just("-1.5e3".to_string()),
            Just("TRUE".to_string()),
            prop::collection::vec(iri(), 0..4).prop_map(|v| format!("[{}]", v.join(" "))),
        ]
    }

    fn node_atom() -> impl Strategy<Value = String> {
        (
            prop::bool::ANY,
            prop::sample::select(vec!["hasValue", "in", "class", "message", "pattern"]),
            value(),
        )
            .prop_map(|(not, name, v)| {
                let v = if name == "in" && !v.starts_with('[') {
                    format!("[{v}]")
                } else {
                    v
                };
                format!("{}{name}={v}", if not { "!" } else { "" })
            })
    }

    fn property_atom(depth: u32) -> BoxedStrategy<String> {
        let simple = prop_oneof![
            iri(),
            Just("IRI".to_string()),
            iri().prop_map(|i| format!("@{i}")),
            Just("@<http://example.com/ns#S>".to_string()),
            node_atom().prop_filter("a node parameter", |a| !a.contains("hasValue=[")),
            Just("minLength=2".to_string()),
        ];
        if depth == 0 {
            return simple.boxed();
        }
        prop_oneof![
            4 => simple,
            1 => body(depth - 1).prop_map(|b| b),
        ]
        .boxed()
    }

    fn property(depth: u32) -> BoxedStrategy<String> {
        (
            path(),
            prop::collection::vec(
                prop_oneof![
                    property_atom(depth),
                    (property_atom(depth), property_atom(depth))
                        .prop_map(|(a, b)| format!("{a}|{b}")),
                    Just("[1..*]".to_string()),
                    Just("[0..1]".to_string()),
                ],
                0..5,
            ),
        )
            .prop_map(|(p, atoms)| format!("{p} {} .", atoms.join(" ")))
            .boxed()
    }

    fn body(depth: u32) -> BoxedStrategy<String> {
        let constraint = prop_oneof![
            3 => property(depth),
            1 => prop::collection::vec(node_atom(), 1..4).prop_map(|a| format!("{} .", a.join(" "))),
            1 => iri().prop_map(|i| format!("@{i} .")),
        ];
        prop::collection::vec(constraint, 0..4)
            .prop_map(|cs| format!("{{ {} }}", cs.join("\n")))
            .boxed()
    }

    pub fn document() -> impl Strategy<Value = String> {
        let shape = (
            prop::bool::ANY,
            iri(),
            prop::collection::vec(iri(), 0..3),
            body(2),
        )
            .prop_map(|(class, name, targets, body)| {
                if class {
                    format!("shapeClass {name} {body}")
                } else if targets.is_empty() {
                    format!("shape {name} {body}")
                } else {
                    format!("shape {name} -> {} {body}", targets.join(" "))
                }
            });
        prop::collection::vec(shape, 1..4).prop_map(|shapes| {
            format!(
                "PREFIX ex: <http://example.com/ns#>\nPREFIX : <http://example.com/colon#>\nPREFIX unused: <http://example.com/unused#>\nBASE <http://example.com/base/>\n{}",
                shapes.join("\n")
            )
        })
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(
            std::env::var("PROPTEST_CASES").ok().and_then(|v| v.parse().ok()).unwrap_or(64)
        ))]

        #[test]
        fn generated_documents_format_safely(
            doc in document(),
            width in prop::sample::select(vec![30u16, 40, 100]),
            prune in prop::bool::ANY,
            align in prop::bool::ANY,
            at in any::<prop::sample::Index>(),
            own_line in prop::bool::ANY,
        ) {
            let opts = Options {
                line_width: width,
                prune_prefixes: prune,
                align_values: align,
                ..Options::default()
            };
            // some generated documents are not SHACLC (an undeclared prefix, say): the
            // reference parser says so, and there is nothing to check
            if sparkles_shaclc::parse(&doc, Some(BASE)).is_err() {
                return Ok(());
            }
            check("generated", &doc, &opts).map_err(TestCaseError::fail)?;
            let ends = token_ends(&doc);
            let end = ends[at.index(ends.len())];
            let injected = match own_line {
                true => format!("{}\n# swept\n{}", &doc[..end], &doc[end..]),
                false => format!("{} # swept\n{}", &doc[..end], &doc[end..]),
            };
            check("generated + comment", &injected, &opts).map_err(TestCaseError::fail)?;
        }
    }
}

// ------------------------------------------------------------------- behaviors ----

fn fmt(text: &str) -> String {
    format(text, Language::Shaclc, &Options::default())
        .unwrap_or_else(|e| panic!("{e}\n{text}"))
        .text
}

#[test]
fn documents_without_shapes_format_to_themselves() {
    for text in [
        "",
        "\n\n",
        "# only a comment\n",
        "# a\n\n# b\n",
        "BASE <http://e/>\n",
    ] {
        let out = fmt(text);
        assert_eq!(fmt(&out), out, "{text:?}");
        assert_eq!(comments(text), comments(&out), "{text:?}");
    }
    assert_eq!(fmt("BASE   <http://e/>"), "BASE <http://e/>\n");
}

#[test]
fn a_byte_order_mark_is_dropped() {
    assert_eq!(fmt("\u{feff}shape <s> {}"), "shape <s> {}\n");
}

#[test]
fn ignore_file_keeps_the_text_even_when_it_is_not_valid_shaclc() {
    let text = "# sparkles-fmt: ignore-file\nshape   <s>   {";
    let f = format(text, Language::Shaclc, &Options::default()).unwrap();
    assert_eq!(f.text, text);
    assert!(!f.changed);
}

#[test]
fn the_keys_that_do_not_apply_change_nothing() {
    use sparkles_fmt::{DirectiveStyle, TurtleLayout};
    let text = DOCS[0].1;
    let base = fmt(text);
    let opts = Options {
        sort: true,
        canonicalize: true,
        directive_style: DirectiveStyle::Turtle,
        prefix_groups: vec![vec!["ex".into()]],
        type_shorthand: false,
        turtle_layout: TurtleLayout::Conventional,
        ..Options::default()
    };
    let f = format(text, Language::Shaclc, &opts).unwrap();
    assert_eq!(f.text, base);
    assert!(f.warnings.is_empty(), "{:?}", f.warnings);
}

#[test]
fn a_comment_where_nothing_starts_moves_with_a_warning() {
    let text = "shape <s> {\n  <p> # between\n  <t> .\n}\n";
    let f = format(text, Language::Shaclc, &Options::default()).unwrap();
    // the comment follows `<p>`, which no node of its own ends: it leads the next part
    assert_eq!(comments(&f.text), ["# between"]);
    let text = "shape <s> { <p> <t> [ # inside the count\n 1..1] . }\n";
    let f = format(text, Language::Shaclc, &Options::default()).unwrap();
    assert!(
        f.warnings.iter().any(|w| w.code == "comment-moved"),
        "{f:?}"
    );
}

#[test]
fn the_cursor_follows_its_token() {
    let text = "shape   <s>   { <p>   IRI . }";
    let at = text.find("IRI").unwrap();
    let opts = Options {
        cursor: Some(at + 1),
        ..Options::default()
    };
    let f = format(text, Language::Shaclc, &opts).unwrap();
    assert_eq!(f.cursor, Some(f.text.find("IRI").unwrap() + 1));
}

#[test]
fn nesting_deeper_than_the_limit_is_refused() {
    let deep = format!(
        "shape <s> {{ {} <p> IRI . {} }}",
        "<p> { ".repeat(300),
        "} . ".repeat(300)
    );
    let e = format(&deep, Language::Shaclc, &Options::default()).unwrap_err();
    assert!(matches!(e, FormatError::Unsupported { .. }), "{e:?}");
    let ok = format!(
        "shape <s> {{ {} <p> IRI . {} }}",
        "<p> { ".repeat(40),
        "} . ".repeat(40)
    );
    assert!(check("deep", &ok, &Options::default()).is_ok());
}

#[test]
fn the_output_layout_is_as_documented() {
    let out = fmt(
        "PREFIX ex:<http://e/> shape ex:S ->ex:C ex:D{ex:p IRI[1..*]pattern=\"x\".closed=true.}shapeClass ex:T{}",
    );
    assert_eq!(
        out,
        "PREFIX ex: <http://e/>\n\nshape ex:S -> ex:C ex:D {\n  ex:p IRI [1..*] pattern=\"x\" .\n  closed=true .\n}\n\nshapeClass ex:T {}\n"
    );
    // an indent width of four
    let opts = Options {
        indent_width: 4,
        ..Options::default()
    };
    let out = format("shape <s> { <p> <t> . }", Language::Shaclc, &opts)
        .unwrap()
        .text;
    assert_eq!(out, "shape <s> {\n    <p> <t> .\n}\n");
}

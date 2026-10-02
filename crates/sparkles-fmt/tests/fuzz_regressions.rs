//! What the fuzz targets (`crates/sparkles-fmt/fuzz`) found, minimized, under the checks
//! the targets run (this file includes theirs): no panic; a refusal is a positioned
//! syntax error; output means what the input means, keeps its comments and formats to
//! itself; the line formats stream what they print in memory.

#[path = "../fuzz/src/invariants.rs"]
mod invariants;

use invariants::{Extra, HEADER, check, check_stream, decode, encode};
use sparkles_fmt::{Language, Options};

fn holds(lang: Language, opts: &Options, text: &str) {
    if let Err(e) = check(text, lang, opts) {
        panic!("{lang:?} {text:?}: {e}");
    }
}

fn streams(lang: Language, opts: &Options, extra: Extra, bytes: &[u8]) {
    if let Err(e) = check_stream(bytes, lang, opts, extra) {
        panic!("{lang:?} {:?}: {e}", String::from_utf8_lossy(bytes));
    }
}

#[test]
fn the_header_round_trips() {
    let (opts, extra, text) = decode(&[0; HEADER]);
    assert_eq!(
        (opts, extra, text),
        (Options::default(), Extra::default(), &[][..])
    );
    for h in [
        [0xff, 0xf7, 0xff, 8, 0],
        [0x55, 0x92, 1, 3, 0],
        [0xaa, 0x61, 128, 0, 0],
    ] {
        let (opts, extra, _) = decode(&h);
        let mut again = h;
        // the cursor bit and position are not encoded
        again[1] &= !0x08;
        again[4] = 0;
        assert_eq!(encode(&opts, extra), again, "{h:?}");
    }
    let short = decode(b"\x01");
    assert!(short.0.sort && short.2.is_empty());
}

#[test]
fn the_checks_pass_on_ordinary_inputs() {
    let opts = Options::default();
    holds(Language::Sparql, &opts, "select * { ?s ?p ?o } # end\n");
    holds(
        Language::Turtle,
        &opts,
        "@prefix ex: <http://e/> .\nex:a ex:b ex:c .\n",
    );
    holds(
        Language::TriG,
        &opts,
        "<http://g> { <http://a> <http://b> 1 }\n",
    );
    holds(
        Language::NQuads,
        &opts,
        "<http://a> <http://b> \"c\" <http://g> .\n",
    );
    holds(
        Language::JsonLd,
        &opts,
        r#"{"@id": "http://a", "b": [1, 2]}"#,
    );
    holds(Language::Turtle, &opts, "this is not turtle");
    let extra = Extra {
        spill: true,
        chunk_bytes: 1,
        ..Extra::default()
    };
    let sort = Options {
        sort: true,
        ..Options::default()
    };
    streams(
        Language::NTriples,
        &sort,
        extra,
        b"<http://b> <http://p> _:x .\n<http://a> <http://p> _:x .\n",
    );
    streams(
        Language::NTriples,
        &sort,
        extra,
        b"<http://a> <http://p> \"\xff\" .\n",
    );
}

#[test]
fn sparql_nested_thousands_deep_is_refused() {
    // overflowed the stack: the reference parser and the printer recurse
    let opts = Options::default();
    for n in [300, 2000] {
        let groups = format!("SELECT * {}{}", "{".repeat(n), "}".repeat(n));
        holds(Language::Sparql, &opts, &groups);
        let parens = format!("ASK {{ FILTER({}1{}) }}", "(".repeat(n), ")".repeat(n));
        holds(Language::Sparql, &opts, &parens);
    }
}

#[test]
fn sparql_tokens_only_spargebra_reads_as_two() {
    // by the grammar's longest match `prefixr:` is one prefixed name, `CONSTRUCTWHERE`
    // one word and `.2` one decimal; spargebra reads `PREFIX r:`, `CONSTRUCT WHERE` and
    // `. 2`, and the formatter's parser refuses what it accepted (unsupported-syntax)
    let opts = Options::default();
    for (text, spaced) in [
        (
            "prefixr: <http://e/>\nSELECT ?a WHERE { ?a r:p 1 }",
            "prefix r: <http://e/>\nSELECT ?a WHERE { ?a r:p 1 }",
        ),
        ("PREFIX:<>", "PREFIX :<>"),
        ("PREFIXin:<>", "PREFIX in:<>"),
        ("CONSTRUCTWHERE{}", "CONSTRUCT WHERE{}"),
        (
            "CONSTRUCT WHERE { <a:s> <a:p> ?o .2 ?s <a:p> }",
            "CONSTRUCT WHERE { <a:s> <a:p> ?o . 2 ?s <a:p> }",
        ),
    ] {
        let e = sparkles_fmt::format(text, Language::Sparql, &opts);
        assert!(
            matches!(e, Err(sparkles_fmt::FormatError::Unsupported { .. })),
            "{text}: {e:?}"
        );
        assert!(invariants::lenient_reading(text, &opts), "{text}");
        let at = text.len();
        assert_eq!(invariants::space_glued(text, at).as_deref(), Some(spaced));
        holds(Language::Sparql, &opts, text);
    }
    // a prefix label that starts with a keyword is spaced only where the parser stops
    let text = "PREFIXa: <http://e/>\nPREFIX inab: <http://e/>\nASK {}";
    assert!(invariants::lenient_reading(text, &opts));
    // `::q` is one prefixed name (a local name may start with `:`), `: :q` to spargebra
    let text = "PREFIX:<//>SELECT*{::q?o}";
    assert!(invariants::lenient_reading(text, &opts));
    holds(Language::Sparql, &opts, text);
}

#[test]
fn sparql_undeclared_prefix_outside_iris() {
    // U+FFFB may be in a prefix label but not in an IRI: the namespace the reference
    // parse gives an undeclared prefix panicked
    let opts = Options::default();
    holds(Language::Sparql, &opts, "\u{fffb}:");
    holds(
        Language::Sparql,
        &opts,
        "SELECT * { ?s \u{fffb}:p \u{fffb}é:o }",
    );
}

#[test]
fn sparql_duplicate_prefixes_under_comments_are_stable() {
    // the duplicate printed first had its comment read as the file header the second
    // time, and was dropped then as a plain duplicate
    for opts in [
        Options::default(),
        Options {
            sort: true,
            prune_prefixes: true,
            prefix_groups: vec![vec!["rdf".into()], vec!["".into(), "ex".into()]],
            ..Options::default()
        },
    ] {
        for text in [
            "PREFIX dc:<http://purl.org/dc/terms/>\n#\nPREFIX dc: <http://purl.org/dc/terms/>\n#\nPREFIX dc:<http://purl.org/dc/terms/>\n",
            "PREFIX s: <ht>\n#\nPREFIX foaf: <http://xmlns.com/foaf/0.1/>\n# \nPREFIX foaf: <http://xmlns.com/foaf/0.1/>",
            "# header\nPREFIX a: <http://e/>\n# c\nPREFIX a: <http://e/>\nASK {}",
            // the header stays above the declaration sorted first, not the one written
            // first
            "#a\nPREFIX e:<http://example.org/>\nPREFIX :<>PREFIX e:<http://example.org/>#\n",
            "# >\nPREFIX ab: <http://e/ab#>PREFIX ex:</>\n#/>\nPREFIX ab: <http://e/ab#>",
        ] {
            holds(Language::Sparql, &opts, text);
        }
    }
}

#[test]
fn sparql_a_semicolon_alone_keeps_its_comments_in_place() {
    // spargebra accepts a `;` alone; dropped, it left its comment at the start of the
    // output, which the second run read as the file header
    let opts = Options::default();
    for text in [";#", "; # c\n", ";\n\n# c\n", "# h\n;\n# c\n", ";"] {
        holds(Language::Sparql, &opts, text);
    }
    let f = sparkles_fmt::format("; # c\n", Language::Sparql, &opts).unwrap();
    assert_eq!(f.text, ";\n# c\n");
}

#[test]
fn unterminated_long_quotes_are_an_empty_string() {
    // `"""a"@en` without a closing `"""` is `""` and `"a"@en` by the longest match, as
    // the reference parsers read it; the lexer made it one unterminated token
    let opts = Options::default();
    holds(
        Language::Sparql,
        &opts,
        "SELECT * { VALUES ?v { 0 \"\"\"a\"@en :a } }",
    );
    holds(
        Language::Turtle,
        &opts,
        "<a:s> <a:p> \"\"\"\"a\"@en , '''' .",
    );
}

#[test]
fn line_format_version_directives() {
    // oxttl does not know `VERSION`, which the line formats check themselves: the
    // independent comparison leaves those lines out
    let canon = Options {
        canonicalize: true,
        sort: true,
        ..Options::default()
    };
    for opts in [Options::default(), canon] {
        let text = "VERSION'\u{1}:  \0'\n<a:s> <a:p> 1 .\n";
        holds(Language::NTriples, &opts, text);
        holds(Language::NQuads, &opts, &format!("\u{feff}{text}"));
    }
}

#[test]
fn json_ld_nested_deeper_than_the_limit_is_refused() {
    let deep = format!("{}{}", "[".repeat(257), "]".repeat(257));
    holds(Language::JsonLd, &Options::default(), &deep);
}

#[test]
fn ignore_file_keeps_bytes_that_are_not_utf8() {
    // the streamed line formats copy an ignored file byte for byte
    let bytes = b"# sparkles-fmt: ignore-file\n# \xff\n<http://a> <http://p> 1 .\n";
    streams(
        Language::NTriples,
        &Options::default(),
        Extra::default(),
        bytes,
    );
    let mut out = Vec::new();
    let stats = sparkles_fmt::format_lines(
        &bytes[..],
        &mut out,
        Language::NTriples,
        &Options::default(),
        &sparkles_fmt::LinesConfig::default(),
    )
    .unwrap();
    assert!(!stats.changed);
    assert_eq!(out, bytes);
}

#[test]
fn turtle_nested_thousands_deep_is_refused() {
    // overflowed the stack in the printer, which recurses into blank node property lists,
    // collections and triple terms
    let opts = Options::default();
    for n in [300, 2000] {
        let bnodes = format!(
            "<http://a> <http://b> {}{} .\n",
            "[ <http://p> ".repeat(n),
            "]".repeat(n)
        );
        holds(Language::Turtle, &opts, &bnodes);
        let lists = format!(
            "<http://a> <http://b> {}1{} .\n",
            "( ".repeat(n),
            " )".repeat(n)
        );
        holds(Language::Turtle, &opts, &lists);
    }
}

#[test]
fn turtle_identical_prefixes_with_comments_are_idempotent() {
    // as in SPARQL: the duplicate printed first had its comment read as the file header
    // the second time, and was dropped then as a plain duplicate
    for opts in [
        Options::default(),
        Options {
            prefix_groups: vec![vec!["rdf".into()], vec!["".into(), "ex".into()]],
            ..Options::default()
        },
    ] {
        holds(
            Language::TriG,
            &opts,
            "PREFIX : <http://e/>\n#ts\nPREFIX : <http://e/>\n#mtH#s\nPREFIX : <http://e/>\n",
        );
    }
}

#[test]
fn a_cursor_inside_a_character_maps_to_a_character_boundary() {
    // the front ends pass character boundaries, but a library caller may not
    let text = "\"\u{FEFF}\"";
    for c in 0..=text.len() {
        let opts = Options {
            cursor: Some(c),
            ..Options::default()
        };
        let out = sparkles_fmt::format(text, Language::JsonLd, &opts).unwrap();
        let at = out.cursor.unwrap();
        assert!(
            out.text.is_char_boundary(at),
            "cursor {c} → {at} in {:?}",
            out.text
        );
    }
}

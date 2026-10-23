//! `<` and its relatives over two stored simple literals compare their lexical forms by
//! code point. The base vocabulary sorts a string after its own extensions, since the
//! separator after a lexical form is byte 0xFF, so its id order must not decide them.
use oxrdfio::RdfFormat;
use sparkles_core::io::Source;
use sparkles_core::sparql::{QueryOptions, query};
use sparkles_core::store::{Store, StoreOptions};

const VALUES: [&str; 12] = [
    r#""""#,
    r#""a""#,
    r#""ab""#,
    r#""abc""#,
    r#""b""#,
    r#""B""#,
    r#""é""#,
    r#""z!""#,
    r#""z""#,
    r#""ab"@en"#,
    r#""ab"^^<http://www.w3.org/2001/XMLSchema#string>"#,
    r#""10"^^<http://www.w3.org/2001/XMLSchema#integer>"#,
];

fn store() -> Store {
    let s = Store::in_memory(StoreOptions::default());
    let data: String = VALUES
        .iter()
        .enumerate()
        .map(|(i, v)| format!("<urn:s:{i}> <urn:p> {v} .\n"))
        .collect();
    s.load(&[Source::from_bytes(
        data.into_bytes(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
    s
}

/// The ordered pairs of stored values the filter keeps, as subject numbers.
fn pairs(s: &Store, op: &str, decoded: bool) -> Vec<(usize, usize)> {
    // CONCAT computes new strings, which the generic comparison of decoded values reads
    let filter = if decoded {
        format!("FILTER(CONCAT(?x, \"\") {op} CONCAT(?y, \"\"))")
    } else {
        format!("FILTER(?x {op} ?y)")
    };
    let q = format!(
        "SELECT ?a ?b WHERE {{ ?a <urn:p> ?x . ?b <urn:p> ?y \
         FILTER(datatype(?x) = <http://www.w3.org/2001/XMLSchema#string>) \
         FILTER(datatype(?y) = <http://www.w3.org/2001/XMLSchema#string>) {filter} }}"
    );
    let r = query(
        s.snapshot(),
        &q,
        &QueryOptions {
            no_cache: true,
            ..Default::default()
        },
    )
    .unwrap();
    let num = |t: &Option<oxrdf::Term>| match t {
        Some(oxrdf::Term::NamedNode(n)) => n.as_str()["urn:s:".len()..].parse().unwrap(),
        other => panic!("{other:?}"),
    };
    let mut out: Vec<(usize, usize)> = r.rows().iter().map(|r| (num(&r[0]), num(&r[1]))).collect();
    out.sort();
    out
}

#[test]
fn stored_strings_compare_by_code_point_including_prefixes() {
    let s = store();
    let strings: Vec<(usize, String)> = VALUES
        .iter()
        .enumerate()
        .filter(|(_, v)| v.ends_with('"') || v.ends_with("#string>"))
        .map(|(i, v)| {
            let lexical = v.trim_end_matches("^^<http://www.w3.org/2001/XMLSchema#string>");
            let lexical = &lexical[1..lexical.len() - 1];
            (i, lexical.to_string())
        })
        .collect();
    for (op, keep) in [
        ("<", (|a: &str, b: &str| a < b) as fn(&str, &str) -> bool),
        ("<=", |a, b| a <= b),
        (">", |a, b| a > b),
        (">=", |a, b| a >= b),
    ] {
        let mut expected = Vec::new();
        for (i, x) in &strings {
            for (j, y) in &strings {
                if keep(x, y) {
                    expected.push((*i, *j));
                }
            }
        }
        expected.sort();
        assert_eq!(pairs(&s, op, false), expected, "{op}");
        assert_eq!(pairs(&s, op, true), expected, "{op} decoded");
    }
    // "a" sorts before "ab" although its vocabulary key sorts after
    assert!(pairs(&s, "<", false).contains(&(1, 2)));
}

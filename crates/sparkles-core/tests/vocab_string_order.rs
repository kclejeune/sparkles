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

/// Words over a small alphabet, so that many are prefixes of others.
fn words() -> Vec<String> {
    let mut out = vec![String::new()];
    for len in 1..=4 {
        let mut next = Vec::new();
        for w in out.iter().filter(|w| w.chars().count() == len - 1) {
            for c in ['a', 'b', 'é'] {
                next.push(format!("{w}{c}"));
            }
        }
        out.extend(next);
    }
    out
}

/// A product of more than 4,096 rows takes the eager comparison through vocabulary
/// floors. A few numbers, a language string and an IRI compare as the generic filter
/// does.
#[test]
fn large_tables_order_stored_strings_through_their_vocabulary_floors() {
    let words = words();
    let extra = ["5", "7", r#""ab"@en"#, "<urn:iri>"];
    let total = words.len() + extra.len();
    assert!(total * total > 4096 * 3, "{total}");
    let mut data = String::new();
    for (i, w) in words.iter().enumerate() {
        data.push_str(&format!("<urn:s:{i}> <urn:p> \"{w}\" .\n"));
    }
    for (k, v) in extra.iter().enumerate() {
        // their subjects sort after the words' subjects, so the first rows hold strings
        data.push_str(&format!("<urn:z:{k}> <urn:p> {v} .\n"));
    }
    let s = Store::in_memory(StoreOptions::default());
    s.load(&[Source::from_bytes(
        data.into_bytes(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
    let number = |i: usize| match i.checked_sub(words.len()) {
        Some(0) => Some(5),
        Some(1) => Some(7),
        _ => None,
    };
    type Keep = fn(std::cmp::Ordering) -> bool;
    let ops: [(&str, Keep); 4] = [
        ("<", |o| o.is_lt()),
        ("<=", |o| o.is_le()),
        (">", |o| o.is_gt()),
        (">=", |o| o.is_ge()),
    ];
    for (op, keep) in ops {
        let q =
            format!("SELECT ?a ?b WHERE {{ ?a <urn:p> ?x . ?b <urn:p> ?y FILTER(?x {op} ?y) }}");
        let options = QueryOptions {
            no_cache: true,
            ..Default::default()
        };
        let r = query(s.snapshot(), &q, &options).unwrap();
        let num = |t: &Option<oxrdf::Term>| match t {
            Some(oxrdf::Term::NamedNode(n)) => match n.as_str().strip_prefix("urn:z:") {
                Some(k) => words.len() + k.parse::<usize>().unwrap(),
                None => n.as_str()["urn:s:".len()..].parse::<usize>().unwrap(),
            },
            other => panic!("{other:?}"),
        };
        let mut got: Vec<(usize, usize)> =
            r.rows().iter().map(|r| (num(&r[0]), num(&r[1]))).collect();
        got.sort();
        let mut expected = Vec::new();
        for i in 0..total {
            for j in 0..total {
                let order = match (words.get(i), words.get(j), number(i), number(j)) {
                    (Some(a), Some(b), _, _) => Some(a.cmp(b)),
                    (_, _, Some(a), Some(b)) => Some(a.cmp(&b)),
                    // a language string equals itself, and an IRI has no order
                    _ if i == j && i == words.len() + 2 => Some(std::cmp::Ordering::Equal),
                    _ => None,
                };
                if order.is_some_and(keep) {
                    expected.push((i, j));
                }
            }
        }
        assert_eq!(got, expected, "{op}");
    }
}

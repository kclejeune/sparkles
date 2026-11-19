//! The engine's encoders against sparesults, the byte oracle: fixed fixtures for the
//! byte-sensitive rules, a property test over generated solutions in both row views, and
//! the rows counted when the caller's writer fails partway.

use super::super::rowout::Explicit;
use super::super::source::RowView;
use super::super::{RowCount, io, write_buffered};
use super::Tsv;
use crate::error::Result;
use oxrdf::vocab::xsd;
use oxrdf::{
    BaseDirection, BlankNode, Literal, NamedNode, NamedOrBlankNode, Term, Triple, Variable,
};
use proptest::prelude::*;
use sparesults::{QueryResultsFormat, QueryResultsSerializer};
use std::borrow::Cow;
use std::cell::RefCell;
use std::io::Write;
use std::rc::Rc;

/// The formats the engine encodes so far.
#[derive(Clone, Copy, Debug)]
enum Enc {
    Tsv,
}

impl Enc {
    fn oracle(self) -> QueryResultsFormat {
        match self {
            Enc::Tsv => QueryResultsFormat::Tsv,
        }
    }

    fn header(self, out: &mut Explicit<'_, '_>, vars: &[String]) -> std::io::Result<()> {
        match self {
            Enc::Tsv => Tsv::header(out, vars),
        }
    }

    fn row(
        self,
        out: &mut Explicit<'_, '_>,
        row: RowView<'_>,
        width: usize,
    ) -> std::io::Result<()> {
        match self {
            Enc::Tsv => Tsv::row(out, row, width),
        }
    }
}

/// A writer that shares its bytes, so the oracle's row ends can be read while it writes.
#[derive(Clone, Default)]
struct Bytes(Rc<RefCell<Vec<u8>>>);

impl Write for Bytes {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.borrow_mut().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// sparesults' bytes and the offset at which each row ends.
fn oracle(enc: Enc, vars: &[String], rows: &[Vec<Option<Term>>]) -> (Vec<u8>, Vec<usize>) {
    let out = Bytes::default();
    let variables: Vec<Variable> = vars.iter().map(Variable::new_unchecked).collect();
    let mut s = QueryResultsSerializer::from_format(enc.oracle())
        .serialize_solutions_to_writer(out.clone(), variables.clone())
        .unwrap();
    let mut ends = Vec::new();
    for row in rows {
        s.serialize(
            row.iter()
                .enumerate()
                .filter_map(|(c, t)| t.as_ref().map(|t| (variables[c].as_ref(), t.as_ref()))),
        )
        .unwrap();
        ends.push(out.0.borrow().len());
    }
    let _ = s.finish();
    let bytes = out.0.borrow().clone();
    (bytes, ends)
}

/// The encoder's bytes into `w`, each row through the view `sparse` selects, with the rows
/// that reached `w` whole.
fn encode(
    enc: Enc,
    vars: &[String],
    rows: &[Vec<Option<Term>>],
    sparse: bool,
    w: &mut dyn Write,
) -> (Result<()>, u64) {
    let count = RowCount::new();
    let result = write_buffered(w, &count, None, &mut |out, _| {
        let mut out = Explicit::new(out);
        enc.header(&mut out, vars).map_err(io)?;
        for row in rows {
            out.begin_row()?;
            let written = if sparse {
                let cells: Vec<(usize, Cow<'_, Term>)> = row
                    .iter()
                    .enumerate()
                    .filter_map(|(c, t)| t.as_ref().map(|t| (c, Cow::Borrowed(t))))
                    .collect();
                enc.row(&mut out, RowView::Sparse(&cells), vars.len())
            } else {
                enc.row(&mut out, RowView::Dense(row), vars.len())
            };
            written.map_err(io)?;
            out.end_row(1);
        }
        Ok(())
    });
    (result, count.get())
}

/// Takes `limit` bytes and fails every write after them.
struct Failing {
    limit: usize,
    taken: usize,
}

impl Write for Failing {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        if self.taken >= self.limit {
            return Err(std::io::Error::other("the client went away"));
        }
        let n = b.len().min(self.limit - self.taken);
        self.taken += n;
        Ok(n)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn check(
    enc: Enc,
    vars: &[String],
    rows: &[Vec<Option<Term>>],
) -> std::result::Result<(), TestCaseError> {
    let (want, _) = oracle(enc, vars, rows);
    for sparse in [false, true] {
        let mut got = Vec::new();
        let (r, n) = encode(enc, vars, rows, sparse, &mut got);
        prop_assert!(r.is_ok(), "{enc:?} sparse {sparse}: {r:?}");
        prop_assert_eq!(n, rows.len() as u64);
        prop_assert!(
            got == want,
            "{:?} sparse {}:\n got {:?}\nwant {:?}",
            enc,
            sparse,
            String::from_utf8_lossy(&got),
            String::from_utf8_lossy(&want)
        );
    }
    Ok(())
}

/// The rows counted when the caller's writer fails after `limit` bytes are the rows
/// whose ends lie within those bytes.
fn check_failing(
    enc: Enc,
    vars: &[String],
    rows: &[Vec<Option<Term>>],
    limit: usize,
) -> std::result::Result<(), TestCaseError> {
    let (want, ends) = oracle(enc, vars, rows);
    let limit = limit.min(want.len());
    let complete = ends.iter().filter(|&&e| e <= limit).count() as u64;
    for sparse in [false, true] {
        let mut w = Failing { limit, taken: 0 };
        let (r, n) = encode(enc, vars, rows, sparse, &mut w);
        if limit < want.len() {
            prop_assert!(r.is_err(), "{enc:?}: a failed writer must fail the output");
        }
        prop_assert_eq!(n, complete, "{:?} sparse {} limit {}", enc, sparse, limit);
    }
    Ok(())
}

// ---- generators, biased toward the bytes the formats treat specially ----

/// Characters that some format escapes, quotes, trims or treats as a number.
const SPECIAL: &[char] = &[
    'a', 'Z', '0', '7', '.', '-', '+', 'e', 'E', ' ', '\t', '\n', '\r', '"', '\'', '\\', ',', '<',
    '>', '&', ';', '#', ':', '/', '?', '@', '^', '_', 'é', 'ß', '中', '𝄞', '\u{0}', '\u{1}',
    '\u{8}', '\u{b}', '\u{c}', '\u{1f}', '\u{7f}', '\u{85}', '\u{a0}', '\u{2028}', '\u{feff}',
];

fn text() -> impl Strategy<Value = String> {
    prop_oneof![
        4 => prop::collection::vec(prop::sample::select(SPECIAL), 0..10)
            .prop_map(|c| c.into_iter().collect()),
        1 => any::<String>(),
        1 => Just(String::new()),
        // only whitespace, and whitespace at both ends
        1 => prop::collection::vec(prop::sample::select(&[' ', '\t', '\n', '\r'][..]), 1..5)
            .prop_map(|c| c.into_iter().collect()),
        1 => ("[ \t\n\r]{0,3}", "[a-z<&\"]{0,4}", "[ \t\n\r]{0,3}")
            .prop_map(|(a, b, c)| format!("{a}{b}{c}")),
    ]
}

/// Lexical forms near the Turtle number and boolean rules, valid and not.
fn number() -> impl Strategy<Value = String> {
    prop_oneof![
        "[+-]?[0-9]{0,3}",
        "[+-]?[0-9]{0,3}\\.[0-9]{0,3}",
        "[+-]?[0-9]{0,3}(\\.[0-9]{0,3})?[eE][+-]?[0-9]{0,2}",
        "[+-]{0,2}[0-9.eE]{0,4}",
        prop::sample::select(
            &[
                "true", "false", "TRUE", "1", "0", "", ".", "e5", ".e5", "1.e5", "+", "-0"
            ][..]
        )
        .prop_map(str::to_string),
    ]
}

fn iri() -> impl Strategy<Value = NamedNode> {
    prop_oneof![
        3 => "[a-z]{1,3}".prop_map(|s| NamedNode::new_unchecked(format!("http://example.org/{s}"))),
        1 => text().prop_map(|s| NamedNode::new_unchecked(format!("urn:x:{s}"))),
        1 => prop::sample::select(&[xsd::INTEGER, xsd::DECIMAL, xsd::DOUBLE, xsd::BOOLEAN, xsd::STRING][..])
            .prop_map(|d| d.into_owned()),
    ]
}

fn bnode() -> impl Strategy<Value = BlankNode> {
    "[a-zA-Z0-9]{1,6}".prop_map(BlankNode::new_unchecked)
}

fn literal() -> impl Strategy<Value = Literal> {
    let datatype = prop_oneof![
        Just(xsd::INTEGER.into_owned()),
        Just(xsd::DECIMAL.into_owned()),
        Just(xsd::DOUBLE.into_owned()),
        Just(xsd::BOOLEAN.into_owned()),
        Just(xsd::STRING.into_owned()),
        Just(xsd::INT.into_owned()),
        Just(xsd::FLOAT.into_owned()),
        iri(),
    ];
    let lang = prop::sample::select(&["en", "en-US", "fr", "zh-Hant-TW", "x"][..]);
    let direction = prop::option::of(prop::sample::select(
        &[BaseDirection::Ltr, BaseDirection::Rtl][..],
    ));
    prop_oneof![
        2 => text().prop_map(Literal::new_simple_literal),
        3 => (prop_oneof![number(), text()], datatype)
            .prop_map(|(v, d)| Literal::new_typed_literal(v, d)),
        2 => (text(), lang, direction).prop_map(|(v, l, d)| match d {
            Some(d) => Literal::new_directional_language_tagged_literal_unchecked(v, l, d),
            None => Literal::new_language_tagged_literal_unchecked(v, l),
        }),
    ]
}

fn term() -> impl Strategy<Value = Term> {
    let leaf = prop_oneof![
        3 => iri().prop_map(Term::from),
        1 => bnode().prop_map(Term::from),
        4 => literal().prop_map(Term::from),
    ];
    leaf.prop_recursive(2, 6, 1, |inner| {
        (
            prop_oneof![
                iri().prop_map(NamedOrBlankNode::from),
                bnode().prop_map(NamedOrBlankNode::from)
            ],
            iri(),
            inner,
        )
            .prop_map(|(s, p, o)| Term::Triple(Box::new(Triple::new(s, p, o))))
    })
}

fn variables() -> impl Strategy<Value = Vec<String>> {
    prop::collection::vec(
        prop_oneof![
            4 => "[a-z][a-z0-9_]{0,4}",
            1 => prop::sample::select(&["é", "中文", "x\u{300}", "_a", "v1", "𝄞"][..]).prop_map(str::to_string),
        ],
        0..5,
    )
    .prop_map(|mut v| {
        // names are unique in a result
        let mut seen = std::collections::HashSet::new();
        v.retain(|n| seen.insert(n.clone()));
        v
    })
}

fn solutions() -> impl Strategy<Value = (Vec<String>, Vec<Vec<Option<Term>>>)> {
    variables().prop_flat_map(|vars| {
        let width = vars.len();
        let row = prop::collection::vec(
            prop_oneof![1 => Just(None), 3 => term().prop_map(Some)],
            width,
        );
        (Just(vars), prop::collection::vec(row, 0..5))
    })
}

/// The cases each property test runs, 100,000 unless `SPARKLES_ENCODE_CASES` says
/// otherwise.
fn cases() -> u32 {
    std::env::var("SPARKLES_ENCODE_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(100_000)
}

proptest! {
    #![proptest_config(ProptestConfig { cases: cases(), ..ProptestConfig::default() })]

    #[test]
    fn tsv_equals_sparesults((vars, rows) in solutions()) {
        check(Enc::Tsv, &vars, &rows)?;
    }

    #[test]
    fn tsv_counts_the_rows_a_failed_writer_took((vars, rows) in solutions(), limit in 0usize..400) {
        check_failing(Enc::Tsv, &vars, &rows, limit)?;
    }
}

// ---- fixtures ----

fn lit(v: &str, d: NamedNode) -> Option<Term> {
    Some(Literal::new_typed_literal(v, d).into())
}

fn tsv(vars: &[&str], rows: &[Vec<Option<Term>>]) -> String {
    let vars: Vec<String> = vars.iter().map(|v| v.to_string()).collect();
    let mut got = Vec::new();
    let (r, _) = encode(Enc::Tsv, &vars, rows, false, &mut got);
    r.unwrap();
    let (want, _) = oracle(Enc::Tsv, &vars, rows);
    assert_eq!(got, want, "the oracle disagrees");
    String::from_utf8(got).unwrap()
}

/// Valid lexical forms of the Turtle number and boolean rules are written bare, and any
/// other form of those datatypes is quoted with its datatype.
#[test]
fn tsv_writes_numbers_bare_only_in_their_turtle_forms() {
    let i = || xsd::INTEGER.into_owned();
    let d = || xsd::DECIMAL.into_owned();
    let f = || xsd::DOUBLE.into_owned();
    let b = || xsd::BOOLEAN.into_owned();
    let rows = vec![
        vec![
            lit("-12", i()),
            lit("+.5", d()),
            lit("1.e5", f()),
            lit("true", b()),
        ],
        vec![
            lit("1.0", i()),
            lit("5", d()),
            lit("1.5", f()),
            lit("TRUE", b()),
        ],
        vec![lit("", i()), lit(".", d()), lit(".e5", f()), lit("1", b())],
    ];
    assert_eq!(
        tsv(&["a", "b", "c", "d"], &rows),
        "?a\t?b\t?c\t?d\n\
         -12\t+.5\t1.e5\ttrue\n\
         \"1.0\"^^<http://www.w3.org/2001/XMLSchema#integer>\t\"5\"^^<http://www.w3.org/2001/XMLSchema#decimal>\t\"1.5\"^^<http://www.w3.org/2001/XMLSchema#double>\t\"TRUE\"^^<http://www.w3.org/2001/XMLSchema#boolean>\n\
         \"\"^^<http://www.w3.org/2001/XMLSchema#integer>\t\".\"^^<http://www.w3.org/2001/XMLSchema#decimal>\t\".e5\"^^<http://www.w3.org/2001/XMLSchema#double>\t\"1\"^^<http://www.w3.org/2001/XMLSchema#boolean>\n"
    );
}

/// Escapes, language tags with a direction, triple terms, Unicode names, unbound cells and
/// a result with no variables.
#[test]
fn tsv_escapes_strings_and_writes_directions_and_triple_terms() {
    let t = Term::Triple(Box::new(Triple::new(
        BlankNode::new_unchecked("b0"),
        NamedNode::new_unchecked("http://e/p"),
        Literal::new_directional_language_tagged_literal_unchecked(
            "a\"b",
            "en",
            BaseDirection::Rtl,
        ),
    )));
    let rows = vec![
        vec![
            Some(Literal::new_simple_literal("t\tn\nr\r q\" b\\ é").into()),
            None,
            Some(t),
        ],
        vec![None, None, None],
    ];
    assert_eq!(
        tsv(&["é", "中", "x"], &rows),
        "?é\t?中\t?x\n\"t\\tn\\nr\\r q\\\" b\\\\ é\"\t\t<<( _:b0 <http://e/p> \"a\\\"b\"@en--rtl )>>\n\t\t\n"
    );
    assert_eq!(tsv(&[], &[vec![], vec![]]), "\n\n\n");
}

/// A literal longer than the output buffer goes through in one write, and the rows around
/// it are counted when the writer fails inside it or after it.
#[test]
fn tsv_handles_values_longer_than_the_buffer() {
    let big = "x".repeat(20 << 10);
    let rows = vec![
        vec![Some(Literal::new_simple_literal("a").into())],
        vec![Some(Literal::new_simple_literal(&big).into())],
        vec![Some(Literal::new_simple_literal("b").into())],
    ];
    let vars = vec!["v".to_string()];
    let s = tsv(&["v"], &rows);
    assert_eq!(s.len(), 3 + 4 + big.len() + 3 + 4);
    let (_, ends) = oracle(Enc::Tsv, &vars, &rows);
    for limit in [
        ends[0] - 1,
        ends[0],
        ends[0] + 100,
        ends[1] - 1,
        ends[1],
        ends[2] - 1,
        ends[2],
    ] {
        check_failing(Enc::Tsv, &vars, &rows, limit).unwrap();
    }
}

/// Deeply nested triple terms, beyond what the generator makes.
#[test]
fn tsv_writes_deeply_nested_triple_terms() {
    let mut t: Term = NamedNode::new_unchecked("http://e/o").into();
    for _ in 0..64 {
        t = Term::Triple(Box::new(Triple::new(
            NamedNode::new_unchecked("http://e/s"),
            NamedNode::new_unchecked("http://e/p"),
            t,
        )));
    }
    tsv(&["t"], &[vec![Some(t)]]);
}

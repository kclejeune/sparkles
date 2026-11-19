//! The engine's encoders against sparesults, the byte oracle: fixed fixtures for the
//! byte-sensitive rules, a property test over generated solutions in both row views, and
//! the rows counted when the caller's writer fails partway.

use super::super::rowout::Explicit;
use super::super::source::RowView;
use super::super::{RowCount, io, write_buffered};
use super::{Csv, Json, Tsv};
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
    Csv,
    Json,
}

/// An encoder writing one answer, with the state a format keeps across its rows.
enum Running<'a> {
    Tsv,
    Csv,
    Json(Json<'a>),
}

impl Enc {
    fn oracle(self) -> QueryResultsFormat {
        match self {
            Enc::Tsv => QueryResultsFormat::Tsv,
            Enc::Csv => QueryResultsFormat::Csv,
            Enc::Json => QueryResultsFormat::Json,
        }
    }

    /// Write the header and return the encoder for the rows, its state charged first as
    /// select() charges it.
    fn start<'a>(self, out: &mut Explicit<'_, 'a>, vars: &[String]) -> Result<Running<'a>> {
        Ok(match self {
            Enc::Tsv => {
                Tsv::header(out, vars).map_err(io)?;
                Running::Tsv
            }
            Enc::Csv => {
                Csv::header(out, vars).map_err(io)?;
                Running::Csv
            }
            Enc::Json => {
                let json = Json::new(out.ctx(), vars)?;
                Json::header(out, vars).map_err(io)?;
                Running::Json(json)
            }
        })
    }
}

impl Running<'_> {
    fn row(
        &mut self,
        out: &mut Explicit<'_, '_>,
        row: RowView<'_>,
        width: usize,
    ) -> std::io::Result<()> {
        match self {
            Running::Tsv => Tsv::row(out, row, width),
            Running::Csv => Csv::row(out, row, width),
            Running::Json(json) => json.row(out, row, width),
        }
    }

    fn footer(&self, out: &mut Explicit<'_, '_>) -> std::io::Result<()> {
        match self {
            Running::Tsv | Running::Csv => Ok(()),
            Running::Json(_) => Json::footer(out),
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
        let mut enc = enc.start(&mut out, vars)?;
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
            out.end_row(1).map_err(io)?;
        }
        enc.footer(&mut out).map_err(io)
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
/// whose ends lie within the bytes it took, and it takes every byte up to the limit.
fn check_failing(
    enc: Enc,
    vars: &[String],
    rows: &[Vec<Option<Term>>],
    limit: usize,
) -> std::result::Result<(), TestCaseError> {
    let (want, ends) = oracle(enc, vars, rows);
    let limit = limit.min(want.len());
    for sparse in [false, true] {
        let mut w = Failing { limit, taken: 0 };
        let (r, n) = encode(enc, vars, rows, sparse, &mut w);
        prop_assert_eq!(w.taken, limit, "{:?} sparse {}: bytes taken", enc, sparse);
        if limit < want.len() {
            prop_assert!(r.is_err(), "{enc:?}: a failed writer must fail the output");
        } else {
            prop_assert!(r.is_ok(), "{enc:?}: a writer that takes it all: {r:?}");
        }
        let complete = ends.iter().filter(|&&e| e <= w.taken).count() as u64;
        prop_assert_eq!(n, complete, "{:?} sparse {} limit {}", enc, sparse, limit);
    }
    Ok(())
}

/// Records the length of each write it takes.
#[derive(Default)]
struct Pieces(Vec<usize>);

impl Write for Pieces {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.push(b.len());
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// The lengths of the writes the caller's writer takes when sparesults writes through the
/// staged mode, one write per row, as the formats it serializes did before the engine
/// encoded them.
fn oracle_pieces(enc: Enc, vars: &[String], rows: &[Vec<Option<Term>>]) -> Vec<usize> {
    let mut w = Pieces::default();
    let count = RowCount::new();
    let variables: Vec<Variable> = vars.iter().map(Variable::new_unchecked).collect();
    write_buffered(&mut w, &count, None, &mut |out, staged| {
        let mut s = QueryResultsSerializer::from_format(enc.oracle())
            .serialize_solutions_to_writer(&mut *out, variables.clone())
            .map_err(io)?;
        for row in rows {
            s.serialize(
                row.iter()
                    .enumerate()
                    .filter_map(|(c, t)| t.as_ref().map(|t| (variables[c].as_ref(), t.as_ref()))),
            )
            .map_err(io)?;
            staged.set(staged.get() + 1);
        }
        let _ = s.finish();
        Ok(())
    })
    .unwrap();
    assert_eq!(count.get(), rows.len() as u64);
    w.0
}

/// The encoder hands the caller's writer the same pieces as sparesults' row writes did,
/// so each piece ends where a row ends and a row longer than the buffer goes in one
/// write, which a size limit refuses whole.
fn check_pieces(
    enc: Enc,
    vars: &[String],
    rows: &[Vec<Option<Term>>],
) -> std::result::Result<(), TestCaseError> {
    let want = oracle_pieces(enc, vars, rows);
    for sparse in [false, true] {
        let mut w = Pieces::default();
        let (r, n) = encode(enc, vars, rows, sparse, &mut w);
        prop_assert!(r.is_ok(), "{enc:?} sparse {sparse}: {r:?}");
        prop_assert_eq!(n, rows.len() as u64);
        prop_assert_eq!(&w.0, &want, "{:?} sparse {}: write lengths", enc, sparse);
    }
    Ok(())
}

/// The length of the header sparesults writes, the bytes before the first row.
fn header_len(enc: Enc, vars: &[String]) -> usize {
    let footer = match enc {
        Enc::Tsv | Enc::Csv => 0,
        Enc::Json => b"]}}".len(),
    };
    oracle(enc, vars, &[]).0.len() - footer
}

/// Every piece the caller's writer takes ends where the header or a row ends, or at the
/// end of the document, so a row's pieces never reach it before the row is whole and a
/// row longer than the buffer goes in one write. sparesults' JSON writes each character
/// on its own, so its staged pieces drain mid-row and are no oracle for the pieces.
fn check_row_pieces(
    enc: Enc,
    vars: &[String],
    rows: &[Vec<Option<Term>>],
) -> std::result::Result<(), TestCaseError> {
    let (want, ends) = oracle(enc, vars, rows);
    let mut allowed: std::collections::BTreeSet<usize> = ends.into_iter().collect();
    allowed.insert(header_len(enc, vars));
    allowed.insert(want.len());
    for sparse in [false, true] {
        let mut w = Pieces::default();
        let (r, n) = encode(enc, vars, rows, sparse, &mut w);
        prop_assert!(r.is_ok(), "{enc:?} sparse {sparse}: {r:?}");
        prop_assert_eq!(n, rows.len() as u64);
        let mut at = 0;
        for &len in &w.0 {
            at += len;
            prop_assert!(
                allowed.contains(&at),
                "{:?} sparse {}: a piece ends at {}, inside a row",
                enc,
                sparse,
                at
            );
        }
        prop_assert_eq!(at, want.len());
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

/// Answers whose rows span several drains of the buffer: many rows, some of them near or
/// past the buffer's size and full of escaped bytes.
fn long_solutions() -> impl Strategy<Value = (Vec<String>, Vec<Vec<Option<Term>>>)> {
    let value = prop_oneof![
        6 => text(),
        1 => (1usize..12_000).prop_map(|n| "x".repeat(n)),
        1 => (1usize..6_000).prop_map(|n| "x\t".repeat(n)),
        1 => (8_180usize..8_200).prop_map(|n| "y".repeat(n)),
    ];
    let cell = prop_oneof![
        1 => Just(None),
        2 => term().prop_map(Some),
        2 => value.prop_map(|v| Some(Literal::new_simple_literal(v).into())),
    ]
    .boxed();
    (1usize..4).prop_flat_map(move |width| {
        let vars = (0..width).map(|i| format!("v{i}")).collect::<Vec<_>>();
        let row = prop::collection::vec(cell.clone(), width);
        (Just(vars), prop::collection::vec(row, 0..40))
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

    #[test]
    fn csv_equals_sparesults((vars, rows) in solutions()) {
        check(Enc::Csv, &vars, &rows)?;
    }

    #[test]
    fn csv_counts_the_rows_a_failed_writer_took((vars, rows) in solutions(), limit in 0usize..400) {
        check_failing(Enc::Csv, &vars, &rows, limit)?;
    }

    #[test]
    fn json_equals_sparesults((vars, rows) in solutions()) {
        check(Enc::Json, &vars, &rows)?;
    }

    #[test]
    fn json_counts_the_rows_a_failed_writer_took((vars, rows) in solutions(), limit in 0usize..400) {
        check_failing(Enc::Json, &vars, &rows, limit)?;
    }
}

proptest! {
    // each case writes up to a few hundred kilobytes
    #![proptest_config(ProptestConfig { cases: cases() / 50, ..ProptestConfig::default() })]

    #[test]
    fn tsv_pieces_equal_sparesults_row_writes((vars, rows) in long_solutions()) {
        check_pieces(Enc::Tsv, &vars, &rows)?;
    }

    #[test]
    fn tsv_counts_the_rows_a_failed_writer_took_over_drains(
        (vars, rows) in long_solutions(),
        at in 0.0f64..=1.0,
    ) {
        let total = oracle(Enc::Tsv, &vars, &rows).0.len();
        check_failing(Enc::Tsv, &vars, &rows, (total as f64 * at) as usize)?;
    }

    #[test]
    fn csv_pieces_equal_sparesults_row_writes((vars, rows) in long_solutions()) {
        check_pieces(Enc::Csv, &vars, &rows)?;
    }

    #[test]
    fn csv_counts_the_rows_a_failed_writer_took_over_drains(
        (vars, rows) in long_solutions(),
        at in 0.0f64..=1.0,
    ) {
        let total = oracle(Enc::Csv, &vars, &rows).0.len();
        check_failing(Enc::Csv, &vars, &rows, (total as f64 * at) as usize)?;
    }

    #[test]
    fn json_pieces_end_where_rows_end((vars, rows) in long_solutions()) {
        check_row_pieces(Enc::Json, &vars, &rows)?;
    }

    #[test]
    fn json_counts_the_rows_a_failed_writer_took_over_drains(
        (vars, rows) in long_solutions(),
        at in 0.0f64..=1.0,
    ) {
        let total = oracle(Enc::Json, &vars, &rows).0.len();
        check_failing(Enc::Json, &vars, &rows, (total as f64 * at) as usize)?;
    }
}

// ---- fixtures ----

fn lit(v: &str, d: NamedNode) -> Option<Term> {
    Some(Literal::new_typed_literal(v, d).into())
}

fn encoded(enc: Enc, vars: &[&str], rows: &[Vec<Option<Term>>]) -> String {
    let vars: Vec<String> = vars.iter().map(|v| v.to_string()).collect();
    let mut got = Vec::new();
    let (r, _) = encode(enc, &vars, rows, false, &mut got);
    r.unwrap();
    let (want, _) = oracle(enc, &vars, rows);
    assert_eq!(got, want, "the oracle disagrees");
    String::from_utf8(got).unwrap()
}

fn tsv(vars: &[&str], rows: &[Vec<Option<Term>>]) -> String {
    encoded(Enc::Tsv, vars, rows)
}

fn csv(vars: &[&str], rows: &[Vec<Option<Term>>]) -> String {
    encoded(Enc::Csv, vars, rows)
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

fn simple(v: &str) -> Option<Term> {
    Some(Literal::new_simple_literal(v).into())
}

/// Rows of 8,191, 8,192 and 8,193 bytes after a short row and after an empty buffer, and a
/// first row longer than the buffer, reach the caller's writer in the pieces sparesults'
/// row writes did.
#[test]
fn tsv_pieces_at_the_buffer_size() {
    let vars = vec!["v".to_string()];
    // a row is the value, two quotes and a line feed
    let row = |n: usize| vec![simple(&"z".repeat(n - 3))];
    let mut rows = vec![vec![simple("a")]];
    for n in [8_191, 8_192, 8_193, 8_192, 8_191] {
        rows.push(row(n));
        rows.push(vec![simple("b")]);
    }
    rows.extend([row(8_192), row(8_193), row(8_191), row(20_000)]);
    check_pieces(Enc::Tsv, &vars, &rows).unwrap();
    check_pieces(Enc::Tsv, &vars, &[row(20_000), vec![simple("a")]]).unwrap();
    check_pieces(Enc::Tsv, &vars, &[vec![simple(&"x\t".repeat(35_000))]]).unwrap();
    for limit in [0, 3, 4, 100, 8_000, 9_000, 40_000, 60_000, 90_000] {
        check_failing(Enc::Tsv, &vars, &rows, limit).unwrap();
    }
}

/// Refuses a write whole when it would take the bytes past `cap`, as the result-size
/// limit does.
struct Capped {
    cap: usize,
    taken: usize,
}

impl Write for Capped {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        if self.taken + b.len() > self.cap {
            return Err(std::io::Error::other("result size budget exceeded"));
        }
        self.taken += b.len();
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

const SOURCE_ERROR: &str = "the source's error after the last row";

/// The error of the encoder, and of sparesults through the staged mode, when the rows
/// are followed by a source error and the caller's writer refuses writes past `cap`.
fn errors_after(
    enc: Enc,
    vars: &[String],
    rows: &[Vec<Option<Term>>],
    cap: usize,
) -> (crate::error::Error, crate::error::Error) {
    let count = RowCount::new();
    let explicit = write_buffered(
        &mut Capped { cap, taken: 0 },
        &count,
        None,
        &mut |out, _| {
            let mut out = Explicit::new(out);
            let mut enc = enc.start(&mut out, vars)?;
            for row in rows {
                out.begin_row()?;
                enc.row(&mut out, RowView::Dense(row), vars.len())
                    .map_err(io)?;
                out.end_row(1).map_err(io)?;
            }
            Err(crate::error::Error::invalid(SOURCE_ERROR))
        },
    );
    let variables: Vec<Variable> = vars.iter().map(Variable::new_unchecked).collect();
    let staged =
        write_buffered(
            &mut Capped { cap, taken: 0 },
            &RowCount::new(),
            None,
            &mut |out, staged| {
                let mut s = QueryResultsSerializer::from_format(enc.oracle())
                    .serialize_solutions_to_writer(&mut *out, variables.clone())
                    .map_err(io)?;
                for row in rows {
                    s.serialize(row.iter().enumerate().filter_map(|(c, t)| {
                        t.as_ref().map(|t| (variables[c].as_ref(), t.as_ref()))
                    }))
                    .map_err(io)?;
                    staged.set(staged.get() + 1);
                }
                Err(crate::error::Error::invalid(SOURCE_ERROR))
            },
        );
    (explicit.unwrap_err(), staged.unwrap_err())
}

/// A row or header of exactly the buffer's size that did not fit where it started goes on
/// at once, as sparesults' write of it did, so a writer that refuses it fails the output
/// before the source's next error, which would otherwise hide the refusal.
#[test]
fn a_refused_piece_of_the_buffer_size_fails_before_a_later_error() {
    for enc in [Enc::Tsv, Enc::Csv] {
        // TSV quotes a simple literal and ends a line with LF, CSV writes it bare and ends
        // a line with CRLF, and both headers of one short name take 3 bytes
        let (quotes, eol) = match enc {
            Enc::Tsv => (2, 1),
            Enc::Csv => (0, 2),
            Enc::Json => unreachable!("the JSON case has its own test"),
        };
        let vars = vec!["v".to_string()];
        let rows = vec![vec![simple(&"z".repeat(8_192 - quotes - eol))]];
        // a name of 8,190 bytes makes a header of exactly the buffer's size in both
        let long = vec!["h".repeat(8_190)];
        for (case, (explicit, staged)) in [
            ("row", errors_after(enc, &vars, &rows, 3 + 8_191)),
            ("header", errors_after(enc, &long, &[], 8_191)),
        ] {
            for (path, e) in [("explicit", &explicit), ("staged", &staged)] {
                assert!(
                    matches!(e, crate::error::Error::Io(_)),
                    "{enc:?} {case} {path}: the refusal was hidden by {e:?}"
                );
            }
        }
    }
}

/// A sparse row of 70 columns with runs of more than 32 empty cells at its start, inside
/// and at its end, and an answer of many short rows that spans several drains.
#[test]
fn tsv_writes_long_tab_runs_and_many_short_rows() {
    let vars: Vec<String> = (0..70).map(|i| format!("c{i}")).collect();
    let mut row = vec![None; 70];
    row[33] = simple("m");
    row[35] = simple("n");
    let mut empty_but_one = vec![None; 70];
    empty_but_one[69] = simple("last");
    let rows = vec![row, vec![None; 70], empty_but_one];
    let refs: Vec<&str> = vars.iter().map(String::as_str).collect();
    let s = tsv(&refs, &rows);
    assert!(s.ends_with(&format!("{}\"last\"\n", "\t".repeat(69))));
    check_pieces(Enc::Tsv, &vars, &rows).unwrap();

    let vars = vec!["a".to_string(), "b".to_string()];
    let rows: Vec<Vec<Option<Term>>> = (0..5_000)
        .map(|i| {
            vec![
                simple(&format!("r{i}")),
                (i % 3 != 0).then(|| simple("q\"t")).flatten(),
            ]
        })
        .collect();
    tsv(&["a", "b"], &rows);
    check_pieces(Enc::Tsv, &vars, &rows).unwrap();
    for limit in [8_191, 8_192, 20_000, 50_000] {
        check_failing(Enc::Tsv, &vars, &rows, limit).unwrap();
    }
}

/// CSV writes IRIs and lexical forms bare, quotes a lexical form with a quote, comma, line
/// feed or carriage return and doubles its quotes, and writes a triple term's parts
/// separated by spaces.
#[test]
fn csv_quotes_only_the_lexical_forms_that_need_it() {
    let t = Term::Triple(Box::new(Triple::new(
        BlankNode::new_unchecked("b0"),
        NamedNode::new_unchecked("http://e/p,q"),
        Literal::new_language_tagged_literal_unchecked("a\"b", "en"),
    )));
    let rows = vec![
        vec![
            simple("plain\ttab"),
            Some(NamedNode::new_unchecked("http://e/a,b\"c").into()),
            Some(t),
        ],
        vec![simple("\"\""), simple("a,b"), simple("cr\rlf\n")],
        vec![lit("42", xsd::INTEGER.into_owned()), simple(""), None],
    ];
    assert_eq!(
        csv(&["é", "b", "c"], &rows),
        "é,b,c\r\n\
         plain\ttab,http://e/a,b\"c,_:b0 http://e/p,q \"a\"\"b\"\r\n\
         \"\"\"\"\"\",\"a,b\",\"cr\rlf\n\"\r\n\
         42,,\r\n"
    );
    assert_eq!(csv(&[], &[vec![], vec![]]), "\r\n\r\n\r\n");
}

/// CSV rows past the buffer and runs of more than 32 empty cells, as for TSV.
#[test]
fn csv_handles_long_rows_and_long_comma_runs() {
    let vars = vec!["v".to_string()];
    let row = |n: usize| vec![simple(&"z".repeat(n - 2))];
    let rows = vec![
        vec![simple("a")],
        row(8_191),
        row(8_192),
        row(8_193),
        vec![simple(&"\",".repeat(30_000))],
        vec![simple("b")],
    ];
    check_pieces(Enc::Csv, &vars, &rows).unwrap();
    for limit in [0, 3, 8_000, 9_000, 40_000, 80_000] {
        check_failing(Enc::Csv, &vars, &rows, limit).unwrap();
    }
    let vars: Vec<String> = (0..70).map(|i| format!("c{i}")).collect();
    let mut sparse = vec![None; 70];
    sparse[33] = simple("m");
    sparse[69] = simple("n");
    let rows = vec![sparse, vec![None; 70]];
    check(Enc::Csv, &vars, &rows).unwrap();
    check_pieces(Enc::Csv, &vars, &rows).unwrap();
}

/// Runs of more than 32 empty cells before, between and after two bound cells, in both
/// row views against the byte oracle.
#[test]
fn wide_gaps_in_both_views() {
    let vars: Vec<String> = (0..120).map(|i| format!("c{i}")).collect();
    let mut row = vec![None; 120];
    row[40] = simple("a");
    row[80] = simple("b");
    let rows = vec![row, vec![None; 120]];
    for enc in [Enc::Tsv, Enc::Csv] {
        check(enc, &vars, &rows).unwrap();
        check_pieces(enc, &vars, &rows).unwrap();
    }
}

fn json(vars: &[&str], rows: &[Vec<Option<Term>>]) -> String {
    encoded(Enc::Json, vars, rows)
}

/// The escapes json-event-parser spells: the short forms, `\u00XX` in lowercase hex for
/// the other control characters, and DEL, U+2028 and other characters as they are.
#[test]
fn json_escapes_strings_as_json_event_parser_does() {
    let all: String = (0u8..0x20).map(char::from).collect();
    let got = json(
        &["v"],
        &[
            vec![simple(&all)],
            vec![simple("q\"b\\s\u{7f}\u{2028}\u{feff}é中𝄞")],
            vec![simple("")],
        ],
    );
    assert!(
        got.contains(r#""\u0000\u0001\u0002\u0003\u0004\u0005\u0006\u0007\b\t\n\u000b\f\r\u000e"#)
    );
    assert!(got.contains(r#"\u001e\u001f""#));
    assert!(got.contains("q\\\"b\\\\s\u{7f}\u{2028}\u{feff}é中𝄞"));
}

/// Literals with languages, directions and datatypes, xsd:string without its datatype,
/// IRIs and blank nodes, triple terms, Unicode names, unbound cells and an answer without
/// variables.
#[test]
fn json_writes_every_term_form() {
    let s = NamedNode::new_unchecked("http://e/s\"q");
    let p = NamedNode::new_unchecked("http://e/p");
    let triple = Term::Triple(Box::new(Triple::new(
        NamedOrBlankNode::from(BlankNode::new_unchecked("b0")),
        p.clone(),
        Term::from(Literal::new_directional_language_tagged_literal_unchecked(
            "x",
            "ar",
            BaseDirection::Rtl,
        )),
    )));
    let got = json(
        &["a", "é中", "c"],
        &[
            vec![
                Some(s.clone().into()),
                Some(BlankNode::new_unchecked("n1").into()),
                lit("1", xsd::INTEGER.into()),
            ],
            vec![
                None,
                Some(Literal::new_language_tagged_literal_unchecked("hi", "en-GB").into()),
                Some(
                    Literal::new_directional_language_tagged_literal_unchecked(
                        "x",
                        "en",
                        BaseDirection::Ltr,
                    )
                    .into(),
                ),
            ],
            vec![lit("s", xsd::STRING.into()), Some(triple), None],
            vec![None, None, None],
        ],
    );
    assert_eq!(
        got,
        concat!(
            r#"{"head":{"vars":["a","é中","c"]},"results":{"bindings":["#,
            r#"{"a":{"type":"uri","value":"http://e/s\"q"},"é中":{"type":"bnode","value":"n1"},"#,
            r#""c":{"type":"literal","value":"1","datatype":"http://www.w3.org/2001/XMLSchema#integer"}},"#,
            r#"{"é中":{"type":"literal","value":"hi","xml:lang":"en-GB"},"#,
            r#""c":{"type":"literal","value":"x","xml:lang":"en","its:dir":"ltr"}},"#,
            r#"{"a":{"type":"literal","value":"s"},"é中":{"type":"triple","value":{"#,
            r#""subject":{"type":"bnode","value":"b0"},"predicate":{"type":"uri","value":"http://e/p"},"#,
            r#""object":{"type":"literal","value":"x","xml:lang":"ar","its:dir":"rtl"}}}},"#,
            r#"{}]}}"#
        )
    );
    assert_eq!(
        json(&[], &[vec![], vec![]]),
        r#"{"head":{"vars":[]},"results":{"bindings":[{},{}]}}"#
    );
    assert_eq!(
        json(&["x"], &[]),
        r#"{"head":{"vars":["x"]},"results":{"bindings":[]}}"#
    );
}

/// Values and rows around and past the buffer's size, a row of quotes past it whose
/// escapes double it, and deeply nested triple terms.
#[test]
fn json_handles_values_longer_than_the_buffer() {
    let vars = vec!["v".to_string()];
    let mut rows = vec![vec![simple("a")]];
    for n in [8_150, 8_160, 8_170, 8_180, 8_190, 8_200, 20_000] {
        rows.push(vec![simple(&"z".repeat(n))]);
    }
    rows.push(vec![simple(&"\"".repeat(9_000))]);
    rows.push(vec![simple(&"\u{1}".repeat(3_000))]);
    check(Enc::Json, &vars, &rows).unwrap();
    check_row_pieces(Enc::Json, &vars, &rows).unwrap();
    for limit in [0, 3, 100, 8_000, 9_000, 40_000, 60_000, 90_000] {
        check_failing(Enc::Json, &vars, &rows, limit).unwrap();
    }
    let mut t: Term = NamedNode::new_unchecked("http://e/o").into();
    for _ in 0..64 {
        t = Term::Triple(Box::new(Triple::new(
            NamedNode::new_unchecked("http://e/s"),
            NamedNode::new_unchecked("http://e/p"),
            t,
        )));
    }
    json(&["t"], &[vec![Some(t)]]);
}

/// A JSON row or header of exactly the buffer's size that did not fit where it started
/// goes on at once, so a writer that refuses it fails the output before the source's next
/// error. sparesults' JSON reaches the staged buffer a character at a time, so its staged
/// path is no oracle here.
#[test]
fn a_refused_json_piece_of_the_buffer_size_fails_before_a_later_error() {
    let vars = vec!["v".to_string()];
    let header = header_len(Enc::Json, &vars);
    // a row of an empty value, less the header and the footer
    let empty = oracle(Enc::Json, &vars, &[vec![simple("")]]).0.len() - header - 3;
    let rows = vec![vec![simple(&"z".repeat(8_192 - empty))]];
    let (e, _) = errors_after(Enc::Json, &vars, &rows, header + 8_191);
    assert!(
        matches!(e, crate::error::Error::Io(_)),
        "row: the refusal was hidden by {e:?}"
    );
    let one = header_len(Enc::Json, &["h".to_string()]);
    let long = vec!["h".repeat(8_192 - (one - 1))];
    assert_eq!(header_len(Enc::Json, &long), 8_192);
    let (e, _) = errors_after(Enc::Json, &long, &[], 8_191);
    assert!(
        matches!(e, crate::error::Error::Io(_)),
        "header: the refusal was hidden by {e:?}"
    );
}

fn small_answer() -> crate::sparql::QueryResult {
    let store = crate::store::Store::in_memory(crate::store::StoreOptions::default());
    let options = crate::sparql::QueryOptions {
        max_memory_bytes: Some(1 << 20),
        no_cache: true,
        ..Default::default()
    };
    crate::sparql::query(
        store.snapshot(),
        "SELECT ?o WHERE { VALUES ?o { 1 } }",
        &options,
    )
    .unwrap()
}

/// The most the query can still charge.
fn room(ctx: &std::sync::Arc<crate::sparql::ctx::Ctx>) -> u64 {
    let (mut lo, mut hi) = (0u64, 1 << 21);
    while lo + 1 < hi {
        let mid = (lo + hi) / 2;
        if crate::sparql::ctx::OwnedCharge::new(ctx, mid).is_ok() {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    lo
}

/// SPARQL JSON through a writer charged to `ctx`, one short row.
fn json_charged(ctx: &std::sync::Arc<crate::sparql::ctx::Ctx>, vars: &[String]) -> Result<()> {
    let row: Vec<Option<Term>> = vars.iter().map(|_| simple("x")).collect();
    write_buffered(
        &mut Vec::new(),
        &RowCount::new(),
        Some(ctx.clone()),
        &mut |out, _| {
            let mut out = Explicit::new(out);
            let mut enc = Enc::Json.start(&mut out, vars)?;
            out.begin_row()?;
            enc.row(&mut out, RowView::Dense(&row), vars.len())
                .map_err(io)?;
            out.end_row(1).map_err(io)?;
            enc.footer(&mut out).map_err(io)
        },
    )
}

/// The escaped keys of SPARQL JSON are charged to the query before they are allocated,
/// beside the writer's buffer and marks, and released with the writer.
#[test]
fn json_keys_are_charged_before_they_are_allocated() {
    use super::super::rowout::{FIRST_MARKS, MARK_BYTES, OUTPUT_BUFFER};
    let vars: Vec<String> = (0..3)
        .map(|i| format!("v{i}{}", "\"".repeat(1_000)))
        .collect();
    // each key is its name with every quote escaped, two quotes and a colon, and the
    // table holds one pointer per key
    let keys = 3 * (2 + 2 * 1_000 + 3 + std::mem::size_of::<Box<[u8]>>() as u64);
    assert_eq!(Json::charged(&vars), keys);
    let writer = OUTPUT_BUFFER as u64 + FIRST_MARKS as u64 * MARK_BYTES;
    for (left, fits) in [(writer + keys - 1, false), (writer + keys, true)] {
        let r = small_answer();
        let ctx = r.test_context().clone();
        let before = room(&ctx);
        let held = crate::sparql::ctx::OwnedCharge::new(&ctx, before - left).unwrap();
        let got = json_charged(&ctx, &vars);
        match got {
            Ok(()) => assert!(fits, "{left}: the keys were not charged"),
            Err(crate::error::Error::BudgetExceeded(crate::error::Budget {
                kind: crate::error::BudgetKind::Memory,
                ..
            })) => assert!(!fits, "{left}: refused with room for the keys"),
            Err(e) => panic!("the refusal lost its type: {e:?}"),
        }
        drop(held);
        assert_eq!(room(&ctx), before, "{left}: the charge outlived the writer");
    }
}

/// Json::new itself charges its keys before it allocates them: it is refused with one byte
/// less than the keys need, admitted with exactly that room, and gives the room back when
/// it is dropped, whatever the writer around it charges.
#[test]
fn json_new_charges_its_keys_itself() {
    let vars: Vec<String> = (0..3)
        .map(|i| format!("v{i}{}", "\n".repeat(100)))
        .collect();
    let keys = Json::charged(&vars);
    // two quotes, the name with every newline escaped, a colon and the key's pointer
    assert_eq!(
        keys,
        3 * (2 + 2 + 2 * 100 + 1 + std::mem::size_of::<Box<[u8]>>() as u64)
    );
    for (left, fits) in [(keys - 1, false), (keys, true)] {
        let r = small_answer();
        let ctx = r.test_context().clone();
        let before = room(&ctx);
        let held = crate::sparql::ctx::OwnedCharge::new(&ctx, before - left).unwrap();
        match Json::new(Some(&*ctx), &vars) {
            Ok(json) => {
                assert!(fits, "{left}: the keys were not charged");
                assert_eq!(
                    room(&ctx),
                    0,
                    "{left}: the keys charged more than they need"
                );
                drop(json);
            }
            Err(crate::error::Error::BudgetExceeded(crate::error::Budget {
                kind: crate::error::BudgetKind::Memory,
                ..
            })) => assert!(!fits, "{left}: refused with room for the keys"),
            Err(e) => panic!("the refusal lost its type: {e:?}"),
        }
        drop(held);
        assert_eq!(room(&ctx), before, "{left}: the charge outlived the keys");
    }
}

//! Tests of the datatype checker: the range algebra and sizes at the boundaries the
//! standards fix, the W3C OWL 2 test cases that involve datatypes as local problems,
//! and differential tests against a model finder that reads the data ranges directly.

use super::range::{Ranges, Rid, string_cell};
use super::sets::{CAP, Size};
use super::solve::{Answer, Atom, Bounds, Problem, Prop, check};
use super::value::{DVal, Unsupported, of_literal};
use crate::model::{DataRange, Datatype, FacetRestriction, Literal, iri};
use crate::vocab;
use proptest::prelude::*;
use std::collections::BTreeSet;

fn xsd(l: &str) -> String {
    format!("{}{l}", vocab::XSD)
}
fn lit(lex: &str, l: &str) -> Literal {
    Literal::typed(lex, &xsd(l))
}
fn dt(l: &str) -> DataRange {
    DataRange::Datatype(Datatype::new(xsd(l)))
}
fn dt_iri(i: &str) -> DataRange {
    DataRange::Datatype(Datatype::new(i))
}
fn restr(l: &str, fs: &[(&str, Literal)]) -> DataRange {
    DataRange::DatatypeRestriction(
        Datatype::new(xsd(l)),
        fs.iter()
            .map(|(f, v)| FacetRestriction {
                facet: iri(if f.starts_with("http") {
                    f.to_string()
                } else {
                    xsd(f)
                }),
                value: v.clone(),
            })
            .collect(),
    )
}
fn plain_restr(fs: &[(&str, Literal)]) -> DataRange {
    DataRange::DatatypeRestriction(
        Datatype::new(vocab::RDF_PLAIN_LITERAL),
        fs.iter()
            .map(|(f, v)| FacetRestriction {
                facet: iri(f.to_string()),
                value: v.clone(),
            })
            .collect(),
    )
}
fn one(ls: &[Literal]) -> DataRange {
    DataRange::OneOf(ls.iter().cloned().collect())
}
fn not(r: DataRange) -> DataRange {
    DataRange::ComplementOf(Box::new(r))
}
fn and(rs: Vec<DataRange>) -> DataRange {
    DataRange::IntersectionOf(rs.into_iter().collect())
}
fn or(rs: Vec<DataRange>) -> DataRange {
    DataRange::UnionOf(rs.into_iter().collect())
}
fn val(l: &Literal) -> DVal {
    of_literal(l).unwrap()
}

struct T {
    r: Ranges,
}
impl T {
    fn new() -> T {
        T { r: Ranges::new() }
    }
    fn of(&mut self, d: &DataRange) -> Rid {
        self.r.of(d).unwrap()
    }
    fn size(&mut self, d: &DataRange) -> Size {
        let x = self.of(d);
        self.r.size(x)
    }
    fn within(&mut self, a: &DataRange, b: &DataRange) -> bool {
        let (a, b) = (self.of(a), self.of(b));
        self.r.within(a, b).unwrap()
    }
    fn empty(&mut self, d: &DataRange) -> bool {
        let x = self.of(d);
        self.r.is_empty(x).unwrap()
    }
    fn has(&mut self, d: &DataRange, l: &Literal) -> bool {
        let x = self.of(d);
        self.r.contains(x, &val(l))
    }
}

#[test]
fn numeric_tower_nests() {
    let mut t = T::new();
    let chain = ["byte", "short", "int", "long", "integer", "decimal"];
    for w in chain.windows(2) {
        assert!(t.within(&dt(w[0]), &dt(w[1])), "{} within {}", w[0], w[1]);
        assert!(!t.within(&dt(w[1]), &dt(w[0])));
    }
    assert!(t.within(&dt("decimal"), &dt_iri(vocab::OWL_RATIONAL)));
    assert!(t.within(&dt_iri(vocab::OWL_RATIONAL), &dt_iri(vocab::OWL_REAL)));
    assert!(!t.within(&dt_iri(vocab::OWL_REAL), &dt_iri(vocab::OWL_RATIONAL)));
    assert_eq!(t.size(&dt("byte")), Size::exact(256));
    assert_eq!(t.size(&dt("int")), Size::exact(1 << 32));
    assert_eq!(t.size(&dt("unsignedLong")), Size::INFINITE);
    for d in ["double", "float"] {
        let m = and(vec![dt(d), dt_iri(vocab::OWL_REAL)]);
        assert!(t.empty(&m), "{d} is disjoint from owl:real");
    }
    assert!(t.empty(&and(vec![dt("double"), dt("float")])));
    assert!(t.has(&dt("int"), &lit("-0", "integer")));
    let third = Literal::typed("1/3", vocab::OWL_RATIONAL);
    assert!(t.has(&dt_iri(vocab::OWL_RATIONAL), &third));
    assert!(!t.has(&dt("decimal"), &third));
}

#[test]
fn real_facets() {
    let mut t = T::new();
    let r = restr(
        "integer",
        &[
            ("minInclusive", lit("0.5", "decimal")),
            ("maxInclusive", lit("2.5", "decimal")),
        ],
    );
    assert_eq!(t.size(&r), Size::exact(2));
    let gap = restr(
        "integer",
        &[
            ("minExclusive", lit("2", "integer")),
            ("maxExclusive", lit("3", "integer")),
        ],
    );
    assert!(t.empty(&gap));
    let dgap = restr(
        "decimal",
        &[
            ("minExclusive", lit("2", "integer")),
            ("maxExclusive", lit("3", "integer")),
        ],
    );
    assert_eq!(t.size(&dgap), Size::INFINITE);
    let point = restr(
        "decimal",
        &[
            ("minInclusive", lit("2.5", "decimal")),
            ("maxInclusive", lit("2.5", "decimal")),
        ],
    );
    assert_eq!(t.size(&point), Size::exact(1));
    let third = Literal::typed("1/3", vocab::OWL_RATIONAL);
    let dpoint = restr(
        "decimal",
        &[
            ("minInclusive", third.clone()),
            ("maxInclusive", third.clone()),
        ],
    );
    assert!(t.empty(&dpoint), "1/3 is no decimal");
    let rpoint = DataRange::DatatypeRestriction(
        Datatype::new(vocab::OWL_RATIONAL),
        [
            FacetRestriction {
                facet: iri(xsd("minInclusive")),
                value: third.clone(),
            },
            FacetRestriction {
                facet: iri(xsd("maxInclusive")),
                value: third,
            },
        ]
        .into_iter()
        .collect(),
    );
    assert_eq!(t.size(&rpoint), Size::exact(1));
    // the reals that are not rationals are infinite and hold no literal
    let irr = and(vec![
        dt_iri(vocab::OWL_REAL),
        not(dt_iri(vocab::OWL_RATIONAL)),
    ]);
    assert_eq!(t.size(&irr), Size::INFINITE);
    // a maximum of zero and a minimum of zero meet at one integer
    let z = restr(
        "nonNegativeInteger",
        &[("maxInclusive", lit("0", "integer"))],
    );
    assert_eq!(t.size(&z), Size::exact(1));
}

#[test]
fn float_order_and_identity() {
    let mut t = T::new();
    let f = |s: &str| lit(s, "float");
    assert_eq!(t.size(&dt("float")), Size::exact(4_278_190_083));
    // the zeros are equal for the facets and two values for identity
    let zz = restr(
        "float",
        &[("minInclusive", f("0")), ("maxInclusive", f("0"))],
    );
    assert_eq!(t.size(&zz), Size::exact(2));
    assert!(t.has(&zz, &f("-0")) && t.has(&zz, &f("+0")));
    let pos = restr("float", &[("minInclusive", f("+0"))]);
    assert!(t.has(&pos, &f("-0")));
    let gt = restr("float", &[("minExclusive", f("-0"))]);
    assert!(!t.has(&gt, &f("+0")) && !t.has(&gt, &f("-0")));
    assert!(t.has(&gt, &f("1.401298464324817e-45")));
    let lt = restr("float", &[("maxExclusive", f("0"))]);
    assert!(!t.has(&lt, &f("-0")));
    assert_eq!(t.size(&one(&[f("0")])), Size::exact(1));
    assert_ne!(val(&f("0")), val(&f("-0")));
    // NaN is in the datatype, excluded by every bound, and a NaN bound is empty
    assert!(t.has(&dt("float"), &f("NaN")));
    assert!(!t.has(&restr("float", &[("maxInclusive", f("INF"))]), &f("NaN")));
    assert!(t.empty(&restr("float", &[("minInclusive", f("NaN"))])));
    // Datatype-Float-Discrete-001: nothing lies between 0 and the least subnormal
    let disc = restr(
        "float",
        &[
            ("minExclusive", f("0.0")),
            ("maxExclusive", f("1.401298464324817e-45")),
        ],
    );
    assert!(t.empty(&disc));
    let edge = restr("double", &[("minExclusive", lit("INF", "double"))]);
    assert!(t.empty(&edge));
    let edge = restr("double", &[("maxExclusive", lit("-INF", "double"))]);
    assert!(t.empty(&edge));
    // Minus Infinity is not in owl:real
    assert!(!t.has(&dt_iri(vocab::OWL_REAL), &f("-INF")));
}

#[test]
fn boolean_cover() {
    let mut t = T::new();
    let tf = or(vec![
        one(&[lit("true", "boolean")]),
        one(&[lit("0", "boolean")]),
    ]);
    let (a, b) = (t.of(&tf), t.of(&dt("boolean")));
    assert_eq!(a, b);
    assert_eq!(t.size(&dt("boolean")), Size::exact(2));
}

#[test]
fn strings() {
    let mut t = T::new();
    let chain = [
        "language",
        "NCName",
        "Name",
        "NMTOKEN",
        "token",
        "normalizedString",
        "string",
    ];
    for w in chain.windows(2) {
        assert!(t.within(&dt(w[0]), &dt(w[1])), "{} within {}", w[0], w[1]);
        assert!(!t.within(&dt(w[1]), &dt(w[0])));
    }
    assert!(t.within(&dt("string"), &dt_iri(vocab::RDF_PLAIN_LITERAL)));
    assert!(t.empty(&and(vec![dt("string"), dt("anyURI")])));
    assert!(t.empty(&and(vec![dt("hexBinary"), dt("base64Binary")])));
    let n = |k: &str| lit(k, "nonNegativeInteger");
    // the PlainLiteral specification's count of the strings of length one
    assert_eq!(
        t.size(&restr("string", &[("length", n("1"))])),
        Size::exact(1_112_033)
    );
    assert_eq!(
        t.size(&restr("language", &[("length", n("1"))])),
        Size::exact(52)
    );
    assert_eq!(
        t.size(&restr("string", &[("length", n("0"))])),
        Size::exact(1)
    );
    assert_eq!(
        t.size(&restr("NMTOKEN", &[("length", n("0"))])),
        Size::exact(0)
    );
    let nt = and(vec![
        restr("normalizedString", &[("length", n("1"))]),
        not(dt("token")),
    ]);
    assert_eq!(t.size(&nt), Size::exact(1));
    let sn = and(vec![
        restr("string", &[("length", n("1"))]),
        not(dt("normalizedString")),
    ]);
    assert_eq!(t.size(&sn), Size::exact(3));
    let nn = and(vec![
        restr("Name", &[("length", n("1"))]),
        not(dt("NCName")),
    ]);
    assert_eq!(t.size(&nn), Size::exact(1));
    assert_eq!(string_cell(6, 2), Size::exact(52 * 52));
    // three letters, or one letter, a hyphen and a letter or digit
    assert_eq!(string_cell(6, 3), Size::exact(52 * 52 * 52 + 52 * 62));
    // a minus value in a counted cell
    let less = and(vec![
        restr("string", &[("length", n("1"))]),
        not(one(&[lit("a", "string")])),
    ]);
    assert_eq!(t.size(&less), Size::exact(1_112_032));
    // uncounted lengths give bounds
    let two = t.size(&restr("token", &[("length", n("2"))]));
    assert!(two.lo > 1_000_000 && two.lo < CAP && two.hi.is_some() && !two.is_exact());
    // hex and base64 count octets
    assert_eq!(
        t.size(&restr("hexBinary", &[("maxLength", n("1"))])),
        Size::exact(257)
    );
    assert!(t.has(
        &restr("base64Binary", &[("length", n("2"))]),
        &lit("AAA=", "base64Binary")
    ));
    assert!(t.has(
        &restr("hexBinary", &[("length", n("2"))]),
        &lit("0aFF", "hexBinary")
    ));
    assert_eq!(
        val(&lit("0aFF", "hexBinary")),
        val(&lit("0AFF", "hexBinary"))
    );
    assert!(t.has(&not(dt("int")), &lit("abc", "string")));
}

#[test]
fn language_tags() {
    let mut t = T::new();
    let en = Literal::lang("x", "en-US").unwrap();
    let de = Literal::lang("x", "de").unwrap();
    let plain = dt_iri(vocab::RDF_PLAIN_LITERAL);
    assert!(t.has(&plain, &en) && !t.has(&dt("string"), &en));
    let r = plain_restr(&[(vocab::RDF_LANG_RANGE, lit("EN", "string"))]);
    assert!(t.has(&r, &en) && !t.has(&r, &de));
    assert!(!t.has(&r, &lit("x", "string")));
    let eng = Literal::lang("x", "eng").unwrap();
    assert!(!t.has(&r, &eng), "a basic range matches whole subtags");
    let star = plain_restr(&[(vocab::RDF_LANG_RANGE, lit("*", "string"))]);
    let (u, p) = (t.of(&or(vec![star, dt("string")])), t.of(&plain));
    assert_eq!(u, p);
    let len = plain_restr(&[(&xsd("length"), lit("1", "integer"))]);
    assert!(t.has(&len, &en) && !t.has(&len, &Literal::lang("xy", "en").unwrap()));
    // a range minus a subrange keeps the rest of its tags
    let enus = plain_restr(&[(vocab::RDF_LANG_RANGE, lit("en-us", "string"))]);
    let rest = and(vec![r.clone(), not(enus)]);
    assert!(!t.has(&rest, &en) && t.has(&rest, &Literal::lang("x", "en-GB").unwrap()));
    assert_eq!(t.size(&rest), Size::INFINITE);
}

#[test]
fn date_times() {
    let mut t = T::new();
    let d = |s: &str| lit(s, "dateTime");
    // §4.7: an inclusive bound with an offset takes the instant, and a local time only
    // when it is above the bound even with +14:00
    let r = restr(
        "dateTime",
        &[("minInclusive", d("1956-01-01T04:00:00-05:00"))],
    );
    assert!(t.has(&r, &d("1956-01-01T10:00:00Z")));
    assert!(!t.has(&r, &d("1956-01-01T10:00:00")));
    assert!(t.has(&r, &d("1956-01-02T00:00:01")));
    assert!(!t.has(&r, &d("1956-01-01T23:00:00")));
    let a = d("1956-06-25T04:00:00-05:00");
    let b = d("1956-06-25T10:00:00+01:00");
    assert_ne!(val(&a), val(&b));
    assert!(t.has(&r, &a) && t.has(&r, &b));
    let at = |s: &str| {
        restr(
            "dateTime",
            &[("minInclusive", d(s)), ("maxInclusive", d(s))],
        )
    };
    assert_eq!(
        t.size(&at("2008-10-08T20:44:11.656+01:00")),
        Size::exact(1681)
    );
    assert_eq!(t.size(&at("2008-10-08T20:44:11.656")), Size::exact(1));
    assert!(t.within(&dt("dateTimeStamp"), &dt("dateTime")));
    assert!(!t.has(&dt("dateTimeStamp"), &d("2008-10-08T20:44:11")));
    // a local bound on a value with an offset
    let lb = restr("dateTime", &[("maxInclusive", d("2000-01-01T12:00:00"))]);
    assert!(t.has(&lb, &d("1999-12-31T21:59:59Z")));
    assert!(!t.has(&lb, &d("1999-12-31T22:00:00Z")));
    assert_eq!(
        val(&d("2000-01-01T24:00:00Z")),
        val(&d("2000-01-02T00:00:00Z"))
    );
    assert_eq!(
        val(&d("2000-01-01T12:00:00.50Z")),
        val(&d("2000-01-01T12:00:00.5Z"))
    );
}

#[test]
fn opaque_and_complement() {
    let mut t = T::new();
    assert_eq!(of_literal(&lit("x", "integer")), Err(Unsupported::IllTyped));
    assert_eq!(
        of_literal(&Literal::typed("<a/>", vocab::RDF_XML_LITERAL)),
        Err(Unsupported::XmlLiteral)
    );
    let p = restr("string", &[("pattern", lit("a+", "string"))]);
    assert_eq!(t.r.of(&p), Err(Unsupported::Pattern));
    let x = t.of(&restr("int", &[("minInclusive", lit("3", "int"))]));
    let nx = t.r.not(x);
    assert_eq!(t.r.not(nx), x);
    let lit_all = t.of(&dt_iri(vocab::RDFS_LITERAL));
    assert_eq!(t.r.join(x, nx), lit_all);
    assert_eq!(t.r.meet(x, nx), Ranges::EMPTY);
    assert!(t.r.bytes() > 0);
    // the dense strata keep no breakpoint outside the stratum, so a range and its
    // complement join to one id
    for r in [
        restr("decimal", &[("minInclusive", lit("5", "integer"))]),
        DataRange::DatatypeRestriction(
            Datatype::new(vocab::OWL_REAL),
            [FacetRestriction {
                facet: iri(xsd("minInclusive")),
                value: Literal::typed("1/3", vocab::OWL_RATIONAL),
            }]
            .into_iter()
            .collect(),
        ),
    ] {
        let x = t.of(&r);
        let nx = t.r.not(x);
        assert_eq!(t.r.join(x, nx), Ranges::FULL, "{r:?}");
        assert_eq!(t.r.meet(x, nx), Ranges::EMPTY);
    }
    assert!(!t.has(&dt_iri(vocab::RDF_XML_LITERAL), &lit("a", "string")));
    // the string-integer-clash case
    assert!(t.empty(&and(vec![dt("integer"), one(&[lit("aString", "string")])])));
}

// ------------------------------------------------------------- local problems ----

struct P {
    t: T,
    p: Problem,
}
impl P {
    fn new(props: usize) -> P {
        P {
            t: T::new(),
            p: Problem {
                props: vec![Prop::default(); props],
                ..Problem::default()
            },
        }
    }
    fn min(&mut self, d: usize, n: u64, r: &DataRange) -> &mut Self {
        let r = self.t.of(r);
        self.p.atoms.push(Atom::AtLeast(d, n, r));
        self
    }
    fn max(&mut self, d: usize, n: u64, r: &DataRange) -> &mut Self {
        let r = self.t.of(r);
        self.p.atoms.push(Atom::AtMost(d, n, r));
        self
    }
    fn all(&mut self, d: usize, r: &DataRange) -> &mut Self {
        self.max(d, 0, &not(r.clone()))
    }
    fn value(&mut self, d: usize, l: Literal) -> &mut Self {
        self.min(d, 1, &one(&[l]))
    }
    fn range(&mut self, d: usize, r: &DataRange) -> &mut Self {
        let r = self.t.of(r);
        self.p.props[d].ranges.push(r);
        self
    }
    fn check(&mut self) -> Answer {
        check(&mut self.t.r, &self.p, &Bounds::default())
    }
}

fn lit_top() -> DataRange {
    dt_iri(vocab::RDFS_LITERAL)
}

#[test]
fn w3c_cases() {
    use Answer::*;
    let i = |s: &str| lit(s, "integer");
    // Consistent Datatype restrictions with Different Types
    let mut p = P::new(1);
    p.all(0, &one(&[i("3"), lit("4", "int")]))
        .all(0, &one(&[lit("2", "short"), lit("3", "int")]))
        .min(0, 1, &one(&[i("3")]));
    assert_eq!(p.check(), Sat);
    // Different types in Datatype Restrictions and Complement
    let mut p = P::new(1);
    p.all(0, &one(&[i("3"), lit("4", "int")]))
        .all(0, &one(&[lit("2", "short"), i("3")]))
        .min(0, 1, &not(one(&[i("3")])));
    assert_eq!(p.check(), Unsat);
    // Consistent owl:real range with DataOneOf, and Minus Infinity is not in owl:real
    let mut p = P::new(1);
    p.all(0, &dt_iri(vocab::OWL_REAL))
        .min(0, 1, &one(&[lit("-INF", "float"), i("-0")]));
    assert_eq!(p.check(), Sat);
    p.max(0, 0, &one(&[lit("0", "unsignedInt")]));
    assert_eq!(p.check(), Unsat);
    // Contradicting datatype Restrictions
    let mut p = P::new(1);
    p.all(0, &one(&[i("3"), i("4")]))
        .all(0, &one(&[i("2"), i("3")]))
        .min(0, 1, &restr("integer", &[("minInclusive", i("4"))]));
    assert_eq!(p.check(), Unsat);
    // datatype-restriction-min-max-inconsistency
    let mut p = P::new(1);
    p.min(0, 1, &restr("integer", &[("minInclusive", i("18"))]))
        .all(0, &restr("integer", &[("maxInclusive", i("10"))]));
    assert_eq!(p.check(), Unsat);
    // Consistent-dateTime and Contradicting-dateTime-restrictions
    let d = |s: &str| lit(s, "dateTime");
    let mut p = P::new(1);
    p.min(
        0,
        1,
        &restr(
            "dateTime",
            &[("minInclusive", d("2008-10-08T20:44:11.656+01:00"))],
        ),
    )
    .all(
        0,
        &restr(
            "dateTime",
            &[("maxInclusive", d("2008-10-08T20:44:11.656+01:00"))],
        ),
    );
    assert_eq!(p.check(), Sat);
    let mut p = P::new(1);
    p.value(0, d("2007-10-08T20:44:11.656+01:00")).all(
        0,
        &restr(
            "dateTime",
            &[
                ("minInclusive", d("2008-07-08T20:44:11.656+01:00")),
                ("maxInclusive", d("2008-10-08T20:44:11.656+01:00")),
            ],
        ),
    );
    assert_eq!(p.check(), Unsat);
    // Datatype-DataComplementOf-001
    let mut p = P::new(1);
    p.range(0, &not(dt("positiveInteger")))
        .value(0, lit("-1", "negativeInteger"))
        .value(0, lit("A string", "string"));
    assert_eq!(p.check(), Sat);
    // Datatype-Float-Discrete-001
    let mut p = P::new(1);
    p.min(
        0,
        1,
        &restr(
            "float",
            &[
                ("minExclusive", lit("0.0", "float")),
                ("maxExclusive", lit("1.401298464324817e-45", "float")),
            ],
        ),
    );
    assert_eq!(p.check(), Unsat);
    // consistent-dataproperty-disjointness and Inconsistent Disjoint Dataproperties
    for (k, want) in [("18", Sat), ("10", Unsat)] {
        let mut p = P::new(2);
        p.p.disjoint.push((0, 1));
        p.value(0, i("10")).min(
            1,
            1,
            &restr("integer", &[("minInclusive", i(k)), ("maxInclusive", i(k))]),
        );
        assert_eq!(p.check(), want, "disjoint data properties at {k}");
    }
    // inconsistent_datatypes and string-integer-clash
    let mut p = P::new(1);
    p.all(0, &dt("string")).min(0, 1, &dt("integer"));
    assert_eq!(p.check(), Unsat);
    let mut p = P::new(1);
    p.range(0, &dt("integer"))
        .value(0, lit("aString", "string"));
    assert_eq!(p.check(), Unsat);
    // New-Feature-DataQCR-001: two names entail at least two strings
    let mut p = P::new(1);
    p.value(0, Literal::simple("Meg Griffin"))
        .value(0, Literal::simple("Megan Griffin"))
        .max(0, 1, &dt("string"));
    assert_eq!(p.check(), Unsat);
    // New-Feature-DisjointDataProperties-001
    let mut p = P::new(2);
    p.p.disjoint.push((0, 1));
    p.value(0, Literal::simple("Peter Griffin"))
        .value(1, Literal::simple("Peter Griffin"));
    assert_eq!(p.check(), Unsat);
    // New-Feature-Keys-006: a functional key property with two names
    let mut p = P::new(1);
    p.value(0, Literal::simple("Peter"))
        .value(0, Literal::simple("Kichwa-Tembo"))
        .max(0, 1, &lit_top());
    assert_eq!(p.check(), Unsat);
    // New-Feature-NegativeDataPropertyAssertion-001
    let mut p = P::new(1);
    p.value(0, i("5")).max(0, 0, &one(&[i("5")]));
    assert_eq!(p.check(), Unsat);
    // New-Feature-Rational-001, -002 and -003
    let rat = |s: &str| Literal::typed(s, vocab::OWL_RATIONAL);
    let mut p = P::new(1);
    p.all(0, &dt_iri(vocab::OWL_RATIONAL)).min(0, 2, &lit_top());
    assert_eq!(p.check(), Sat);
    let mut p = P::new(1);
    p.all(0, &one(&[lit("0.5", "decimal"), rat("1/2")]))
        .min(0, 2, &lit_top());
    assert_eq!(p.check(), Unsat);
    let mut p = P::new(1);
    p.all(0, &one(&[lit("0.3333333333333333", "decimal"), rat("1/3")]))
        .min(0, 2, &lit_top());
    assert_eq!(p.check(), Sat);
    // Plus and Minus Zero Integer, and Plus and Minus Zero are Distinct
    let mut p = P::new(1);
    p.all(0, &one(&[i("0")])).min(0, 1, &one(&[i("-0")]));
    assert_eq!(p.check(), Sat);
    let mut p = P::new(1);
    p.value(0, lit("+0.0", "float"))
        .value(0, lit("-0.0", "float"))
        .max(0, 1, &lit_top());
    assert_eq!(p.check(), Unsat);
    // Qualified-cardinality-boolean: exactly two Booleans entail each value
    let mut p = P::new(1);
    p.min(0, 2, &dt("boolean")).max(0, 2, &dt("boolean")).max(
        0,
        0,
        &one(&[lit("true", "boolean")]),
    );
    assert_eq!(p.check(), Unsat);
    // New-Feature-BottomDataProperty-001 is the empty range on a property
    let mut p = P::new(1);
    p.range(0, &not(lit_top())).min(0, 1, &lit_top());
    assert_eq!(p.check(), Unsat);
    // the same instant at two offsets is two values under a functional property
    let mut p = P::new(1);
    p.value(0, d("1956-06-25T04:00:00-05:00"))
        .value(0, d("1956-06-25T10:00:00+01:00"))
        .max(0, 1, &lit_top());
    assert_eq!(p.check(), Unsat);
}

#[test]
fn counting() {
    use Answer::*;
    // two existentials under a functional property meet in one value
    let mut p = P::new(1);
    p.min(0, 1, &dt("int"))
        .min(
            0,
            1,
            &restr("integer", &[("minInclusive", lit("5", "integer"))]),
        )
        .max(0, 1, &lit_top());
    assert_eq!(p.check(), Sat);
    p.min(0, 1, &dt("string"));
    assert_eq!(p.check(), Unsat);
    // a large minimum needs no witnesses
    let mut p = P::new(1);
    p.min(0, 4_000_000_000, &dt("int"));
    assert_eq!(p.check(), Sat);
    p.max(0, 3_999_999_999, &lit_top());
    assert_eq!(p.check(), Unsat);
    let mut p = P::new(1);
    p.min(0, 4_294_967_297, &dt("int"));
    assert_eq!(p.check(), Unsat);
    // counts past the saturation of sizes are not decided
    let mut p = P::new(1);
    p.min(0, 10_000_000_000_000_000_000, &dt("double"));
    assert_eq!(p.check(), Unknown(super::solve::Why::Uncounted));
    // a subproperty's values count for its superproperty
    let mut p = P::new(2);
    p.p.props[0].supers.push(1);
    p.min(0, 3, &dt("boolean"));
    assert_eq!(p.check(), Unsat);
    let mut p = P::new(2);
    p.p.props[0].supers.push(1);
    p.min(0, 2, &dt("byte")).max(1, 1, &lit_top());
    assert_eq!(p.check(), Unsat);
    let mut p = P::new(2);
    p.p.props[0].supers.push(1);
    p.range(1, &dt("string")).min(0, 1, &dt("byte"));
    assert_eq!(p.check(), Unsat);
    // minimums of zero always hold, and their negation never does
    let mut p = P::new(1);
    p.min(0, 0, &not(lit_top()));
    assert_eq!(p.check(), Sat);
    p.p.atoms[0] = p.p.atoms[0].negate();
    assert_eq!(p.check(), Unsat);
    // an uncounted cell: tokens of length 2 against a large maximum and minimum
    let mut p = P::new(1);
    p.min(
        0,
        4_000_000_000,
        &restr("token", &[("length", lit("2", "integer"))]),
    );
    assert_eq!(p.check(), Unknown(super::solve::Why::Uncounted));
}

// --------------------------------------------------------------- differential ----

/// A data range over a small universe, read directly by [`holds`].
#[derive(Clone, Debug)]
enum R {
    Bool,
    Int,
    Str,
    /// integers with optional bounds, each with whether it is inclusive
    IntRange(Option<(i64, bool)>, Option<(i64, bool)>),
    One(Vec<V>),
    Not(Box<R>),
    And(Box<R>, Box<R>),
    Or(Box<R>, Box<R>),
}

#[derive(Clone, Debug, PartialEq)]
enum V {
    B(bool),
    I(i64),
    S(String),
}

fn v_lit(v: &V) -> Literal {
    match v {
        V::B(b) => lit(&b.to_string(), "boolean"),
        V::I(i) => lit(&i.to_string(), "integer"),
        V::S(s) => Literal::simple(s.clone()),
    }
}

fn to_range(r: &R) -> DataRange {
    match r {
        R::Bool => dt("boolean"),
        R::Int => dt("integer"),
        R::Str => dt("string"),
        R::IntRange(lo, hi) => {
            let mut fs = Vec::new();
            if let Some((x, inc)) = lo {
                fs.push((
                    if *inc { "minInclusive" } else { "minExclusive" },
                    lit(&x.to_string(), "integer"),
                ));
            }
            if let Some((x, inc)) = hi {
                fs.push((
                    if *inc { "maxInclusive" } else { "maxExclusive" },
                    lit(&x.to_string(), "integer"),
                ));
            }
            restr("integer", &fs)
        }
        R::One(vs) => one(&vs.iter().map(v_lit).collect::<Vec<_>>()),
        R::Not(x) => not(to_range(x)),
        R::And(a, b) => and(vec![to_range(a), to_range(b)]),
        R::Or(a, b) => or(vec![to_range(a), to_range(b)]),
    }
}

/// Membership read from the semantics of each construct.
fn holds(r: &R, v: &V) -> bool {
    match (r, v) {
        (R::Bool, V::B(_)) | (R::Int, V::I(_)) | (R::Str, V::S(_)) => true,
        (R::IntRange(lo, hi), V::I(x)) => {
            lo.is_none_or(|(l, inc)| if inc { *x >= l } else { *x > l })
                && hi.is_none_or(|(h, inc)| if inc { *x <= h } else { *x < h })
        }
        (R::One(vs), v) => vs.contains(v),
        (R::Not(a), v) => !holds(a, v),
        (R::And(a, b), v) => holds(a, v) && holds(b, v),
        (R::Or(a, b), v) => holds(a, v) || holds(b, v),
        _ => false,
    }
}

fn arb_v() -> impl Strategy<Value = V> {
    prop_oneof![
        any::<bool>().prop_map(V::B),
        (-1i64..=3).prop_map(V::I),
        prop_oneof![Just("a"), Just("b")].prop_map(|s| V::S(s.to_string())),
    ]
}

fn arb_r() -> impl Strategy<Value = R> {
    let bound = proptest::option::of((-1i64..=3, any::<bool>()));
    let leaf = prop_oneof![
        Just(R::Bool),
        Just(R::Int),
        Just(R::Str),
        (bound.clone(), bound).prop_map(|(a, b)| R::IntRange(a, b)),
        proptest::collection::vec(arb_v(), 1..3).prop_map(R::One),
    ];
    leaf.prop_recursive(3, 12, 2, |inner| {
        prop_oneof![
            inner.clone().prop_map(|x| R::Not(Box::new(x))),
            (inner.clone(), inner.clone()).prop_map(|(a, b)| R::And(Box::new(a), Box::new(b))),
            (inner.clone(), inner).prop_map(|(a, b)| R::Or(Box::new(a), Box::new(b))),
        ]
    })
}

/// The finite universe of the size test: the Booleans, the integers -1 to 3 and two
/// strings.
fn finite() -> (R, Vec<V>) {
    let r = R::Or(
        Box::new(R::Or(
            Box::new(R::Bool),
            Box::new(R::IntRange(Some((-1, true)), Some((3, true)))),
        )),
        Box::new(R::One(vec![V::S("a".into()), V::S("b".into())])),
    );
    let mut vs = vec![V::B(false), V::B(true), V::S("a".into()), V::S("b".into())];
    vs.extend((-1..=3).map(V::I));
    (r, vs)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(300))]

    #[test]
    fn algebra_matches_semantics(r in arb_r()) {
        let mut t = T::new();
        let x = t.of(&to_range(&r));
        let (fin, vs) = finite();
        let mut probes = vs.clone();
        probes.extend([V::I(-7), V::I(9), V::S("c".into())]);
        for v in &probes {
            prop_assert_eq!(t.r.contains(x, &val(&v_lit(v))), holds(&r, v), "{:?}", v);
        }
        let both = R::And(Box::new(r.clone()), Box::new(fin));
        let y = t.of(&to_range(&both));
        let count = vs.iter().filter(|v| holds(&both, v)).count() as u128;
        prop_assert_eq!(t.r.size(y), Size::exact(count));
    }

    #[test]
    fn local_problem_matches_model_finder(
        atoms in proptest::collection::vec((0usize..2, any::<bool>(), 0u64..=2, arb_r()), 1..4),
        sub in any::<bool>(),
        disjoint in any::<bool>(),
        range in proptest::option::of(arb_r()),
    ) {
        let demand: u64 = atoms.iter().filter(|a| a.1).map(|a| a.2).sum();
        prop_assume!(demand <= 2);
        // the strings are an infinite cell, so the model finder gets fresh ones
        let universe = R::Or(
            Box::new(R::Or(Box::new(R::Bool), Box::new(R::IntRange(Some((0, true)), Some((1, true)))))),
            Box::new(R::Str),
        );
        let mut vs = vec![V::B(false), V::B(true), V::I(0), V::I(1), V::S("a".into()), V::S("b".into())];
        for k in 0..demand {
            vs.push(V::S(format!("fresh{k}")));
        }
        let mut t = T::new();
        let u = t.of(&to_range(&universe));
        let mut p = Problem { props: vec![Prop::default(); 2], ..Problem::default() };
        for q in &mut p.props {
            q.ranges.push(u);
        }
        if let Some(rr) = &range {
            let x = t.of(&to_range(rr));
            p.props[1].ranges.push(x);
        }
        if sub { p.props[0].supers.push(1); }
        if disjoint && !sub { p.disjoint.push((0, 1)); }
        for (d, least, n, r) in &atoms {
            let x = t.of(&to_range(r));
            p.atoms.push(if *least { Atom::AtLeast(*d, *n, x) } else { Atom::AtMost(*d, *n, x) });
        }
        let got = check(&mut t.r, &p, &Bounds::default());
        // the model finder: a set of values for each property
        let nv = vs.len();
        let mut found = false;
        'outer: for s0 in 0u32..(1 << nv) {
            for s1 in 0u32..(1 << nv) {
                let sets = [s0, s1];
                let ok_range = |d: usize| (0..nv).all(|i| sets[d] & (1 << i) == 0 || {
                    holds(&universe, &vs[i]) && (d == 0 || range.as_ref().is_none_or(|rr| holds(rr, &vs[i])))
                });
                if !ok_range(0) || !ok_range(1) { continue; }
                if sub && s0 & !s1 != 0 { continue; }
                if disjoint && !sub && s0 & s1 != 0 { continue; }
                let ok = atoms.iter().all(|(d, least, n, r)| {
                    let c = (0..nv).filter(|&i| sets[*d] & (1 << i) != 0 && holds(r, &vs[i])).count() as u64;
                    if *least { c >= *n } else { c <= *n }
                });
                if ok { found = true; break 'outer; }
            }
        }
        prop_assert_eq!(got, if found { Answer::Sat } else { Answer::Unsat });
    }
}

#[test]
fn values_parse() {
    assert_eq!(val(&lit("1.50", "decimal")), val(&lit("1.5", "decimal")));
    assert_eq!(
        val(&lit("+.5", "decimal")),
        val(&Literal::typed("1/2", vocab::OWL_RATIONAL))
    );
    assert_eq!(val(&lit("1", "boolean")), DVal::Bool(true));
    assert_eq!(val(&lit("1e0", "double")), val(&lit("1.", "double")));
    assert_eq!(val(&lit("0.1", "float")), val(&lit("0.100000001", "float")));
    assert_ne!(val(&lit("1", "float")), val(&lit("1", "double")));
    let s: BTreeSet<DVal> = [val(&lit("a", "string")), val(&lit("a", "token"))].into();
    assert_eq!(s.len(), 1, "a token is a string value");
}

#[test]
fn date_time_lexical_forms() {
    let d = |s: &str| of_literal(&lit(s, "dateTime"));
    let a = d("2000-01-01T00:00:00.1234567890123456789012345Z").unwrap();
    let b = d("2000-01-01T00:00:00.123456789012345678901234Z").unwrap();
    assert_ne!(a, b, "seconds have unbounded precision");
    assert_eq!(d("2001-02-29T00:00:00Z"), Err(Unsupported::IllTyped));
    assert!(d("0000-02-29T00:00:00Z").is_ok(), "year 0 is a leap year");
    assert!(d("-0001-12-31T23:59:59-14:00").is_ok());
    assert!(d("123456789-01-01T00:00:00").is_ok());
    for bad in [
        "2000-01-01T24:00:01Z",
        "2000-01-01T00:00:00+14:01",
        "2000-1-01T00:00:00",
        "02000-01-01T00:00:00",
        "2000-01-01T00:00:00.",
        "2000-01-01T00:00",
        "2000-01-01T00:00:00+1\u{e9}2a",
        "2000-01-01T00:00:00\u{e9}+01:00",
    ] {
        assert_eq!(d(bad), Err(Unsupported::IllTyped), "{bad}");
    }
    assert_eq!(
        of_literal(&lit("2000-01-01T00:00:00", "dateTimeStamp")),
        Err(Unsupported::IllTyped)
    );
}

#[test]
fn data_domain_beyond_the_map() {
    // rdfs:Literal is the whole data domain, which may hold values of no datatype of the
    // map (Direct Semantics §2.2)
    let mut t = T::new();
    let map: Vec<DataRange> = vocab::DATATYPE_MAP.iter().map(|d| dt_iri(d)).collect();
    let outside = not(or(map));
    assert_eq!(t.size(&outside), Size::INFINITE);
    let mut p = P::new(1);
    p.min(0, 1, &outside);
    assert_eq!(p.check(), Answer::Sat);
}

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
    assert_eq!(t.size(&dt("unsignedLong")), Size::exact(CAP));
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
    p.p.atoms[0] = p.p.atoms[0].negate().unwrap();
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

// ------------------------------------------------- regressions of the reviews ----

fn lang_range(r: &str) -> (&'static str, Literal) {
    (vocab::RDF_LANG_RANGE, lit(r, "string"))
}
fn nn(n: u64) -> Literal {
    lit(&n.to_string(), "nonNegativeInteger")
}

#[test]
fn language_ranges_count_well_formed_tags() {
    let mut t = T::new();
    // no well-formed tag has the primary subtag `a`
    let a = plain_restr(&[lang_range("a")]);
    assert!(t.empty(&a));
    let mut p = P::new(1);
    p.min(0, 1, &a);
    assert_eq!(p.check(), Answer::Unsat);
    // `i-klingon` is a whole irregular tag, so with length 0 it has one value
    let k = plain_restr(&[
        lang_range("i-klingon"),
        ("http://www.w3.org/2001/XMLSchema#length", nn(0)),
    ]);
    assert_eq!(t.size(&k), Size::exact(1));
    let mut p = P::new(1);
    p.min(0, 2, &k);
    assert_eq!(p.check(), Answer::Unsat);
    // `i` matches the thirteen irregular tags that start with it
    let i = plain_restr(&[
        lang_range("i"),
        ("http://www.w3.org/2001/XMLSchema#length", nn(0)),
    ]);
    assert_eq!(t.size(&i), Size::exact(13));
    // `en-a` is no tag, but `en-a-bb` is, so the range is infinite
    let ea = plain_restr(&[
        lang_range("en-a"),
        ("http://www.w3.org/2001/XMLSchema#length", nn(0)),
    ]);
    assert_eq!(t.size(&ea), Size::INFINITE);
    // a range less a longer one still governs infinitely many tags
    let en = plain_restr(&[lang_range("en")]);
    let enus = plain_restr(&[lang_range("en-us")]);
    assert_eq!(t.size(&and(vec![en, not(enus)])), Size::INFINITE);
}

#[test]
fn tagged_literals_are_checked() {
    let bad = Literal::lang("\u{0}", "en").unwrap();
    assert_eq!(of_literal(&bad), Err(Unsupported::IllTyped));
    // the typed form reads its tag by the same rule
    let typed = Literal::typed("s@a", vocab::RDF_PLAIN_LITERAL);
    assert_eq!(of_literal(&typed), Err(Unsupported::IllTyped));
    let typed = Literal::typed("s@en-gb-oed", vocab::RDF_PLAIN_LITERAL);
    assert!(of_literal(&typed).is_ok());
}

#[test]
fn saturated_sizes_subtract() {
    assert!(Size::exact(CAP).minus(1).lo < CAP);
    assert_eq!(Size::INFINITE.minus(1), Size::INFINITE);
    // (integer[0, CAP-4] ∪ the three strings of cell 0) less those three strings has
    // CAP-3 values
    let mut p = P::new(1);
    let ints = restr(
        "integer",
        &[
            ("minInclusive", lit("0", "integer")),
            ("maxInclusive", lit(&(CAP - 4).to_string(), "integer")),
        ],
    );
    let three = and(vec![
        restr("string", &[("length", nn(1))]),
        not(dt("normalizedString")),
    ]);
    let gone = one(&[
        lit("\t", "string"),
        lit("\n", "string"),
        lit("\r", "string"),
    ]);
    let x = and(vec![or(vec![ints.clone(), three]), not(gone)]);
    p.min(0, (CAP - 2) as u64, &x);
    assert_ne!(p.check(), Answer::Sat);
    // and the same with 256 bytes of hexBinary removed from a saturated union
    let big = restr(
        "integer",
        &[
            ("minInclusive", lit("0", "integer")),
            ("maxInclusive", lit("9223372036854775708", "integer")),
        ],
    );
    let hex1 = restr("hexBinary", &[("length", nn(1))]);
    let all: Vec<Literal> = (0u8..=255)
        .map(|b| lit(&format!("{b:02X}"), "hexBinary"))
        .collect();
    let x = and(vec![or(vec![big, hex1]), not(one(&all))]);
    let mut t = T::new();
    assert!(t.size(&x).lo <= CAP - 99);
    let mut p = P::new(1);
    p.min(0, (1u64 << 63) - 50, &x);
    assert_ne!(p.check(), Answer::Sat);
}

#[test]
fn short_strings_have_one_form() {
    let mut t = T::new();
    let z = nn(0);
    assert_eq!(
        t.of(&restr("NMTOKEN", &[("length", z.clone())])),
        Ranges::EMPTY
    );
    assert_eq!(
        t.of(&restr("language", &[("length", z.clone())])),
        Ranges::EMPTY
    );
    let s0 = t.of(&restr("string", &[("length", z.clone())]));
    assert_eq!(s0, t.of(&one(&[lit("", "string")])));
    assert_eq!(s0, t.of(&restr("token", &[("length", z.clone())])));
    assert_eq!(
        t.of(&and(vec![
            restr("string", &[("length", z.clone())]),
            not(one(&[lit("", "string")]))
        ])),
        Ranges::EMPTY
    );
    for d in ["hexBinary", "base64Binary", "anyURI"] {
        assert_eq!(
            t.of(&restr(d, &[("length", z.clone())])),
            t.of(&one(&[lit("", d)]))
        );
    }
    assert_eq!(
        t.of(&and(vec![
            restr("normalizedString", &[("length", nn(1))]),
            not(dt("token"))
        ])),
        t.of(&one(&[lit(" ", "string")]))
    );
    assert_eq!(
        t.of(&and(vec![
            restr("Name", &[("length", nn(1))]),
            not(dt("NCName"))
        ])),
        t.of(&one(&[lit(":", "string")]))
    );
    // complements keep them
    let e = t.r.not(Ranges::EMPTY);
    assert_eq!(e, Ranges::FULL);
    assert!(t.has(&not(dt("integer")), &lit("", "string")));
    assert!(!t.has(&not(dt("token")), &lit("", "string")));
}

#[test]
fn dense_forms_are_unique() {
    let mut t = T::new();
    let x = t.of(&restr("decimal", &[("minInclusive", lit("3", "integer"))]));
    let nx = t.r.not(x);
    let u = t.r.join(x, nx);
    assert_eq!(u, Ranges::FULL);
    assert_eq!(t.r.not(u), Ranges::EMPTY);
    assert_eq!(t.r.not(Ranges::EMPTY), Ranges::FULL);
    let d = t.of(&dt("decimal"));
    let split = t.of(&or(vec![
        restr("decimal", &[("maxInclusive", lit("1", "integer"))]),
        restr("decimal", &[("minInclusive", lit("1", "integer"))]),
    ]));
    assert_eq!(d, split);
}

#[test]
fn time_facets_read_long_fractions() {
    let mut r = Ranges::new();
    let x = restr(
        "dateTimeStamp",
        &[(
            "minInclusive",
            lit("2000-01-01T00:00:00.1234567890123456789Z", "dateTimeStamp"),
        )],
    );
    assert!(r.of(&x).is_ok());
    // a bound without an offset cannot restrict dateTimeStamp
    let x = restr(
        "dateTimeStamp",
        &[("minInclusive", lit("2000-01-01T00:00:00", "dateTime"))],
    );
    assert!(matches!(r.of(&x), Err(Unsupported::Facet(_))));
    let x = restr("dateTime", &[("minInclusive", lit("1", "integer"))]);
    assert!(matches!(r.of(&x), Err(Unsupported::Facet(_))));
}

#[test]
fn long_lexical_forms_are_left_out() {
    let long = format!("0.{}1", "0".repeat(super::value::MAX_DIGITS));
    assert_eq!(of_literal(&lit(&long, "decimal")), Err(Unsupported::Large));
    let t = format!(
        "2000-01-01T00:00:00.{}1Z",
        "0".repeat(super::value::MAX_DIGITS)
    );
    assert_eq!(of_literal(&lit(&t, "dateTime")), Err(Unsupported::Large));
    let ok = format!("0.{}1", "0".repeat(100));
    assert!(of_literal(&lit(&ok, "decimal")).is_ok());
    // a long string is no number
    assert!(of_literal(&lit(&"a".repeat(10_000), "string")).is_ok());
}

#[test]
fn large_counts_with_several_cells() {
    let mut p = P::new(1);
    p.min(0, 1_000_000, &dt("int"))
        .max(0, 999_999, &dt("int"))
        .min(0, 1, &dt("short"));
    assert_eq!(p.check(), Answer::Unsat);
    let mut p = P::new(1);
    let nonneg = restr("int", &[("minInclusive", lit("0", "int"))]);
    p.min(0, 1_000_000, &dt("int"))
        .max(0, 1_000_000, &dt("int"))
        .min(0, 500_000, &nonneg)
        .max(0, 500_000, &nonneg);
    assert_eq!(p.check(), Answer::Sat);
}

#[test]
fn edge_api() {
    let mut r = Ranges::default();
    assert!(r.of(&and(vec![dt("boolean")])).is_ok());
    assert_eq!(Atom::AtMost(0, u64::MAX, Ranges::FULL).negate(), None);
    let p = Problem {
        props: vec![Prop::default(); 64],
        ..Problem::default()
    };
    let b = Bounds {
        props: 64,
        ..Bounds::default()
    };
    assert_eq!(
        check(&mut r, &p, &b),
        Answer::Unknown(super::solve::Why::Bound)
    );
}

// ------------------------------------------------ the wide differential test ----

/// A value of the wide test: integers, halves as decimals, thirds as rationals, floats by
/// index into [`FLOATS`], strings, tagged strings, and date-times with an offset by their
/// hour and offset in hours.
#[derive(Clone, Debug, PartialEq)]
enum X {
    I(i64),
    D(i64),
    Q(i64),
    F(usize),
    S(&'static str),
    L(&'static str, &'static str),
    T(i64, i64),
}

const FLOATS: [f32; 5] = [-1.0, -0.0, 0.0, 1.5, f32::NAN];
const FLOAT_LEX: [&str; 5] = ["-1.0", "-0.0", "0.0", "1.5", "NaN"];
const STRS: [&str; 7] = ["", " ", "a", "ab", "a b", "\t", ":"];
const LANGS: [(&str, &str); 3] = [("a", "en"), ("a", "en-us"), ("", "fr")];

impl X {
    /// The value times 6, for the real numbers.
    fn six(&self) -> Option<i64> {
        match self {
            X::I(i) => Some(6 * i),
            X::D(h) => Some(3 * h),
            X::Q(t) => Some(2 * t),
            _ => None,
        }
    }
    fn lit(&self) -> Literal {
        match self {
            X::I(i) => lit(&i.to_string(), "integer"),
            X::D(h) => lit(&half(*h), "decimal"),
            X::Q(t) => Literal::typed(format!("{t}/3"), vocab::OWL_RATIONAL),
            X::F(k) => lit(FLOAT_LEX[*k], "float"),
            X::S(s) => Literal::simple(s.to_string()),
            X::L(s, t) => Literal::lang(*s, t).unwrap(),
            X::T(h, o) => lit(&stamp(*h, *o), "dateTimeStamp"),
        }
    }
}

fn half(h: i64) -> String {
    format!(
        "{}{}.{}",
        if h < 0 { "-" } else { "" },
        h.abs() / 2,
        if h % 2 == 0 { 0 } else { 5 }
    )
}
fn stamp(h: i64, o: i64) -> String {
    let off = if o == 0 {
        "Z".to_string()
    } else {
        format!("{}{:02}:00", if o < 0 { '-' } else { '+' }, o.abs())
    };
    format!("2000-01-01T{h:02}:00:00{off}")
}

#[derive(Clone, Copy, Debug)]
enum Side {
    Lo(bool),
    Hi(bool),
}

#[derive(Clone, Debug)]
enum W {
    /// decimal with a bound at a half
    Dec(Side, i64),
    /// owl:rational with a bound at a half
    Rat(Side, i64),
    Real,
    /// float with a bound at a float other than NaN
    Float(Side, usize),
    FloatAll,
    /// a string datatype by level of the chain, 0 for string to 4 for Name
    Level(usize),
    /// string with length, minLength or maxLength
    Len(u8, u64),
    /// rdf:PlainLiteral with length, minLength or maxLength
    PlainLen(u8, u64),
    Range(&'static str),
    /// dateTimeStamp with a bound at an hour with Z
    Time(Side, i64),
    One(Vec<X>),
    Not(Box<W>),
    And(Box<W>, Box<W>),
    Or(Box<W>, Box<W>),
}

const LEVELS: [&str; 5] = ["string", "normalizedString", "token", "NMTOKEN", "Name"];
const LEN_FACETS: [&str; 3] = ["length", "minLength", "maxLength"];

fn bound_facet(s: Side) -> &'static str {
    match s {
        Side::Lo(true) => "minInclusive",
        Side::Lo(false) => "minExclusive",
        Side::Hi(true) => "maxInclusive",
        Side::Hi(false) => "maxExclusive",
    }
}

fn w_range(w: &W) -> DataRange {
    let bound = |dt: &str, s: Side, v: Literal| {
        DataRange::DatatypeRestriction(
            Datatype::new(dt),
            [FacetRestriction {
                facet: iri(xsd(bound_facet(s))),
                value: v,
            }]
            .into_iter()
            .collect(),
        )
    };
    match w {
        W::Dec(s, h) => bound(&xsd("decimal"), *s, lit(&half(*h), "decimal")),
        W::Rat(s, h) => bound(vocab::OWL_RATIONAL, *s, lit(&half(*h), "decimal")),
        W::Real => dt_iri(vocab::OWL_REAL),
        W::Float(s, k) => bound(&xsd("float"), *s, lit(FLOAT_LEX[*k], "float")),
        W::FloatAll => dt("float"),
        W::Level(k) => dt(LEVELS[*k]),
        W::Len(f, n) => restr("string", &[(LEN_FACETS[*f as usize], nn(*n))]),
        W::PlainLen(f, n) => plain_restr(&[(&xsd(LEN_FACETS[*f as usize]), nn(*n))]),
        W::Range(r) => plain_restr(&[lang_range(r)]),
        W::Time(s, h) => bound(
            &xsd("dateTimeStamp"),
            *s,
            lit(&stamp(*h, 0), "dateTimeStamp"),
        ),
        W::One(xs) => one(&xs.iter().map(X::lit).collect::<Vec<_>>()),
        W::Not(a) => not(w_range(a)),
        W::And(a, b) => and(vec![w_range(a), w_range(b)]),
        W::Or(a, b) => or(vec![w_range(a), w_range(b)]),
    }
}

fn cmp_side<T: PartialOrd>(s: Side, x: T, b: T) -> bool {
    match s {
        Side::Lo(true) => x >= b,
        Side::Lo(false) => x > b,
        Side::Hi(true) => x <= b,
        Side::Hi(false) => x < b,
    }
}

fn level_of(s: &str) -> usize {
    // the deepest level of the chain that holds `s`, of string to Name
    let tok = !s.contains('\t') && !s.starts_with(' ') && !s.ends_with(' ') && !s.contains("  ");
    let nmtoken = !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == ':');
    let name = nmtoken && !s.starts_with(|c: char| c.is_ascii_digit());
    if s.contains('\t') {
        0
    } else if !tok {
        1
    } else if !nmtoken {
        2
    } else if !name {
        3
    } else {
        4
    }
}

fn w_holds(w: &W, x: &X) -> bool {
    let len_ok = |f: u8, n: u64, l: u64| match f {
        0 => l == n,
        1 => l >= n,
        _ => l <= n,
    };
    match (w, x) {
        (W::Dec(s, h), x) if !matches!(x, X::Q(_)) => {
            x.six().is_some_and(|v| cmp_side(*s, v, 3 * h))
        }
        (W::Rat(s, h), x) | (W::Dec(s, h), x @ X::Q(_)) => {
            matches!(w, W::Rat(..)) && x.six().is_some_and(|v| cmp_side(*s, v, 3 * h))
        }
        (W::Real, x) => x.six().is_some(),
        (W::Float(s, k), X::F(i)) => cmp_side(*s, FLOATS[*i], FLOATS[*k]),
        (W::FloatAll, X::F(_)) => true,
        (W::Level(k), X::S(s)) => level_of(s) >= *k,
        (W::Len(f, n), X::S(s)) => len_ok(*f, *n, s.chars().count() as u64),
        (W::PlainLen(f, n), X::S(s) | X::L(s, _)) => len_ok(*f, *n, s.chars().count() as u64),
        (W::Range(r), X::L(_, t)) => *r == "*" || t == r || t.starts_with(&format!("{r}-")),
        (W::Time(s, h), X::T(xh, o)) => cmp_side(*s, 60 * xh - o * 60, 60 * h),
        (W::One(xs), x) => xs.iter().any(|y| match (y, x) {
            (X::F(a), X::F(b)) => FLOAT_LEX[*a] == FLOAT_LEX[*b],
            _ => y == x,
        }),
        (W::Not(a), x) => !w_holds(a, x),
        (W::And(a, b), x) => w_holds(a, x) && w_holds(b, x),
        (W::Or(a, b), x) => w_holds(a, x) || w_holds(b, x),
        _ => false,
    }
}

fn all_x() -> Vec<X> {
    let mut v: Vec<X> = Vec::new();
    v.extend((-2..=3).map(X::I));
    v.extend([-3, -1, 1, 5].map(X::D));
    v.extend([-1, 1, 2].map(X::Q));
    v.extend((0..FLOATS.len()).map(X::F));
    v.extend(STRS.map(X::S));
    v.extend(LANGS.map(|(s, t)| X::L(s, t)));
    for h in 0..3 {
        for o in [0, 1] {
            v.push(X::T(h, o));
        }
    }
    v
}

fn arb_x() -> impl Strategy<Value = X> {
    let xs = all_x();
    (0..xs.len()).prop_map(move |i| xs[i].clone())
}

fn arb_w() -> impl Strategy<Value = W> {
    let side = prop_oneof![
        any::<bool>().prop_map(Side::Lo),
        any::<bool>().prop_map(Side::Hi)
    ];
    let leaf = prop_oneof![
        (side.clone(), -4i64..=6).prop_map(|(s, h)| W::Dec(s, h)),
        (side.clone(), -4i64..=6).prop_map(|(s, h)| W::Rat(s, h)),
        Just(W::Real),
        (side.clone(), 0usize..4).prop_map(|(s, k)| W::Float(s, k)),
        Just(W::FloatAll),
        (0usize..5).prop_map(W::Level),
        (0u8..3, 0u64..=3).prop_map(|(f, n)| W::Len(f, n)),
        (0u8..3, 0u64..=2).prop_map(|(f, n)| W::PlainLen(f, n)),
        prop_oneof![Just("*"), Just("en"), Just("en-us"), Just("fr")].prop_map(W::Range),
        (side, 0i64..=3).prop_map(|(s, h)| W::Time(s, h)),
        proptest::collection::vec(arb_x(), 1..4).prop_map(W::One),
    ];
    leaf.prop_recursive(3, 12, 2, |inner| {
        prop_oneof![
            inner.clone().prop_map(|x| W::Not(Box::new(x))),
            (inner.clone(), inner.clone()).prop_map(|(a, b)| W::And(Box::new(a), Box::new(b))),
            (inner.clone(), inner).prop_map(|(a, b)| W::Or(Box::new(a), Box::new(b))),
        ]
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(400))]

    /// Membership and sizes across the families against their semantics, and the laws of
    /// the algebra on ids for these families, whose finite classes the forms hold one way.
    #[test]
    fn wide_algebra_matches_semantics(a in arb_w(), b in arb_w()) {
        let mut t = T::new();
        let x = t.of(&w_range(&a));
        let xs = all_x();
        for v in &xs {
            prop_assert_eq!(t.r.contains(x, &val(&v.lit())), w_holds(&a, v), "{:?}", v);
        }
        let universe = W::One(xs.clone());
        let y = t.of(&w_range(&W::And(Box::new(a.clone()), Box::new(universe))));
        let count = xs.iter().filter(|v| w_holds(&a, v)).count() as u128;
        prop_assert_eq!(t.r.size(y), Size::exact(count));
        let nx = t.r.not(x);
        prop_assert_eq!(t.r.join(x, nx), Ranges::FULL);
        prop_assert_eq!(t.r.meet(x, nx), Ranges::EMPTY);
        let z = t.of(&w_range(&b));
        let nz = t.r.not(z);
        let m = t.r.meet(x, z);
        let lhs = t.r.not(m);
        let rhs = t.r.join(nx, nz);
        prop_assert_eq!(lhs, rhs);
        // `not` computed afresh, not through the table's reverse entry
        let again = t.r.nf(nx).not();
        prop_assert_eq!(t.r.intern(again), x);
    }
}

#[test]
fn unbounded_length_families_are_infinite() {
    let mut t = T::new();
    for d in [
        dt("string"),
        dt("language"),
        dt("anyURI"),
        dt("hexBinary"),
        dt("base64Binary"),
        plain_restr(&[lang_range("i-klingon")]),
        and(vec![
            dt("hexBinary"),
            not(restr("hexBinary", &[("length", nn(11))])),
        ]),
    ] {
        assert_eq!(t.size(&d), Size::INFINITE, "{d:?}");
    }
    // a saturated bounded length set stays finite
    let long = restr("hexBinary", &[("maxLength", nn(20))]);
    let s = t.size(&long);
    assert!(s.lo == CAP && !s.inf, "{s:?}");
    // removing values from an infinite range leaves it infinite
    let x = and(vec![
        dt("string"),
        not(one(&[
            lit("a", "string"),
            lit("b", "string"),
            lit("c", "string"),
        ])),
    ]);
    assert_eq!(t.size(&x), Size::INFINITE);
    let mut p = P::new(1);
    p.min(0, (CAP - 2) as u64, &x);
    assert_eq!(p.check(), Answer::Sat);
}

// ------------------------------------------------------- the rule on units ----

mod units_rule {
    use super::*;
    use crate::classify::dtcheck::range::Fail;
    use crate::classify::dtcheck::rule::{Group, Units, units};
    use crate::classify::dtcheck::solve::{Why, check_stopping};

    fn atom(p: &mut P, d: usize, n: u64, r: &DataRange) -> Atom {
        let r = p.t.of(r);
        Atom::AtLeast(d, n, r)
    }
    fn run(p: &mut P, body: &[(Atom, Group)]) -> Units {
        units(&mut p.t.r, &p.p, body, &Bounds::default(), &|| false).unwrap()
    }
    fn b(s: &str) -> Literal {
        lit(s, "boolean")
    }

    #[test]
    fn conflict_and_entailment() {
        // ≥2 d.boolean entails ∃d.{true}
        let mut p = P::new(1);
        p.min(0, 2, &dt("boolean"));
        let t = atom(&mut p, 0, 1, &one(&[b("true")]));
        let u = run(&mut p, &[(t, Group::Other)]);
        assert_eq!(u.entailed, vec![0]);
        assert!(!u.conflict && !u.cover);
        // ≥3 d.boolean is a conflict
        p.min(0, 3, &dt("boolean"));
        assert!(run(&mut p, &[]).conflict);
    }

    #[test]
    fn covers_and_classes() {
        // ∃d.boolean with ∃d.{true} and ∃d.{false} as body atoms
        let mut p = P::new(1);
        p.min(0, 1, &dt("boolean"));
        let t = atom(&mut p, 0, 1, &one(&[b("true")]));
        let f = atom(&mut p, 0, 1, &one(&[b("false")]));
        // two classes: a cover remains
        let u = run(&mut p, &[(t, Group::Only(1)), (f, Group::Only(2))]);
        assert!(u.cover && u.classes.is_empty() && u.entailed.is_empty());
        // one class for both: the class is given and no cover remains
        let u = run(&mut p, &[(t, Group::Only(7)), (f, Group::Only(7))]);
        assert_eq!(u.classes, vec![7]);
        assert!(!u.cover);
        // a free atom takes no part
        let u = run(&mut p, &[(t, Group::Free), (f, Group::Other)]);
        assert!(!u.cover && u.classes.is_empty());
        // Neon's cover, whose first disjunct's range contains the others:
        // ∃d.int[1,2] ⊓ ∃d.int[2,3] ⊑ ≥2 d.integer ⊔ ∃d.{2}
        let i = |s: &str| lit(s, "int");
        let mut p = P::new(1);
        p.min(
            0,
            1,
            &restr("int", &[("minInclusive", i("1")), ("maxInclusive", i("2"))]),
        )
        .min(
            0,
            1,
            &restr("int", &[("minInclusive", i("2")), ("maxInclusive", i("3"))]),
        );
        let two = atom(&mut p, 0, 2, &dt("integer"));
        let v2 = atom(&mut p, 0, 1, &one(&[i("2")]));
        let u = run(&mut p, &[(two, Group::Other), (v2, Group::Other)]);
        assert!(u.cover && u.entailed.is_empty());
    }

    #[test]
    fn negative_assertions_are_conflicts() {
        // ∃d.boolean with ∀d.¬{true} and ∀d.¬{false} on one constant
        let mut p = P::new(1);
        p.min(0, 1, &dt("boolean"))
            .all(0, &not(one(&[b("true")])))
            .all(0, &not(one(&[b("false")])));
        assert!(run(&mut p, &[]).conflict);
    }

    #[test]
    fn stop_flag() {
        let mut p = P::new(1);
        p.min(0, 1, &dt("boolean"));
        assert_eq!(
            check_stopping(&mut p.t.r, &p.p, &Bounds::default(), &|| true),
            Answer::Unknown(Why::Stopped)
        );
        assert_eq!(
            units(&mut p.t.r, &p.p, &[], &Bounds::default(), &|| true),
            Err(Why::Stopped)
        );
        let r = one(&[b("true"), b("false")]);
        assert_eq!(p.t.r.of_stopping(&r, &|| true), Err(Fail::Stopped));
        // the flag set after the search starts still stops it: a pigeonhole over many
        // types that needs more than one poll interval
        let n = std::cell::Cell::new(0u32);
        let stop = || {
            n.set(n.get() + 1);
            n.get() > 3
        };
        let mut p = P::new(3);
        for d in 0..3 {
            p.min(d, 40, &dt("integer"));
        }
        p.max(0, 100, &dt("integer"));
        let got = check_stopping(&mut p.t.r, &p.p, &Bounds::default(), &stop);
        assert!(matches!(got, Answer::Sat | Answer::Unknown(Why::Stopped)));
    }

    #[test]
    fn one_shot_stop_in_search_returns_stopped_not_bound() {
        // A one-shot stop firing once during search must still report Why::Stopped,
        // never Why::Bound after a second poll returns false. Using `nodes = 1024` makes
        // `budget.is_multiple_of(1024)` fire deterministically on the first search node.
        let mut p = P::new(4);
        for d in 0..4 {
            p.min(d, 5, &dt("integer"));
            p.max(d, 5, &dt("integer"));
        }
        // Poll 1: check_bounded entry; poll 2: cell loop; poll 3: first search node.
        let polls = std::cell::Cell::new(0u32);
        let one_shot = || {
            let k = polls.get() + 1;
            polls.set(k);
            k == 3
        };
        let bounds = Bounds {
            nodes: 1024,
            ..Bounds::default()
        };
        let got = check_stopping(&mut p.t.r, &p.p, &bounds, &one_shot);
        assert_eq!(got, Answer::Unknown(Why::Stopped));
        assert_eq!(polls.get(), 3);
    }

    #[test]
    fn large_one_of_stops_before_growing_past_admit_limit() {
        let mut r = Ranges::new();
        let mut defs = super::super::range::Defs::default();
        let lits: std::collections::BTreeSet<Literal> = (0..200)
            .map(|i| Literal::typed(format!("{i:0>200}"), vocab::XSD_STRING))
            .collect();
        let range = DataRange::OneOf(lits);
        let admitted = std::cell::Cell::new(0usize);
        let cap = 4096usize;
        let got = r.of_defined_bounded(&range, &mut defs, &|| false, &|bytes| {
            let next = admitted.get().saturating_add(bytes);
            if next > cap {
                false
            } else {
                admitted.set(next);
                true
            }
        });
        assert_eq!(got, Err(Fail::Memory));
        assert!(admitted.get() <= cap);
        // Refusal precedes interning, so no partial OneOf prefix was retained in Ranges.
        assert_eq!(r.bytes(), Ranges::new().bytes());
    }

    #[test]
    fn large_lang_range_facet_stops_before_building_facet_nf() {
        let mut r = Ranges::new();
        let mut defs = super::super::range::Defs::default();
        // A long valid BCP 47 language range (subtags of 1..=8 alnum separated by '-').
        let long_tag = std::iter::repeat_n("abcdef", 1000)
            .collect::<Vec<_>>()
            .join("-");
        let range = plain_restr(&[(vocab::RDF_LANG_RANGE, Literal::simple(&long_tag))]);
        let admitted = std::cell::Cell::new(0usize);
        let cap = 1024usize;
        let got = r.of_defined_bounded(&range, &mut defs, &|| false, &|bytes| {
            let next = admitted.get().saturating_add(bytes);
            if next > cap {
                false
            } else {
                admitted.set(next);
                true
            }
        });
        assert_eq!(got, Err(Fail::Memory));
        assert!(admitted.get() <= cap);
        assert_eq!(r.bytes(), Ranges::new().bytes());
    }

    #[test]
    fn atom_id_denies_capacity_growth_before_mutating_table() {
        use super::super::atoms::{DtKey, DtRun, DtTable, Halted, Stop};
        use super::super::comp::Comps;
        use super::super::range::Defs;
        use crate::classify::{Limits, numeric};

        let mut table = DtTable::default();
        // Fill until `atoms` is exactly at a large Vec capacity boundary (64).
        for n in 0..64u64 {
            table.id(DtKey {
                role: 0,
                at_least: true,
                n,
                range: Ranges::FULL,
            });
        }
        assert_eq!(table.atoms.len(), 64);
        assert_eq!(table.atoms.capacity(), 64);
        let before_bytes = table.bytes();
        let before_atoms_cap = table.atoms.capacity();
        let growth = table.next_growth_bytes();
        assert!(growth >= 64 * std::mem::size_of::<DtKey>());

        // Set cap strictly above fixed entry size (64 B) but strictly below the planned
        // Vec capacity doubling.
        let cap = before_bytes + 64;
        let limits = Limits {
            max_memory: Some(cap),
            ..Limits::default()
        };
        let budget = numeric::Budget::new(&limits, before_bytes, None);
        let mut run = DtRun {
            comps: Comps::default(),
            defs: Defs::default(),
            stop: Stop::default(),
            budget: Some(budget),
            stopped: None,
        };
        let next = DtKey {
            role: 0,
            at_least: true,
            n: 64,
            range: Ranges::FULL,
        };
        assert_eq!(run.atom_id(&mut table, next), None);
        assert!(matches!(run.stopped, Some(Halted::Memory(_))));
        assert_eq!(table.atoms.len(), 64);
        assert_eq!(table.atoms.capacity(), before_atoms_cap);
        assert_eq!(table.bytes(), before_bytes);
    }

    #[test]
    fn shared_def_expansion_does_not_double_charge_ast_in_el_range_or_of_in() {
        use super::super::comp::{CompFail, Comps};
        use super::super::range::Defs;
        use crate::model::{AxiomKind, Class, ClassExpression, DataProperty};
        use rustc_hash::FxHashMap;
        use std::sync::Arc;

        let lits: std::collections::BTreeSet<Literal> = (0..250)
            .map(|i| Literal::typed(format!("{i:0>200}"), vocab::XSD_STRING))
            .collect();
        let big = Arc::new(DataRange::OneOf(lits));
        let dt_big = Datatype::new("urn:dt:big");
        let mut map = FxHashMap::default();
        map.insert(dt_big.clone(), Arc::clone(&big));
        let defs = Defs::new(map);
        let ast_bytes = super::super::range::data_range_bytes(&big);
        assert!(ast_bytes > 40_000);

        // Two uses of `:big` in EL-like positions. Under a tight scratch cap well below
        // `ast_bytes`, `Walk::el_range` checks `el_defs` without cloning the AST, and
        // `ensure_scratch` only pays shallow Arc-map bytes; conversion then stops cleanly
        // inside `DataOneOf` literal admission without cloning the AST.
        let ax = [
            AxiomKind::SubClassOf(
                ClassExpression::Class(Class::new("urn:dt:A")),
                ClassExpression::DataSomeValuesFrom(
                    vec![DataProperty::new("urn:dt:d")],
                    DataRange::Datatype(dt_big.clone()),
                ),
            ),
            AxiomKind::SubClassOf(
                ClassExpression::Class(Class::new("urn:dt:B")),
                ClassExpression::DataSomeValuesFrom(
                    vec![DataProperty::new("urn:dt:d")],
                    DataRange::Datatype(dt_big),
                ),
            ),
        ];
        let refs: Vec<&AxiomKind> = ax.iter().collect();
        let admitted = std::cell::Cell::new(0usize);
        let cap = 8192usize;
        let res = Comps::of_bounded(&refs, &defs, &|| false, &|bytes| {
            let next = admitted.get().saturating_add(bytes);
            if next > cap {
                false
            } else {
                admitted.set(next);
                true
            }
        });
        assert!(matches!(res, Err(CompFail::Memory)));
        assert!(admitted.get() <= cap);
        // The immutable AST Arc must have no stray cloned references after the call.
        assert_eq!(Arc::strong_count(&big), 2);
    }
}

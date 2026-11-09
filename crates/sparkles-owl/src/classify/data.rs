//! The concrete domain of OWL 2 EL: the datatypes of the profile's datatype map, the
//! values of literals and the data ranges the profile allows, as elements of a small
//! lattice.
//!
//! The datatypes of OWL 2 EL (Profiles §2.2.1) form chains under `rdfs:Literal`
//! (Structural Specification §4): `owl:real ⊃ owl:rational ⊃ xsd:decimal ⊃ xsd:integer ⊃
//! xsd:nonNegativeInteger`, `rdf:PlainLiteral ⊃ xsd:string ⊃ xsd:normalizedString ⊃
//! xsd:token ⊃ xsd:NMTOKEN ⊃ xsd:Name ⊃ xsd:NCName` and `xsd:dateTime ⊃
//! xsd:dateTimeStamp`, while `xsd:anyURI`, `xsd:hexBinary`, `xsd:base64Binary` and
//! `rdf:XMLLiteral` stand alone. Datatypes on different chains have disjoint value
//! spaces (§4.5 and §4.6 state it of the binary datatypes and of `xsd:anyURI` and
//! `xsd:string`). Every value space is infinite, so a data range of the profile, an
//! intersection of datatypes and one-literal enumerations, is empty, one datatype or one
//! value, and the profile needs no case analysis over data.
//!
//! A literal's value is canonical, so that two literals are the same value exactly when
//! their values are equal: exact rationals for the numbers, strings, language-tagged
//! strings with lower-case tags, binary data and date-times as an instant with its
//! offset, since §4.7 makes two date-times with different offsets different values. A
//! literal without such a value, an XML literal, an ill-typed one, one of a datatype
//! outside the map, and a datatype outside the map, are opaque
//! atoms that only equal themselves, and their axioms are reported as used in part.

use super::numeric::{self, Budget, Number, NumericLedger, Reservation};
use super::{Error, Limits};
use crate::datatypes;
use crate::model::{DataRange, Datatype, Literal};
use crate::vocab;
use rustc_hash::FxHashMap;
use std::sync::Arc;

/// A datatype of the OWL 2 EL datatype map.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Kind {
    Real,
    Rational,
    Decimal,
    Integer,
    NonNegativeInteger,
    PlainLiteral,
    String,
    NormalizedString,
    Token,
    Nmtoken,
    Name,
    NcName,
    AnyUri,
    HexBinary,
    Base64Binary,
    XmlLiteral,
    DateTime,
    DateTimeStamp,
}

impl Kind {
    /// The datatype of an IRI, if the EL datatype map has it. `rdfs:Literal` is the top
    /// of the lattice and has no kind.
    pub fn of(iri: &str) -> Option<Kind> {
        use Kind::*;
        if iri == vocab::OWL_REAL {
            return Some(Real);
        }
        if iri == vocab::OWL_RATIONAL {
            return Some(Rational);
        }
        if iri == vocab::RDF_PLAIN_LITERAL {
            return Some(PlainLiteral);
        }
        if iri == vocab::RDF_XML_LITERAL {
            return Some(XmlLiteral);
        }
        Some(match iri.strip_prefix(vocab::XSD)? {
            "decimal" => Decimal,
            "integer" => Integer,
            "nonNegativeInteger" => NonNegativeInteger,
            "string" => String,
            "normalizedString" => NormalizedString,
            "token" => Token,
            "NMTOKEN" => Nmtoken,
            "Name" => Name,
            "NCName" => NcName,
            "anyURI" => AnyUri,
            "hexBinary" => HexBinary,
            "base64Binary" => Base64Binary,
            "dateTime" => DateTime,
            "dateTimeStamp" => DateTimeStamp,
            _ => return None,
        })
    }

    /// The next larger datatype on the kind's chain.
    fn parent(self) -> Option<Kind> {
        use Kind::*;
        Some(match self {
            Rational => Real,
            Decimal => Rational,
            Integer => Decimal,
            NonNegativeInteger => Integer,
            String => PlainLiteral,
            NormalizedString => String,
            Token => NormalizedString,
            Nmtoken => Token,
            Name => Nmtoken,
            NcName => Name,
            DateTimeStamp => DateTime,
            _ => return None,
        })
    }

    /// Whether every value of `self` is a value of `other`.
    pub fn within(self, other: Kind) -> bool {
        let mut k = Some(self);
        while let Some(x) = k {
            if x == other {
                return true;
            }
            k = x.parent();
        }
        false
    }
}

/// The canonical value of a literal.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Value {
    /// a rational number in lowest terms, with a positive denominator
    Num(Number),
    Str(String),
    /// a string with a lower-case language tag, a value of `rdf:PlainLiteral` alone
    Lang(String, String),
    Uri(String),
    Hex(String),
    Base64(String),
    /// the instant of a date-time, as `oxsdatatypes` encodes it, and its offset in
    /// minutes
    Time([u8; 16], Option<i16>),
}

impl Value {
    /// Whether the value is in the value space of the datatype.
    pub fn is_a(&self, k: Kind) -> bool {
        use Kind::*;
        match self {
            Value::Num(n) => n.is_a(k),
            Value::Str(s) => match k {
                PlainLiteral | String => true,
                NormalizedString => datatypes::normalized(s),
                Token => datatypes::token(s),
                Nmtoken => !s.is_empty() && s.chars().all(datatypes::is_name_char),
                Name => datatypes::name(s, true),
                NcName => datatypes::name(s, false),
                _ => false,
            },
            Value::Lang(..) => k == PlainLiteral,
            Value::Uri(_) => k == AnyUri,
            Value::Hex(_) => k == HexBinary,
            Value::Base64(_) => k == Base64Binary,
            Value::Time(_, offset) => k == DateTime || (k == DateTimeStamp && offset.is_some()),
        }
    }
}

/// The canonical value of a literal, or `None` when it has none the classifier can
/// compare: an ill-typed literal, an XML literal, one of a datatype outside the OWL 2 EL
/// map.
fn value_at(l: &Literal, guard: &Arc<Budget>) -> Result<Option<Value>, Error> {
    guard.poll()?;
    let dt = l.datatype();
    if l.language().is_none()
        && let Some(k @ (Kind::Integer | Kind::NonNegativeInteger | Kind::Decimal | Kind::Rational)) =
            Kind::of(dt)
    {
        return Ok(numeric::parse(l.value(), k, guard)?.map(Value::Num));
    }
    Ok(nonnumeric_value(l))
}

#[cfg(test)]
fn value(l: &Literal) -> Option<Value> {
    value_at(l, &Budget::new(&Limits::default(), 0, None)).unwrap()
}

fn nonnumeric_value(l: &Literal) -> Option<Value> {
    let lex = l.value();
    if let Some(lang) = l.language() {
        return Some(Value::Lang(lex.to_string(), lang.to_ascii_lowercase()));
    }
    let dt = l.datatype();
    if dt == vocab::RDF_PLAIN_LITERAL {
        let (text, lang) = lex.rsplit_once('@')?;
        if datatypes::lexical(dt, lex) != Some(true) {
            return None;
        }
        return Some(if lang.is_empty() {
            Value::Str(text.to_string())
        } else {
            Value::Lang(text.to_string(), lang.to_ascii_lowercase())
        });
    }
    let kind = Kind::of(dt)?;
    if datatypes::lexical(dt, lex) != Some(true) {
        return None;
    }
    use Kind::*;
    match kind {
        Integer | NonNegativeInteger | Decimal | Rational => unreachable!("guarded numeric route"),
        String | NormalizedString | Token | Nmtoken | Name | NcName => {
            Some(Value::Str(lex.to_string()))
        }
        AnyUri => Some(Value::Uri(lex.to_string())),
        HexBinary => Some(Value::Hex(lex.to_ascii_uppercase())),
        Base64Binary => Some(Value::Base64(lex.replace(' ', ""))),
        DateTime | DateTimeStamp => {
            let t: oxsdatatypes::DateTime = lex.parse().ok()?;
            let b = t.to_be_bytes();
            let mut instant = [0u8; 16];
            instant.copy_from_slice(&b[..16]);
            let offset = t
                .timezone_offset()
                .map(|o| i16::from_be_bytes(o.to_be_bytes()));
            Some(Value::Time(instant, offset))
        }
        Real | PlainLiteral | XmlLiteral => None,
    }
}

/// The part of a data range the lattice knows.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Known {
    /// `rdfs:Literal`, every data value
    Top,
    Empty,
    Kind(Kind),
    Value(Value),
}

impl Known {
    fn meet(&self, other: &Known) -> Known {
        use Known as K;
        match (self, other) {
            (K::Empty, _) | (_, K::Empty) => K::Empty,
            (K::Top, x) | (x, K::Top) => x.clone(),
            (K::Kind(a), K::Kind(b)) => {
                if a.within(*b) {
                    K::Kind(*a)
                } else if b.within(*a) {
                    K::Kind(*b)
                } else {
                    K::Empty
                }
            }
            (K::Kind(k), K::Value(v)) | (K::Value(v), K::Kind(k)) => {
                if v.is_a(*k) {
                    K::Value(v.clone())
                } else {
                    K::Empty
                }
            }
            (K::Value(a), K::Value(b)) => {
                if a == b {
                    K::Value(a.clone())
                } else {
                    K::Empty
                }
            }
        }
    }

    fn within(&self, other: &Known) -> bool {
        use Known as K;
        match (self, other) {
            (K::Empty, _) | (_, K::Top) => true,
            (K::Kind(a), K::Kind(b)) => a.within(*b),
            (K::Value(v), K::Kind(k)) => v.is_a(*k),
            (K::Value(a), K::Value(b)) => a == b,
            // a datatype has infinitely many values
            _ => false,
        }
    }
}

/// A data range as an element of the lattice: the part it knows, and the opaque atoms it
/// is an intersection with, sorted.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DataVal {
    pub known: Known,
    pub opaque: Vec<u32>,
}

impl DataVal {
    pub(super) fn owned_bytes(&self) -> usize {
        self.opaque.capacity() * std::mem::size_of::<u32>()
    }

    pub const TOP: DataVal = DataVal {
        known: Known::Top,
        opaque: Vec::new(),
    };

    pub fn is_empty(&self) -> bool {
        self.known == Known::Empty
    }

    pub fn meet(&self, other: &DataVal) -> DataVal {
        let known = self.known.meet(&other.known);
        if known == Known::Empty {
            return DataVal {
                known,
                opaque: Vec::new(),
            };
        }
        let mut opaque = self.opaque.clone();
        opaque.extend_from_slice(&other.opaque);
        opaque.sort_unstable();
        opaque.dedup();
        DataVal { known, opaque }
    }

    /// Whether every value of `self` is a value of `other`, as far as the lattice knows:
    /// an opaque atom of `other` must be one of `self`'s.
    pub fn within(&self, other: &DataVal) -> bool {
        self.is_empty()
            || (self.known.within(&other.known)
                && other
                    .opaque
                    .iter()
                    .all(|a| self.opaque.binary_search(a).is_ok()))
    }

    /// Whether the range is one value the lattice knows.
    pub fn single(&self) -> Option<&Value> {
        match &self.known {
            Known::Value(v) if self.opaque.is_empty() => Some(v),
            _ => None,
        }
    }
}

/// What an opaque atom stands for.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Opaque {
    Datatype(Datatype),
    Literal(Literal),
}

/// Maps the data ranges of an ontology into the lattice, with its datatype definitions.
#[derive(Default)]
pub struct Ranges {
    pub opaque: Vec<Opaque>,
    opaque_ids: FxHashMap<Opaque, u32>,
    pub definitions: FxHashMap<Datatype, DataRange>,
    pub(super) budget: Option<Arc<Budget>>,
    pub(super) ledger: Option<Arc<NumericLedger>>,
    halt: Option<Error>,
}

/// The deepest nesting of data ranges and datatype definitions followed.
const MAX_DEPTH: usize = 64;

impl Ranges {
    pub(super) fn begin(&mut self, budget: Arc<Budget>) {
        self.halt = None;
        self.budget = Some(budget);
    }
    pub(super) fn end(&mut self) {
        if let Some(b) = self.budget.take() {
            self.ledger = b.ledger();
        }
    }
    pub(super) fn check(&self) -> Result<(), Error> {
        self.halt.clone().map_or(Ok(()), Err)
    }
    pub(super) fn check_halt(&self) -> Result<(), super::index::Halt> {
        self.check().map_err(|e| match e {
            Error::Memory { used, .. } => super::index::Halt::Memory(used),
            _ => super::index::Halt::Stopped,
        })
    }
    pub(super) fn admit_checked(&mut self, bytes: Option<usize>) -> bool {
        match bytes {
            Some(bytes) => self.admit(bytes),
            None => {
                self.halt = Some(Error::Memory {
                    limit: usize::MAX,
                    used: usize::MAX,
                });
                false
            }
        }
    }
    /// Preallocate interner storage while its old buffers are still retained. The
    /// default helper interner has no build budget and needs no allocated guard.
    pub(super) fn reserve_storage(
        &mut self,
        bytes: Option<usize>,
    ) -> Result<Option<Reservation>, ()> {
        if self.halt.is_some() {
            return Err(());
        }
        let result = match bytes {
            Some(bytes) => self.budget.as_ref().map(|b| b.reserve(bytes)).transpose(),
            None => Err(Error::Memory {
                limit: usize::MAX,
                used: usize::MAX,
            }),
        };
        result.map_err(|e| {
            self.halt = Some(e);
        })
    }
    pub(super) fn retain_storage(
        &mut self,
        reservation: Option<Reservation>,
        bytes: Option<usize>,
        planned: usize,
    ) -> bool {
        let result = match reservation {
            Some(r) => r.retain(bytes),
            None if bytes.is_some_and(|n| n <= planned) => Ok(()),
            None => Err(Error::Memory {
                limit: usize::MAX,
                used: usize::MAX,
            }),
        };
        if let Err(e) = result {
            self.halt = Some(e);
            return false;
        }
        true
    }
    pub(super) fn admit_copies(&mut self, vals: &[DataVal]) -> bool {
        let bytes = vals
            .len()
            .checked_mul(std::mem::size_of::<DataVal>())
            .and_then(|base| {
                vals.iter().try_fold(base, |total, v| {
                    total.checked_add(v.opaque.len().checked_mul(4)?)
                })
            });
        self.admit_checked(bytes)
    }
    pub(super) fn admit(&mut self, bytes: usize) -> bool {
        if self.halt.is_some() {
            return false;
        }
        if let Some(b) = &self.budget
            && let Err(e) = b.grow_base(bytes)
        {
            self.halt = Some(e);
            return false;
        }
        true
    }

    fn atom(&mut self, o: Opaque) -> DataVal {
        let id = match self.opaque_ids.get(&o) {
            Some(&id) => id,
            None => {
                let id = self.opaque.len() as u32;
                self.opaque.push(o.clone());
                self.opaque_ids.insert(o, id);
                id
            }
        };
        DataVal {
            known: Known::Top,
            opaque: vec![id],
        }
    }

    /// The lattice element of a literal, and whether it is opaque.
    pub fn literal(&mut self, l: &Literal) -> (DataVal, bool) {
        if self.halt.is_some() {
            return (DataVal::TOP, false);
        }
        let default;
        let guard = match &self.budget {
            Some(b) => b,
            None => {
                default = Budget::new(&Limits::default(), 0, self.ledger.clone());
                &default
            }
        };
        let parsed = value_at(l, guard);
        self.ledger = guard.ledger();
        let parsed = match parsed {
            Ok(v) => v,
            Err(e) => {
                self.halt = Some(e);
                return (DataVal::TOP, false);
            }
        };
        match parsed {
            Some(v) => (
                DataVal {
                    known: Known::Value(v),
                    opaque: Vec::new(),
                },
                false,
            ),
            None => (self.atom(Opaque::Literal(l.clone())), true),
        }
    }

    /// The lattice element of a data range of OWL 2 EL, and whether it holds an opaque
    /// atom. A range outside the profile is an opaque atom too, which the profile check
    /// has already left out.
    pub fn range(&mut self, r: &DataRange) -> (DataVal, bool) {
        self.range_at(r, 0)
    }

    fn range_at(&mut self, r: &DataRange, depth: usize) -> (DataVal, bool) {
        match r {
            DataRange::Datatype(d) => self.datatype(d, depth),
            DataRange::IntersectionOf(rs) => {
                let mut acc = DataVal::TOP;
                let mut opaque = false;
                for r in rs {
                    let (v, o) = self.range_at(r, depth + 1);
                    if self.halt.is_some() {
                        return (DataVal::TOP, false);
                    }
                    acc = acc.meet(&v);
                    opaque |= o;
                }
                (acc, opaque)
            }
            DataRange::OneOf(ls) if ls.len() == 1 => match ls.iter().next() {
                Some(l) => self.literal(l),
                None => (DataVal::TOP, false),
            },
            other => {
                let d = Datatype::new(format!("urn:sparkles:range:{other:?}"));
                (self.atom(Opaque::Datatype(d)), true)
            }
        }
    }

    fn datatype(&mut self, d: &Datatype, depth: usize) -> (DataVal, bool) {
        if d.as_str() == vocab::RDFS_LITERAL {
            return (DataVal::TOP, false);
        }
        if let Some(k) = Kind::of(d.as_str()) {
            return (
                DataVal {
                    known: Known::Kind(k),
                    opaque: Vec::new(),
                },
                false,
            );
        }
        if depth < MAX_DEPTH
            && let Some(def) = self.definitions.get(d).cloned()
        {
            return self.range_at(&def, depth + 1);
        }
        (self.atom(Opaque::Datatype(d.clone())), true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn xsd(l: &str) -> String {
        format!("{}{l}", vocab::XSD)
    }
    fn lit(v: &str, dt: &str) -> Literal {
        Literal::typed(v, &xsd(dt))
    }
    fn val(l: &Literal) -> DataVal {
        Ranges::default().literal(l).0
    }
    fn kind(dt: &str) -> DataVal {
        Ranges::default()
            .range(&DataRange::Datatype(Datatype::new(xsd(dt))))
            .0
    }

    #[test]
    fn numbers_are_equal_across_lexical_forms_and_datatypes() {
        assert_eq!(val(&lit("01", "integer")), val(&lit("+1.0", "decimal")));
        assert_eq!(
            value(&Literal::typed("2/4", vocab::OWL_RATIONAL)),
            value(&lit("0.5", "decimal"))
        );
        assert_ne!(val(&lit("1", "integer")), val(&lit("2", "integer")));
        assert!(val(&lit("0.5", "decimal")).within(&kind("decimal")));
        assert!(!val(&lit("0.5", "decimal")).within(&kind("integer")));
        assert!(val(&lit("5", "integer")).within(&kind("nonNegativeInteger")));
        assert!(!val(&lit("-5", "integer")).within(&kind("nonNegativeInteger")));
        let third = val(&Literal::typed("1/3", vocab::OWL_RATIONAL));
        assert!(!third.within(&kind("decimal")));
        assert!(third.meet(&kind("decimal")).is_empty());
    }

    #[test]
    fn the_chains_meet_and_the_others_are_disjoint() {
        assert_eq!(kind("integer").meet(&kind("decimal")), kind("integer"));
        assert!(kind("integer").meet(&kind("string")).is_empty());
        assert!(kind("anyURI").meet(&kind("string")).is_empty());
        assert!(kind("hexBinary").meet(&kind("base64Binary")).is_empty());
        assert!(kind("NCName").within(&kind("NMTOKEN")));
        assert!(kind("token").within(&DataVal::TOP));
        assert!(!kind("string").within(&kind("token")));
        let plain = Ranges::default()
            .range(&DataRange::Datatype(Datatype::new(
                vocab::RDF_PLAIN_LITERAL,
            )))
            .0;
        assert!(kind("string").within(&plain));
    }

    #[test]
    fn strings_and_language_tags() {
        let s = val(&Literal::simple("a b"));
        assert!(s.within(&kind("token")));
        assert!(!s.within(&kind("NMTOKEN")));
        assert_eq!(s, val(&lit("a b", "token")));
        let en = val(&Literal::lang("x", "EN").unwrap());
        assert_eq!(en, val(&Literal::typed("x@en", vocab::RDF_PLAIN_LITERAL)));
        assert!(en.meet(&kind("string")).is_empty());
        assert_eq!(
            val(&Literal::typed("x@", vocab::RDF_PLAIN_LITERAL)),
            val(&Literal::simple("x"))
        );
    }

    #[test]
    fn date_times_with_different_offsets_are_different_values() {
        let a = val(&lit("2020-01-01T00:00:00Z", "dateTime"));
        let b = val(&lit("2020-01-01T01:00:00+01:00", "dateTime"));
        let c = val(&lit("2020-01-01T00:00:00.000Z", "dateTime"));
        let local = val(&lit("2020-01-01T00:00:00", "dateTime"));
        assert_ne!(a, b);
        assert_eq!(a, c);
        assert!(a.within(&kind("dateTimeStamp")));
        assert!(!local.within(&kind("dateTimeStamp")));
        assert!(local.within(&kind("dateTime")));
    }

    #[test]
    fn opaque_atoms_only_equal_themselves() {
        let mut r = Ranges::default();
        let (bad, o) = r.literal(&lit("x", "integer"));
        assert!(o);
        let (other, _) = r.literal(&lit("y", "integer"));
        // two opaque values may be one value, so their meet is not empty
        assert!(!bad.meet(&other).is_empty());
        assert!(bad.within(&bad));
        assert!(!bad.within(&other));
        let (dt, o) = r.range(&DataRange::Datatype(Datatype::new("http://ex.org/dt")));
        assert!(o);
        assert!(!kind("integer").within(&dt));
        assert!(kind("integer").meet(&dt).within(&dt));
    }

    #[test]
    fn datatype_definitions_name_their_ranges() {
        let mut r = Ranges::default();
        let d = Datatype::new("http://ex.org/count");
        r.definitions.insert(
            d.clone(),
            DataRange::Datatype(Datatype::new(xsd("nonNegativeInteger"))),
        );
        let (v, o) = r.range(&DataRange::Datatype(d));
        assert!(!o);
        assert_eq!(v, kind("nonNegativeInteger"));
    }
}

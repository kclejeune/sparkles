//! Node constraints on store ids: kind and datatype from the id's tag or term kind,
//! lexical validity through [`sparkles::xsd`], string facets in code points, patterns
//! with SPARQL `REGEX` semantics, numeric facets with XPath promotion, and value sets
//! as id sets, base-vocabulary id ranges for IRI stems, and language ranges. Results
//! for vocabulary ids are cached per constraint.

use crate::PrefixMap;
use crate::ShexFailure;
use crate::ast::{
    Exclusion, NodeConstraint, NodeKind, NumericLiteral, ObjectLiteral, ObjectValue, Stem,
    ValueSetValue,
};
use crate::ir::{NcId, NcIr};
use crate::report::compact_iri;
use oxrdf::vocab::{rdf, xsd};
use oxrdf::{Literal, NamedNode, Term};
use rustc_hash::{FxHashMap, FxHashSet};
use sparkles::id::{Id, Tag, unpack_decimal};
use sparkles::sparql::expr::{compile_regex, lang_matches};
use sparkles::sparql::value::{Value, compare};
use sparkles::store::{Snapshot, TermKind};
use std::cell::OnceCell;
use std::cmp::Ordering;
use std::sync::{Arc, Mutex};

/// Compile a node constraint's `pattern` with its flags (`s m i x q`), with the
/// semantics of SPARQL `REGEX`. The pattern is the ShExJ form (ShExC escapes undone).
pub fn compile_pattern(pattern: &str, flags: Option<&str>) -> Result<regex::Regex, String> {
    compile_regex(pattern, flags.unwrap_or(""))
        .map_err(|_| format!("invalid pattern /{pattern}/{}", flags.unwrap_or("")))
}

/// Undo the escapes of a ShExC regular expression (`/…/`): `\/` becomes `/` and
/// `\uXXXX`/`\UXXXXXXXX` the character; every other escape is the regular expression's
/// own and is kept. `None` for an invalid `\u` escape.
pub fn unescape_shexc_pattern(raw: &str) -> Option<String> {
    let mut out = String::with_capacity(raw.len());
    let mut it = raw.chars();
    while let Some(c) = it.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match it.next() {
            Some('/') => out.push('/'),
            Some(u @ ('u' | 'U')) => {
                let n = if u == 'u' { 4 } else { 8 };
                let hex: String = it.by_ref().take(n).collect();
                if hex.len() != n {
                    return None;
                }
                out.push(char::from_u32(u32::from_str_radix(&hex, 16).ok()?)?);
            }
            Some(e) => {
                out.push('\\');
                out.push(e);
            }
            None => out.push('\\'),
        }
    }
    Some(out)
}

/// A numeric facet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Bound {
    MinInclusive,
    MinExclusive,
    MaxInclusive,
    MaxExclusive,
}

impl Bound {
    fn holds(self, o: Ordering) -> bool {
        match self {
            Bound::MinInclusive => o != Ordering::Less,
            Bound::MinExclusive => o == Ordering::Greater,
            Bound::MaxInclusive => o != Ordering::Greater,
            Bound::MaxExclusive => o == Ordering::Less,
        }
    }

    fn keyword(self) -> &'static str {
        match self {
            Bound::MinInclusive => "MININCLUSIVE",
            Bound::MinExclusive => "MINEXCLUSIVE",
            Bound::MaxInclusive => "MAXINCLUSIVE",
            Bound::MaxExclusive => "MAXEXCLUSIVE",
        }
    }
}

/// A datatype constraint.
struct Datatype {
    iri: String,
    /// the tag of inline ids of this datatype (inline ids are valid by construction)
    inline: Option<Tag>,
}

/// An item of a value set that is not a plain IRI or literal.
enum Item {
    Language(String),
    IriStem(Stemmed),
    LiteralStem(String),
    LanguageStem(String),
    IriRange {
        /// `None`: the wildcard
        stem: Option<Stemmed>,
        exclusions: Vec<IriExcl>,
    },
    LiteralRange {
        stem: Option<String>,
        exclusions: Vec<Exclusion>,
    },
    LanguageRange {
        stem: Option<String>,
        exclusions: Vec<Exclusion>,
    },
}

/// An IRI stem with the base-vocabulary ids of the IRIs that start with it.
struct Stemmed {
    stem: String,
    range: (Id, Id),
}

impl Stemmed {
    fn new(snap: &Snapshot, stem: &str) -> Stemmed {
        Stemmed {
            stem: stem.to_string(),
            range: snap.iri_prefix_range(stem),
        }
    }

    fn matches(&self, n: &Node<'_>) -> bool {
        if let Some(id) = n.id
            && id.tag() == Tag::Vocab
        {
            return self.range.0 <= id && id < self.range.1;
        }
        matches!(n.term(), Some(Term::NamedNode(i)) if i.as_str().starts_with(&self.stem))
    }
}

enum IriExcl {
    Value(String, Option<Id>),
    Stem(Stemmed),
}

impl IriExcl {
    fn matches(&self, n: &Node<'_>) -> bool {
        match self {
            IriExcl::Value(iri, id) => match (id, n.id) {
                (Some(a), Some(b)) => *a == b,
                (None, Some(_)) => false,
                _ => matches!(n.term(), Some(Term::NamedNode(i)) if i.as_str() == iri),
            },
            IriExcl::Stem(s) => s.matches(n),
        }
    }
}

/// A value set resolved against a snapshot.
struct Values {
    /// the plain IRIs and literals that are in the store
    ids: FxHashSet<Id>,
    /// all plain IRIs and literals (for terms that are not in the store)
    terms: FxHashSet<Term>,
    items: Vec<Item>,
}

impl Values {
    fn new(snap: &Snapshot, vs: &[ValueSetValue]) -> Values {
        let mut v = Values {
            ids: FxHashSet::default(),
            terms: FxHashSet::default(),
            items: Vec::new(),
        };
        let range = |stem: &Stem| match stem {
            Stem::Value(s) => Some(s.clone()),
            Stem::Wildcard => None,
        };
        for x in vs {
            match x {
                ValueSetValue::Object(o) => {
                    let t = object_term(o);
                    if let Some(id) = snap.lookup_term(&t) {
                        v.ids.insert(id);
                    }
                    v.terms.insert(t);
                }
                ValueSetValue::Language(l) => v.items.push(Item::Language(l.to_ascii_lowercase())),
                ValueSetValue::IriStem(s) => v.items.push(Item::IriStem(Stemmed::new(snap, s))),
                ValueSetValue::LiteralStem(s) => v.items.push(Item::LiteralStem(s.clone())),
                ValueSetValue::LanguageStem(s) => v.items.push(Item::LanguageStem(s.clone())),
                ValueSetValue::IriStemRange { stem, exclusions } => v.items.push(Item::IriRange {
                    stem: range(stem).map(|s| Stemmed::new(snap, &s)),
                    exclusions: exclusions
                        .iter()
                        .map(|e| match e {
                            Exclusion::Value(i) => IriExcl::Value(i.clone(), snap.lookup_iri(i)),
                            Exclusion::Stem(s) => IriExcl::Stem(Stemmed::new(snap, s)),
                        })
                        .collect(),
                }),
                ValueSetValue::LiteralStemRange { stem, exclusions } => {
                    v.items.push(Item::LiteralRange {
                        stem: range(stem),
                        exclusions: exclusions.clone(),
                    })
                }
                ValueSetValue::LanguageStemRange { stem, exclusions } => {
                    v.items.push(Item::LanguageRange {
                        stem: range(stem),
                        exclusions: exclusions.clone(),
                    })
                }
            }
        }
        v
    }

    fn matches(&self, n: &Node<'_>) -> bool {
        match n.id {
            Some(id) => {
                if self.ids.contains(&id) {
                    return true;
                }
            }
            None => {
                if n.term().is_some_and(|t| self.terms.contains(t)) {
                    return true;
                }
            }
        }
        self.items.iter().any(|i| item_matches(i, n))
    }
}

fn item_matches(item: &Item, n: &Node<'_>) -> bool {
    match item {
        Item::Language(l) => n.lang().is_some_and(|t| t.eq_ignore_ascii_case(l)),
        Item::IriStem(s) => s.matches(n),
        Item::LiteralStem(s) => n.lexical().is_some_and(|x| x.starts_with(s.as_str())),
        Item::LanguageStem(s) => n.lang().is_some_and(|t| lang_stem(t, s)),
        Item::IriRange { stem, exclusions } => {
            stem.as_ref().is_none_or(|s| s.matches(n)) && !exclusions.iter().any(|e| e.matches(n))
        }
        Item::LiteralRange { stem, exclusions } => {
            stem.as_ref()
                .is_none_or(|s| n.lexical().is_some_and(|x| x.starts_with(s.as_str())))
                && !exclusions.iter().any(|e| match (e, n.lexical()) {
                    (Exclusion::Value(v), Some(x)) => x == v,
                    (Exclusion::Stem(s), Some(x)) => x.starts_with(s.as_str()),
                    (_, None) => false,
                })
        }
        Item::LanguageRange { stem, exclusions } => {
            stem.as_ref()
                .is_none_or(|s| n.lang().is_some_and(|t| lang_stem(t, s)))
                && !exclusions.iter().any(|e| match (e, n.lang()) {
                    (Exclusion::Value(v), Some(t)) => t.eq_ignore_ascii_case(v),
                    (Exclusion::Stem(s), Some(t)) => lang_stem(t, s),
                    (_, None) => false,
                })
        }
    }
}

/// RFC 4647 basic filtering of a language tag by a stem; the empty stem matches every
/// tag.
fn lang_stem(tag: &str, stem: &str) -> bool {
    lang_matches(tag, if stem.is_empty() { "*" } else { stem })
}

/// The RDF term of a value-set IRI or literal.
pub fn object_term(o: &ObjectValue) -> Term {
    match o {
        ObjectValue::Iri(i) => NamedNode::new_unchecked(i.as_str()).into(),
        ObjectValue::Literal(l) => literal_term(l).into(),
    }
}

fn literal_term(l: &ObjectLiteral) -> Literal {
    match (&l.language, &l.datatype) {
        (Some(lang), _) => {
            Literal::new_language_tagged_literal(&l.value, lang).unwrap_or_else(|_| {
                Literal::new_language_tagged_literal_unchecked(&l.value, lang.to_ascii_lowercase())
            })
        }
        (None, Some(dt)) => Literal::new_typed_literal(&l.value, NamedNode::new_unchecked(dt)),
        (None, None) => Literal::new_simple_literal(&l.value),
    }
}

/// A node being checked: its store id (if it has one) and its term, decoded once when a
/// part of the constraint needs it.
struct Node<'a> {
    snap: &'a Snapshot,
    id: Option<Id>,
    term: OnceCell<Option<Term>>,
}

impl<'a> Node<'a> {
    fn stored(snap: &'a Snapshot, id: Id) -> Node<'a> {
        Node {
            snap,
            id: Some(id),
            term: OnceCell::new(),
        }
    }

    fn unstored(snap: &'a Snapshot, term: &Term) -> Node<'a> {
        Node {
            snap,
            id: None,
            term: OnceCell::from(Some(term.clone())),
        }
    }

    fn term(&self) -> Option<&Term> {
        self.term
            .get_or_init(|| self.id.and_then(|id| self.snap.term(id)))
            .as_ref()
    }

    fn kind(&self) -> TermKind {
        match self.id {
            Some(id) => self.snap.term_kind(id),
            None => match self.term() {
                Some(Term::NamedNode(_)) => TermKind::Iri,
                Some(Term::BlankNode(_)) => TermKind::BNode,
                Some(Term::Literal(_)) => TermKind::Literal,
                Some(Term::Triple(_)) => TermKind::Triple,
                None => TermKind::Other,
            },
        }
    }

    fn literal(&self) -> Option<&Literal> {
        match self.term() {
            Some(Term::Literal(l)) => Some(l),
            _ => None,
        }
    }

    fn lexical(&self) -> Option<&str> {
        self.literal().map(|l| l.value())
    }

    fn lang(&self) -> Option<&str> {
        self.literal().and_then(|l| l.language())
    }

    /// The string the string facets read: an IRI, a literal's lexical form, a blank
    /// node's label.
    fn string(&self) -> Option<&str> {
        match self.term()? {
            Term::NamedNode(n) => Some(n.as_str()),
            Term::BlankNode(b) => Some(b.as_str()),
            Term::Literal(l) => Some(l.value()),
            Term::Triple(_) => None,
        }
    }

    /// The numeric value of a numeric literal with a valid lexical form.
    fn number(&self) -> Option<Value> {
        if let Some(id) = self.id {
            match id.tag() {
                Tag::Int => return Some(Value::Integer(id.as_i64().into())),
                Tag::Double => return Some(Value::Double(id.as_f64().into())),
                Tag::Decimal => return Some(Value::Decimal(unpack_decimal(id.payload()))),
                Tag::Bool | Tag::DateTime | Tag::Date | Tag::BNode => return None,
                _ => {}
            }
        }
        let l = self.literal()?;
        let v = Value::from_literal(l);
        (v.is_numeric() && sparkles::xsd::is_valid(l)).then_some(v)
    }
}

/// The inline tag of a datatype's canonical literals.
fn inline_tag(dt: &str) -> Option<Tag> {
    Some(match dt {
        d if d == xsd::INTEGER.as_str() => Tag::Int,
        d if d == xsd::DOUBLE.as_str() => Tag::Double,
        d if d == xsd::DECIMAL.as_str() => Tag::Decimal,
        d if d == xsd::BOOLEAN.as_str() => Tag::Bool,
        d if d == xsd::DATE_TIME.as_str() => Tag::DateTime,
        d if d == xsd::DATE.as_str() => Tag::Date,
        _ => return None,
    })
}

fn is_inline(tag: Tag) -> bool {
    matches!(
        tag,
        Tag::Int | Tag::Double | Tag::Decimal | Tag::Bool | Tag::DateTime | Tag::Date
    )
}

/// The digits of a decimal: (total digits, fraction digits) of its canonical form,
/// without leading zeros of the integer part and trailing zeros of the fraction.
fn digits(v: &Value) -> Option<(u64, u64)> {
    let s = match v {
        Value::Integer(i) => i.to_string(),
        Value::Decimal(d) => d.to_string(),
        _ => return None,
    };
    let s = s.trim_start_matches(['+', '-']);
    let (int, frac) = s.split_once('.').unwrap_or((s, ""));
    let int = int.trim_start_matches('0');
    let frac = frac.trim_end_matches('0');
    Some(((int.len() + frac.len()) as u64, frac.len() as u64))
}

fn numeric_value(n: &NumericLiteral) -> Value {
    match n {
        NumericLiteral::Integer(s) => Value::from_typed(s, xsd::INTEGER.as_str()),
        NumericLiteral::Decimal(s) => Value::from_typed(s, xsd::DECIMAL.as_str()),
        NumericLiteral::Double(s) => Value::from_typed(s, xsd::DOUBLE.as_str()),
    }
}

fn numeric_lexical(n: &NumericLiteral) -> &str {
    match n {
        NumericLiteral::Integer(s) | NumericLiteral::Decimal(s) | NumericLiteral::Double(s) => s,
    }
}

/// The part of a node constraint a node fails.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Part {
    Kind,
    Datatype,
    Length(usize),
    Pattern,
    Numeric(usize),
    TotalDigits,
    FractionDigits,
    Values,
}

/// One node constraint resolved against a snapshot.
struct Plan {
    kind: Option<NodeKind>,
    datatype: Option<Datatype>,
    /// `LENGTH`, `MINLENGTH`, `MAXLENGTH` (in this order)
    lengths: [Option<u64>; 3],
    /// the pattern, or `Err` if it does not compile (it then never matches)
    pattern: Option<Result<regex::Regex, ()>>,
    numeric: Vec<(Bound, Value)>,
    total_digits: Option<u64>,
    fraction_digits: Option<u64>,
    values: Option<Values>,
    /// some part needs the term of a vocabulary id: cache the results
    cached: bool,
}

impl Plan {
    fn new(snap: &Snapshot, ir: &NcIr) -> Plan {
        let nc = &ir.nc;
        let pattern = nc.pattern.as_ref().map(|p| match &ir.regex {
            Some(re) => Ok(re.clone()),
            None => compile_pattern(p, nc.flags.as_deref()).map_err(|_| ()),
        });
        let numeric = [
            (Bound::MinInclusive, &nc.min_inclusive),
            (Bound::MinExclusive, &nc.min_exclusive),
            (Bound::MaxInclusive, &nc.max_inclusive),
            (Bound::MaxExclusive, &nc.max_exclusive),
        ]
        .into_iter()
        .filter_map(|(b, v)| v.as_ref().map(|v| (b, numeric_value(v))))
        .collect::<Vec<_>>();
        let values = nc.values.as_ref().map(|v| Values::new(snap, v));
        let cached = nc.datatype.is_some()
            || nc.length.is_some()
            || nc.min_length.is_some()
            || nc.max_length.is_some()
            || pattern.is_some()
            || !numeric.is_empty()
            || nc.total_digits.is_some()
            || nc.fraction_digits.is_some()
            || values
                .as_ref()
                .is_some_and(|v| v.items.iter().any(|i| !matches!(i, Item::IriStem(_))));
        Plan {
            kind: nc.node_kind,
            datatype: nc.datatype.as_ref().map(|d| Datatype {
                iri: d.clone(),
                inline: inline_tag(d),
            }),
            lengths: [nc.length, nc.min_length, nc.max_length],
            pattern,
            numeric,
            total_digits: nc.total_digits,
            fraction_digits: nc.fraction_digits,
            values,
            cached,
        }
    }

    /// The first part `n` fails, in the order cheapest first.
    fn first_failure(&self, n: &Node<'_>) -> Option<Part> {
        if let Some(k) = self.kind {
            let ok = matches!(
                (k, n.kind()),
                (NodeKind::Iri, TermKind::Iri)
                    | (NodeKind::BNode, TermKind::BNode)
                    | (NodeKind::NonLiteral, TermKind::Iri | TermKind::BNode)
                    | (NodeKind::Literal, TermKind::Literal)
            );
            if !ok {
                return Some(Part::Kind);
            }
        }
        if let Some(v) = &self.values
            && !v.matches(n)
        {
            return Some(Part::Values);
        }
        if let Some(dt) = &self.datatype
            && !datatype_ok(dt, n)
        {
            return Some(Part::Datatype);
        }
        if self.lengths.iter().any(Option::is_some) {
            let Some(s) = n.string() else {
                let i = self.lengths.iter().position(Option::is_some).unwrap_or(0);
                return Some(Part::Length(i));
            };
            let len = s.chars().count() as u64;
            let [eq, min, max] = self.lengths;
            if eq.is_some_and(|l| len != l) {
                return Some(Part::Length(0));
            }
            if min.is_some_and(|l| len < l) {
                return Some(Part::Length(1));
            }
            if max.is_some_and(|l| len > l) {
                return Some(Part::Length(2));
            }
        }
        if let Some(p) = &self.pattern {
            let ok = match (p, n.string()) {
                (Ok(re), Some(s)) => re.is_match(s),
                _ => false,
            };
            if !ok {
                return Some(Part::Pattern);
            }
        }
        if !self.numeric.is_empty() || self.total_digits.is_some() || self.fraction_digits.is_some()
        {
            let v = n.number();
            for (i, (b, bound)) in self.numeric.iter().enumerate() {
                let ok = v
                    .as_ref()
                    .and_then(|v| compare(v, bound).ok().flatten())
                    .is_some_and(|o| b.holds(o));
                if !ok {
                    return Some(Part::Numeric(i));
                }
            }
            if self.total_digits.is_some() || self.fraction_digits.is_some() {
                let d = v.as_ref().and_then(digits);
                if let Some(t) = self.total_digits
                    && !d.is_some_and(|(total, _)| total <= t)
                {
                    return Some(Part::TotalDigits);
                }
                if let Some(f) = self.fraction_digits
                    && !d.is_some_and(|(_, frac)| frac <= f)
                {
                    return Some(Part::FractionDigits);
                }
            }
        }
        None
    }
}

fn datatype_ok(dt: &Datatype, n: &Node<'_>) -> bool {
    if let Some(id) = n.id
        && is_inline(id.tag())
    {
        return dt.inline == Some(id.tag());
    }
    match n.literal() {
        Some(l) if l.datatype().as_str() == dt.iri => {
            dt.iri == rdf::LANG_STRING.as_str() || sparkles::xsd::is_valid(l)
        }
        _ => false,
    }
}

const SHARDS: usize = 16;
/// Entries per shard before the shard is cleared.
const SHARD_CAP: usize = 1 << 14;

/// Results of one constraint on vocabulary ids, sharded by id.
struct Cache {
    shards: [Mutex<FxHashMap<Id, bool>>; SHARDS],
}

impl Cache {
    fn new() -> Cache {
        Cache {
            shards: std::array::from_fn(|_| Mutex::new(FxHashMap::default())),
        }
    }

    fn shard(&self, id: Id) -> &Mutex<FxHashMap<Id, bool>> {
        let h = id.0.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 60;
        &self.shards[h as usize % SHARDS]
    }

    fn get(&self, id: Id) -> Option<bool> {
        self.shard(id).lock().ok()?.get(&id).copied()
    }

    fn put(&self, id: Id, v: bool) {
        if let Ok(mut m) = self.shard(id).lock() {
            if m.len() >= SHARD_CAP {
                m.clear();
            }
            m.insert(id, v);
        }
    }
}

/// The node constraints of a compiled schema, resolved against one snapshot.
pub struct NcPlan {
    pub snap: Arc<Snapshot>,
    ncs: Vec<NodeConstraint>,
    plans: Vec<Plan>,
    caches: Vec<Option<Cache>>,
    prefixes: PrefixMap,
}

impl NcPlan {
    pub fn new(snap: &Arc<Snapshot>, ncs: &[NcIr]) -> NcPlan {
        let plans: Vec<Plan> = ncs.iter().map(|nc| Plan::new(snap, nc)).collect();
        NcPlan {
            snap: snap.clone(),
            ncs: ncs.iter().map(|nc| nc.nc.clone()).collect(),
            caches: plans.iter().map(|p| p.cached.then(Cache::new)).collect(),
            plans,
            prefixes: PrefixMap::new(),
        }
    }

    /// Write IRIs in [`explain`](Self::explain)'s constraints with these prefixes.
    pub fn with_prefixes(mut self, prefixes: &PrefixMap) -> NcPlan {
        self.prefixes = prefixes.clone();
        self
    }

    /// Does the stored node `node` satisfy constraint `nc`?
    pub fn check(&self, nc: NcId, node: Id) -> anyhow::Result<bool> {
        let plan = &self.plans[nc.index()];
        let cache = match &self.caches[nc.index()] {
            Some(c) if matches!(node.tag(), Tag::Vocab | Tag::Delta) => Some(c),
            _ => None,
        };
        if let Some(v) = cache.and_then(|c| c.get(node)) {
            return Ok(v);
        }
        let ok = plan
            .first_failure(&Node::stored(&self.snap, node))
            .is_none();
        if let Some(c) = cache {
            c.put(node, ok);
        }
        Ok(ok)
    }

    /// Does `term`, which is not in the store, satisfy constraint `nc`?
    pub fn check_term(&self, nc: NcId, term: &Term) -> anyhow::Result<bool> {
        if let Some(id) = self.snap.lookup_term(term) {
            return self.check(nc, id);
        }
        Ok(self.plans[nc.index()]
            .first_failure(&Node::unstored(&self.snap, term))
            .is_none())
    }

    /// Why `node` fails `nc` (`None` if it does not).
    pub fn explain(&self, nc: NcId, node: Id) -> anyhow::Result<Option<ShexFailure>> {
        let n = Node::stored(&self.snap, node);
        Ok(self.failure(nc, &n))
    }

    /// Why `term`, which need not be in the store, fails `nc` (`None` if it does not).
    pub fn explain_term(&self, nc: NcId, term: &Term) -> anyhow::Result<Option<ShexFailure>> {
        if let Some(id) = self.snap.lookup_term(term) {
            return self.explain(nc, id);
        }
        Ok(self.failure(nc, &Node::unstored(&self.snap, term)))
    }

    fn failure(&self, nc: NcId, n: &Node<'_>) -> Option<ShexFailure> {
        let part = self.plans[nc.index()].first_failure(n)?;
        let c = &self.ncs[nc.index()];
        let value = n
            .term()
            .cloned()
            .unwrap_or_else(|| NamedNode::new_unchecked("urn:x-sparkles:unknown").into());
        let iri = |i: &str| compact_iri(i, &self.prefixes);
        let unbounded = || NumericLiteral::Integer(String::new());
        Some(match part {
            Part::Kind => ShexFailure::NodeKind {
                value,
                constraint: c.node_kind.map_or("", |k| k.as_str()).to_ascii_uppercase(),
            },
            Part::Datatype => ShexFailure::Datatype {
                value,
                constraint: iri(c.datatype.as_deref().unwrap_or_default()),
            },
            Part::Values => ShexFailure::ValueSet {
                value,
                constraint: write_values(c.values.as_deref().unwrap_or_default(), &self.prefixes),
            },
            Part::Length(i) => {
                let (kw, v) = [
                    ("LENGTH", c.length),
                    ("MINLENGTH", c.min_length),
                    ("MAXLENGTH", c.max_length),
                ][i];
                ShexFailure::Facet {
                    value,
                    constraint: format!("{kw} {}", v.unwrap_or_default()),
                }
            }
            Part::Pattern => ShexFailure::Facet {
                value,
                constraint: format!(
                    "PATTERN /{}/{}",
                    c.pattern.as_deref().unwrap_or_default().replace('/', "\\/"),
                    c.flags.as_deref().unwrap_or_default()
                ),
            },
            Part::Numeric(i) => {
                let b = self.plans[nc.index()].numeric[i].0;
                let lit = match b {
                    Bound::MinInclusive => &c.min_inclusive,
                    Bound::MinExclusive => &c.min_exclusive,
                    Bound::MaxInclusive => &c.max_inclusive,
                    Bound::MaxExclusive => &c.max_exclusive,
                };
                let lit = lit.clone().unwrap_or_else(unbounded);
                ShexFailure::Facet {
                    value,
                    constraint: format!("{} {}", b.keyword(), numeric_lexical(&lit)),
                }
            }
            Part::TotalDigits => ShexFailure::Facet {
                value,
                constraint: format!("TOTALDIGITS {}", c.total_digits.unwrap_or_default()),
            },
            Part::FractionDigits => ShexFailure::Facet {
                value,
                constraint: format!("FRACTIONDIGITS {}", c.fraction_digits.unwrap_or_default()),
            },
        })
    }
}

/// A value set in ShExC, the first few values only.
fn write_values(vs: &[ValueSetValue], prefixes: &PrefixMap) -> String {
    const SHOWN: usize = 5;
    let iri = |i: &str| compact_iri(i, prefixes);
    let lit = |s: &str| format!("{s:?}");
    let stem = |s: &Stem, f: &dyn Fn(&str) -> String| match s {
        Stem::Value(v) => format!("{}~", f(v)),
        Stem::Wildcard => ".".to_string(),
    };
    let excls = |ex: &[Exclusion], f: &dyn Fn(&str) -> String| {
        ex.iter()
            .map(|e| match e {
                Exclusion::Value(v) => format!(" - {}", f(v)),
                Exclusion::Stem(v) => format!(" - {}~", f(v)),
            })
            .collect::<String>()
    };
    let at = |l: &str| format!("@{l}");
    let mut parts: Vec<String> = vs
        .iter()
        .take(SHOWN)
        .map(|v| match v {
            ValueSetValue::Object(ObjectValue::Iri(i)) => iri(i),
            ValueSetValue::Object(ObjectValue::Literal(l)) => match (&l.language, &l.datatype) {
                (Some(lang), _) => format!("{}@{lang}", lit(&l.value)),
                (None, Some(dt)) => format!("{}^^{}", lit(&l.value), iri(dt)),
                (None, None) => lit(&l.value),
            },
            ValueSetValue::IriStem(s) => format!("{}~", iri(s)),
            ValueSetValue::LiteralStem(s) => format!("{}~", lit(s)),
            ValueSetValue::Language(l) => at(l),
            ValueSetValue::LanguageStem(l) => format!("@{l}~"),
            ValueSetValue::IriStemRange {
                stem: s,
                exclusions,
            } => {
                format!("{}{}", stem(s, &iri), excls(exclusions, &iri))
            }
            ValueSetValue::LiteralStemRange {
                stem: s,
                exclusions,
            } => {
                format!("{}{}", stem(s, &lit), excls(exclusions, &lit))
            }
            ValueSetValue::LanguageStemRange {
                stem: s,
                exclusions,
            } => {
                format!("{}{}", stem(s, &at), excls(exclusions, &at))
            }
        })
        .collect();
    if vs.len() > SHOWN {
        parts.push("…".to_string());
    }
    format!("[{}]", parts.join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::NumericLiteral as N;
    use sparkles::io::{RdfFormat, Source};
    use sparkles::store::{Store, StoreOptions};

    const XSDNS: &str = "http://www.w3.org/2001/XMLSchema#";

    /// A store whose base vocabulary holds the data below, and whose delta holds
    /// `ex:sub/3`, `"delta"@en-GB` and `"01"^^xsd:integer` (added by a later write).
    fn store() -> Store {
        let s = Store::in_memory(StoreOptions::default());
        let ttl = r#"
            @prefix ex: <http://ex.org/> .
            @prefix xsd: <http://www.w3.org/2001/XMLSchema#> .
            ex:a ex:p "ab", "abc", "abcd", "𝒳𝒴", "x"@en, "y"@en-US, "z"@fr, 1, 5, 150, 151,
                1.5e2, "NaN"^^xsd:double, "1.0"^^xsd:decimal, "5"^^xsd:byte, "128"^^xsd:byte,
                "abc"^^xsd:integer, 1.2345, 1.23456, "01.23450"^^xsd:decimal, 123450, 1234560,
                "1.2345"^^xsd:float, "12345"^^xsd:integer, _:b1, <<( ex:a ex:p ex:b )>> .
            ex:b ex:p <http://ex.org/sub/1>, <http://ex.org/sub/2>, <http://other.org/z> .
        "#;
        s.load(&[Source::from_bytes(
            ttl.as_bytes().to_vec(),
            RdfFormat::Turtle,
            None,
        )])
        .unwrap();
        let mut txn = s.write();
        let p = txn.intern(&iri("http://ex.org/p")).unwrap();
        let b = txn.intern(&iri("http://ex.org/b")).unwrap();
        for t in [
            iri("http://ex.org/sub/3"),
            lang("delta", "en-GB"),
            typed("01", "integer"),
        ] {
            let o = txn.intern(&t).unwrap();
            txn.insert([b, p, o, Id::DEFAULT_GRAPH]).unwrap();
        }
        txn.commit().unwrap();
        s
    }

    fn iri(s: &str) -> Term {
        NamedNode::new_unchecked(s).into()
    }

    fn typed(lex: &str, dt: &str) -> Term {
        Literal::new_typed_literal(lex, NamedNode::new_unchecked(format!("{XSDNS}{dt}"))).into()
    }

    fn plain(s: &str) -> Term {
        Literal::new_simple_literal(s).into()
    }

    fn lang(s: &str, l: &str) -> Term {
        Literal::new_language_tagged_literal(s, l).unwrap().into()
    }

    fn plan(snap: &Arc<Snapshot>, nc: NodeConstraint) -> NcPlan {
        NcPlan::new(snap, &[NcIr { nc, regex: None }])
    }

    /// Check `term` (which the store must hold) against `nc`, twice (the second time
    /// from the cache), and check that `explain` agrees.
    fn holds(snap: &Arc<Snapshot>, nc: NodeConstraint, term: &Term) -> bool {
        let plan = plan(snap, nc);
        let id = snap
            .lookup_term(term)
            .unwrap_or_else(|| panic!("{term} is not in the store"));
        let a = plan.check(NcId(0), id).unwrap();
        assert_eq!(plan.check(NcId(0), id).unwrap(), a);
        assert_eq!(plan.explain(NcId(0), id).unwrap().is_none(), a, "{term}");
        a
    }

    fn values(vs: Vec<ValueSetValue>) -> NodeConstraint {
        NodeConstraint {
            values: Some(vs),
            ..Default::default()
        }
    }

    fn objects(snap: &Snapshot, s: &str) -> Vec<Id> {
        let s = snap.lookup_iri(s).unwrap();
        let p = snap.lookup_iri("http://ex.org/p").unwrap();
        let mut found = Vec::new();
        snap.scan(sparkles::index::Perm::Spo, &[s.0, p.0], |c| {
            match c {
                sparkles::store::Chunk::Row(k) => found.push(Id(k[2])),
                sparkles::store::Chunk::Block(b, s, e) => {
                    found.extend((s..e).map(|i| Id(b.key(i)[2])))
                }
            }
            Ok(true)
        })
        .unwrap();
        found
    }

    #[test]
    fn node_kinds() {
        let store = store();
        let snap = store.snapshot();
        let kind = |k| NcIr {
            nc: NodeConstraint {
                node_kind: Some(k),
                ..Default::default()
            },
            regex: None,
        };
        let objs = objects(&snap, "http://ex.org/a");
        let b = *objs.iter().find(|o| o.tag() == Tag::BNode).unwrap();
        let triple = *objs
            .iter()
            .find(|&&o| snap.term_kind(o) == TermKind::Triple)
            .unwrap();
        let plan = NcPlan::new(
            &snap,
            &[
                kind(NodeKind::Iri),
                kind(NodeKind::BNode),
                kind(NodeKind::NonLiteral),
                kind(NodeKind::Literal),
            ],
        );
        let row = |id: Id| {
            (0..4)
                .map(|i| plan.check(NcId(i), id).unwrap())
                .collect::<Vec<_>>()
        };
        let a = snap.lookup_iri("http://ex.org/a").unwrap();
        let sub3 = snap.lookup_iri("http://ex.org/sub/3").unwrap();
        assert_eq!(sub3.tag(), Tag::Delta);
        assert_eq!(row(a), [true, false, true, false]);
        assert_eq!(row(sub3), [true, false, true, false]);
        assert_eq!(row(b), [false, true, true, false]);
        assert_eq!(row(Id::from_i64(1).unwrap()), [false, false, false, true]);
        let ab = snap.lookup_term(&plain("ab")).unwrap();
        assert_eq!(row(ab), [false, false, false, true]);
        // a triple term is none of the kinds
        assert_eq!(row(triple), [false; 4]);
        match plan.explain(NcId(0), Id::from_i64(1).unwrap()).unwrap() {
            Some(ShexFailure::NodeKind { constraint, .. }) => assert_eq!(constraint, "IRI"),
            other => panic!("{other:?}"),
        }
        // terms that are not in the store
        let lit = plain("not stored");
        assert!(plan.check_term(NcId(3), &lit).unwrap());
        assert!(!plan.check_term(NcId(0), &lit).unwrap());
        assert!(plan.check_term(NcId(0), &iri("http://ex.org/new")).unwrap());
    }

    #[test]
    fn datatypes_need_valid_lexical_forms() {
        let store = store();
        let snap = store.snapshot();
        let dt = |d: &str| NodeConstraint {
            datatype: Some(d.to_string()),
            ..Default::default()
        };
        let int = || dt(&format!("{XSDNS}integer"));
        assert!(holds(&snap, int(), &typed("1", "integer")));
        assert!(holds(&snap, int(), &typed("01", "integer")));
        assert!(!holds(&snap, int(), &typed("abc", "integer")));
        assert!(!holds(&snap, int(), &typed("5", "byte")));
        assert!(!holds(&snap, int(), &typed("1.0", "decimal")));
        let byte = || dt(&format!("{XSDNS}byte"));
        assert!(holds(&snap, byte(), &typed("5", "byte")));
        assert!(!holds(&snap, byte(), &typed("128", "byte")));
        assert!(holds(
            &snap,
            dt(&format!("{XSDNS}double")),
            &typed("NaN", "double")
        ));
        assert!(holds(&snap, dt(&format!("{XSDNS}string")), &plain("ab")));
        assert!(!holds(
            &snap,
            dt(&format!("{XSDNS}string")),
            &lang("x", "en")
        ));
        let ls = || dt(rdf::LANG_STRING.as_str());
        assert!(holds(&snap, ls(), &lang("x", "en")));
        assert!(holds(&snap, ls(), &lang("delta", "en-GB")));
        assert!(!holds(&snap, ls(), &plain("ab")));
        assert!(!holds(&snap, int(), &iri("http://ex.org/a")));
        let p = plan(&snap, int());
        assert!(!p.check_term(NcId(0), &typed("12x", "integer")).unwrap());
        assert!(p.check_term(NcId(0), &typed("+0012", "integer")).unwrap());
    }

    #[test]
    fn string_facets_count_code_points() {
        let store = store();
        let snap = store.snapshot();
        let len = |l: [Option<u64>; 3]| NodeConstraint {
            length: l[0],
            min_length: l[1],
            max_length: l[2],
            ..Default::default()
        };
        // the length is at least MINLENGTH and at most MAXLENGTH
        assert!(!holds(&snap, len([None, Some(3), None]), &plain("ab")));
        assert!(holds(&snap, len([None, Some(3), None]), &plain("abc")));
        assert!(holds(&snap, len([None, Some(3), None]), &plain("abcd")));
        assert!(holds(&snap, len([None, None, Some(3)]), &plain("ab")));
        assert!(holds(&snap, len([None, None, Some(3)]), &plain("abc")));
        assert!(!holds(&snap, len([None, None, Some(3)]), &plain("abcd")));
        assert!(holds(&snap, len([Some(3), None, None]), &plain("abc")));
        assert!(!holds(&snap, len([Some(3), None, None]), &plain("abcd")));
        // two characters outside the BMP: 2 code points (4 UTF-16 units, 8 bytes)
        assert!(holds(&snap, len([Some(2), None, None]), &plain("𝒳𝒴")));
        assert!(!holds(&snap, len([None, Some(3), None]), &plain("𝒳𝒴")));
        // IRIs by their string, literals of any datatype by their lexical form
        assert!(holds(
            &snap,
            len([Some(15), None, None]),
            &iri("http://ex.org/a")
        ));
        assert!(holds(
            &snap,
            len([Some(19), None, None]),
            &iri("http://ex.org/sub/3")
        ));
        assert!(holds(
            &snap,
            len([Some(3), None, None]),
            &typed("150", "integer")
        ));
        match plan(&snap, len([None, Some(3), None]))
            .explain_term(NcId(0), &plain("ab"))
            .unwrap()
        {
            Some(ShexFailure::Facet { constraint, value }) => {
                assert_eq!((constraint.as_str(), value), ("MINLENGTH 3", plain("ab")))
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn patterns() {
        let store = store();
        let snap = store.snapshot();
        let pat = |p: &str, f: Option<&str>| NodeConstraint {
            pattern: Some(p.to_string()),
            flags: f.map(str::to_string),
            ..Default::default()
        };
        assert!(holds(&snap, pat("^ab", None), &plain("abcd")));
        assert!(!holds(&snap, pat("^AB", None), &plain("abcd")));
        assert!(holds(&snap, pat("^AB", Some("i")), &plain("abcd")));
        assert!(holds(&snap, pat("^𝒳.$", None), &plain("𝒳𝒴")));
        assert!(holds(
            &snap,
            pat("/sub/", None),
            &iri("http://ex.org/sub/3")
        ));
        assert!(!holds(&snap, pat("a.c", Some("q")), &plain("abc")));
        // an invalid pattern matches nothing
        assert!(!holds(&snap, pat("(", None), &plain("ab")));
        // the compiled pattern of the IR is used when there is one
        let ir = NcIr {
            nc: pat("never used", None),
            regex: Some(compile_pattern("^a", None).unwrap()),
        };
        let p = NcPlan::new(&snap, &[ir]);
        assert!(p.check_term(NcId(0), &plain("ab")).unwrap());
        assert!(compile_pattern("(", None).is_err());
        assert_eq!(
            unescape_shexc_pattern(r"a\/bA\.\\/").as_deref(),
            Some(r"a/bA\.\\/")
        );
        assert_eq!(unescape_shexc_pattern(r"\u00"), None);
    }

    #[test]
    fn numeric_facets() {
        let store = store();
        let snap = store.snapshot();
        let max_incl = |n: N| NodeConstraint {
            max_inclusive: Some(n),
            ..Default::default()
        };
        let i = |s: &str| N::Integer(s.to_string());
        assert!(holds(&snap, max_incl(i("150")), &typed("150", "integer")));
        assert!(!holds(&snap, max_incl(i("150")), &typed("151", "integer")));
        assert!(holds(&snap, max_incl(i("150")), &typed("1.5e2", "double")));
        let dec150 = || max_incl(N::Decimal("150.0".into()));
        assert!(holds(&snap, dec150(), &typed("150", "integer")));
        let dbl150 = || max_incl(N::Double("1.5e2".into()));
        assert!(!holds(&snap, dbl150(), &typed("151", "integer")));
        // NaN compares with nothing
        assert!(!holds(&snap, max_incl(i("150")), &typed("NaN", "double")));
        // derived integer types are numeric; non-numeric values and invalid ones fail
        assert!(holds(&snap, max_incl(i("5")), &typed("5", "byte")));
        assert!(!holds(&snap, max_incl(i("500")), &typed("128", "byte")));
        assert!(!holds(&snap, max_incl(i("150")), &plain("ab")));
        assert!(!holds(&snap, max_incl(i("150")), &typed("abc", "integer")));
        assert!(!holds(&snap, max_incl(i("150")), &iri("http://ex.org/a")));
        let excl = |min: &str, max: &str| NodeConstraint {
            min_exclusive: Some(i(min)),
            max_exclusive: Some(i(max)),
            ..Default::default()
        };
        assert!(holds(&snap, excl("4", "6"), &typed("5", "integer")));
        assert!(!holds(&snap, excl("5", "6"), &typed("5", "integer")));
        assert!(!holds(&snap, excl("4", "5"), &typed("5", "integer")));
        let min_incl = || NodeConstraint {
            min_inclusive: Some(i("1")),
            ..Default::default()
        };
        assert!(holds(&snap, min_incl(), &typed("01", "integer")));
        assert!(holds(&snap, min_incl(), &typed("1.0", "decimal")));
        // the acceptance example: foaf:age 200 against xsd:integer MAXINCLUSIVE 150
        let p = plan(
            &snap,
            NodeConstraint {
                datatype: Some(format!("{XSDNS}integer")),
                max_inclusive: Some(i("150")),
                ..Default::default()
            },
        );
        let carol_age = typed("200", "integer");
        assert!(!p.check_term(NcId(0), &carol_age).unwrap());
        match p.explain_term(NcId(0), &carol_age).unwrap() {
            Some(ShexFailure::Facet { constraint, value }) => {
                assert_eq!(constraint, "MAXINCLUSIVE 150");
                assert_eq!(value, carol_age);
            }
            other => panic!("{other:?}"),
        }
        assert!(p.check_term(NcId(0), &typed("30", "integer")).unwrap());
        assert_eq!(
            p.explain_term(NcId(0), &typed("30", "integer")).unwrap(),
            None
        );
    }

    #[test]
    fn digit_facets() {
        let store = store();
        let snap = store.snapshot();
        let td = |n| NodeConstraint {
            total_digits: Some(n),
            ..Default::default()
        };
        let fd = |n| NodeConstraint {
            fraction_digits: Some(n),
            ..Default::default()
        };
        let dec = |s: &str| typed(s, "decimal");
        // the number of digits is at most TOTALDIGITS
        assert!(holds(&snap, td(5), &dec("1.2345")));
        assert!(!holds(&snap, td(5), &dec("1.23456")));
        assert!(holds(&snap, td(6), &dec("1.23456")));
        // leading and trailing zeros do not count (the canonical form)
        assert!(holds(&snap, td(5), &dec("01.23450")));
        assert!(holds(&snap, td(6), &typed("123450", "integer")));
        assert!(!holds(&snap, td(6), &typed("1234560", "integer")));
        assert!(holds(&snap, td(5), &typed("12345", "integer")));
        assert!(holds(&snap, td(2), &typed("5", "byte")));
        assert!(!holds(&snap, td(2), &typed("128", "byte")));
        // float and double fail, as do non-numbers
        assert!(!holds(&snap, td(5), &typed("1.2345", "float")));
        assert!(!holds(&snap, td(5), &typed("1.5e2", "double")));
        assert!(!holds(&snap, td(5), &plain("ab")));
        // the number of fraction digits is at most FRACTIONDIGITS
        assert!(holds(&snap, fd(4), &dec("1.2345")));
        assert!(!holds(&snap, fd(4), &dec("1.23456")));
        assert!(holds(&snap, fd(4), &dec("01.23450")));
        assert!(holds(&snap, fd(0), &typed("12345", "integer")));
        assert!(!holds(&snap, fd(5), &typed("1.2345", "float")));
        let v = |lex: &str, dt: &str| Value::from_typed(lex, &format!("{XSDNS}{dt}"));
        assert_eq!(digits(&v("0.05", "decimal")), Some((2, 2)));
        assert_eq!(digits(&v("-100", "integer")), Some((3, 0)));
        assert_eq!(digits(&v("-0.0", "decimal")), Some((0, 0)));
    }

    #[test]
    fn value_sets_are_term_equality() {
        let store = store();
        let snap = store.snapshot();
        let literal = |value: &str, language: Option<&str>, datatype: Option<&str>| {
            ValueSetValue::Object(ObjectValue::Literal(ObjectLiteral {
                value: value.into(),
                language: language.map(str::to_string),
                datatype: datatype.map(|d| format!("{XSDNS}{d}")),
            }))
        };
        let vs = || values(vec![literal("1", None, Some("integer"))]);
        assert!(holds(&snap, vs(), &typed("1", "integer")));
        // NumericEquivalence: "01"^^xsd:integer is not 1, nor is 1.0
        assert!(!holds(&snap, vs(), &typed("01", "integer")));
        assert!(!holds(&snap, vs(), &typed("1.0", "decimal")));
        assert!(!holds(&snap, vs(), &plain("ab")));
        let a = ValueSetValue::Object(ObjectValue::Iri("http://ex.org/a".into()));
        let absent = ValueSetValue::Object(ObjectValue::Iri("http://ex.org/absent".into()));
        let vs = || {
            values(vec![
                literal("abc", None, None),
                literal("x", Some("EN"), None),
                a.clone(),
                absent.clone(),
            ])
        };
        assert!(holds(&snap, vs(), &plain("abc")));
        assert!(holds(&snap, vs(), &lang("x", "en")));
        assert!(holds(&snap, vs(), &iri("http://ex.org/a")));
        assert!(!holds(&snap, vs(), &iri("http://ex.org/b")));
        assert!(!holds(&snap, vs(), &plain("ab")));
        // terms that are not in the store compare as terms
        let p = plan(&snap, vs());
        assert!(p.check_term(NcId(0), &iri("http://ex.org/absent")).unwrap());
        assert!(
            !p.check_term(NcId(0), &iri("http://ex.org/absent2"))
                .unwrap()
        );
        // the empty value set matches nothing
        assert!(!holds(&snap, values(vec![]), &iri("http://ex.org/a")));
        match plan(&snap, vs())
            .with_prefixes(&vec![("ex".into(), "http://ex.org/".into())])
            .explain_term(NcId(0), &plain("q"))
            .unwrap()
        {
            Some(ShexFailure::ValueSet { constraint, .. }) => {
                assert_eq!(constraint, r#"["abc" "x"@EN ex:a ex:absent]"#)
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn iri_stems_on_vocabulary_and_delta_ids() {
        let store = store();
        let snap = store.snapshot();
        let tag = |s: &str| snap.lookup_iri(s).unwrap().tag();
        assert_eq!(tag("http://ex.org/sub/1"), Tag::Vocab);
        assert_eq!(tag("http://ex.org/sub/3"), Tag::Delta);
        let stem = || values(vec![ValueSetValue::IriStem("http://ex.org/sub/".into())]);
        assert!(holds(&snap, stem(), &iri("http://ex.org/sub/1")));
        assert!(holds(&snap, stem(), &iri("http://ex.org/sub/2")));
        assert!(holds(&snap, stem(), &iri("http://ex.org/sub/3")));
        assert!(!holds(&snap, stem(), &iri("http://other.org/z")));
        assert!(!holds(&snap, stem(), &iri("http://ex.org/a")));
        let p = plan(&snap, stem());
        assert!(
            p.check_term(NcId(0), &iri("http://ex.org/sub/new"))
                .unwrap()
        );
        // a literal whose lexical form starts with the stem is not an IRI
        let ab = values(vec![ValueSetValue::IriStem("ab".into())]);
        assert!(!holds(&snap, ab, &plain("abc")));
        let range = |stem: Stem| {
            values(vec![ValueSetValue::IriStemRange {
                stem,
                exclusions: vec![
                    Exclusion::Value("http://ex.org/sub/2".into()),
                    Exclusion::Value("http://ex.org/sub/3".into()),
                    Exclusion::Stem("http://ex.org/b".into()),
                ],
            }])
        };
        let ex = || range(Stem::Value("http://ex.org/".into()));
        assert!(holds(&snap, ex(), &iri("http://ex.org/sub/1")));
        assert!(!holds(&snap, ex(), &iri("http://ex.org/sub/2")));
        assert!(!holds(&snap, ex(), &iri("http://ex.org/sub/3")));
        assert!(!holds(&snap, ex(), &iri("http://ex.org/b")));
        assert!(holds(&snap, ex(), &iri("http://ex.org/a")));
        assert!(!holds(&snap, ex(), &iri("http://other.org/z")));
        // the wildcard: anything not excluded, literals included
        assert!(holds(
            &snap,
            range(Stem::Wildcard),
            &iri("http://other.org/z")
        ));
        assert!(holds(&snap, range(Stem::Wildcard), &plain("ab")));
        assert!(!holds(
            &snap,
            range(Stem::Wildcard),
            &iri("http://ex.org/sub/3")
        ));
        // a delta id against an exclusion stem
        let stem_excl = || {
            values(vec![ValueSetValue::IriStemRange {
                stem: Stem::Wildcard,
                exclusions: vec![Exclusion::Stem("http://ex.org/sub/".into())],
            }])
        };
        assert!(!holds(&snap, stem_excl(), &iri("http://ex.org/sub/3")));
        assert!(!holds(&snap, stem_excl(), &iri("http://ex.org/sub/1")));
        assert!(holds(&snap, stem_excl(), &iri("http://ex.org/a")));
    }

    #[test]
    fn literal_and_language_stems() {
        let store = store();
        let snap = store.snapshot();
        let lit = || values(vec![ValueSetValue::LiteralStem("ab".into())]);
        assert!(holds(&snap, lit(), &plain("abcd")));
        assert!(holds(&snap, lit(), &plain("ab")));
        assert!(!holds(&snap, lit(), &lang("x", "en")));
        assert!(!holds(&snap, lit(), &iri("http://ex.org/a")));
        let lit_range = || {
            values(vec![ValueSetValue::LiteralStemRange {
                stem: Stem::Value("ab".into()),
                exclusions: vec![
                    Exclusion::Value("abc".into()),
                    Exclusion::Stem("abcd".into()),
                ],
            }])
        };
        assert!(holds(&snap, lit_range(), &plain("ab")));
        assert!(!holds(&snap, lit_range(), &plain("abc")));
        assert!(!holds(&snap, lit_range(), &plain("abcd")));

        // @en matches the tag exactly (case-insensitively); @en~ by basic filtering
        let en = || values(vec![ValueSetValue::Language("EN".into())]);
        assert!(holds(&snap, en(), &lang("x", "en")));
        assert!(!holds(&snap, en(), &lang("y", "en-us")));
        assert!(!holds(&snap, en(), &plain("ab")));
        let en_stem = || values(vec![ValueSetValue::LanguageStem("en".into())]);
        assert!(holds(&snap, en_stem(), &lang("x", "en")));
        assert!(holds(&snap, en_stem(), &lang("y", "en-us")));
        assert!(holds(&snap, en_stem(), &lang("delta", "en-gb")));
        assert!(!holds(&snap, en_stem(), &lang("z", "fr")));
        assert!(!holds(&snap, en_stem(), &plain("ab")));
        let p = plan(&snap, en_stem());
        assert!(!p.check_term(NcId(0), &lang("q", "english")).unwrap());
        // @~ matches every language-tagged literal, and nothing else
        let any = || values(vec![ValueSetValue::LanguageStem(String::new())]);
        assert!(holds(&snap, any(), &lang("z", "fr")));
        assert!(holds(&snap, any(), &lang("delta", "en-gb")));
        assert!(!holds(&snap, any(), &plain("ab")));
        assert!(!holds(&snap, any(), &iri("http://ex.org/a")));
        // [@~ - @en-US] and [. - @en~]
        let range =
            |stem, exclusions| values(vec![ValueSetValue::LanguageStemRange { stem, exclusions }]);
        let not_us = || {
            range(
                Stem::Value(String::new()),
                vec![Exclusion::Value("en-US".into())],
            )
        };
        assert!(holds(&snap, not_us(), &lang("x", "en")));
        assert!(!holds(&snap, not_us(), &lang("y", "en-us")));
        assert!(!holds(&snap, not_us(), &plain("ab")));
        let not_en = || range(Stem::Wildcard, vec![Exclusion::Stem("en".into())]);
        assert!(holds(&snap, not_en(), &lang("z", "fr")));
        assert!(holds(&snap, not_en(), &plain("ab")));
        assert!(!holds(&snap, not_en(), &lang("delta", "en-gb")));
        assert!(!holds(&snap, not_en(), &lang("x", "en")));
    }

    #[test]
    fn every_part_must_hold() {
        let store = store();
        let snap = store.snapshot();
        let nc = || NodeConstraint {
            node_kind: Some(NodeKind::Literal),
            datatype: Some(format!("{XSDNS}integer")),
            min_inclusive: Some(N::Integer("2".into())),
            values: Some(vec![
                ValueSetValue::LiteralStem("1".into()),
                ValueSetValue::LiteralStem("5".into()),
            ]),
            ..Default::default()
        };
        assert!(holds(&snap, nc(), &typed("5", "integer")));
        assert!(holds(&snap, nc(), &typed("150", "integer")));
        // the stem holds, MININCLUSIVE does not
        assert!(!holds(&snap, nc(), &typed("1", "integer")));
        // the wrong datatype
        assert!(!holds(&snap, nc(), &typed("5", "byte")));
        // not in the value set
        assert!(!holds(&snap, nc(), &typed("abc", "integer")));
    }
}

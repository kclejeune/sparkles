//! Data ranges in normal form over the whole OWL 2 datatype map.
//!
//! A normal form is a base, a union over the families of the map of sets that each
//! family represents exactly, together with finite sets of values included beyond the
//! base and excluded from it. Values that a family's base represents as points, the
//! numbers, the floating-point and Boolean values, the date-times without an offset and
//! the strings of the classes with at most three members, are never held in the
//! exceptions. Equal ranges then intern to one id except when a larger finite class,
//! such as `xsd:hexBinary` of length 1, is held in the base by one and in the exceptions
//! by the other. Ids are therefore cache keys: whether one range is within another is
//! decided through sizes, never by comparing ids.

use super::bcp47;
use super::sets::{CAP, Dense, Disc, End, Rat, Size, Step};
use super::value::{self, DVal, NAN, Unsupported, double_key, float_key};
use crate::datatypes;
use crate::model::{DataRange, Datatype, Literal};
use crate::vocab;
use num_bigint::BigInt;
use rustc_hash::FxHashMap;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;

/// The cells of the string chain `string ⊃ normalizedString ⊃ token ⊃ NMTOKEN ⊃ Name ⊃
/// NCName ⊃ language`: cell `i` holds the strings at level `i` and not at level `i + 1`.
pub const CELLS: usize = 7;

/// The number of characters of XML 1.0, the size of each string position.
pub const XML_CHARS: u128 = 1_112_033;

/// The offsets of a date-time from -14:00 to +14:00 in minutes.
pub const OFFSETS: u128 = 1681;

/// Fourteen hours in seconds, the widest offset.
const FOURTEEN_HOURS: i64 = 14 * 3600;

type Len = Disc<u64>;

fn len_all() -> Len {
    len_from(0)
}
fn len_not(l: &Len) -> Len {
    l.not(&End::At(0), &End::Max)
}
fn len_from(lo: u64) -> Len {
    Disc::range(End::At(lo), End::Max)
}
fn len_not_from(l: &Len, lo: u64) -> Len {
    l.not(&End::At(lo), &End::Max)
}

/// The shortest length that each cell of the string chain holds in its base. The
/// shorter strings of a cell are a class of at most three members held as [`Tiny`]
/// bits: the tab, line feed and carriage return in cell 0, the space in cell 1, the
/// empty string in cell 2 and `:` in cell 4. The cells with no string of length 0 or 1
/// start where their strings do.
const CELL_MIN: [u64; CELLS] = [2, 2, 1, 1, 2, 1, 1];

/// The shortest length of `xsd:anyURI`, `xsd:hexBinary` and `xsd:base64Binary` in the
/// base, whose single values of length 0 are [`Tiny`] bits.
const BIN_MIN: u64 = 1;

/// The members of the classes of at most three values, one bit each.
struct Tiny;

impl Tiny {
    const TAB: u16 = 1 << 0;
    const LF: u16 = 1 << 1;
    const CR: u16 = 1 << 2;
    const SPACE: u16 = 1 << 3;
    const EMPTY: u16 = 1 << 4;
    const COLON: u16 = 1 << 5;
    const URI: u16 = 1 << 6;
    const HEX: u16 = 1 << 7;
    const B64: u16 = 1 << 8;
    const ALL: u16 = (1 << 9) - 1;
    /// the bits of the plain strings, by cell and length
    const STR: [(u16, usize, u64); 6] = [
        (Tiny::TAB, 0, 1),
        (Tiny::LF, 0, 1),
        (Tiny::CR, 0, 1),
        (Tiny::SPACE, 1, 1),
        (Tiny::EMPTY, 2, 0),
        (Tiny::COLON, 4, 1),
    ];
    const PLAIN: u16 = Tiny::TAB | Tiny::LF | Tiny::CR | Tiny::SPACE | Tiny::EMPTY | Tiny::COLON;
    fn of(v: &DVal) -> Option<u16> {
        Some(match v {
            DVal::Str(s) => match s.as_str() {
                "\t" => Tiny::TAB,
                "\n" => Tiny::LF,
                "\r" => Tiny::CR,
                " " => Tiny::SPACE,
                "" => Tiny::EMPTY,
                ":" => Tiny::COLON,
                _ => return None,
            },
            DVal::Uri(s) if s.is_empty() => Tiny::URI,
            DVal::Hex(b) if b.is_empty() => Tiny::HEX,
            DVal::B64(b) if b.is_empty() => Tiny::B64,
            _ => return None,
        })
    }
    /// The plain string bits of the cells from `k` on.
    fn from_level(k: usize) -> u16 {
        Tiny::STR
            .iter()
            .filter(|(_, c, _)| *c >= k)
            .fold(0, |a, (b, _, _)| a | b)
    }
}

fn dec_m(q: &Rat) -> bool {
    q.is_decimal() && !q.is_int()
}
fn rat_m(q: &Rat) -> bool {
    !q.is_decimal()
}
fn irr_m(_: &Rat) -> bool {
    false
}
fn time_m(q: &Rat) -> bool {
    q.is_decimal()
}

fn float_dom() -> (End<u64>, End<u64>) {
    (
        End::At(float_key(f32::NEG_INFINITY)),
        End::At(float_key(f32::INFINITY)),
    )
}
fn double_dom() -> (End<u64>, End<u64>) {
    (
        End::At(double_key(f64::NEG_INFINITY)),
        End::At(double_key(f64::INFINITY)),
    )
}

/// The strings with language tags: a set of lengths for each tag, the set of the
/// longest basic language range in `over` that matches the tag, or `default`. No entry
/// of `over` equals the set it overrides, so the form is unique.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Tags {
    default: Len,
    over: BTreeMap<String, Len>,
}

impl Tags {
    fn uniform(l: Len) -> Tags {
        Tags {
            default: l,
            over: BTreeMap::new(),
        }
    }
    /// The set of the longest key that matches `tag`, among the proper prefixes of the
    /// tag when `proper`.
    fn eff(&self, tag: &str, proper: bool) -> &Len {
        let mut t = tag;
        if proper {
            match t.rfind('-') {
                Some(i) => t = &t[..i],
                None => return &self.default,
            }
        }
        loop {
            if let Some(l) = self.over.get(t) {
                return l;
            }
            match t.rfind('-') {
                Some(i) => t = &t[..i],
                None => return &self.default,
            }
        }
    }
    fn map2(&self, o: &Tags, f: impl Fn(&Len, &Len) -> Len) -> Tags {
        let keys: BTreeSet<&String> = self.over.keys().chain(o.over.keys()).collect();
        let mut r = Tags {
            default: f(&self.default, &o.default),
            over: keys
                .into_iter()
                .map(|k| (k.clone(), f(self.eff(k, false), o.eff(k, false))))
                .collect(),
        };
        r.canon();
        r
    }
    fn map1(&self, f: impl Fn(&Len) -> Len) -> Tags {
        let mut r = Tags {
            default: f(&self.default),
            over: self.over.iter().map(|(k, l)| (k.clone(), f(l))).collect(),
        };
        r.canon();
        r
    }
    fn canon(&mut self) {
        let drop: Vec<String> = self
            .over
            .iter()
            .filter(|(k, l)| self.eff(k, true) == *l)
            .map(|(k, _)| k.clone())
            .collect();
        for k in drop {
            self.over.remove(&k);
        }
    }
    /// The number of tagged strings. A tag is one of the irregular grandfathered tags,
    /// which are counted one by one, or one of the infinitely many others. The tags that
    /// no key matches include infinitely many others while there are fewer keys than
    /// primary subtags of two letters, and a key whose range matches other tags governs
    /// infinitely many of them unless longer keys cover the extensions of its
    /// [`bcp47::open_completion`], which the count of fewer than 36 keys rules out. A
    /// case outside these is not known.
    fn size(&self) -> Size {
        let mut s = Size::ZERO;
        for g in bcp47::IRREGULAR {
            s = s.add(sum_lengths(self.eff(g, false), |n| exact_pow(XML_CHARS, n)));
        }
        if !self.default.is_empty() {
            s = s.add(if self.over.len() < 26 * 26 {
                Size::INFINITE
            } else {
                Size::UNKNOWN
            });
        }
        for (k, l) in &self.over {
            if l.is_empty() {
                continue;
            }
            let Some(c) = bcp47::open_completion(k) else {
                continue;
            };
            let longer = self.over.keys().filter(|o| *o != k && bcp47::prefix(k, o));
            let between = longer.clone().any(|o| bcp47::prefix(o, &c));
            let beyond = longer.filter(|o| bcp47::prefix(&c, o)).count();
            s = s.add(if !between && beyond < 36 {
                Size::INFINITE
            } else {
                Size::UNKNOWN
            });
        }
        s
    }
}

/// A data range in normal form.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Nf {
    int: Disc<BigInt>,
    dec: Dense,
    rat: Dense,
    irr: Dense,
    float: Disc<u64>,
    float_nan: bool,
    double: Disc<u64>,
    double_nan: bool,
    cells: [Len; CELLS],
    tags: Tags,
    uri: Len,
    hex: Len,
    b64: Len,
    /// bit 0 for false, bit 1 for true
    boolean: u8,
    /// the members of the classes of at most three values, by [`Tiny`] bit
    tiny: u16,
    /// date-times with an offset, by the instant
    toff: Dense,
    /// date-times without an offset, by the local time read as UTC
    tloc: Dense,
    xml: bool,
    /// the values of the data domain outside every datatype of the map, which the
    /// Direct Semantics allows (§2.2): `rdfs:Literal` is the whole domain
    other: bool,
    inc: BTreeSet<DVal>,
    exc: BTreeSet<DVal>,
}

#[derive(Clone, Copy)]
enum Op {
    Meet,
    Join,
}

impl Op {
    fn b(self, a: bool, b: bool) -> bool {
        match self {
            Op::Meet => a && b,
            Op::Join => a || b,
        }
    }
    fn disc<T: Step>(self, a: &Disc<T>, b: &Disc<T>) -> Disc<T> {
        match self {
            Op::Meet => a.meet(b),
            Op::Join => a.join(b),
        }
    }
    fn dense(self, a: &Dense, b: &Dense, m: fn(&Rat) -> bool) -> Dense {
        a.op(b, |x, y| self.b(x, y), m)
    }
}

fn chars(s: &str) -> u64 {
    s.chars().count() as u64
}

/// The cell of the string chain that holds `s`, a string of XML characters.
pub fn cell_of(s: &str) -> usize {
    if !datatypes::normalized(s) {
        0
    } else if !datatypes::token(s) {
        1
    } else if s.is_empty() || !s.chars().all(datatypes::is_name_char) {
        2
    } else if !datatypes::name(s, true) {
        3
    } else if !datatypes::name(s, false) {
        4
    } else if !datatypes::language_tag(s) {
        5
    } else {
        6
    }
}

impl Nf {
    pub fn empty() -> Nf {
        Nf {
            int: Disc::empty(),
            dec: Dense::empty(),
            rat: Dense::empty(),
            irr: Dense::empty(),
            float: Disc::empty(),
            float_nan: false,
            double: Disc::empty(),
            double_nan: false,
            cells: std::array::from_fn(|_| Disc::empty()),
            tags: Tags::uniform(Disc::empty()),
            uri: Disc::empty(),
            hex: Disc::empty(),
            b64: Disc::empty(),
            boolean: 0,
            tiny: 0,
            toff: Dense::empty(),
            tloc: Dense::empty(),
            xml: false,
            other: false,
            inc: BTreeSet::new(),
            exc: BTreeSet::new(),
        }
    }

    /// `rdfs:Literal`, every data value.
    pub fn full() -> Nf {
        let (flo, fhi) = float_dom();
        let (dlo, dhi) = double_dom();
        Nf {
            int: Disc::range(End::Min, End::Max),
            dec: Dense::full(),
            rat: Dense::full(),
            irr: Dense::full(),
            float: Disc::range(flo, fhi),
            float_nan: true,
            double: Disc::range(dlo, dhi),
            double_nan: true,
            cells: std::array::from_fn(|i| len_from(CELL_MIN[i])),
            tags: Tags::uniform(len_all()),
            uri: len_from(BIN_MIN),
            hex: len_from(BIN_MIN),
            b64: len_from(BIN_MIN),
            boolean: 3,
            tiny: Tiny::ALL,
            toff: Dense::full(),
            tloc: Dense::full(),
            xml: true,
            other: true,
            inc: BTreeSet::new(),
            exc: BTreeSet::new(),
        }
    }

    fn base_contains(&self, v: &DVal) -> bool {
        if let Some(b) = Tiny::of(v) {
            return self.tiny & b != 0;
        }
        match v {
            DVal::Real(q) => {
                if q.is_int() {
                    self.int.contains(&q.floor())
                } else if q.is_decimal() {
                    self.dec.contains(q)
                } else {
                    self.rat.contains(q)
                }
            }
            DVal::Float(k) => {
                if *k == NAN {
                    self.float_nan
                } else {
                    self.float.contains(k)
                }
            }
            DVal::Double(k) => {
                if *k == NAN {
                    self.double_nan
                } else {
                    self.double.contains(k)
                }
            }
            DVal::Str(s) => self.cells[cell_of(s)].contains(&chars(s)),
            DVal::Lang(s, t) => self.tags.eff(t, false).contains(&chars(s)),
            DVal::Uri(s) => self.uri.contains(&chars(s)),
            DVal::Hex(b) => self.hex.contains(&(b.len() as u64)),
            DVal::B64(b) => self.b64.contains(&(b.len() as u64)),
            DVal::Bool(b) => self.boolean & (1 << u8::from(*b)) != 0,
            DVal::Time(q, Some(_)) => self.toff.contains(q),
            DVal::Time(q, None) => self.tloc.contains(q),
        }
    }

    pub fn contains(&self, v: &DVal) -> bool {
        if self.inc.contains(v) {
            return true;
        }
        if self.exc.contains(v) {
            return false;
        }
        self.base_contains(v)
    }

    /// The range of one value.
    pub fn point(v: &DVal) -> Nf {
        let mut n = Nf::empty();
        match v {
            DVal::Real(q) => {
                if q.is_int() {
                    n.int = Disc::point(q.floor());
                } else if q.is_decimal() {
                    n.dec = Dense::point(q.clone(), dec_m);
                } else {
                    n.rat = Dense::point(q.clone(), rat_m);
                }
            }
            DVal::Float(k) if *k == NAN => n.float_nan = true,
            DVal::Float(k) => n.float = Disc::point(*k),
            DVal::Double(k) if *k == NAN => n.double_nan = true,
            DVal::Double(k) => n.double = Disc::point(*k),
            DVal::Bool(b) => n.boolean = 1 << u8::from(*b),
            DVal::Time(q, None) => n.tloc = Dense::point(q.clone(), time_m),
            _ if Tiny::of(v).is_some() => n.tiny = Tiny::of(v).expect("a tiny value"),
            _ => {
                n.inc.insert(v.clone());
            }
        }
        n
    }

    fn base(&self, o: &Nf, op: Op) -> Nf {
        Nf {
            int: op.disc(&self.int, &o.int),
            dec: op.dense(&self.dec, &o.dec, dec_m),
            rat: op.dense(&self.rat, &o.rat, rat_m),
            irr: op.dense(&self.irr, &o.irr, irr_m),
            float: op.disc(&self.float, &o.float),
            float_nan: op.b(self.float_nan, o.float_nan),
            double: op.disc(&self.double, &o.double),
            double_nan: op.b(self.double_nan, o.double_nan),
            cells: std::array::from_fn(|i| op.disc(&self.cells[i], &o.cells[i])),
            tags: self.tags.map2(&o.tags, |a, b| op.disc(a, b)),
            uri: op.disc(&self.uri, &o.uri),
            hex: op.disc(&self.hex, &o.hex),
            b64: op.disc(&self.b64, &o.b64),
            boolean: match op {
                Op::Meet => self.boolean & o.boolean,
                Op::Join => self.boolean | o.boolean,
            },
            tiny: match op {
                Op::Meet => self.tiny & o.tiny,
                Op::Join => self.tiny | o.tiny,
            },
            toff: op.dense(&self.toff, &o.toff, time_m),
            tloc: op.dense(&self.tloc, &o.tloc, time_m),
            xml: op.b(self.xml, o.xml),
            other: op.b(self.other, o.other),
            inc: BTreeSet::new(),
            exc: BTreeSet::new(),
        }
    }

    fn combine(&self, o: &Nf, op: Op) -> Nf {
        let mut r = self.base(o, op);
        let cands: BTreeSet<&DVal> = self
            .inc
            .iter()
            .chain(&self.exc)
            .chain(&o.inc)
            .chain(&o.exc)
            .collect();
        for v in cands {
            let m = op.b(self.contains(v), o.contains(v));
            let b = r.base_contains(v);
            if m && !b {
                r.inc.insert(v.clone());
            } else if !m && b {
                r.exc.insert(v.clone());
            }
        }
        r
    }

    pub fn meet(&self, o: &Nf) -> Nf {
        self.combine(o, Op::Meet)
    }
    pub fn join(&self, o: &Nf) -> Nf {
        self.combine(o, Op::Join)
    }

    /// The complement within the data domain (Direct Semantics, Table 3).
    pub fn not(&self) -> Nf {
        let (flo, fhi) = float_dom();
        let (dlo, dhi) = double_dom();
        Nf {
            int: self.int.not(&End::Min, &End::Max),
            dec: self.dec.not(dec_m),
            rat: self.rat.not(rat_m),
            irr: self.irr.not(irr_m),
            float: self.float.not(&flo, &fhi),
            float_nan: !self.float_nan,
            double: self.double.not(&dlo, &dhi),
            double_nan: !self.double_nan,
            cells: std::array::from_fn(|i| len_not_from(&self.cells[i], CELL_MIN[i])),
            tags: self.tags.map1(len_not),
            uri: len_not_from(&self.uri, BIN_MIN),
            hex: len_not_from(&self.hex, BIN_MIN),
            b64: len_not_from(&self.b64, BIN_MIN),
            boolean: !self.boolean & 3,
            tiny: !self.tiny & Tiny::ALL,
            toff: self.toff.not(time_m),
            tloc: self.tloc.not(time_m),
            xml: !self.xml,
            other: !self.other,
            inc: self.exc.clone(),
            exc: self.inc.clone(),
        }
    }

    pub fn size(&self) -> Size {
        let mut s = self.int.size();
        for d in [&self.dec, &self.rat, &self.irr] {
            s = s.add(d.size(1));
        }
        s = s
            .add(self.float.size())
            .add(Size::exact(u128::from(self.float_nan)));
        s = s
            .add(self.double.size())
            .add(Size::exact(u128::from(self.double_nan)));
        for (i, c) in self.cells.iter().enumerate() {
            s = s.add(sum_lengths(c, |n| string_cell(i, n)));
        }
        s = s.add(self.tags.size());
        s = s.add(sum_lengths(&self.uri, uri_length));
        for b in [&self.hex, &self.b64] {
            s = s.add(sum_lengths(b, |n| exact_pow(256, n)));
        }
        s = s.add(Size::exact(u128::from(self.boolean.count_ones())));
        s = s.add(Size::exact(u128::from(self.tiny.count_ones())));
        s = s.add(self.toff.size(OFFSETS)).add(self.tloc.size(1));
        if self.xml || self.other {
            s = s.add(Size::INFINITE);
        }
        s.minus(self.exc.len() as u128)
            .add(Size::exact(self.inc.len() as u128))
    }

    /// The heap and inline bytes of the form, for the run's memory budget.
    pub fn bytes(&self) -> usize {
        let lens = self
            .cells
            .iter()
            .chain([&self.uri, &self.hex, &self.b64, &self.tags.default])
            .chain(self.tags.over.values())
            .map(Disc::bytes)
            .sum::<usize>();
        size_of::<Nf>()
            + self.int.bytes()
            + self.float.bytes()
            + self.double.bytes()
            + lens
            + [&self.dec, &self.rat, &self.irr, &self.toff, &self.tloc]
                .iter()
                .map(|d| d.bytes())
                .sum::<usize>()
            + self
                .inc
                .iter()
                .chain(&self.exc)
                .map(|v| DVal::bytes(v) + 32)
                .sum::<usize>()
            + self.tags.over.keys().map(|k| k.len() + 64).sum::<usize>()
    }
}

/// `b^e` saturating at [`CAP`].
fn spow(b: u128, e: u64) -> u128 {
    let mut r: u128 = 1;
    for _ in 0..e {
        r = r.saturating_mul(b);
        if r >= CAP {
            return CAP;
        }
    }
    r
}

fn exact_pow(b: u128, e: u64) -> Size {
    Size::exact(spow(b, e))
}

fn bounds(lo: u128, hi: u128) -> Size {
    Size {
        lo: lo.min(CAP),
        hi: (hi < CAP).then_some(hi),
        inf: false,
    }
}

/// The sizes of a set of lengths, summed over the lengths it holds.
fn sum_lengths(l: &Len, f: impl Fn(u64) -> Size) -> Size {
    // Every length from the shortest a family holds in its base on has a value, so a
    // set of lengths without an upper end is infinitely many values.
    if l.iv.last().is_some_and(|(_, b)| *b == End::Max) {
        return Size::INFINITE;
    }
    let mut s = Size::ZERO;
    for (a, b) in &l.iv {
        let (End::At(a), End::At(b)) = (a, b) else {
            unreachable!("lengths start at a length and end below the last interval")
        };
        for n in *a..=*b {
            s = s.add(f(n));
            if s.lo >= CAP && s.hi.is_none() {
                // a saturated finite count
                return s;
            }
        }
    }
    s
}

struct CharCounts {
    /// the strings of length 1 in each cell
    one: [u128; CELLS],
    name_chars: u128,
}

fn char_counts() -> &'static CharCounts {
    static C: OnceLock<CharCounts> = OnceLock::new();
    C.get_or_init(|| {
        let mut one = [0u128; CELLS];
        let mut name_chars = 0;
        let mut buf = [0u8; 4];
        for u in 0..=0x10FFFFu32 {
            let Some(c) = char::from_u32(u) else { continue };
            let s = c.encode_utf8(&mut buf);
            if !datatypes::xml_chars(s) {
                continue;
            }
            one[cell_of(s)] += 1;
            if datatypes::is_name_char(c) {
                name_chars += 1;
            }
        }
        CharCounts { one, name_chars }
    })
}

/// The `xsd:language` strings of each length: `[a-zA-Z]{1,8}(-[a-zA-Z0-9]{1,8})*`.
fn language_count(n: u64) -> u128 {
    if n > 100 {
        return CAP;
    }
    let n = n as usize;
    // t[m]: the continuations `(-[a-zA-Z0-9]{1,8})*` of length m
    let mut t = vec![0u128; n + 1];
    t[0] = 1;
    for m in 1..=n {
        let mut v: u128 = 0;
        for k in 1..=8usize {
            if k < m {
                v = v.saturating_add(spow(62, k as u64).saturating_mul(t[m - 1 - k]));
            }
        }
        t[m] = v.min(CAP);
    }
    let mut l: u128 = 0;
    for k in 1..=8usize.min(n) {
        l = l.saturating_add(spow(52, k as u64).saturating_mul(t[n - k]));
    }
    l.min(CAP)
}

/// The strings of length `n` in cell `i` of the string chain: exact at lengths 0 and 1
/// and for `language`, and otherwise bounded below by an explicit family of members
/// and above by every string of that length.
pub fn string_cell(i: usize, n: u64) -> Size {
    if i == 6 {
        return Size::exact(language_count(n));
    }
    if n == 0 {
        return Size::exact(u128::from(i == 2));
    }
    let cc = char_counts();
    if n == 1 {
        return Size::exact(cc.one[i]);
    }
    let nc = cc.name_chars;
    let lo = match i {
        // a tab first
        0 => spow(XML_CHARS, n - 1).saturating_mul(3),
        // a space first, then characters of normalizedString
        1 => spow(XML_CHARS - 3, n - 1),
        // `!` first, then name characters
        2 => spow(nc, n - 1),
        // a digit first, then name characters
        3 => spow(nc, n - 1).saturating_mul(10),
        // `:` first, or `_` first, then name characters other than `:`
        _ => spow(nc - 1, n - 1),
    };
    bounds(lo, spow(XML_CHARS, n))
}

/// The values of `xsd:anyURI` of length `n`. XML Schema 1.1 makes every string of
/// characters a value and OWL 2 §4.6 calls the value space the set of IRIs, so the
/// count is bounded below by the strings of ASCII letters, which are relative IRIs,
/// and above by every string.
fn uri_length(n: u64) -> Size {
    bounds(spow(26, n), spow(XML_CHARS, n))
}

/// The value of an integer datatype's bounds, by local name.
fn integer_bounds(l: &str) -> Option<(End<BigInt>, End<BigInt>)> {
    let b = |x: i128| End::At(BigInt::from(x));
    Some(match l {
        "integer" => (End::Min, End::Max),
        "nonNegativeInteger" => (b(0), End::Max),
        "nonPositiveInteger" => (End::Min, b(0)),
        "positiveInteger" => (b(1), End::Max),
        "negativeInteger" => (End::Min, b(-1)),
        "long" => (b(i64::MIN.into()), b(i64::MAX.into())),
        "int" => (b(i32::MIN.into()), b(i32::MAX.into())),
        "short" => (b(i16::MIN.into()), b(i16::MAX.into())),
        "byte" => (b(i8::MIN.into()), b(i8::MAX.into())),
        "unsignedLong" => (b(0), b(u64::MAX.into())),
        "unsignedInt" => (b(0), b(u32::MAX.into())),
        "unsignedShort" => (b(0), b(u16::MAX.into())),
        "unsignedByte" => (b(0), b(u8::MAX.into())),
        _ => return None,
    })
}

/// The family of a datatype that facets restrict.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Fam {
    Real,
    Float,
    Double,
    Plain,
    Uri,
    Binary,
    Time,
    None,
}

/// The range of a datatype of the map, and the family its facets restrict.
fn datatype_nf(iri: &str) -> Result<(Nf, Fam), Unsupported> {
    let mut n = Nf::empty();
    if iri == vocab::RDFS_LITERAL {
        return Ok((Nf::full(), Fam::None));
    }
    let f = Nf::full();
    if iri == vocab::OWL_REAL || iri == vocab::OWL_RATIONAL {
        n.int = f.int;
        n.dec = f.dec;
        n.rat = f.rat;
        if iri == vocab::OWL_REAL {
            n.irr = f.irr;
        }
        return Ok((n, Fam::Real));
    }
    if iri == vocab::RDF_PLAIN_LITERAL {
        n.cells = f.cells;
        n.tags = f.tags;
        n.tiny = Tiny::PLAIN;
        return Ok((n, Fam::Plain));
    }
    if iri == vocab::RDF_LANG_STRING {
        n.tags = f.tags;
        return Ok((n, Fam::Plain));
    }
    if iri == vocab::RDF_XML_LITERAL {
        n.xml = true;
        return Ok((n, Fam::None));
    }
    let Some(l) = iri.strip_prefix(vocab::XSD) else {
        return Err(Unsupported::Datatype(iri.to_string()));
    };
    if let Some((lo, hi)) = integer_bounds(l) {
        n.int = Disc::range(lo, hi);
        return Ok((n, Fam::Real));
    }
    let level = |k: usize| -> [Len; CELLS] {
        std::array::from_fn(|i| {
            if i >= k {
                f.cells[i].clone()
            } else {
                Disc::empty()
            }
        })
    };
    let fam = match l {
        "decimal" => {
            n.int = f.int;
            n.dec = f.dec;
            Fam::Real
        }
        "double" => {
            n.double = f.double;
            n.double_nan = true;
            Fam::Double
        }
        "float" => {
            n.float = f.float;
            n.float_nan = true;
            Fam::Float
        }
        "string" => {
            n.cells = level(0);
            n.tiny = Tiny::from_level(0);
            Fam::Plain
        }
        "normalizedString" => {
            n.cells = level(1);
            n.tiny = Tiny::from_level(1);
            Fam::Plain
        }
        "token" => {
            n.cells = level(2);
            n.tiny = Tiny::from_level(2);
            Fam::Plain
        }
        "NMTOKEN" => {
            n.cells = level(3);
            n.tiny = Tiny::from_level(3);
            Fam::Plain
        }
        "Name" => {
            n.cells = level(4);
            n.tiny = Tiny::from_level(4);
            Fam::Plain
        }
        "NCName" => {
            n.cells = level(5);
            n.tiny = Tiny::from_level(5);
            Fam::Plain
        }
        "language" => {
            n.cells = level(6);
            n.tiny = Tiny::from_level(6);
            Fam::Plain
        }
        "anyURI" => {
            n.uri = f.uri;
            n.tiny = Tiny::URI;
            Fam::Uri
        }
        "hexBinary" => {
            n.hex = f.hex;
            n.tiny = Tiny::HEX;
            Fam::Binary
        }
        "base64Binary" => {
            n.b64 = f.b64;
            n.tiny = Tiny::B64;
            Fam::Binary
        }
        "boolean" => {
            n.boolean = 3;
            Fam::None
        }
        "dateTime" => {
            n.toff = f.toff;
            n.tloc = f.tloc;
            Fam::Time
        }
        "dateTimeStamp" => {
            n.toff = f.toff;
            Fam::Time
        }
        _ => return Err(Unsupported::Datatype(iri.to_string())),
    };
    Ok((n, fam))
}

/// A bound facet: lower or upper, inclusive or not.
#[derive(Clone, Copy)]
struct Bound {
    lower: bool,
    incl: bool,
}

fn dense_bound(q: Rat, b: Bound, m: fn(&Rat) -> bool) -> Dense {
    if b.lower {
        Dense::interval(Some((q, b.incl)), None, m)
    } else {
        Dense::interval(None, Some((q, b.incl)), m)
    }
}

fn key_bound(
    k: u64,
    zero: Option<(u64, u64)>,
    b: Bound,
    lo: &End<u64>,
    hi: &End<u64>,
) -> Disc<u64> {
    // the two zeros are equal, so a bound at either cuts at both
    let (kl, kh) = zero.unwrap_or((k, k));
    let at = |x: u64| End::At(x);
    match (b.lower, b.incl) {
        (true, true) => Disc::range(at(kl), hi.clone()),
        (true, false) => match hi {
            End::At(h) if kh >= *h => Disc::empty(),
            _ => Disc::range(at(kh + 1), hi.clone()),
        },
        (false, true) => Disc::range(lo.clone(), at(kh)),
        (false, false) => match lo {
            End::At(l) if kl <= *l => Disc::empty(),
            _ => Disc::range(lo.clone(), at(kl - 1)),
        },
    }
}

/// The set a facet pair restricts its family to, with every other family full.
fn facet_nf(fam: Fam, facet: &str, value: &Literal) -> Result<Nf, Unsupported> {
    let name = facet.strip_prefix(vocab::XSD).unwrap_or(facet);
    let mut n = Nf::full();
    let bound = match name {
        "minInclusive" => Some(Bound {
            lower: true,
            incl: true,
        }),
        "minExclusive" => Some(Bound {
            lower: true,
            incl: false,
        }),
        "maxInclusive" => Some(Bound {
            lower: false,
            incl: true,
        }),
        "maxExclusive" => Some(Bound {
            lower: false,
            incl: false,
        }),
        _ => None,
    };
    if let Some(b) = bound {
        let v = value::of_literal(value)?;
        match (fam, v) {
            (Fam::Real, DVal::Real(q)) => {
                n.int = match (b.lower, b.incl) {
                    (true, true) => Disc::range(End::At(q.ceil()), End::Max),
                    (true, false) => Disc::range(End::At(q.floor() + 1), End::Max),
                    (false, true) => Disc::range(End::Min, End::At(q.floor())),
                    (false, false) => Disc::range(End::Min, End::At(q.ceil() - 1)),
                };
                n.dec = dense_bound(q.clone(), b, dec_m);
                n.rat = dense_bound(q.clone(), b, rat_m);
                n.irr = dense_bound(q, b, irr_m);
            }
            (Fam::Float, DVal::Float(k)) => {
                n.float_nan = false;
                let z = (float_key(-0.0), float_key(0.0));
                let (lo, hi) = float_dom();
                n.float = if k == NAN {
                    Disc::empty()
                } else {
                    key_bound(k, (k == z.0 || k == z.1).then_some(z), b, &lo, &hi)
                };
            }
            (Fam::Double, DVal::Double(k)) => {
                n.double_nan = false;
                let z = (double_key(-0.0), double_key(0.0));
                let (lo, hi) = double_dom();
                n.double = if k == NAN {
                    Disc::empty()
                } else {
                    key_bound(k, (k == z.0 || k == z.1).then_some(z), b, &lo, &hi)
                };
            }
            (Fam::Time, DVal::Time(q, off)) => {
                // A value with an offset and one without are never equal. One without
                // is below one with only when it is with -14:00 imputed, the latest
                // instant it can be, and above it only with +14:00, the earliest
                // (Structural Specification §4.7).
                let strict = Bound { incl: false, ..b };
                let shift = if b.lower {
                    FOURTEEN_HOURS
                } else {
                    -FOURTEEN_HOURS
                };
                if off.is_some() {
                    n.toff = dense_bound(q.clone(), b, time_m);
                    n.tloc = dense_bound(q.plus(shift), strict, time_m);
                } else {
                    n.tloc = dense_bound(q.clone(), b, time_m);
                    n.toff = dense_bound(q.plus(shift), strict, time_m);
                }
            }
            _ => return Err(Unsupported::Facet(facet.to_string())),
        }
        return Ok(n);
    }
    match name {
        "length" | "minLength" | "maxLength" => {
            let DVal::Real(q) = value::of_literal(value)? else {
                return Err(Unsupported::Facet(facet.to_string()));
            };
            let k = u64::try_from(q.floor()).map_err(|_| Unsupported::Large)?;
            if !q.is_int() || k == u64::MAX {
                return Err(Unsupported::Large);
            }
            let l = match name {
                "length" => Disc::point(k),
                "minLength" => Disc::range(End::At(k), End::Max),
                _ => Disc::range(End::At(0), End::At(k)),
            };
            let zero = |bit: u16| if l.contains(&0) { bit } else { 0 };
            match fam {
                Fam::Plain => {
                    n.cells = std::array::from_fn(|i| l.meet(&len_from(CELL_MIN[i])));
                    let strs = Tiny::STR
                        .iter()
                        .filter(|(_, _, len)| l.contains(len))
                        .fold(0, |a, (b, _, _)| a | b);
                    n.tiny = (n.tiny & !Tiny::PLAIN) | strs;
                    n.tags = Tags::uniform(l);
                }
                Fam::Uri => {
                    n.uri = l.meet(&len_from(BIN_MIN));
                    n.tiny = (n.tiny & !Tiny::URI) | zero(Tiny::URI);
                }
                Fam::Binary => {
                    n.hex = l.meet(&len_from(BIN_MIN));
                    n.b64 = n.hex.clone();
                    let bits = Tiny::HEX | Tiny::B64;
                    n.tiny = (n.tiny & !bits) | zero(bits);
                }
                _ => return Err(Unsupported::Facet(facet.to_string())),
            }
            Ok(n)
        }
        "pattern" => Err(Unsupported::Pattern),
        _ if facet == vocab::RDF_LANG_RANGE && fam == Fam::Plain => {
            let r = match value::of_literal(value)? {
                DVal::Str(s) => s.to_ascii_lowercase(),
                _ => return Err(Unsupported::Facet(facet.to_string())),
            };
            n.cells = std::array::from_fn(|_| Disc::empty());
            n.tiny &= !Tiny::PLAIN;
            if r == "*" {
                n.tags = Tags::uniform(len_all());
            } else if r.contains('*') {
                // OWL 2 §4.3 supports basic language ranges only
                return Err(Unsupported::Facet(facet.to_string()));
            } else {
                let mut t = Tags::uniform(Disc::empty());
                t.over.insert(r, len_all());
                n.tags = t;
            }
            Ok(n)
        }
        _ => Err(Unsupported::Facet(facet.to_string())),
    }
}

/// Whether a facet pair can restrict `xsd:dateTime` or `xsd:dateTimeStamp`: a bound
/// whose value is a date-time, with an offset when it restricts `xsd:dateTimeStamp`.
fn time_facet(dt: &str, facet: &str, value: &Literal) -> Result<(), Unsupported> {
    let bad = || Unsupported::Facet(facet.to_string());
    let name = facet.strip_prefix(vocab::XSD).ok_or_else(bad)?;
    let vt = value.datatype();
    let time = |l: &str| vt.strip_prefix(vocab::XSD) == Some(l);
    if !matches!(
        name,
        "minInclusive" | "minExclusive" | "maxInclusive" | "maxExclusive"
    ) || !(time("dateTime") || time("dateTimeStamp"))
    {
        return Err(bad());
    }
    match value::of_literal(value)? {
        DVal::Time(_, None) if dt.ends_with("#dateTimeStamp") => Err(bad()),
        _ => Ok(()),
    }
}

/// An interned range.
pub type Rid = u32;

/// The interned ranges of one run, with the meets, joins and complements already
/// computed.
#[derive(Clone)]
pub struct Ranges {
    nfs: Vec<Nf>,
    ids: FxHashMap<Nf, Rid>,
    meets: FxHashMap<(Rid, Rid), Rid>,
    joins: FxHashMap<(Rid, Rid), Rid>,
    nots: FxHashMap<Rid, Rid>,
    sizes: FxHashMap<Rid, Size>,
    bytes: usize,
}

impl Ranges {
    pub const EMPTY: Rid = 0;
    pub const FULL: Rid = 1;

    pub fn new() -> Ranges {
        let mut r = Ranges {
            nfs: Vec::new(),
            ids: FxHashMap::default(),
            meets: FxHashMap::default(),
            joins: FxHashMap::default(),
            nots: FxHashMap::default(),
            sizes: FxHashMap::default(),
            bytes: 0,
        };
        r.intern(Nf::empty());
        r.intern(Nf::full());
        r
    }
    pub fn intern(&mut self, nf: Nf) -> Rid {
        if let Some(id) = self.ids.get(&nf) {
            return *id;
        }
        let id = self.nfs.len() as Rid;
        self.bytes += 2 * nf.bytes();
        self.nfs.push(nf.clone());
        self.ids.insert(nf, id);
        id
    }
}

impl Default for Ranges {
    fn default() -> Ranges {
        Ranges::new()
    }
}

impl Ranges {
    pub fn nf(&self, id: Rid) -> &Nf {
        &self.nfs[id as usize]
    }
    /// The bytes the table retains, for the run's memory budget.
    pub fn bytes(&self) -> usize {
        self.bytes + 48 * (self.meets.len() + self.joins.len() + self.nots.len() + self.sizes.len())
    }
    pub fn meet(&mut self, a: Rid, b: Rid) -> Rid {
        let k = (a.min(b), a.max(b));
        if let Some(r) = self.meets.get(&k) {
            return *r;
        }
        let r = self.nf(a).meet(self.nf(b));
        let r = self.intern(r);
        self.meets.insert(k, r);
        r
    }
    pub fn join(&mut self, a: Rid, b: Rid) -> Rid {
        let k = (a.min(b), a.max(b));
        if let Some(r) = self.joins.get(&k) {
            return *r;
        }
        let r = self.nf(a).join(self.nf(b));
        let r = self.intern(r);
        self.joins.insert(k, r);
        r
    }
    pub fn not(&mut self, a: Rid) -> Rid {
        if let Some(r) = self.nots.get(&a) {
            return *r;
        }
        let r = self.nf(a).not();
        let r = self.intern(r);
        self.nots.insert(a, r);
        self.nots.insert(r, a);
        r
    }
    pub fn size(&mut self, a: Rid) -> Size {
        if let Some(s) = self.sizes.get(&a) {
            return *s;
        }
        let s = self.nf(a).size();
        self.sizes.insert(a, s);
        s
    }
    /// Whether the range is empty, or `None` when its size is not known well enough.
    pub fn is_empty(&mut self, a: Rid) -> Option<bool> {
        let s = self.size(a);
        if s.lo > 0 {
            Some(false)
        } else if s.hi == Some(0) {
            Some(true)
        } else {
            None
        }
    }
    /// Whether every value of `a` is a value of `b`, or `None` when not known.
    pub fn within(&mut self, a: Rid, b: Rid) -> Option<bool> {
        let nb = self.not(b);
        let m = self.meet(a, nb);
        self.is_empty(m)
    }
    pub fn contains(&self, a: Rid, v: &DVal) -> bool {
        self.nf(a).contains(v)
    }
    pub fn value(&mut self, v: &DVal) -> Rid {
        self.intern(Nf::point(v))
    }
    pub fn literal(&mut self, l: &Literal) -> Result<Rid, Unsupported> {
        let v = value::of_literal(l)?;
        Ok(self.value(&v))
    }
    pub fn datatype(&mut self, iri: &str) -> Result<Rid, Unsupported> {
        let (n, _) = datatype_nf(iri)?;
        Ok(self.intern(n))
    }
    /// The range of a data range of the ontology.
    pub fn of(&mut self, r: &DataRange) -> Result<Rid, Unsupported> {
        match self.of_stopping(r, &|| false) {
            Ok(x) => Ok(x),
            Err(Fail::Unsupported(u)) => Err(u),
            Err(Fail::Stopped) => unreachable!("the flag is never set"),
        }
    }

    /// The range of a data range of the ontology, stopping once `stop` returns true. The
    /// flag is read before each operand, since an enumeration of n literals costs n joins.
    pub fn of_stopping(&mut self, r: &DataRange, stop: &dyn Fn() -> bool) -> Result<Rid, Fail> {
        self.of_in(r, None, 0, stop)
    }

    /// [`Ranges::of_stopping`], with each datatype that `defs` defines standing for its
    /// definition. Each definition is converted once, so definitions shared by many
    /// ranges or nested in each other cost their size and not the size of their
    /// expansion. `defs` must be used with this table only, since it keeps its ids.
    pub fn of_defined(
        &mut self,
        r: &DataRange,
        defs: &mut Defs,
        stop: &dyn Fn() -> bool,
    ) -> Result<Rid, Fail> {
        self.of_in(r, Some(defs), 0, stop)
    }

    fn of_in(
        &mut self,
        r: &DataRange,
        mut defs: Option<&mut Defs>,
        depth: usize,
        stop: &dyn Fn() -> bool,
    ) -> Result<Rid, Fail> {
        if stop() {
            return Err(Fail::Stopped);
        }
        Ok(match r {
            DataRange::Datatype(d) => match defs {
                Some(defs) if defs.map.contains_key(d) => {
                    match defs.memo.get(d) {
                        Some(Some(Ok(id))) => return Ok(*id),
                        Some(Some(Err(u))) => return Err(Fail::Unsupported(u.clone())),
                        // a definition that names itself, which OWL 2 does not allow
                        Some(None) => {
                            return Err(Fail::Unsupported(Unsupported::Datatype(
                                d.0.as_str().to_string(),
                            )));
                        }
                        None => {}
                    }
                    if depth >= Defs::MAX_DEPTH {
                        return Err(Fail::Unsupported(Unsupported::Datatype(
                            d.0.as_str().to_string(),
                        )));
                    }
                    defs.memo.insert(d.clone(), None);
                    let x = defs.map[d].clone();
                    match self.of_in(&x, Some(&mut *defs), depth + 1, stop) {
                        Ok(id) => {
                            defs.memo.insert(d.clone(), Some(Ok(id)));
                            id
                        }
                        Err(Fail::Unsupported(u)) => {
                            defs.memo.insert(d.clone(), Some(Err(u.clone())));
                            return Err(Fail::Unsupported(u));
                        }
                        Err(Fail::Stopped) => {
                            defs.memo.remove(d);
                            return Err(Fail::Stopped);
                        }
                    }
                }
                _ => self.datatype(d.0.as_str())?,
            },
            DataRange::IntersectionOf(rs) => {
                let mut acc = Ranges::FULL;
                for x in rs {
                    let x = self.of_in(x, defs.as_deref_mut(), depth, stop)?;
                    acc = self.meet(acc, x);
                }
                acc
            }
            DataRange::UnionOf(rs) => {
                let mut acc = Ranges::EMPTY;
                for x in rs {
                    let x = self.of_in(x, defs.as_deref_mut(), depth, stop)?;
                    acc = self.join(acc, x);
                }
                acc
            }
            DataRange::ComplementOf(x) => {
                let x = self.of_in(x, defs, depth, stop)?;
                self.not(x)
            }
            DataRange::OneOf(ls) => {
                let mut acc = Ranges::EMPTY;
                for l in ls {
                    if stop() {
                        return Err(Fail::Stopped);
                    }
                    let x = self.literal(l)?;
                    acc = self.join(acc, x);
                }
                acc
            }
            DataRange::DatatypeRestriction(d, fs) => {
                let dt = d.0.as_str();
                let (mut n, fam) = datatype_nf(dt)?;
                for f in fs {
                    let facet = f.facet.as_str();
                    if fam == Fam::Time {
                        // checked here, since `datatypes::facet_problem` reads date-times
                        // with a parser that refuses fractions past eighteen digits
                        time_facet(dt, facet, &f.value)?;
                    } else if let Some(p) = datatypes::facet_problem(dt, facet, &f.value) {
                        return Err(Fail::Unsupported(Unsupported::Facet(p)));
                    }
                    n = n.meet(&facet_nf(fam, facet, &f.value)?);
                }
                self.intern(n)
            }
        })
    }
}

/// The datatype definitions of a run, with the range of each defined datatype in one
/// [`Ranges`] table once [`Ranges::of_defined`] has converted it.
#[derive(Clone, Debug, Default)]
pub struct Defs {
    map: FxHashMap<Datatype, DataRange>,
    /// `None` while the definition is being converted
    memo: FxHashMap<Datatype, Option<Result<Rid, Unsupported>>>,
}

impl Defs {
    /// The deepest nesting of definitions a conversion follows.
    pub const MAX_DEPTH: usize = 64;

    pub fn new(map: FxHashMap<Datatype, DataRange>) -> Defs {
        Defs {
            map,
            memo: FxHashMap::default(),
        }
    }

    pub fn map(&self) -> &FxHashMap<Datatype, DataRange> {
        &self.map
    }

    /// The definitions without the ranges converted so far, for another table.
    pub fn unconverted(&self) -> Defs {
        Defs::new(self.map.clone())
    }

    /// The bytes the converted ranges take, the definitions aside.
    pub fn bytes(&self) -> usize {
        self.memo.capacity()
            * (std::mem::size_of::<(Datatype, Option<Result<Rid, Unsupported>>)>() + 1)
    }
}

/// Why [`Ranges::of_stopping`] gave no range.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Fail {
    /// the range is outside the checker, and its axiom is used in part
    Unsupported(Unsupported),
    /// the caller's stop flag was set
    Stopped,
}

impl From<Unsupported> for Fail {
    fn from(u: Unsupported) -> Fail {
        Fail::Unsupported(u)
    }
}

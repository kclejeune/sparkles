//! Patterns and ranges for the draft: what string values and numbers or dates have in
//! common, found from the values of each instance and counted exactly, so that a
//! constraint drafted below full support excludes the instances the counts say it does.
//!
//! A pattern is drafted for a property whose values are all `xsd:string` literals (a
//! value of any other kind could match or miss the pattern in a way the counts do not
//! follow). It is a fixed prefix (`^ISBN-`, from the characters before the first digit)
//! or the character-class skeleton most values share (`^[A-Z]{2}[0-9]{4}$`, from at most
//! 64 distinct skeletons per property). A range is drafted for a property
//! whose values are all inline numbers of one datatype or all inline dates or dateTimes
//! that agree on having a timezone, from the smallest and largest value of each instance.

use super::{ConstraintDraft, ConstraintValue, Sets, constraint, passes};
use crate::id::{Id, Tag};
use crate::store::Snapshot;
use oxrdf::Term;
use oxsdatatypes::{Date, DateTime, Decimal};
use rustc_hash::FxHashMap;
use std::cmp::Ordering;

/// The largest number of characters of a value a pattern looks at.
const MAX_LEN: usize = 64;

/// The longest fixed prefix.
const MAX_HEAD: usize = 24;

/// Distinct skeletons and prefixes kept per predicate; past it the predicate gets no
/// pattern.
const MAX_NAMES: usize = 4096;

/// Instances whose smallest and largest values are kept per property for a range.
const MAX_RANGE_INSTANCES: usize = 200_000;

/// Punctuation a pattern may contain besides letters and digits.
const SAFE: &str = " -_.,:;/@#+()&%=!?'";

fn safe_punct(c: char) -> bool {
    SAFE.contains(c)
}

/// The text of the character in a pattern: letters and digits as they are, punctuation as
/// a one-character class (so nothing needs an escape).
fn literal(c: char, out: &mut String) {
    if c.is_ascii_alphanumeric() {
        out.push(c);
    } else {
        out.push('[');
        out.push(c);
        out.push(']');
    }
}

/// The character-class skeleton of a string, as the body of an anchored regular
/// expression: runs of upper-case letters, lower-case letters and digits become
/// `[A-Z]{n}`, `[a-z]{n}` and `[0-9]{n}`, and a few punctuation marks stand for
/// themselves. `None` for an empty or long string, or one with any other character
/// (which no skeleton matches).
pub(super) fn skeleton(s: &str) -> Option<String> {
    if s.is_empty() || s.chars().count() > MAX_LEN {
        return None;
    }
    let class = |c: char| match c {
        'A'..='Z' => Some(("[A-Z]", 0)),
        'a'..='z' => Some(("[a-z]", 1)),
        '0'..='9' => Some(("[0-9]", 2)),
        _ => None,
    };
    let mut out = String::from("^");
    let mut run: Option<(&str, u8, usize)> = None;
    let flush = |run: &mut Option<(&str, u8, usize)>, out: &mut String| {
        if let Some((text, _, n)) = run.take() {
            out.push_str(text);
            if n > 1 {
                out.push_str(&format!("{{{n}}}"));
            }
        }
    };
    for c in s.chars() {
        match class(c) {
            Some((text, k)) => match &mut run {
                Some((_, kind, n)) if *kind == k => *n += 1,
                _ => {
                    flush(&mut run, &mut out);
                    run = Some((text, k, 1));
                }
            },
            None if safe_punct(c) => {
                flush(&mut run, &mut out);
                literal(c, &mut out);
            }
            None => return None,
        }
    }
    flush(&mut run, &mut out);
    out.push('$');
    Some(out)
}

/// The characters of a string before its first digit or character a pattern cannot
/// spell, at most [`MAX_HEAD`]. A string starts with a prefix of such characters exactly
/// when its head does.
pub(super) fn head(s: &str) -> String {
    s.chars()
        .take_while(|c| c.is_ascii_alphabetic() || safe_punct(*c))
        .take(MAX_HEAD)
        .collect()
}

/// The regular expression for a fixed prefix.
fn prefix_pattern(p: &str) -> String {
    let mut out = String::from("^");
    for c in p.chars() {
        literal(c, &mut out);
    }
    out
}

/// The skeletons and prefixes of the string values of one predicate, interned.
#[derive(Default)]
pub(super) struct PatNames {
    pub skels: Vec<String>,
    skel_index: FxHashMap<String, u32>,
    pub heads: Vec<String>,
    head_index: FxHashMap<String, u32>,
    /// more names than [`MAX_NAMES`] were seen
    pub overflow: bool,
}

impl PatNames {
    /// The ids (+ 1; 0: none) of the skeleton and the head of a string value.
    pub fn intern(&mut self, s: &str) -> (u32, u32) {
        let skel = skeleton(s).map_or(0, |k| match self.skel_index.get(&k) {
            Some(&i) => i + 1,
            None if self.skels.len() >= MAX_NAMES => {
                self.overflow = true;
                0
            }
            None => {
                let i = self.skels.len() as u32;
                self.skels.push(k.clone());
                self.skel_index.insert(k, i);
                i + 1
            }
        });
        let h = head(s);
        let head = match self.head_index.get(&h) {
            Some(&i) => i + 1,
            None if self.heads.len() >= MAX_NAMES => {
                self.overflow = true;
                0
            }
            None => {
                let i = self.heads.len() as u32;
                self.heads.push(h.clone());
                self.head_index.insert(h, i);
                i + 1
            }
        };
        (skel, head)
    }
}

/// The pattern constraint for the strings of a property, if one reaches the support.
/// `skels` and `heads` hold, per instance, the skeletons (none when a value has no
/// skeleton) and the prefixes of its values.
pub(super) fn decide_pattern(
    names: &PatNames,
    skels: Option<&Sets<u32>>,
    heads: Option<&Sets<u32>>,
    w: u64,
    support: f64,
) -> Option<(bool, ConstraintDraft)> {
    if names.overflow {
        return None;
    }
    // the skeleton most instances share
    let mut best: Option<(bool, ConstraintDraft)> = None;
    if let Some(sets) = skels {
        let order: Vec<u32> = sets.by_use().into_iter().map(|(v, _)| v).take(1).collect();
        if let Some(&top) = order.first() {
            let n = sets.covered(&order)[1];
            if n >= 2 {
                let text = names.skels[top as usize].clone();
                let c = constraint("pattern", ConstraintValue::Text(text), w, n);
                let ok = passes(n, w, support);
                if ok {
                    return Some((true, c));
                }
                best = Some((false, c));
            }
        }
    }
    // the longest prefix that enough instances share
    if let Some(sets) = heads {
        let order: Vec<u32> = sets.by_use().into_iter().map(|(v, _)| v).collect();
        let mut lcp: Option<String> = None;
        for &h in order.iter().take(8) {
            let next = &names.heads[h as usize];
            let p = match &lcp {
                None => next.clone(),
                Some(p) => common_prefix(p, next),
            };
            if p.chars().count() < 2 {
                break;
            }
            let n: u64 = sets
                .sigs
                .iter()
                .filter(|(sig, _)| {
                    sig.iter()
                        .all(|v| names.heads[*v as usize].starts_with(p.as_str()))
                })
                .map(|(_, n)| *n)
                .sum();
            if n >= 2 {
                let c = constraint("pattern", ConstraintValue::Text(prefix_pattern(&p)), w, n);
                if passes(n, w, support) {
                    return Some((true, c));
                }
                if best.as_ref().is_none_or(|(_, b)| b.satisfied < n) {
                    best = Some((false, c));
                }
            }
            lcp = Some(p);
        }
    }
    best
}

fn common_prefix(a: &str, b: &str) -> String {
    a.chars()
        .zip(b.chars())
        .take_while(|(x, y)| x == y)
        .map(|(x, _)| x)
        .collect()
}

// ---------------------------------------------------------------------- ranges ------

/// A value of a range, comparable with the others of its family.
#[derive(Clone, Copy, Debug)]
pub(super) enum Key {
    Int(i64),
    Dec(Decimal),
    Dbl(f64),
    DateTime(DateTime),
    Date(Date),
}

impl Key {
    fn cmp(&self, other: &Key) -> Ordering {
        match (self, other) {
            (Key::Int(a), Key::Int(b)) => a.cmp(b),
            (Key::Dec(a), Key::Dec(b)) => a.cmp(b),
            (Key::Dbl(a), Key::Dbl(b)) => a.partial_cmp(b).unwrap_or(Ordering::Equal),
            (Key::DateTime(a), Key::DateTime(b)) => a.partial_cmp(b).unwrap_or(Ordering::Equal),
            (Key::Date(a), Key::Date(b)) => a.partial_cmp(b).unwrap_or(Ordering::Equal),
            _ => Ordering::Equal,
        }
    }

    /// The family of the key: its datatype, and for dates whether it has a timezone.
    pub fn family(&self) -> u8 {
        match self {
            Key::Int(_) => 0,
            Key::Dec(_) => 1,
            Key::Dbl(_) => 2,
            Key::DateTime(d) => 3 + d.timezone_offset().is_some() as u8,
            Key::Date(d) => 5 + d.timezone_offset().is_some() as u8,
        }
    }
}

/// The key of an inline number or date (`None` for anything else, and for NaN).
pub(super) fn key_of(snap: &Snapshot, id: Id) -> Option<Key> {
    match id.tag() {
        Tag::Int => Some(Key::Int(id.as_i64())),
        Tag::Double => {
            let d = id.as_f64();
            (!d.is_nan()).then_some(Key::Dbl(d))
        }
        Tag::Decimal | Tag::DateTime | Tag::Date => {
            let Some(Term::Literal(l)) = snap.term(id) else {
                return None;
            };
            match id.tag() {
                Tag::Decimal => l.value().parse().ok().map(Key::Dec),
                Tag::DateTime => l.value().parse().ok().map(Key::DateTime),
                _ => l.value().parse().ok().map(Key::Date),
            }
        }
        _ => None,
    }
}

/// The smallest and largest value of an instance, with the ids that hold them.
#[derive(Clone, Copy, Debug)]
pub(super) struct Span {
    pub min: (Key, u64),
    pub max: (Key, u64),
}

impl Span {
    /// The span of one value.
    pub fn of(k: Key, id: u64) -> Span {
        Span {
            min: (k, id),
            max: (k, id),
        }
    }

    /// Include another value; `false` when it is not of the span's family.
    pub fn add(&mut self, k: Key, id: u64) -> bool {
        if k.family() != self.min.0.family() {
            return false;
        }
        if k.cmp(&self.min.0) == Ordering::Less {
            self.min = (k, id);
        }
        if k.cmp(&self.max.0) == Ordering::Greater {
            self.max = (k, id);
        }
        true
    }
}

/// The spans of the instances of one class and property.
#[derive(Default)]
pub(super) struct RangeAcc {
    family: Option<u8>,
    spans: Vec<Span>,
}

impl RangeAcc {
    /// Add an instance; `false` when the property has no range (another family, or too
    /// many instances).
    pub fn add(&mut self, s: &Span) -> bool {
        let f = s.min.0.family();
        if *self.family.get_or_insert(f) != f || self.spans.len() >= MAX_RANGE_INSTANCES {
            return false;
        }
        self.spans.push(*s);
        true
    }
}

/// The bounds of a range: the ids that hold them, and how many instances keep every
/// value within each.
pub(super) struct Bounds {
    pub min: u64,
    pub kept_min: u64,
    pub max: u64,
    pub kept_max: u64,
}

/// The bounds a range drafts at `support`: the smallest value that enough instances keep
/// all of their values above, and the largest they keep below.
pub(super) fn decide_range(acc: &RangeAcc, w: u64, support: f64) -> Option<Bounds> {
    if acc.spans.is_empty() || acc.spans.len() as u64 != w {
        return None;
    }
    let n = acc.spans.len();
    // the instances that must stay inside
    let need = (((support * n as f64) - 1e-9).ceil() as usize).clamp(1, n);
    let mut mins: Vec<&Span> = acc.spans.iter().collect();
    mins.sort_by(|a, b| a.min.0.cmp(&b.min.0));
    let lo = mins[n - need].min;
    let kept_min = acc
        .spans
        .iter()
        .filter(|s| s.min.0.cmp(&lo.0) != Ordering::Less)
        .count() as u64;
    let mut maxs: Vec<&Span> = acc.spans.iter().collect();
    maxs.sort_by(|a, b| a.max.0.cmp(&b.max.0));
    let hi = maxs[need - 1].max;
    let kept_max = acc
        .spans
        .iter()
        .filter(|s| s.max.0.cmp(&hi.0) != Ordering::Greater)
        .count() as u64;
    Some(Bounds {
        min: lo.1,
        kept_min,
        max: hi.1,
        kept_max,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skeletons() {
        assert_eq!(skeleton("AB1234").as_deref(), Some("^[A-Z]{2}[0-9]{4}$"));
        assert_eq!(
            skeleton("Person 5").as_deref(),
            Some("^[A-Z][a-z]{5}[ ][0-9]$")
        );
        assert_eq!(skeleton("a-b").as_deref(), Some("^[a-z][-][a-z]$"));
        assert_eq!(skeleton("é"), None);
        assert_eq!(skeleton(""), None);
        assert_eq!(skeleton("a\n"), None);
        assert_eq!(skeleton(&"a".repeat(65)), None);
    }

    #[test]
    fn heads_and_prefixes() {
        assert_eq!(head("ISBN-0123"), "ISBN-");
        assert_eq!(head("Boss"), "Boss");
        assert_eq!(head("12"), "");
        assert_eq!(prefix_pattern("ISBN-"), "^ISBN[-]");
        assert_eq!(common_prefix("Person ", "Person5"), "Person");
    }
}

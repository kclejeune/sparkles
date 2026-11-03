//! A search over the entity index: the tiers in order, each ranked, until the page is full.

use super::index::{ANY_GRAPH, Field, Index, LOCAL_NAME, PROPS, TYPES};
use super::text::{chars, normalize, normalize_mapped, source_ranges, words};
use super::{MatchKind, SearchRequest};
use crate::id::Id;
use crate::store::Snapshot;
use levenshtein_automata::{DFA, Distance, LevenshteinAutomatonBuilder, SINK_STATE};
use oxrdf::{BlankNode, Literal, NamedNode, Term};
use rustc_hash::{FxHashMap, FxHashSet};
use smallvec::SmallVec;
use std::cell::RefCell;
use std::collections::{BTreeMap, BinaryHeap};
use std::ops::{Bound, Range};
use std::sync::OnceLock;

/// The most IRIs an IRI-prefix query ranks.
const MAX_IRI_PREFIX: usize = 10_000;

/// The edits a query word may take: none up to 2 characters, one up to 5 and two from 6
/// for a whole word, and for a word typed so far (a prefix), none up to 3 characters, one
/// up to 7 and two from 8.
pub fn max_edits(chars: usize, prefix: bool) -> u8 {
    match (prefix, chars) {
        (false, 0..=2) | (true, 0..=3) => 0,
        (false, 3..=5) | (true, 4..=7) => 1,
        _ => 2,
    }
}

fn builder(d: u8) -> &'static LevenshteinAutomatonBuilder {
    static B: [OnceLock<LevenshteinAutomatonBuilder>; 2] = [OnceLock::new(), OnceLock::new()];
    B[(d - 1) as usize].get_or_init(|| LevenshteinAutomatonBuilder::new(d, true))
}

fn dfa(q: &str, d: u8, prefix: bool) -> DFA {
    let b = builder(d);
    if prefix {
        b.build_prefix_dfa(q)
    } else {
        b.build_dfa(q)
    }
}

/// The smallest key above every key that starts with `p`, or `None` when there is none.
fn successor(p: &[u8]) -> Option<Vec<u8>> {
    let mut s = p.to_vec();
    while let Some(b) = s.pop() {
        if b < u8::MAX {
            s.push(b + 1);
            return Some(s);
        }
    }
    None
}

/// The keys of `map` that `dfa` accepts, with their distance, in key order. Keys whose
/// prefix the automaton rejects are skipped together. With `settle`, the automaton is a
/// prefix automaton whose distance no longer changes once that many characters are read,
/// so every key under such a prefix is taken without reading it.
fn walk<'m, V>(
    map: &'m BTreeMap<Box<[u8]>, V>,
    dfa: &DFA,
    settle: Option<usize>,
    mut f: impl FnMut(&'m [u8], &'m V, u8),
) {
    let mut states: Vec<u32> = vec![dfa.initial_state()];
    let mut prev: Vec<u8> = Vec::new();
    let mut from: Bound<Vec<u8>> = Bound::Unbounded;
    'seek: loop {
        let range = map.range::<[u8], _>((from.as_ref().map(|v| v.as_slice()), Bound::Unbounded));
        for (k, v) in range {
            let k: &[u8] = k;
            let lcp = prev
                .iter()
                .zip(k)
                .take_while(|(a, b)| a == b)
                .count()
                .min(states.len() - 1);
            states.truncate(lcp + 1);
            let mut dead = None;
            let mut settled = None;
            let mut chars = k[..lcp].iter().filter(|b| (**b & 0xC0) != 0x80).count();
            for (i, &b) in k.iter().enumerate().skip(lcp) {
                let s = dfa.transition(states[i], b);
                if s == SINK_STATE {
                    dead = Some(i);
                    break;
                }
                states.push(s);
                if (b & 0xC0) != 0x80 {
                    chars += 1;
                }
                let boundary = k.get(i + 1).is_none_or(|n| (n & 0xC0) != 0x80);
                if let Some(n) = settle
                    && chars >= n
                    && boundary
                    && let Distance::Exact(d) = dfa.distance(s)
                {
                    settled = Some((i + 1, d));
                    break;
                }
            }
            prev.clear();
            if let Some((depth, d)) = settled {
                // every key under this prefix has distance d
                let p = &k[..depth];
                let hi = successor(p);
                let sub = match &hi {
                    Some(h) => {
                        map.range::<[u8], _>((Bound::Included(p), Bound::Excluded(h.as_slice())))
                    }
                    None => map.range::<[u8], _>((Bound::Included(p), Bound::Unbounded)),
                };
                for (k2, v2) in sub {
                    f(k2, v2, d);
                }
                prev.extend_from_slice(p);
                states.truncate(depth + 1);
                match hi {
                    Some(next) => {
                        from = Bound::Included(next);
                        continue 'seek;
                    }
                    None => return,
                }
            }
            match dead {
                Some(i) => {
                    prev.extend_from_slice(&k[..i]);
                    match successor(&k[..=i]) {
                        Some(next) => {
                            from = Bound::Included(next);
                            continue 'seek;
                        }
                        None => return,
                    }
                }
                None => {
                    prev.extend_from_slice(k);
                    if let Distance::Exact(d) = dfa.distance(*states.last().unwrap_or(&0)) {
                        f(k, v, d);
                    }
                }
            }
        }
        return;
    }
}

/// The order of the hits of one search, compared field by field.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub(super) struct Rank<'a> {
    pub tier: u8,
    pub distance: u8,
    pub field: u8,
    pub deprecated: bool,
    pub sub: u8,
    pub lang: u8,
    pub len: u32,
    pub text: &'a str,
    pub iri: &'a str,
}

/// One ranked hit: its entity, the value that matched (`None` for the IRI) and how.
#[derive(Clone, Debug)]
pub(super) struct Candidate<'a> {
    pub entity: u32,
    pub value: Option<u32>,
    pub rank: Rank<'a>,
    pub kind: MatchKind,
    /// a fuzzy match of the whole text's beginning, rather than of its words
    pub whole: bool,
}

/// Which graphs, kinds and triples the caller may see.
pub(super) struct Scope<'a> {
    pub ix: &'a Index,
    /// by graph id of the index
    pub graphs: Vec<bool>,
    /// the kind bits a hit must have one of (0: any)
    pub kinds: u16,
    /// the caller's view, when protections hide some triples: every fact is checked
    /// against it
    pub view: Option<&'a Snapshot>,
    entity_memo: RefCell<FxHashMap<u32, bool>>,
    fact_memo: RefCell<FxHashMap<(u32, u32), bool>>,
}

impl<'a> Scope<'a> {
    pub fn new(ix: &'a Index, graphs: Vec<bool>, kinds: u16, view: Option<&'a Snapshot>) -> Self {
        Scope {
            ix,
            graphs,
            kinds,
            view,
            entity_memo: Default::default(),
            fact_memo: Default::default(),
        }
    }

    fn graph(&self, g: u32) -> bool {
        g == ANY_GRAPH || self.graphs.get(g as usize).copied().unwrap_or(false)
    }

    fn graph_id(&self, view: &Snapshot, g: u32) -> Option<Id> {
        let name = &self.ix.graphs[g as usize];
        if name.is_empty() {
            Some(Id::DEFAULT_GRAPH)
        } else {
            view.lookup_iri(name)
        }
    }

    fn holds(&self, view: &Snapshot, s: &Term, p: &str, o: &Term, g: u32) -> bool {
        let q = (|| {
            Some([
                view.lookup_term(s)?,
                view.lookup_iri(p)?,
                view.lookup_term(o)?,
                self.graph_id(view, g)?,
            ])
        })();
        q.is_some_and(|q| view.contains(&q).unwrap_or(false))
    }

    /// Whether type fact `t` of entity `e` is visible.
    fn type_ok(&self, e: u32, g: u32, t: u8) -> bool {
        if !self.graph(g) {
            return false;
        }
        let Some(view) = self.view else {
            return true;
        };
        let (ns, local, _) = TYPES[t as usize];
        let o = Term::NamedNode(NamedNode::new_unchecked(format!("{ns}{local}")));
        self.holds(
            view,
            &iri_term(&self.ix.entities[e as usize].iri),
            "http://www.w3.org/1999/02/22-rdf-syntax-ns#type",
            &o,
            g,
        )
    }

    /// The kind bits of entity `e` the caller sees.
    pub fn kinds_of(&self, e: u32) -> u16 {
        let ent = &self.ix.entities[e as usize];
        ent.types
            .iter()
            .filter(|(g, t)| self.type_ok(e, *g, *t))
            .fold(0, |k, (_, t)| k | TYPES[*t as usize].2.bit())
    }

    /// Whether entity `e` is visible with a kind the filter keeps.
    pub fn entity_ok(&self, e: u32) -> bool {
        let ok = |k: u16| k != 0 && (self.kinds == 0 || k & self.kinds != 0);
        if self.view.is_none() {
            return ok(self.kinds_of(e));
        }
        if let Some(&r) = self.entity_memo.borrow().get(&e) {
            return r;
        }
        let r = ok(self.kinds_of(e));
        self.entity_memo.borrow_mut().insert(e, r);
        r
    }

    /// Whether `owl:deprecated` holds for entity `e` in a visible graph.
    pub fn deprecated(&self, e: u32) -> bool {
        let ent = &self.ix.entities[e as usize];
        ent.deprecated.iter().any(|(g, lit)| {
            self.graph(*g)
                && match self.view {
                    None => true,
                    Some(view) => {
                        let o = parse_literal(lit);
                        o.is_some_and(|o| {
                            self.holds(
                                view,
                                &iri_term(&ent.iri),
                                "http://www.w3.org/2002/07/owl#deprecated",
                                &o,
                                *g,
                            )
                        })
                    }
                }
        })
    }

    /// Whether value `v` is visible.
    pub fn value_ok(&self, v: u32) -> bool {
        let x = &self.ix.values[v as usize];
        if !x.live() || !self.graph(x.graph) {
            return false;
        }
        let Some(view) = self.view else {
            return true;
        };
        if x.prop == LOCAL_NAME {
            return true;
        }
        if let Some(&ok) = self.fact_memo.borrow().get(&(v, 0)) {
            return ok;
        }
        let ix = self.ix;
        let lit = {
            let lang = &ix.langs[x.lang as usize];
            let dt = &ix.datatypes[x.datatype as usize];
            if !lang.is_empty() {
                Literal::new_language_tagged_literal_unchecked(&*x.text, &**lang)
            } else if !dt.is_empty() {
                Literal::new_typed_literal(&*x.text, NamedNode::new_unchecked(&**dt))
            } else {
                Literal::new_simple_literal(&*x.text)
            }
        };
        let iri = iri_term(&ix.entities[x.entity as usize].iri);
        let p = PROPS[x.prop as usize].iri();
        let ok = if x.node == super::index::NO_NODE {
            self.holds(view, &iri, &p, &Term::Literal(lit), x.graph)
        } else {
            let node = node_term(&ix.nodes[x.node as usize].key);
            node.as_ref().is_some_and(|n| {
                self.holds(view, &iri, &p, n, x.graph)
                    && ix.nodes[x.node as usize].forms.iter().any(|f| {
                        *f.0 == *x.text
                            && f.1 == x.lang
                            && f.2 == x.datatype
                            && self.graph(f.3)
                            && self.holds(
                                view,
                                n,
                                "http://www.w3.org/2008/05/skos-xl#literalForm",
                                &Term::Literal(lit.clone()),
                                f.3,
                            )
                    })
            })
        };
        self.fact_memo.borrow_mut().insert((v, 0), ok);
        ok
    }
}

fn iri_term(iri: &str) -> Term {
    Term::NamedNode(NamedNode::new_unchecked(iri))
}

fn node_term(key: &str) -> Option<Term> {
    if let Some(iri) = key.strip_prefix('<').and_then(|k| k.strip_suffix('>')) {
        Some(Term::NamedNode(NamedNode::new_unchecked(iri)))
    } else {
        key.strip_prefix("_:")
            .map(|b| Term::BlankNode(BlankNode::new_unchecked(b)))
    }
}

/// The literal of an N-Triples string such as `"true"^^<…#boolean>`.
fn parse_literal(s: &str) -> Option<Term> {
    let rest = s.strip_prefix('"')?;
    let end = rest.rfind('"')?;
    let value = &rest[..end];
    let tail = &rest[end + 1..];
    Some(Term::Literal(match tail.strip_prefix("^^<") {
        Some(dt) => {
            Literal::new_typed_literal(value, NamedNode::new_unchecked(dt.trim_end_matches('>')))
        }
        None => match tail.strip_prefix('@') {
            Some(lang) => Literal::new_language_tagged_literal_unchecked(value, lang),
            None => Literal::new_simple_literal(value),
        },
    }))
}

/// The rank of each language tag of the index for the caller's preferences.
pub(super) fn language_ranks(ix: &Index, prefs: &[String]) -> Vec<u8> {
    let primary = |t: &str| t.split('-').next().unwrap_or("").to_string();
    let prefs: Vec<String> = prefs.iter().map(|p| p.to_ascii_lowercase()).collect();
    let n = prefs.len();
    ix.langs
        .iter()
        .map(|tag| {
            let tag: &str = tag;
            if n == 0 {
                return match tag {
                    "" => 0,
                    t if primary(t) == "en" => 0,
                    _ => 1,
                };
            }
            for (i, p) in prefs.iter().enumerate() {
                if tag == p {
                    return (2 * i).min(200) as u8;
                }
                if !tag.is_empty() && primary(tag) == primary(p) {
                    return (2 * i + 1).min(200) as u8;
                }
            }
            match tag {
                "" => (2 * n).min(200) as u8,
                t if primary(t) == "en" => (2 * n + 1).min(200) as u8,
                _ => (2 * n + 2).min(201) as u8,
            }
        })
        .collect()
}

/// What the query matched, kept for the highlights of its hits.
pub(super) struct Query {
    pub norm: String,
    pub terms: Vec<String>,
    /// whether the last term matches words by prefix
    pub last_prefix: bool,
    /// the words each term matched with edits
    pub fuzzy: Vec<FxHashSet<Box<[u8]>>>,
    /// the IRI the query names, as a full IRI or a prefixed name
    pub iri: Option<String>,
}

/// The IRI a query names: `<iri>`, an absolute IRI with `://` or `urn:`, or a prefixed
/// name with one of `prefixes`.
pub(super) fn query_iri(q: &str, prefixes: &BTreeMap<String, String>) -> Option<String> {
    let q = q.trim();
    if q.is_empty() || q.contains(char::is_whitespace) || !q.contains(':') {
        return None;
    }
    if let Some(i) = q.strip_prefix('<').and_then(|q| q.strip_suffix('>')) {
        return Some(i.to_string());
    }
    if q.contains("://") || q.starts_with("urn:") {
        return Some(q.to_string());
    }
    let (p, l) = q.split_once(':')?;
    prefixes.get(p).map(|ns| format!("{ns}{l}"))
}

struct Ranker<'a, 's> {
    scope: &'s Scope<'a>,
    langs: &'s [u8],
    /// the hits the page needs, all passes together
    need: usize,
    done: FxHashSet<u32>,
    out: Vec<Candidate<'a>>,
    tier: FxHashMap<u32, Candidate<'a>>,
    /// the first rank of each entity of this pass, the best ones the pass needs kept:
    /// a value ranked after all of them cannot reach the page
    bound: BinaryHeap<Rank<'a>>,
}

impl<'a, 's> Ranker<'a, 's> {
    /// Whether a value of rank `rank` is ranked after enough entities of the pass.
    fn beyond(&self, rank: &Rank<'a>) -> bool {
        let want = self.need.saturating_sub(self.out.len());
        self.bound.len() >= want && self.bound.peek().is_some_and(|b| rank > b)
    }

    fn offer(&mut self, v: u32, tier: u8, distance: u8, kind: MatchKind, whole: bool) {
        let ix = self.scope.ix;
        let x = &ix.values[v as usize];
        if !x.live() {
            return;
        }
        let e = x.entity;
        let prop = &PROPS[x.prop as usize];
        let rank = Rank {
            tier,
            distance,
            field: prop.field as u8,
            deprecated: !ix.entities[e as usize].deprecated.is_empty() && self.scope.deprecated(e),
            sub: prop.rank,
            lang: self.langs.get(x.lang as usize).copied().unwrap_or(u8::MAX),
            len: chars(&x.text) as u32,
            text: &x.text,
            iri: &ix.entities[e as usize].iri,
        };
        if self.beyond(&rank) || self.done.contains(&e) {
            return;
        }
        if self.tier.get(&e).is_some_and(|c| c.rank <= rank) {
            return;
        }
        if !self.scope.entity_ok(e) || !self.scope.value_ok(v) {
            return;
        }
        self.put(Candidate {
            entity: e,
            value: Some(v),
            rank,
            kind,
            whole,
        });
    }

    fn offer_iri(&mut self, e: u32, tier: u8, kind: MatchKind) {
        let iri: &'a str = &self.scope.ix.entities[e as usize].iri;
        let rank = Rank {
            tier,
            distance: 0,
            field: Field::ShortForm as u8,
            deprecated: self.scope.deprecated(e),
            sub: 0,
            lang: 0,
            len: chars(iri) as u32,
            text: iri,
            iri,
        };
        if self.beyond(&rank) || self.done.contains(&e) || !self.scope.entity_ok(e) {
            return;
        }
        self.put(Candidate {
            entity: e,
            value: None,
            rank,
            kind,
            whole: false,
        });
    }

    fn put(&mut self, c: Candidate<'a>) {
        match self.tier.get_mut(&c.entity) {
            Some(old) if old.rank <= c.rank => {}
            Some(old) => *old = c,
            None => {
                // an entity's first rank bounds its best one
                let want = self.need.saturating_sub(self.out.len());
                self.bound.push(c.rank);
                if self.bound.len() > want {
                    self.bound.pop();
                }
                self.tier.insert(c.entity, c);
            }
        }
    }

    /// Rank the pass's candidates after those of the earlier passes.
    fn close(&mut self) {
        let mut c: Vec<Candidate<'a>> = self.tier.drain().map(|(_, c)| c).collect();
        c.sort_unstable_by(|a, b| a.rank.cmp(&b.rank));
        for c in c {
            self.done.insert(c.entity);
            self.out.push(c);
        }
        self.bound.clear();
    }
}

/// The sorted ids of the values with a word that starts with `p`.
fn prefix_postings(ix: &Index, p: &[u8]) -> Vec<u32> {
    let hi = successor(p);
    let range = match &hi {
        Some(h) => ix
            .words
            .range::<[u8], _>((Bound::Included(p), Bound::Excluded(h.as_slice()))),
        None => ix
            .words
            .range::<[u8], _>((Bound::Included(p), Bound::Unbounded)),
    };
    let mut out: Vec<u32> = Vec::new();
    let mut lists = 0;
    for (_, l) in range {
        out.extend_from_slice(l);
        lists += 1;
    }
    if lists > 1 {
        out.sort_unstable();
        out.dedup();
    }
    out
}

/// The sorted ids in both sorted lists.
fn intersect(a: &[u32], b: &[u32]) -> Vec<u32> {
    let (small, big) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    let mut out = Vec::with_capacity(small.len());
    let mut lo = 0;
    for &x in small {
        match big[lo..].binary_search(&x) {
            Ok(i) => {
                out.push(x);
                lo += i + 1;
            }
            Err(i) => lo += i,
        }
        if lo >= big.len() {
            break;
        }
    }
    out
}

/// The values whose words cover every term, the last one by prefix when `last_prefix`.
fn word_candidates(ix: &Index, terms: &[String], last_prefix: bool) -> Vec<u32> {
    let Some((last, rest)) = terms.split_last() else {
        return Vec::new();
    };
    let mut lists: Vec<&Vec<u32>> = Vec::with_capacity(rest.len());
    for t in rest {
        match ix.words.get(t.as_bytes()) {
            Some(l) => lists.push(l),
            None => return Vec::new(),
        }
    }
    if lists.is_empty() {
        return if last_prefix {
            prefix_postings(ix, last.as_bytes())
        } else {
            ix.words.get(last.as_bytes()).cloned().unwrap_or_default()
        };
    }
    lists.sort_by_key(|l| l.len());
    let mut cands: Vec<u32> = lists[0].clone();
    for l in &lists[1..] {
        cands = intersect(&cands, l);
        if cands.is_empty() {
            return cands;
        }
    }
    if last_prefix {
        // the postings of the words the last term begins, when they are few, else a
        // check of each candidate's words
        let p = last.as_bytes();
        let hi = successor(p);
        let range = match &hi {
            Some(h) => ix
                .words
                .range::<[u8], _>((Bound::Included(p), Bound::Excluded(h.as_slice()))),
            None => ix
                .words
                .range::<[u8], _>((Bound::Included(p), Bound::Unbounded)),
        };
        let mut total = 0usize;
        let mut small = true;
        for (_, l) in range {
            total += l.len();
            if total > 4 * cands.len() + 64 {
                small = false;
                break;
            }
        }
        if small {
            return intersect(&cands, &prefix_postings(ix, p));
        }
        cands.retain(|&v| {
            let n = normalize(&ix.values[v as usize].text);
            words(&n).any(|w| w.starts_with(last.as_str()))
        });
        cands
    } else {
        match ix.words.get(last.as_bytes()) {
            Some(l) => intersect(&cands, l),
            None => Vec::new(),
        }
    }
}

/// Run `req` over the index for the caller of `scope`: the ranked hits from `offset` and
/// whether more follow.
pub(super) fn run<'a>(
    scope: &Scope<'a>,
    req: &SearchRequest,
    langs: &[u8],
    prefixes: &BTreeMap<String, String>,
) -> (Vec<Candidate<'a>>, bool, Query) {
    let ix = scope.ix;
    let norm = normalize(&req.query);
    let terms: Vec<String> = words(&norm).map(str::to_string).collect();
    let last_prefix = terms.last().is_some_and(|t| chars(t) >= 2);
    let mut q = Query {
        norm,
        fuzzy: vec![FxHashSet::default(); terms.len()],
        terms,
        last_prefix,
        iri: query_iri(&req.query, prefixes),
    };
    let need = req.offset + req.limit + 1;
    let mut r = Ranker {
        scope,
        langs,
        need,
        bound: BinaryHeap::new(),
        done: FxHashSet::default(),
        out: Vec::new(),
        tier: FxHashMap::default(),
    };
    // tiers 0 and 1: a label equals the query or starts with it; then tiers 2 and 3,
    // the same for synonyms and short forms
    let whole = |r: &mut Ranker<'a, '_>,
                 map: &'a BTreeMap<Box<[u8]>, SmallVec<[u32; 2]>>,
                 tier: u8| {
        let p = q.norm.as_bytes();
        let hi = successor(p);
        let range = match &hi {
            Some(h) => map.range::<[u8], _>((Bound::Included(p), Bound::Excluded(h.as_slice()))),
            None => map.range::<[u8], _>((Bound::Included(p), Bound::Unbounded)),
        };
        for (k, vs) in range {
            let exact = &**k == p;
            let (t, kind) = if exact {
                (tier, MatchKind::Exact)
            } else {
                (tier + 1, MatchKind::Prefix)
            };
            for &v in vs {
                r.offer(v, t, 0, kind, false);
            }
        }
    };
    if !q.norm.is_empty() {
        whole(&mut r, &ix.labels, 0);
        r.close();
        if r.out.len() < need {
            whole(&mut r, &ix.alts, 2);
        }
    }
    if let Some(iri) = &q.iri {
        if let Some(&e) = ix.by_iri.get(iri.as_str()) {
            r.offer_iri(e, 2, MatchKind::Exact);
        }
        for (k, &e) in ix
            .by_iri
            .range::<str, _>((Bound::Excluded(iri.as_str()), Bound::Unbounded))
            .take(MAX_IRI_PREFIX)
        {
            if !k.starts_with(iri.as_str()) {
                break;
            }
            r.offer_iri(e, 3, MatchKind::Prefix);
        }
    }
    r.close();
    // tiers 4 and 5: every word of the query is a word of the text
    let mut definitions: Vec<u32> = Vec::new();
    if r.out.len() < need && !q.terms.is_empty() {
        for v in word_candidates(ix, &q.terms, q.last_prefix) {
            match ix.values[v as usize].field() {
                Field::Definition => definitions.push(v),
                Field::Label => r.offer(v, 4, 0, MatchKind::Words, false),
                _ => r.offer(v, 5, 0, MatchKind::Words, false),
            }
        }
        r.close();
    }
    // tier 6: within the edits the query's length allows
    if r.out.len() < need && req.fuzzy && !q.terms.is_empty() {
        fuzzy(ix, &mut q, &mut r);
        r.close();
    }
    // tier 7: the words of a definition
    if r.out.len() < need && req.definitions {
        for v in definitions {
            r.offer(v, 7, 0, MatchKind::Words, false);
        }
        r.close();
    }
    let more = r.out.len() > req.offset + req.limit;
    let mut out = r.out;
    out.truncate(req.offset + req.limit);
    let out = out.split_off(req.offset.min(out.len()));
    (out, more, q)
}

/// The fuzzy tier: texts whose beginning is within the query's edits, and texts whose
/// words match the query's words with edits.
fn fuzzy(ix: &Index, q: &mut Query, r: &mut Ranker<'_, '_>) {
    let d = max_edits(chars(&q.norm), true);
    if d > 0 {
        let a = dfa(&q.norm, d, true);
        let settle = Some(chars(&q.norm) + d as usize);
        for map in [&ix.labels, &ix.alts] {
            walk(map, &a, settle, |_, vs, dist| {
                if dist == 0 {
                    return;
                }
                for &v in vs.iter() {
                    r.offer(v, 6, dist, MatchKind::Fuzzy, true);
                }
            });
        }
    }
    // the words each term matches, with the fewest edits for each
    let n = q.terms.len();
    let mut sets: Vec<FxHashMap<&[u8], u8>> = Vec::with_capacity(n);
    let mut any_edit = false;
    for (i, t) in q.terms.iter().enumerate() {
        let last = i + 1 == n;
        let prefix = last && q.last_prefix;
        let d = max_edits(chars(t), prefix);
        let mut m: FxHashMap<&[u8], u8> = FxHashMap::default();
        if d > 0 {
            any_edit = true;
            let a = dfa(t, d, prefix);
            let settle = prefix.then(|| chars(t) + d as usize);
            walk(&ix.words, &a, settle, |w, _, dist| {
                m.insert(w, dist);
            });
            q.fuzzy[i] = m
                .iter()
                .filter(|(_, d)| **d > 0)
                .map(|(w, _)| Box::<[u8]>::from(*w))
                .collect();
        } else if prefix {
            let hi = successor(t.as_bytes());
            let range = match &hi {
                Some(h) => ix.words.range::<[u8], _>((
                    Bound::Included(t.as_bytes()),
                    Bound::Excluded(h.as_slice()),
                )),
                None => ix
                    .words
                    .range::<[u8], _>((Bound::Included(t.as_bytes()), Bound::Unbounded)),
            };
            for (w, _) in range {
                m.insert(w, 0);
            }
        } else if let Some((w, _)) = ix.words.get_key_value(t.as_bytes()) {
            m.insert(w, 0);
        }
        if m.is_empty() {
            return;
        }
        sets.push(m);
    }
    if !any_edit {
        return;
    }
    // the candidates of the term with the fewest postings, checked against the others
    let size = |m: &FxHashMap<&[u8], u8>| -> usize {
        m.keys().map(|w| ix.words.get(*w).map_or(0, Vec::len)).sum()
    };
    let first = (0..n).min_by_key(|&i| size(&sets[i])).unwrap_or(0);
    let mut cands: FxHashMap<u32, u8> = FxHashMap::default();
    for (w, &d) in &sets[first] {
        if let Some(l) = ix.words.get(*w) {
            for &v in l {
                let e = cands.entry(v).or_insert(d);
                *e = (*e).min(d);
            }
        }
    }
    for (v, d0) in cands {
        let x = &ix.values[v as usize];
        if x.field() == Field::Definition || !x.live() {
            continue;
        }
        let mut total = d0;
        if n > 1 {
            let mut norm: Option<String> = None;
            let mut ok = true;
            for (i, set) in sets.iter().enumerate() {
                if i == first {
                    continue;
                }
                // a few words: their postings; many: the value's own words
                let best = if set.len() <= 16 {
                    set.iter()
                        .filter(|(w, _)| {
                            ix.words
                                .get(**w)
                                .is_some_and(|l| l.binary_search(&v).is_ok())
                        })
                        .map(|(_, d)| *d)
                        .min()
                } else {
                    let n = norm.get_or_insert_with(|| normalize(&x.text));
                    words(n)
                        .filter_map(|w| set.get(w.as_bytes()))
                        .min()
                        .copied()
                };
                match best {
                    Some(d) => total += d,
                    None => {
                        ok = false;
                        break;
                    }
                }
            }
            if !ok {
                continue;
            }
        }
        if total > 0 && total <= 2 {
            r.offer(v, 6, total, MatchKind::Fuzzy, false);
        }
    }
}

/// The byte ranges of `text` to highlight for a hit of `kind`.
pub(super) fn highlight(text: &str, kind: MatchKind, whole: bool, q: &Query) -> Vec<Range<usize>> {
    let (n, map) = normalize_mapped(text);
    let mut ranges: Vec<Range<usize>> = Vec::new();
    match kind {
        MatchKind::Exact | MatchKind::Prefix => {
            if n.starts_with(q.norm.as_str()) {
                ranges.push(0..q.norm.len());
            }
        }
        MatchKind::Fuzzy if whole => {
            let mut end = q.norm.len().min(n.len());
            while !n.is_char_boundary(end) {
                end -= 1;
            }
            ranges.push(0..end);
        }
        _ => {
            let k = q.terms.len();
            let mut at = 0;
            for w in n.split(' ') {
                let start = at;
                at += w.len() + 1;
                for (i, t) in q.terms.iter().enumerate() {
                    let last = i + 1 == k;
                    let len = if w == t.as_str() {
                        w.len()
                    } else if last && q.last_prefix && w.starts_with(t.as_str()) {
                        t.len()
                    } else if q.fuzzy[i].contains(w.as_bytes()) {
                        w.len()
                    } else {
                        continue;
                    };
                    ranges.push(start..start + len);
                    break;
                }
            }
        }
    }
    source_ranges(&map, &ranges)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edits_grow_with_the_length() {
        assert_eq!(max_edits(2, false), 0);
        assert_eq!(max_edits(3, false), 1);
        assert_eq!(max_edits(6, false), 2);
        assert_eq!(max_edits(3, true), 0);
        assert_eq!(max_edits(4, true), 1);
        assert_eq!(max_edits(7, true), 1);
        assert_eq!(max_edits(8, true), 2);
    }

    #[test]
    fn the_walk_finds_keys_within_the_edits() {
        let mut m: BTreeMap<Box<[u8]>, ()> = BTreeMap::new();
        for w in [
            "glucose",
            "glucoside",
            "galactose",
            "fructose",
            "gluten",
            "glycine",
        ] {
            m.insert(w.as_bytes().into(), ());
        }
        let mut got = Vec::new();
        walk(&m, &dfa("glucsoe", 2, false), None, |k, _, d| {
            got.push((String::from_utf8(k.to_vec()).unwrap(), d))
        });
        assert_eq!(got, [("glucose".to_string(), 1)]);
        let mut got = Vec::new();
        walk(&m, &dfa("gluk", 1, true), Some(5), |k, _, d| {
            got.push((String::from_utf8(k.to_vec()).unwrap(), d))
        });
        assert_eq!(
            got,
            [
                ("glucose".to_string(), 1),
                ("glucoside".to_string(), 1),
                ("gluten".to_string(), 1)
            ]
        );
    }

    #[test]
    fn intersections_of_sorted_lists() {
        assert_eq!(intersect(&[1, 3, 5, 7], &[2, 3, 4, 7, 9]), [3, 7]);
        assert!(intersect(&[1], &[]).is_empty());
    }

    #[test]
    fn iris_and_prefixed_names() {
        let mut p = BTreeMap::new();
        p.insert(
            "obo".to_string(),
            "http://purl.obolibrary.org/obo/".to_string(),
        );
        assert_eq!(
            query_iri("obo:CHEBI_1", &p).as_deref(),
            Some("http://purl.obolibrary.org/obo/CHEBI_1")
        );
        assert_eq!(query_iri("<urn:x:y>", &p).as_deref(), Some("urn:x:y"));
        assert_eq!(
            query_iri("http://ex.org/a", &p).as_deref(),
            Some("http://ex.org/a")
        );
        assert_eq!(query_iri("CHEBI:15377", &p), None);
        assert_eq!(query_iri("water", &p), None);
    }
}

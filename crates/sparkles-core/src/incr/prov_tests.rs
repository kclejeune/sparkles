//! Differential tests of the provenance annotations (spec F13 §5, §11): after every
//! pass, each row's `count`, `lineage` and `why` annotation equals a brute-force
//! recomputation over the store's quads, on random histories with insertions,
//! deletions, changes that cancel within a commit and across commits, compactions, and
//! witness bounds small enough to overflow.

use super::circuit::{Circuit, DeltaSet};
use super::compile::compile;
use super::prov::{Absence, Ann, Atom, FactKey, Provenance, Witness};
use crate::sparql::{self, QueryOptions};
use crate::store::{Store, StoreOptions};
use crate::task::Control;
use oxrdf::{NamedNode, Term};
use rand::rngs::StdRng;
use rand::{RngExt, SeedableRng};
use std::collections::{BTreeMap, BTreeSet, HashMap};

const NS: &str = "http://ex/";
const NODES: u8 = 4;
const PREDS: u8 = 3;
const VARS: [&str; 3] = ["a", "b", "c"];

fn iri(s: String) -> Term {
    Term::NamedNode(NamedNode::new_unchecked(format!("{NS}{s}")))
}

fn node(i: u8) -> Term {
    iri(format!("n{i}"))
}

fn pred(i: u8) -> Term {
    iri(format!("p{i}"))
}

#[derive(Clone, Copy, Debug)]
enum T {
    Var(u8),
    Node(u8),
}

/// A query of the fragment the annotations cover, rendered as SPARQL and evaluated
/// by brute force.
#[derive(Clone, Debug)]
enum Q {
    Bgp(Vec<(T, u8, T)>),
    Join(Box<Q>, Box<Q>),
    Union(Box<Q>, Box<Q>),
    Optional(Box<Q>, Box<Q>),
    Minus(Box<Q>, Box<Q>),
    NotExists(Box<Q>, Box<Q>),
    Exists(Box<Q>, Box<Q>),
    Distinct(Box<Q>),
    Project(Box<Q>, Vec<String>),
    /// `COUNT(*)` grouped by a variable, or one implicit group; the count's variable
    Count(Box<Q>, Option<String>, String),
}

fn gen_t(rng: &mut StdRng) -> T {
    if rng.random_bool(0.75) {
        T::Var(rng.random_range(0..VARS.len() as u8))
    } else {
        T::Node(rng.random_range(0..NODES))
    }
}

fn generate(rng: &mut StdRng, depth: u32, counts: &mut u32) -> Q {
    let leaf = depth == 0 || rng.random_bool(0.3);
    if leaf {
        let n = rng.random_range(1..=2);
        return Q::Bgp(
            (0..n)
                .map(|_| (gen_t(rng), rng.random_range(0..PREDS), gen_t(rng)))
                .collect(),
        );
    }
    let mut sub = |rng: &mut StdRng| Box::new(generate(rng, depth - 1, counts));
    match rng.random_range(0..10) {
        0 => Q::Join(sub(rng), sub(rng)),
        1 => Q::Union(sub(rng), sub(rng)),
        2 => Q::Optional(sub(rng), sub(rng)),
        3 => Q::Minus(sub(rng), sub(rng)),
        4 => Q::NotExists(sub(rng), sub(rng)),
        5 => Q::Exists(sub(rng), sub(rng)),
        6 => Q::Distinct(sub(rng)),
        7 => {
            let inner = sub(rng);
            let scope: Vec<String> = scope(&inner).into_iter().collect();
            let mut keep: Vec<String> = scope
                .iter()
                .filter(|_| rng.random_bool(0.6))
                .cloned()
                .collect();
            // `(1 AS ?k)` cannot bind a variable the inner pattern binds
            if keep.is_empty() && scope.iter().any(|v| v == "k") {
                keep.push("k".into());
            }
            Q::Project(inner, keep)
        }
        _ => {
            let inner = sub(rng);
            let scope: Vec<String> = scope(&inner).into_iter().collect();
            let key = if !scope.is_empty() && rng.random_bool(0.7) {
                Some(scope[rng.random_range(0..scope.len())].clone())
            } else {
                None
            };
            *counts += 1;
            Q::Count(inner, key, format!("n{counts}"))
        }
    }
}

/// The variables in scope of a pattern.
fn scope(q: &Q) -> BTreeSet<String> {
    match q {
        Q::Bgp(ps) => ps
            .iter()
            .flat_map(|(s, _, o)| [s, o])
            .filter_map(|t| match t {
                T::Var(v) => Some(VARS[*v as usize].to_string()),
                T::Node(_) => None,
            })
            .collect(),
        Q::Join(a, b) | Q::Union(a, b) | Q::Optional(a, b) => &scope(a) | &scope(b),
        Q::Minus(a, _) | Q::NotExists(a, _) | Q::Exists(a, _) => scope(a),
        Q::Distinct(a) => scope(a),
        // the empty projection binds `?k` to 1
        Q::Project(_, vs) if vs.is_empty() => ["k".to_string()].into(),
        Q::Project(_, vs) => vs.iter().cloned().collect(),
        Q::Count(_, k, n) => k.iter().cloned().chain([n.clone()]).collect(),
    }
}

fn render_t(t: T) -> String {
    match t {
        T::Var(v) => format!("?{}", VARS[v as usize]),
        T::Node(n) => format!("<{NS}n{n}>"),
    }
}

/// The pattern as the inside of a group.
fn render(q: &Q) -> String {
    match q {
        Q::Bgp(ps) => ps
            .iter()
            .map(|(s, p, o)| format!("{} <{NS}p{p}> {} .", render_t(*s), render_t(*o)))
            .collect::<Vec<_>>()
            .join(" "),
        Q::Join(a, b) => format!("{{ {} }} {{ {} }}", render(a), render(b)),
        Q::Union(a, b) => format!("{{ {} }} UNION {{ {} }}", render(a), render(b)),
        Q::Optional(a, b) => format!("{{ {} }} OPTIONAL {{ {} }}", render(a), render(b)),
        Q::Minus(a, b) => format!("{{ {} }} MINUS {{ {} }}", render(a), render(b)),
        Q::NotExists(a, b) => format!("{{ {} }} FILTER NOT EXISTS {{ {} }}", render(a), render(b)),
        Q::Exists(a, b) => format!("{{ {} }} FILTER EXISTS {{ {} }}", render(a), render(b)),
        Q::Distinct(a) => format!("{{ SELECT DISTINCT * {{ {} }} }}", render(a)),
        Q::Project(a, vs) => {
            let vars: Vec<String> = vs.iter().map(|v| format!("?{v}")).collect();
            if vars.is_empty() {
                // a projection to no variable keeps one solution per solution
                format!("{{ SELECT (1 AS ?k) {{ {} }} }}", render(a))
            } else {
                format!("{{ SELECT {} {{ {} }} }}", vars.join(" "), render(a))
            }
        }
        Q::Count(a, k, n) => match k {
            Some(k) => format!(
                "{{ SELECT ?{k} (COUNT(*) AS ?{n}) {{ {} }} GROUP BY ?{k} }}",
                render(a)
            ),
            None => format!("{{ SELECT (COUNT(*) AS ?{n}) {{ {} }} }}", render(a)),
        },
    }
}

/// A term ordered by its text, for keys of ordered maps.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
struct V(Term);

impl PartialOrd for V {
    fn partial_cmp(&self, o: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(o))
    }
}

impl Ord for V {
    fn cmp(&self, o: &Self) -> std::cmp::Ordering {
        self.0.to_string().cmp(&o.0.to_string())
    }
}

type Mapping = BTreeMap<String, V>;
/// The exact `why` polynomial of a solution: its derivations per support.
type Poly = HashMap<Witness, i64>;

fn mul(a: &Poly, b: &Poly) -> Poly {
    let mut out = Poly::new();
    for (x, cx) in a {
        for (y, cy) in b {
            *out.entry(x.union(y)).or_insert(0) += cx * cy;
        }
    }
    out.retain(|_, c| *c != 0);
    out
}

fn add(a: &mut Poly, b: &Poly) {
    for (x, c) in b {
        *a.entry(x.clone()).or_insert(0) += c;
    }
    a.retain(|_, c| *c != 0);
}

fn compatible(a: &Mapping, b: &Mapping) -> bool {
    a.iter().all(|(k, v)| b.get(k).is_none_or(|w| w == v))
}

fn merge(a: &Mapping, b: &Mapping) -> Mapping {
    let mut m = a.clone();
    m.extend(b.iter().map(|(k, v)| (k.clone(), v.clone())));
    m
}

/// A negative condition with its node number left out: tests compare conditions by
/// their bindings, since the numbering belongs to the circuit.
fn absent(shared: &BTreeSet<String>, m: &Mapping) -> Poly {
    let b: Vec<(&str, Option<Term>)> = shared
        .iter()
        .map(|v| (v.as_str(), m.get(v).map(|x| x.0.clone())))
        .collect();
    let a = Absence::of_terms(0, &b);
    Poly::from([(Witness::new(vec![Atom::Absent(a)]), 1)])
}

type Sols = Vec<(Mapping, i64, Poly)>;
/// Operations of one request: insertions (`true`) or deletions of triples.
type Ops = Vec<(bool, Vec<Quad>)>;

/// Solutions with each mapping once.
fn consolidate(s: Sols) -> Sols {
    let mut m: BTreeMap<Mapping, (i64, Poly)> = BTreeMap::new();
    for (k, w, p) in s {
        let e = m.entry(k).or_insert_with(|| (0, Poly::new()));
        e.0 += w;
        add(&mut e.1, &p);
    }
    m.into_iter()
        .filter(|(_, (w, _))| *w != 0)
        .map(|(k, (w, p))| (k, w, p))
        .collect()
}

/// The pattern's solutions over `data`, by brute force, with bag weights and exact
/// polynomials.
/// A stored quad: subject, predicate, object and graph, 0 for the default graph.
type Quad = (u8, u8, u8, u8);

fn graph(g: u8) -> Term {
    iri(format!("g{g}"))
}

/// The triples a query reads with the key of the quad that each derivation uses: the
/// default graph's quads, or over the union default graph the named graphs' triples,
/// each represented by its quad with the smallest key.
struct Data(Vec<((u8, u8, u8), FactKey)>);

impl Data {
    fn new(quads: &BTreeSet<Quad>, union: bool) -> Data {
        let mut m: BTreeMap<(u8, u8, u8), FactKey> = BTreeMap::new();
        for &(a, p, b, g) in quads {
            if union != (g != 0) {
                continue;
            }
            let gt = (g != 0).then(|| graph(g));
            let k = FactKey::of_terms(&node(a), &pred(p), &node(b), gt.as_ref());
            let e = m.entry((a, p, b)).or_insert(k);
            if k < *e {
                *e = k;
            }
        }
        Data(m.into_iter().collect())
    }
}

fn eval(q: &Q, data: &Data) -> Sols {
    match q {
        Q::Bgp(ps) => {
            let mut sols: Vec<(Mapping, Vec<Atom>)> = vec![(Mapping::new(), Vec::new())];
            for (s, p, o) in ps {
                let mut next = Vec::new();
                for (m, atoms) in &sols {
                    for &((ds, dp, d_o), key) in &data.0 {
                        if dp != *p {
                            continue;
                        }
                        let mut m = m.clone();
                        let mut ok = true;
                        for (t, v) in [(s, ds), (o, d_o)] {
                            match t {
                                T::Node(n) => ok &= *n == v,
                                T::Var(x) => {
                                    let name = VARS[*x as usize].to_string();
                                    match m.get(&name) {
                                        Some(b) => ok &= b.0 == node(v),
                                        None => {
                                            m.insert(name, V(node(v)));
                                        }
                                    }
                                }
                            }
                        }
                        if ok {
                            let mut atoms = atoms.clone();
                            atoms.push(Atom::Fact(key));
                            next.push((m, atoms));
                        }
                    }
                }
                sols = next;
            }
            consolidate(
                sols.into_iter()
                    .map(|(m, a)| (m, 1, Poly::from([(Witness::new(a), 1)])))
                    .collect(),
            )
        }
        Q::Join(a, b) => {
            let (l, r) = (eval(a, data), eval(b, data));
            let mut out = Vec::new();
            for (ma, wa, pa) in &l {
                for (mb, wb, pb) in &r {
                    if compatible(ma, mb) {
                        out.push((merge(ma, mb), wa * wb, mul(pa, pb)));
                    }
                }
            }
            consolidate(out)
        }
        Q::Union(a, b) => {
            let mut out = eval(a, data);
            out.extend(eval(b, data));
            consolidate(out)
        }
        Q::Optional(a, b) | Q::Minus(a, b) | Q::NotExists(a, b) | Q::Exists(a, b) => {
            let (l, r) = (eval(a, data), eval(b, data));
            let shared: BTreeSet<String> = &scope(a) & &scope(b);
            let mut out = Vec::new();
            for (ma, wa, pa) in &l {
                let matches: Vec<&(Mapping, i64, Poly)> = r
                    .iter()
                    .filter(|(mb, _, _)| {
                        compatible(ma, mb)
                            && (!matches!(q, Q::Minus(..)) || mb.keys().any(|k| ma.contains_key(k)))
                    })
                    .collect();
                match q {
                    Q::Optional(..) => {
                        for (mb, wb, pb) in &matches {
                            out.push((merge(ma, mb), wa * wb, mul(pa, pb)));
                        }
                        if matches.is_empty() {
                            out.push((ma.clone(), *wa, mul(pa, &absent(&shared, ma))));
                        }
                    }
                    Q::Exists(..) => {
                        if !matches.is_empty() {
                            let mut s = Poly::new();
                            for (_, _, pb) in &matches {
                                add(&mut s, pb);
                            }
                            out.push((ma.clone(), *wa, mul(pa, &s)));
                        }
                    }
                    _ => {
                        if matches.is_empty() {
                            out.push((ma.clone(), *wa, mul(pa, &absent(&shared, ma))));
                        }
                    }
                }
            }
            consolidate(out)
        }
        Q::Distinct(a) => consolidate(eval(a, data))
            .into_iter()
            .map(|(m, _, p)| (m, 1, p))
            .collect(),
        Q::Project(a, vs) => {
            let out: Sols = eval(a, data)
                .into_iter()
                .map(|(m, w, p)| {
                    let mut k: Mapping = m.into_iter().filter(|(v, _)| vs.contains(v)).collect();
                    if vs.is_empty() {
                        k.insert("k".into(), V(one()));
                    }
                    (k, w, p)
                })
                .collect();
            consolidate(out)
        }
        Q::Count(a, k, n) => {
            let mut groups: BTreeMap<Option<V>, (i64, Poly)> = BTreeMap::new();
            for (m, w, p) in eval(a, data) {
                let key = k.as_ref().and_then(|k| m.get(k).cloned());
                let e = groups.entry(key).or_insert_with(|| (0, Poly::new()));
                e.0 += w;
                add(&mut e.1, &p);
            }
            if k.is_none() && groups.is_empty() {
                let p = absent(&BTreeSet::new(), &Mapping::new());
                groups.insert(None, (0, p));
            }
            groups
                .into_iter()
                .map(|(key, (c, p))| {
                    let mut m = Mapping::new();
                    if let (Some(k), Some(v)) = (k, key) {
                        m.insert(k.clone(), v);
                    }
                    m.insert(n.clone(), V(integer(c)));
                    (m, 1, p)
                })
                .collect()
        }
    }
}

fn one() -> Term {
    integer(1)
}

fn integer(n: i64) -> Term {
    Term::Literal(oxrdf::Literal::new_typed_literal(
        n.to_string(),
        oxrdf::vocab::xsd::INTEGER,
    ))
}

/// A row's mapping, from the circuit's columns and terms.
fn mapping(cols: &[String], row: &[Option<Term>]) -> Mapping {
    cols.iter()
        .zip(row)
        .filter_map(|(c, t)| t.clone().map(|t| (c.clone(), V(t))))
        .collect()
}

/// Witnesses with the node numbers of their conditions left out.
fn strip(w: &Witness) -> Witness {
    Witness::new(
        w.atoms()
            .iter()
            .map(|a| match a {
                Atom::Absent(c) => Atom::Absent(Absence { node: 0, ..*c }),
                f => *f,
            })
            .collect(),
    )
}

/// One request of operations in order, each an insertion (`true`) or a deletion of
/// quads, committed together.
fn request_quads(s: &Store, ops: &[(bool, Vec<Quad>)]) {
    let t = |&(a, p, b, g): &Quad| {
        let t = format!("<{NS}n{a}> <{NS}p{p}> <{NS}n{b}> .");
        if g == 0 {
            t
        } else {
            format!("GRAPH <{NS}g{g}> {{ {t} }}")
        }
    };
    let ops: Vec<String> = ops
        .iter()
        .filter(|(_, ts)| !ts.is_empty())
        .map(|(ins, ts)| {
            format!(
                "{} DATA {{ {} }}",
                if *ins { "INSERT" } else { "DELETE" },
                ts.iter().map(t).collect::<String>()
            )
        })
        .collect();
    if ops.is_empty() {
        return;
    }
    sparql::update::update(s, &ops.join(" ; "), &QueryOptions::default()).expect("update");
}

fn update(s: &Store, del: &[(u8, u8, u8)], ins: &[(u8, u8, u8)]) {
    let q = |ts: &[(u8, u8, u8)]| ts.iter().map(|&(a, p, b)| (a, p, b, 0)).collect();
    request_quads(s, &[(false, q(del)), (true, q(ins))]);
}

/// Compare one circuit's rows and annotations with the brute-force solutions.
fn check(c: &Circuit, want: &Sols, mode: Provenance, what: &str) -> Result<(), String> {
    let cols = c.columns();
    let mut got: BTreeMap<Mapping, (i64, Ann)> = BTreeMap::new();
    for (row, w, ann) in c.raw_annotations() {
        let ann = ann.unwrap_or_else(|| Ann::zero(mode));
        got.insert(mapping(&cols, &row), (w, ann));
    }
    let want: BTreeMap<&Mapping, (i64, &Poly)> =
        want.iter().map(|(m, w, p)| (m, (*w, p))).collect();
    let gk: Vec<&Mapping> = got.keys().collect();
    let wk: Vec<&Mapping> = want.keys().copied().collect();
    if gk != wk {
        return Err(format!("{what}: rows differ\n got {gk:?}\nwant {wk:?}"));
    }
    for (m, (w, ann)) in &got {
        let (ww, poly) = want[m];
        if *w != ww {
            return Err(format!("{what}: weight of {m:?}: {w} != {ww}"));
        }
        let total: i64 = poly.values().sum();
        if ann.count() != total {
            return Err(format!(
                "{what}: count of {m:?}: {} != {total}\n{ann:?}\n{poly:?}",
                ann.count()
            ));
        }
        match ann {
            Ann::Count(_) => {}
            Ann::Lineage(l) => {
                let mut exact: HashMap<Atom, i64> = HashMap::new();
                for (x, c) in poly {
                    for a in x.atoms() {
                        *exact.entry(*a).or_insert(0) += c;
                    }
                }
                exact.retain(|_, c| *c != 0);
                let got: HashMap<Atom, i64> = l
                    .atoms
                    .iter()
                    .map(|(a, c)| (strip(&Witness::new(vec![*a])).atoms()[0], *c))
                    .fold(HashMap::new(), |mut h, (a, c)| {
                        *h.entry(a).or_insert(0) += c;
                        h
                    });
                // two conditions with the same bindings meet once their numbers are
                // left out, so conditions compare by presence and facts by count
                let facts =
                    |m: &HashMap<Atom, i64>| -> (BTreeMap<FactKey, i64>, BTreeSet<Absence>) {
                        let mut f = BTreeMap::new();
                        let mut a = BTreeSet::new();
                        for (k, c) in m {
                            match k {
                                Atom::Fact(x) => {
                                    f.insert(*x, *c);
                                }
                                Atom::Absent(x) if *c > 0 => {
                                    a.insert(*x);
                                }
                                Atom::Absent(_) => {}
                            }
                        }
                        (f, a)
                    };
                if l.unknown != 0 || facts(&got) != facts(&exact) {
                    return Err(format!(
                        "{what}: lineage of {m:?}\n got {got:?}\nwant {exact:?}"
                    ));
                }
            }
            Ann::Why(x) => {
                let mut kept: HashMap<Witness, i64> = HashMap::new();
                for (w, c) in &x.supports {
                    *kept.entry(strip(w)).or_insert(0) += c;
                }
                let max = mode.max_witnesses();
                if kept.len() > max {
                    return Err(format!("{what}: {} supports kept", kept.len()));
                }
                for (w, c) in &kept {
                    if *c <= 0 || *c > poly.get(w).copied().unwrap_or(0) {
                        return Err(format!(
                            "{what}: support {w:?} of {m:?} kept {c} times\n{poly:?}"
                        ));
                    }
                }
                if x.unknown < 0 {
                    return Err(format!("{what}: negative unknown count"));
                }
                if x.unknown == 0 && kept != *poly {
                    return Err(format!(
                        "{what}: supports of {m:?} not exact\n got {kept:?}\nwant {poly:?}"
                    ));
                }
                // a report without truncation has the minimal supports, smallest first
                let r = ann.report(max);
                if !r.truncated {
                    let mut all: Vec<&Witness> = poly.keys().collect();
                    all.sort();
                    let minimal: Vec<Witness> = all
                        .iter()
                        .filter(|w| !all.iter().any(|y| y.len() < w.len() && y.is_subset(w)))
                        .map(|w| (*w).clone())
                        .collect();
                    // conditions of different operators meet once their numbers are
                    // left out, so the report holds every minimal support of the
                    // oracle and only supports of the oracle
                    let rep: BTreeSet<Witness> = r.witnesses.iter().map(strip).collect();
                    let ok = rep.iter().all(|w| poly.contains_key(w))
                        && (minimal.len() > max || minimal.iter().all(|w| rep.contains(w)))
                        && r.witnesses.len() <= max;
                    if !ok {
                        return Err(format!(
                            "{what}: report of {m:?}\n got {rep:?}\nwant {minimal:?}"
                        ));
                    }
                }
            }
        }
    }
    Ok(())
}

/// Whether a circuit has rows whose derivations are unknown: those of a `recompute`
/// node, and those a correlated `EXISTS` keeps, since the engine evaluates its condition
/// without reporting why it holds (spec F13, item 528).
fn unknown_witnesses(p: &super::ops::PlanNode) -> bool {
    p.has_recompute() || p.operator == "ExistsFilter" || p.children.iter().any(unknown_witnesses)
}

fn run(seed: u64, mode: Provenance) -> Result<bool, String> {
    let mut rng = StdRng::seed_from_u64(seed);
    // every third case reads the union of two named graphs as its default graph
    let union = seed % 3 == 2;
    let s = Store::in_memory(StoreOptions {
        union_default_graph: union,
        ..Default::default()
    });
    let mut data: BTreeSet<Quad> = BTreeSet::new();
    let triple = |rng: &mut StdRng| {
        (
            rng.random_range(0..NODES),
            rng.random_range(0..PREDS),
            rng.random_range(0..NODES),
            if union { rng.random_range(1..=2) } else { 0 },
        )
    };
    let init: Vec<Quad> = (0..rng.random_range(4..14))
        .map(|_| triple(&mut rng))
        .collect();
    request_quads(&s, &[(true, init.clone())]);
    data.extend(init);
    let mut counts = 0;
    let q = generate(&mut rng, 3, &mut counts);
    let text = format!("SELECT * WHERE {{ {} }}", render(&q));
    let parsed = sparql::parse_query(&text, None, &[]).map_err(|e| format!("{text}: {e}"))?;
    let compiled = compile(&parsed, None).map_err(|e| format!("{text}: {e}"))?;
    let mut c = Circuit::new(compiled, None);
    if unknown_witnesses(&c.plan()) {
        return Ok(false);
    }
    c.set_provenance(mode).map_err(|e| e.to_string())?;
    c.build(s.snapshot(), &Control::none())
        .map_err(|e| format!("{text}: build: {e}"))?;
    check(
        &c,
        &eval(&q, &Data::new(&data, union)),
        mode,
        &format!("{text}\nafter the build"),
    )?;
    for step in 0..24 {
        let all: Vec<Quad> = data.iter().copied().collect();
        let pick = |rng: &mut StdRng| all[rng.random_range(0..all.len())];
        let ops: Ops = match rng.random_range(0..6) {
            // a quad added and removed in one commit cancels within it
            0 => {
                let t = triple(&mut rng);
                if data.contains(&t) {
                    vec![(false, vec![t]), (true, vec![t])]
                } else {
                    vec![(true, vec![t]), (false, vec![t])]
                }
            }
            1 if !all.is_empty() => vec![(false, vec![pick(&mut rng)])],
            // a replacement: removals and an addition in one commit
            2 if !all.is_empty() => {
                let del: Vec<_> = (0..rng.random_range(1..4))
                    .map(|_| pick(&mut rng))
                    .collect();
                vec![(false, del), (true, vec![triple(&mut rng)])]
            }
            _ => vec![(
                true,
                (0..rng.random_range(1..4))
                    .map(|_| triple(&mut rng))
                    .collect(),
            )],
        };
        request_quads(&s, &ops);
        for (ins, ts) in &ops {
            for t in ts {
                if *ins {
                    data.insert(*t);
                } else {
                    data.remove(t);
                }
            }
        }
        if rng.random_bool(0.1) {
            s.compact().map_err(|e| e.to_string())?;
        }
        // some passes cover several commits, whose changes cancel across them
        if rng.random_bool(0.35) {
            continue;
        }
        // changes the store no longer lists, as across a compaction of a store
        // without a change log, build the circuit again
        let d = DeltaSet::read(&s, c.applied, &Control::none());
        c.advance(&d, &Control::none())
            .map_err(|e| format!("{text}: advance: {e}"))?;
        check(
            &c,
            &eval(&q, &Data::new(&data, union)),
            mode,
            &format!("{text}\nmode {mode:?}, after step {step}"),
        )?;
    }
    Ok(true)
}

fn modes() -> [Provenance; 5] {
    [
        Provenance::Count,
        Provenance::Lineage,
        Provenance::why(),
        Provenance::Why { max_witnesses: 2 },
        Provenance::Why { max_witnesses: 1 },
    ]
}

/// Random histories against the brute-force annotations, for every mode. The run's
/// size and seed come from `SPARKLES_PROV_CASES` and `SPARKLES_PROV_SEED`. The cases are
/// split over four tests, which run in parallel: part `part` of `parts` takes the cases
/// whose index leaves `part` modulo `parts`, so every case runs once, and asserts the
/// share of incremental runs over its own cases.
fn annotations_part(part: u64, parts: u64) {
    let cases: u64 = std::env::var("SPARKLES_PROV_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(120);
    let base: u64 = std::env::var("SPARKLES_PROV_SEED")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(415);
    let mut ran = 0;
    let mut mine = 0;
    for i in (0..cases).filter(|i| i % parts == part) {
        mine += 1;
        let seed = base + i;
        for mode in modes() {
            match run(seed, mode) {
                Ok(true) => ran += 1,
                Ok(false) => break,
                Err(e) => panic!("seed {seed}: {e}"),
            }
        }
    }
    assert!(
        ran >= mine * 3,
        "only {ran} runs were incremental out of {mine} cases"
    );
}

#[test]
fn annotations_match_a_brute_force_recomputation_part_1_of_4() {
    annotations_part(0, 4);
}

#[test]
fn annotations_match_a_brute_force_recomputation_part_2_of_4() {
    annotations_part(1, 4);
}

#[test]
fn annotations_match_a_brute_force_recomputation_part_3_of_4() {
    annotations_part(2, 4);
}

#[test]
fn annotations_match_a_brute_force_recomputation_part_4_of_4() {
    annotations_part(3, 4);
}

/// A bound of one witness overflows on a row with two derivations, keeps the smaller
/// support, and converges to the exact set once the extra derivation goes.
#[test]
fn bounded_witnesses_report_truncation_and_converge() {
    let s = Store::in_memory(StoreOptions::default());
    update(&s, &[], &[(0, 0, 1), (0, 1, 2), (2, 2, 1)]);
    let text = format!(
        "SELECT ?a WHERE {{ {{ ?a <{NS}p0> ?b }} UNION {{ ?a <{NS}p1> ?c . ?c <{NS}p2> ?b }} }}"
    );
    let parsed = sparql::parse_query(&text, None, &[]).unwrap();
    let mut c = Circuit::new(compile(&parsed, None).unwrap(), None);
    c.set_provenance(Provenance::Why { max_witnesses: 1 })
        .unwrap();
    c.build(s.snapshot(), &Control::none()).unwrap();
    let rows = c.annotated_rows();
    assert_eq!(rows.len(), 1);
    let a = rows[0].2.clone().unwrap();
    assert_eq!(a.count, 2);
    assert!(a.truncated);
    let small = Witness::new(vec![Atom::Fact(FactKey::of_terms(
        &node(0),
        &pred(0),
        &node(1),
        None,
    ))]);
    assert_eq!(
        a.witnesses,
        vec![small.clone()],
        "the smaller support is kept"
    );
    // the long derivation goes: nothing is unknown any more
    update(&s, &[(2, 2, 1)], &[]);
    let d = DeltaSet::read(&s, c.applied, &Control::none());
    c.advance(&d, &Control::none()).unwrap();
    let a = c.annotated_rows()[0].2.clone().unwrap();
    assert_eq!((a.count, a.truncated), (1, false));
    assert_eq!(a.witnesses, vec![small]);
    // the kept derivation goes and the long one returns: the state knows it again
    update(&s, &[(0, 0, 1)], &[(2, 2, 1)]);
    let d = DeltaSet::read(&s, c.applied, &Control::none());
    c.advance(&d, &Control::none()).unwrap();
    let a = c.annotated_rows()[0].2.clone().unwrap();
    assert_eq!((a.count, a.truncated, a.witnesses.len()), (1, false, 1));
    assert_eq!(a.witnesses[0].len(), 2);
    // the fact keys resolve to the quads through the snapshot
    let keys: Vec<FactKey> = a.witnesses[0]
        .atoms()
        .iter()
        .filter_map(|x| match x {
            Atom::Fact(k) => Some(*k),
            Atom::Absent(_) => None,
        })
        .collect();
    let resolved = c.resolve_facts(&keys, &Control::none()).unwrap();
    let mut quads: Vec<String> = resolved
        .iter()
        .map(|(_, q)| {
            let (t, g) = q.clone().expect("resolved");
            assert!(g.is_none());
            format!("{} {} {}", t[0], t[1], t[2])
        })
        .collect();
    quads.sort();
    assert_eq!(
        quads,
        vec![
            format!("<{NS}n0> <{NS}p1> <{NS}n2>"),
            format!("<{NS}n2> <{NS}p2> <{NS}n1>"),
        ]
    );
}

/// A `MINUS` row's witness names the condition, and the circuit describes it.
#[test]
fn negative_conditions_are_witnesses() {
    let s = Store::in_memory(StoreOptions::default());
    update(&s, &[], &[(0, 0, 1), (2, 0, 1), (2, 1, 3)]);
    let text = format!("SELECT ?a WHERE {{ ?a <{NS}p0> ?b MINUS {{ ?a <{NS}p1> ?c }} }}");
    let parsed = sparql::parse_query(&text, None, &[]).unwrap();
    let mut c = Circuit::new(compile(&parsed, None).unwrap(), None);
    c.set_provenance(Provenance::why()).unwrap();
    c.build(s.snapshot(), &Control::none()).unwrap();
    let rows = c.annotated_rows();
    assert_eq!(rows.len(), 1);
    let w = &rows[0].2.as_ref().unwrap().witnesses[0];
    let absent: Vec<&Absence> = w
        .atoms()
        .iter()
        .filter_map(|a| match a {
            Atom::Absent(c) => Some(c),
            _ => None,
        })
        .collect();
    assert_eq!(absent.len(), 1);
    assert_eq!(
        absent[0].bindings,
        Absence::of_terms(0, &[("a", Some(node(0)))]).bindings
    );
    let conds = c.conditions();
    assert_eq!(conds.len(), 1);
    assert_eq!(conds[0].0, absent[0].node);
    assert!(conds[0].1.starts_with("Minus on [?a]"), "{conds:?}");
    // the condition fails: the row goes
    update(&s, &[], &[(0, 1, 3)]);
    let d = DeltaSet::read(&s, c.applied, &Control::none());
    c.advance(&d, &Control::none()).unwrap();
    assert!(c.annotated_rows().is_empty());
}

/// A circuit's annotations survive the encoding that checkpoints and generation
/// switches use.
#[test]
fn annotations_round_trip_through_the_state_encoding() {
    for mode in modes() {
        let s = Store::in_memory(StoreOptions::default());
        update(&s, &[], &[(0, 0, 1), (1, 0, 2), (1, 1, 2), (3, 0, 1)]);
        let text = format!(
            "SELECT ?a (COUNT(*) AS ?n) WHERE {{ ?a <{NS}p0> ?b OPTIONAL {{ ?b <{NS}p0> ?c }} \
             FILTER NOT EXISTS {{ ?a <{NS}p1> ?b }} }} GROUP BY ?a"
        );
        let parsed = sparql::parse_query(&text, None, &[]).unwrap();
        let mut c = Circuit::new(compile(&parsed, None).unwrap(), None);
        c.set_provenance(mode).unwrap();
        c.build(s.snapshot(), &Control::none()).unwrap();
        let snap = s.snapshot();
        let body = c.encode(&snap).unwrap();
        let mut d = Circuit::new(compile(&parsed, None).unwrap(), None);
        d.set_provenance(mode).unwrap();
        d.decode(&body, &snap).unwrap();
        d.applied = c.applied;
        let norm = |c: &Circuit| {
            let mut v: Vec<String> = c
                .annotated_rows()
                .into_iter()
                .map(|(r, w, a)| format!("{r:?} {w} {a:?}"))
                .collect();
            v.sort();
            v
        };
        assert_eq!(norm(&c), norm(&d), "{mode:?}");
        // both keep moving alike
        update(&s, &[(1, 1, 2)], &[(2, 0, 3)]);
        for x in [&mut c, &mut d] {
            let delta = DeltaSet::read(&s, x.applied, &Control::none());
            x.advance(&delta, &Control::none()).unwrap();
        }
        assert_eq!(norm(&c), norm(&d), "{mode:?} after a pass");
        // a state without annotations is not read as one with them
        let mut plain = Circuit::new(compile(&parsed, None).unwrap(), None);
        assert!(plain.decode(&body, &snap).is_err(), "{mode:?}");
    }
}

fn built(s: &Store, text: &str, mode: Provenance) -> Circuit {
    let parsed = sparql::parse_query(text, None, &[]).unwrap();
    let mut c = Circuit::new(compile(&parsed, None).unwrap(), None);
    c.set_provenance(mode).unwrap();
    c.build(s.snapshot(), &Control::none()).unwrap();
    c
}

fn pass(s: &Store, c: &mut Circuit) {
    let d = DeltaSet::read(s, c.applied, &Control::none());
    c.advance(&d, &Control::none()).unwrap();
}

fn sparql_update(s: &Store, u: &str) {
    sparql::update::update(s, u, &QueryOptions::default()).unwrap();
}

/// Over the union default graph, a triple's witness is the stored named-graph quad
/// with the smallest key among those that hold it. It moves when that quad goes while
/// the triple stays, it resolves to the stored quad, and many graphs do not multiply a
/// row's supports.
#[test]
fn union_default_graph_witnesses_name_stored_quads() {
    let s = Store::in_memory(StoreOptions {
        union_default_graph: true,
        ..Default::default()
    });
    let g = |i: u8| iri(format!("g{i}"));
    let ins =
        |gi: u8| format!("INSERT DATA {{ GRAPH <{NS}g{gi}> {{ <{NS}n0> <{NS}p0> <{NS}n1> }} }}");
    let del =
        |gi: u8| format!("DELETE DATA {{ GRAPH <{NS}g{gi}> {{ <{NS}n0> <{NS}p0> <{NS}n1> }} }}");
    sparql_update(&s, &ins(1));
    let text = format!("SELECT ?a WHERE {{ ?a <{NS}p0> ?b }}");
    let mut c = built(&s, &text, Provenance::why());
    let key = |gi: u8| FactKey::of_terms(&node(0), &pred(0), &node(1), Some(&g(gi)));
    let wit = |c: &Circuit| {
        let a = c.annotated_rows()[0].2.clone().unwrap();
        (a.count, a.witnesses)
    };
    let one = |gi: u8| Witness::new(vec![Atom::Fact(key(gi))]);
    assert_eq!(wit(&c), (1, vec![one(1)]));
    // a second graph holds the triple: the smaller key represents it
    sparql_update(&s, &ins(2));
    pass(&s, &mut c);
    let (small, large) = if key(1) < key(2) { (1, 2) } else { (2, 1) };
    assert_eq!(wit(&c), (1, vec![one(small)]));
    // the representative's quad goes and the triple stays: the other quad represents it
    sparql_update(&s, &del(small));
    pass(&s, &mut c);
    assert_eq!(wit(&c), (1, vec![one(large)]));
    let r = c
        .resolve_facts(&[key(large), key(small)], &Control::none())
        .unwrap();
    assert_eq!(r[0].1.as_ref().and_then(|q| q.1.clone()), Some(g(large)));
    assert!(r[1].1.is_none(), "a removed quad does not resolve");
    // a rebuild agrees with the passes
    assert_eq!(wit(&built(&s, &text, Provenance::why())), wit(&c));
    sparql_update(&s, &del(large));
    pass(&s, &mut c);
    assert!(c.annotated_rows().is_empty());
    // four patterns over forty graphs give one support, not 40^4
    let mut u = String::from("INSERT DATA {");
    for gi in 0..40 {
        u.push_str(&format!(
            " GRAPH <{NS}h{gi}> {{ <{NS}n0> <{NS}p0> <{NS}n1> . <{NS}n0> <{NS}p0> <{NS}n2> . \
             <{NS}n0> <{NS}p0> <{NS}n3> . <{NS}n0> <{NS}p1> <{NS}n1> }}"
        ));
    }
    u.push_str(" }");
    sparql_update(&s, &u);
    pass(&s, &mut c);
    let q4 = format!(
        "SELECT ?a WHERE {{ ?a <{NS}p0> <{NS}n1> . ?a <{NS}p0> <{NS}n2> . \
         ?a <{NS}p0> <{NS}n3> . ?a <{NS}p1> <{NS}n1> }}"
    );
    let c4 = built(&s, &q4, Provenance::Why { max_witnesses: 1 });
    let a = c4.annotated_rows()[0].2.clone().unwrap();
    assert_eq!((a.count, a.witnesses.len(), a.truncated), (1, 1, false));
    assert_eq!(a.witnesses[0].len(), 4);
}

/// Resolving keys stops once they are found and honours cancellation.
#[test]
fn resolving_keys_stops_and_cancels() {
    let s = Store::in_memory(StoreOptions::default());
    let mut u = String::from("INSERT DATA {");
    for i in 0..3000 {
        u.push_str(&format!(" <{NS}m{i}> <{NS}p0> <{NS}n0> ."));
    }
    u.push_str(" }");
    sparql_update(&s, &u);
    let c = built(
        &s,
        &format!("SELECT DISTINCT ?b WHERE {{ ?a <{NS}p0> ?b }}"),
        Provenance::Count,
    );
    // a key the scan meets first ends it
    let (_, all) = c.resolve_counted(
        &[FactKey::of_terms(&node(3), &pred(0), &node(0), None)],
        &Control::none(),
    );
    assert!(all >= 3000, "a missing key scans every quad ({all})");
    let k = FactKey::of_terms(&iri("m0".into()), &pred(0), &node(0), None);
    let (r, early) = c.resolve_counted(&[k], &Control::none());
    assert!(r.unwrap()[0].1.is_some());
    assert!(early < 100, "the scan stops at the key ({early} quads)");
    // cancellation stops the scan within one check
    let control = Control::none();
    control.cancel.cancel();
    let missing = FactKey::of_terms(&node(3), &pred(0), &node(0), None);
    let (r, cancelled) = c.resolve_counted(&[missing], &control);
    assert!(r.is_err());
    assert!(cancelled <= 1024, "{cancelled} quads after cancellation");
}

/// An annotated build matches the patterns itself, and it reads the pattern with the
/// fewest estimated quads next. With one large team, extending `?q memberOf ?t` by
/// `?p memberOf ?t` would visit every pair of members, while `?p knows ?q` first visits
/// a few quads per member.
#[test]
fn an_annotated_build_orders_patterns_by_estimate() {
    let s = Store::in_memory(StoreOptions::default());
    let n = 400;
    let mut u = String::from("INSERT DATA {");
    for i in 0..n {
        u.push_str(&format!(
            " <{NS}m{i}> <{NS}memberOf> <{NS}team> . <{NS}m{i}> <{NS}knows> <{NS}m{}> .",
            (i + 1) % n
        ));
    }
    u.push_str(" }");
    sparql_update(&s, &u);
    let q = format!(
        "SELECT ?p ?q WHERE {{ ?p <{NS}knows> ?q . ?p <{NS}memberOf> ?t . ?q <{NS}memberOf> ?t }}"
    );
    let c = built(&s, &q, Provenance::Count);
    assert_eq!(c.result.len(), n);
    assert!(
        c.build_scanned < 20 * n as u64,
        "the build visited {} quads",
        c.build_scanned
    );
}

/// A build estimates a scan prefix once, however many bindings share it. Here every
/// `:p` row binds the same `?t`, so the `:r` range is estimated once and not once per
/// row, while the `:q` probes differ per subject.
#[test]
fn an_annotated_build_estimates_a_prefix_once() {
    let s = Store::in_memory(StoreOptions::default());
    let n = 300;
    let mut u = String::from("INSERT DATA {");
    for i in 0..n {
        u.push_str(&format!(" <{NS}s{i}> <{NS}p> <{NS}t> ."));
    }
    for i in 0..2 * n {
        u.push_str(&format!(" <{NS}x{i}> <{NS}q> <{NS}c> ."));
    }
    for i in 0..3 * n {
        u.push_str(&format!(" <{NS}t> <{NS}r> <{NS}v{i}> ."));
    }
    u.push_str(" }");
    sparql_update(&s, &u);
    let q = format!("SELECT ?s ?v WHERE {{ ?s <{NS}p> ?t . ?s <{NS}q> <{NS}c> . ?t <{NS}r> ?v }}");
    let c = built(&s, &q, Provenance::Count);
    assert_eq!(c.result.len(), 0);
    assert!(
        c.build_estimated <= n as u64 + 10,
        "the build computed {} estimates for {n} rows",
        c.build_estimated
    );
}

/// An OPTIONAL's filter reads left variables beyond the shared ones, so two rows
/// without a match that differ only there have different conditions.
#[test]
fn optional_filters_bind_their_left_variables() {
    let s = Store::in_memory(StoreOptions::default());
    sparql_update(
        &s,
        &format!(
            "INSERT DATA {{ <{NS}n0> <{NS}p0> 1 . <{NS}n0> <{NS}p0> 2 . <{NS}n0> <{NS}p1> 0 }}"
        ),
    );
    let c = built(
        &s,
        &format!(
            "SELECT ?s ?v WHERE {{ ?s <{NS}p0> ?v OPTIONAL {{ ?s <{NS}p1> ?x FILTER(?x > ?v) }} }}"
        ),
        Provenance::why(),
    );
    let absent: BTreeSet<Absence> = c
        .annotated_rows()
        .iter()
        .flat_map(|(_, _, a)| a.clone().unwrap().witnesses)
        .flat_map(|w| w.atoms().to_vec())
        .filter_map(|a| match a {
            Atom::Absent(x) => Some(x),
            _ => None,
        })
        .collect();
    assert_eq!(absent.len(), 2);
    let conds = c.conditions();
    assert!(conds[0].1.contains("FILTER"), "{conds:?}");
}

/// A CONSTRUCT template with blank nodes keeps the derivation count of its solution,
/// also when only the count changes.
#[test]
fn template_blank_nodes_keep_derivation_counts() {
    let s = Store::in_memory(StoreOptions::default());
    update(&s, &[], &[(0, 0, 1), (0, 0, 2)]);
    let mut c = built(
        &s,
        &format!(
            "CONSTRUCT {{ _:b <{NS}out> ?s }} WHERE {{ {{ SELECT DISTINCT ?s {{ ?s <{NS}p0> ?o }} }} }}"
        ),
        Provenance::Count,
    );
    let count = |c: &Circuit| -> Vec<i64> {
        c.annotated_rows()
            .iter()
            .map(|(_, _, a)| a.clone().unwrap().count)
            .collect()
    };
    assert_eq!(count(&c), vec![2]);
    update(&s, &[], &[(0, 0, 3)]);
    pass(&s, &mut c);
    assert_eq!(count(&c), vec![3]);
    update(&s, &[(0, 0, 1), (0, 0, 2), (0, 0, 3)], &[]);
    pass(&s, &mut c);
    assert!(c.annotated_rows().is_empty());
}

/// Turning provenance off clears every operator's annotations, so the state encodes
/// as a circuit without provenance does.
#[test]
fn turning_provenance_off_clears_the_state() {
    let s = Store::in_memory(StoreOptions::default());
    update(&s, &[], &[(0, 0, 1), (1, 1, 2)]);
    let text = format!(
        "SELECT * WHERE {{ {{ SELECT DISTINCT ?a {{ ?a <{NS}p0> ?b }} }} {{ ?a <{NS}p0> ?c }} \
         OPTIONAL {{ ?c <{NS}p1> ?d }} }}"
    );
    let mut c = built(&s, &text, Provenance::why());
    c.set_provenance(Provenance::None).unwrap();
    c.build(s.snapshot(), &Control::none()).unwrap();
    let snap = s.snapshot();
    let body = c.encode(&snap).unwrap();
    let parsed = sparql::parse_query(&text, None, &[]).unwrap();
    let mut plain = Circuit::new(compile(&parsed, None).unwrap(), None);
    plain.decode(&body, &snap).unwrap();
    assert!(c.annotation(&[]).is_none());
}

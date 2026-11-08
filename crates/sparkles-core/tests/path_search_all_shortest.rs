//! Weighted `path:allShortest` between one pair, which searches from both ends (F07,
//! ROADMAP item 277). The paths are compared with the search from the source alone, which
//! an unbound target or several targets still run, and with every simple path enumerated
//! here by brute force with exact sums of the weights.

use sparkles_core::sparql::{QueryOptions, query};
use sparkles_core::store::{Store, StoreOptions};
use std::collections::{BTreeMap, BTreeSet};

const PREFIXES: &str = "PREFIX ex: <http://ex/> PREFIX path: <urn:x-sparkles:path#> ";

fn store_of(ttl: &str) -> Store {
    let s = Store::in_memory(StoreOptions::default());
    s.load(&[sparkles_core::io::Source::from_bytes(
        format!("@prefix ex: <http://ex/> .\n{ttl}").into_bytes(),
        sparkles_core::io::RdfFormat::Turtle,
        None,
    )])
    .unwrap();
    s
}

fn short(t: &Option<oxrdf::Term>) -> String {
    match t {
        None => "-".into(),
        Some(oxrdf::Term::NamedNode(n)) => n
            .as_str()
            .strip_prefix("http://ex/")
            .unwrap_or(n.as_str())
            .to_string(),
        Some(oxrdf::Term::Literal(l)) => l.value().to_string(),
        Some(t) => t.to_string(),
    }
}

fn rows(s: &Store, q: &str) -> Vec<BTreeMap<String, String>> {
    let r = query(
        s.snapshot(),
        &format!("{PREFIXES}{q}"),
        &QueryOptions::default(),
    )
    .unwrap_or_else(|e| panic!("{q}: {e}"));
    r.rows()
        .unwrap()
        .iter()
        .map(|row| {
            r.variables()
                .iter()
                .zip(row.values())
                .map(|(v, t)| (v.clone(), short(t)))
                .collect()
        })
        .collect()
}

/// A stored triple `(subject, predicate, object)` of `ex:n{s} ex:p{p} ex:n{o}`.
type E = (usize, usize, usize);

/// A path as its stored triples in path order, with the cost the search reported.
type Found = (Vec<E>, f64);

/// The paths of a search's rows by path index. Each row needs `?t`, `?path`, `?i`, `?s`,
/// `?p`, `?o` and `?c`; `target` keeps only the paths to that node.
fn paths_of(rows: &[BTreeMap<String, String>], target: Option<&str>) -> Vec<Found> {
    let num = |v: &str| {
        v.trim_start_matches(|c: char| !c.is_ascii_digit())
            .parse::<usize>()
            .unwrap()
    };
    let mut by: BTreeMap<usize, (BTreeMap<usize, E>, f64)> = BTreeMap::new();
    for r in rows {
        if target.is_some_and(|t| r["t"] != t) {
            continue;
        }
        let e = by
            .entry(r["path"].parse().unwrap())
            .or_insert_with(|| (BTreeMap::new(), r["c"].parse().unwrap()));
        e.0.insert(
            r["i"].parse().unwrap(),
            (num(&r["s"]), num(&r["p"]), num(&r["o"])),
        );
    }
    by.into_values()
        .map(|(es, c)| (es.into_values().collect(), c))
        .collect()
}

/// A weight as an exact integer multiple of 2^-60, which every weight drawn here is.
fn exact(w: f64) -> i128 {
    let x = w * (1u64 << 60) as f64;
    assert_eq!(x.fract(), 0.0, "{w} is a multiple of 2^-60");
    x as i128
}

/// The double nearest to an exact sum.
fn rounded(x: i128) -> f64 {
    x as f64 / (1u64 << 60) as f64
}

/// A small deterministic generator (xorshift).
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

/// Every simple path from `s`, as the nodes and the stored triples, where the last node
/// may be `s` again, over `adj` (node → [(next node, stored triple)]). No node strictly
/// inside a path may be a node of `inner_banned`.
fn brute(
    adj: &BTreeMap<usize, Vec<(usize, E)>>,
    s: usize,
    inner_banned: &BTreeSet<usize>,
) -> Vec<(usize, Vec<E>)> {
    fn go(
        adj: &BTreeMap<usize, Vec<(usize, E)>>,
        s: usize,
        x: usize,
        banned: &BTreeSet<usize>,
        on: &mut Vec<usize>,
        path: &mut Vec<E>,
        out: &mut Vec<(usize, Vec<E>)>,
    ) {
        if x != s && banned.contains(&x) {
            return;
        }
        for &(y, t) in adj.get(&x).map(Vec::as_slice).unwrap_or(&[]) {
            if y != s && on.contains(&y) {
                continue;
            }
            path.push(t);
            out.push((y, path.clone()));
            if y != s {
                on.push(y);
                go(adj, s, y, banned, on, path, out);
                on.pop();
            }
            path.pop();
        }
    }
    let mut out = Vec::new();
    go(
        adj,
        s,
        s,
        inner_banned,
        &mut vec![s],
        &mut Vec::new(),
        &mut out,
    );
    out
}

/// The weights drawn: `None` is an edge without a weight, which weighs 1. The decimals
/// tie only in some orders of addition, as 0.1 + 0.2 + 0.3 does with 0.3 + 0.2 + 0.1.
const WEIGHTS: [Option<f64>; 10] = [
    None,
    Some(0.0),
    Some(0.0),
    Some(1.0),
    Some(2.0),
    Some(0.5),
    Some(0.1),
    Some(0.2),
    Some(0.3),
    Some(3.0),
];

/// Weighted `path:allShortest` from both ends on random graphs, with ties, zero weights,
/// cycles through the source, self-loops and parallel edges, in every direction and with
/// skipped predicates and hub limits. For each pair the paths must be exactly the simple
/// paths of least exact cost, the same as those of the search from the source alone, each
/// reported with its exact cost rounded to a double, and no path may be returned twice.
#[test]
fn random_weighted_all_shortest_from_both_ends_against_one_end() {
    let mut r = Rng(0x2770_a115_b07e);
    let mut checked = 0usize;
    let mut tied = 0usize;
    for case in 0..120 {
        let n = 5 + r.below(5);
        let m = n + r.below(3 * n);
        let mut triples: BTreeMap<E, Option<f64>> = BTreeMap::new();
        for _ in 0..m {
            let w = WEIGHTS[r.below(WEIGHTS.len())];
            triples.insert((r.below(n), r.below(3), r.below(n)), w);
        }
        let ttl: String = triples
            .iter()
            .map(|((a, p, b), w)| match w {
                Some(w) => format!("ex:n{a} ex:p{p} ex:n{b} {{| ex:w {w:?}e0 |}} .\n"),
                None => format!("ex:n{a} ex:p{p} ex:n{b} .\n"),
            })
            .collect();
        let s = store_of(&ttl);
        let weight = |t: &E| exact(triples[t].unwrap_or(1.0));
        let skip_p2 = r.below(3) == 0;
        let max_degree = [None, None, Some(2), Some(3)][r.below(4)];
        let (dir, back, both) = [
            ("path:forward", false, false),
            ("path:backward", true, false),
            ("path:both", false, true),
        ][r.below(3)];
        // the adjacency in path order
        let mut adj: BTreeMap<usize, Vec<(usize, E)>> = BTreeMap::new();
        for &(a, p, b) in triples.keys() {
            if skip_p2 && p == 2 {
                continue;
            }
            let t = (a, p, b);
            if back || both {
                adj.entry(b).or_default().push((a, t));
            }
            if !back && !(both && a == b) {
                adj.entry(a).or_default().push((b, t));
            }
        }
        let hubs: BTreeSet<usize> = (0..n)
            .filter(|x| max_degree.is_some_and(|l| adj.get(x).map_or(0, Vec::len) > l))
            .collect();
        let src = r.below(n);
        let mut extra = format!("path:direction {dir}");
        if skip_p2 {
            extra.push_str(" ; path:excludePredicate ex:p2");
        }
        if let Some(l) = max_degree {
            extra.push_str(&format!(" ; path:maxDegree {l}"));
        }
        let q = |target: &str, more: &str| {
            format!(
                "SELECT ?t ?path ?i ?s ?p ?o ?c WHERE {{ {more} SERVICE path:search {{ [] \
                 path:source ex:n{src} ; path:target {target} ; path:weight ex:w ; {extra} ; \
                 path:algorithm path:allShortest ; path:cost ?c ; path:pathIndex ?path ; \
                 path:edgeIndex ?i ; path:edgeSubject ?s ; path:edgePredicate ?p ; \
                 path:edgeObject ?o }} }}"
            )
        };
        // the search from the source alone, to every node
        let one_end = rows(&s, &q("?t", ""));
        for t in 0..n {
            let ctx = format!("case {case} {extra} n{src}→n{t}");
            // a hub may end a path, but no node in between may be one, and the source and
            // target are never hubs
            let mut banned = hubs.clone();
            banned.remove(&src);
            banned.remove(&t);
            let all: Vec<(Vec<E>, i128)> = brute(&adj, src, &banned)
                .into_iter()
                .filter(|(e, _)| *e == t)
                .map(|(_, p)| {
                    let c = p.iter().map(weight).sum();
                    (p, c)
                })
                .collect();
            let least = all.iter().map(|x| x.1).min();
            let want: BTreeSet<&Vec<E>> = all
                .iter()
                .filter(|x| Some(x.1) == least)
                .map(|x| &x.0)
                .collect();
            let check = |got: &[Found], what: &str| {
                let set: BTreeSet<&Vec<E>> = got.iter().map(|x| &x.0).collect();
                assert_eq!(set.len(), got.len(), "{ctx}, {what}: a path returned twice");
                assert_eq!(set, want, "{ctx}, {what}: the cheapest paths");
                for (p, c) in got {
                    assert_eq!(
                        *c,
                        rounded(least.unwrap()),
                        "{ctx}, {what}: the cost of {p:?}"
                    );
                }
            };
            let pair = paths_of(&rows(&s, &q(&format!("ex:n{t}"), "")), None);
            check(&pair, "from both ends");
            // path:shortest returns one of them, a cycle through the source too
            let one = paths_of(
                &rows(
                    &s,
                    &q(&format!("ex:n{t}"), "").replace("path:allShortest", "path:shortest"),
                ),
                None,
            );
            assert_eq!(one.len(), usize::from(!want.is_empty()), "{ctx}: shortest");
            if let Some((p, c)) = one.first() {
                assert!(want.contains(p), "{ctx}: shortest {p:?}");
                assert_eq!(*c, rounded(least.unwrap()), "{ctx}: the cost of {p:?}");
            }
            // the target bound beside another one runs from the source alone
            let other = (t + 1) % n;
            let two = paths_of(
                &rows(
                    &s,
                    &q("?t", &format!("VALUES ?t {{ ex:n{t} ex:n{other} }}")),
                ),
                Some(&format!("n{t}")),
            );
            if other != t {
                check(&two, "with two targets");
            }
            let mut unbound = paths_of(&one_end, Some(&format!("n{t}")));
            if t == src {
                // to every node, the source's own row is the cycle through it
                unbound.retain(|(p, _)| !p.is_empty());
            }
            check(&unbound, "to every node");
            // a limit stops after that many of the same paths
            if want.len() > 1 {
                tied += 1;
                let limited = paths_of(
                    &rows(
                        &s,
                        &q(&format!("ex:n{t}"), "")
                            .replace("path:algorithm", "path:limit 1 ; path:algorithm"),
                    ),
                    None,
                );
                assert_eq!(limited.len(), 1, "{ctx}: path:limit 1");
                assert!(want.contains(&limited[0].0), "{ctx}: path:limit 1");
            }
            checked += 1;
        }
    }
    assert!(checked > 500, "{checked} pairs");
    assert!(tied > 50, "only {tied} pairs had tied paths");
}

fn sequences(s: &Store, params: &str) -> BTreeSet<String> {
    let r = rows(
        s,
        &format!(
            "SELECT ?path ?i ?s ?o WHERE {{ SERVICE path:search {{ [] {params} ; \
             path:pathIndex ?path ; path:edgeIndex ?i ; path:edgeSubject ?s ; \
             path:edgeObject ?o }} }}"
        ),
    );
    let mut by: BTreeMap<String, BTreeMap<usize, (String, String)>> = BTreeMap::new();
    for x in &r {
        by.entry(x["path"].clone())
            .or_default()
            .insert(x["i"].parse().unwrap(), (x["s"].clone(), x["o"].clone()));
    }
    by.into_values()
        .map(|edges| {
            let mut nodes = vec![edges[&0].0.clone()];
            nodes.extend(edges.into_values().map(|e| e.1));
            nodes.join(" ")
        })
        .collect()
}

/// Weights that tie only as exact sums: `s a b t` adds 0.1, 0.2 and 0.3 and `s c d t` adds
/// them the other way round. In plain doubles the first sums to 0.6000000000000001 and
/// the second to 0.6, so only one of them was the cheapest. Both are found, with the same
/// cost, from both ends and from the source alone.
#[test]
fn all_shortest_ties_on_exact_sums() {
    let s = store_of(
        "ex:s ex:p ex:a {| ex:w 0.1e0 |} . ex:a ex:p ex:b {| ex:w 0.2e0 |} .
         ex:b ex:p ex:t {| ex:w 0.3e0 |} . ex:s ex:p ex:c {| ex:w 0.3e0 |} .
         ex:c ex:p ex:d {| ex:w 0.2e0 |} . ex:d ex:p ex:t {| ex:w 0.1e0 |} .",
    );
    let want: BTreeSet<String> = ["s a b t".to_string(), "s c d t".to_string()].into();
    let params = "path:source ex:s ; path:weight ex:w ; path:algorithm path:allShortest";
    assert_eq!(sequences(&s, &format!("{params} ; path:target ex:t")), want);
    let costs: BTreeSet<String> = rows(
        &s,
        &format!(
            "SELECT ?t ?c WHERE {{ SERVICE path:search {{ [] {params} ; path:target ?t ; \
             path:cost ?c }} }}"
        ),
    )
    .into_iter()
    .filter(|r| r["t"] == "t")
    .map(|r| r["c"].clone())
    .collect();
    assert_eq!(costs.len(), 1, "{costs:?}");
    let c: f64 = costs.into_iter().next().unwrap().parse().unwrap();
    assert_eq!(c, rounded(exact(0.1) + exact(0.2) + exact(0.3)));
}

/// A zero-weight chain where the two sides meet: `s a b c t` costs 2 through the
/// zero-weight edges `a b` and `b c`, which also run back from `c` to `b` and `b` to `a`,
/// and `s x t` costs 2 too. Each path is found once although the sides reach every node
/// of the chain at the same cost.
#[test]
fn all_shortest_meets_once_on_a_zero_weight_chain() {
    let s = store_of(
        "ex:s ex:p ex:a {| ex:w 1 |} . ex:a ex:p ex:b {| ex:w 0 |} .
         ex:b ex:p ex:c {| ex:w 0 |} . ex:c ex:p ex:t {| ex:w 1 |} .
         ex:c ex:p ex:b {| ex:w 0 |} . ex:b ex:p ex:a {| ex:w 0 |} .
         ex:s ex:p ex:x {| ex:w 1 |} . ex:x ex:p ex:t {| ex:w 1 |} .
         ex:a ex:p ex:t {| ex:w 2 |} .",
    );
    let r = rows(
        &s,
        "SELECT ?path ?i WHERE { SERVICE path:search { [] path:source ex:s ; \
         path:target ex:t ; path:weight ex:w ; path:algorithm path:allShortest ; \
         path:pathIndex ?path ; path:edgeIndex ?i } }",
    );
    let paths: BTreeSet<&String> = r.iter().map(|x| &x["path"]).collect();
    assert_eq!(paths.len(), 2, "{r:?}");
    let want: BTreeSet<String> = ["s a b c t".to_string(), "s x t".to_string()].into();
    let params = "path:source ex:s ; path:target ex:t ; path:weight ex:w ; \
                  path:algorithm path:allShortest";
    assert_eq!(sequences(&s, params), want);
    // followed both ways, `a b` and `b c` are each two parallel edges, so `s a b c t` is
    // four paths besides `s x t`
    let r = rows(
        &s,
        &format!(
            "SELECT ?path ?i WHERE {{ SERVICE path:search {{ [] {params} ; \
             path:direction path:both ; path:pathIndex ?path ; path:edgeIndex ?i }} }}"
        ),
    );
    let paths: BTreeSet<&String> = r.iter().map(|x| &x["path"]).collect();
    assert_eq!(paths.len(), 5, "{r:?}");
}

/// The cheapest cycles through the source, from both ends: `s a s` and `s b c s` cost 2,
/// `s d s` costs 3, and a zero-weight self-loop on `a` is not followed. With
/// `path:minLength 0` the only path is the empty one.
#[test]
fn all_shortest_cycles_through_the_source() {
    let s = store_of(
        "ex:s ex:p ex:a {| ex:w 1 |} . ex:a ex:p ex:s {| ex:w 1 |} .
         ex:s ex:p ex:b {| ex:w 1 |} . ex:b ex:p ex:c {| ex:w 0 |} .
         ex:c ex:p ex:s {| ex:w 1 |} . ex:s ex:p ex:d {| ex:w 1 |} .
         ex:d ex:p ex:s {| ex:w 2 |} . ex:a ex:p ex:a {| ex:w 0 |} .",
    );
    let params = "path:source ex:s ; path:target ex:s ; path:weight ex:w ; \
                  path:algorithm path:allShortest";
    let want: BTreeSet<String> = ["s a s".to_string(), "s b c s".to_string()].into();
    assert_eq!(sequences(&s, params), want);
    let r = rows(
        &s,
        &format!(
            "SELECT ?l WHERE {{ SERVICE path:search {{ [] {params} ; path:minLength 0 ; \
             path:length ?l }} }}"
        ),
    );
    assert_eq!(r.len(), 1);
    assert_eq!(r[0]["l"], "0");
    // a self-loop on the source is the cheapest cycle when it weighs least
    let s = store_of(
        "ex:s ex:p ex:a {| ex:w 1 |} . ex:a ex:p ex:s {| ex:w 1 |} .
         ex:s ex:p ex:s {| ex:w 2 |} . ex:s ex:q ex:s {| ex:w 2 |} .",
    );
    let r = rows(
        &s,
        &format!(
            "SELECT ?path ?i ?p WHERE {{ SERVICE path:search {{ [] {params} ; \
             path:pathIndex ?path ; path:edgeIndex ?i ; path:edgePredicate ?p }} }}"
        ),
    );
    let paths: BTreeSet<&String> = r.iter().map(|x| &x["path"]).collect();
    assert_eq!(paths.len(), 3, "{r:?}");
}

/// A chain of 32 diamonds of unit weights closed back to the source has 2^32 cheapest
/// cycles through it. `path:shortest` returns one of them at once, since it follows first
/// parents only, and `path:allShortest` with a limit stops after that many.
#[test]
fn one_cheapest_cycle_through_many_tied_ones() {
    let mut ttl = String::new();
    for i in 0..32 {
        let (a, b) = (format!("ex:c{i}"), format!("ex:c{}", i + 1));
        ttl += &format!(
            "{a} ex:p ex:u{i} {{| ex:w 1 |}} . ex:u{i} ex:p {b} {{| ex:w 1 |}} . \
             {a} ex:p ex:d{i} {{| ex:w 1 |}} . ex:d{i} ex:p {b} {{| ex:w 1 |}} . "
        );
    }
    ttl += "ex:c32 ex:p ex:c0 {| ex:w 1 |} .";
    let s = store_of(&ttl);
    let opts = QueryOptions {
        timeout: Some(std::time::Duration::from_secs(10)),
        ..Default::default()
    };
    for (alg, n) in [("path:shortest", 1), ("path:allShortest ; path:limit 3", 3)] {
        let r = query(
            s.snapshot(),
            &format!(
                "{PREFIXES}SELECT ?path ?l ?c WHERE {{ SERVICE path:search {{ [] path:source ex:c0 ; \
                 path:target ex:c0 ; path:weight ex:w ; path:algorithm {alg} ; \
                 path:pathIndex ?path ; path:length ?l ; path:cost ?c }} }}"
            ),
            &opts,
        )
        .unwrap_or_else(|e| panic!("{alg}: {e}"));
        assert_eq!(r.len(), n, "{alg}");
    }
}

/// Weights at the largest double make every cost of a path past the source's first edge
/// infinite. The path is still found, by the search from the source alone.
#[test]
fn all_shortest_with_infinite_costs() {
    let w = f64::MAX;
    let s = store_of(&format!(
        "ex:s ex:p ex:a {{| ex:w {w:e} |}} . ex:a ex:p ex:b {{| ex:w {w:e} |}} .
         ex:b ex:p ex:c {{| ex:w {w:e} |}} . ex:c ex:p ex:t {{| ex:w {w:e} |}} ."
    ));
    let params = "path:source ex:s ; path:target ex:t ; path:weight ex:w ; \
                  path:algorithm path:allShortest";
    assert_eq!(
        sequences(&s, params),
        BTreeSet::from(["s a b c t".to_string()])
    );
}

/// Weights 2^106 apart: double-double sums are no longer exact, so the two sides of a
/// search from both ends could add the same total differently. `s a b c t` weighs 1, 2^106,
/// 1 and 2^53, and `s d e f t` weighs 2^53, 1, 1 and 2^106, with a dead end `s z` of 1e300.
/// Both paths are found, as the search from the source alone finds them.
#[test]
fn all_shortest_with_weights_too_wide_to_add_exactly() {
    let (big, mid) = ("81129638414606681695789005144064e0", "9007199254740992e0");
    let s = store_of(&format!(
        "ex:s ex:p ex:a {{| ex:w 1 |}} . ex:a ex:p ex:b {{| ex:w {big} |}} .
         ex:b ex:p ex:c {{| ex:w 1 |}} . ex:c ex:p ex:t {{| ex:w {mid} |}} .
         ex:s ex:p ex:d {{| ex:w {mid} |}} . ex:d ex:p ex:e {{| ex:w 1 |}} .
         ex:e ex:p ex:f {{| ex:w 1 |}} . ex:f ex:p ex:t {{| ex:w {big} |}} .
         ex:s ex:p ex:z {{| ex:w 1e300 |}} ."
    ));
    let params = "path:source ex:s ; path:weight ex:w ; path:algorithm path:allShortest";
    let want = BTreeSet::from(["s a b c t".to_string(), "s d e f t".to_string()]);
    assert_eq!(sequences(&s, &format!("{params} ; path:target ex:t")), want);
}

/// A complete graph of 200 nodes where every edge weighs zero has a tie at every node.
/// `path:shortest` from a node to itself keeps first parents and one meeting, so it holds
/// about as much memory as the cheapest path between two of the nodes, while
/// `path:allShortest` keeps every tied parent and meeting.
#[test]
fn one_cheapest_cycle_in_a_dense_zero_weight_graph() {
    let n = 200;
    let mut ttl = String::new();
    for i in 0..n {
        for j in (0..n).filter(|&j| j != i) {
            ttl += &format!("ex:n{i} ex:p ex:n{j} . ");
        }
    }
    let s = store_of(&ttl);
    let peak = |target: &str, alg: &str| {
        let r = query(
            s.snapshot(),
            &format!(
                "{PREFIXES}SELECT ?c WHERE {{ SERVICE path:search {{ [] path:source ex:n0 ; \
                 path:target {target} ; path:weight ex:w ; path:defaultWeight 0 ; \
                 path:algorithm {alg} ; path:limit 1 ; path:cost ?c }} }}"
            ),
            &QueryOptions::default(),
        )
        .unwrap();
        assert_eq!(r.len(), 1, "{target} {alg}");
        r.mem_peak_bytes
    };
    let pair = peak("ex:n1", "path:shortest");
    let cycle = peak("ex:n0", "path:shortest");
    let all = peak("ex:n0", "path:allShortest");
    assert!(cycle <= 2 * pair, "cycle {cycle}, pair {pair}");
    assert!(all > 4 * cycle, "all {all}, cycle {cycle}");
}

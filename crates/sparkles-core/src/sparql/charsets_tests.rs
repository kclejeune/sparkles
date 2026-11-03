//! Join estimates for subject stars from characteristic sets: the sets a build counts,
//! exact estimates for stars of independent predicates, and the same answers with the
//! estimates on and off.

use super::*;
use crate::io::{RdfFormat, Source};
use crate::store::{Store, StoreOptions};

const PREFIXES: &str = "PREFIX ex: <http://ex.org/> ";

/// Estimates from characteristic sets, without those from probed values that a small
/// store would otherwise use for most joins.
fn sets() -> Optimizations {
    Optimizations {
        probed_keys: false,
        ..Optimizations::ALL
    }
}

fn off() -> Optimizations {
    Optimizations {
        characteristic_sets: false,
        ..sets()
    }
}

/// The subject sets alone, without the node sets.
fn subject_sets() -> Optimizations {
    Optimizations {
        node_sets: false,
        ..sets()
    }
}

fn opts(opt: Optimizations) -> QueryOptions {
    QueryOptions {
        optimizations: Some(opt),
        no_cache: true,
        ..Default::default()
    }
}

fn load(ttl: String) -> Store {
    let s = Store::in_memory(StoreOptions::default());
    s.load(&[Source::from_bytes(
        ttl.into_bytes(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
    s
}

/// `n` subjects: every one has `ex:a`, every second `ex:b`, every third two values of
/// `ex:c`, and every fifth an `ex:d` that links to another subject. `ex:b`, `ex:c` and
/// `ex:d` occur independently of each other.
fn subjects(n: usize) -> String {
    let mut t = String::from("@prefix ex: <http://ex.org/> .\n");
    for i in 0..n {
        t.push_str(&format!("ex:s{i} ex:a {i} .\n"));
        if i % 2 == 0 {
            t.push_str(&format!("ex:s{i} ex:b \"b{i}\" .\n"));
        }
        if i % 3 == 0 {
            t.push_str(&format!("ex:s{i} ex:c 1, 2 .\n"));
        }
        if i % 5 == 0 {
            t.push_str(&format!("ex:s{i} ex:d ex:s{} .\n", (i * 7) % n));
        }
    }
    t
}

/// The joins of a plan: their estimated rows.
fn joins(p: &PlanInfo, out: &mut Vec<f64>) {
    if p.operator.contains("Join") {
        out.push(p.estimated_rows);
    }
    for c in &p.children {
        joins(c, out);
    }
}

fn root_est(snap: &Arc<crate::store::Snapshot>, q: &str, opt: Optimizations) -> f64 {
    let (_, info) = explain(snap.clone(), &format!("{PREFIXES}{q}"), &opts(opt))
        .unwrap_or_else(|e| panic!("{q}: {e}"));
    let mut js = Vec::new();
    joins(&info, &mut js);
    js[0]
}

fn answer(snap: &Arc<crate::store::Snapshot>, q: &str, opt: Optimizations) -> Vec<String> {
    let r = query(snap.clone(), &format!("{PREFIXES}{q}"), &opts(opt))
        .unwrap_or_else(|e| panic!("{q}: {e}"));
    let mut rows: Vec<String> = r
        .rows()
        .into_iter()
        .map(|row| {
            row.into_iter()
                .map(|t| t.map_or("UNDEF".to_string(), |t| t.to_string()))
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect();
    rows.sort();
    rows
}

#[test]
fn builds_count_characteristic_sets() {
    let s = load(subjects(60));
    let snap = s.snapshot();
    let stats = &snap.generation.stats;
    // a, ab, ac, ad, abc, abd, acd, abcd
    assert_eq!(stats.charsets.len(), 8, "{:?}", stats.charsets);
    assert_eq!(stats.charset_others, 0);
    let subjects: u64 = stats.charsets.iter().map(|c| c.subjects).sum();
    assert_eq!(subjects, 60);
    // the 2 subjects divisible by 30 have all four predicates, two values of ex:c each
    let all = stats.charsets.iter().find(|c| c.preds.len() == 4).unwrap();
    assert_eq!(all.subjects, 2);
    assert_eq!(all.triples, vec![2, 2, 4, 2]);
}

/// A star of independent predicates: the estimate from the sets is the number of rows,
/// where the estimate from distinct values assumes every subject with the rarer
/// predicate has the others.
#[test]
fn stars_of_independent_predicates_are_estimated_exactly() {
    let n = 600;
    let s = load(subjects(n));
    let snap = s.snapshot();
    for (q, rows) in [
        ("SELECT * WHERE { ?s ex:b ?b ; ex:c ?c }", 200.0),
        ("SELECT * WHERE { ?s ex:b ?b ; ex:d ?d }", 60.0),
        (
            "SELECT * WHERE { ?s ex:a ?a ; ex:b ?b ; ex:c ?c ; ex:d ?d }",
            40.0,
        ),
    ] {
        assert_eq!(answer(&snap, q, sets()).len() as f64, rows, "{q}");
        let est = root_est(&snap, q, sets());
        assert!((est - rows).abs() < 0.5, "{q}: estimated {est} for {rows}");
        let old = root_est(&snap, q, off());
        assert!(old > rows * 1.3, "{q}: {old} without the sets");
    }
}

/// Classes count as items of the sets: the instances of `ex:C1` all have `ex:p`, those
/// of `ex:C2` none, and other subjects have `ex:p` without a class.
#[test]
fn classes_are_items_of_the_sets() {
    let mut t = String::from("@prefix ex: <http://ex.org/> .\n");
    for i in 0..600 {
        match i % 3 {
            0 => t.push_str(&format!("ex:s{i} a ex:C1 ; ex:p {i} .\n")),
            1 => t.push_str(&format!("ex:s{i} a ex:C2 .\n")),
            _ => t.push_str(&format!("ex:s{i} ex:p {i} .\n")),
        }
    }
    let s = load(t);
    let snap = s.snapshot();
    for (q, rows) in [
        ("SELECT * WHERE { ?s a ex:C1 ; ex:p ?o }", 200.0),
        ("SELECT * WHERE { ?s a ex:C2 ; ex:p ?o }", 0.0),
    ] {
        assert_eq!(answer(&snap, q, sets()).len() as f64, rows, "{q}");
        let est = root_est(&snap, q, sets());
        assert!(
            (est - rows.max(1.0)).abs() < 0.5,
            "{q}: estimated {est} for {rows}"
        );
    }
}

/// Patterns with a constant object other than a class, or a predicate that occurs twice,
/// keep the estimate from distinct values, and so does a chain without the node sets.
#[test]
fn other_patterns_keep_the_estimate_from_distinct_values() {
    let s = load(subjects(300));
    let snap = s.snapshot();
    for q in [
        "SELECT * WHERE { ?s ex:b ?b ; ex:c 2 }",
        "SELECT * WHERE { ?s ex:c ?x ; ex:c ?y }",
        "SELECT * WHERE { ?s ex:d ?o . ?o ex:b ?b }",
        "SELECT * WHERE { ?s ex:d ?s . ?s ex:b ?b }",
    ] {
        assert_eq!(
            root_est(&snap, q, subject_sets()),
            root_est(&snap, q, off()),
            "{q}"
        );
    }
}

/// Random stars, chains and filters: the same solutions with the estimates on and off
/// (a debug build also checks that the join ordering's summaries match the plans).
#[test]
fn estimates_from_sets_never_change_answers() {
    let s = load(subjects(240));
    let snap = s.snapshot();
    let mut x = 0x9e37_79b9_7f4a_7c15u64;
    let mut next = move || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    let preds = ["ex:a", "ex:b", "ex:c", "ex:d"];
    for round in 0..60 {
        let n = 2 + round % 5;
        let mut pats = Vec::new();
        for i in 0..n {
            let s = if i > 0 && next() % 4 == 0 {
                "?o0"
            } else {
                "?s"
            };
            let p = preds[next() as usize % 4];
            let o = match next() % 6 {
                0 => "1".to_string(),
                1 if p == "ex:d" => "?s".to_string(),
                _ => format!("?o{i}"),
            };
            pats.push(format!("{s} {p} {o}"));
        }
        let mut q = format!("SELECT * WHERE {{ {} .", pats.join(" . "));
        if round % 3 == 0 {
            q.push_str(" FILTER(STRLEN(STR(?s)) > 6)");
        }
        q.push_str(" }");
        assert_eq!(answer(&snap, &q, sets()), answer(&snap, &q, off()), "{q}");
    }
}

/// The node sets count every node by its outgoing and incoming predicates and classes, and
/// the remainders are empty when every set is kept.
#[test]
fn builds_count_node_sets() {
    let s = load(subjects(60));
    let snap = s.snapshot();
    let stats = &snap.generation.stats;
    let c = stats.correlations.get().expect("correlations");
    assert_eq!(c.version, crate::builder::correlations::VERSION);
    assert_eq!(c.subject_rest.n, 0);
    assert_eq!(c.node_rest.n, 0);
    // 60 subjects, the integers 0 to 59 (ex:c's 1 and 2 among them) and 30 strings
    let nodes: u64 = c.node_sets.iter().map(|n| n.subjects).sum();
    assert_eq!(nodes, 150, "{:?}", c.node_sets);
    // every quad is an outgoing item of its subject and an incoming item of its object
    let (mut out, mut inn) = (0, 0);
    for n in &c.node_sets {
        for (&i, &t) in n.preds.iter().zip(&n.triples) {
            if crate::builder::correlations::is_in_item(i) {
                inn += t;
            } else if crate::builder::item_class(i).is_none() {
                out += t;
            }
        }
    }
    assert_eq!((out, inn), (stats.quads, stats.quads));
    assert!(crate::builder::correlations::check(stats, c).is_none());
}

/// Joins on a variable that is an object, chains and object joins, are estimated from the
/// node sets: exactly here, where every node of a set has the same number of triples of
/// each item, and far from it without them.
#[test]
fn object_joins_and_chains_are_estimated_from_node_sets() {
    let n = 600;
    let s = load(subjects(n));
    let snap = s.snapshot();
    for q in [
        // ex:d points at subjects, half of which have ex:b
        "SELECT * WHERE { ?x ex:d ?y . ?y ex:b ?b }",
        // ex:c's objects 1 and 2 are also objects of ex:a, once each
        "SELECT * WHERE { ?x ex:a ?v . ?y ex:c ?v }",
        // the subjects that ex:d points at, by their own ex:c
        "SELECT * WHERE { ?x ex:d ?y . ?y ex:c ?c }",
    ] {
        let rows = answer(&snap, q, sets()).len() as f64;
        let est = root_est(&snap, q, sets());
        assert!(
            (est - rows.max(1.0)).abs() <= 0.5 + rows * 0.02,
            "{q}: estimated {est} for {rows}"
        );
        let old = root_est(&snap, q, subject_sets());
        let q_err = |e: f64| e.max(rows).max(1.0) / e.min(rows).max(1.0);
        assert!(
            q_err(old) > q_err(est),
            "{q}: {old} without the node sets, {est} with"
        );
        assert_eq!(
            answer(&snap, q, sets()),
            answer(&snap, q, subject_sets()),
            "{q}"
        );
    }
}

/// The generation directory of an on-disk store.
fn gen_dir(root: &std::path::Path) -> std::path::PathBuf {
    let mut gens: Vec<_> = std::fs::read_dir(root)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("gen-"))
        })
        .collect();
    gens.sort();
    gens.pop().expect("a generation")
}

/// Rewrite the statistics of the store at `root`, then open it again.
fn with_stats(root: &std::path::Path, f: impl FnOnce(&mut serde_json::Value)) -> Store {
    let path = gen_dir(root).join("stats.json");
    let mut v: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    f(&mut v);
    std::fs::write(&path, serde_json::to_vec(&v).unwrap()).unwrap();
    Store::open(root, StoreOptions::default()).unwrap()
}

/// A store whose sets all moved into the remainder estimates stars of independent items
/// from it as the kept sets did, and one without a remainder keeps the coverage rule.
#[test]
fn stars_are_estimated_from_the_remainder() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("db");
    let q = "SELECT * WHERE { ?s ex:b ?b ; ex:c ?c ; ex:d ?d }";
    let (kept, rows) = {
        let s = Store::open(&root, StoreOptions::default()).unwrap();
        s.load(&[Source::from_bytes(
            subjects(600).into_bytes(),
            RdfFormat::Turtle,
            None,
        )])
        .unwrap();
        let snap = s.snapshot();
        (
            root_est(&snap, q, subject_sets()),
            answer(&snap, q, sets()).len() as f64,
        )
    };
    assert!((kept - rows).abs() < 0.5, "{kept} for {rows}");
    // every set into the remainder
    let s = with_stats(&root, |v| {
        let sets = v["charsets"].as_array().unwrap().clone();
        let mut n = 0u64;
        let mut items: std::collections::BTreeMap<u64, (u64, u64)> = Default::default();
        for c in &sets {
            let h = c["subjects"].as_u64().unwrap();
            n += h;
            for (p, t) in c["preds"]
                .as_array()
                .unwrap()
                .iter()
                .zip(c["triples"].as_array().unwrap())
            {
                let e = items.entry(p.as_u64().unwrap()).or_default();
                e.0 += h;
                e.1 += t.as_u64().unwrap();
            }
        }
        v["charsets"] = serde_json::json!([]);
        v["charset_others"] = serde_json::json!(n);
        v["correlations"]["subjectRest"] = serde_json::json!({
            "n": n,
            "items": items.iter().map(|(&i, &(h, t))| [i, h, t]).collect::<Vec<_>>(),
        });
    });
    let snap = s.snapshot();
    let est = root_est(&snap, q, subject_sets());
    assert!((est - rows).abs() <= rows * 0.05, "{est} for {rows}");
    // without the remainder, no item is covered by the kept sets
    let old = root_est(
        &snap,
        q,
        Optimizations {
            set_remainders: false,
            ..subject_sets()
        },
    );
    assert_eq!(old, root_est(&snap, q, off()));
    assert_eq!(answer(&snap, q, sets()), answer(&snap, q, off()));
}

/// Correlations of a version this build does not read are refused with a reason, the
/// store opens, and the planner estimates as without them; a version after the other
/// members is read as well as one before them.
#[test]
fn correlations_of_another_version_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("db");
    let q = "SELECT * WHERE { ?x ex:d ?y . ?y ex:b ?b }";
    {
        let s = Store::open(&root, StoreOptions::default()).unwrap();
        s.load(&[Source::from_bytes(
            subjects(300).into_bytes(),
            RdfFormat::Turtle,
            None,
        )])
        .unwrap();
    }
    // the version last: the same statistics
    let s = with_stats(&root, |v| {
        let c = v["correlations"].as_object_mut().unwrap();
        let version = c.remove("version").unwrap();
        c.insert("version".into(), version);
    });
    let snap = s.snapshot();
    assert!(snap.generation.stats.correlations.get().is_some());
    let with = root_est(&snap, q, sets());
    drop((snap, s));
    let s = with_stats(&root, |v| {
        v["correlations"] = serde_json::json!({ "version": 2, "nodeSets": "a shape of later" });
    });
    let snap = s.snapshot();
    let c = &snap.generation.stats.correlations;
    assert!(c.get().is_none());
    let why = c.refusal().expect("a reason");
    assert!(why.contains("version 2"), "{why}");
    let refused = root_est(&snap, q, sets());
    assert_eq!(refused, root_est(&snap, q, subject_sets()));
    assert_ne!(refused, with);
    let rep = crate::check::check(&root, &Default::default()).unwrap();
    assert!(rep.to_text().contains("version 2"), "{}", rep.to_text());
    // the next write of the statistics leaves the refused member out
    let j = serde_json::to_value(&snap.generation.stats).unwrap();
    assert!(j.get("correlations").is_none(), "{j}");
}

/// The node pass counts each signature in one place, its set or the remainder, whichever
/// thread meets its nodes: with fewer sets admitted than there are, every set kept holds
/// all the nodes of its signature, as without a limit, and the remainder the others. The
/// data spans several blocks of SPO and OPS, so the pass splits it into several ranges,
/// and each range meets the 64 signatures in another order.
#[test]
fn node_sets_past_the_limit_keep_whole_signatures() {
    use crate::builder::correlations::node_sets_seen;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("db");
    // subject i has the predicates p(j) for the bits j of i % 64, and ex:q
    let mut t = String::from("@prefix ex: <http://ex.org/> .\n");
    for i in 0..30_000u32 {
        for j in 0..6 {
            if (i % 64) >> j & 1 == 1 {
                t.push_str(&format!("ex:s{i} ex:p{j} ex:o{} .\n", i % 13));
            }
        }
        t.push_str(&format!("ex:s{i} ex:q {i} .\n"));
    }
    let s = Store::open(&root, StoreOptions::default()).unwrap();
    s.load(&[Source::from_bytes(t.into_bytes(), RdfFormat::Turtle, None)])
        .unwrap();
    assert!(s.snapshot().generation.stats.quads > 3 * 32_768);
    drop(s);
    // the data has no rdf:type, so no classes
    let rdf_type = None;
    let generation = gen_dir(&root);
    let never = || Ok(());
    let (all, all_rest) = node_sets_seen(&generation, rdf_type, 8, usize::MAX, &never).unwrap();
    assert_eq!(all_rest.n, 0);
    let full: std::collections::HashMap<Vec<u64>, (u64, Vec<u64>)> = all
        .iter()
        .map(|c| (c.preds.clone(), (c.subjects, c.triples.clone())))
        .collect();
    for seen in [3, 10, 40, 60] {
        let (kept, rest) = node_sets_seen(&generation, rdf_type, 8, seen, &never).unwrap();
        let mut nodes = rest.n;
        for c in &kept {
            assert_eq!(
                full.get(&c.preds),
                Some(&(c.subjects, c.triples.clone())),
                "seen {seen}: a kept set lacks some of its nodes"
            );
            nodes += c.subjects;
        }
        let total: u64 = all.iter().map(|c| c.subjects).sum();
        assert_eq!(nodes, total, "seen {seen}");
        assert!(kept.len() <= seen, "seen {seen}: {} sets kept", kept.len());
    }
}

/// A node-family variable that a constant object also restricts is estimated from its
/// outgoing items over the subject sets, which treat the restriction as independent,
/// instead of falling back to the distinct values: `ex:b` and `ex:e` never share a
/// subject, which the subject sets tell, whatever the subjects of `ex:k` are.
#[test]
fn restricted_node_variables_use_the_subject_sets() {
    let n = 600;
    let mut t = subjects(n);
    for i in 0..n {
        if i % 2 == 1 {
            t.push_str(&format!("ex:s{i} ex:e {i} .\n"));
        }
        if i % 100 == 0 {
            t.push_str(&format!("ex:s{i} ex:k ex:K .\n"));
        }
    }
    let s = load(t);
    let snap = s.snapshot();
    // `?x ex:a ?y` gives `?y` an incoming item, so the node sets estimate its joins
    let q = "SELECT * WHERE { ?y ex:k ex:K . ?y ex:b ?b . ?y ex:e ?e . ?x ex:a ?y }";
    assert!(answer(&snap, q, sets()).is_empty());
    let all = |o| {
        let (_, info) = explain(snap.clone(), &format!("{PREFIXES}{q}"), &opts(o)).unwrap();
        let mut js = Vec::new();
        joins(&info, &mut js);
        js
    };
    let with = all(sets());
    assert_eq!(with, all(subject_sets()));
    assert!(with[0] <= 1.0, "{with:?}");
    assert!(all(off())[0] > 1.0);
    assert_eq!(answer(&snap, q, sets()), answer(&snap, q, off()));
}

/// Data with more node signatures than a small cap: subject `i` has the predicates `p(j)`
/// for the bits `j` of `i % 64` and `i % 7`, and `ex:q`, so the signatures differ in how
/// many nodes they hold.
fn many_signatures() -> String {
    let mut t = String::from("@prefix ex: <http://ex.org/> .\n");
    for i in 0..20_000u32 {
        let bits = (i % 64) | ((i % 7) << 6);
        for j in 0..9 {
            if bits >> j & 1 == 1 {
                t.push_str(&format!("ex:s{i} ex:p{j} ex:o{} .\n", i % 13));
            }
        }
        t.push_str(&format!("ex:s{i} ex:q {} .\n", i % 500));
    }
    t
}

/// The node pass keeps the same sets and remainder at every thread count, with and
/// without more signatures than it counts, and past its cap it keeps the signatures with
/// the most nodes.
#[test]
fn node_sets_are_the_same_at_every_thread_count() {
    use crate::builder::correlations::node_sets_seen;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("db");
    let s = Store::open(&root, StoreOptions::default()).unwrap();
    s.load(&[Source::from_bytes(
        many_signatures().into_bytes(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
    drop(s);
    let generation = gen_dir(&root);
    let never = || Ok(());
    let (all, _) = node_sets_seen(&generation, None, 1, usize::MAX, &never).unwrap();
    assert!(all.len() > 100, "{} signatures", all.len());
    for seen in [5, 40, usize::MAX] {
        let one = node_sets_seen(&generation, None, 1, seen, &never).unwrap();
        for threads in [2, 3, 8] {
            let other = node_sets_seen(&generation, None, threads, seen, &never).unwrap();
            assert!(
                one == other,
                "seen {seen}: {threads} threads differ from one"
            );
        }
        if seen < all.len() {
            // the kept sets are those of the most nodes, as the sketch tells them apart
            let (kept, _) = &one;
            assert_eq!(kept.len(), seen);
            let least = kept.iter().map(|c| c.subjects).min().unwrap();
            let left_out = all
                .iter()
                .filter(|c| !kept.iter().any(|k| k.preds == c.preds))
                .map(|c| c.subjects)
                .max()
                .unwrap();
            assert!(
                least >= left_out,
                "seen {seen}: kept {least}, left out {left_out}"
            );
        }
    }
}

/// Loads of the same data at different thread counts write the same `stats.json`, byte
/// for byte, with the node pass under and over its cap.
#[test]
fn loads_at_every_thread_count_write_the_same_statistics() {
    let data = many_signatures();
    for seen in [40, crate::builder::correlations::MAX_NODE_SETS_SEEN] {
        let mut written: Vec<Vec<u8>> = Vec::new();
        for threads in [1, 2, 8] {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path().join("db");
            let opts = StoreOptions {
                build: crate::builder::BuildOptions {
                    threads,
                    node_sets_seen: seen,
                    ..Default::default()
                },
                ..Default::default()
            };
            let s = Store::open(&root, opts).unwrap();
            s.load(&[Source::from_bytes(
                data.clone().into_bytes(),
                RdfFormat::Turtle,
                None,
            )])
            .unwrap();
            drop(s);
            written.push(std::fs::read(gen_dir(&root).join("stats.json")).unwrap());
        }
        assert!(written.iter().all(|w| *w == written[0]), "seen {seen}");
    }
}

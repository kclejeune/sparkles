//! Reports brought up to date from changes ([`update`]) against reports computed from
//! scratch, over random writes, and the copy of small selections ([`Src`]).

use super::*;
use crate::history::At;
use crate::sparql::QueryOptions;
use crate::store::{DiffOptions, Store, StoreOptions};

const INFERRED: &str = "urn:x-sparkles:inferred";

/// A small deterministic generator (xorshift64*).
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    fn pick<'a>(&mut self, xs: &[&'a str]) -> &'a str {
        xs[self.below(xs.len())]
    }
}

const SUBJECTS: [&str; 6] = ["ex:s0", "ex:s1", "ex:s2", "ex:s3", "ex:s4", "ex:s5"];
const CLASSES: [&str; 3] = ["ex:C0", "ex:C1", "ex:C2"];
const OBJECTS: [&str; 16] = [
    "ex:o0",
    "ex:o1",
    "ex:s0",
    "ex:s1",
    "1",
    "2",
    "\"01\"^^xsd:integer",
    "\"a\"",
    "\"a\"@en",
    "\"b\"@de",
    "\"2020-01-01\"^^xsd:date",
    "\"123456789012345678901234567890\"^^xsd:integer",
    "\"x\"^^ex:dt",
    "\"1.5\"^^xsd:decimal",
    "<<( ex:s0 ex:p0 ex:o0 )>>",
    "true",
];
const GRAPHS: [&str; 4] = ["", "ex:g1", "ex:g2", "<urn:x-sparkles:inferred>"];

/// A random triple, and whether it has a fresh blank node (which cannot be deleted by
/// value).
fn triple(r: &mut Rng) -> (String, bool) {
    match r.below(10) {
        0..=4 => {
            let s = r.pick(&SUBJECTS);
            let p = r.pick(&["ex:p0", "ex:p1", "ex:p2"]);
            if r.below(12) == 0 {
                (format!("{s} {p} []"), true)
            } else {
                (format!("{s} {p} {}", r.pick(&OBJECTS)), false)
            }
        }
        5 | 6 => {
            let s = r.pick(&SUBJECTS);
            if r.below(10) == 0 {
                (format!("{s} rdf:type []"), true)
            } else {
                (format!("{s} rdf:type {}", r.pick(&CLASSES)), false)
            }
        }
        7 => {
            let s = r.pick(&[&SUBJECTS[..2], &CLASSES[..], &["ex:p0", "<urn:o>"]].concat());
            let l = r.pick(&["\"L\"@en", "\"M\"", "\"N\"@de"]);
            let p = r.pick(&["rdfs:label", "rdfs:comment", "owl:versionInfo"]);
            (format!("{s} {p} {l}"), false)
        }
        8 => {
            let s = r.pick(&CLASSES);
            match r.below(4) {
                0 => (format!("{s} rdfs:subClassOf []"), true),
                1 => (format!("{s} a owl:Class"), false),
                _ => (format!("{s} rdfs:subClassOf {}", r.pick(&CLASSES)), false),
            }
        }
        _ => match r.below(3) {
            0 => ("<urn:o> a owl:Ontology".to_string(), false),
            1 => (
                format!(
                    "{} rdfs:domain {}",
                    r.pick(&["ex:p0", "ex:p1"]),
                    r.pick(&CLASSES)
                ),
                false,
            ),
            _ => ("ex:p2 a owl:FunctionalProperty".to_string(), false),
        },
    }
}

fn in_graph(g: &str, t: &str) -> String {
    if g.is_empty() {
        format!("{t} .")
    } else {
        format!("GRAPH {g} {{ {t} }}")
    }
}

const PREFIXES: &str = "PREFIX ex: <http://ex.org/>
PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#>
PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
PREFIX owl: <http://www.w3.org/2002/07/owl#>
PREFIX xsd: <http://www.w3.org/2001/XMLSchema#>
";

/// One random write of a few changes (one commit).
fn random_write(r: &mut Rng, known: &mut Vec<(String, String)>) -> String {
    let mut ops = Vec::new();
    for _ in 0..1 + r.below(4) {
        let g = r.pick(&GRAPHS);
        match r.below(10) {
            0..=5 => {
                let (t, fresh) = triple(r);
                if !fresh {
                    known.push((g.to_string(), t.clone()));
                }
                ops.push(format!("INSERT DATA {{ {} }}", in_graph(g, &t)));
            }
            6..=8 if !known.is_empty() => {
                let (g, t) = known.swap_remove(r.below(known.len()));
                ops.push(format!("DELETE DATA {{ {} }}", in_graph(&g, &t)));
            }
            _ => {
                let s = r.pick(&[&SUBJECTS[..], &CLASSES[..]].concat());
                let pat = in_graph(g, &format!("{s} ?p ?o"));
                ops.push(format!("DELETE WHERE {{ {pat} }}"));
            }
        }
    }
    format!("{PREFIXES}{}", ops.join(" ;\n"))
}

fn selections() -> Vec<SchemaOptions> {
    let named = |i: &str| GraphSelection::Named(oxrdf::NamedNode::new(i).unwrap());
    let base = SchemaOptions {
        inferred_graph: Some(INFERRED.into()),
        ..Default::default()
    };
    vec![
        base.clone(),
        SchemaOptions {
            include_inferred: false,
            ..base.clone()
        },
        SchemaOptions {
            graph: GraphSelection::Union,
            include_inferred: false,
            ..base.clone()
        },
        SchemaOptions {
            graph: GraphSelection::Union,
            declared_from_inferred: true,
            ..base.clone()
        },
        SchemaOptions {
            graph: named("http://ex.org/g1"),
            ..base.clone()
        },
        SchemaOptions {
            declared_graph: Some(named("http://ex.org/g2")),
            ..base
        },
    ]
}

/// The report as JSON, without the time it was computed.
fn json(r: &SchemaReport) -> serde_json::Value {
    let mut v = serde_json::to_value(r).unwrap();
    v["snapshot"].as_object_mut().unwrap().remove("computedAt");
    v
}

fn run(store: &Store, steps: usize, seed: u64, compact_every: usize) -> (usize, usize) {
    let mut r = Rng(seed);
    let mut known = Vec::new();
    let opts = selections();
    let mut prev: Vec<Option<SchemaReport>> = vec![None; opts.len()];
    let (mut updated, mut fallbacks) = (0, 0);
    for step in 0..steps {
        let text = random_write(&mut r, &mut known);
        crate::sparql::update::update(store, &text, &QueryOptions::default())
            .unwrap_or_else(|e| panic!("step {step}: {e}\n{text}"));
        if compact_every > 0 && step % compact_every == compact_every - 1 {
            store.compact().unwrap();
        }
        let snap = store.snapshot();
        for (i, o) in opts.iter().enumerate() {
            let full = discover(&snap, o);
            // a compaction collects the old generation, and its commits are gone
            let diff = prev[i].as_ref().and_then(|old| {
                store
                    .diff(
                        &At::Commit(old.snapshot.commit),
                        &At::Commit(snap.commit),
                        &DiffOptions::default(),
                    )
                    .ok()
            });
            let up = match (&prev[i], &diff) {
                (Some(old), Some(diff)) => Some(update(old, &snap, diff, o)),
                _ => None,
            };
            match (full, up) {
                (Ok(full), Some(Ok(Some(up)))) => {
                    assert_eq!(
                        json(&up),
                        json(&full),
                        "step {step}, selection {i}, after:\n{text}"
                    );
                    updated += 1;
                    prev[i] = Some(up);
                }
                (Ok(full), Some(Ok(None))) => {
                    fallbacks += 1;
                    prev[i] = Some(full);
                }
                (Ok(full), None) => prev[i] = Some(full),
                (Err(SchemaError::NoSuchGraph(_)), Some(Err(SchemaError::NoSuchGraph(_))))
                | (Err(SchemaError::NoSuchGraph(_)), None) => prev[i] = None,
                (full, up) => panic!(
                    "step {step}, selection {i}: full {:?}, update {:?}",
                    full.map(|_| ()),
                    up.map(|u| u.map(|x| x.is_some()))
                ),
            }
        }
    }
    (updated, fallbacks)
}

#[test]
fn updates_equal_full_reports() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("db"), StoreOptions::default()).unwrap();
    let (updated, fallbacks) = run(&store, 150, 0x5eed_1234_abcd_0001, 40);
    assert_eq!(fallbacks, 0);
    assert!(updated > 500, "{updated} updates");
}

#[test]
fn updates_equal_full_reports_with_a_union_default_graph() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(
        &dir.path().join("db"),
        StoreOptions {
            union_default_graph: true,
            ..Default::default()
        },
    )
    .unwrap();
    let (updated, fallbacks) = run(&store, 80, 0x0bad_cafe_f00d_0002, 0);
    assert_eq!(fallbacks, 0);
    assert!(updated > 300, "{updated} updates");
}

#[test]
fn no_update_for_other_requests_or_commits() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("db"), StoreOptions::default()).unwrap();
    let up = |u: &str| {
        crate::sparql::update::update(&store, &format!("{PREFIXES}{u}"), &QueryOptions::default())
            .unwrap()
    };
    up("INSERT DATA { ex:a a ex:C ; ex:p 1 }");
    let opts = SchemaOptions::default();
    let old = discover(&store.snapshot(), &opts).unwrap();
    up("INSERT DATA { ex:b a ex:C }");
    up("INSERT DATA { ex:c a ex:C }");
    let snap = store.snapshot();
    let diff = |from: u64| {
        store
            .diff(
                &At::Commit(from),
                &At::Commit(snap.commit),
                &DiffOptions::default(),
            )
            .unwrap()
    };
    let d = diff(old.snapshot.commit);
    let r = update(&old, &snap, &d, &opts).unwrap().unwrap();
    assert_eq!(r.classes[0].observed.instances, 3);
    // a diff from another commit, and requests it cannot maintain
    assert!(
        update(&old, &snap, &diff(old.snapshot.commit + 1), &opts)
            .unwrap()
            .is_none()
    );
    for o in [
        SchemaOptions {
            subject_classes: true,
            ..Default::default()
        },
        SchemaOptions {
            term_totals: true,
            ..Default::default()
        },
    ] {
        assert!(update(&old, &snap, &d, &o).unwrap().is_none());
    }
    let other = SchemaOptions {
        include_inferred: false,
        ..Default::default()
    };
    assert!(update(&old, &snap, &d, &other).unwrap().is_none());
    // a report with subject classes keeps nothing to maintain
    let with = discover(
        &snap,
        &SchemaOptions {
            subject_classes: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(with.maintenance.is_none());
}

#[test]
fn small_named_graphs_are_copied() {
    let store = Store::in_memory(StoreOptions::default());
    let mut text = String::from(PREFIXES);
    text.push_str("INSERT DATA {\n");
    for i in 0..400 {
        text.push_str(&format!("ex:x{i} a ex:Big ; ex:v {i} .\n"));
    }
    text.push_str(
        "GRAPH ex:g { ex:a a ex:Small ; ex:v 1, \"one\"@en ; rdfs:label \"A\" . ex:Small rdfs:subClassOf ex:Thing } }",
    );
    crate::sparql::update::update(&store, &text, &QueryOptions::default()).unwrap();
    let snap = store.snapshot();
    let g = GraphSelection::parse("http://ex.org/g").unwrap();
    let budget = Budget {
        deadline: None,
        cancel: None,
    };
    let opts = SchemaOptions {
        graph: g.clone(),
        ..Default::default()
    };
    let sel = Selected::resolve(&snap, &opts).unwrap();
    let src = Src::for_filters(&snap, &[&sel.observed, &sel.declared], &budget).unwrap();
    assert!(src.is_copied());
    let defaults = SchemaOptions::default();
    let all = Selected::resolve(&snap, &defaults).unwrap();
    let src = Src::for_filters(&snap, &[&all.observed], &budget).unwrap();
    assert!(!src.is_copied(), "the default graph is most of the store");
    // the copied report is the report of the indexes
    let copied = discover(&snap, &opts).unwrap();
    assert_eq!(copied.totals.triples, 5);
    assert_eq!(
        copied
            .classes
            .iter()
            .map(|c| c.iri.as_str())
            .collect::<Vec<_>>(),
        ["http://ex.org/Small", "http://ex.org/Thing"]
    );
    let direct = {
        let budget = Budget {
            deadline: None,
            cancel: None,
        };
        let src = Src::direct(&snap);
        let sel = Selected::resolve(&snap, &opts).unwrap();
        let mut accs = FxHashMap::default();
        for p in snap.distinct_first(Perm::Pso).unwrap() {
            let mut acc = PredAcc::default();
            subject_pass(&src, p, &sel.observed, &budget, &mut acc, None).unwrap();
            if acc.triples > 0 {
                object_pass(&src, p, &sel.observed, &budget, &mut acc, None).unwrap();
                accs.insert(p, acc);
            }
        }
        accs
    };
    for p in &copied.predicates {
        let id = snap.lookup_iri(&p.iri).unwrap().0;
        let d = direct[&id].observed();
        assert_eq!(
            serde_json::to_value(&d).unwrap(),
            serde_json::to_value(&p.observed).unwrap()
        );
    }
    // drafts read the copy as well
    let draft = draft_shapes(
        &snap,
        &DraftOptions {
            schema: opts.clone(),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(draft.shapes.len(), 2);
}

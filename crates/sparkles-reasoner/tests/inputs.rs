//! Input graphs other than the default graph, and `owl:imports`: incremental runs against
//! full ones over random changes to several graphs, imports resolved in the dataset, by a
//! location mapping, and fetched from files and over HTTP.

use oxrdf::{GraphName, NamedNode, Quad, Term, Triple};
use sparkles_core::index::Perm;
use sparkles_core::io::{RdfFormat, Source};
use sparkles_core::sparql::QueryOptions;
use sparkles_core::store::{Store, StoreOptions};
use sparkles_reasoner::{
    Cache, Extras, GraphRef, INFERRED_GRAPH, ImportMode, Incremental, Inputs, LocationMapping,
    Method, Profile, ReasonOptions, ReasonReport, fetch_imports, infer, materialize_incremental,
};
use std::collections::BTreeSet;

const EX: &str = "http://ex.org/";
const RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
const RDFS: &str = "http://www.w3.org/2000/01/rdf-schema#";
const OWL: &str = "http://www.w3.org/2002/07/owl#";

/// xorshift64*
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

fn iri(s: &str) -> NamedNode {
    NamedNode::new_unchecked(s)
}

/// A random triple over a small vocabulary.
fn random_triple(r: &mut Rng, owl: bool) -> Triple {
    let c = |r: &mut Rng| iri(&format!("{EX}C{}", r.below(6)));
    let p = |r: &mut Rng| iri(&format!("{EX}p{}", r.below(5)));
    let i = |r: &mut Rng| iri(&format!("{EX}i{}", r.below(10)));
    let v = |ns: &str, l: &str| iri(&format!("{ns}{l}"));
    let k = r.below(if owl { 13 } else { 10 });
    let (s, pr, o): (NamedNode, NamedNode, Term) = match k {
        0 => (c(r), v(RDFS, "subClassOf"), c(r).into()),
        1 => (p(r), v(RDFS, "subPropertyOf"), p(r).into()),
        2 => (p(r), v(RDFS, "domain"), c(r).into()),
        3 => (p(r), v(RDFS, "range"), c(r).into()),
        4 | 5 => (i(r), v(RDF, "type"), c(r).into()),
        6..=8 => (i(r), p(r), i(r).into()),
        9 => (i(r), p(r), oxrdf::Literal::from(r.below(4) as i64).into()),
        10 => (i(r), v(OWL, "sameAs"), i(r).into()),
        11 => (p(r), v(OWL, "inverseOf"), p(r).into()),
        _ => (
            p(r),
            v(RDF, "type"),
            v(OWL, ["TransitiveProperty", "SymmetricProperty"][r.below(2)]).into(),
        ),
    };
    Triple::new(s, pr, o)
}

fn graph_name(g: &str) -> GraphName {
    if g == "default" {
        GraphName::DefaultGraph
    } else {
        GraphName::NamedNode(iri(g))
    }
}

/// Insert and delete quads in one commit.
fn change(s: &Store, add: &[(Triple, &str)], del: &[(Triple, String)]) {
    let mut txn = s.write();
    let mut labels = Default::default();
    let q = |t: &Triple, g: &str| {
        Quad::new(
            t.subject.clone(),
            t.predicate.clone(),
            t.object.clone(),
            graph_name(g),
        )
    };
    for (t, g) in del {
        let q = txn.encode_quad(&q(t, g), &mut labels).unwrap();
        txn.delete(q).unwrap();
    }
    for (t, g) in add {
        let q = txn.encode_quad(&q(t, g), &mut labels).unwrap();
        txn.insert(q).unwrap();
    }
    txn.commit().unwrap();
}

/// Every quad outside the inferred graph, with its graph's name.
fn quads(s: &Store) -> Vec<(Triple, String)> {
    let snap = s.snapshot();
    let mut out = Vec::new();
    for k in snap.scan_keys(Perm::Gspo, &[]).unwrap() {
        let q = snap.quad_to_terms(&Perm::Gspo.to_quad(&k)).unwrap();
        let g = match &q.graph_name {
            GraphName::NamedNode(n) if n.as_str() == INFERRED_GRAPH => continue,
            GraphName::NamedNode(n) => n.as_str().to_string(),
            _ => "default".to_string(),
        };
        out.push((Triple::new(q.subject, q.predicate, q.object), g));
    }
    out.sort_by_key(|(t, g)| format!("{t} {g}"));
    out
}

fn inferred(s: &Store) -> BTreeSet<String> {
    let snap = s.snapshot();
    let Some(g) = snap.lookup_iri(INFERRED_GRAPH) else {
        return BTreeSet::new();
    };
    snap.scan_keys(Perm::Gspo, &[g.0])
        .unwrap()
        .iter()
        .map(|k| {
            let q = snap.quad_to_terms(&Perm::Gspo.to_quad(k)).unwrap();
            Triple::new(q.subject, q.predicate, q.object).to_string()
        })
        .collect()
}

fn opts(inputs: &Inputs) -> ReasonOptions {
    ReasonOptions {
        inputs: inputs.clone(),
        ..Default::default()
    }
}

/// What a full run would write now.
fn expected(s: &Store, p: &Profile, inputs: &Inputs) -> BTreeSet<String> {
    let (ts, _) = infer(s.snapshot(), p, &opts(inputs)).unwrap();
    ts.iter().map(|t| t.to_string()).collect()
}

fn run(
    s: &Store,
    p: &Profile,
    inputs: &Inputs,
    since: Option<u64>,
    cache: Option<&Cache>,
) -> ReasonReport {
    materialize_incremental(
        s,
        p,
        &Extras::default(),
        Incremental { since, cache },
        &opts(inputs),
    )
    .unwrap_or_else(|e| panic!("{e:#}"))
}

fn check(s: &Store, p: &Profile, inputs: &Inputs, r: &ReasonReport, what: &str) {
    let (got, want) = (inferred(s), expected(s, p, inputs));
    if got != want {
        panic!(
            "{what} ({:?}, {:?}): extra {:#?} missing {:#?}",
            r.method,
            r.fallback,
            got.difference(&want).collect::<Vec<_>>(),
            want.difference(&got).collect::<Vec<_>>()
        );
    }
    assert_eq!(r.inferred as usize, got.len(), "{what}: inferred count");
}

const ONTO: &str = "http://ex.org/onto";
const DATA2: &str = "http://ex.org/data2";
const OTHER: &str = "http://ex.org/other";
const GRAPHS: [&str; 4] = ["default", ONTO, DATA2, OTHER];

fn graph_inputs() -> Inputs {
    Inputs {
        data_graphs: vec![GraphRef::Default, GraphRef::Named(DATA2.into())],
        ontology_graphs: vec![GraphRef::Named(ONTO.into())],
        ..Default::default()
    }
}

/// Random changes to the input graphs and to a graph that is not one, often a triple
/// that another graph holds too, each followed by an incremental run checked against a
/// full one. Returns the (incremental, full) runs.
fn random_changes(
    s: &Store,
    p: &Profile,
    owl: bool,
    seed: u64,
    steps: usize,
    cache: Option<&Cache>,
) -> (usize, usize) {
    let inputs = graph_inputs();
    let mut r = Rng(seed);
    let first: Vec<(Triple, &str)> = (0..40)
        .map(|_| (random_triple(&mut r, owl), GRAPHS[r.below(4)]))
        .collect();
    change(s, &first, &[]);
    let rep = run(s, p, &inputs, None, cache);
    check(s, p, &inputs, &rep, "first run");
    assert_eq!(rep.inputs.as_ref().unwrap().graphs.len(), 3);
    let mut since = rep.receipt.unwrap().commit.seq;
    let (mut inc, mut full) = (0, 0);
    for step in 0..steps {
        let now = quads(s);
        let del: Vec<(Triple, String)> = (0..r.below(4))
            .filter(|_| !now.is_empty())
            .map(|_| now[r.below(now.len())].clone())
            .collect();
        let add: Vec<(Triple, &str)> = (0..r.below(5))
            .map(|_| {
                // a copy of a triple of another graph, or a new one
                let t = if !now.is_empty() && r.below(3) == 0 {
                    now[r.below(now.len())].0.clone()
                } else {
                    random_triple(&mut r, owl)
                };
                (t, GRAPHS[r.below(4)])
            })
            .collect();
        change(s, &add, &del);
        let rep = run(s, p, &inputs, Some(since), cache);
        match rep.method {
            Method::Incremental => inc += 1,
            Method::Full => full += 1,
        }
        check(s, p, &inputs, &rep, &format!("seed {seed} step {step}"));
        since = rep.receipt.unwrap().commit.seq;
    }
    (inc, full)
}

fn persistent() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open(&dir.path().join("db"), StoreOptions::default()).unwrap();
    (dir, s)
}

#[test]
fn graph_sets_kept_in_memory() {
    for (p, owl) in [(Profile::Rdfs, false), (Profile::OwlRl, true)] {
        for seed in 1..6 {
            let s = Store::in_memory(StoreOptions::default());
            let cache = Cache::default();
            let (inc, full) = random_changes(&s, &p, owl, seed, 12, Some(&cache));
            assert!(inc > full, "{p}: {inc} incremental, {full} full");
        }
    }
}

#[test]
fn graph_sets_read_from_the_dataset() {
    for (p, owl) in [(Profile::Rdfs, false), (Profile::OwlRl, true)] {
        for seed in 1..4 {
            let (_d, s) = persistent();
            let (inc, full) = random_changes(&s, &p, owl, seed, 10, None);
            assert!(inc > full, "{p}: {inc} incremental, {full} full");
        }
    }
}

#[test]
fn graph_sets_kept_for_a_persistent_dataset() {
    for (p, owl) in [(Profile::Rdfs, false), (Profile::OwlRl, true)] {
        let (_d, s) = persistent();
        let cache = Cache::default();
        let (inc, full) = random_changes(&s, &p, owl, 7, 15, Some(&cache));
        assert!(inc > full, "{p}: {inc} incremental, {full} full");
    }
}

fn ttl(s: &Store, graph: Option<&str>, text: &str) {
    let text =
        format!("@prefix ex: <{EX}> . @prefix rdfs: <{RDFS}> . @prefix owl: <{OWL}> . {text}");
    s.load(&[Source::from_bytes(
        text.into_bytes(),
        RdfFormat::Turtle,
        graph.map(iri),
    )])
    .unwrap();
}

fn update(s: &Store, u: &str) {
    let u = format!("PREFIX ex: <{EX}> PREFIX rdfs: <{RDFS}> PREFIX owl: <{OWL}> {u}");
    sparkles_core::sparql::update::update(s, &u, &QueryOptions::default()).unwrap();
}

fn has(s: &Store, t: &str) -> bool {
    inferred(s).iter().any(|x| x == t)
}

const TOM_ANIMAL: &str =
    "<http://ex.org/tom> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://ex.org/Animal>";
const TOM_THING: &str =
    "<http://ex.org/tom> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://ex.org/Thing>";

/// The ontology in a named graph: its entailments go to the inferred graph, its own
/// triples stay in it, and changes to it are maintained incrementally.
#[test]
fn ontology_graph() {
    let s = Store::in_memory(StoreOptions::default());
    let cache = Cache::default();
    ttl(&s, Some(ONTO), "ex:Cat rdfs:subClassOf ex:Animal .");
    ttl(&s, None, "ex:tom a ex:Cat .");
    let inputs = Inputs {
        ontology_graphs: vec![GraphRef::Named(ONTO.into())],
        ..Default::default()
    };
    let rep = run(&s, &Profile::Rdfs, &inputs, None, Some(&cache));
    assert!(has(&s, TOM_ANIMAL));
    assert!(
        !inferred(&s)
            .iter()
            .any(|t| t.contains("subClassOf> <http://ex.org/Animal")
                && t.starts_with("<http://ex.org/Cat>"))
    );
    let since = rep.receipt.unwrap().commit.seq;
    update(
        &s,
        "INSERT DATA { GRAPH ex:onto { ex:Animal rdfs:subClassOf ex:Thing } }",
    );
    let rep = run(&s, &Profile::Rdfs, &inputs, Some(since), Some(&cache));
    assert_eq!(rep.method, Method::Incremental, "{:?}", rep.fallback);
    assert!(has(&s, TOM_THING));
    check(
        &s,
        &Profile::Rdfs,
        &inputs,
        &rep,
        "after the ontology changed",
    );
    // without the ontology graph the run reads another set: a full run
    let since = rep.receipt.unwrap().commit.seq;
    let rep = run(
        &s,
        &Profile::Rdfs,
        &Inputs::default(),
        Some(since),
        Some(&cache),
    );
    assert_eq!(rep.method, Method::Full);
    assert_eq!(
        rep.fallback.as_deref(),
        Some("the input graphs changed since the previous run")
    );
    assert!(!has(&s, TOM_ANIMAL));
}

#[test]
fn the_inferred_graph_is_never_an_input() {
    let s = Store::in_memory(StoreOptions::default());
    let inputs = Inputs {
        data_graphs: vec![GraphRef::Named(INFERRED_GRAPH.into())],
        ..Default::default()
    };
    let e = materialize_incremental(
        &s,
        &Profile::Rdfs,
        &Extras::default(),
        Incremental::default(),
        &opts(&inputs),
    )
    .unwrap_err();
    assert!(e.to_string().contains("cannot be an input"), "{e}");
}

/// `owl:imports` to graphs of the dataset, directly and through a location mapping;
/// a change of the import set runs in full, one inside an imported graph incrementally.
#[test]
fn imports_in_the_dataset() {
    let s = Store::in_memory(StoreOptions::default());
    let cache = Cache::default();
    ttl(
        &s,
        None,
        "<urn:o> owl:imports <http://ex.org/onto> . ex:tom a ex:Cat .",
    );
    ttl(&s, Some(ONTO), "ex:Cat rdfs:subClassOf ex:Animal .");
    let inputs = Inputs::default();
    let rep = run(&s, &Profile::Rdfs, &inputs, None, Some(&cache));
    let resolved = rep.inputs.clone().unwrap();
    assert_eq!(
        resolved.graphs,
        [GraphRef::Default, GraphRef::Named(ONTO.into())]
    );
    assert_eq!(resolved.imports[0].graph.as_deref(), Some(ONTO));
    assert!(has(&s, TOM_ANIMAL));
    // a change inside the imported graph
    let since = rep.receipt.unwrap().commit.seq;
    update(
        &s,
        "INSERT DATA { GRAPH ex:onto { ex:Animal rdfs:subClassOf ex:Thing } }",
    );
    let rep = run(&s, &Profile::Rdfs, &inputs, Some(since), Some(&cache));
    assert_eq!(rep.method, Method::Incremental, "{:?}", rep.fallback);
    assert!(has(&s, TOM_THING));
    // an import that does not resolve is a warning, and a new import set a full run
    let since = rep.receipt.unwrap().commit.seq;
    update(
        &s,
        "INSERT DATA { <urn:o> owl:imports <http://ex.org/missing> }",
    );
    let rep = run(&s, &Profile::Rdfs, &inputs, Some(since), Some(&cache));
    assert_eq!(rep.method, Method::Incremental, "{:?}", rep.fallback);
    assert!(
        rep.warnings.iter().any(|w| w.contains("did not resolve")),
        "{:?}",
        rep.warnings
    );
    let watched = rep.inputs.as_ref().unwrap().watched();
    assert!(watched.contains(&GraphRef::Named(format!("{EX}missing"))));
    let since = rep.receipt.unwrap().commit.seq;
    ttl(
        &s,
        Some("http://ex.org/missing"),
        "ex:Thing rdfs:subClassOf ex:Being .",
    );
    let rep = run(&s, &Profile::Rdfs, &inputs, Some(since), Some(&cache));
    assert_eq!(rep.method, Method::Full);
    assert_eq!(
        rep.fallback.as_deref(),
        Some("the input graphs changed since the previous run")
    );
    check(
        &s,
        &Profile::Rdfs,
        &inputs,
        &rep,
        "after the import resolved",
    );
    // imports ignored
    let none = Inputs {
        imports: ImportMode::None,
        ..Default::default()
    };
    let rep = run(&s, &Profile::Rdfs, &none, None, None);
    assert!(!has(&s, TOM_ANIMAL));
    assert!(rep.inputs.unwrap().imports.is_empty());
    // a location mapping to another graph name
    update(
        &s,
        "DELETE DATA { <urn:o> owl:imports <http://ex.org/onto> }",
    );
    update(
        &s,
        "INSERT DATA { <urn:o> owl:imports <http://purl.example/onto/v1> }",
    );
    let mapped = Inputs {
        location_mapping: LocationMapping {
            prefixes: [("http://purl.example/onto/v1".into(), ONTO.into())].into(),
            ..Default::default()
        },
        ..Default::default()
    };
    let rep = run(&s, &Profile::Rdfs, &mapped, None, None);
    let r = rep.inputs.unwrap();
    let i = r.imports.iter().find(|i| i.iri.ends_with("/v1")).unwrap();
    assert_eq!(i.location.as_deref(), Some(ONTO));
    assert_eq!(i.graph.as_deref(), Some(ONTO));
    assert!(has(&s, TOM_ANIMAL));
}

#[test]
fn location_mappings() {
    let m: LocationMapping = serde_json::from_str(
        r#"[{"name": "http://a.example/x", "altName": "file:///x.ttl"},
            {"prefix": "http://a.example/", "altPrefix": "file:///a/"},
            {"prefix": "http://a.example/deep/", "altPrefix": "file:///deep/"}]"#,
    )
    .unwrap();
    assert_eq!(m.map("http://a.example/x"), "file:///x.ttl");
    assert_eq!(m.map("http://a.example/y"), "file:///a/y");
    assert_eq!(m.map("http://a.example/deep/z"), "file:///deep/z");
    assert_eq!(m.map("http://b.example/z"), "http://b.example/z");
    let jena = r#"@prefix lm: <http://jena.hpl.hp.com/2004/08/location-mapping#> .
        [] lm:mapping [ lm:name "http://a.example/x" ; lm:altName "file:///x.ttl" ] ,
                      [ lm:prefix "http://a.example/" ; lm:altPrefix "file:///a/" ] ,
                      [ lm:prefix "http://a.example/deep/" ; lm:altPrefix "file:///deep/" ] ."#;
    assert_eq!(
        LocationMapping::from_jena(jena.as_bytes(), RdfFormat::Turtle).unwrap(),
        m
    );
    let back: LocationMapping = serde_json::from_value(serde_json::to_value(&m).unwrap()).unwrap();
    assert_eq!(back, m);
}

/// Imports fetched from files: loaded into the graph of the import IRI once, found there
/// by later runs, and replaced on request.
#[test]
fn imports_fetched_from_files() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("onto.ttl");
    std::fs::write(
        &file,
        format!(
            "<{EX}Cat> <{RDFS}subClassOf> <{EX}Animal> . <urn:onto> <{OWL}imports> <{EX}more> ."
        ),
    )
    .unwrap();
    std::fs::write(
        dir.path().join("more.ttl"),
        format!("<{EX}Animal> <{RDFS}subClassOf> <{EX}Thing> ."),
    )
    .unwrap();
    let s = Store::in_memory(StoreOptions::default());
    ttl(
        &s,
        None,
        "<urn:o> owl:imports <http://ex.org/onto> . ex:tom a ex:Cat .",
    );
    let base = format!("file://{}/", dir.path().display());
    let inputs = Inputs {
        imports: ImportMode::Fetch,
        location_mapping: LocationMapping {
            names: [
                (format!("{EX}onto"), format!("{base}onto.ttl")),
                (format!("{EX}more"), format!("{base}more.ttl")),
            ]
            .into(),
            ..Default::default()
        },
        ..Default::default()
    };
    let q = QueryOptions::default();
    let f = fetch_imports(&s, &inputs, &[], &q).unwrap();
    assert_eq!(f.fetched, [format!("{EX}onto"), format!("{EX}more")]);
    let rep = run(&s, &Profile::Rdfs, &inputs, None, None);
    assert!(has(&s, TOM_ANIMAL) && has(&s, TOM_THING));
    assert_eq!(rep.inputs.unwrap().graphs.len(), 3);
    // a second fetch finds the copies
    let f = fetch_imports(&s, &inputs, &[], &q).unwrap();
    assert!(f.fetched.is_empty(), "{f:?}");
    // a refresh replaces a copy
    std::fs::write(&file, format!("<{EX}Cat> <{RDFS}subClassOf> <{EX}Pet> .")).unwrap();
    let f = fetch_imports(&s, &inputs, &[format!("{EX}onto")], &q).unwrap();
    assert_eq!(f.fetched, [format!("{EX}onto")]);
    run(&s, &Profile::Rdfs, &inputs, None, None);
    let pet =
        "<http://ex.org/tom> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://ex.org/Pet>";
    assert!(has(&s, pet) && !has(&s, TOM_ANIMAL));
    // a location that cannot be read is a warning
    update(
        &s,
        "INSERT DATA { <urn:o> owl:imports <http://ex.org/gone> }",
    );
    let mut gone = inputs.clone();
    gone.location_mapping
        .names
        .insert(format!("{EX}gone"), format!("{base}gone.ttl"));
    let f = fetch_imports(&s, &gone, &[], &q).unwrap();
    assert!(f.fetched.is_empty());
    assert!(f.warnings[0].contains("could not be fetched"), "{f:?}");
    // file loads the options refuse fail the fetch
    let refused = QueryOptions {
        file_loads: sparkles_core::sparql::FileLoads::Disabled,
        ..Default::default()
    };
    update(
        &s,
        "INSERT DATA { <urn:o> owl:imports <http://ex.org/third> }",
    );
    let mut third = inputs.clone();
    third
        .location_mapping
        .names
        .insert(format!("{EX}third"), format!("{base}more.ttl"));
    assert!(fetch_imports(&s, &third, &[], &refused).is_err());
}

/// An import fetched over HTTP, under the outbound policy: refused at a loopback address
/// by default, loaded once allowed.
#[test]
fn imports_fetched_over_http() {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let body = format!("<{EX}Cat> <{RDFS}subClassOf> <{EX}Animal> .\n");
    let served = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let served2 = served.clone();
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            let mut c = conn.unwrap();
            let mut buf = [0u8; 4096];
            let _ = c.read(&mut buf);
            served2.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let _ = write!(
                c,
                "HTTP/1.1 200 OK\r\nContent-Type: application/n-triples\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    let s = Store::in_memory(StoreOptions::default());
    ttl(
        &s,
        None,
        "<urn:o> owl:imports <http://ex.org/onto> . ex:tom a ex:Cat .",
    );
    let inputs = Inputs {
        imports: ImportMode::Fetch,
        location_mapping: LocationMapping {
            prefixes: [(EX.into(), format!("http://127.0.0.1:{port}/"))].into(),
            ..Default::default()
        },
        ..Default::default()
    };
    // the default policy refuses loopback
    let e = fetch_imports(&s, &inputs, &[], &QueryOptions::default()).unwrap_err();
    assert!(format!("{e:#}").contains("owl:imports"), "{e:#}");
    assert_eq!(served.load(std::sync::atomic::Ordering::SeqCst), 0);
    let allowed = QueryOptions {
        outbound: sparkles_core::outbound::OutboundPolicy {
            allow_private: true,
            ..Default::default()
        },
        ..Default::default()
    };
    let f = fetch_imports(&s, &inputs, &[], &allowed).unwrap();
    assert_eq!(f.fetched, [format!("{EX}onto")]);
    let rep = run(&s, &Profile::Rdfs, &inputs, None, None);
    assert!(has(&s, TOM_ANIMAL));
    let i = &rep.inputs.unwrap().imports[0];
    assert_eq!(i.graph.as_deref(), Some(ONTO));
    // the copy is in the dataset: no second request
    fetch_imports(&s, &inputs, &[], &allowed).unwrap();
    assert_eq!(served.load(std::sync::atomic::Ordering::SeqCst), 1);
}

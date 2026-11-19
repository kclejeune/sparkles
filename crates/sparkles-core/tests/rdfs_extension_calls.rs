//! RDFS on read and query-time reasoning never introduce a call of a registered property
//! function that the query does not write (roadmap item 1198). A registered property is
//! a call, not stored data, so a schema that declares it a subproperty of a queried
//! predicate, gives it a domain or range, or puts a subproperty below it leaves every
//! query as it is without the schema. Before the fix the added call panicked in the
//! callback guard, and the stream's destructor panicked again and aborted the process.
use oxrdf::{NamedNode, Term, Triple};
use sparkles_core::sparql::extensions::{
    ArgumentShape, ExtensionRegistry, PropertyContext, PropertyDescriptor, PropertyFunction,
    PropertyInput, PropertyPosition, PropertyRow, PropertyStream, ScalarError,
};
use sparkles_core::sparql::rdfs::{RdfsOnRead, RdfsSchema};
use sparkles_core::sparql::reasoning::{
    Demand, DemandInfo, DemandTerm, Derived, ReasoningProvider, ReasoningSession, Report,
    SessionRequest,
};
use sparkles_core::sparql::{
    CursorOptions, FallbackPolicy, QueryOptions, query, select_cursor, update::update,
};
use sparkles_core::store::{Store, StoreOptions};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

const CALLBACK: &str = "urn:callback";
const RDFS: &str = "http://www.w3.org/2000/01/rdf-schema#";

struct Once(Option<PropertyRow>);
impl PropertyStream for Once {
    fn next(&mut self, _: &PropertyContext<'_>) -> Result<Option<PropertyRow>, ScalarError> {
        Ok(self.0.take())
    }
}
/// Answers its subject as its object, counting every call.
struct Echo(Arc<AtomicUsize>);
impl PropertyFunction for Echo {
    fn open<'q>(
        &self,
        _: PropertyContext<'q>,
        input: PropertyInput,
    ) -> Result<Box<dyn PropertyStream + 'q>, ScalarError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(Once(Some(PropertyRow {
            subject: vec![None; input.subject.len()],
            object: vec![input.subject[0].clone()],
        }))))
    }
}

fn store() -> Store {
    let s = Store::in_memory(StoreOptions::default());
    update(
        &s,
        "INSERT DATA { <urn:s:1> <urn:p> 1 . <urn:s:2> <urn:p> 2 . <urn:s:3> <urn:p> 3 .
                       <urn:s:1> <urn:child> 5 }",
        &Default::default(),
    )
    .unwrap();
    s
}

fn registry(opens: &Arc<AtomicUsize>) -> Arc<ExtensionRegistry> {
    registry_of(CALLBACK, ArgumentShape::Term, opens)
}

fn registry_of(
    iri: &str,
    subject: ArgumentShape,
    opens: &Arc<AtomicUsize>,
) -> Arc<ExtensionRegistry> {
    let mut r = ExtensionRegistry::builder();
    let mut d = PropertyDescriptor::new(
        iri,
        subject,
        ArgumentShape::Term,
        vec![PropertyPosition::Subject(0)],
        vec![PropertyPosition::Object(0)],
        Echo(opens.clone()),
    )
    .unwrap();
    // a cursor streams the call, so the streamed runs below need no materialization
    d.prefix_output = true;
    r.register_property(d).unwrap();
    r.build()
}

fn schema() -> RdfsSchema {
    let n = |s: &str| NamedNode::new(s).unwrap();
    let t = |s: &str, p: &str, o: &str| Triple::new(n(s), n(&format!("{RDFS}{p}")), n(o));
    RdfsSchema::from_triples(&[
        t(CALLBACK, "subPropertyOf", "urn:parent"),
        t("urn:child", "subPropertyOf", CALLBACK),
        t(CALLBACK, "domain", "urn:C"),
        t(CALLBACK, "range", "urn:R"),
    ])
}

fn options(opens: &Arc<AtomicUsize>, rdfs: bool) -> QueryOptions {
    QueryOptions {
        extensions: Some(registry(opens)),
        rdfs: rdfs.then(|| Arc::new(RdfsOnRead::fixed(schema()))),
        no_cache: true,
        ..Default::default()
    }
}

type Rows = Vec<Vec<Option<Term>>>;

fn eager(s: &Store, q: &str, o: &QueryOptions) -> sparkles_core::Result<Rows> {
    let mut rows: Rows = query(s.snapshot(), q, o)?
        .rows()?
        .iter()
        .map(|r| r.values().to_vec())
        .collect();
    rows.sort_by_key(|r| format!("{r:?}"));
    Ok(rows)
}

fn streamed(s: &Store, q: &str, o: &QueryOptions) -> sparkles_core::Result<Rows> {
    let cursor = CursorOptions {
        batch_rows: 2,
        fallback: FallbackPolicy::RejectMaterialization,
        ..Default::default()
    };
    let mut c = select_cursor(s.snapshot(), q, o, &cursor)?;
    let mut rows = Vec::new();
    while let Some(b) = c.next_batch()? {
        for i in 0..b.len() {
            rows.push(b.row(i)?.values().to_vec());
        }
    }
    rows.sort_by_key(|r| format!("{r:?}"));
    Ok(rows)
}

/// The shapes that read the parent predicate, the domain class or the range class,
/// none of which writes the registered property.
const UNNAMED: &[&str] = &[
    "SELECT * { ?s <urn:p> ?v . ?s <urn:parent> ?x }",
    "SELECT * { ?s <urn:p> ?v FILTER EXISTS { ?s <urn:parent> ?x } }",
    "SELECT * { ?s <urn:p> ?v BIND(EXISTS { ?s <urn:parent> ?x } AS ?b) }",
    "SELECT * { ?s <urn:p> ?v OPTIONAL { ?s <urn:p> ?w FILTER(?w > 0) }
                BIND(EXISTS { ?s <urn:parent> ?x } AS ?b)
                BIND(EXISTS { ?s <urn:parent> ?y } AS ?c) }",
    "SELECT * { ?s <urn:p> ?v . ?s a <urn:C> }",
    "SELECT * { ?s <urn:p> ?v . ?s a ?class }",
    "SELECT * { ?s <urn:p> ?v . ?s ?q ?x }",
    "SELECT * { ?s <urn:p> ?v . ?s <urn:parent>+ ?x }",
    "SELECT * { ?s <urn:p> ?v . ?s <urn:parent>{1,2} ?x }",
    "SELECT * { ?s <urn:p> ?v . ?s (<urn:callback>|<urn:parent>) ?x }",
    "SELECT * { ?s <urn:p> ?v . ?s (<urn:parent>|<urn:q>)/<urn:p> ?x }",
    "SELECT * { ?s a <urn:R> }",
];

/// Each shape calls nothing, fails with no error, and answers as RDFS on read answers
/// when the property is not registered and has no stored triples. The subproperty
/// below the registered property still entails the parent through it, since the
/// closure of the schema is unchanged. A variable predicate can tell the two
/// apart, binding the registered property to an entailed triple, and so can a path that
/// names it, which no longer matches what the schema derives through it.
fn same_as_unregistered(read: fn(&Store, &str, &QueryOptions) -> sparkles_core::Result<Rows>) {
    let s = store();
    for q in UNNAMED {
        let opens = Arc::new(AtomicUsize::new(0));
        let with = read(&s, q, &options(&opens, true));
        assert_eq!(opens.load(Ordering::SeqCst), 0, "a call was added to {q}");
        let with = with.unwrap_or_else(|e| panic!("{q} with the schema: {e}"));
        let unregistered = QueryOptions {
            extensions: None,
            ..options(&opens, true)
        };
        let expected = read(&s, q, &unregistered).unwrap();
        if q.contains("?q") || q.contains("<urn:callback>") {
            // these name the registered property itself, which then binds or matches
            // nothing that the schema derives through it
            assert!(with.len() < expected.len(), "{q}");
        } else {
            assert_eq!(with, expected, "{q}");
        }
    }
}

#[test]
fn eager_rdfs_adds_no_registered_call() {
    same_as_unregistered(eager);
}

#[test]
fn streamed_rdfs_adds_no_registered_call() {
    same_as_unregistered(streamed);
}

/// A query that writes the registered property keeps its call as written: a subproperty
/// declared below it does not turn the call into a union with stored triples.
#[test]
fn a_written_call_is_left_as_written() {
    let s = store();
    let q = "SELECT * { ?s <urn:p> ?v . ?s <urn:callback> ?x }";
    let opens = Arc::new(AtomicUsize::new(0));
    let plain = eager(&s, q, &options(&opens, false)).unwrap();
    assert_eq!(opens.swap(0, Ordering::SeqCst), 3);
    let with = eager(&s, q, &options(&opens, true)).unwrap();
    assert_eq!(opens.swap(0, Ordering::SeqCst), 3);
    assert_eq!(with, plain);
    assert_eq!(with.len(), 3);
    let streamed_rows = streamed(&s, q, &options(&opens, true)).unwrap();
    assert_eq!(opens.load(Ordering::SeqCst), 3);
    assert_eq!(streamed_rows, plain);
}

/// The WHERE clause of an update is rewritten by the same rule.
#[test]
fn update_where_rdfs_adds_no_registered_call() {
    for (u, inserted) in [
        (
            "INSERT { ?s <urn:out> ?x } WHERE { ?s <urn:p> ?v FILTER EXISTS { ?s <urn:parent> ?x } }",
            0,
        ),
        (
            "INSERT { ?s <urn:out> ?x } WHERE { ?s <urn:p> ?v . ?s <urn:callback> ?x }",
            3,
        ),
    ] {
        let s = store();
        let opens = Arc::new(AtomicUsize::new(0));
        update(&s, u, &options(&opens, true)).unwrap_or_else(|e| panic!("{u}: {e}"));
        assert_eq!(opens.load(Ordering::SeqCst), inserted, "{u}");
        let count = query(
            s.snapshot(),
            "SELECT * { ?s <urn:out> ?x }",
            &Default::default(),
        )
        .unwrap()
        .rows()
        .unwrap()
        .len();
        assert_eq!(count, inserted, "{u}");
    }
}

/// A provider that derives `<urn:derived>` and nothing else, and records the seeds of
/// every demand.
struct Deriving {
    seeds: Arc<AtomicUsize>,
    plain: Arc<AtomicUsize>,
}
impl ReasoningProvider for Deriving {
    fn label(&self) -> String {
        "derived-only".into()
    }
    fn start(&self, _: SessionRequest) -> sparkles_core::Result<Box<dyn ReasoningSession>> {
        Ok(Box::new(DerivingSession {
            seeds: self.seeds.clone(),
            plain: self.plain.clone(),
            unseeded: Vec::new(),
        }))
    }
}
struct DerivingSession {
    seeds: Arc<AtomicUsize>,
    plain: Arc<AtomicUsize>,
    /// the demands with no seeded position, from `prepare`
    unseeded: Vec<usize>,
}
impl ReasoningSession for DerivingSession {
    fn intensional(
        &self,
        _: Option<&Term>,
        p: Option<&Term>,
        _: Option<&Term>,
    ) -> sparkles_core::Result<bool> {
        Ok(matches!(p, Some(Term::NamedNode(n)) if n.as_str() == "urn:derived"))
    }
    fn prepare(&mut self, d: &[Demand], _: bool) -> sparkles_core::Result<Vec<DemandInfo>> {
        self.unseeded = (0..d.len())
            .filter(|&i| ![&d[i].s, &d[i].o].contains(&&DemandTerm::Seeded))
            .collect();
        Ok(vec![DemandInfo::default(); d.len()])
    }
    fn derive(&mut self, seeds: Vec<(usize, Vec<u64>)>) -> sparkles_core::Result<Derived> {
        self.seeds
            .fetch_add(seeds.iter().map(|(_, s)| s.len()).sum(), Ordering::SeqCst);
        // a demand with constants alone, not a seeded one given no seeds
        self.plain.fetch_add(
            seeds
                .iter()
                .filter(|(d, s)| s.is_empty() && self.unseeded.contains(d))
                .count(),
            Ordering::SeqCst,
        );
        Ok(Derived::default())
    }
    fn report(&self) -> Report {
        Report::default()
    }
}

/// The seeds of a derived pattern are read by a query of their own. A registered call
/// among the patterns that bind the seed variable is not run there: the pattern falls
/// back to its demand with constants, and the callback runs only in the query itself.
#[test]
fn reasoning_seeds_run_no_registered_call() {
    let s = store();
    let q = "SELECT * { <urn:s:1> <urn:callback> ?x . ?x <urn:derived> ?y }";
    let opens = Arc::new(AtomicUsize::new(0));
    let plain = eager(&s, q, &options(&opens, false)).unwrap();
    let without = opens.swap(0, Ordering::SeqCst);
    let seeds = Arc::new(AtomicUsize::new(0));
    let plain_demands = Arc::new(AtomicUsize::new(0));
    let o = QueryOptions {
        reasoning: Some(Arc::new(Deriving {
            seeds: seeds.clone(),
            plain: plain_demands.clone(),
        })),
        ..options(&opens, false)
    };
    let with = eager(&s, q, &o).unwrap();
    assert_eq!(opens.swap(0, Ordering::SeqCst), without);
    assert_eq!(with, plain);
    // no seed is read through the call, and the derived pattern is derived with its
    // constants alone instead
    assert_eq!(seeds.load(Ordering::SeqCst), 0);
    assert_eq!(plain_demands.swap(0, Ordering::SeqCst), 1);
    let streamed_rows = streamed(&s, q, &o).unwrap();
    assert_eq!(opens.load(Ordering::SeqCst), without);
    assert_eq!(streamed_rows, plain);
}

/// A written call keeps its inputs when a pattern that binds them is rewritten, whether
/// the rewritten pattern's only subproperty is the registered property or a stored one.
#[test]
fn a_written_call_reads_inputs_from_rewritten_patterns() {
    let s = store();
    update(
        &s,
        "INSERT DATA { <urn:s:1> <urn:parent> 1 . <urn:s:2> <urn:p2> 7 }",
        &Default::default(),
    )
    .unwrap();
    let n = |x: &str| NamedNode::new(x).unwrap();
    let sub = n(&format!("{RDFS}subPropertyOf"));
    for (schema, q, calls) in [
        (
            vec![Triple::new(n(CALLBACK), sub.clone(), n("urn:parent"))],
            "SELECT * { ?s <urn:parent> ?v . ?s <urn:callback> ?x }",
            1,
        ),
        (
            vec![Triple::new(n("urn:p2"), sub.clone(), n("urn:p"))],
            "SELECT * { ?s <urn:p> ?v . ?s <urn:callback> ?x }",
            4,
        ),
    ] {
        let opens = Arc::new(AtomicUsize::new(0));
        let o = QueryOptions {
            rdfs: Some(Arc::new(RdfsOnRead::fixed(RdfsSchema::from_triples(
                &schema,
            )))),
            ..options(&opens, false)
        };
        let rows = eager(&s, q, &o).unwrap_or_else(|e| panic!("{q}: {e}"));
        assert_eq!(rows.len(), calls, "{q}");
        assert_eq!(opens.swap(0, Ordering::SeqCst), calls, "{q}");
        assert_eq!(streamed(&s, q, &o).unwrap(), rows, "{q}");
        assert_eq!(opens.load(Ordering::SeqCst), calls, "{q}");
    }
}

/// A call with a list argument keeps the list written in its pattern under RDFS on read.
#[test]
fn a_written_list_argument_survives_the_rewrite() {
    let s = store();
    let opens = Arc::new(AtomicUsize::new(0));
    let o = QueryOptions {
        extensions: Some(registry_of(CALLBACK, ArgumentShape::List(1..=4), &opens)),
        ..options(&opens, true)
    };
    let q = "SELECT * { ?s <urn:p> ?v . (?s ?v) <urn:callback> ?x }";
    let without = QueryOptions {
        rdfs: None,
        ..o.clone()
    };
    let plain = eager(&s, q, &without).unwrap();
    assert_eq!(opens.swap(0, Ordering::SeqCst), 3);
    let with = eager(&s, q, &o).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(opens.swap(0, Ordering::SeqCst), 3);
    assert_eq!(with, plain);
}

/// A registered `rdf:type` gets no type branches from a variable predicate.
#[test]
fn a_registered_type_gets_no_type_branch() {
    let s = store();
    let opens = Arc::new(AtomicUsize::new(0));
    let n = |x: &str| NamedNode::new(x).unwrap();
    let schema = RdfsSchema::from_triples(&[Triple::new(
        n("urn:C"),
        n(&format!("{RDFS}subClassOf")),
        n("urn:D"),
    )]);
    let o = QueryOptions {
        extensions: Some(registry_of(
            "http://www.w3.org/1999/02/22-rdf-syntax-ns#type",
            ArgumentShape::Term,
            &opens,
        )),
        rdfs: Some(Arc::new(RdfsOnRead::fixed(schema))),
        no_cache: true,
        ..Default::default()
    };
    for q in [
        "SELECT * { <urn:s:1> ?p ?o }",
        "SELECT * { ?s ?p <urn:D> }",
        "SELECT * { ?s ?p ?o }",
    ] {
        eager(&s, q, &o).unwrap_or_else(|e| panic!("{q}: {e}"));
        assert_eq!(opens.load(Ordering::SeqCst), 0, "{q}");
    }
}

/// Blank nodes keep one binding across the algebra nodes of a group, a list head shared
/// with an ordinary pattern is still the call's list, and a cell-like pattern of a
/// term-shaped argument is rewritten as any other pattern is.
#[test]
fn blank_nodes_and_list_cells_under_the_rewrite() {
    type Make<'a> = dyn Fn(&Arc<AtomicUsize>) -> QueryOptions + 'a;
    let s = store();
    update(
        &s,
        "INSERT DATA { <urn:s:1> <urn:tag> 9 . <urn:s:1> <urn:firstChild> <urn:a> }",
        &Default::default(),
    )
    .unwrap();
    let n = |x: &str| NamedNode::new(x).unwrap();
    let schema = RdfsSchema::from_triples(&[
        Triple::new(
            n("urn:firstChild"),
            n(&format!("{RDFS}subPropertyOf")),
            n("http://www.w3.org/1999/02/22-rdf-syntax-ns#first"),
        ),
        Triple::new(n("urn:q"), n(&format!("{RDFS}subPropertyOf")), n("urn:r")),
    ]);
    let rdfs = Some(Arc::new(RdfsOnRead::fixed(schema)));
    let term = |opens: &Arc<AtomicUsize>| QueryOptions {
        rdfs: rdfs.clone(),
        ..options(opens, false)
    };
    let list = |opens: &Arc<AtomicUsize>| QueryOptions {
        extensions: Some(registry_of(CALLBACK, ArgumentShape::List(1..=4), opens)),
        rdfs: rdfs.clone(),
        ..options(opens, false)
    };
    // each case answers as it does without RDFS, except the last, whose answer the
    // schema entails
    let cases: [(&str, &Make<'_>, Option<usize>); 3] = [
        (
            "SELECT ?x { <urn:s:1> <urn:p>+ _:b . _:b <urn:callback> ?x }",
            &term,
            None,
        ),
        (
            "SELECT * { ?s <urn:p> ?v . _:args rdf:first ?s ; rdf:rest rdf:nil ;
                        <urn:tag> ?tag ; <urn:callback> ?x }",
            &list,
            None,
        ),
        (
            "SELECT ?x { _:s rdf:first ?v . _:s <urn:callback> ?x }",
            &term,
            Some(1),
        ),
    ];
    for (q, make, entailed) in cases {
        let q = format!("PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> {q}");
        let opens = Arc::new(AtomicUsize::new(0));
        let without = QueryOptions {
            rdfs: None,
            ..make(&opens)
        };
        let plain = eager(&s, &q, &without).unwrap_or_else(|e| panic!("{q}: {e}"));
        let plain_calls = opens.swap(0, Ordering::SeqCst);
        let got = eager(&s, &q, &make(&opens)).unwrap_or_else(|e| panic!("{q}: {e}"));
        let calls = opens.swap(0, Ordering::SeqCst);
        match entailed {
            None => {
                assert!(!plain.is_empty(), "{q}");
                assert_eq!(got, plain, "{q}");
                assert_eq!(calls, plain_calls, "{q}");
            }
            Some(n) => {
                assert_eq!(got.len(), n, "{q}");
                assert_eq!(calls, n, "{q}");
            }
        }
        assert_eq!(streamed(&s, &q, &make(&opens)).unwrap(), got, "{q}");
    }
}

/// A query variable named like the rewrite's blank-node variables is not a list cell,
/// and a list keeps its cells when `rdf:rest` is registered as well.
#[test]
fn renamed_blank_nodes_cannot_meet_query_variables() {
    let s = store();
    update(
        &s,
        "INSERT DATA { <urn:l> <http://www.w3.org/1999/02/22-rdf-syntax-ns#first> <urn:b> .
                       <urn:s:1> a <urn:C> }",
        &Default::default(),
    )
    .unwrap();
    let n = |x: &str| NamedNode::new(x).unwrap();
    let schema = |t: Vec<Triple>| Some(Arc::new(RdfsOnRead::fixed(RdfsSchema::from_triples(&t))));
    let sub_class = vec![Triple::new(
        n("urn:C"),
        n(&format!("{RDFS}subClassOf")),
        n("urn:D"),
    )];
    let q = "PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#>
        SELECT ?x { _:args rdf:first <urn:a> ; rdf:rest rdf:nil ; <urn:callback> ?x .
                    ?__rdfsb_args rdf:first <urn:b> }";
    for rdfs in [None, schema(Vec::new()), schema(sub_class.clone())] {
        let opens = Arc::new(AtomicUsize::new(0));
        let o = QueryOptions {
            extensions: Some(registry_of(CALLBACK, ArgumentShape::List(1..=4), &opens)),
            rdfs,
            ..options(&opens, false)
        };
        let rows = eager(&s, q, &o).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(rows.len(), 1);
        assert_eq!(opens.load(Ordering::SeqCst), 1);
    }
    // `rdf:rest` registered as a pass-through call beside a list-shaped call
    let opens = Arc::new(AtomicUsize::new(0));
    let mut r = ExtensionRegistry::builder();
    for (iri, subject, required, produces) in [
        (
            CALLBACK,
            ArgumentShape::List(1..=4),
            vec![PropertyPosition::Subject(0)],
            vec![PropertyPosition::Object(0)],
        ),
        (
            "http://www.w3.org/1999/02/22-rdf-syntax-ns#rest",
            ArgumentShape::Term,
            vec![],
            vec![],
        ),
    ] {
        let mut d = PropertyDescriptor::new(
            iri,
            subject,
            ArgumentShape::Term,
            required,
            produces,
            Echo(opens.clone()),
        )
        .unwrap();
        d.prefix_output = true;
        r.register_property(d).unwrap();
    }
    let q = "SELECT ?x { <urn:s:1> a <urn:C> . (<urn:a> <urn:b>) <urn:callback> ?x }";
    let registry = r.build();
    let run = |rdfs| {
        let o = QueryOptions {
            extensions: Some(registry.clone()),
            rdfs,
            no_cache: true,
            ..Default::default()
        };
        eager(&s, q, &o).map(|rows| rows.len())
    };
    // the outcome without RDFS, an answer or a refusal, is the outcome with the schema
    let without = run(None).map_err(|e| e.to_string());
    assert_eq!(run(schema(sub_class)).map_err(|e| e.to_string()), without);
}

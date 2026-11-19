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
    Demand, DemandInfo, Derived, ReasoningProvider, ReasoningSession, Report, SessionRequest,
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
            subject: vec![None],
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
    let mut r = ExtensionRegistry::builder();
    let mut d = PropertyDescriptor::new(
        CALLBACK,
        ArgumentShape::Term,
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
    "SELECT * { ?s a <urn:R> }",
];

/// Each shape calls nothing, fails with no error, and answers as RDFS on read answers
/// when the property is not registered and has no stored triples. The subproperty
/// below the registered property still entails the parent through it, since the
/// closure of the schema is unchanged. Only a variable predicate can tell the two
/// apart, since it would bind the registered property to the entailed triple.
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
        if !q.contains("?q") {
            assert_eq!(with, expected, "{q}");
        } else {
            assert!(with.len() < expected.len(), "{q}");
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
}
impl ReasoningProvider for Deriving {
    fn label(&self) -> String {
        "derived-only".into()
    }
    fn start(&self, _: SessionRequest) -> sparkles_core::Result<Box<dyn ReasoningSession>> {
        Ok(Box::new(DerivingSession {
            seeds: self.seeds.clone(),
        }))
    }
}
struct DerivingSession {
    seeds: Arc<AtomicUsize>,
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
        Ok(vec![DemandInfo::default(); d.len()])
    }
    fn derive(&mut self, seeds: Vec<(usize, Vec<u64>)>) -> sparkles_core::Result<Derived> {
        self.seeds
            .fetch_add(seeds.iter().map(|(_, s)| s.len()).sum(), Ordering::SeqCst);
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
    let o = QueryOptions {
        reasoning: Some(Arc::new(Deriving {
            seeds: seeds.clone(),
        })),
        ..options(&opens, false)
    };
    let with = eager(&s, q, &o).unwrap();
    assert_eq!(opens.swap(0, Ordering::SeqCst), without);
    assert_eq!(with, plain);
    assert_eq!(seeds.load(Ordering::SeqCst), 0);
    let streamed_rows = streamed(&s, q, &o).unwrap();
    assert_eq!(opens.load(Ordering::SeqCst), without);
    assert_eq!(streamed_rows, plain);
}

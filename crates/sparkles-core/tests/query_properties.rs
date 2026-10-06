use oxrdf::{Literal, Term};
use sparkles_core::sparql::extensions::{
    ArgumentShape, ExtensionRegistry, PropertyContext, PropertyDescriptor, PropertyFunction,
    PropertyInput, PropertyPosition, PropertyRow, PropertyStream, ScalarError,
};
use sparkles_core::sparql::{QueryOptions, explain, query};
use sparkles_core::store::{Store, StoreOptions};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

const IRI: &str = "urn:test:property";
fn store() -> Store {
    Store::in_memory(StoreOptions::default())
}
fn value(n: i64) -> Option<Term> {
    Some(Literal::from(n).into())
}
fn row(n: i64) -> PropertyRow {
    PropertyRow {
        subject: vec![None],
        object: vec![value(n)],
    }
}
fn insert(s: &Store, text: &str) {
    sparkles_core::sparql::update::update(s, text, &Default::default()).unwrap();
}

struct Rows {
    rows: std::vec::IntoIter<PropertyRow>,
    error: Option<ScalarError>,
    drops: Arc<AtomicUsize>,
    panic_drop: bool,
}
impl PropertyStream for Rows {
    fn next(&mut self, context: &PropertyContext<'_>) -> Result<Option<PropertyRow>, ScalarError> {
        context.check()?;
        match self.rows.next() {
            Some(row) => Ok(Some(row)),
            None => match self.error.take() {
                Some(e) => Err(e),
                None => Ok(None),
            },
        }
    }
}
impl Drop for Rows {
    fn drop(&mut self) {
        self.drops.fetch_add(1, Ordering::SeqCst);
        if self.panic_drop {
            panic!("secret destructor")
        }
    }
}
struct Factory {
    opens: Arc<AtomicUsize>,
    drops: Arc<AtomicUsize>,
    mode: u8,
}
impl PropertyFunction for Factory {
    fn open<'q>(
        &self,
        context: PropertyContext<'q>,
        input: PropertyInput,
    ) -> Result<Box<dyn PropertyStream + 'q>, ScalarError> {
        self.opens.fetch_add(1, Ordering::SeqCst);
        let rows = match self.mode {
            0 => vec![row(7), row(7)],
            1 => vec![PropertyRow {
                subject: vec![None],
                object: vec![input.subject[0].clone()],
            }],
            2..=4 => vec![row(7)],
            5 => vec![PropertyRow {
                subject: vec![value(99)],
                object: vec![None],
            }],
            6 => vec![PropertyRow {
                subject: vec![None],
                object: vec![],
            }],
            7 => {
                context.retain(8192).ok();
                vec![row(7)]
            }
            11 => {
                context.retain(u64::MAX).ok();
                vec![row(7)]
            }
            12 => {
                context.charge(u64::MAX).ok();
                vec![row(7)]
            }
            8 => panic!("secret factory"),
            9 => return Err(ScalarError::Expression),
            10 => vec![PropertyRow {
                subject: vec![None; input.subject.len()],
                object: vec![value(input.subject.len() as i64)],
            }],
            _ => vec![],
        };
        Ok(Box::new(Rows {
            rows: rows.into_iter(),
            error: match self.mode {
                2 => Some(ScalarError::Expression),
                3 => Some(ScalarError::Execution("broken".into())),
                _ => None,
            },
            drops: self.drops.clone(),
            panic_drop: self.mode == 4,
        }))
    }
}
fn descriptor(
    mode: u8,
    required: bool,
) -> (PropertyDescriptor, Arc<AtomicUsize>, Arc<AtomicUsize>) {
    let opens = Arc::new(AtomicUsize::new(0));
    let drops = Arc::new(AtomicUsize::new(0));
    let d = PropertyDescriptor::new(
        IRI,
        ArgumentShape::Term,
        ArgumentShape::Term,
        if required {
            vec![PropertyPosition::Subject(0)]
        } else {
            vec![]
        },
        vec![PropertyPosition::Object(0)],
        Factory {
            opens: opens.clone(),
            drops: drops.clone(),
            mode,
        },
    )
    .unwrap();
    (d, opens, drops)
}
fn options(d: PropertyDescriptor) -> QueryOptions {
    let mut r = ExtensionRegistry::builder();
    r.register_property(d).unwrap();
    QueryOptions {
        extensions: Some(r.build()),
        ..Default::default()
    }
}
fn run(s: &Store, source: &str, opts: &QueryOptions) -> Vec<Vec<Option<Term>>> {
    query(s.snapshot(), source, opts).unwrap().rows()
}

#[test]
fn per_input_duplicates_and_empty_inputs() {
    let s = store();
    let (d, opens, drops) = descriptor(0, true);
    let o = options(d);
    let rows = run(
        &s,
        &format!("SELECT ?x ?y {{ VALUES ?x {{1 1}} ?x <{IRI}> ?y }}"),
        &o,
    );
    assert_eq!(rows, vec![vec![value(1), value(7)]; 4]);
    assert_eq!(opens.load(Ordering::SeqCst), 2);
    assert_eq!(drops.load(Ordering::SeqCst), 2);
    assert!(
        run(
            &s,
            &format!("SELECT * {{ VALUES ?x {{}} ?x <{IRI}> ?y }}"),
            &o
        )
        .is_empty()
    );
    assert_eq!(opens.load(Ordering::SeqCst), 2);
}

#[test]
fn ordinary_bgp_supplies_required_input_even_when_written_later() {
    let s = store();
    insert(&s, "INSERT DATA { <urn:s> <urn:p> 11 }");
    let (d, opens, _) = descriptor(1, true);
    let o = options(d);
    assert_eq!(
        run(
            &s,
            &format!("SELECT ?y {{ ?x <{IRI}> ?y . <urn:s> <urn:p> ?x }}"),
            &o
        ),
        vec![vec![value(11)]]
    );
    assert_eq!(opens.load(Ordering::SeqCst), 1);
}

#[test]
fn schema_missing_fails_but_runtime_unbound_skips_open() {
    let s = store();
    let (d, opens, _) = descriptor(1, true);
    let o = options(d);
    assert!(query(s.snapshot(), &format!("SELECT * {{ ?x <{IRI}> ?y }}"), &o).is_err());
    assert_eq!(opens.load(Ordering::SeqCst), 0);
    assert_eq!(
        run(
            &s,
            &format!("SELECT ?y {{ VALUES ?x {{UNDEF 3}} ?x <{IRI}> ?y }}"),
            &o
        ),
        vec![vec![value(3)]]
    );
    assert_eq!(opens.load(Ordering::SeqCst), 1);
}

#[test]
fn bound_outputs_and_repeated_variables_are_constraints() {
    let s = store();
    let (d, opens, _) = descriptor(0, true);
    let o = options(d);
    assert_eq!(
        run(
            &s,
            &format!("SELECT ?x {{ VALUES (?x ?y) {{(1 7) (2 8)}} ?x <{IRI}> ?y }}"),
            &o
        ),
        vec![vec![value(1)]; 2]
    );
    assert_eq!(opens.load(Ordering::SeqCst), 2);
    assert_eq!(
        run(
            &s,
            &format!("SELECT ?x {{ VALUES ?x {{7 8}} ?x <{IRI}> ?x }}"),
            &o
        ),
        vec![vec![value(7)]; 2]
    );
}

#[test]
fn optional_and_lateral_do_not_memoize_duplicate_inputs() {
    let s = store();
    for optional in [false, true] {
        let (d, opens, drops) = descriptor(1, true);
        let o = options(d);
        let op = if optional { "OPTIONAL" } else { "LATERAL" };
        let rows = run(
            &s,
            &format!("SELECT ?x ?y {{ VALUES ?x {{1 1 UNDEF}} {op} {{ ?x <{IRI}> ?y }} }}"),
            &o,
        );
        assert_eq!(rows.len(), if optional { 3 } else { 2 });
        assert_eq!(opens.load(Ordering::SeqCst), 2);
        assert_eq!(drops.load(Ordering::SeqCst), 2);
        assert_eq!(&rows[..2], &vec![vec![value(1), value(1)]; 2]);
    }
}

#[test]
fn domain_errors_discard_all_provisional_rows_and_cleanup() {
    let s = store();
    for mode in [2, 9] {
        let (d, opens, drops) = descriptor(mode, true);
        let o = options(d);
        assert!(
            run(
                &s,
                &format!("SELECT ?y {{ VALUES ?x {{1 2}} ?x <{IRI}> ?y }}"),
                &o
            )
            .is_empty()
        );
        assert_eq!(opens.load(Ordering::SeqCst), 2);
        assert_eq!(drops.load(Ordering::SeqCst), if mode == 2 { 2 } else { 0 });
        let t = s.write();
        t.commit().unwrap();
    }
}

#[test]
fn discarded_rows_reuse_charged_storage_under_a_finite_memory_budget() {
    let s = store();
    let (d, opens, drops) = descriptor(2, true);
    let mut o = options(d);
    o.max_memory_bytes = Some(20_000);
    let inputs = std::iter::repeat_n("1", 1500).collect::<Vec<_>>().join(" ");
    assert!(
        run(
            &s,
            &format!("SELECT ?y {{ VALUES ?x {{{inputs}}} ?x <{IRI}> ?y }}"),
            &o,
        )
        .is_empty()
    );
    assert_eq!(opens.load(Ordering::SeqCst), 1500);
    assert_eq!(drops.load(Ordering::SeqCst), 1500);
    o.max_rows_produced = Some(100);
    assert!(
        query(
            s.snapshot(),
            &format!("SELECT ?y {{ VALUES ?x {{{inputs}}} ?x <{IRI}> ?y }}"),
            &o,
        )
        .is_err()
    );
}

#[test]
fn lifecycle_protocol_and_suppressed_budget_errors_are_fatal() {
    let s = store();
    for mode in [3, 4, 6, 7, 8, 11, 12] {
        let (d, opens, drops) = descriptor(mode, false);
        let mut o = options(d);
        if mode == 7 {
            o.max_memory_bytes = Some(4096);
        }
        let error = query(
            s.snapshot(),
            &format!("SELECT * {{ <urn:s> <{IRI}> ?y }}"),
            &o,
        )
        .err()
        .unwrap()
        .to_string();
        assert!(!error.contains("secret"));
        assert_eq!(opens.load(Ordering::SeqCst), 1);
        assert_eq!(drops.load(Ordering::SeqCst), if mode == 8 { 0 } else { 1 });
    }
}

#[test]
fn undeclared_output_binding_is_fatal() {
    let s = store();
    let (d, _, _) = descriptor(5, false);
    assert!(
        query(
            s.snapshot(),
            &format!("SELECT * {{ ?x <{IRI}> ?y }}"),
            &options(d)
        )
        .is_err()
    );
}

#[test]
fn unused_registry_variable_predicates_paths_and_unknown_iris_stay_rdf() {
    let s = store();
    insert(
        &s,
        &format!("INSERT DATA {{ <urn:s> <{IRI}> 5 . <urn:s> <urn:other> 6 }}"),
    );
    let (d, opens, _) = descriptor(8, false);
    let o = options(d);
    let anonymous = regex::Regex::new("[0-9a-f]{16,32}").unwrap();
    for source in [
        format!("SELECT * {{ ?s <{IRI}>+ ?o }}"),
        "SELECT * { ?s ?p ?o }".into(),
        "SELECT * { ?s <urn:other> ?o }".into(),
        "SELECT (COUNT(*) AS ?n) { ?s ?p ?o }".into(),
    ] {
        let plain = explain(s.snapshot(), &source, &Default::default()).unwrap();
        let registered = explain(s.snapshot(), &source, &o).unwrap();
        assert_eq!(
            anonymous.replace_all(&serde_json::to_string(&plain).unwrap(), "anon"),
            anonymous.replace_all(&serde_json::to_string(&registered).unwrap(), "anon")
        );
        assert_eq!(run(&s, &source, &o), run(&s, &source, &Default::default()));
    }
    assert_eq!(opens.load(Ordering::SeqCst), 0);
}

#[test]
fn descriptors_reject_shapes_positions_duplicates_and_builtins() {
    let (d, _, _) = descriptor(0, false);
    let mut b = ExtensionRegistry::builder();
    b.register_property(d.clone()).unwrap();
    assert!(b.register_property(d.clone()).is_err());
    for mut bad in [d.clone(), d.clone(), d.clone(), d.clone()] {
        bad.required = vec![PropertyPosition::Subject(0)];
        bad.produces.push(PropertyPosition::Subject(0));
        assert!(ExtensionRegistry::builder().register_property(bad).is_err());
    }
    let mut bad = d.clone();
    bad.cardinality_factor = Some(f64::NAN);
    assert!(ExtensionRegistry::builder().register_property(bad).is_err());
    let mut bad = d.clone();
    bad.subject = ArgumentShape::List(0..=128);
    assert!(ExtensionRegistry::builder().register_property(bad).is_err());
    for iri in [
        "http://jena.apache.org/ARQ/property#strSplit",
        "http://jena.apache.org/spatial#nearby",
    ] {
        let mut bad = d.clone();
        bad.iri = oxrdf::NamedNode::new(iri).unwrap();
        assert!(ExtensionRegistry::builder().register_property(bad).is_err());
    }
}

struct ScanFactory {
    batches: Arc<AtomicUsize>,
    subjects: bool,
    worker: bool,
}
struct ScanStream<'q> {
    scan: sparkles_core::sparql::extensions::PropertyScan<'q>,
    batch: Option<sparkles_core::sparql::extensions::PropertyReadBatch<'q>>,
    index: usize,
    batches: Arc<AtomicUsize>,
    subjects: bool,
    worker: bool,
}
impl PropertyFunction for ScanFactory {
    fn open<'q>(
        &self,
        context: PropertyContext<'q>,
        _: PropertyInput,
    ) -> Result<Box<dyn PropertyStream + 'q>, ScalarError> {
        let scan = context.view().scan(None, None, None)?;
        Ok(Box::new(ScanStream {
            scan,
            batch: None,
            index: 0,
            batches: self.batches.clone(),
            subjects: self.subjects,
            worker: self.worker,
        }))
    }
}
impl PropertyStream for ScanStream<'_> {
    fn next(&mut self, _: &PropertyContext<'_>) -> Result<Option<PropertyRow>, ScalarError> {
        if self
            .batch
            .as_ref()
            .is_none_or(|b| self.index == b.triples().len())
        {
            self.batch = if self.worker {
                std::thread::scope(|scope| scope.spawn(|| self.scan.next_batch()).join().unwrap())?
            } else {
                self.scan.next_batch()?
            };
            self.index = 0;
            if self.batch.is_some() {
                self.batches.fetch_add(1, Ordering::SeqCst);
            }
        }
        let Some(batch) = &self.batch else {
            return Ok(None);
        };
        let triple = &batch.triples()[self.index];
        self.index += 1;
        Ok(Some(PropertyRow {
            subject: vec![None],
            object: vec![Some(if self.subjects {
                triple.subject.clone().into()
            } else {
                triple.object.clone()
            })],
        }))
    }
}
fn scans(subjects: bool, worker: bool) -> (QueryOptions, Arc<AtomicUsize>) {
    let batches = Arc::new(AtomicUsize::new(0));
    (
        options(
            PropertyDescriptor::new(
                IRI,
                ArgumentShape::Term,
                ArgumentShape::Term,
                vec![],
                vec![PropertyPosition::Object(0)],
                ScanFactory {
                    batches: batches.clone(),
                    subjects,
                    worker,
                },
            )
            .unwrap(),
        ),
        batches,
    )
}

#[test]
fn captured_scans_respect_graph_selection_and_actual_worker_threads() {
    let s = store();
    insert(
        &s,
        "INSERT DATA { <urn:d> <urn:p> 0 . GRAPH <urn:a> { <urn:a> <urn:p> 1 } GRAPH <urn:b> { <urn:b> <urn:p> 2 } }",
    );
    let (mut o, batches) = scans(false, true);
    let source = format!("SELECT ?y {{ <urn:s> <{IRI}> ?y }}");
    assert_eq!(run(&s, &source, &o), vec![vec![value(0)]]);
    assert_eq!(
        run(
            &s,
            &format!("SELECT ?y FROM <urn:a> FROM <urn:b> {{ <urn:s> <{IRI}> ?y }} ORDER BY ?y"),
            &o
        ),
        vec![vec![value(1)], vec![value(2)]]
    );
    o.graphs = Some(Arc::new(sparkles_core::access::GraphAccess::graphs(
        sparkles_core::access::Graphs::Only(sparkles_core::access::GraphRule::new(["urn:a"], &[])),
        sparkles_core::access::Graphs::none(),
    )));
    assert!(run(&s, &source, &o).is_empty());
    assert_eq!(
        run(
            &s,
            &format!(
                "SELECT ?g ?y FROM NAMED <urn:a> FROM NAMED <urn:b> {{ GRAPH ?g {{ <urn:s> <{IRI}> ?y }} }}"
            ),
            &o
        ),
        vec![vec![
            Some(oxrdf::NamedNode::new("urn:a").unwrap().into()),
            value(1)
        ]]
    );
    assert!(
        run(
            &s,
            &format!("SELECT ?y {{ GRAPH <urn:b> {{ <urn:s> <{IRI}> ?y }} }}"),
            &o
        )
        .is_empty()
    );
    assert_eq!(batches.load(Ordering::SeqCst), 3);
}

#[test]
fn paged_union_scans_deduplicate_across_graphs_and_batch_boundaries() {
    let s = Store::in_memory(StoreOptions {
        union_default_graph: true,
        ..Default::default()
    });
    let triples: String = (0..1100)
        .map(|i| format!("<urn:s{i:04}> <urn:p> {i} . "))
        .collect();
    insert(
        &s,
        &format!("INSERT DATA {{ GRAPH <urn:a> {{ {triples} }} GRAPH <urn:b> {{ {triples} }} }}"),
    );
    s.compact().unwrap();
    // Include a delta over the immutable base, so the same paging boundary exercises
    // both Block and Row chunks and the merged graph duplicate ledger.
    insert(
        &s,
        "INSERT DATA { GRAPH <urn:b> { <urn:s1100> <urn:p> 1100 } }",
    );
    let (o, batches) = scans(false, false);
    let rows = run(
        &s,
        &format!("SELECT ?y {{ <urn:s> <{IRI}> ?y }} ORDER BY ?y"),
        &o,
    );
    assert_eq!(rows, (0..1101).map(|i| vec![value(i)]).collect::<Vec<_>>());
    assert_eq!(batches.load(Ordering::SeqCst), 2);
}

#[test]
fn captured_read_work_and_memory_are_bounded_and_sticky() {
    let s = store();
    insert(&s, "INSERT DATA { <urn:a> <urn:p> 1 . <urn:b> <urn:p> 2 }");
    for memory in [false, true] {
        let (mut o, _) = scans(false, false);
        if memory {
            o.max_memory_bytes = Some(400);
        } else {
            o.max_rows_produced = Some(1);
        }
        assert!(
            query(
                s.snapshot(),
                &format!("SELECT * {{ <urn:s> <{IRI}> ?y }}"),
                &o
            )
            .is_err()
        );
    }
}

#[test]
fn scans_preserve_received_stored_blank_nodes_without_forgery() {
    let s = store();
    insert(&s, "INSERT DATA { _:stored <urn:p> 1 }");
    let stored = run(&s, "SELECT ?s { ?s <urn:p> 1 }", &Default::default())[0][0].clone();
    let (o, _) = scans(true, false);
    assert_eq!(
        run(&s, &format!("SELECT ?y {{ <urn:s> <{IRI}> ?y }}"), &o),
        vec![vec![stored]]
    );
}

struct ListFactory;
impl PropertyFunction for ListFactory {
    fn open<'q>(
        &self,
        _: PropertyContext<'q>,
        input: PropertyInput,
    ) -> Result<Box<dyn PropertyStream + 'q>, ScalarError> {
        let output = PropertyRow {
            subject: vec![None; input.subject.len()],
            object: input.subject.clone(),
        };
        Ok(Box::new(Rows {
            rows: vec![output].into_iter(),
            error: None,
            drops: Arc::new(AtomicUsize::new(0)),
            panic_drop: false,
        }))
    }
}
#[test]
fn list_shapes_and_dependency_ordering() {
    let s = store();
    let list = PropertyDescriptor::new(
        IRI,
        ArgumentShape::List(2..=2),
        ArgumentShape::List(2..=2),
        vec![PropertyPosition::Subject(0), PropertyPosition::Subject(1)],
        vec![PropertyPosition::Object(0), PropertyPosition::Object(1)],
        ListFactory,
    )
    .unwrap();
    let o = options(list);
    assert_eq!(
        run(&s, &format!("SELECT ?a ?b {{ (1 2) <{IRI}> (?a ?b) }}"), &o),
        vec![vec![value(1), value(2)]]
    );
    assert!(
        query(
            s.snapshot(),
            &format!("SELECT * {{ (1) <{IRI}> (?a ?b) }}"),
            &o
        )
        .is_err()
    );
    let (first, opens, _) = descriptor(1, true);
    let mut second = first.clone();
    second.iri = oxrdf::NamedNode::new("urn:test:second").unwrap();
    let mut registry = ExtensionRegistry::builder();
    registry
        .register_property(first)
        .unwrap()
        .register_property(second)
        .unwrap();
    let o = QueryOptions {
        extensions: Some(registry.build()),
        ..Default::default()
    };
    // The dependent call is written first, and must wait for the independent call.
    assert_eq!(
        run(
            &s,
            &format!("SELECT ?z {{ ?y <urn:test:second> ?z . 4 <{IRI}> ?y }}"),
            &o
        ),
        vec![vec![value(4)]]
    );
    assert_eq!(opens.load(Ordering::SeqCst), 2);
    assert!(
        query(
            s.snapshot(),
            &format!("SELECT * {{ ?y <urn:test:second> ?z . ?z <{IRI}> ?y }}"),
            &o
        )
        .is_err()
    );
    assert_eq!(opens.load(Ordering::SeqCst), 2);
}

#[test]
fn initial_bindings_union_subqueries_and_exists_preserve_callback_counts() {
    let s = store();
    let (d, opens, _) = descriptor(1, true);
    let mut o = options(d);
    o.initial_bindings
        .push(("x".into(), Literal::from(42).into()));
    assert_eq!(
        run(&s, &format!("SELECT ?x ?y {{ ?x <{IRI}> ?y }}"), &o),
        vec![vec![value(42), value(42)]]
    );
    o.initial_bindings.clear();
    assert_eq!(
        run(
            &s,
            &format!(
                "SELECT ?y {{ {{ SELECT ?y {{ VALUES ?x {{1 1}} ?x <{IRI}> ?y }} }} UNION {{ VALUES ?x {{2}} ?x <{IRI}> ?y }} }} ORDER BY ?y"
            ),
            &o
        ),
        vec![vec![value(1)], vec![value(1)], vec![value(2)]]
    );
    assert_eq!(opens.load(Ordering::SeqCst), 4);
    assert_eq!(
        run(
            &s,
            &format!("SELECT ?x {{ VALUES ?x {{1 1 UNDEF}} FILTER EXISTS {{ ?x <{IRI}> ?y }} }}"),
            &o
        ),
        vec![vec![value(1)]; 2]
    );
    assert_eq!(opens.load(Ordering::SeqCst), 6);
    let before = opens.load(Ordering::SeqCst);
    assert!(query(s.snapshot(), &format!("SELECT ?y {{ VALUES ?x {{1}} LATERAL {{ BIND(?x AS ?v) {{ SELECT ?y {{ ?x <{IRI}> ?y }} }} }} }}"), &o).is_err());
    assert_eq!(
        opens.load(Ordering::SeqCst),
        before,
        "a hidden subquery input must fail schema planning before callbacks"
    );
}

#[test]
fn callback_filter_barriers_keep_evaluation_at_the_algebra_location() {
    let s = store();
    let (d, opens, _) = descriptor(1, true);
    let o = options(d);
    assert_eq!(
        run(
            &s,
            &format!("SELECT ?x {{ VALUES ?x {{1 2}} ?x <{IRI}> ?y FILTER(?x = 1) }}"),
            &o
        ),
        vec![vec![value(1)]]
    );
    assert_eq!(opens.load(Ordering::SeqCst), 2);
    assert_eq!(
        run(
            &s,
            &format!("SELECT ?x {{ {{ VALUES ?x {{1 2}} FILTER(?x = 1) }} ?x <{IRI}> ?y }}"),
            &o
        ),
        vec![vec![value(1)]]
    );
    assert_eq!(opens.load(Ordering::SeqCst), 3);
    // No required bindings still means one invocation per duplicate LATERAL row.
    let (d, opens, _) = descriptor(0, false);
    let o = options(d);
    assert_eq!(
        run(
            &s,
            &format!("SELECT ?y {{ VALUES ?x {{1 1}} LATERAL {{ <urn:s> <{IRI}> ?y }} }}"),
            &o
        )
        .len(),
        4
    );
    assert_eq!(opens.load(Ordering::SeqCst), 2);
}

struct CancelFactory {
    cancel: Arc<std::sync::atomic::AtomicBool>,
    drops: Arc<AtomicUsize>,
}
impl PropertyFunction for CancelFactory {
    fn open<'q>(
        &self,
        _: PropertyContext<'q>,
        _: PropertyInput,
    ) -> Result<Box<dyn PropertyStream + 'q>, ScalarError> {
        self.cancel.store(true, Ordering::SeqCst);
        Ok(Box::new(Rows {
            rows: vec![row(7)].into_iter(),
            error: None,
            drops: self.drops.clone(),
            panic_drop: true,
        }))
    }
}
#[test]
fn successful_factory_return_is_guarded_before_post_call_cancellation() {
    let s = store();
    let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let drops = Arc::new(AtomicUsize::new(0));
    let mut o = options(
        PropertyDescriptor::new(
            IRI,
            ArgumentShape::Term,
            ArgumentShape::Term,
            vec![],
            vec![PropertyPosition::Object(0)],
            CancelFactory {
                cancel: cancel.clone(),
                drops: drops.clone(),
            },
        )
        .unwrap(),
    );
    o.cancel = Some(cancel);
    assert!(query(s.snapshot(), &format!("ASK {{ <urn:s> <{IRI}> ?y }}"), &o).is_err());
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}

#[test]
fn fatal_property_failure_prevents_prior_update_mutations_committing() {
    let s = store();
    let (d, _, drops) = descriptor(3, false);
    let o = options(d);
    let mut transaction = s.write();
    let update = format!(
        "INSERT DATA {{ <urn:prior> <urn:p> 1 }}; INSERT {{ <urn:new> <urn:p> ?y }} WHERE {{ <urn:s> <{IRI}> ?y }}"
    );
    assert!(sparkles_core::sparql::update::update_in(&mut transaction, &update, &o).is_err());
    assert!(transaction.commit().is_err());
    assert_eq!(drops.load(Ordering::SeqCst), 1);
    assert!(run(&s, "SELECT * { ?s ?p ?o }", &Default::default()).is_empty());
    insert(&s, "INSERT DATA { <urn:recovered> <urn:p> 2 }");
}

fn bounded(f: impl FnOnce() + Send + 'static) {
    let (send, receive) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        send.send(std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)))
            .unwrap();
    });
    receive
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("property callback deadlocked")
        .unwrap();
    worker.join().unwrap();
}
struct Reenter {
    target: Arc<Store>,
    phase: u8,
}
impl Reenter {
    fn attempt(&self, phase: u8) {
        if self.phase == phase {
            let _ = self.target.try_write_with(
                sparkles_core::commit::CommitKind::Transaction,
                Default::default(),
            );
        }
    }
}
impl PropertyFunction for Reenter {
    fn open<'q>(
        &self,
        _: PropertyContext<'q>,
        _: PropertyInput,
    ) -> Result<Box<dyn PropertyStream + 'q>, ScalarError> {
        self.attempt(0);
        Ok(Box::new(Reenter {
            target: self.target.clone(),
            phase: self.phase,
        }))
    }
}
impl PropertyStream for Reenter {
    fn next(&mut self, _: &PropertyContext<'_>) -> Result<Option<PropertyRow>, ScalarError> {
        self.attempt(1);
        Ok(None)
    }
}
impl Drop for Reenter {
    fn drop(&mut self) {
        self.attempt(2);
    }
}
#[test]
fn same_and_cross_held_families_are_guarded_in_all_lifecycle_stages() {
    for same in [false, true] {
        for phase in 0..3 {
            bounded(move || {
                let a = Arc::new(store());
                let b = if same { a.clone() } else { Arc::new(store()) };
                let o = options(
                    PropertyDescriptor::new(
                        IRI,
                        ArgumentShape::Term,
                        ArgumentShape::Term,
                        vec![],
                        vec![PropertyPosition::Object(0)],
                        Reenter {
                            target: b.clone(),
                            phase,
                        },
                    )
                    .unwrap(),
                );
                let t = b.write();
                let snapshot = if same {
                    Arc::new(t.view())
                } else {
                    a.snapshot()
                };
                assert!(query(snapshot, &format!("ASK {{ <urn:s> <{IRI}> ?y }}"), &o).is_err());
                assert!(t.commit().is_err());
                // Dropping the registry-owned factory later is outside execution; do
                // not treat that ordinary host teardown as a property stream callback.
                drop(o);
                let t = b.write();
                t.commit().unwrap();
            });
        }
    }
}

#[test]
fn captured_reads_hide_protected_triples() {
    use sparkles_core::access::{
        Caller, GraphAccess, Graphs, Limits, Protection, Rule, TripleRules,
    };
    let s = store();
    insert(
        &s,
        "INSERT DATA { <urn:s> <urn:public> 1 . <urn:s> <urn:secret> 2 }",
    );
    let protection = Protection {
        name: "private".into(),
        predicates: Some(vec!["urn:secret".into()]),
        classes: None,
        subclasses: false,
        graphs: None,
        pattern: None,
        prefixes: Default::default(),
        hide_inferences: false,
    };
    let rules = TripleRules {
        rules: vec![Rule {
            protection: Arc::new(protection),
            read: Graphs::none(),
            write: Graphs::none(),
        }],
        caller: Caller::default(),
        limits: Limits::default(),
    };
    let (mut o, _) = scans(false, false);
    o.graphs = Some(Arc::new(GraphAccess::with_triples(
        Graphs::All,
        Graphs::All,
        rules,
    )));
    assert_eq!(
        run(&s, &format!("SELECT ?y {{ <urn:s> <{IRI}> ?y }}"), &o),
        vec![vec![value(1)]]
    );
}

struct PatternFactory {
    subject: Option<oxrdf::NamedOrBlankNode>,
    predicate: Option<oxrdf::NamedNode>,
    object: Option<Term>,
    received: bool,
}
impl PropertyFunction for PatternFactory {
    fn open<'q>(
        &self,
        context: PropertyContext<'q>,
        input: PropertyInput,
    ) -> Result<Box<dyn PropertyStream + 'q>, ScalarError> {
        let subject = if self.received {
            Some(oxrdf::NamedOrBlankNode::try_from(input.subject[0].clone().unwrap()).unwrap())
        } else {
            self.subject.clone()
        };
        let scan = context
            .view()
            .scan(subject, self.predicate.clone(), self.object.clone())?;
        Ok(Box::new(ScanStream {
            scan,
            batch: None,
            index: 0,
            batches: Arc::new(AtomicUsize::new(0)),
            subjects: false,
            worker: false,
        }))
    }
}
#[test]
fn scan_patterns_use_rdf_identity_and_cannot_forge_stored_blank_nodes() {
    let s = store();
    insert(
        &s,
        "INSERT DATA { _:stored <urn:p> 1 . <urn:s> <urn:p> 2 . <urn:s> <urn:q> 3 }",
    );
    let stored = run(&s, "SELECT ?s { ?s <urn:p> 1 }", &Default::default())[0][0]
        .clone()
        .unwrap();
    for received in [false, true] {
        let d = PropertyDescriptor::new(
            IRI,
            ArgumentShape::Term,
            ArgumentShape::Term,
            if received {
                vec![PropertyPosition::Subject(0)]
            } else {
                vec![]
            },
            vec![PropertyPosition::Object(0)],
            PatternFactory {
                subject: Some(oxrdf::NamedOrBlankNode::try_from(stored.clone()).unwrap()),
                predicate: Some(oxrdf::NamedNode::new("urn:p").unwrap()),
                object: None,
                received,
            },
        )
        .unwrap();
        let mut o = options(d);
        if received {
            o.initial_bindings.push(("x".into(), stored.clone()));
        }
        let rows = run(&s, &format!("SELECT ?y {{ ?x <{IRI}> ?y }}"), &o);
        assert_eq!(
            rows,
            if received {
                vec![vec![value(1)]]
            } else {
                vec![]
            }
        );
    }
    for subject in [false, true] {
        for predicate in [false, true] {
            for object in [false, true] {
                let o = options(
                    PropertyDescriptor::new(
                        IRI,
                        ArgumentShape::Term,
                        ArgumentShape::Term,
                        vec![],
                        vec![PropertyPosition::Object(0)],
                        PatternFactory {
                            subject: subject
                                .then(|| oxrdf::NamedNode::new("urn:s").unwrap().into()),
                            predicate: predicate.then(|| oxrdf::NamedNode::new("urn:p").unwrap()),
                            object: object.then(|| Literal::from(2).into()),
                            received: false,
                        },
                    )
                    .unwrap(),
                );
                let mut expected = vec![value(2)];
                if !object {
                    if !subject {
                        expected.push(value(1));
                    }
                    if !predicate {
                        expected.push(value(3));
                    }
                }
                expected.sort_by_key(|t| t.as_ref().unwrap().to_string());
                assert_eq!(
                    run(
                        &s,
                        &format!("SELECT ?y {{ <urn:s> <{IRI}> ?y }} ORDER BY ?y"),
                        &o
                    ),
                    expected.into_iter().map(|t| vec![t]).collect::<Vec<_>>()
                );
            }
        }
    }
}

struct Returned(Term);
impl PropertyFunction for Returned {
    fn open<'q>(
        &self,
        _: PropertyContext<'q>,
        _: PropertyInput,
    ) -> Result<Box<dyn PropertyStream + 'q>, ScalarError> {
        Ok(Box::new(Rows {
            rows: vec![PropertyRow {
                subject: vec![None],
                object: vec![Some(self.0.clone())],
            }]
            .into_iter(),
            error: None,
            drops: Arc::new(AtomicUsize::new(0)),
            panic_drop: false,
        }))
    }
}
#[test]
fn output_validation_and_blank_identity_are_bounded() {
    let s = store();
    insert(&s, "INSERT DATA { _:stored <urn:p> 1 }");
    let stored = run(&s, "SELECT ?s { ?s <urn:p> 1 }", &Default::default())[0][0]
        .clone()
        .unwrap();
    let make = |t| {
        options(
            PropertyDescriptor::new(
                IRI,
                ArgumentShape::Term,
                ArgumentShape::Term,
                vec![],
                vec![PropertyPosition::Object(0)],
                Returned(t),
            )
            .unwrap(),
        )
    };
    let rows = run(
        &s,
        &format!("SELECT ?y {{ <urn:s> <{IRI}> ?y }}"),
        &make(stored.clone()),
    );
    assert_ne!(rows[0][0], Some(stored));
    assert!(matches!(rows[0][0], Some(Term::BlankNode(_))));
    for bad in [
        oxrdf::NamedNode::new_unchecked("relative").into(),
        Literal::from("x".repeat(1 << 20)).into(),
    ] {
        assert!(
            query(
                s.snapshot(),
                &format!("ASK {{ <urn:s> <{IRI}> ?y }}"),
                &make(bad)
            )
            .is_err()
        );
    }
}

#[test]
fn byte_bounded_scan_batches_resume_without_skipping_the_next_triple() {
    let s = store();
    let literal = "x".repeat(600_000);
    insert(
        &s,
        &format!("INSERT DATA {{ <urn:a> <urn:p> \"{literal}\" . <urn:b> <urn:p> \"{literal}\" }}"),
    );
    let (o, batches) = scans(true, false);
    assert_eq!(
        run(
            &s,
            &format!("SELECT ?y {{ <urn:s> <{IRI}> ?y }} ORDER BY ?y"),
            &o
        ),
        vec![
            vec![Some(oxrdf::NamedNode::new("urn:a").unwrap().into())],
            vec![Some(oxrdf::NamedNode::new("urn:b").unwrap().into())]
        ]
    );
    assert_eq!(batches.load(Ordering::SeqCst), 2);
}

#[test]
fn dynamic_missing_input_exists_is_sticky_after_prior_callbacks_and_updates() {
    let s = store();
    let (d, opens, _) = descriptor(1, true);
    let o = options(d);
    let pattern =
        format!("VALUES ?x {{1}} ?x <{IRI}> ?y FILTER EXISTS {{ ?missing <{IRI}> ?out }}");
    assert!(query(s.snapshot(), &format!("SELECT * {{ {pattern} }}"), &o).is_err());
    assert_eq!(opens.load(Ordering::SeqCst), 1);
    let mut transaction = s.write();
    let update = format!(
        "INSERT DATA {{ <urn:prior> <urn:p> 1 }}; INSERT {{ <urn:new> <urn:p> ?y }} WHERE {{ {pattern} }}"
    );
    assert!(sparkles_core::sparql::update::update_in(&mut transaction, &update, &o).is_err());
    assert!(transaction.commit().is_err());
    assert_eq!(opens.load(Ordering::SeqCst), 2);
    assert!(run(&s, "SELECT * { ?s ?p ?o }", &Default::default()).is_empty());
    insert(&s, "INSERT DATA { <urn:recovered> <urn:p> 1 }");
}

#[test]
fn received_composite_blank_nodes_roundtrip_through_outputs() {
    let s = store();
    insert(&s, "INSERT DATA { _:stored <urn:p> 1 }");
    let (d, _, _) = descriptor(1, true);
    let o = options(d);
    let rows = run(
        &s,
        &format!(
            "PREFIX cdt: <http://w3id.org/awslabs/neptune/SPARQL-CDTs/> SELECT ?x ?y {{ ?s <urn:p> 1 BIND(cdt:List(?s) AS ?x) ?x <{IRI}> ?y }}"
        ),
        &o,
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], rows[0][1]);
    insert(
        &s,
        "INSERT DATA { _:stored <urn:p> 2 . <urn:s> <urn:l> \"[_:stored]\"^^<http://w3id.org/awslabs/neptune/SPARQL-CDTs/List> }",
    );
    let stored_literal = run(&s, "SELECT ?l { <urn:s> <urn:l> ?l }", &Default::default())[0][0]
        .clone()
        .unwrap();
    let o = options(
        PropertyDescriptor::new(
            IRI,
            ArgumentShape::Term,
            ArgumentShape::Term,
            vec![],
            vec![PropertyPosition::Object(0)],
            PatternFactory {
                subject: None,
                predicate: Some(oxrdf::NamedNode::new("urn:l").unwrap()),
                object: None,
                received: false,
            },
        )
        .unwrap(),
    );
    assert_eq!(
        run(&s, &format!("SELECT ?y {{ <urn:s> <{IRI}> ?y }}"), &o),
        vec![vec![Some(stored_literal.clone())]]
    );
    let o = options(
        PropertyDescriptor::new(
            IRI,
            ArgumentShape::Term,
            ArgumentShape::Term,
            vec![],
            vec![PropertyPosition::Object(0)],
            Returned(stored_literal.clone()),
        )
        .unwrap(),
    );
    assert_ne!(
        run(&s, &format!("SELECT ?y {{ <urn:s> <{IRI}> ?y }}"), &o)[0][0],
        Some(stored_literal)
    );
}

struct DrainFactory;
impl PropertyFunction for DrainFactory {
    fn open<'q>(
        &self,
        context: PropertyContext<'q>,
        _: PropertyInput,
    ) -> Result<Box<dyn PropertyStream + 'q>, ScalarError> {
        let mut scan = context.view().scan(None, None, None)?;
        while scan.next_batch()?.is_some() {}
        Ok(Box::new(Rows {
            rows: Vec::new().into_iter(),
            error: None,
            drops: Arc::new(AtomicUsize::new(0)),
            panic_drop: false,
        }))
    }
}
#[test]
fn scan_work_counts_consumed_permitted_candidates_once_across_byte_pages() {
    use sparkles_core::access::{GraphAccess, GraphRule, Graphs};
    let s = Store::in_memory(StoreOptions {
        union_default_graph: true,
        ..Default::default()
    });
    let literal = "x".repeat(600_000);
    let body = format!("<urn:s1> <urn:p> \"{literal}\" . <urn:s2> <urn:p> \"{literal}\" .");
    insert(
        &s,
        &format!("INSERT DATA {{ GRAPH <urn:a> {{ {body} }} GRAPH <urn:b> {{ {body} }} }}"),
    );
    let mut o = options(
        PropertyDescriptor::new(
            IRI,
            ArgumentShape::Term,
            ArgumentShape::Term,
            vec![],
            vec![PropertyPosition::Object(0)],
            DrainFactory,
        )
        .unwrap(),
    );
    let work = Arc::new(std::sync::atomic::AtomicU64::new(0));
    o.work = Some(work.clone());
    assert!(run(&s, &format!("SELECT * {{ <urn:s> <{IRI}> ?y }}"), &o).is_empty());
    assert_eq!(
        work.load(Ordering::SeqCst),
        5,
        "one unit row and four permitted raw scan candidates"
    );
    work.store(0, Ordering::SeqCst);
    o.graphs = Some(Arc::new(GraphAccess::graphs(
        Graphs::Only(GraphRule::new(["urn:a"], &[])),
        Graphs::none(),
    )));
    assert!(run(&s, &format!("SELECT * {{ <urn:s> <{IRI}> ?y }}"), &o).is_empty());
    assert_eq!(
        work.load(Ordering::SeqCst),
        3,
        "excluded graph cardinality is not exposed in work metrics"
    );
}

#[test]
fn registered_lists_support_total_argument_ceiling_and_reject_cycles_before_open() {
    let s = store();
    for length in [65, 127] {
        let (mut d, opens, _) = descriptor(10, false);
        d.subject = ArgumentShape::List(length..=length);
        d.required = vec![PropertyPosition::Subject(length - 1)];
        let o = options(d);
        let terms = (0..length)
            .map(|n| n.to_string())
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(
            run(&s, &format!("SELECT ?y {{ ({terms}) <{IRI}> ?y }}"), &o),
            vec![vec![value(length as i64)]]
        );
        assert_eq!(opens.load(Ordering::SeqCst), 1);
    }
    let (mut d, opens, _) = descriptor(0, false);
    d.subject = ArgumentShape::List(0..=127);
    let o = options(d);
    let source = format!(
        "PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> SELECT * {{ _:a rdf:first 1; rdf:rest _:a . _:a <{IRI}> ?y }}"
    );
    assert!(query(s.snapshot(), &source, &o).is_err());
    assert_eq!(opens.load(Ordering::SeqCst), 0);
}

#[test]
fn unrelated_registered_scalars_keep_ordinary_optional_scope() {
    use sparkles_core::sparql::extensions::{ScalarContext, ScalarDescriptor};
    let s = store();
    let (d, opens, _) = descriptor(8, false);
    let calls = Arc::new(AtomicUsize::new(0));
    let called = calls.clone();
    let mut r = ExtensionRegistry::builder();
    r.register_property(d).unwrap();
    r.register_scalar(
        ScalarDescriptor::new(
            "urn:test:scalar",
            1..=1,
            move |_: &ScalarContext<'_>, args: &[Term]| {
                called.fetch_add(1, Ordering::SeqCst);
                Ok(args[0].clone())
            },
        )
        .unwrap(),
    )
    .unwrap();
    let o = QueryOptions {
        extensions: Some(r.build()),
        ..Default::default()
    };
    assert_eq!(
        run(
            &s,
            "SELECT ?x ?y { VALUES ?x {1} OPTIONAL { BIND(<urn:test:scalar>(?x) AS ?y) } }",
            &o
        ),
        vec![vec![value(1), None]]
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        run(
            &s,
            "SELECT ?y { VALUES ?x {1 1} LATERAL { BIND(<urn:test:scalar>(?x) AS ?y) } }",
            &o
        ),
        vec![vec![value(1)]; 2]
    );
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(opens.load(Ordering::SeqCst), 0);
}

#[test]
fn query_written_list_heads_can_be_shared_across_calls_and_sides() {
    let s = store();
    insert(
        &s,
        "INSERT DATA { <urn:unrelated> <http://www.w3.org/1999/02/22-rdf-syntax-ns#first> 9 }",
    );
    let descriptor = |iri| {
        PropertyDescriptor::new(
            iri,
            ArgumentShape::List(2..=2),
            ArgumentShape::List(2..=2),
            vec![PropertyPosition::Subject(0), PropertyPosition::Subject(1)],
            vec![PropertyPosition::Object(0), PropertyPosition::Object(1)],
            ListFactory,
        )
        .unwrap()
    };
    let mut r = ExtensionRegistry::builder();
    r.register_property(descriptor(IRI))
        .unwrap()
        .register_property(descriptor("urn:test:second"))
        .unwrap();
    let o = QueryOptions {
        extensions: Some(r.build()),
        ..Default::default()
    };
    let cells = "_:head rdf:first 1; rdf:rest _:tail . _:tail rdf:first 2; rdf:rest rdf:nil .";
    let prefix = "PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#>";
    assert_eq!(
        run(
            &s,
            &format!(
                "{prefix} SELECT ?a ?b ?c ?d ?keep {{ {cells} _:head <{IRI}> (?a ?b) . _:head <urn:test:second> (?c ?d) . <urn:unrelated> rdf:first ?keep }}"
            ),
            &o
        ),
        vec![vec![value(1), value(2), value(1), value(2), value(9)]]
    );
    assert_eq!(
        run(
            &s,
            &format!("{prefix} SELECT (COUNT(*) AS ?n) {{ {cells} _:head <{IRI}> _:head }}"),
            &o
        ),
        vec![vec![value(1)]]
    );
}

#[test]
fn unbound_argument_slots_are_charged_before_factory_allocation() {
    let s = store();
    let (mut d, opens, _) = descriptor(20, false);
    d.subject = ArgumentShape::List(127..=127);
    let mut o = options(d);
    o.max_memory_bytes = Some(3000);
    let variables = (0..127)
        .map(|n| format!("?slot{n}"))
        .collect::<Vec<_>>()
        .join(" ");
    let source = format!("SELECT (COUNT(*) AS ?n) {{ ({variables}) <{IRI}> ?out }}");
    assert!(query(s.snapshot(), &source, &o).is_err());
    assert_eq!(opens.load(Ordering::SeqCst), 0);
    o.max_memory_bytes = None;
    assert_eq!(run(&s, &source, &o), vec![vec![value(0)]]);
    assert_eq!(opens.load(Ordering::SeqCst), 1);
}

struct ConversionAggregate(Term);
impl sparkles_core::sparql::extensions::AggregateAccumulator for ConversionAggregate {
    fn add(
        &mut self,
        _: &sparkles_core::sparql::extensions::AggregateContext<'_>,
        _: &Term,
    ) -> Result<(), ScalarError> {
        Ok(())
    }
    fn finish(
        &mut self,
        _: &sparkles_core::sparql::extensions::AggregateContext<'_>,
    ) -> sparkles_core::sparql::extensions::ScalarResult {
        Ok(self.0.clone())
    }
}

fn conversion_options(term: Term) -> QueryOptions {
    use sparkles_core::sparql::extensions::{
        AggregateAccumulator, AggregateContext, AggregateDescriptor, ScalarContext,
        ScalarDescriptor,
    };
    let mut registry = ExtensionRegistry::builder();
    let scalar = term.clone();
    registry
        .register_scalar(
            ScalarDescriptor::new(
                "urn:convert:scalar",
                0..=0,
                move |_: &ScalarContext<'_>, _: &[Term]| Ok(scalar.clone()),
            )
            .unwrap(),
        )
        .unwrap();
    let aggregate = term.clone();
    registry
        .register_aggregate(
            AggregateDescriptor::new(
                "urn:convert:aggregate",
                1,
                move |_: &AggregateContext<'_>| {
                    Ok(Box::new(ConversionAggregate(aggregate.clone()))
                        as Box<dyn AggregateAccumulator>)
                },
            )
            .unwrap(),
        )
        .unwrap();
    registry
        .register_property(
            PropertyDescriptor::new(
                "urn:convert:property",
                ArgumentShape::Term,
                ArgumentShape::Term,
                vec![],
                vec![PropertyPosition::Object(0)],
                Returned(term),
            )
            .unwrap(),
        )
        .unwrap();
    QueryOptions {
        extensions: Some(registry.build()),
        ..Default::default()
    }
}

fn conversion_patterns() -> [&'static str; 3] {
    [
        "BIND(<urn:convert:scalar>() AS ?x)",
        "{ SELECT (AGG <urn:convert:aggregate>(?v) AS ?x) { VALUES ?v {1} } }",
        "<urn:s> <urn:convert:property> ?x",
    ]
}

fn composite(lex: String) -> Term {
    Literal::new_typed_literal(
        lex,
        oxrdf::NamedNode::new(sparkles_core::sparql::cdt::LIST).unwrap(),
    )
    .into()
}

#[test]
fn mapped_composite_output_ceiling_is_fatal_for_all_callback_categories() {
    let o = conversion_options(composite(format!("[{}]", vec!["_:a"; 190_000].join(", "))));
    for pattern in conversion_patterns() {
        let s = store();
        let error = query(s.snapshot(), &format!("SELECT ?x {{ {pattern} }}"), &o)
            .err()
            .unwrap();
        assert!(error.to_string().contains("byte ceiling"), "{error}");
        let mut transaction = s.write();
        let update = format!(
            "INSERT DATA {{ <urn:prior> <urn:p> 1 }}; INSERT {{ <urn:new> <urn:p> ?x }} WHERE {{ {pattern} }}"
        );
        assert!(sparkles_core::sparql::update::update_in(&mut transaction, &update, &o).is_err());
        assert!(transaction.commit().is_err());
        assert!(run(&s, "SELECT * {?s ?p ?o}", &Default::default()).is_empty());
        insert(&s, "INSERT DATA { <urn:recovered> <urn:p> 1 }");
    }
}

#[test]
fn composite_conversion_temporaries_and_nesting_are_bounded() {
    let s = store();
    let mut o = conversion_options(composite(format!("[_:a, {}]", vec!["0"; 2000].join(", "))));
    o.max_memory_bytes = Some(30_000);
    for pattern in conversion_patterns() {
        let error = query(s.snapshot(), &format!("SELECT ?x {{ {pattern} }}"), &o)
            .err()
            .unwrap();
        assert!(error.to_string().contains("memory"), "{error}");
    }
    let deep = format!("{}_:a{}", "[".repeat(129), "]".repeat(129));
    let o = conversion_options(composite(deep));
    for pattern in conversion_patterns() {
        let error = query(s.snapshot(), &format!("SELECT ?x {{ {pattern} }}"), &o)
            .err()
            .unwrap();
        assert!(error.to_string().contains("nesting ceiling"), "{error}");
    }
    // Literal text and IRI contents are opaque to the lexical nesting limit.
    let o = conversion_options(composite(format!("[\"{}\", _:a]", "[".repeat(200))));
    for pattern in conversion_patterns() {
        assert_eq!(run(&s, &format!("SELECT ?x {{ {pattern} }}"), &o).len(), 1);
    }
}

#[test]
fn original_callback_blank_labels_are_retained_once_under_finite_budgets() {
    use sparkles_core::sparql::extensions::{ScalarContext, ScalarDescriptor};
    let mut registry = ExtensionRegistry::builder();
    registry
        .register_scalar(
            ScalarDescriptor::new(
                "urn:large:blank",
                1..=1,
                |_: &ScalarContext<'_>, args: &[Term]| {
                    Ok(oxrdf::BlankNode::new(format!(
                        "a{}{}",
                        "x".repeat(20_000),
                        match &args[0] {
                            Term::Literal(l) => l.value(),
                            _ => unreachable!(),
                        }
                    ))
                    .unwrap()
                    .into())
                },
            )
            .unwrap(),
        )
        .unwrap();
    let o = QueryOptions {
        extensions: Some(registry.build()),
        max_memory_bytes: Some(150_000),
        ..Default::default()
    };
    let s = store();
    let repeated = vec!["1"; 100].join(" ");
    assert_eq!(
        run(
            &s,
            &format!("SELECT ?x {{ VALUES ?n {{{repeated}}} BIND(<urn:large:blank>(?n) AS ?x) }}"),
            &o
        )
        .len(),
        100
    );
    let distinct = (0..20).map(|n| n.to_string()).collect::<Vec<_>>().join(" ");
    let error = query(
        s.snapshot(),
        &format!("SELECT ?x {{ VALUES ?n {{{distinct}}} BIND(<urn:large:blank>(?n) AS ?x) }}"),
        &o,
    )
    .err()
    .unwrap();
    assert!(error.to_string().contains("memory"), "{error}");
}

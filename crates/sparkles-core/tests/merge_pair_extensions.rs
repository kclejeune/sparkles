//! A streamed merge OPTIONAL tests its condition on the pairs before it copies rows
//! (`merge_pair_conditions`) only in a query that references no registered extension.
//! Deciding tests other pairs together, and in another order beside held rows, which a
//! callback could observe. A query that references a registration anywhere therefore
//! gives the same batches with the switch on and off, wherever the reference stands: a
//! scalar under a name in the XSD namespace and an aggregate that a HAVING tests, in the
//! condition's EXISTS or in an EXISTS nested in it, and a property function in another
//! branch of the query. An EXISTS whose pattern holds a property function has no cursor
//! implementation, so its OPTIONAL is evaluated eagerly under the cursor and never
//! reaches the switch, which the test states for those two cases.
use oxrdf::{Literal, Term};
use oxrdfio::RdfFormat;
use sparkles_core::io::Source;
use sparkles_core::sparql::extensions::{
    AggregateAccumulator, AggregateContext, AggregateDescriptor, ArgumentShape, ExtensionRegistry,
    PropertyContext, PropertyDescriptor, PropertyFunction, PropertyInput, PropertyPosition,
    PropertyRow, PropertyStream, ScalarContext, ScalarDescriptor, ScalarError, ScalarResult,
};
use sparkles_core::sparql::{
    CursorOptions, ExecutionMode, Optimizations, QueryExecution, QueryOptions, query_execution,
};
use sparkles_core::store::{Store, StoreOptions};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// A name in the XSD namespace that is not a cast, which a scalar may be registered
/// under.
const SCALAR: &str = "http://www.w3.org/2001/XMLSchema#myCallback";
const PROPERTY: &str = "urn:test:property";
const AGGREGATE: &str = "urn:test:aggregate";

/// The calls of the scalar, the property function and the aggregate.
static CALLS: [AtomicUsize; 3] = [const { AtomicUsize::new(0) }; 3];

/// One row for every input, with the subject as its object.
struct Echo;

struct Once(Option<PropertyRow>);

impl PropertyStream for Once {
    fn next(&mut self, _: &PropertyContext<'_>) -> Result<Option<PropertyRow>, ScalarError> {
        Ok(self.0.take())
    }
}

impl PropertyFunction for Echo {
    fn open<'q>(
        &self,
        _: PropertyContext<'q>,
        input: PropertyInput,
    ) -> Result<Box<dyn PropertyStream + 'q>, ScalarError> {
        CALLS[1].fetch_add(1, Ordering::Relaxed);
        Ok(Box::new(Once(Some(PropertyRow {
            subject: vec![None],
            object: vec![input.subject[0].clone()],
        }))))
    }
}

/// The number of arguments of a group.
struct Count(i64);

impl AggregateAccumulator for Count {
    fn add(&mut self, _: &AggregateContext<'_>, _: &Term) -> Result<(), ScalarError> {
        CALLS[2].fetch_add(1, Ordering::Relaxed);
        self.0 += 1;
        Ok(())
    }

    fn finish(&mut self, _: &AggregateContext<'_>) -> ScalarResult {
        Ok(Literal::from(self.0).into())
    }
}

fn registry() -> Arc<ExtensionRegistry> {
    let mut registry = ExtensionRegistry::builder();
    registry
        .register_scalar(
            ScalarDescriptor::new(
                SCALAR,
                1..=1,
                |_: &ScalarContext<'_>, _: &[Term]| -> ScalarResult {
                    CALLS[0].fetch_add(1, Ordering::Relaxed);
                    Ok(Literal::from(true).into())
                },
            )
            .unwrap(),
        )
        .unwrap()
        .register_property(
            PropertyDescriptor::new(
                PROPERTY,
                ArgumentShape::Term,
                ArgumentShape::Term,
                vec![PropertyPosition::Subject(0)],
                vec![PropertyPosition::Object(0)],
                Echo,
            )
            .unwrap(),
        )
        .unwrap()
        .register_aggregate(
            AggregateDescriptor::new(AGGREGATE, 1, |_: &AggregateContext<'_>| {
                Ok(Box::new(Count(0)) as Box<dyn AggregateAccumulator>)
            })
            .unwrap(),
        )
        .unwrap();
    registry.build()
}

/// `subjects` subjects with `p`, each with one to four `q` objects of which a third
/// have `r`.
fn store(subjects: usize) -> Store {
    let s = Store::in_memory(StoreOptions::default());
    let mut data = String::new();
    for i in 0..subjects {
        data.push_str(&format!(
            "<urn:s:{i}> <urn:p> \"{}\"^^<http://www.w3.org/2001/XMLSchema#integer> .\n",
            i % 97
        ));
        for j in 0..(1 + i % 4) {
            data.push_str(&format!(
                "<urn:s:{i}> <urn:q> <urn:o:{}> .\n",
                (i * 7 + j * 331) % 3000
            ));
        }
    }
    for o in 0..3000 {
        if o % 3 == 0 {
            data.push_str(&format!("<urn:o:{o}> <urn:r> \"kept\" .\n"));
        }
    }
    s.load(&[Source::from_bytes(
        data.into_bytes(),
        RdfFormat::NTriples,
        None,
    )])
    .unwrap();
    s
}

/// The OPTIONAL of the test, with `exists` as its condition.
fn optional(exists: &str) -> String {
    format!("?s <urn:p> ?v OPTIONAL {{ ?s <urn:q> ?o FILTER EXISTS {{ {exists} }} }}")
}

/// The rows of each batch of the query over `pattern`, which holds a merge OPTIONAL
/// with a condition, the bag of the rows, and whether the cursor streamed the merge
/// instead of evaluating the OPTIONAL eagerly.
fn batches(s: &Store, pattern: &str, pairs: bool) -> (Vec<usize>, Vec<String>, bool) {
    let q = format!("SELECT ?s ?v ?o {{ {pattern} }}");
    let opts = QueryOptions {
        extensions: Some(registry()),
        no_cache: true,
        optimizations: Some(Optimizations {
            merge_pair_conditions: pairs,
            ..Default::default()
        }),
        ..Default::default()
    };
    let QueryExecution::Select(mut c) = query_execution(
        s.snapshot(),
        &q,
        &opts,
        &CursorOptions::default(),
        ExecutionMode::Streaming,
    )
    .unwrap() else {
        panic!("{q} did not stream");
    };
    let (mut sizes, mut bag) = (Vec::new(), Vec::new());
    while let Some(batch) = c.next_batch().unwrap() {
        sizes.push(batch.len());
        for row in 0..batch.len() {
            let keys: Vec<_> = (0..batch.width())
                .map(|column| batch.key(row, column).unwrap())
                .collect();
            bag.push(format!("{keys:?}"));
        }
    }
    let plan = c.plan_json().unwrap();
    assert!(
        plan.contains("[merge on ?s]")
            || plan.contains(r#""operator":"MergeJoin","description":"on ?s""#),
        "{q}: {plan}"
    );
    let node = &plan[plan.find(r#""operator":"OptionalJoin""#).expect(&plan)..];
    let streamed = node[node.find(r#""materializes":"#).expect(&plan)..]
        .starts_with(r#""materializes":false"#);
    bag.sort();
    (sizes, bag, streamed)
}

#[test]
fn a_query_that_references_a_registration_gives_the_same_batches_with_the_switch_on_and_off() {
    let s = store(6000);
    // The registry is set and the query references nothing in it, so the merge decides:
    // it fills its batches, where the test after the batch leaves them short.
    let plain = optional("?o <urn:r> ?k");
    // The same OPTIONAL beside a UNION branch. Its predicate is a property function in
    // one query and a predicate that no triple has in the other.
    let beside = |predicate: &str| format!("{{ {plain} }} UNION {{ <urn:s:1> <{predicate}> ?o }}");
    for control in [plain.clone(), beside("urn:test:unregistered")] {
        let (off, on) = (batches(&s, &control, false), batches(&s, &control, true));
        assert_eq!(off.1, on.1, "{control}");
        assert!(off.2 && on.2, "{control}");
        assert!(
            on.0.len() < off.0.len(),
            "{control}: {} and {}",
            on.0.len(),
            off.0.len()
        );
    }
    // The name, the callback, the pattern and whether the cursor streams the merge.
    let cases = [
        (
            "scalar",
            0,
            optional(&format!("?o <urn:r> ?k FILTER(<{SCALAR}>(?k))")),
            true,
        ),
        (
            "aggregate in HAVING",
            2,
            optional(&format!(
                "SELECT ?o {{ ?o <urn:r> ?k }} GROUP BY ?o HAVING(AGG <{AGGREGATE}>(?k) > 0)"
            )),
            true,
        ),
        (
            "nested scalar",
            0,
            optional(&format!(
                "?o <urn:r> ?k FILTER EXISTS {{ ?o <urn:r> ?x FILTER(<{SCALAR}>(?x)) }}"
            )),
            true,
        ),
        (
            "nested aggregate",
            2,
            optional(&format!(
                "?o <urn:r> ?k FILTER EXISTS {{ SELECT ?o {{ ?o <urn:r> ?j }} GROUP BY ?o \
                 HAVING(AGG <{AGGREGATE}>(?j) > 0) }}"
            )),
            true,
        ),
        (
            "property function in a UNION branch",
            1,
            beside(PROPERTY),
            true,
        ),
        // The cursor evaluates these two OPTIONALs eagerly, so the switch is not reached.
        (
            "property function",
            1,
            optional(&format!("?o <urn:r> ?k . ?o <{PROPERTY}> ?x")),
            false,
        ),
        (
            "nested property function",
            1,
            optional(&format!(
                "?o <urn:r> ?k FILTER EXISTS {{ ?o <{PROPERTY}> ?x }}"
            )),
            false,
        ),
    ];
    let mut differing = Vec::new();
    for (name, callback, pattern, streams) in cases {
        // Each setting calls the registration, and does not only name it.
        let calls = || CALLS[callback].load(Ordering::Relaxed);
        let before = calls();
        let off = batches(&s, &pattern, false);
        let between = calls();
        let on = batches(&s, &pattern, true);
        assert!(before < between && between < calls(), "{name}");
        assert_eq!(off.1, on.1, "{name}");
        assert_eq!((off.2, on.2), (streams, streams), "{name}");
        if off.0 != on.0 {
            differing.push(format!(
                "{name}: {} and {} batches",
                off.0.len(),
                on.0.len()
            ));
        }
        // The condition holds for some pairs and fails for others.
        let bound = off.1.iter().filter(|row| !row.ends_with("None]")).count();
        assert!(bound > 0 && bound < off.1.len(), "{name}: {bound}");
    }
    assert!(differing.is_empty(), "{differing:#?}");
}

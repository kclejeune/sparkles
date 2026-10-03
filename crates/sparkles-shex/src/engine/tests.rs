use super::*;
use crate::ir::Tri;
use crate::{Association, NoImports, Schema};
use oxrdf::NamedNode;
use proptest::prelude::*;
use rustc_hash::FxHashSet;
use sparkles_core::io::{RdfFormat, Source};
use sparkles_core::store::{Store, StoreOptions};
use std::sync::atomic::AtomicBool;
use std::time::Duration;

const EX: &str = "http://ex.org/";
const PREFIXES: &str = "PREFIX ex: <http://ex.org/> PREFIX foaf: <http://xmlns.com/foaf/0.1/> \
                        PREFIX xsd: <http://www.w3.org/2001/XMLSchema#>\n";

/// The acceptance example's schema.
const SCHEMA: &str = "start = @ex:Person
ex:Person EXTRA a { a [ex:Person] ; foaf:name xsd:string ;
  foaf:age xsd:integer MAXINCLUSIVE 150 ? ; foaf:knows @ex:Person * }
ex:Org CLOSED { a [ex:Org] ; foaf:name . ; ex:city [\"Paris\" \"Kyoto\"] }";

const DATA: &str = "ex:alice a ex:Person ; foaf:name \"Alice\" ; foaf:age 30 ; foaf:knows ex:bob .
ex:bob a ex:Person ; foaf:name \"Bob\" ; foaf:knows ex:alice .
ex:carol a ex:Person ; foaf:age 200 .
ex:acme a ex:Org ; foaf:name \"ACME\" ; ex:city \"Paris\" ; ex:mayor ex:bob .";

fn store(turtle: &str) -> Store {
    let s = Store::in_memory(StoreOptions::default());
    let text =
        format!("@prefix ex: <{EX}> . @prefix foaf: <http://xmlns.com/foaf/0.1/> .\n{turtle}");
    s.load(&[Source::from_bytes(
        text.into_bytes(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
    s
}

fn schema(text: &str) -> CompiledSchema {
    let s = Schema::parse_shexc(&format!("{PREFIXES}{text}"), None).unwrap();
    crate::compile(&s, &NoImports).unwrap()
}

fn ex(local: &str) -> Term {
    NamedNode::new_unchecked(format!("{EX}{local}")).into()
}

fn label(local: &str) -> ShapeLabel {
    ShapeLabel::Iri(format!("{EX}{local}"))
}

fn map(pairs: &[(&str, &str)]) -> ShapeMap {
    ShapeMap(
        pairs
            .iter()
            .map(|(n, s)| Association {
                node: NodeSelector::Term(ex(n)),
                shape: label(s),
            })
            .collect(),
    )
}

fn statuses(r: &ResultMap) -> Vec<bool> {
    r.results
        .iter()
        .map(|x| x.status == Status::Conformant)
        .collect()
}

#[test]
fn acceptance_example() {
    let store = store(DATA);
    let schema = schema(SCHEMA);
    let m = map(&[
        ("alice", "Person"),
        ("bob", "Person"),
        ("carol", "Person"),
        ("acme", "Org"),
    ]);
    let r = validate(&store.snapshot(), &schema, &m, &Default::default()).unwrap();
    assert_eq!(statuses(&r), vec![true, true, false, false]);
    assert_eq!((r.conforms, r.conformant, r.nonconformant), (false, 2, 2));
    let carol = &r.results[2];
    assert!(carol.failures.contains(&ShexFailure::Cardinality {
        predicate: "http://xmlns.com/foaf/0.1/name".into(),
        inverse: false,
        min: 1,
        max: Some(1),
        count: 0,
    }));
    assert!(carol.failures.iter().any(|f| matches!(f,
        ShexFailure::Facet { constraint, value } if constraint == "MAXINCLUSIVE 150"
            && *value == Term::from(oxrdf::Literal::from(200)))));
    assert_eq!(
        carol.reason.as_deref(),
        Some("ex:carol: 0 foaf:name arcs, {1,1} needed")
    );
    let acme = &r.results[3];
    assert!(matches!(&acme.failures[0],
        ShexFailure::Closed { predicate, value }
            if predicate == "http://ex.org/mayor" && *value == ex("bob")));
    assert_eq!(
        acme.reason.as_deref(),
        Some("ex:acme: CLOSED shape forbids ex:mayor ex:bob")
    );
    // START, a node that is not in the store, and only the nonconformant results
    let m = ShapeMap(vec![
        Association {
            node: NodeSelector::Term(ex("alice")),
            shape: ShapeLabel::Start,
        },
        Association {
            node: NodeSelector::Term(ex("nobody")),
            shape: label("Person"),
        },
    ]);
    let opts = ValidateOptions {
        only_nonconformant: true,
        ..Default::default()
    };
    let r = validate(&store.snapshot(), &schema, &m, &opts).unwrap();
    assert_eq!((r.conformant, r.nonconformant, r.results.len()), (1, 1, 1));
    assert_eq!(r.results[0].node, ex("nobody"));
    // an undefined label
    let err = validate(
        &store.snapshot(),
        &schema,
        &map(&[("alice", "Nope")]),
        &opts,
    );
    assert!(err.unwrap_err().downcast::<crate::SchemaError>().is_ok());
}

#[test]
fn recursion_is_a_greatest_fixed_point() {
    let store = store(DATA);
    let schema = schema(SCHEMA);
    let m = map(&[("alice", "Person"), ("bob", "Person")]);
    let r = validate(&store.snapshot(), &schema, &m, &Default::default()).unwrap();
    assert_eq!(statuses(&r), vec![true, true]);
    let more = format!("<{EX}bob> <http://xmlns.com/foaf/0.1/age> \"old\" .");
    store
        .load(&[Source::from_bytes(
            more.into_bytes(),
            RdfFormat::NTriples,
            None,
        )])
        .unwrap();
    let r = validate(&store.snapshot(), &schema, &m, &Default::default()).unwrap();
    assert_eq!(statuses(&r), vec![false, false]);
    assert_eq!(
        r.results[0].failures,
        vec![ShexFailure::Reference {
            shape: "ex:Person".into(),
            value: ex("bob"),
        }]
    );
    assert!(matches!(
        r.results[1].failures[0],
        ShexFailure::Datatype { .. }
    ));
    // one node against one shape
    let one = validate_node(
        &store.snapshot(),
        &schema,
        &ex("alice"),
        &label("Person"),
        &Default::default(),
    )
    .unwrap();
    assert_eq!(one.status, Status::Nonconformant);
}

#[test]
fn inverse_cardinality_and_extra() {
    let store = store(
        "ex:a ex:parentOf ex:kid . ex:b ex:parentOf ex:kid . ex:c ex:parentOf ex:kid .
         ex:a a ex:Person . ex:b a ex:Person . ex:c a ex:Person .
         ex:t foaf:knows ex:a , ex:x .",
    );
    let schema = schema(
        "ex:Person { a [ex:Person] }
         ex:Child { ^ex:parentOf @ex:Person {1,2} }
         ex:T EXTRA foaf:knows { foaf:knows @ex:Person }
         ex:U { foaf:knows @ex:Person }",
    );
    let m = map(&[("kid", "Child"), ("t", "T"), ("t", "U")]);
    let r = validate(&store.snapshot(), &schema, &m, &Default::default()).unwrap();
    assert_eq!(statuses(&r), vec![false, true, false]);
    assert_eq!(
        r.results[0].failures,
        vec![ShexFailure::Cardinality {
            predicate: format!("{EX}parentOf"),
            inverse: true,
            min: 1,
            max: Some(2),
            count: 3,
        }]
    );
    assert!(r.results[2].failures.contains(&ShexFailure::Reference {
        shape: "ex:Person".into(),
        value: ex("x"),
    }));
}

/// `ex:n0 foaf:knows ex:n1 … ex:n{len-1}`, every node with a name except the last when
/// `broken`.
fn chain(len: usize, broken: bool) -> Store {
    let mut nt = String::with_capacity(len * 120);
    let knows = "<http://xmlns.com/foaf/0.1/knows>";
    let name = "<http://xmlns.com/foaf/0.1/name>";
    for i in 0..len {
        if i + 1 < len {
            nt.push_str(&format!("<{EX}n{i}> {knows} <{EX}n{}> .\n", i + 1));
        }
        if !(broken && i + 1 == len) {
            nt.push_str(&format!("<{EX}n{i}> {name} \"n\" .\n"));
        }
    }
    let s = Store::in_memory(StoreOptions::default());
    s.load(&[Source::from_bytes(
        nt.into_bytes(),
        RdfFormat::NTriples,
        None,
    )])
    .unwrap();
    s
}

const CHAIN: &str = "ex:P { foaf:name . ; foaf:knows @ex:P * }";

#[test]
fn long_reference_chains_need_no_stack() {
    let store = chain(200_000, false);
    let schema = schema(CHAIN);
    let h = std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(move || {
            let opts = ValidateOptions {
                parallel: false,
                ..Default::default()
            };
            validate(&store.snapshot(), &schema, &map(&[("n0", "P")]), &opts).unwrap()
        })
        .unwrap();
    let r = h.join().unwrap();
    assert!(r.conforms);
}

#[test]
fn failures_propagate_back_along_a_chain() {
    let store = chain(2_000, true);
    let schema = schema(CHAIN);
    let m = map(&[("n0", "P"), ("n1998", "P")]);
    let r = validate(&store.snapshot(), &schema, &m, &Default::default()).unwrap();
    assert_eq!(statuses(&r), vec![false, false]);
}

#[test]
fn sequential_and_parallel_agree() {
    // 3000 people knowing pseudo-random others; some too old
    let mut ttl = String::new();
    let mut x: u64 = 7;
    let mut next = |n: u64| {
        x = x
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (x >> 33) % n
    };
    for i in 0..3000 {
        let age = if next(50) == 0 { 200 } else { 30 };
        ttl.push_str(&format!("ex:p{i} foaf:name \"p\" ; foaf:age {age}"));
        for _ in 0..next(4) {
            ttl.push_str(&format!(" ; foaf:knows ex:p{}", next(3000)));
        }
        ttl.push_str(" .\n");
    }
    let store = store(&ttl);
    let schema =
        schema("ex:P { foaf:name . ; foaf:age xsd:integer MAXINCLUSIVE 150 ; foaf:knows @ex:P * }");
    let names: Vec<String> = (0..3000).map(|i| format!("p{i}")).collect();
    let pairs: Vec<(&str, &str)> = names.iter().map(|n| (n.as_str(), "P")).collect();
    let m = map(&pairs);
    let run = |parallel: bool| {
        let opts = ValidateOptions {
            parallel,
            ..Default::default()
        };
        validate(&store.snapshot(), &schema, &m, &opts).unwrap()
    };
    let (a, b) = (run(false), run(true));
    assert_eq!(a.results, b.results);
    assert!(a.conformant > 0 && a.nonconformant > 0);
}

#[test]
fn limits_are_typed_errors() {
    let store = chain(2_000, false);
    let schema = schema(CHAIN);
    let m = map(&[("n0", "P")]);
    let err = |opts: ValidateOptions| {
        validate(&store.snapshot(), &schema, &m, &opts)
            .unwrap_err()
            .downcast::<sparkles_core::Error>()
            .unwrap()
    };
    match err(ValidateOptions {
        max_pairs: Some(100),
        ..Default::default()
    }) {
        sparkles_core::Error::BudgetExceeded(b) => {
            assert_eq!(b.kind, sparkles_core::BudgetKind::ValidationWork)
        }
        e => panic!("{e:?}"),
    }
    let e = err(ValidateOptions {
        cancel: Some(Arc::new(AtomicBool::new(true))),
        ..Default::default()
    });
    assert!(matches!(e, sparkles_core::Error::Cancelled), "{e:?}");
    let e = err(ValidateOptions {
        timeout: Some(Duration::ZERO),
        ..Default::default()
    });
    assert!(matches!(e, sparkles_core::Error::Timeout), "{e:?}");
    // the report limit
    let r = validate(
        &store.snapshot(),
        &schema,
        &map(&[("n0", "P"), ("n1", "P")]),
        &ValidateOptions {
            max_results: Some(1),
            ..Default::default()
        },
    );
    assert!(r.unwrap_err().downcast::<TooManyResults>().is_ok());
}

// ------------------------------------------------ against a naive fixed point ------

/// Shapes over `ex:p` and `ex:q` arcs between nodes, with recursion, negation and
/// EXTRA over a lower stratum.
const NAIVE: &str = "ex:A { ex:p @ex:A * ; ex:q [ex:n0 ex:n1 ex:n2] ? }
ex:B { ex:p @ex:B {0,2} } AND NOT @ex:A
ex:C { ex:p @ex:A + } OR { ex:q @ex:C }
ex:D EXTRA ex:p { ex:p @ex:B ; ex:q . * }
ex:E { ex:q { ex:p @ex:E ? } * }";

/// The typing of every (node, kind) pair by the definition: per stratum, start from
/// all true and remove failing pairs until nothing changes.
fn naive(store: &Store, schema: &CompiledSchema, nodes: &[Id]) -> FxHashMap<(Id, PairKind), bool> {
    let snap = store.snapshot();
    let data = DataGraph::new(snap.clone(), None, &[], &[]).unwrap();
    let ir = schema.ir();
    let env = Env::new(
        ir,
        &snap,
        &data,
        schema.prefixes(),
        Vec::new(),
        &Default::default(),
    );
    let table = std::cell::RefCell::new(FxHashMap::default());
    let mut w = Worker::new(&env.registry, &snap);
    for s in 0..ir.strata.max(1) {
        let kinds: Vec<PairKind> = (0..ir.pairs.len() as u32)
            .map(PairKind)
            .filter(|k| ir.pairs[k.index()].stratum == s)
            .collect();
        for &n in nodes {
            for &k in &kinds {
                table.borrow_mut().insert((n, k), true);
            }
        }
        loop {
            let mut changed = false;
            for &n in nodes {
                for &k in &kinds {
                    if !table.borrow()[&(n, k)] {
                        continue;
                    }
                    let read = |n: Id, k: PairKind| Tri::from(table.borrow()[&(n, k)]);
                    let r = env.eval(ir.pairs[k.index()].se, n, &read, &mut w).unwrap();
                    if r == Tri::False {
                        table.borrow_mut().insert((n, k), false);
                        changed = true;
                    }
                }
            }
            if !changed {
                break;
            }
        }
    }
    table.into_inner()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn agrees_with_a_naive_fixed_point(
        edges in prop::collection::vec((0..30u32, 0..2u32, 0..30u32), 0..80),
        focus in prop::collection::vec((0..30u32, 0..5usize), 1..12),
    ) {
        let mut ttl = String::new();
        for i in 0..30 {
            ttl.push_str(&format!("ex:n{i} ex:r ex:n{i} .\n"));
        }
        for (s, p, o) in &edges {
            ttl.push_str(&format!("ex:n{s} ex:{} ex:n{o} .\n", ["p", "q"][*p as usize]));
        }
        let store = store(&ttl);
        let schema = schema(NAIVE);
        let snap = store.snapshot();
        let nodes: Vec<Id> = (0..30)
            .map(|i| snap.lookup_iri(&format!("{EX}n{i}")).unwrap())
            .collect();
        let want = naive(&store, &schema, &nodes);
        let shapes = ["A", "B", "C", "D", "E"];
        let names: Vec<(String, &str)> =
            focus.iter().map(|(n, s)| (format!("n{n}"), shapes[*s])).collect();
        let pairs: Vec<(&str, &str)> = names.iter().map(|(n, s)| (n.as_str(), *s)).collect();
        let r = validate(&snap, &schema, &map(&pairs), &Default::default()).unwrap();
        let mut seen = FxHashSet::default();
        let mut i = 0;
        for (n, s) in &focus {
            if !seen.insert((*n, *s)) {
                continue;
            }
            let kind = schema.label(&format!("{EX}{}", shapes[*s])).unwrap();
            let expected = want[&(nodes[*n as usize], kind)];
            prop_assert_eq!(r.results[i].status == Status::Conformant, expected,
                "n{} @ {}", n, shapes[*s]);
            i += 1;
        }
    }
}

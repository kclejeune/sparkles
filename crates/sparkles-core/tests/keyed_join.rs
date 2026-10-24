//! Two parts of a group that share no variable and are linked only by a FILTER
//! equality `?a = ?b` must be joined by that equality, not by their product, and the
//! answers must be the filtered product's. BSBM BI q2 counts the features a product
//! shares with each other product this way.
use oxrdf::{NamedNode, Term};
use oxrdfio::RdfFormat;
use sparkles_core::io::Source;
use sparkles_core::sparql::extensions::{
    ExtensionRegistry, ScalarContext, ScalarDescriptor, ScalarResult,
};
use sparkles_core::sparql::{
    CursorOptions, FallbackPolicy, Optimizations, PlanInfo, QueryOptions, query, select_cursor,
};
use sparkles_core::store::{Store, StoreOptions};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

const PRODUCTS: usize = 60;
const FEATURES: usize = 40;

fn load(store: &Store, data: String) {
    store
        .load(&[Source::from_bytes(
            data.into_bytes(),
            RdfFormat::Turtle,
            None,
        )])
        .unwrap();
}

/// The features of product `p`: a few of a shared pool, so products overlap.
fn features(p: usize) -> Vec<usize> {
    (0..FEATURES)
        .filter(|f| (p * 7 + f * 3) % 5 == 0 || (p + f) % 11 == 0)
        .collect()
}

fn products() -> Store {
    let store = Store::in_memory(StoreOptions {
        result_cache_min_ms: 0.0,
        ..Default::default()
    });
    let mut data = String::from(
        "@prefix bsbm: <http://www4.wiwiss.fu-berlin.de/bizer/bsbm/v01/vocabulary/> .\n\
         @prefix ex: <http://example.org/> .\n",
    );
    for p in 0..PRODUCTS {
        data += &format!("ex:p{p} a bsbm:Product .\n");
        for f in features(p) {
            data += &format!("ex:p{p} bsbm:productFeature ex:f{f} .\n");
        }
    }
    load(&store, data);
    store
}

/// BSBM BI q2's text for product `p`.
fn q2(p: usize) -> String {
    format!(
        "prefix bsbm: <http://www4.wiwiss.fu-berlin.de/bizer/bsbm/v01/vocabulary/>
         SELECT ?otherProduct ?sameFeatures {{
           ?otherProduct a bsbm:Product .
           FILTER(?otherProduct != <http://example.org/p{p}>)
           {{ SELECT ?otherProduct (count(?otherFeature) As ?sameFeatures) {{
               <http://example.org/p{p}> bsbm:productFeature ?feature .
               ?otherProduct bsbm:productFeature ?otherFeature .
               FILTER(?feature=?otherFeature)
             }} Group By ?otherProduct }}
         }} Order By desc(?sameFeatures) ?otherProduct Limit 10"
    )
}

/// The ten products that share most features with `p`, from the fixture's description.
fn q2_oracle(p: usize) -> Vec<(String, usize)> {
    let mine = features(p);
    let mut rows: Vec<(String, usize)> = (0..PRODUCTS)
        .filter(|&o| o != p)
        .map(|o| {
            let shared = features(o).iter().filter(|f| mine.contains(f)).count();
            (format!("http://example.org/p{o}"), shared)
        })
        .filter(|(_, n)| *n > 0)
        .collect();
    rows.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    rows.truncate(10);
    rows
}

fn options(max_rows: usize, keyed: bool) -> QueryOptions {
    QueryOptions {
        max_rows: Some(max_rows),
        no_cache: true,
        optimizations: Some(Optimizations {
            keyed_join: keyed,
            ..Optimizations::ALL
        }),
        ..Default::default()
    }
}

fn operators(p: &PlanInfo, name: &str) -> Vec<String> {
    let mut todo = vec![p];
    let mut found = Vec::new();
    while let Some(p) = todo.pop() {
        if p.operator == name {
            found.push(p.description.clone());
        }
        todo.extend(&p.children);
    }
    found
}

fn bag(rows: Vec<Vec<Option<Term>>>) -> Vec<String> {
    let mut result = rows
        .into_iter()
        .map(|r| format!("{r:?}"))
        .collect::<Vec<_>>();
    result.sort();
    result
}

/// The rows of a cursor that may not materialize any operator.
fn strict_rows(store: &Store, text: &str, options: &QueryOptions) -> Vec<String> {
    let mut cursor = select_cursor(
        store.snapshot(),
        text,
        options,
        &CursorOptions {
            batch_rows: 7,
            fallback: FallbackPolicy::RejectMaterialization,
            ..Default::default()
        },
    )
    .unwrap();
    let mut rows = Vec::new();
    while let Some(batch) = cursor.next_batch().unwrap() {
        for i in 0..batch.len() {
            rows.push(batch.row(i).unwrap());
        }
    }
    bag(rows)
}

#[test]
fn bsbm_bi_q2_matches_an_oracle_through_a_keyed_join() {
    let store = products();
    for p in [0, 7, 33] {
        let expected = q2_oracle(p);
        assert!(!expected.is_empty());
        for keyed in [true, false] {
            let r = query(store.snapshot(), &q2(p), &options(1_000_000, keyed)).unwrap();
            let got: Vec<(String, usize)> = r
                .rows()
                .into_iter()
                .map(|row| {
                    let Some(Term::NamedNode(n)) = &row[0] else {
                        panic!("{row:?}")
                    };
                    let Some(Term::Literal(c)) = &row[1] else {
                        panic!("{row:?}")
                    };
                    (n.as_str().to_string(), c.value().parse().unwrap())
                })
                .collect();
            assert_eq!(got, expected, "product {p}, keyed {keyed}");
            let keyed_ops = operators(&r.plan, "KeyedJoin");
            assert_eq!(keyed_ops.len(), usize::from(keyed), "{keyed_ops:?}");
            assert_eq!(
                operators(&r.plan, "CartesianProduct").len(),
                usize::from(!keyed)
            );
        }
    }
}

#[test]
fn keyed_join_avoids_a_product_over_the_row_limit() {
    let store = products();
    // The product of a product's features and all feature triples exceeds this limit,
    // while the candidate pairs do not.
    let text = "prefix bsbm: <http://www4.wiwiss.fu-berlin.de/bizer/bsbm/v01/vocabulary/>
        SELECT ?other WHERE { <http://example.org/p3> bsbm:productFeature ?f .
          ?other bsbm:productFeature ?g FILTER(?f = ?g) }";
    let limit = 2000;
    let r = query(store.snapshot(), text, &options(limit, true)).unwrap();
    assert!(!r.rows().is_empty());
    assert!(query(store.snapshot(), text, &options(limit, false)).is_err());
}

/// Values of ?x and ?y that mix IRIs, blank nodes, numbers equal by value with
/// different ids, dates in different time zones, booleans, strings, language strings
/// whose tags differ in case, literals of an unknown datatype, and unbound values.
fn mixed() -> Store {
    let store = Store::in_memory(StoreOptions::default());
    let data = r#"
        @prefix ex: <http://example.org/> .
        @prefix xsd: <http://www.w3.org/2001/XMLSchema#> .
        ex:l1 ex:v ex:a .
        ex:l2 ex:v _:b .
        ex:l3 ex:v "1"^^xsd:integer .
        ex:l4 ex:v "01"^^xsd:integer .
        ex:l5 ex:v "1.0"^^xsd:decimal .
        ex:l6 ex:v "a" .
        ex:l7 ex:v "a"@en .
        ex:l8 ex:v "x"^^ex:unknown .
        ex:l9 ex:v "2001-01-01T10:00:00Z"^^xsd:dateTime .
        ex:l10 ex:v "1"^^xsd:boolean .
        ex:l11 ex:v "a"^^xsd:string .
        ex:l12 ex:v "chat"@en-GB .
        ex:l13 ex:v "1"^^xsd:int .
        ex:l14 ex:other ex:a .
        ex:r1 ex:w ex:a .
        ex:r2 ex:w _:b .
        ex:r3 ex:w "1"^^xsd:integer .
        ex:r4 ex:w "1"^^xsd:double .
        ex:r5 ex:w "a" .
        ex:r6 ex:w "a"@EN .
        ex:r7 ex:w "x"^^ex:unknown .
        ex:r8 ex:w "y"^^ex:unknown .
        ex:r9 ex:w ex:a .
        ex:r10 ex:w "2001-01-01T11:00:00+01:00"^^xsd:dateTime .
        ex:r11 ex:w true .
        ex:r12 ex:w "chat"@en-gb .
        ex:r13 ex:w "1e0"^^xsd:float .
    "#;
    load(&store, data.to_string());
    store
}

#[test]
fn keyed_join_keeps_value_equality_errors_and_unbound_values() {
    let store = mixed();
    for (pattern, keyed_expected) in [
        ("?l ex:v ?x . ?r ex:w ?y FILTER(?x = ?y)", true),
        ("?l ex:v ?x . ?r ex:w ?y FILTER(?y = ?x)", true),
        (
            "?l ex:v ?x . ?r ex:w ?y FILTER(?x = ?y && !sameTerm(?l, ex:l3))",
            true,
        ),
        (
            "?l ex:v ?x . ?r ex:w ?y FILTER(?x = ?y) FILTER(?r != ex:r9)",
            true,
        ),
        // ?x is unbound on the rows without ex:v
        (
            "?l ?p ?any OPTIONAL { ?l ex:v ?x } ?r ex:w ?y FILTER(?x = ?y)",
            true,
        ),
        (
            "?l ex:v ?x . ?r ex:w ?y FILTER(?x = ?y || ?r = ex:r8)",
            false,
        ),
    ] {
        let text = format!("PREFIX ex: <http://example.org/> SELECT ?l ?r WHERE {{ {pattern} }}");
        let run = |keyed| {
            let r = query(store.snapshot(), &text, &options(1_000_000, keyed)).unwrap();
            (bag(r.rows()), operators(&r.plan, "KeyedJoin").len())
        };
        let (on, used) = run(true);
        let (off, unused) = run(false);
        assert!(!off.is_empty(), "{pattern}");
        assert_eq!(on, off, "{pattern}");
        assert_eq!(unused, 0, "{pattern}");
        assert_eq!(used, usize::from(keyed_expected), "{pattern}");
        assert_eq!(
            strict_rows(&store, &text, &options(1_000_000, true)),
            off,
            "cursor: {pattern}"
        );
    }
}

#[test]
fn keyed_join_answers_ask_in_a_cursor_and_eagerly() {
    let store = mixed();
    for (condition, expected) in [("?x = ?y", true), ("?x = ?y && ?x = ex:zzz", false)] {
        let text = format!(
            "PREFIX ex: <http://example.org/> ASK {{ ?l ex:v ?x . ?r ex:w ?y FILTER({condition}) }}"
        );
        for keyed in [true, false] {
            let r = query(store.snapshot(), &text, &options(1_000_000, keyed)).unwrap();
            assert_eq!(r.boolean, expected, "{condition}, keyed {keyed}");
        }
    }
}

/// An application callback in the FILTER sees every pair the product tests, so its
/// calls and their order decide the answer as before.
#[test]
fn application_callbacks_in_the_filter_keep_the_product() {
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let tick = move |_: &ScalarContext<'_>, _: &[Term]| -> ScalarResult {
        count.fetch_add(1, Ordering::SeqCst);
        Ok(oxrdf::Literal::from(true).into())
    };
    let mut registry = ExtensionRegistry::builder();
    registry
        .register_scalar(ScalarDescriptor::new("urn:test:tick", 0..=0, tick).unwrap())
        .unwrap();
    let options = QueryOptions {
        extensions: Some(registry.build()),
        no_cache: true,
        ..Default::default()
    };
    let store = Store::in_memory(StoreOptions::default());
    let text = "SELECT ?y WHERE { VALUES ?x { <urn:b> } VALUES ?y { <urn:a> <urn:b> } \
                FILTER(<urn:test:tick>() && ?x = ?y) }";
    let r = query(store.snapshot(), text, &options).unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(
        bag(r.rows()),
        bag(vec![vec![Some(Term::NamedNode(
            NamedNode::new("urn:b").unwrap()
        ))]])
    );
    assert!(operators(&r.plan, "KeyedJoin").is_empty());
}

/// SP2Bench q12a's shape: two large parts linked by an equality of names. A cursor
/// reads both alternately and stops at the first matching pair. The answers agree
/// with the product's, whether a pair exists or not.
#[test]
fn ask_over_a_large_keyed_join_reads_both_sides_alternately() {
    let store = Store::in_memory(StoreOptions::default());
    let mut data = String::from("@prefix ex: <http://example.org/> .\n");
    for i in 0..3000 {
        data += &format!("ex:a{i} ex:creator ex:p{i} . ex:p{i} ex:name \"Person {i}\" .\n");
        // the inproceedings' authors are other people, except one shared name
        let j = i + 5000;
        data += &format!("ex:b{i} ex:author ex:q{j} . ex:q{j} ex:label \"Person {j}\" .\n");
    }
    data += "ex:q9 ex:label \"Person 2999\" . ex:bq ex:author ex:q9 .\n";
    load(&store, data);
    for (extra, expected) in [("", true), ("&& ?n1 != \"Person 2999\"", false)] {
        let text = format!(
            "PREFIX ex: <http://example.org/> ASK {{ ?a ex:creator ?p1 . ?p1 ex:name ?n1 . \
             ?b ex:author ?p2 . ?p2 ex:label ?n2 FILTER(?n1 = ?n2 {extra}) }}"
        );
        for keyed in [true, false] {
            let r = query(store.snapshot(), &text, &options(100_000_000, keyed)).unwrap();
            assert_eq!(r.boolean, expected, "{extra}, keyed {keyed}");
        }
        let r = query(store.snapshot(), &text, &options(100_000_000, true)).unwrap();
        let keyed_ops = operators(&r.plan, "KeyedJoin");
        assert_eq!(keyed_ops.len(), 1, "{extra}");
    }
}

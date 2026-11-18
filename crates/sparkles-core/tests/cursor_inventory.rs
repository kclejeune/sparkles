//! The inventory of plan forms under the strict cursor policy (item 829). Every form
//! whose kernel answers whole tables either runs as a native cursor operator, with no
//! node that materializes, or is refused with the materialization error at exactly the
//! operator the refused list names. Each later reader moves its rows from the refused
//! list to the native one.
//!
//! Registered property functions without a declared prefix output are refused by
//! decision, and `tests/query_properties.rs` (`a_declared_prefix_output_streams_its_input`)
//! covers them, since they need a registration of their own.

use oxrdf::Term;
use oxrdfio::RdfFormat;
use sparkles_core::io::Source;
use sparkles_core::sparql::{
    CursorOptions, CursorPlan, FallbackPolicy, LocalEndpoints, LocalTarget, Optimizations,
    QueryOptions, query, select_cursor,
};
use sparkles_core::store::{Snapshot, Store, StoreOptions};
use sparkles_core::{Error, Result};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// Why the strict policy refuses a form.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Why {
    /// A decision of the X14 plan keeps it refused.
    Decision(&'static str),
    /// A roadmap row holds the change that would give it a reader.
    Row(u32),
    /// A group of item 829 part 2 gives it a reader. Row 829 is not complete while any
    /// form is pending.
    Pending(&'static str),
}

/// What the strict policy does with a form.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Expect {
    Native,
    Refused(Why),
}

use Expect::{Native, Refused};

/// One plan form: a name, the operator the plan names for it, and the query.
struct Form {
    name: &'static str,
    operator: &'static str,
    query: String,
    expect: Expect,
    /// Text that some operator description of the drained plan holds, such as the note
    /// of a branch the form must take.
    note: Option<&'static str>,
}

fn form(name: &'static str, operator: &'static str, query: &str, expect: Expect) -> Form {
    Form {
        name,
        operator,
        query: query.to_string(),
        expect,
        note: None,
    }
}

impl Form {
    fn noting(mut self, note: &'static str) -> Self {
        self.note = Some(note);
        self
    }
}

/// The operators that may run their whole subtree eagerly, which the strict policy
/// refuses. Each later group removes its kind, and the list ends empty.
const EAGER_SUBTREES: &[&str] = &["SpatialKnn", "VectorSearch"];

/// The forms whose plan runs a subtree eagerly, which name one of [`EAGER_SUBTREES`].
const SUBTREE_FORMS: &[&str] = &[
    "vector ORDER BY answered by the index",
    "vector ORDER BY with its generic fallback",
    "nearest neighbours",
];

const P: &str = "PREFIX path: <urn:x-sparkles:path#> PREFIX algo: <urn:x-sparkles:algo#> \
    PREFIX hist: <urn:x-sparkles:history#> PREFIX apf: <http://jena.apache.org/ARQ/property#> \
    PREFIX ex: <http://example.org/> PREFIX spk: <urn:x-sparkles:> \
    PREFIX text: <http://jena.apache.org/text#> PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#> \
    PREFIX geo: <http://www.opengis.net/ont/geosparql#> \
    PREFIX geof: <http://www.opengis.net/def/function/geosparql/> \
    PREFIX spatial: <http://jena.apache.org/spatial#> ";

fn load(s: &Store, data: &str, format: RdfFormat) {
    s.load(&[Source::from_bytes(data.as_bytes().to_vec(), format, None)])
        .unwrap();
}

/// A graph of 40 numbered subjects with `<urn:p>` values and a ring of `<urn:q>` edges,
/// and a few vectors.
fn base() -> Store {
    let s = Store::in_memory(StoreOptions::default());
    let n = 40;
    let mut data = String::new();
    for i in 0..n {
        data.push_str(&format!("<urn:s:{i}> <urn:p> {i} .\n"));
        data.push_str(&format!(
            "<urn:s:{i}> <urn:q> <urn:s:{}> .\n",
            (i * 7 + 1) % n
        ));
        data.push_str(&format!("<urn:s:{i}> <urn:q> <urn:s:{}> .\n", (i + 1) % n));
    }
    for (i, v) in ["[1, 0, 0]", "[0.8, 0.6, 0]", "[0, 1, 0]", "[-1, 0, 0]"]
        .iter()
        .enumerate()
    {
        data.push_str(&format!(
            "<http://example.org/v{i}> <http://example.org/emb> \"{v}\"^^<urn:x-sparkles:vector> .\n"
        ));
    }
    load(&s, &data, RdfFormat::Turtle);
    s
}

/// The in-process endpoint `http://local/sparql`, which reads `snap`.
struct Local(Arc<Snapshot>);

impl std::fmt::Debug for Local {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Local")
    }
}

impl LocalEndpoints for Local {
    fn resolve(&self, iri: &str) -> Option<Result<LocalTarget>> {
        (iri == "http://local/sparql").then(|| {
            Ok(LocalTarget {
                snap: self.0.clone(),
                options: QueryOptions {
                    no_cache: true,
                    ..Default::default()
                },
                hold: None,
            })
        })
    }
}

/// A view source with one view whose column ?x holds the integers below 5.
#[derive(Debug)]
struct Integers;

impl sparkles_core::sparql::viewsvc::ViewSource for Integers {
    fn read_view(
        &self,
        _: &sparkles_core::sparql::viewsvc::ViewRead<'_>,
    ) -> Result<sparkles_core::sparql::viewsvc::ViewRows> {
        Ok(sparkles_core::sparql::viewsvc::ViewRows {
            columns: vec!["x".into()],
            rows: (0..5)
                .map(|i| (vec![Some(Term::Literal(oxrdf::Literal::from(i)))], 1))
                .collect(),
            commit: 0,
        })
    }
}

fn reasons(plan: &CursorPlan, subtree: &mut Vec<String>, materializes: &mut Vec<String>) {
    if plan
        .reason
        .as_deref()
        .is_some_and(|r| r.contains("runs its whole subtree eagerly"))
    {
        subtree.push(plan.operator.operator.clone());
    }
    if plan.materializes {
        materializes.push(plan.operator.operator.clone());
    }
    for child in &plan.children {
        reasons(child, subtree, materializes);
    }
}

fn names(plan: &CursorPlan, out: &mut Vec<String>) {
    out.push(plan.operator.operator.clone());
    for child in &plan.children {
        names(child, out);
    }
}

fn notes(info: &sparkles_core::sparql::PlanInfo, out: &mut Vec<String>) {
    out.push(info.description.clone());
    for child in &info.children {
        notes(child, out);
    }
}

fn descriptions(plan: &CursorPlan, out: &mut Vec<String>) {
    notes(&plan.operator, out);
    for child in &plan.children {
        descriptions(child, out);
    }
}

fn bag(rows: Vec<Vec<Option<Term>>>) -> Vec<String> {
    let mut rows: Vec<String> = rows.into_iter().map(|r| format!("{r:?}")).collect();
    rows.sort();
    rows
}

/// The answer of a cursor under `policy` with batches of `rows`, its cursor plan once
/// drained, and the work it counted.
fn drain(
    snap: &Arc<Snapshot>,
    q: &str,
    opts: &QueryOptions,
    policy: FallbackPolicy,
    rows: usize,
) -> Result<(Vec<String>, CursorPlan, u64)> {
    let work = Arc::new(AtomicU64::new(0));
    let opts = QueryOptions {
        work: Some(work.clone()),
        ..opts.clone()
    };
    let cursor = CursorOptions {
        batch_rows: rows,
        fallback: policy,
        ..Default::default()
    };
    let mut c = select_cursor(snap.clone(), q, &opts, &cursor)?;
    let mut answer = Vec::new();
    while let Some(batch) = c.next_batch()? {
        for row in 0..batch.len() {
            answer.push(batch.row(row)?.values().to_vec());
        }
    }
    Ok((bag(answer), c.plan().clone(), work.load(Ordering::Relaxed)))
}

/// Runs every form of `forms` on `snap` and checks it against its expectation, and
/// returns the refused forms.
fn check(snap: &Arc<Snapshot>, opts: &QueryOptions, forms: &[Form]) -> Vec<(String, Why)> {
    let mut refused = Vec::new();
    for f in forms {
        let q = format!("{P}{}", f.query);
        let eager = query(snap.clone(), &q, opts).unwrap_or_else(|e| panic!("{}: {e}", f.name));
        let work = Arc::new(AtomicU64::new(0));
        let counted = QueryOptions {
            work: Some(work.clone()),
            ..opts.clone()
        };
        let expected = bag(query(snap.clone(), &q, &counted)
            .unwrap()
            .rows()
            .unwrap()
            .iter()
            .map(|r| r.values().to_vec())
            .collect());
        assert!(!eager.is_empty(), "{}: the fixture answers nothing", f.name);
        // Under the permissive policy every form answers as eager execution does, and
        // only the listed kinds run their whole subtree eagerly.
        let (answer, plan, _) = drain(snap, &q, opts, FallbackPolicy::AllowMaterialization, 2)
            .unwrap_or_else(|e| panic!("{}: {e}", f.name));
        assert_eq!(answer, expected, "{}", f.name);
        let mut ops = Vec::new();
        names(&plan, &mut ops);
        assert!(
            ops.iter().any(|op| op == f.operator),
            "{}: the plan has no {}: {ops:?}",
            f.name,
            f.operator
        );
        let (mut subtree, mut materializes) = (Vec::new(), Vec::new());
        reasons(&plan, &mut subtree, &mut materializes);
        for op in &subtree {
            assert!(
                EAGER_SUBTREES.contains(&op.as_str()),
                "{}: {op} runs its subtree eagerly",
                f.name
            );
        }
        assert_eq!(
            !subtree.is_empty(),
            SUBTREE_FORMS.contains(&f.name),
            "{}: {subtree:?}",
            f.name
        );
        if let Some(note) = f.note {
            let mut found = Vec::new();
            descriptions(&plan, &mut found);
            assert!(
                found.iter().any(|d| d.contains(note)),
                "{}: no description holds {note:?}: {found:?}",
                f.name
            );
        }
        if std::env::var_os("INVENTORY_PLANS").is_some() {
            eprintln!("{}: {plan:#?}", f.name);
        }
        match (
            f.expect,
            drain(snap, &q, opts, FallbackPolicy::RejectMaterialization, 2),
        ) {
            (Native, Ok((answer, plan, streamed))) => {
                assert_eq!(answer, expected, "{}", f.name);
                let (mut subtree, mut materializes) = (Vec::new(), Vec::new());
                reasons(&plan, &mut subtree, &mut materializes);
                assert!(materializes.is_empty(), "{}: {materializes:?}", f.name);
                assert!(subtree.is_empty(), "{}: {subtree:?}", f.name);
                // A native operator counts the rows eager execution counts.
                assert_eq!(
                    streamed,
                    work.load(Ordering::Relaxed),
                    "{}: work against eager",
                    f.name
                );
            }
            (Refused(why), Err(Error::Unsupported(message))) => {
                assert!(
                    message.contains(&format!("materialization at {}", f.operator)),
                    "{}: {message}",
                    f.name
                );
                refused.push((f.name.to_string(), why));
            }
            (Native, Err(e)) => panic!("{}: listed native but refused: {e}", f.name),
            (Refused(why), Ok(_)) => {
                panic!("{}: listed refused ({why:?}) but runs natively", f.name)
            }
            (_, Err(e)) => panic!("{}: {e}", f.name),
        }
    }
    refused
}

#[test]
fn every_plan_form_has_a_native_operator_or_a_named_refusal() {
    let s = base();
    let snap = s.snapshot();
    let remote = base();
    let opts = QueryOptions {
        no_cache: true,
        allow_service: true,
        local_endpoints: Some(Arc::new(Local(remote.snapshot()))),
        views: Some(Arc::new(Integers)),
        ..Default::default()
    };
    let src = "path:source <urn:s:1>";
    let mut forms = vec![
        // SERVICE
        form(
            "SERVICE",
            "Service",
            "SELECT ?s ?o { SERVICE <http://local/sparql> { ?s <urn:p> ?o } }",
            Refused(Why::Pending("group 1")),
        ),
        form(
            "SILENT SERVICE",
            "Service",
            "SELECT ?s ?o { SERVICE SILENT <http://local/sparql> { ?s <urn:p> ?o } }",
            Refused(Why::Decision("Q-SVC1: SILENT buffers its whole answer")),
        ),
        // views
        form(
            "view read",
            "ViewRead",
            "SELECT ?x { SERVICE <urn:x-sparkles:view#read> { [] <urn:x-sparkles:view#name> \"ints\" ; <urn:x-sparkles:view#bind> ?x } }",
            Refused(Why::Row(1121)),
        ),
        // history
        form(
            "history changes with a limit",
            "HistoryChanges",
            "SELECT ?s ?c { SERVICE hist:changes { << ?s <urn:p> ?o >> hist:commit ?c ; hist:limit 5 } }",
            Native,
        ),
        form(
            "history changes without a limit",
            "HistoryChanges",
            "SELECT ?s ?c { SERVICE hist:changes { << ?s <urn:p> ?o >> hist:commit ?c } }",
            Refused(Why::Pending("group 2")),
        ),
        // property functions
        form(
            "constant property function",
            "PropertyFunction",
            "SELECT ?t { ?t apf:strSplit (\"a b c\" \" \") }",
            Refused(Why::Decision("Q1: ARQ's constant property functions")),
        ),
        form(
            "ORDER BY over an EXISTS that does not stream",
            "OrderBy",
            "SELECT ?s { ?s <urn:p> ?o } ORDER BY (EXISTS { ?t apf:strSplit (\"a b\" \" \") })",
            Refused(Why::Decision(
                "an operator whose own EXISTS does not stream",
            )),
        ),
        // path searches
        form(
            "path search leaf with a limit",
            "PathSearch",
            &format!(
                "SELECT ?b {{ SERVICE path:search {{ [] {src} ; path:target ?b ; path:predicate <urn:q> ; path:limit 5 }} }}"
            ),
            Native,
        ),
        form(
            "path search over its group with a limit",
            "PathSearch",
            "SELECT ?s ?b { ?s <urn:p> ?o FILTER(?o < 3) SERVICE path:search { [] path:source ?s ; path:target ?b ; path:predicate <urn:q> ; path:limit 4 } }",
            Native,
        ),
        form(
            "shortest path between constants",
            "PathSearch",
            &format!(
                "SELECT ?i {{ SERVICE path:search {{ [] {src} ; path:target <urn:s:9> ; path:predicate <urn:q> ; path:edgeIndex ?i }} }}"
            ),
            Refused(Why::Pending("group 5")),
        ),
        form(
            "all paths between constants",
            "PathSearch",
            &format!(
                "SELECT ?p ?i {{ SERVICE path:search {{ [] {src} ; path:target <urn:s:9> ; path:predicate <urn:q> ; path:algorithm path:all ; path:maxLength 3 ; path:pathIndex ?p ; path:edgeIndex ?i }} }}"
            ),
            Refused(Why::Pending("group 5")),
        ),
        form(
            "all shortest paths between constants",
            "PathSearch",
            &format!(
                "SELECT ?p ?i {{ SERVICE path:search {{ [] {src} ; path:target <urn:s:9> ; path:predicate <urn:q> ; path:algorithm path:allShortest ; path:pathIndex ?p ; path:edgeIndex ?i }} }}"
            ),
            Refused(Why::Row(1122)),
        ),
        form(
            "shortest paths to a variable end",
            "PathSearch",
            &format!(
                "SELECT ?b {{ SERVICE path:search {{ [] {src} ; path:target ?b ; path:predicate <urn:q> }} }}"
            ),
            Refused(Why::Row(1122)),
        ),
        form(
            "path search over its group without a limit",
            "PathSearch",
            "SELECT ?s ?b { ?s <urn:p> ?o FILTER(?o < 3) SERVICE path:search { [] path:source ?s ; path:target <urn:s:9> ; path:predicate <urn:q> } }",
            Refused(Why::Row(1123)),
        ),
        form(
            "path search over an edge pattern without a limit",
            "PathSearch",
            &format!(
                "SELECT ?i {{ SERVICE path:search {{ [] {src} ; path:target <urn:s:9> ; path:edgeIndex ?i ; path:start ?x ; path:end ?y . ?x <urn:q> ?y }} }}"
            ),
            Refused(Why::Row(1123)),
        ),
        // algorithm calls
        form(
            "algorithm call leaf",
            "AlgoCall",
            "SELECT ?n ?c { SERVICE algo:wcc { [] algo:predicate <urn:q> ; algo:node ?n ; algo:component ?c } }",
            Native,
        ),
        // vector searches
        form(
            "constant vector search",
            "VectorSearch",
            "SELECT ?s ?score { (?s ?score) spk:vectorSearch (ex:emb \"[1,0,0]\"^^spk:vector 2) }",
            Native,
        ),
        form(
            "vector ORDER BY answered by the index",
            "VectorSearch",
            "SELECT ?s { ?s ex:emb ?v } ORDER BY DESC(spk:cosine(?v, \"[1,0,0]\"^^spk:vector)) LIMIT 2",
            Refused(Why::Pending("group 6 (8b)")),
        ),
        // Four vectors have a score, so a limit of six runs the generic plan.
        form(
            "vector ORDER BY with its generic fallback",
            "VectorSearch",
            "SELECT ?s { ?s ex:emb ?v } ORDER BY DESC(spk:cosine(?v, \"[1,0,0]\"^^spk:vector)) LIMIT 6",
            Refused(Why::Pending("group 6 (8b)")),
        )
        .noting("ran the generic plan"),
    ];
    let mut refused = check(&snap, &opts, &forms);
    // An ordered top-k without its heap.
    let mut no_heap = Optimizations::ALL;
    no_heap.topk_heap = false;
    let without_heap = QueryOptions {
        optimizations: Some(no_heap),
        ..opts.clone()
    };
    // An ordered top-k reads decimals, whose ids follow their values, from a range
    // large enough that the planner reads it in value order.
    let decimals = Store::in_memory(StoreOptions::default());
    let data: String = (0..50_000)
        .map(|i| {
            format!(
                "<urn:s:{i}> <urn:v> \"{}.{:02}\"^^<http://www.w3.org/2001/XMLSchema#decimal> .\n",
                (i * 7919) % 5000,
                i % 100
            )
        })
        .collect();
    load(&decimals, &data, RdfFormat::Turtle);
    forms = vec![form(
        "index top-k without its heap",
        "IndexTopK",
        "SELECT ?s ?v { ?s <urn:v> ?v } ORDER BY DESC(?v) LIMIT 3",
        Refused(Why::Pending("group 6 (8a)")),
    )];
    refused.extend(check(&decimals.snapshot(), &without_heap, &forms));
    #[cfg(feature = "text")]
    refused.extend(text_forms());
    #[cfg(feature = "geo")]
    refused.extend(geo_forms());
    // The pending forms are what remains of item 829.
    let pending: Vec<_> = refused
        .iter()
        .filter(|(_, why)| matches!(why, Why::Pending(_)))
        .collect();
    eprintln!("pending forms: {pending:?}");
}

#[cfg(feature = "text")]
fn text_forms() -> Vec<(String, Why)> {
    let s = Store::in_memory(StoreOptions::default());
    load(
        &s,
        r#"
        @prefix ex: <http://example.org/> .
        @prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
        @prefix spk: <urn:x-sparkles:> .
        ex:b1 a ex:Doc ; rdfs:label "The Quick Brown Fox" ; ex:emb "[1, 0]"^^spk:vector .
        ex:b2 a ex:Doc ; rdfs:label "Brown Bears" ; ex:emb "[0.8, 0.6]"^^spk:vector .
        ex:p1 rdfs:label "Foxglove fox" ; ex:emb "[0, 1]"^^spk:vector .
        "#,
        RdfFormat::Turtle,
    );
    s.enable_text(sparkles_core::text::TextConfig::default())
        .unwrap();
    let opts = QueryOptions {
        no_cache: true,
        ..Default::default()
    };
    check(
        &s.snapshot(),
        &opts,
        &[
            form(
                "text search with maxHits",
                "TextSearch",
                "SELECT ?s ?sc { (?s ?sc) text:query (\"fox\" 5) }",
                Native,
            ),
            form(
                "text search without maxHits",
                "TextSearch",
                "SELECT ?s { ?s text:query \"fox\" }",
                Refused(Why::Pending("text group")),
            ),
            form(
                "text search over its group",
                "TextSearch",
                "SELECT ?q ?s { VALUES ?q { \"fox\" \"brown\" } ?s text:query ?q }",
                Native,
            ),
            form(
                "constant hybrid search",
                "HybridSearch",
                "SELECT ?s ?score { (?s ?score) spk:hybridSearch ((\"brown\") (ex:emb \"[1,0]\"^^spk:vector 3) 3) }",
                Native,
            ),
            form(
                "hybrid search over its group",
                "HybridSearch",
                "SELECT ?q ?s { VALUES ?q { \"[1,0]\"^^spk:vector \"[0,1]\"^^spk:vector } (?s ?score) spk:hybridSearch ((\"brown\") (ex:emb ?q 2) 2) }",
                Native,
            ),
            form(
                "hybrid search over its candidates",
                "HybridSearch",
                "SELECT ?s { ?s a ex:Doc . (?s ?score) spk:hybridSearch ((\"brown\") (ex:emb \"[0,1]\"^^spk:vector 2 \"candidates:join\") 5) }",
                Native,
            ),
        ],
    )
}

#[cfg(feature = "geo")]
fn geo_forms() -> Vec<(String, Why)> {
    let s = Store::in_memory(StoreOptions::default());
    let mut data = String::from(
        r#"
        @prefix ex: <http://example.org/> .
        @prefix geo: <http://www.opengis.net/ont/geosparql#> .
        ex:A geo:hasDefaultGeometry ex:gA . ex:gA geo:asWKT "POLYGON((0 0, 10 0, 10 10, 0 10, 0 0))"^^geo:wktLiteral .
        ex:B geo:hasDefaultGeometry ex:gB . ex:gB geo:asWKT "POLYGON((5 5, 15 5, 15 15, 5 15, 5 5))"^^geo:wktLiteral .
        ex:p1 geo:hasGeometry ex:g1 . ex:g1 geo:asWKT "POINT(2 2)"^^geo:wktLiteral .
        ex:p2 geo:hasGeometry ex:g2 . ex:g2 geo:asWKT "POINT(3 4)"^^geo:wktLiteral .
        "#,
    );
    for i in 0..600 {
        data.push_str(&format!(
            "ex:f{i} geo:hasGeometry ex:fg{i} . ex:fg{i} geo:asWKT \"POINT({} {})\"^^geo:wktLiteral .\n",
            100 + i % 40,
            100 + i / 40
        ));
    }
    load(&s, &data, RdfFormat::Turtle);
    // The rewrite answers relation triples such as `?x geo:sfContains ex:g1`.
    s.enable_geo(sparkles_core::geo::GeoConfig {
        query_rewrite: true,
        ..Default::default()
    })
    .unwrap();
    let opts = QueryOptions {
        no_cache: true,
        ..Default::default()
    };
    let window = "\"POLYGON((0 0, 6 0, 6 6, 0 6, 0 0))\"^^geo:wktLiteral";
    check(
        &s.snapshot(),
        &opts,
        &[
            form(
                "spatial scan",
                "SpatialScan",
                &format!("SELECT ?g {{ ?g geo:asWKT ?w FILTER(geof:sfWithin(?w, {window})) }}"),
                Refused(Why::Pending("group 4 (6a)")),
            ),
            form(
                "spatial property function with a limit",
                "SpatialPf",
                "SELECT ?f { ?f spatial:withinBox(0 0 6 6 5) }",
                Native,
            ),
            form(
                "spatial property function without a limit",
                "SpatialPf",
                "SELECT ?f { ?f spatial:withinBox(0 0 6 6) }",
                Refused(Why::Pending("group 4 (6a)")),
            ),
            form(
                "spatial relation",
                "SpatialRelate",
                "SELECT ?x { ?x geo:sfContains ex:g1 }",
                Refused(Why::Pending("group 4 (6b)")),
            ),
            form(
                "spatial join of two index sides",
                "SpatialJoin",
                "SELECT ?a ?b { ?a geo:asWKT ?wa . ?b geo:asWKT ?wb FILTER(geof:sfContains(?wa, ?wb) && ?a != ?b) }",
                Refused(Why::Pending("group 4 (6c)")),
            ),
            form(
                "spatial join over a plan input",
                "SpatialJoin",
                "SELECT ?a ?b { ?a geo:hasGeometry ?ga . ?ga geo:asWKT ?wa . ?b geo:asWKT ?wb FILTER(geof:sfWithin(?wa, ?wb)) }",
                Refused(Why::Pending("group 4 (6c)")),
            ),
            form(
                "nearest neighbours",
                "SpatialKnn",
                "SELECT ?g ?d { ?g geo:asWKT ?w BIND(geof:metricDistance(?w, \"POINT(9 1)\"^^geo:wktLiteral) AS ?d) FILTER(BOUND(?d)) } ORDER BY ?d LIMIT 2",
                Refused(Why::Pending("group 6 (8c)")),
            ),
        ],
    )
}

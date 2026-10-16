//! Deeply nested queries, updates and documents fail with an error, never with a stack
//! overflow (which aborts the process). Everything runs on a thread with a 2 MiB stack,
//! what a spawned thread, a tokio thread and a rayon thread get by default.

use spargebra::nesting::{MAX_DEPTH, MAX_NESTING, measure};
use sparkles::Dataset;
use sparkles::io::RdfFormat;
use sparkles::sparql::depth::{MAX_PLAN_DEPTH, check_query, check_update, query_depth, with_stack};

/// Run `f` on a thread with a 2 MiB stack.
fn on_small_stack<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new()
        .stack_size(2 << 20)
        .spawn(f)
        .unwrap()
        .join()
        .unwrap()
}

fn dataset() -> Dataset {
    let ds = Dataset::memory();
    ds.load_str(
        "<urn:s> <urn:p> <urn:o> . <urn:s> <urn:p> <urn:s> .",
        RdfFormat::Turtle,
    )
    .unwrap();
    ds
}

fn rep(s: &str, n: usize) -> String {
    s.repeat(n)
}

/// Queries whose brackets (or `!`s) nest `n` levels, for `n` of at least 3.
fn nested_queries(n: usize) -> Vec<(&'static str, String)> {
    let r = rep;
    vec![
        (
            "groups",
            format!("SELECT * {} ?s ?p ?o {}", r("{", n), r("}", n)),
        ),
        (
            "parentheses",
            format!("SELECT * {{ FILTER({}1{}) }}", r("(", n - 2), r(")", n - 2)),
        ),
        (
            "function calls",
            format!(
                "SELECT * {{ FILTER({}1{}) }}",
                r("STR(", n - 2),
                r(")", n - 2)
            ),
        ),
        (
            "unary not",
            format!("SELECT * {{ FILTER({}true) }}", r("!", n - 2)),
        ),
        (
            "blank nodes",
            format!(
                "SELECT * {{ ?s ?p {}?o{} }}",
                r("[ <urn:p> ", n - 1),
                r(" ]", n - 1)
            ),
        ),
        (
            "collections",
            format!("SELECT * {{ ?s ?p {}1{} }}", r("(", n - 1), r(")", n - 1)),
        ),
        (
            "triple terms",
            format!(
                "SELECT * {{ ?s ?p {}<urn:o>{} }}",
                r("<<( <urn:s> <urn:p> ", n - 1),
                r(" )>>", n - 1)
            ),
        ),
        (
            "property paths",
            format!(
                "SELECT * {{ ?s {}<urn:p>{} ?o }}",
                r("(", n - 1),
                r(")", n - 1)
            ),
        ),
        (
            "not exists",
            format!(
                "SELECT * {{ {}?s ?p ?o{} }}",
                r("FILTER NOT EXISTS { ", n - 1),
                r(" }", n - 1)
            ),
        ),
        (
            "optional",
            format!(
                "SELECT * {{ {}?s ?p ?o{} }}",
                r("?s ?p ?o OPTIONAL { ", n - 1),
                r(" }", n - 1)
            ),
        ),
    ]
}

/// Queries that chain `k` operators or group elements at one level.
fn chained_queries(k: usize) -> Vec<(&'static str, String)> {
    let r = rep;
    vec![
        (
            "or",
            format!(
                "SELECT * {{ ?s ?p ?o FILTER(?o = ?o{}) }}",
                r(" || ?o = ?o", k)
            ),
        ),
        (
            "sum",
            format!("SELECT * {{ BIND(1{} AS ?x) }}", r(" + 1", k)),
        ),
        (
            "optional",
            format!("SELECT * {{ ?s ?p ?o {} }}", r("OPTIONAL { ?s ?p ?o } ", k)),
        ),
        (
            "union",
            format!(
                "SELECT * {{ {{ ?s ?p ?o }}{} }}",
                r(" UNION { ?s ?p ?o }", k)
            ),
        ),
        (
            "minus",
            format!(
                "SELECT * {{ ?s ?p ?o {} }}",
                r("MINUS { ?s ?p <urn:x> } ", k)
            ),
        ),
        (
            "path",
            format!("SELECT * {{ ?s <urn:p>{} ?o }}", r("/<urn:p>", k)),
        ),
        (
            "inverse steps in parentheses",
            format!("SELECT * {{ ?s (<urn:p>{}) ?o }}", r("^<urn:p>", k)),
        ),
        (
            "path triples",
            format!("SELECT * {{ ?s ?p ?o . {} }}", r("?s <urn:p>* ?o . ", k)),
        ),
        (
            "filters",
            format!("SELECT * {{ ?s ?p ?o {} }}", r("FILTER(true) ", k)),
        ),
        (
            "describe",
            format!("DESCRIBE <urn:s>{} WHERE {{ ?s ?p ?o }}", r(" <urn:s>", k)),
        ),
    ]
}

fn binds(k: usize) -> String {
    let mut q = String::from("SELECT * { ?s ?p ?o ");
    for i in 0..k {
        q.push_str(&format!("BIND({i} AS ?v{i}) "));
    }
    q.push('}');
    q
}

fn refused<T>(r: sparkles::Result<T>, what: &str, limit: usize) {
    match r {
        Err(e) => assert!(
            e.to_string()
                .contains(&format!("nested deeper than {limit} levels")),
            "{what}: {e}"
        ),
        Ok(_) => panic!("{what}: not refused"),
    }
}

#[test]
fn nesting_is_counted_as_the_parser_recurses() {
    for (what, q) in nested_queries(MAX_NESTING) {
        assert_eq!(measure(&q).brackets, MAX_NESTING, "{what}");
    }
}

#[test]
fn nested_queries_run_up_to_the_limit() {
    on_small_stack(|| {
        let ds = dataset();
        for (what, q) in nested_queries(100) {
            ds.query(&q).unwrap_or_else(|e| panic!("{what}: {e}"));
        }
        // at the limit: the parser takes most stack on this thread
        for (what, q) in &nested_queries(MAX_NESTING)[..4] {
            ds.query(q).unwrap_or_else(|e| panic!("{what}: {e}"));
        }
    });
}

#[test]
fn nested_blank_node_plan_runs_on_a_small_stack() {
    on_small_stack(|| {
        let ds = dataset();
        let (_, query) = nested_queries(100)
            .into_iter()
            .find(|(shape, _)| *shape == "blank nodes")
            .unwrap();
        let result = ds.query(&query).unwrap();
        assert_eq!(result.rows().len(), 2);
    });
}

#[test]
fn nested_queries_are_refused_past_the_limit() {
    on_small_stack(|| {
        let ds = dataset();
        for n in [MAX_NESTING + 1, 10_000, 100_000] {
            for (what, q) in nested_queries(n) {
                refused(ds.query(&q), what, MAX_NESTING);
            }
        }
        // a subquery nests twice per level
        let sub = |k| {
            format!(
                "SELECT * {{ {}?s ?p ?o{} }}",
                rep("{ SELECT * { ", k),
                rep(" } }", k)
            )
        };
        ds.query(&sub(50)).unwrap();
        refused(ds.query(&sub(5_000)), "subqueries", MAX_NESTING);
    });
}

#[test]
fn chains_are_refused_past_the_limit() {
    on_small_stack(|| {
        let ds = dataset();
        for (i, (what, q)) in chained_queries(100).into_iter().enumerate() {
            ds.query(&q).unwrap_or_else(|e| panic!("{what}: {e}"));
            // the longest chain under the limit runs, one more operator is refused
            // the longest chain whose count is at the limit
            let depth = |k: usize| measure(&chained_queries(k)[i].1).depth;
            let ks: Vec<usize> = (0..MAX_DEPTH).collect();
            let k = ks.partition_point(|&k| depth(k) <= MAX_DEPTH) - 1;
            assert!(k > MAX_DEPTH - 8, "{what}: {k}");
            let (_, ok) = &chained_queries(k)[i];
            if !what.starts_with("path") && what != "describe" {
                // (evaluating a path of a thousand steps, a thousand path patterns or the
                // description of a thousand resources takes minutes in a debug build)
                ds.query(ok).unwrap_or_else(|e| panic!("{what}: {e}"));
            }
            refused(ds.query(&chained_queries(k + 1)[i].1), what, MAX_DEPTH);
            refused(ds.query(&chained_queries(100_000)[i].1), what, MAX_DEPTH);
        }
        ds.query(&binds(1000)).unwrap();
        refused(ds.query(&binds(2000)), "binds", MAX_DEPTH);
    });
}

#[test]
fn inverse_steps_in_parentheses_are_refused_not_overflowed() {
    // the parser builds `a^b^c` as a sequence nested once per step, and it overflowed
    // an 8 MiB stack at 20,000 steps
    std::thread::Builder::new()
        .stack_size(8 << 20)
        .spawn(|| {
            let ds = dataset();
            let step = "^<urn:p>";
            for k in [20_000, 300_000] {
                let bare = format!("SELECT * {{ ?s (<urn:p>{}) ?o }}", rep(step, k));
                let optional = format!(
                    "SELECT * {{ ?s ?p ?o OPTIONAL {{ ?s (<urn:p>{}) ?o }} }}",
                    rep(step, k)
                );
                for q in [bare, optional] {
                    assert!(measure(&q).depth > MAX_DEPTH, "{k}");
                    refused(ds.query(&q), "inverse steps", MAX_DEPTH);
                }
            }
            // a few steps still run
            ds.query("SELECT * { ?s (<urn:p>^<urn:p>^<urn:p>) ?o }")
                .unwrap();
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn describe_iris_are_refused_not_overflowed() {
    on_small_stack(|| {
        let q = format!(
            "DESCRIBE {} WHERE {{ ?s ?p ?o }}",
            "<urn:s> ".repeat(100_000)
        );
        refused(dataset().query(&q), "describe", MAX_DEPTH);
        let q = format!("DESCRIBE {} WHERE {{ ?s ?p ?o }}", "<urn:s> ".repeat(1000));
        assert!(measure(&q).depth <= MAX_DEPTH);
    });
}

#[test]
fn a_large_basic_graph_pattern_runs_on_a_small_stack() {
    // the plan of a basic graph pattern joins its triples one after the other, so the
    // executor recurses once per triple pattern
    on_small_stack(|| {
        let ds = dataset();
        let mut q = String::from("SELECT * { ");
        for i in 0..300 {
            q.push_str(&format!("?v{i} <urn:p> ?v{} . ", i + 1));
        }
        q.push('}');
        // ?v0 = <urn:s> follows <urn:s> <urn:p> <urn:s> and ends with <urn:o> or <urn:s>
        assert_eq!(ds.query(&q).unwrap().len(), 2);
    });
}

#[test]
fn explain_is_refused_past_the_limit() {
    on_small_stack(|| {
        let ds = dataset();
        let opts = sparkles::sparql::QueryOptions::default();
        for (what, q) in nested_queries(100) {
            sparkles::sparql::explain(ds.snapshot(), &q, &opts)
                .unwrap_or_else(|e| panic!("{what}: {e}"));
        }
        for (what, q) in nested_queries(100_000) {
            refused(
                sparkles::sparql::explain(ds.snapshot(), &q, &opts),
                what,
                MAX_NESTING,
            );
        }
    });
}

/// Updates whose brackets nest `n` levels.
fn nested_updates(n: usize) -> Vec<(&'static str, String)> {
    let r = rep;
    vec![
        (
            "data blank nodes",
            format!(
                "INSERT DATA {{ <urn:a> <urn:p> {}<urn:o>{} }}",
                r("[ <urn:p> ", n - 1),
                r(" ]", n - 1)
            ),
        ),
        (
            "data triple terms",
            format!(
                "INSERT DATA {{ <urn:a> <urn:p> {}<urn:o>{} }}",
                r("<<( <urn:s> <urn:p> ", n - 1),
                r(" )>>", n - 1)
            ),
        ),
        (
            "where groups",
            format!(
                "DELETE {{ ?s ?p ?o }} WHERE {}?s ?p <urn:x>{}",
                r("{ ", n),
                r(" }", n)
            ),
        ),
        (
            "where parentheses",
            format!(
                "INSERT {{ <urn:a> <urn:p> ?x }} WHERE {{ BIND({}1{} AS ?x) }}",
                r("(", n - 2),
                r(")", n - 2)
            ),
        ),
    ]
}

#[test]
fn nested_updates_are_refused_past_the_limit() {
    on_small_stack(|| {
        let ds = dataset();
        for (what, u) in nested_updates(100) {
            ds.update(&u).unwrap_or_else(|e| panic!("{what}: {e}"));
        }
        for n in [MAX_NESTING + 1, 10_000, 100_000] {
            for (what, u) in nested_updates(n) {
                refused(ds.update(&u), what, MAX_NESTING);
            }
        }
        let sum = |k| {
            format!(
                "INSERT {{ <urn:a> <urn:p> ?x }} WHERE {{ BIND(1{} AS ?x) }}",
                rep(" + 1", k)
            )
        };
        ds.update(&sum(100)).unwrap();
        refused(ds.update(&sum(100_000)), "sum", MAX_DEPTH);
        // many operations are not a chain
        ds.update(&rep("INSERT DATA { <urn:a> <urn:p> 1 } ; ", 5_000))
            .unwrap();
    });
}

#[test]
fn updates_cannot_grow_a_triple_term_past_the_limit() {
    on_small_stack(|| {
        let ds = Dataset::memory();
        ds.update("INSERT DATA { <urn:s> <urn:q> <urn:o> }")
            .unwrap();
        let wrap = "DELETE { <urn:s> <urn:q> ?o } INSERT { <urn:s> <urn:q> <<( <urn:s> <urn:p> ?o )>> } \
                    WHERE { <urn:s> <urn:q> ?o }";
        for _ in 0..sparkles::nesting::MAX_TRIPLE_TERMS {
            ds.update(wrap).unwrap();
        }
        let e = ds.update(wrap).unwrap_err().to_string();
        assert!(
            e.contains("triple term nested deeper than 256 levels"),
            "{e}"
        );
        // the deepest one stored reads back
        let r = ds.query("SELECT ?o { <urn:s> <urn:q> ?o }").unwrap();
        let o = r.rows()[0][0].clone().unwrap();
        assert!(o.to_string().starts_with("<<( <urn:s> <urn:p> <<("));
    });
}

/// Turtle whose triple terms nest `n` levels.
fn turtle(n: usize) -> String {
    format!(
        "<urn:s> <urn:p> {}<urn:o>{} .",
        rep("<<( <urn:s> <urn:p> ", n),
        rep(" )>>", n)
    )
}

/// JSON-LD whose objects nest `n` levels.
fn json_ld(n: usize) -> String {
    format!(
        "{}{{\"@id\": \"urn:o\"}}{}",
        rep("{\"urn:p\": ", n - 1),
        rep("}", n - 1)
    )
}

/// RDF/XML whose elements nest `n` levels (a node element and a property element per
/// level of description).
fn rdf_xml(n: usize) -> String {
    let levels = (n - 1) / 2;
    format!(
        "<rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\" xmlns:e=\"urn:e#\">{}<rdf:Description/>{}</rdf:RDF>",
        rep("<rdf:Description><e:p>", levels),
        rep("</e:p></rdf:Description>", levels)
    )
}

#[test]
fn nested_documents_are_refused_past_the_limit() {
    use sparkles::nesting::{MAX_ELEMENTS, MAX_JSON_LD, MAX_TRIPLE_TERMS};
    on_small_stack(|| {
        let jsonld = || RdfFormat::JsonLd {
            profile: oxrdfio::JsonLdProfileSet::empty(),
        };
        let ds = Dataset::memory();
        ds.load_str(&turtle(MAX_TRIPLE_TERMS), RdfFormat::Turtle)
            .unwrap();
        ds.load_str(&json_ld(MAX_JSON_LD), jsonld()).unwrap();
        ds.load_str(&rdf_xml(MAX_ELEMENTS), RdfFormat::RdfXml)
            .unwrap();
        for n in [1, 10_000, 100_000] {
            for format in [RdfFormat::Turtle, RdfFormat::TriG, RdfFormat::NTriples] {
                refused(
                    ds.load_str(&turtle(MAX_TRIPLE_TERMS + n), format),
                    "triple terms",
                    MAX_TRIPLE_TERMS,
                );
            }
            refused(
                ds.load_str(&json_ld(MAX_JSON_LD + n), jsonld()),
                "JSON-LD",
                MAX_JSON_LD,
            );
            refused(
                ds.load_str(&rdf_xml(MAX_ELEMENTS + n), RdfFormat::RdfXml),
                "RDF/XML",
                MAX_ELEMENTS,
            );
        }
        // blank nodes and collections nest without recursion
        let blank = format!(
            "<urn:s> <urn:p> {}<urn:o>{} .",
            rep("[ <urn:p> ", 100_000),
            rep(" ]", 100_000)
        );
        ds.load_str(&blank, RdfFormat::Turtle).unwrap();
        // strings, IRIs and comments are skipped
        let quoted = format!(
            "<urn:s> <urn:p> \"{0}\", '''{0}''' . # {0}\n<urn:s> <urn:p> <urn:{1}> .",
            rep("<<", 1_000),
            rep("%3C%3C", 1_000)
        );
        ds.load_str(&quoted, RdfFormat::Turtle).unwrap();
    });
}

#[test]
fn the_text_count_bounds_the_algebra() {
    // the algebra nests at most about twice as deep as the text count, the bound
    // `MAX_ALGEBRA_DEPTH` relies on
    let mut queries: Vec<String> = Vec::new();
    for n in [3, 10, 50] {
        queries.extend(nested_queries(n).into_iter().map(|(_, q)| q));
        queries.extend(chained_queries(n).into_iter().map(|(_, q)| q));
    }
    queries.push(binds(50));
    for q in &queries {
        let parsed = spargebra::SparqlParser::new().parse_query(q).unwrap();
        let depth = query_depth(&parsed, usize::MAX);
        assert!(
            depth <= 2 * measure(q).depth + 64,
            "{depth} > {:?}: {q}",
            measure(q)
        );
    }
}

#[test]
fn the_w3c_queries_nest_far_below_the_limits() {
    let Some(dir) = std::env::var_os("SPARKLES_W3C_DIR") else {
        return;
    };
    let mut files = vec![std::path::PathBuf::from(dir)];
    let mut checked = 0;
    while let Some(p) = files.pop() {
        if p.is_dir() {
            files.extend(std::fs::read_dir(&p).unwrap().map(|e| e.unwrap().path()));
            continue;
        }
        let ext = p.extension().and_then(|e| e.to_str());
        if !matches!(ext, Some("rq" | "ru")) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&p) else {
            continue;
        };
        let m = measure(&text);
        assert!(
            m.brackets <= MAX_NESTING / 4 && m.depth <= MAX_DEPTH / 8,
            "{}: {m:?}",
            p.display()
        );
        checked += 1;
    }
    assert!(checked > 500, "{checked} files");
}

/// A basic graph pattern of `n` triple patterns over the two triples of [`dataset`],
/// each of which matches.
fn bgp(n: usize) -> String {
    let mut q = String::from("SELECT * { ");
    for i in 0..n {
        let o = if i % 2 == 0 { "s" } else { "o" };
        q.push_str(&format!("<urn:s> <urn:p> <urn:{o}> . "));
    }
    q.push('}');
    q
}

/// A `DELETE WHERE` of `n` triple patterns that match nothing.
fn delete_where(n: usize) -> String {
    let mut u = String::from("DELETE WHERE { ");
    for i in 0..n {
        u.push_str(&format!("<urn:x> <urn:p> <urn:o{i}> . "));
    }
    u.push('}');
    u
}

/// The most triple patterns a [`bgp`] may hold: the planner can join and sort each of
/// them, and the projection and the group around them are plan levels too.
fn largest_bgp() -> usize {
    let n = (MAX_PLAN_DEPTH - 3) / 2;
    let check = |n: usize| {
        let parsed = spargebra::SparqlParser::new().parse_query(&bgp(n)).unwrap();
        check_query(&parsed)
    };
    assert!(check(n).unwrap() <= MAX_PLAN_DEPTH);
    assert!(check(n + 1).is_err());
    n
}

/// The most triple patterns a [`delete_where`] may hold.
fn largest_delete_where() -> usize {
    let n = (MAX_PLAN_DEPTH - 2) / 2;
    let check = |n: usize| {
        let parsed = spargebra::SparqlParser::new()
            .parse_update(&delete_where(n))
            .unwrap();
        check_update(&parsed)
    };
    assert!(check(n).unwrap() <= MAX_PLAN_DEPTH);
    assert!(check(n + 1).is_err());
    n
}

#[test]
fn a_basic_graph_pattern_at_the_plan_limit_runs_and_explains() {
    on_small_stack(|| {
        let ds = dataset();
        let n = largest_bgp();
        assert_eq!(ds.query(&bgp(n)).unwrap().len(), 1);
        let opts = sparkles::sparql::QueryOptions::default();
        let (_, plan) = sparkles::sparql::explain(ds.snapshot(), &bgp(n), &opts).unwrap();
        // writing the plan grows the stack as it needs
        let text = serde_json::to_string(&plan).unwrap();
        assert!(text.len() > 100 * n, "{}", text.len());
        assert_eq!(plan.depth(), n + 1);
        // a JSON value of the plan nests twice as deep, and it is dropped by recursion
        with_stack(plan.depth(), || {
            let value = serde_json::to_value(&plan).unwrap();
            assert_eq!(serde_json::to_string(&value).unwrap().len(), text.len());
        });
        drop(plan);
        ds.update(&delete_where(largest_delete_where())).unwrap();
    });
}

#[test]
fn a_basic_graph_pattern_past_the_plan_limit_is_refused() {
    on_small_stack(|| {
        let ds = dataset();
        let opts = sparkles::sparql::QueryOptions::default();
        let sizes = [
            (largest_bgp(), largest_delete_where()),
            (MAX_PLAN_DEPTH, MAX_PLAN_DEPTH),
            (100_000, 100_000),
        ];
        for (q, u) in sizes {
            let t = std::time::Instant::now();
            plan_refused(ds.query(&bgp(q + 1)));
            plan_refused(sparkles::sparql::explain(ds.snapshot(), &bgp(q + 1), &opts));
            plan_refused(ds.update(&delete_where(u + 1)));
            // refused before planning, which takes seconds at this size
            assert!(t.elapsed().as_secs() < 5, "{q}: {:?}", t.elapsed());
        }
    });
}

/// Four triples with a cycle and a second predicate, so that the planner cannot order a
/// join by a constant and builds joins and sorts for variable joins.
fn cyclic() -> Dataset {
    let ds = Dataset::memory();
    ds.load_str(
        "<urn:s> <urn:p> <urn:o> . <urn:o> <urn:p> <urn:s> . \
         <urn:s> <urn:q> 1 . <urn:o> <urn:q> 2 .",
        RdfFormat::Turtle,
    )
    .unwrap();
    ds
}

/// Shapes of `n` triple patterns, each joined to the one before it by a variable.
fn plan_shapes(n: usize) -> Vec<(String, String)> {
    let select = |body: String| format!("SELECT * {{ {body} }}");
    let each = |f: &dyn Fn(usize) -> String| (0..n).map(f).collect::<String>();
    let mut shapes = vec![
        (
            "chain",
            select(each(&|i| format!("?a{i} <urn:p> ?a{} . ", i + 1))),
        ),
        (
            "reversed chain",
            select(
                (0..n)
                    .rev()
                    .map(|i| format!("?a{i} <urn:p> ?a{} . ", i + 1))
                    .collect(),
            ),
        ),
        (
            "tree",
            select(each(&|i| format!("?a{} <urn:p> ?a{} . ", i / 2, i + 1))),
        ),
        (
            "pairs",
            select(each(&|i| format!("?x <urn:p> ?y{i} . ?y{i} <urn:q> ?z . "))),
        ),
        (
            "chain with a second predicate",
            select(each(&|i| {
                format!("?a{i} <urn:p> ?a{} . ?a{} <urn:q> [] . ", i + 1, i + 1)
            })),
        ),
        (
            "chain with values",
            select(each(&|i| {
                format!(
                    "?a{i} <urn:p> ?a{} . VALUES ?a{} {{ <urn:s> <urn:o> }} ",
                    i + 1,
                    i + 1
                )
            })),
        ),
        (
            "chain with filters",
            select(each(&|i| {
                format!("?a{i} <urn:p> ?a{} . FILTER(isIRI(?a{i})) ", i + 1)
            })),
        ),
        (
            "chain with optionals",
            select(each(&|i| {
                format!(
                    "?a{i} <urn:p> ?a{} . OPTIONAL {{ ?a{} <urn:q> ?c{i} }} ",
                    i + 1,
                    i + 1
                )
            })),
        ),
    ];
    // shapes that do not grow with n
    if n == 100 {
        let fixed = [
            ("nested ranges 8", "?s ((<urn:p>{8}){8}){8} ?o"),
            ("nested ranges 16", "?s ((<urn:p>{16}){16}){4} ?o"),
            ("nested ranges 3 to 5", "?s ((<urn:p>{3,5}){3,5}){3,5} ?o"),
            ("alternative", "?s (<urn:p>|<urn:q>)/(<urn:p>/<urn:q>)* ?o"),
            ("negated set", "?s !(<urn:p>)/<urn:p>{2} ?o"),
        ];
        shapes.extend(fixed.map(|(what, body)| (what, select(body.to_string()))));
    }
    shapes
        .into_iter()
        .map(|(what, q)| (format!("{what} of {n}"), q))
        .collect()
}

#[test]
fn the_measured_plan_depth_bounds_the_plan() {
    on_small_stack(|| {
        let ds = cyclic();
        let opts = sparkles::sparql::QueryOptions::default();
        // the planner flattens joins and sorts before a blank node joins on, so a group of
        // basic graph patterns and a blank node count as deep as the plan they make
        let ground = |n: usize| "<urn:s> <urn:p> ?x . ".repeat(n);
        let chain = |n: usize, v: &str| {
            (0..n)
                .map(|i| format!("?{v}{i} <urn:p> ?{v}{} . ", i + 1))
                .collect::<String>()
        };
        let blank = |n: usize| "?s <urn:p> [ <urn:p> ?o ] . ".repeat(n);
        // (planning a few hundred of the joins takes minutes in a debug build)
        let mut queries: Vec<(String, String)> =
            [100, 200].into_iter().flat_map(plan_shapes).collect();
        let more = [
            format!("SELECT * {{ {} }}", blank(50)),
            format!(
                "SELECT * {{ {} }}",
                "?s <urn:p> [ <urn:p> [ <urn:p> ?o ] ] . ".repeat(30)
            ),
            format!("SELECT * {{ {} BIND(1 AS ?z) {} }}", ground(50), ground(50)),
            format!(
                "SELECT * {{ {} {{ SELECT * {{ {} }} }} }}",
                ground(50),
                ground(50)
            ),
            format!(
                "SELECT * {{ {} GRAPH ?g {{ {} }} }}",
                ground(50),
                ground(50)
            ),
            format!(
                "SELECT * {{ {} OPTIONAL {{ ?x <urn:p> ?y }} {} }}",
                ground(50),
                ground(50)
            ),
            format!(
                "SELECT * {{ {} FILTER(?x != 1) {} }}",
                ground(50),
                ground(50)
            ),
            format!(
                "SELECT * {{ {} OPTIONAL {{ {} }} }}",
                chain(50, "a"),
                chain(50, "b")
            ),
            format!(
                "SELECT * {{ {{ {} }} UNION {{ {} }} }}",
                ground(50),
                ground(50)
            ),
            format!(
                "SELECT * {{ {} MINUS {{ {} }} }}",
                chain(50, "a"),
                chain(50, "b")
            ),
            format!(
                "SELECT * {{ {} FILTER EXISTS {{ {} }} {} }}",
                chain(50, "a"),
                chain(50, "b"),
                chain(50, "c")
            ),
            format!("SELECT * {{ ?s <urn:p>{{3,5}} ?o . {} }}", ground(50)),
            format!("ASK {{ {} }}", chain(100, "a")),
            format!(
                "CONSTRUCT {{ ?a0 <urn:p> ?a1 }} WHERE {{ {} }}",
                chain(100, "a")
            ),
        ];
        queries.extend(more.into_iter().map(|q| (q.clone(), q)));
        for (what, q) in &queries {
            let parsed = spargebra::SparqlParser::new().parse_query(q).unwrap();
            let measured = check_query(&parsed).unwrap();
            let (_, plan) = sparkles::sparql::explain(ds.snapshot(), q, &opts).unwrap();
            assert!(
                plan.depth() <= measured,
                "{what}: {} > {measured}",
                plan.depth()
            );
        }
        // so plans the parts of a query stack into are refused whole
        let ds = dataset();
        let half = MAX_PLAN_DEPTH / 4;
        plan_refused(ds.query(&format!(
            "SELECT * {{ {} BIND(1 AS ?z) {} }}",
            ground(half),
            ground(half)
        )));
        plan_refused(ds.query(&format!(
            "SELECT * {{ {} {{ SELECT * {{ {} }} }} }}",
            ground(half),
            ground(half)
        )));
        plan_refused(ds.query(&format!("SELECT * {{ {} }}", blank(MAX_PLAN_DEPTH / 4 + 1))));
    });
}

/// Prints how long `explain` takes and how large its JSON plan is for chains of triple
/// patterns up to the plan limit. The planning time grows with the square of the chain,
/// and the plan JSON names each node by its path from the root. Run it in a release build
/// for the numbers in the docs, with
/// `cargo test --release -p sparkles --test nesting large_bgp_planning_time -- --ignored --nocapture`.
#[test]
#[ignore = "measures time, not behaviour"]
fn large_bgp_planning_time() {
    let ds = cyclic();
    let opts = sparkles::sparql::QueryOptions::default();
    for n in [500, 1000, (MAX_PLAN_DEPTH - 3) / 2] {
        let q = format!(
            "SELECT * {{ {} }}",
            (0..n)
                .map(|i| format!("?a{i} <urn:p> ?a{} . ", i + 1))
                .collect::<String>()
        );
        let t = std::time::Instant::now();
        let (_, plan) = sparkles::sparql::explain(ds.snapshot(), &q, &opts).unwrap();
        let planned = t.elapsed();
        let bytes = with_stack(plan.depth(), || serde_json::to_string(&plan).unwrap().len());
        println!(
            "chain of {n}: explain {planned:?}, plan depth {}, plan JSON {:.1} MB",
            plan.depth(),
            bytes as f64 / 1e6
        );
        with_stack(plan.depth(), || drop(plan));
    }
}

#[test]
fn nested_ranges_are_refused_not_overflowed() {
    on_small_stack(|| {
        let ds = dataset();
        let opts = sparkles::sparql::QueryOptions::default();
        let cursor = sparkles::sparql::CursorOptions::default();
        // 4096 unrolled steps, from 44 bytes
        let q = "SELECT * { ?s ((<urn:p>{32}){32}){4} ?o }";
        plan_refused(ds.query(q));
        plan_refused(sparkles::sparql::explain(ds.snapshot(), q, &opts));
        plan_refused(sparkles::sparql::query_cursor(
            ds.snapshot(),
            q,
            &opts,
            &cursor,
        ));
        // 32 steps of 32 is within the bound
        let q = "SELECT * { ?s (<urn:p>{32}){32} ?o }";
        let parsed = spargebra::SparqlParser::new().parse_query(q).unwrap();
        check_query(&parsed).unwrap();
        sparkles::sparql::explain(ds.snapshot(), q, &opts).unwrap();
    });
}

fn plan_refused<T>(r: sparkles::Result<T>) {
    let want = format!("query plan nested deeper than {MAX_PLAN_DEPTH} levels");
    match r {
        Err(e) => assert!(e.to_string().contains(&want), "{e}"),
        Ok(_) => panic!("not refused"),
    }
}

//! Tests for the fluent SPARQL query builder (`sparkles::querybuilder`).

use oxrdf::{Literal, NamedNode, Term, Triple};
use sparkles::io::RdfFormat;
use sparkles::querybuilder::{
    AskBuilder, ConstructBuilder, DescribeBuilder, GraphTarget, SelectBuilder, UpdateBuilder,
    WhereBuilder, expr, iri, lit, lit_lang, lit_typed, node, undef, var,
};
use sparkles::{Dataset, Error, Solutions};

const EX: &str = "http://example.org/";

const DATA: &str = r#"
@prefix ex: <http://example.org/> .
@prefix foaf: <http://xmlns.com/foaf/0.1/> .
ex:alice a foaf:Person ; foaf:name "Alice" ; foaf:age 34 ; ex:dept ex:eng ; ex:salary 100 ;
    foaf:knows ex:bob .
ex:bob a foaf:Person ; foaf:name "Bob" ; foaf:age 25 ; ex:dept ex:eng ; ex:salary 80 ;
    foaf:knows ex:carol .
ex:carol a foaf:Person ; foaf:name "Carol" ; foaf:age 41 ; ex:dept ex:sales ; ex:salary 90 ;
    foaf:mbox <mailto:carol@example.org> .
ex:dave a foaf:Person ; foaf:name "Dave" ; ex:dept ex:sales ; ex:salary 70 .
ex:acme a foaf:Organization ; foaf:name "ACME" .
"#;

const GRAPHS: &str = r#"
@prefix ex: <http://example.org/> .
ex:g1 { ex:alice ex:role "admin" . ex:bob ex:role "user" . }
ex:g2 { ex:carol ex:role "user" . }
"#;

fn dataset() -> Dataset {
    let ds = Dataset::memory();
    ds.load_str(DATA, RdfFormat::Turtle).unwrap();
    ds.load_str(GRAPHS, RdfFormat::TriG).unwrap();
    ds
}

fn ex(local: &str) -> String {
    format!("{EX}{local}")
}

/// Lexical form / IRI of a term.
fn text(t: &Term) -> String {
    match t {
        Term::NamedNode(n) => n.as_str().to_string(),
        Term::Literal(l) => l.value().to_string(),
        other => other.to_string(),
    }
}

/// Values of one variable, in solution order (`-` for unbound).
fn col(sols: &Solutions, v: &str) -> Vec<String> {
    sols.iter()
        .map(|s| s.get(v).map(text).unwrap_or_else(|| "-".into()))
        .collect()
}

fn sorted(mut v: Vec<String>) -> Vec<String> {
    v.sort();
    v
}

fn people() -> SelectBuilder {
    SelectBuilder::new().prefix("ex", EX)
}

// ------------------------------------------------------------------ rendering ----

#[test]
fn render_select_full() {
    let q = SelectBuilder::new()
        .prefix("ex", EX)
        .distinct()
        .select("?name")
        .select(var("age"))
        .from(iri("http://example.org/g"))
        .from_named("<http://example.org/h>")
        .where_("?p", "a", "foaf:Person")
        .where_("?p", "foaf:name", "?name")
        .optional(|w| w.where_("?p", "foaf:age", "?age"))
        .filter("?age > 30 || !BOUND(?age)")
        .order_by_desc("?age")
        .order_by("?name")
        .limit(10)
        .offset(5);
    assert_eq!(
        q.build().unwrap(),
        "PREFIX ex: <http://example.org/>\n\
         PREFIX foaf: <http://xmlns.com/foaf/0.1/>\n\
         SELECT DISTINCT ?name ?age\n\
         FROM <http://example.org/g>\n\
         FROM NAMED <http://example.org/h>\n\
         WHERE {\n\
         \x20 ?p a foaf:Person .\n\
         \x20 ?p foaf:name ?name .\n\
         \x20 OPTIONAL {\n\
         \x20   ?p foaf:age ?age .\n\
         \x20 }\n\
         \x20 FILTER(?age > 30 || !BOUND(?age))\n\
         }\n\
         ORDER BY DESC(?age) ?name\n\
         LIMIT 10\n\
         OFFSET 5"
    );
    // Display is the same text.
    assert_eq!(q.to_string(), q.build().unwrap());
}

#[test]
fn render_patterns() {
    let q = SelectBuilder::new()
        .prefix("ex", EX)
        .select_all()
        .union(
            |w| w.where_("?s", "ex:p", "?o"),
            |w| w.where_("?s", "ex:q", "?o"),
        )
        .minus(|w| w.where_("?s", "ex:hidden", true))
        .graph("?g", |w| w.where_("?s", "ex:role", "?r"))
        .bind(expr::strlen(var("r")), "?len")
        .values("?s", [iri(ex("a")), iri(ex("b"))])
        .values_rows(["?x", "?y"], [[node(1), undef()], [node("ex:c"), lit("d")]])
        .filter_not_exists(|w| w.where_("?s", "ex:deleted", true))
        .sub_select(
            SelectBuilder::new()
                .select("?s")
                .select_expr(expr::count_star(), "?n")
                .where_("?s", "ex:p", "?any")
                .group_by("?s"),
        );
    assert_eq!(
        q.build().unwrap(),
        "PREFIX ex: <http://example.org/>\n\
         SELECT *\n\
         WHERE {\n\
         \x20 {\n\
         \x20   ?s ex:p ?o .\n\
         \x20 } UNION {\n\
         \x20   ?s ex:q ?o .\n\
         \x20 }\n\
         \x20 MINUS {\n\
         \x20   ?s ex:hidden true .\n\
         \x20 }\n\
         \x20 GRAPH ?g {\n\
         \x20   ?s ex:role ?r .\n\
         \x20 }\n\
         \x20 BIND(STRLEN(?r) AS ?len)\n\
         \x20 VALUES ?s { <http://example.org/a> <http://example.org/b> }\n\
         \x20 VALUES (?x ?y) { (1 UNDEF) (ex:c \"d\") }\n\
         \x20 FILTER(NOT EXISTS {\n\
         \x20   ?s ex:deleted true .\n\
         \x20 })\n\
         \x20 {\n\
         \x20   SELECT ?s (COUNT(*) AS ?n)\n\
         \x20   WHERE {\n\
         \x20     ?s ex:p ?any .\n\
         \x20   }\n\
         \x20   GROUP BY ?s\n\
         \x20 }\n\
         }"
    );
}

#[test]
fn render_ask_construct_describe() {
    let ask = AskBuilder::new()
        .prefix("ex", EX)
        .where_("ex:alice", "foaf:knows+", "?x");
    assert_eq!(
        ask.build().unwrap(),
        "PREFIX ex: <http://example.org/>\n\
         PREFIX foaf: <http://xmlns.com/foaf/0.1/>\n\
         ASK\n\
         WHERE {\n  ex:alice foaf:knows+ ?x .\n}"
    );

    let short = ConstructBuilder::new().where_("?s", "?p", "?o").limit(3);
    assert_eq!(
        short.build().unwrap(),
        "CONSTRUCT WHERE {\n  ?s ?p ?o .\n}\nLIMIT 3"
    );

    let d = DescribeBuilder::new().describe(iri(ex("alice")));
    assert_eq!(d.build().unwrap(), "DESCRIBE <http://example.org/alice>");
    let d = DescribeBuilder::new()
        .prefix("ex", EX)
        .describe("?p")
        .where_("?p", "ex:dept", "ex:eng");
    assert!(
        d.build()
            .unwrap()
            .starts_with("PREFIX ex: <http://example.org/>\nDESCRIBE ?p\nWHERE {")
    );
}

#[test]
fn render_update() {
    let u = UpdateBuilder::new()
        .prefix("ex", EX)
        .insert_data("ex:a", "ex:p", 1)
        .insert_data_graph("ex:g", "ex:a", "ex:p", lit_lang("chat", "fr"))
        .insert_data("ex:b", "a", "ex:C")
        .then()
        .with("ex:g")
        .delete("?s", "ex:p", "?o")
        .insert_graph("ex:h", "?s", "ex:q", "?o")
        .using("ex:u")
        .where_("?s", "ex:p", "?o")
        .delete_where("?x", "ex:gone", "?y")
        .load_into(iri("http://example.org/data.ttl"), "ex:g")
        .silent()
        .clear("DEFAULT")
        .drop(GraphTarget::graph("ex:g"))
        .create(iri(ex("new")))
        .copy("DEFAULT", "ex:backup");
    assert_eq!(
        u.build().unwrap(),
        "PREFIX ex: <http://example.org/>\n\
         INSERT DATA {\n\
         \x20 ex:a ex:p 1 .\n\
         \x20 ex:b a ex:C .\n\
         \x20 GRAPH ex:g {\n\
         \x20   ex:a ex:p \"chat\"@fr .\n\
         \x20 }\n\
         } ;\n\
         WITH ex:g\n\
         DELETE {\n  ?s ex:p ?o .\n}\n\
         INSERT {\n  GRAPH ex:h {\n    ?s ex:q ?o .\n  }\n}\n\
         USING ex:u\n\
         WHERE {\n  ?s ex:p ?o .\n} ;\n\
         DELETE WHERE {\n  ?x ex:gone ?y .\n} ;\n\
         LOAD SILENT <http://example.org/data.ttl> INTO GRAPH ex:g ;\n\
         CLEAR DEFAULT ;\n\
         DROP GRAPH ex:g ;\n\
         CREATE GRAPH <http://example.org/new> ;\n\
         COPY DEFAULT TO ex:backup"
    );
}

#[test]
fn render_expressions() {
    let a = || var("a");
    let b = || var("b");
    let c = || var("c");
    let cases: Vec<(expr::Expr, &str)> = vec![
        (expr::and(expr::or(a(), b()), c()), "(?a || ?b) && ?c"),
        (expr::or(expr::and(a(), b()), c()), "?a && ?b || ?c"),
        (expr::mul(expr::add(a(), 1), b()), "(?a + 1) * ?b"),
        (expr::add(expr::mul(a(), 2), b()), "?a * 2 + ?b"),
        (expr::sub(a(), expr::sub(b(), c())), "?a - (?b - ?c)"),
        (expr::sub(expr::sub(a(), b()), c()), "?a - ?b - ?c"),
        (expr::sub(a(), -5), "?a - -5"),
        (expr::neg(-5), "-(-5)"),
        (expr::eq(expr::eq(a(), b()), true), "(?a = ?b) = true"),
        (expr::not(expr::and(a(), b())), "!(?a && ?b)"),
        (expr::not(expr::bound(a())), "!BOUND(?a)"),
        (expr::gt("?a + 1", 3), "(?a + 1) > 3"),
        (expr::in_(a(), [1, 2, 3]), "?a IN (1, 2, 3)"),
        (
            expr::not_in(expr::add(a(), 1), [lit("x")]),
            "?a + 1 NOT IN (\"x\")",
        ),
        (expr::and(expr::in_(a(), [1]), b()), "?a IN (1) && ?b"),
        (expr::count_distinct(a()), "COUNT(DISTINCT ?a)"),
        (expr::count_star(), "COUNT(*)"),
        (expr::sum(a()).distinct(), "SUM(DISTINCT ?a)"),
        (
            expr::group_concat(a(), Some("\", ")),
            "GROUP_CONCAT(?a; SEPARATOR = \"\\\", \")",
        ),
        (
            expr::regex_flags(a(), "^a\"b", "i"),
            "REGEX(?a, \"^a\\\"b\", \"i\")",
        ),
        (
            expr::lang_matches(expr::lang(a()), lit("en")),
            "langMatches(LANG(?a), \"en\")",
        ),
        (expr::func("xsd:integer", [a()]), "xsd:integer(?a)"),
        (expr::func("strlen", [a()]), "STRLEN(?a)"),
        (
            expr::func("<http://ex/f>", [a(), b()]),
            "<http://ex/f>(?a, ?b)",
        ),
        (expr::if_(a(), 1, 2.5), "IF(?a, 1, 2.5e0)"),
        (expr::coalesce([a(), lit("none")]), "COALESCE(?a, \"none\")"),
        (expr::and_all([a(), b(), c()]), "?a && ?b && ?c"),
        (
            expr::contains(expr::lcase(a()), lit("x")),
            "CONTAINS(LCASE(?a), \"x\")",
        ),
    ];
    for (e, expected) in cases {
        assert_eq!(e.to_string(), expected);
        // Every case is also valid SPARQL (aggregates as a projection, the rest in a
        // FILTER).
        let q = SelectBuilder::new().where_("?a", "?b", "?c");
        let is_aggregate = ["COUNT(", "SUM(", "GROUP_CONCAT("]
            .iter()
            .any(|p| expected.starts_with(p));
        let q = if is_aggregate {
            q.select_expr(e.clone(), "?r")
        } else {
            q.filter(e.clone())
        };
        q.build().unwrap_or_else(|err| panic!("{expected}: {err}"));
    }
}

#[test]
fn term_conversions() {
    let q = SelectBuilder::new()
        .where_("?s", "<http://ex/p>", 42)
        .where_("?s", "<http://ex/p>", -1.5f64)
        .where_("?s", "<http://ex/p>", false)
        .where_("?s", "<http://ex/p>", lit_typed("2024-01-01", "xsd:date"))
        .where_("?s", "<http://ex/p>", "\"x\"@en-GB")
        .where_(
            "?s",
            "<http://ex/p>",
            Literal::new_typed_literal(
                "7",
                NamedNode::new_unchecked("http://www.w3.org/2001/XMLSchema#int"),
            ),
        )
        .where_("?s", "<http://ex/p>", f64::NAN)
        .where_(
            NamedNode::new_unchecked("http://ex/s"),
            "<http://ex/p>",
            Term::from(Literal::new_simple_literal("t")),
        );
    let text = q.build().unwrap();
    for piece in [
        "?s <http://ex/p> 42 .",
        "?s <http://ex/p> -1.5e0 .",
        "?s <http://ex/p> false .",
        "?s <http://ex/p> \"2024-01-01\"^^xsd:date .",
        "?s <http://ex/p> \"x\"@en-GB .",
        "?s <http://ex/p> \"7\"^^<http://www.w3.org/2001/XMLSchema#int> .",
        "?s <http://ex/p> \"NaN\"^^<http://www.w3.org/2001/XMLSchema#double> .",
        "<http://ex/s> <http://ex/p> \"t\" .",
        "PREFIX xsd: <http://www.w3.org/2001/XMLSchema#>",
    ] {
        assert!(text.contains(piece), "missing {piece:?} in\n{text}");
    }
}

// --------------------------------------------------------------- injection ----

const PAYLOADS: &[&str] = &[
    "\"} ; DROP ALL ; {\"",
    "\" } ; DROP ALL ; { \"",
    "'''\"\"\"} ; DROP ALL ; {",
    "x\\\" } ; DROP ALL ; { ?s ?p \"",
    "line1\nline2\r\n\t} ; DROP ALL",
    "\\u0022 } ; DROP ALL ; {",
    "\\",
    "> } ; DROP ALL ; { <",
];

#[test]
fn injection_literals_stay_literals() {
    let ds = dataset();
    let before = ds.len();
    for payload in PAYLOADS {
        // The payload renders as a single escaped literal and the request parses.
        let u = UpdateBuilder::new().insert_data(iri(ex("inj")), iri(ex("val")), lit(payload));
        let text = u.build().unwrap();
        assert_eq!(text.matches("INSERT DATA").count(), 1);
        assert!(!text.contains("DROP ALL ;\n"));
        u.execute(&ds).unwrap();

        // Round trip: the stored value is exactly the payload, found by value too.
        let q = SelectBuilder::new()
            .select("?v")
            .where_(iri(ex("inj")), iri(ex("val")), "?v")
            .filter(expr::eq(var("v"), lit(payload)));
        let rows = q.execute(&ds).unwrap();
        assert_eq!(
            col(&rows, "v"),
            vec![payload.to_string()],
            "payload {payload:?}"
        );

        // In a filter via set_var and in a regex, too.
        let q = SelectBuilder::new()
            .select("?s")
            .where_("?s", iri(ex("val")), "?v")
            .filter(expr::eq("?v", "?needle"))
            .set_var("?needle", lit(payload));
        assert_eq!(q.execute(&ds).unwrap().len(), 1);
        let q = SelectBuilder::new()
            .where_("?s", "?p", "?o")
            .filter(expr::regex(var("o"), payload));
        q.build().unwrap();

        UpdateBuilder::new()
            .delete_data(iri(ex("inj")), iri(ex("val")), lit(payload))
            .execute(&ds)
            .unwrap();
    }
    assert_eq!(ds.len(), before, "no data may have been dropped");
}

#[test]
fn injection_iris_and_tags_are_rejected() {
    // An IRI cannot be closed early: the forbidden characters are \u-escaped and the
    // parser then rejects the (invalid) IRI instead of executing anything.
    let q = SelectBuilder::new().where_(
        iri("http://ex/a> } ; DROP ALL ; { <http://ex/b"),
        "?p",
        "?o",
    );
    let text = q.to_string();
    assert!(
        text.contains("<http://ex/a\\u003E\\u0020\\u007D\\u0020;"),
        "{text}"
    );
    assert!(q.build().is_err());

    assert!(matches!(
        SelectBuilder::new()
            .where_("?s", "?p", lit_lang("x", "en } ; DROP ALL"))
            .build(),
        Err(Error::Invalid(_))
    ));
    assert!(matches!(
        SelectBuilder::new()
            .where_("?s", "?p", var("x } DROP ALL"))
            .build(),
        Err(Error::Invalid(_))
    ));
    // Term strings are strict: trailing syntax is not accepted.
    assert!(matches!(
        SelectBuilder::new()
            .where_("?s", "?p", "\"x\" } ; DROP ALL ; { ?a ?b \"y\"")
            .build(),
        Err(Error::Invalid(_))
    ));
}

// ------------------------------------------------------------------- errors ----

#[test]
fn errors() {
    // Undeclared prefix.
    let err = SelectBuilder::new()
        .where_("?s", "nope:p", "?o")
        .build()
        .unwrap_err();
    assert!(
        matches!(&err, Error::Invalid(m) if m.contains("undeclared prefix 'nope:'")),
        "{err}"
    );
    // ... also inside raw expressions.
    let err = SelectBuilder::new()
        .where_("?s", "?p", "?o")
        .filter("nope:f(?o)")
        .build()
        .unwrap_err();
    assert!(err.to_string().contains("nope:"), "{err}");
    // Well-known prefixes are declared automatically.
    let q = SelectBuilder::new()
        .where_("?s", "rdfs:label", "?o")
        .build()
        .unwrap();
    assert!(q.starts_with("PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>\n"));
    // Bad term.
    let err = SelectBuilder::new()
        .where_("?s", "not a term", "?o")
        .build()
        .unwrap_err();
    assert!(
        matches!(&err, Error::Invalid(m) if m.contains("not a term")),
        "{err}"
    );
    // Paths only in predicate position.
    assert!(
        SelectBuilder::new()
            .where_("ex:a/ex:b", "?p", "?o")
            .prefix("ex", EX)
            .build()
            .is_err()
    );
    // Projection must be a variable.
    assert!(
        SelectBuilder::new()
            .select("<http://ex/a>")
            .where_("?s", "?p", "?o")
            .build()
            .is_err()
    );
    // Raw expression syntax errors come from the parser.
    let err = SelectBuilder::new()
        .where_("?s", "?p", "?o")
        .filter("?o >")
        .build()
        .unwrap_err();
    assert!(matches!(err, Error::SparqlSyntax(_)), "{err}");
    // VALUES row width, UNDEF outside VALUES, invalid function name.
    assert!(
        SelectBuilder::new()
            .values_rows(["?a", "?b"], [[1]])
            .build()
            .is_err()
    );
    assert!(
        SelectBuilder::new()
            .where_("?s", "?p", undef())
            .build()
            .is_err()
    );
    assert!(
        SelectBuilder::new()
            .filter(expr::func("bad name", [1]))
            .build()
            .is_err()
    );
    // Updates.
    assert!(UpdateBuilder::new().build().is_err());
    assert!(
        UpdateBuilder::new()
            .where_("?s", "?p", "?o")
            .build()
            .is_err()
    );
    assert!(
        UpdateBuilder::new()
            .insert_data("?s", "<http://ex/p>", 1)
            .build()
            .is_err()
    );
    assert!(UpdateBuilder::new().copy("ALL", "DEFAULT").build().is_err());
    // Conflicting prefix between query and sub-select.
    let err = SelectBuilder::new()
        .prefix("ex", "http://a/")
        .where_("?s", "ex:p", "?o")
        .sub_select(
            SelectBuilder::new()
                .prefix("ex", "http://b/")
                .where_("?s", "ex:q", "?o"),
        )
        .build()
        .unwrap_err();
    assert!(err.to_string().contains("declared as both"), "{err}");
}

// ---------------------------------------------------------------- execution ----

#[test]
fn exec_select_optional_filter_order_limit() {
    let ds = dataset();
    let q = people()
        .select("?name")
        .select("?age")
        .where_("?p", "a", "foaf:Person")
        .where_("?p", "foaf:name", "?name")
        .optional(|w| w.where_("?p", "foaf:age", "?age"))
        .filter(expr::or(
            expr::not(expr::bound(var("age"))),
            expr::gt(var("age"), 30),
        ))
        .order_by("?name");
    let rows = q.execute(&ds).unwrap();
    assert_eq!(rows.vars, vec!["name", "age"]);
    assert_eq!(col(&rows, "name"), ["Alice", "Carol", "Dave"]);
    assert_eq!(col(&rows, "age"), ["34", "41", "-"]);

    let rows = q.clone().limit(1).offset(1).execute(&ds).unwrap();
    assert_eq!(col(&rows, "name"), ["Carol"]);

    let rows = q
        .clone()
        .filter("STRSTARTS(?name, \"C\") || ?name = \"Dave\"")
        .order_by_desc("?name")
        .execute(&ds)
        .unwrap();
    // First key (?name ASC) wins.
    assert_eq!(col(&rows, "name"), ["Carol", "Dave"]);

    let rows = people()
        .select("?name")
        .where_("?p", "foaf:name", "?name")
        .where_("?p", "foaf:age", "?age")
        .order_by_desc(expr::sub(0, var("age")))
        .execute(&ds)
        .unwrap();
    assert_eq!(col(&rows, "name"), ["Bob", "Alice", "Carol"]);
}

#[test]
fn exec_group_by_having_aggregates() {
    let ds = dataset();
    let q = people()
        .select("?dept")
        .select_expr(expr::count_star(), "?n")
        .select_expr(expr::sum(var("salary")), "?total")
        .select_expr(expr::avg(var("salary")), "?avg")
        .select_expr(expr::min(var("salary")), "?lo")
        .select_expr(expr::max(var("salary")), "?hi")
        .select_expr(expr::group_concat(var("name"), Some("|")), "?names")
        .select_expr(expr::sample(var("name")), "?one")
        .where_("?p", "ex:dept", "?dept")
        .where_("?p", "ex:salary", "?salary")
        .where_("?p", "foaf:name", "?name")
        .group_by("?dept")
        .order_by("?dept");
    let rows = q.execute(&ds).unwrap();
    assert_eq!(col(&rows, "dept"), [ex("eng"), ex("sales")]);
    assert_eq!(col(&rows, "n"), ["2", "2"]);
    assert_eq!(col(&rows, "total"), ["180", "160"]);
    assert_eq!(col(&rows, "avg"), ["90", "80"]);
    assert_eq!(col(&rows, "lo"), ["80", "70"]);
    assert_eq!(col(&rows, "hi"), ["100", "90"]);
    let names = col(&rows, "names");
    assert_eq!(
        sorted(names[0].split('|').map(String::from).collect()),
        ["Alice", "Bob"]
    );
    assert_eq!(rows.iter().filter(|r| r.get("one").is_some()).count(), 2);

    // Jena ARQ's statistical aggregates
    let stats = people()
        .select("?dept")
        .select_expr(expr::median(var("salary")), "?med")
        .select_expr(expr::mode(var("salary")), "?mode")
        .select_expr(expr::stdev_pop(var("salary")), "?sd")
        .select_expr(expr::var_pop(var("salary")).distinct(), "?vp")
        .select_expr(expr::stdev(var("salary")), "?sds")
        .select_expr(expr::variance(var("salary")), "?var")
        .where_("?p", "ex:dept", "?dept")
        .where_("?p", "ex:salary", "?salary")
        .group_by("?dept")
        .order_by("?dept");
    assert!(stats.build().unwrap().contains("VAR_POP(DISTINCT ?salary)"));
    let rows = stats.execute(&ds).unwrap();
    let num = |v: &str| -> Vec<f64> {
        col(&rows, v)
            .iter()
            .map(|x| x.parse::<f64>().unwrap())
            .collect()
    };
    assert_eq!(num("med"), [90.0, 80.0]);
    assert_eq!(num("sd"), [10.0, 10.0]);
    assert_eq!(num("vp"), [100.0, 100.0]);
    assert_eq!(num("var"), [200.0, 200.0]);

    let rows = q
        .clone()
        .having(expr::gt(expr::sum(var("salary")), 170))
        .execute(&ds)
        .unwrap();
    assert_eq!(col(&rows, "dept"), [ex("eng")]);

    // GROUP BY an expression with an alias, HAVING as a string, COUNT(DISTINCT ..).
    let rows = people()
        .select("?adult")
        .select_expr(expr::count_distinct(var("p")), "?n")
        .where_("?p", "foaf:age", "?age")
        .group_by_as(expr::ge(var("age"), 30), "?adult")
        .having("COUNT(?p) >= 1")
        .order_by("?adult")
        .execute(&ds)
        .unwrap();
    assert_eq!(col(&rows, "adult"), ["false", "true"]);
    assert_eq!(col(&rows, "n"), ["1", "2"]);
}

#[test]
fn exec_union_minus_graph_values_bind_subselect() {
    let ds = dataset();

    // UNION
    let rows = people()
        .select("?x")
        .union(
            |w| w.where_("?x", "a", "foaf:Organization"),
            |w| w.where_("?x", "foaf:mbox", "?m"),
        )
        .execute(&ds)
        .unwrap();
    assert_eq!(sorted(col(&rows, "x")), [ex("acme"), ex("carol")]);
    let rows = people()
        .select("?x")
        .union_of([
            WhereBuilder::new().where_("?x", "foaf:age", 25),
            WhereBuilder::new().where_("?x", "foaf:age", 34),
            WhereBuilder::new().where_("?x", "foaf:age", 41),
        ])
        .execute(&ds)
        .unwrap();
    assert_eq!(rows.len(), 3);

    // MINUS
    let rows = people()
        .select("?p")
        .where_("?p", "a", "foaf:Person")
        .minus(|w| w.where_("?p", "foaf:age", "?a"))
        .execute(&ds)
        .unwrap();
    assert_eq!(col(&rows, "p"), [ex("dave")]);

    // GRAPH with a variable and a fixed graph
    let rows = people()
        .select("?g")
        .select("?who")
        .graph("?g", |w| w.where_("?who", "ex:role", lit("user")))
        .order_by("?who")
        .execute(&ds)
        .unwrap();
    assert_eq!(col(&rows, "who"), [ex("bob"), ex("carol")]);
    assert_eq!(col(&rows, "g"), [ex("g1"), ex("g2")]);
    let rows = people()
        .select("?r")
        .graph("ex:g1", |w| w.where_("ex:alice", "ex:role", "?r"))
        .execute(&ds)
        .unwrap();
    assert_eq!(col(&rows, "r"), ["admin"]);

    // VALUES (single and multi-variable with UNDEF)
    let rows = people()
        .select("?name")
        .values("?p", ["ex:alice", "ex:carol"])
        .where_("?p", "foaf:name", "?name")
        .order_by("?name")
        .execute(&ds)
        .unwrap();
    assert_eq!(col(&rows, "name"), ["Alice", "Carol"]);
    let rows = people()
        .select("?p")
        .select("?name")
        .where_("?p", "foaf:name", "?name")
        .values_rows(
            ["?p", "?name"],
            [
                [node("ex:bob"), undef()],
                [undef(), lit("Dave")],
                [node("ex:bob"), lit("Nope")],
            ],
        )
        .order_by("?name")
        .execute(&ds)
        .unwrap();
    assert_eq!(col(&rows, "name"), ["Bob", "Dave"]);

    // BIND + FILTER EXISTS
    let rows = people()
        .select("?name")
        .select("?next")
        .where_("?p", "foaf:name", "?name")
        .where_("?p", "foaf:age", "?age")
        .bind(expr::add(var("age"), 1), "?next")
        .filter_exists(|w| w.where_("?p", "foaf:knows", "?someone"))
        .order_by("?name")
        .execute(&ds)
        .unwrap();
    assert_eq!(col(&rows, "name"), ["Alice", "Bob"]);
    assert_eq!(col(&rows, "next"), ["35", "26"]);

    // Sub-select: the best-paid person per department.
    let top = SelectBuilder::new()
        .select("?dept")
        .select_expr(expr::max(var("s")), "?max")
        .where_("?x", "ex:dept", "?dept")
        .where_("?x", "ex:salary", "?s")
        .group_by("?dept");
    let rows = people()
        .select("?name")
        .where_("?p", "ex:dept", "?dept")
        .where_("?p", "ex:salary", "?max")
        .where_("?p", "foaf:name", "?name")
        .sub_select(top)
        .order_by("?name")
        .execute(&ds)
        .unwrap();
    assert_eq!(col(&rows, "name"), ["Alice", "Carol"]);

    // Property paths
    let rows = people()
        .select("?x")
        .where_("ex:alice", "foaf:knows+", "?x")
        .execute(&ds)
        .unwrap();
    assert_eq!(sorted(col(&rows, "x")), [ex("bob"), ex("carol")]);
    let rows = people()
        .select("?x")
        .where_("ex:carol", "^foaf:knows/^foaf:knows", "?x")
        .execute(&ds)
        .unwrap();
    assert_eq!(col(&rows, "x"), [ex("alice")]);
}

#[test]
fn exec_set_var_prepared_query() {
    let ds = dataset();
    let by_dept = people()
        .select("?name")
        .where_("?p", "ex:dept", "?dept")
        .where_("?p", "foaf:name", "?name")
        .filter("?p != ?excluded")
        .order_by("?name");
    let rows = by_dept
        .clone()
        .set_var("?dept", iri(ex("eng")))
        .set_var("?excluded", iri(ex("bob")))
        .execute(&ds)
        .unwrap();
    assert_eq!(col(&rows, "name"), ["Alice"]);
    let rows = by_dept
        .set_var("?dept", "ex:sales")
        .set_var("?excluded", iri(ex("nobody")))
        .execute(&ds)
        .unwrap();
    assert_eq!(col(&rows, "name"), ["Carol", "Dave"]);
}

#[test]
fn exec_ask_construct_describe() {
    let ds = dataset();
    assert!(
        AskBuilder::new()
            .prefix("ex", EX)
            .where_("ex:alice", "foaf:knows+", "ex:carol")
            .execute(&ds)
            .unwrap()
    );
    assert!(
        !AskBuilder::new()
            .prefix("ex", EX)
            .where_("ex:carol", "foaf:knows", "?x")
            .execute(&ds)
            .unwrap()
    );
    assert!(
        AskBuilder::new()
            .where_("?p", "foaf:age", "?a")
            .filter(expr::gt(var("a"), 40))
            .execute(&ds)
            .unwrap()
    );

    let triples = ConstructBuilder::new()
        .prefix("ex", EX)
        .construct("?b", "ex:knownBy", "?a")
        .construct("?b", "a", "ex:Known")
        .where_("?a", "foaf:knows", "?b")
        .execute(&ds)
        .unwrap();
    assert_eq!(triples.len(), 4);
    let known_by = NamedNode::new_unchecked(ex("knownBy"));
    assert!(triples.contains(&Triple::new(
        NamedNode::new_unchecked(ex("bob")),
        known_by,
        NamedNode::new_unchecked(ex("alice")),
    )));

    let all = ConstructBuilder::new()
        .where_("?s", "foaf:name", "?o")
        .execute(&ds)
        .unwrap();
    assert_eq!(all.len(), 5);

    let d = DescribeBuilder::new()
        .describe(iri(ex("dave")))
        .execute(&ds)
        .unwrap();
    assert!(!d.is_empty());
}

#[test]
fn exec_updates() {
    let ds = dataset();
    let names = |ds: &Dataset| {
        col(
            &people()
                .select("?name")
                .where_("?p", "foaf:name", "?name")
                .order_by("?name")
                .execute(ds)
                .unwrap(),
            "name",
        )
    };

    // INSERT DATA (triples, graph, oxrdf triples)
    let stats = UpdateBuilder::new()
        .prefix("ex", EX)
        .insert_data("ex:erin", "a", "foaf:Person")
        .insert_data("ex:erin", "foaf:name", lit("Erin"))
        .insert_data("ex:erin", "foaf:age", 29)
        .insert_data_graph("ex:g2", "ex:erin", "ex:role", lit("user"))
        .insert_data_triples([Triple::new(
            NamedNode::new_unchecked(ex("erin")),
            NamedNode::new_unchecked(ex("salary")),
            Literal::from(60),
        )])
        .execute(&ds)
        .unwrap();
    assert_eq!(stats.inserted, 5);
    assert_eq!(
        names(&ds),
        ["ACME", "Alice", "Bob", "Carol", "Dave", "Erin"]
    );

    // DELETE { } INSERT { } WHERE { }: double the salaries in eng.
    UpdateBuilder::new()
        .prefix("ex", EX)
        .delete("?p", "ex:salary", "?s")
        .insert("?p", "ex:salary", "?new")
        .where_("?p", "ex:dept", "ex:eng")
        .where_("?p", "ex:salary", "?s")
        .bind(expr::mul(var("s"), 2), "?new")
        .execute(&ds)
        .unwrap();
    let rows = people()
        .select("?s")
        .where_("ex:alice", "ex:salary", "?s")
        .execute(&ds)
        .unwrap();
    assert_eq!(col(&rows, "s"), ["200"]);

    // Graph-scoped template + DELETE WHERE + multiple operations.
    UpdateBuilder::new()
        .prefix("ex", EX)
        .insert_graph("ex:archive", "?p", "foaf:name", "?n")
        .where_("?p", "foaf:name", "?n")
        .where_("?p", "ex:dept", "ex:sales")
        .delete_where("?p", "ex:dept", "ex:sales")
        .then()
        .delete_where("ex:acme", "?pp", "?oo")
        .execute(&ds)
        .unwrap();
    let rows = people()
        .select("?n")
        .graph("ex:archive", |w| w.where_("?p", "foaf:name", "?n"))
        .order_by("?n")
        .execute(&ds)
        .unwrap();
    assert_eq!(col(&rows, "n"), ["Carol", "Dave"]);
    assert!(
        !AskBuilder::new()
            .where_("?p", "<http://example.org/dept>", iri(ex("sales")))
            .execute(&ds)
            .unwrap()
    );
    assert_eq!(names(&ds), ["Alice", "Bob", "Carol", "Dave", "Erin"]);

    // WITH + DELETE DATA + CLEAR / DROP / CREATE / COPY
    UpdateBuilder::new()
        .prefix("ex", EX)
        .with("ex:g1")
        .delete("?p", "ex:role", "?r")
        .insert("?p", "ex:role", lit("guest"))
        .where_("?p", "ex:role", "?r")
        .delete_data(iri(ex("erin")), "foaf:age", 29)
        .copy("ex:g1", "ex:g1copy")
        .execute(&ds)
        .unwrap();
    let roles = people()
        .select("?r")
        .graph("ex:g1copy", |w| w.where_("?p", "ex:role", "?r"))
        .execute(&ds)
        .unwrap();
    assert_eq!(col(&roles, "r"), ["guest", "guest"]);
    assert!(
        !AskBuilder::new()
            .where_(iri(ex("erin")), "foaf:age", "?a")
            .execute(&ds)
            .unwrap()
    );
    UpdateBuilder::new()
        .drop(GraphTarget::graph(iri(ex("g1copy"))))
        .clear("NAMED")
        .execute(&ds)
        .unwrap();
    assert!(
        !AskBuilder::new()
            .graph("?g", |w| w.where_("?s", "?p", "?o"))
            .execute(&ds)
            .unwrap()
    );
    assert!(!names(&ds).is_empty());
}

#[test]
fn invalid_values_rows_are_errors_not_panics() {
    let empty: Vec<Vec<i32>> = vec![Vec::new()];
    for q in [
        // a single variable with an empty row
        SelectBuilder::new().values_rows(["?x"], empty.clone()),
        // an overlong row
        SelectBuilder::new().values_rows(["?x"], [[1, 2]]),
        // two variables, an empty row
        SelectBuilder::new().values_rows(["?a", "?b"], empty),
    ] {
        assert!(q.build().is_err());
    }
    // valid single-variable rows, including UNDEF, still render
    let ok = SelectBuilder::new()
        .values_rows(["?x"], [[node(1)], [undef()]])
        .build()
        .unwrap();
    assert!(ok.contains("VALUES ?x { 1 UNDEF }"), "{ok}");
}

//! Tests of the mapping engine (spec F19 §12, §13).

use super::*;
use std::collections::BTreeSet;

const CUSTOMERS: &str = "id,name,country\n1,Ann,NO\n2,Bob,\n";
const ORDERS: &str = "id,customer,total,placed\n10,1,99.50,2026-01-02\n11,1,5.00,2026-02-03\n12,2,12.00,2026-02-04\n";

fn files(dir: &Path, fs: &[(&str, &str)]) -> Inputs {
    for (n, body) in fs {
        std::fs::write(dir.join(n), body).unwrap();
    }
    Inputs {
        files: Vec::new(),
        dir: Some(dir.to_path_buf()),
        connections: Vec::new(),
    }
}

fn nquads(m: &Mapping, inputs: &Inputs, opts: &RunOptions) -> Result<BTreeSet<String>> {
    let mut out = Vec::new();
    write(m, inputs, opts, &mut out, RdfFormat::NQuads)?;
    Ok(String::from_utf8(out)
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect())
}

fn run_text(kind: MappingKind, text: &str, fs: &[(&str, &str)]) -> Result<BTreeSet<String>> {
    let dir = tempfile::tempdir().unwrap();
    let inputs = files(dir.path(), fs);
    let m = parse(text, kind, None)?;
    nquads(&m, &inputs, &RunOptions::default())
}

const SHOP_RML_CORE: &str = r#"
@prefix rml: <http://w3id.org/rml/> . @prefix ex: <http://ex.org/> .
<http://ex.org/map/C> a rml:TriplesMap ;
  rml:logicalSource [ rml:source [ a rml:RelativePathSource ; rml:path "customer.csv" ] ; rml:referenceFormulation rml:CSV ] ;
  rml:subjectMap [ rml:template "http://ex.org/customer/{id}" ; rml:class ex:Customer ] ;
  rml:predicateObjectMap [ rml:predicate ex:name ; rml:objectMap [ rml:reference "name" ] ] ,
                         [ rml:predicate ex:country ; rml:objectMap [ rml:template "http://ex.org/country/{country}" ] ] .
<http://ex.org/map/O> a rml:TriplesMap ;
  rml:logicalSource [ rml:source "orders.csv" ; rml:referenceFormulation rml:CSV ] ;
  rml:subjectMap [ rml:template "http://ex.org/order/{id}" ] ;
  rml:predicateObjectMap [ rml:predicate ex:total ; rml:objectMap [ rml:reference "total" ] ] ,
    [ rml:predicate ex:customer ; rml:objectMap [ rml:parentTriplesMap <http://ex.org/map/C> ;
        rml:joinCondition [ rml:child "customer" ; rml:parent "id" ] ] ] .
"#;

const SHOP_YARRRML: &str = r#"
prefixes:
  ex: "http://ex.org/"
mappings:
  C:
    sources: [customer.csv~csv]
    s: ex:customer/$(id)
    po:
      - [a, ex:Customer]
      - [ex:name, $(name)]
      - [ex:country, ex:country/$(country)~iri]
  O:
    sources:
      - access: orders.csv
        referenceFormulation: csv
    s: http://ex.org/order/$(id)
    po:
      - [ex:total, $(total)]
      - p: ex:customer
        o:
          mapping: C
          condition:
            function: equal
            parameters:
              - [str1, $(customer)]
              - [str2, $(id)]
"#;

fn shop_expected(typed: bool) -> BTreeSet<String> {
    let total = |v: &str| {
        if typed {
            format!("\"{v}\"^^<http://www.w3.org/2001/XMLSchema#decimal>")
        } else {
            format!("\"{v}\"")
        }
    };
    let (a, b, c) = if typed {
        ("99.5", "5.0", "12.0")
    } else {
        ("99.50", "5.00", "12.00")
    };
    [
        "<http://ex.org/customer/1> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://ex.org/Customer> .".to_string(),
        "<http://ex.org/customer/2> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://ex.org/Customer> .".to_string(),
        "<http://ex.org/customer/1> <http://ex.org/name> \"Ann\" .".to_string(),
        "<http://ex.org/customer/2> <http://ex.org/name> \"Bob\" .".to_string(),
        "<http://ex.org/customer/1> <http://ex.org/country> <http://ex.org/country/NO> .".to_string(),
        format!("<http://ex.org/order/10> <http://ex.org/total> {} .", total(a)),
        format!("<http://ex.org/order/11> <http://ex.org/total> {} .", total(b)),
        format!("<http://ex.org/order/12> <http://ex.org/total> {} .", total(c)),
        "<http://ex.org/order/10> <http://ex.org/customer> <http://ex.org/customer/1> .".to_string(),
        "<http://ex.org/order/11> <http://ex.org/customer> <http://ex.org/customer/1> .".to_string(),
        "<http://ex.org/order/12> <http://ex.org/customer> <http://ex.org/customer/2> .".to_string(),
    ]
    .into_iter()
    .collect()
}

#[test]
fn a2_rml_core_and_yarrrml_give_the_same_triples() {
    let fs = [("customer.csv", CUSTOMERS), ("orders.csv", ORDERS)];
    let core = run_text(MappingKind::Rml, SHOP_RML_CORE, &fs).unwrap();
    assert_eq!(core, shop_expected(false));
    let yaml = run_text(MappingKind::Yarrrml, SHOP_YARRRML, &fs).unwrap();
    assert_eq!(yaml, shop_expected(false));
    // a CSVW description of the orders types the totals
    let meta = r#"{"@context": "http://www.w3.org/ns/csvw", "tableSchema": {"columns": [
        {"name": "id", "datatype": "integer"}, {"name": "customer", "datatype": "integer"},
        {"name": "total", "datatype": "decimal"}, {"name": "placed", "datatype": "date"}]}}"#;
    let typed = run_text(
        MappingKind::Rml,
        SHOP_RML_CORE,
        &[
            ("customer.csv", CUSTOMERS),
            ("orders.csv", ORDERS),
            ("orders.csv-metadata.json", meta),
        ],
    )
    .unwrap();
    assert_eq!(typed, shop_expected(true));
}

#[test]
fn convert_prints_rml_core_that_reads_back_the_same() {
    let fs = [("customer.csv", CUSTOMERS), ("orders.csv", ORDERS)];
    let m = parse(SHOP_YARRRML, MappingKind::Yarrrml, None).unwrap();
    let ttl = rdf::to_turtle(&m, false).unwrap();
    assert!(ttl.contains("rml:parentTriplesMap"), "{ttl}");
    let back = run_text(MappingKind::Rml, &ttl, &fs).unwrap();
    assert_eq!(back, shop_expected(false), "{ttl}");
    // R2RML needs SQL sources
    assert!(rdf::to_turtle(&m, true).is_err());
}

#[test]
fn detects_kinds() {
    assert_eq!(detect(SHOP_RML_CORE), Some(MappingKind::Rml));
    assert_eq!(detect(SHOP_YARRRML), Some(MappingKind::Yarrrml));
    assert_eq!(
        detect(r#"{"@context": "http://www.w3.org/ns/csvw", "url": "x.csv"}"#),
        Some(MappingKind::Csvw)
    );
    assert_eq!(
        detect("PREFIX ex: <http://ex.org/> CONSTRUCT { ?s ex:p ?o } WHERE {}"),
        Some(MappingKind::Template)
    );
    assert_eq!(detect("hello"), None);
}

#[test]
fn a3_nulls_and_relative_iris() {
    let m = r#"@prefix rr: <http://www.w3.org/ns/r2rml#> . @prefix rml: <http://semweb.mmlab.be/ns/rml#> .
        @prefix ql: <http://semweb.mmlab.be/ns/ql#> . @prefix ex: <http://ex.org/> .
        <http://ex.org/map/O> rml:logicalSource [ rml:source "orders.csv" ; rml:referenceFormulation ql:CSV ] ;
          rr:subjectMap [ rr:template "http://ex.org/order/{id}" ] ;
          rr:predicateObjectMap [ rr:predicate ex:link ; rr:objectMap [ rr:template "rel/{customer}" ] ] ."#;
    let orders = "id,customer\n12,\n13,7\n";
    let e = run_text(MappingKind::Rml, m, &[("orders.csv", orders)])
        .expect_err("a relative IRI fails")
        .to_string();
    assert!(e.contains("<http://ex.org/map/O>"), "{e}");
    assert!(e.contains("row 3"), "{e}");
    assert!(e.contains("template \"rel/{customer}\""), "{e}");
    // with a base the IRI resolves, and the NULL customer gives no triple
    let dir = tempfile::tempdir().unwrap();
    let inputs = files(dir.path(), &[("orders.csv", orders)]);
    let mapping = parse(m, MappingKind::Rml, None).unwrap();
    let out = nquads(
        &mapping,
        &inputs,
        &RunOptions {
            base: Some("http://ex.org/base/".into()),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(
        out,
        [
            "<http://ex.org/order/13> <http://ex.org/link> <http://ex.org/base/rel/7> ."
                .to_string()
        ]
        .into_iter()
        .collect()
    );
}

#[test]
fn a4_json_with_multi_valued_references() {
    let m = r#"@prefix rml: <http://w3id.org/rml/> . @prefix ex: <http://ex.org/> .
        <http://ex.org/m> a rml:TriplesMap ;
          rml:logicalSource [ rml:source "people.json" ; rml:referenceFormulation rml:JSONPath ; rml:iterator "$.people[*]" ] ;
          rml:subjectMap [ rml:template "http://ex.org/p/{id}" ] ;
          rml:predicateObjectMap [ rml:predicate ex:tag ; rml:objectMap [ rml:reference "tags[*]" ] ] ,
                                 [ rml:predicate ex:id ; rml:objectMap [ rml:reference "id" ] ] ."#;
    let out = run_text(
        MappingKind::Rml,
        m,
        &[("people.json", r#"{"people":[{"id":7,"tags":["a","b"]}]}"#)],
    )
    .unwrap();
    assert_eq!(
        out,
        [
            "<http://ex.org/p/7> <http://ex.org/tag> \"a\" .",
            "<http://ex.org/p/7> <http://ex.org/tag> \"b\" .",
            "<http://ex.org/p/7> <http://ex.org/id> \"7\"^^<http://www.w3.org/2001/XMLSchema#integer> .",
        ]
        .into_iter()
        .map(str::to_string)
        .collect()
    );
    // JSON Lines, one line at a time
    let jl = m
        .replace("people.json", "people.jsonl")
        .replace("$.people[*]", "$");
    let out = run_text(
        MappingKind::Rml,
        &jl,
        &[(
            "people.jsonl",
            "{\"id\":1,\"tags\":[\"x\"]}\n\n{\"id\":2,\"tags\":[]}\n",
        )],
    )
    .unwrap();
    assert_eq!(out.len(), 3, "{out:?}");
}

#[test]
fn a6_in_memory_join_over_the_budget_fails_and_names_the_join() {
    let dir = tempfile::tempdir().unwrap();
    let mut customers = String::from("id,name,country\n");
    for i in 0..2000 {
        customers.push_str(&format!("{i},n{i},NO\n"));
    }
    let inputs = files(
        dir.path(),
        &[("customer.csv", &customers), ("orders.csv", ORDERS)],
    );
    let m = parse(SHOP_RML_CORE, MappingKind::Rml, None).unwrap();
    let e = nquads(
        &m,
        &inputs,
        &RunOptions {
            memory_budget: Some(16 << 10),
            ..Default::default()
        },
    )
    .expect_err("over the budget")
    .to_string();
    assert!(
        e.contains("the join of <http://ex.org/map/O> with <http://ex.org/map/C>"),
        "{e}"
    );
    assert!(e.contains("parent rows"), "{e}");
    assert!(nquads(&m, &inputs, &RunOptions::default()).is_ok());
}

#[test]
fn refusals() {
    // mixed vocabularies
    let mixed = r#"@prefix rml: <http://w3id.org/rml/> . @prefix rr: <http://www.w3.org/ns/r2rml#> .
        <http://ex.org/m> rml:logicalSource [ rml:source "a.csv" ] ; rr:subjectMap [ rr:template "http://e/{a}" ] ."#;
    assert!(
        parse(mixed, MappingKind::Rml, None)
            .unwrap_err()
            .to_string()
            .contains("mixes")
    );
    // XPath
    let xml = r#"@prefix rr: <http://www.w3.org/ns/r2rml#> . @prefix rml: <http://semweb.mmlab.be/ns/rml#> .
        @prefix ql: <http://semweb.mmlab.be/ns/ql#> .
        <http://ex.org/m> rml:logicalSource [ rml:source "a.xml" ; rml:referenceFormulation ql:XPath ; rml:iterator "/a" ] ;
          rr:subjectMap [ rr:template "http://e/{a}" ] ."#;
    assert!(
        parse(xml, MappingKind::Rml, None)
            .unwrap_err()
            .to_string()
            .contains("XPath")
    );
    // credentials in a mapping
    let d2rq = r#"@prefix rr: <http://www.w3.org/ns/r2rml#> . @prefix rml: <http://semweb.mmlab.be/ns/rml#> .
        @prefix d2rq: <http://www.wiwiss.fu-berlin.de/suhl/bizer/D2RQ/0.1#> .
        <#DB> a d2rq:Database ; d2rq:jdbcDSN "jdbc:x" ; d2rq:password "secret" .
        <http://ex.org/m> rml:logicalSource [ rml:source <#DB> ; rr:tableName "t" ] ;
          rr:subjectMap [ rr:template "http://e/{a}" ] ."#;
    let e = parse(d2rq, MappingKind::Rml, Some("http://ex.org/m.ttl"))
        .unwrap_err()
        .to_string();
    assert!(e.contains("password"), "{e}");
    // an unknown function
    let f = r#"@prefix rml: <http://w3id.org/rml/> .
        <http://ex.org/m> rml:logicalSource [ rml:source "a.csv" ; rml:referenceFormulation rml:CSV ] ;
          rml:subjectMap [ rml:template "http://e/{a}" ] ;
          rml:predicateObjectMap [ rml:predicate <http://e/p> ; rml:objectMap [ rml:functionExecution [ rml:function <http://ex.org/nope> ] ] ] ."#;
    assert!(
        parse(f, MappingKind::Rml, None)
            .unwrap_err()
            .to_string()
            .contains("CONSTRUCT")
    );
    // an unknown YARRRML key, with its path
    let y = "mappings:\n  a:\n    sources: [a.csv~csv]\n    s: http://e/$(a)\n    bogus: 1\n";
    let e = parse(y, MappingKind::Yarrrml, None)
        .unwrap_err()
        .to_string();
    assert!(e.contains("$.mappings.a.bogus"), "{e}");
    // a missing file
    let e = run_text(
        MappingKind::Rml,
        SHOP_RML_CORE,
        &[("customer.csv", CUSTOMERS)],
    )
    .unwrap_err()
    .to_string();
    assert!(e.contains("mapping-source-missing"), "{e}");
}

#[test]
fn functions_languages_datatypes_and_graphs() {
    let m = r#"@prefix rr: <http://www.w3.org/ns/r2rml#> . @prefix rml: <http://semweb.mmlab.be/ns/rml#> .
        @prefix ql: <http://semweb.mmlab.be/ns/ql#> . @prefix fnml: <http://semweb.mmlab.be/ns/fnml#> .
        @prefix fno: <https://w3id.org/function/ontology#> . @prefix grel: <http://users.ugent.be/~bjdmeest/function/grel.ttl#> .
        @prefix xsd: <http://www.w3.org/2001/XMLSchema#> .
        <http://ex.org/m> rml:logicalSource [ rml:source "p.csv" ; rml:referenceFormulation ql:CSV ] ;
          rr:subjectMap [ rr:template "http://e/{id}" ; rr:graph <http://e/g> ] ;
          rr:predicateObjectMap [ rr:predicate <http://e/up> ; rr:objectMap [ fnml:functionValue [
              rr:predicateObjectMap [ rr:predicate fno:executes ; rr:objectMap [ rr:constant grel:toUpperCase ] ] ,
                                    [ rr:predicate grel:valueParameter ; rr:objectMap [ rml:reference "name" ] ] ] ] ] ,
            [ rr:predicate <http://e/name> ; rr:objectMap [ rml:reference "name" ; rr:language "en" ] ] ,
            [ rr:predicate <http://e/age> ; rr:objectMap [ rml:reference "age" ; rr:datatype xsd:integer ] ;
              rr:graphMap [ rr:constant rr:defaultGraph ] ] ."#;
    let out = run_text(MappingKind::Rml, m, &[("p.csv", "id,name,age\n1,ann,30\n")]).unwrap();
    let want: BTreeSet<String> = [
        "<http://e/1> <http://e/up> \"ANN\" <http://e/g> .",
        "<http://e/1> <http://e/name> \"ann\"@en <http://e/g> .",
        "<http://e/1> <http://e/age> \"30\"^^<http://www.w3.org/2001/XMLSchema#integer> <http://e/g> .",
        "<http://e/1> <http://e/age> \"30\"^^<http://www.w3.org/2001/XMLSchema#integer> .",
    ]
    .into_iter()
    .map(str::to_string)
    .collect();
    assert_eq!(out, want);
    // an invalid literal for its datatype is a data error
    let e = run_text(
        MappingKind::Rml,
        m,
        &[("p.csv", "id,name,age\n1,ann,old\n")],
    )
    .unwrap_err()
    .to_string();
    assert!(e.contains("not a valid literal"), "{e}");
    // a YARRRML function and a language map
    let y = r#"
prefixes:
  ex: "http://e/"
mappings:
  p:
    sources: [p.csv~csv]
    s: ex:$(id)
    po:
      - p: ex:up
        o:
          function: grel:toUpperCase
          parameters:
            - [grel:valueParameter, $(name)]
      - [ex:name, $(name), $(lang)~lang]
"#;
    let out = run_text(
        MappingKind::Yarrrml,
        y,
        &[("p.csv", "id,name,lang\n1,ann,nb\n")],
    )
    .unwrap();
    assert!(
        out.contains("<http://e/1> <http://e/up> \"ANN\" ."),
        "{out:?}"
    );
    assert!(
        out.contains("<http://e/1> <http://e/name> \"ann\"@nb ."),
        "{out:?}"
    );
}

#[test]
fn only_graph_redirects_the_default_graph_and_refuses_others() {
    let dir = tempfile::tempdir().unwrap();
    let inputs = files(
        dir.path(),
        &[("customer.csv", CUSTOMERS), ("orders.csv", ORDERS)],
    );
    let m = parse(SHOP_RML_CORE, MappingKind::Rml, None).unwrap();
    let g = oxrdf::NamedNode::new("urn:g").unwrap();
    let out = nquads(
        &m,
        &inputs,
        &RunOptions {
            only_graph: Some(g),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(out.iter().all(|l| l.ends_with("<urn:g> .")), "{out:?}");
    let other = SHOP_RML_CORE.replace(
        "rml:class ex:Customer ]",
        "rml:class ex:Customer ; rml:graph <urn:other> ]",
    );
    let m = parse(&other, MappingKind::Rml, None).unwrap();
    let e = nquads(
        &m,
        &inputs,
        &RunOptions {
            only_graph: Some(oxrdf::NamedNode::new("urn:g").unwrap()),
            ..Default::default()
        },
    )
    .unwrap_err()
    .to_string();
    assert!(e.contains("mapping-graph-conflict"), "{e}");
    assert_eq!(
        m.constant_graphs(),
        Some(vec![
            Some(oxrdf::NamedNode::new("urn:other").unwrap()),
            None
        ])
    );
}

#[test]
fn a_large_csv_streams() {
    // 200,000 rows go through without holding the table
    let dir = tempfile::tempdir().unwrap();
    let mut f = std::io::BufWriter::new(std::fs::File::create(dir.path().join("big.csv")).unwrap());
    writeln!(f, "id,name").unwrap();
    for i in 0..200_000 {
        writeln!(f, "{i},n{i}").unwrap();
    }
    drop(f);
    let inputs = Inputs {
        dir: Some(dir.path().to_path_buf()),
        ..Default::default()
    };
    let y = "mappings:\n  a:\n    sources: [big.csv~csv]\n    s: http://e/$(id)\n    po:\n      - [http://e/name, $(name)]\n";
    let m = parse(y, MappingKind::Yarrrml, None).unwrap();
    let mut n = 0u64;
    let stats = run(&m, &inputs, &RunOptions::default(), &mut |_| {
        n += 1;
        Ok(())
    })
    .unwrap();
    assert_eq!(n, 200_000);
    assert_eq!(stats.quads, 200_000);
    assert_eq!(stats.rows, 200_000);
}

#[cfg(feature = "mapping-sqlite")]
mod sqlite {
    use super::*;

    const SHOP_R2RML: &str = r#"
@prefix rr: <http://www.w3.org/ns/r2rml#> . @prefix ex: <http://ex.org/> .
<http://ex.org/map/C> rr:logicalTable [ rr:tableName "customer" ] ;
  rr:subjectMap [ rr:template "http://ex.org/customer/{id}" ; rr:class ex:Customer ] ;
  rr:predicateObjectMap [ rr:predicate ex:name ; rr:objectMap [ rr:column "name" ] ] ,
                        [ rr:predicate ex:country ; rr:objectMap [ rr:template "http://ex.org/country/{country}" ] ] .
<http://ex.org/map/O> rr:logicalTable [ rr:tableName "orders" ] ;
  rr:subjectMap [ rr:template "http://ex.org/order/{id}" ] ;
  rr:predicateObjectMap [ rr:predicate ex:total ; rr:objectMap [ rr:column "total" ] ] ,
    [ rr:predicate ex:customer ; rr:objectMap [ rr:parentTriplesMap <http://ex.org/map/C> ;
        rr:joinCondition [ rr:child "customer" ; rr:parent "id" ] ] ] .
"#;

    fn shop_db(dir: &Path) -> PathBuf {
        let p = dir.join("shop.db");
        let c = rusqlite::Connection::open(&p).unwrap();
        c.execute_batch(
            "CREATE TABLE customer (id int PRIMARY KEY, name text NOT NULL, country char(2));
             CREATE TABLE orders (id int PRIMARY KEY, customer int REFERENCES customer, total numeric(10,2), placed date);
             INSERT INTO customer VALUES (1, 'Ann', 'NO'), (2, 'Bob', NULL);
             INSERT INTO orders VALUES (10, 1, 99.50, '2026-01-02'), (11, 1, 5.00, '2026-02-03'), (12, 2, 12.00, '2026-02-04');",
        )
        .unwrap();
        p
    }

    #[test]
    fn a1_r2rml_over_sqlite_with_the_join_in_sql() {
        let dir = tempfile::tempdir().unwrap();
        let db = shop_db(dir.path());
        let inputs = Inputs {
            connections: vec![NamedConnection {
                name: "shop".into(),
                connection: Connection::Sqlite { path: db },
                jdbc_dsn: None,
            }],
            ..Default::default()
        };
        let m = parse(SHOP_R2RML, MappingKind::Rml, None).unwrap();
        assert_eq!(m.vocabulary, model::Vocabulary::R2rml);
        let joins = exec::describe_joins(&m);
        assert!(joins[0].contains("one SQL statement"), "{joins:?}");
        let out = nquads(&m, &inputs, &RunOptions::default()).unwrap();
        assert_eq!(out, shop_expected(true));
        // the same mapping as R2RML Turtle again
        let ttl = rdf::to_turtle(&m, true).unwrap();
        let back = parse(&ttl, MappingKind::Rml, None).unwrap();
        assert_eq!(
            nquads(&back, &inputs, &RunOptions::default()).unwrap(),
            shop_expected(true)
        );
    }

    #[test]
    fn a_connection_iri_and_a_query() {
        let dir = tempfile::tempdir().unwrap();
        let db = shop_db(dir.path());
        let y = r#"
mappings:
  big:
    sources:
      - access: urn:x-sparkles:connection:shop
        query: SELECT id, total * 2 AS doubled FROM orders WHERE total > 6
    s: http://ex.org/order/$(id)
    po:
      - [http://ex.org/doubled, $(doubled)]
"#;
        let inputs = Inputs {
            connections: vec![NamedConnection {
                name: "shop".into(),
                connection: Connection::Sqlite { path: db },
                jdbc_dsn: None,
            }],
            ..Default::default()
        };
        let m = parse(y, MappingKind::Yarrrml, None).unwrap();
        let out = nquads(&m, &inputs, &RunOptions::default()).unwrap();
        assert_eq!(out.len(), 2, "{out:?}");
        assert!(
            out.contains("<http://ex.org/order/10> <http://ex.org/doubled> \"1.99E2\"^^<http://www.w3.org/2001/XMLSchema#double> ."),
            "{out:?}"
        );
        // a missing connection
        let e = nquads(&m, &Inputs::default(), &RunOptions::default())
            .unwrap_err()
            .to_string();
        assert!(e.contains("mapping-connection-missing"), "{e}");
    }
}

#[cfg(feature = "mapping-parquet")]
mod parquet_source {
    use super::*;
    use parquet::data_type::{ByteArray, ByteArrayType, Int64Type};
    use parquet::file::properties::WriterProperties;
    use parquet::file::writer::SerializedFileWriter;
    use parquet::schema::parser::parse_message_type;
    use std::sync::Arc;

    /// A Parquet file of `groups` row groups of `per` rows: `id` and `name`.
    pub(crate) fn write_parquet(path: &Path, groups: usize, per: usize) {
        let schema = Arc::new(
            parse_message_type("message t { REQUIRED INT64 id; OPTIONAL BYTE_ARRAY name (UTF8); }")
                .unwrap(),
        );
        let file = std::fs::File::create(path).unwrap();
        let mut w =
            SerializedFileWriter::new(file, schema, Arc::new(WriterProperties::builder().build()))
                .unwrap();
        for g in 0..groups {
            let mut rg = w.next_row_group().unwrap();
            let ids: Vec<i64> = (0..per).map(|i| (g * per + i) as i64).collect();
            let mut col = rg.next_column().unwrap().unwrap();
            col.typed::<Int64Type>()
                .write_batch(&ids, None, None)
                .unwrap();
            col.close().unwrap();
            // every tenth name is NULL
            let names: Vec<ByteArray> = ids
                .iter()
                .filter(|i| *i % 10 != 0)
                .map(|i| ByteArray::from(format!("n{i}").as_str()))
                .collect();
            let defs: Vec<i16> = ids
                .iter()
                .map(|i| if i % 10 == 0 { 0 } else { 1 })
                .collect();
            let mut col = rg.next_column().unwrap().unwrap();
            col.typed::<ByteArrayType>()
                .write_batch(&names, Some(&defs), None)
                .unwrap();
            col.close().unwrap();
            rg.close().unwrap();
        }
        w.close().unwrap();
    }

    /// The file of acceptance example A5, 10 million rows in 100 row groups, written to
    /// `$SPARKLES_A5_PARQUET` for a measurement of `sparkles map run` (spec F19 §12).
    #[test]
    #[ignore = "writes a large file for a measurement"]
    fn write_a5_parquet() {
        let path = std::env::var("SPARKLES_A5_PARQUET").expect("SPARKLES_A5_PARQUET");
        write_parquet(Path::new(&path), 100, 100_000);
    }

    #[test]
    fn parquet_rows_map_with_natural_types_and_nulls() {
        let dir = tempfile::tempdir().unwrap();
        write_parquet(&dir.path().join("t.parquet"), 2, 15);
        let y = "mappings:\n  t:\n    sources: [t.parquet]\n    s: http://e/$(id)\n    po:\n      - [http://e/id, $(id)]\n      - [http://e/name, $(name)]\n";
        let inputs = Inputs {
            dir: Some(dir.path().to_path_buf()),
            ..Default::default()
        };
        let m = parse(y, MappingKind::Yarrrml, None).unwrap();
        let out = nquads(&m, &inputs, &RunOptions::default()).unwrap();
        // 30 ids, 27 names
        assert_eq!(out.len(), 57);
        assert!(out.contains(
            "<http://e/29> <http://e/id> \"29\"^^<http://www.w3.org/2001/XMLSchema#integer> ."
        ));
        assert!(out.contains("<http://e/29> <http://e/name> \"n29\" ."));
        assert!(
            !out.iter()
                .any(|l| l.starts_with("<http://e/20> <http://e/name>"))
        );
    }
}

use std::io::Write as _;

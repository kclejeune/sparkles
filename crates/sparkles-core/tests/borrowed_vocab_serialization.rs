use oxrdf::{GraphName, Literal, NamedNode, Quad, Term};
use sparkles_core::id::Id;
use sparkles_core::io::{RdfFormat, Source};
use sparkles_core::sparql::results::{SolutionsFormat, sparkles_json, term_json, write_solutions};
use sparkles_core::sparql::{QueryOptions, query};
use sparkles_core::store::{Store, StoreOptions};

#[test]
fn borrowed_base_terms_preserve_json_tsv_and_other_id_branches() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path(), StoreOptions::default()).unwrap();
    store.load(&[Source::from_bytes(
        b"<urn:s0> <urn:p> <urn:o> .\n<urn:s1> <urn:p> \"caf\xc3\xa9\"@fr .\n<urn:s2> <urn:p> \"custom\"^^<urn:datatype> .\n".to_vec(),
        RdfFormat::NTriples, None,
    )]).unwrap();
    let snap = store.snapshot();
    let terms = [
        Term::NamedNode(NamedNode::new_unchecked("urn:o")),
        Term::Literal(Literal::new_language_tagged_literal_unchecked("café", "fr")),
        Term::Literal(Literal::new_typed_literal(
            "custom",
            NamedNode::new_unchecked("urn:datatype"),
        )),
    ];
    for term in &terms {
        let key = sparkles_core::id::term_key(term);
        let id = Id::vocab(snap.generation.vocab.find(&key).unwrap());
        assert_eq!(snap.term(id), Some(term.clone()));
    }
    assert_eq!(snap.term(Id::vocab(u64::MAX >> 4)), None);
    assert_eq!(
        snap.term(Id::from_i64(42).unwrap()),
        Some(Term::Literal(Literal::from(42)))
    );
    let result = query(snap, "SELECT ?s ?o ?missing WHERE { ?s <urn:p> ?o OPTIONAL { ?s <urn:missing> ?missing } } ORDER BY ?s", &QueryOptions::default()).unwrap();
    let rich = sparkles_json(&result, None);
    for (i, term) in terms.iter().enumerate() {
        assert_eq!(rich["rows"][i][1], term_json(term));
        assert!(rich["rows"][i][2].is_null());
    }
    for fmt in [SolutionsFormat::Json, SolutionsFormat::Tsv] {
        let mut out = Vec::new();
        write_solutions(&result, fmt, &mut out, None).unwrap();
        if fmt == SolutionsFormat::Json {
            let value: serde_json::Value = serde_json::from_slice(&out).unwrap();
            assert_eq!(value["results"]["bindings"].as_array().unwrap().len(), 3);
            assert_eq!(value["results"]["bindings"][1]["o"]["xml:lang"], "fr");
            assert_eq!(
                value["results"]["bindings"][2]["o"]["datatype"],
                "urn:datatype"
            );
        } else {
            assert_eq!(
                String::from_utf8(out).unwrap(),
                "?s\t?o\t?missing\n<urn:s0>\t<urn:o>\t\n<urn:s1>\t\"café\"@fr\t\n<urn:s2>\t\"custom\"^^<urn:datatype>\t\n"
            );
        }
    }
    assert_eq!(
        sparkles_json(&result, Some(1))["rows"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    // Delta IDs and query-local values retain their existing decoding route.
    let delta = Term::Literal(Literal::new_simple_literal("later vocabulary"));
    let mut tx = store.write();
    let ids = tx
        .encode_quad(
            &Quad::new(
                NamedNode::new_unchecked("urn:later"),
                NamedNode::new_unchecked("urn:q"),
                delta.clone(),
                GraphName::DefaultGraph,
            ),
            &mut std::collections::HashMap::new(),
        )
        .unwrap();
    tx.insert(ids).unwrap();
    tx.commit().unwrap();
    assert_eq!(store.snapshot().term(ids[2]), Some(delta));
    let local = query(
        store.snapshot(),
        "SELECT (CONCAT(\"computed\", \" value\") AS ?v) WHERE {}",
        &QueryOptions::default(),
    )
    .unwrap();
    assert_eq!(
        sparkles_json(&local, None)["rows"][0][0]["value"],
        "computed value"
    );
}

#[test]
fn sorted_vocabulary_contains_count_preserves_exact_result() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path(), StoreOptions::default()).unwrap();
    let n = 8192;
    let data: String = (0..n)
        .map(|i| {
            format!(
                "<urn:person:{i}> <urn:name> \"{} {i:05}\" .\n",
                if i % 3 == 0 { "Ada" } else { "Bob" },
            )
        })
        .collect();
    store
        .load(&[Source::from_bytes(
            data.into_bytes(),
            RdfFormat::NTriples,
            None,
        )])
        .unwrap();
    let result = query(
        store.snapshot(),
        "SELECT (COUNT(*) AS ?c) WHERE { ?s <urn:name> ?name FILTER(CONTAINS(?name, \"Ada\")) }",
        &QueryOptions::default(),
    )
    .unwrap();
    assert_eq!(result.len(), 1);
    let value = sparkles_json(&result, None);
    assert_eq!(value["rows"][0][0]["value"], ((n + 2) / 3).to_string());
    let mut tsv = Vec::new();
    write_solutions(&result, SolutionsFormat::Tsv, &mut tsv, None).unwrap();
    assert_eq!(
        String::from_utf8(tsv).unwrap(),
        format!("?c\n{}\n", (n + 2) / 3)
    );
}

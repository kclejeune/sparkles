//! Conformance of the mapping engine (spec F19 §13.1) against the vendored W3C R2RML
//! test cases (run on SQLite, feature `mapping-sqlite`) and the RML-Core test cases of
//! the W3C Knowledge Graph Construction Community Group, under `testsuite/mapping/`.
//!
//! Each case's output is compared with the expected N-Quads by dataset isomorphism, and
//! a case that expects an error must fail to parse or run. Excluded cases are listed with
//! a reason. The test fails when a case outside the list fails, and when a listed case
//! passes, so the list stays honest.

use oxrdf::dataset::CanonicalizationAlgorithm;
use oxrdf::{Dataset, NamedOrBlankNode, Term};
use sparkles_core::mapping::{self, Inputs, MappingKind, RunOptions};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

fn suite(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../testsuite/mapping")
        .join(name)
}

/// The subjects and their (predicate, object) pairs of a manifest.
fn manifest(path: &Path, base: &str) -> HashMap<NamedOrBlankNode, Vec<(String, Term)>> {
    let text = std::fs::read_to_string(path).unwrap();
    let mut out: HashMap<NamedOrBlankNode, Vec<(String, Term)>> = HashMap::new();
    for t in oxttl::TurtleParser::new()
        .with_base_iri(base)
        .unwrap()
        .for_slice(&text)
    {
        let t = t.unwrap();
        out.entry(t.subject)
            .or_default()
            .push((t.predicate.as_str().to_string(), t.object));
    }
    out
}

fn lit(props: &[(String, Term)], p: &str) -> Option<String> {
    props.iter().find(|(q, _)| q == p).map(|(_, o)| match o {
        Term::Literal(l) => l.value().to_string(),
        Term::NamedNode(n) => n.as_str().to_string(),
        o => o.to_string(),
    })
}

fn obj<'a>(props: &'a [(String, Term)], p: &str) -> Option<&'a Term> {
    props.iter().find(|(q, _)| q == p).map(|(_, o)| o)
}

fn dataset(nq: &[u8]) -> Result<Dataset, String> {
    let mut d = Dataset::new();
    for q in oxttl::NQuadsParser::new().for_slice(nq) {
        d.insert(&q.map_err(|e| e.to_string())?);
    }
    d.canonicalize(CanonicalizationAlgorithm::Unstable);
    Ok(d)
}

/// Run one case: `Ok(())` when its outcome matches the expectation.
fn check(
    mapping_text: &str,
    inputs: &Inputs,
    base: &str,
    expected: Option<&[u8]>,
) -> Result<(), String> {
    let opts = RunOptions {
        base: Some(base.to_string()),
        ..Default::default()
    };
    let got = mapping::parse(mapping_text, MappingKind::Rml, Some(base)).and_then(|m| {
        let mut out = Vec::new();
        mapping::write(&m, inputs, &opts, &mut out, oxrdfio::RdfFormat::NQuads)?;
        Ok(out)
    });
    match (got, expected) {
        (Err(_), None) => Ok(()),
        (Ok(out), None) => Err(format!(
            "an error was expected, and the run gave:\n{}",
            String::from_utf8_lossy(&out)
        )),
        (Err(e), Some(_)) => Err(format!("failed: {e}")),
        (Ok(out), Some(want)) => {
            let (g, w) = (dataset(&out)?, dataset(want)?);
            if g == w {
                Ok(())
            } else {
                Err(format!(
                    "different output\n--- got\n{}--- expected\n{}",
                    String::from_utf8_lossy(&out),
                    String::from_utf8_lossy(want)
                ))
            }
        }
    }
}

/// Compare the outcomes with the exclusions and print a summary.
fn judge(suite: &str, results: Vec<(String, Result<(), String>)>, excluded: &[(&str, &str)]) {
    let excluded: HashMap<&str, &str> = excluded.iter().copied().collect();
    let mut passed = 0;
    let mut failures = Vec::new();
    let mut unexpected = Vec::new();
    for (id, r) in &results {
        match (r, excluded.get(id.as_str())) {
            (Ok(()), None) => passed += 1,
            (Ok(()), Some(_)) => unexpected.push(id.clone()),
            (Err(e), None) => failures.push(format!("{id}: {e}")),
            (Err(_), Some(_)) => {}
        }
    }
    for id in excluded.keys() {
        assert!(
            results.iter().any(|(r, _)| r == id),
            "{suite}: the excluded case {id} does not exist"
        );
    }
    eprintln!(
        "{suite}: {passed} passed, {} excluded, {} failed, {} total",
        excluded.len(),
        failures.len(),
        results.len()
    );
    assert!(
        failures.is_empty(),
        "{suite}: {} cases failed:\n\n{}",
        failures.len(),
        failures.join("\n\n")
    );
    assert!(
        unexpected.is_empty(),
        "{suite}: excluded cases now pass, remove them from the list: {unexpected:?}"
    );
}

const TD: &str = "http://www.w3.org/2006/03/test-description#";
const RT: &str = "http://w3id.org/rml/test/";
const DCT: &str = "http://purl.org/dc/terms/";

/// RML-Core cases that are excluded, with the reason.
const RML_CORE_EXCLUDED: &[(&str, &str)] = &[
    (
        "RMLTC0027b-JSON",
        "rml:UnsafeIRI generates IRIs with spaces, which are not valid IRIs and which no Sparkles store can hold; the expected output is not even valid N-Quads",
    ),
    (
        "RMLTC0030b-JSON",
        "join conditions over templates (RML-Core expression maps as rml:childMap and rml:parentMap) are not built: join conditions compare references",
    ),
    (
        "RMLTC0030c-JSON",
        "a join condition with a constant parent map is not built",
    ),
    (
        "RMLTC0030d-JSON",
        "join conditions over a template and a constant are not built",
    ),
    (
        "RMLTC0030e-JSON",
        "a join condition with a constant child map is not built",
    ),
    (
        "RMLTC0030f-JSON",
        "join conditions over a constant and a template are not built",
    ),
];

#[test]
fn rml_core_test_cases() {
    let dir = suite("rml-core");
    let m = manifest(&dir.join("manifest.ttl"), "http://w3id.org/rml/core/test/");
    let mut results = Vec::new();
    for props in m.values() {
        let is_case = props.iter().any(|(p, o)| {
            p == "http://www.w3.org/1999/02/22-rdf-syntax-ns#type"
                && matches!(o, Term::NamedNode(n) if n.as_str() == format!("{TD}TestCase"))
        });
        if !is_case {
            continue;
        }
        let id = lit(props, &format!("{DCT}identifier")).unwrap();
        let case = dir.join(&id);
        let has_error = lit(props, &format!("{RT}hasError")).as_deref() == Some("true");
        let base =
            lit(props, &format!("{RT}defaultBaseIRI")).unwrap_or("http://example.com/".into());
        let mapping = std::fs::read_to_string(
            case.join(lit(props, &format!("{RT}mappingDocument")).unwrap_or("mapping.ttl".into())),
        )
        .unwrap();
        let expected = if has_error {
            None
        } else {
            Some(std::fs::read(case.join("output.nq")).unwrap())
        };
        let inputs = Inputs {
            dir: Some(case.clone()),
            ..Default::default()
        };
        results.push((id, check(&mapping, &inputs, &base, expected.as_deref())));
    }
    results.sort_by(|a, b| a.0.cmp(&b.0));
    assert!(results.len() > 70, "{} cases", results.len());
    judge("RML-Core", results, RML_CORE_EXCLUDED);
}

#[cfg(feature = "mapping-sqlite")]
mod r2rml {
    use super::*;
    use sparkles_core::mapping::{Connection, NamedConnection};

    const RDB: &str = "http://purl.org/NET/rdb2rdf-test#";

    /// R2RML cases that are excluded, with the reason.
    const EXCLUDED: &[(&str, &str)] = &[
        (
            "R2RMLTC0002f",
            "SQLite matches identifiers without regard to case, so the regular identifier Name finds the column \"Name\"; the SQL standard folds it to NAME, which does not",
        ),
        (
            "R2RMLTC0015b",
            "\"english\" is a well-formed BCP 47 tag (a five to eight letter primary subtag); the case wants a check against the language subtag registry, which Sparkles does not make",
        ),
        (
            "R2RMLTC0018a",
            "the expected output relies on CHAR(15) blank padding, which SQLite does not do, and lists one of the table's three rows",
        ),
    ];

    /// A database script in SQLite's dialect: SQLite has no `DROP … CASCADE`.
    fn sqlite_script(sql: &str) -> String {
        sql.lines()
            .map(|l| {
                let t = l.trim_end();
                let lower = t.to_ascii_lowercase();
                if lower.starts_with("drop table") {
                    let cut = lower.find(" cascade").unwrap_or(t.len());
                    let mut s = t[..cut].trim_end_matches(';').to_string();
                    s.push(';');
                    s
                } else {
                    t.to_string()
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn r2rml_test_cases() {
        let dir = suite("r2rml");
        let m = manifest(
            &dir.join("manifest.ttl"),
            "http://www.w3.org/2001/sw/rdb2rdf/test-cases/",
        );
        let mut results = Vec::new();
        let tmp = tempfile::tempdir().unwrap();
        for props in m.values() {
            let is_case = props.iter().any(|(p, o)| {
                p == "http://www.w3.org/1999/02/22-rdf-syntax-ns#type"
                    && matches!(o, Term::NamedNode(n) if n.as_str() == format!("{RDB}R2RML"))
            });
            if !is_case {
                continue;
            }
            let id = lit(props, &format!("{DCT}identifier")).unwrap();
            let case = dir.join(&id);
            if !case.is_dir() {
                continue;
            }
            let Some(Term::NamedNode(db)) = obj(props, &format!("{RDB}database")) else {
                panic!("{id}: no database");
            };
            let db_props = m
                .get(&NamedOrBlankNode::NamedNode(db.clone()))
                .unwrap_or_else(|| panic!("{id}: no database description"));
            let script = lit(db_props, &format!("{RDB}sqlScriptFile")).unwrap();
            let sql = std::fs::read_to_string(dir.join("databases").join(&script)).unwrap();
            let path = tmp.path().join(format!("{id}.db"));
            let c = rusqlite::Connection::open(&path).unwrap();
            if let Err(e) = c.execute_batch(&sqlite_script(&sql)) {
                results.push((id, Err(format!("the database script fails in SQLite: {e}"))));
                continue;
            }
            drop(c);
            let mapping_file = lit(props, &format!("{RDB}mappingDocument")).unwrap();
            let mapping = std::fs::read_to_string(case.join(&mapping_file)).unwrap();
            let expected =
                if lit(props, &format!("{RDB}hasExpectedOutput")).as_deref() == Some("true") {
                    let out = lit(props, &format!("{RDB}output")).unwrap();
                    Some(std::fs::read(case.join(out)).unwrap())
                } else {
                    None
                };
            let inputs = Inputs {
                connections: vec![NamedConnection {
                    name: "default".into(),
                    connection: Connection::Sqlite { path },
                    jdbc_dsn: None,
                }],
                ..Default::default()
            };
            results.push((
                id,
                check(
                    &mapping,
                    &inputs,
                    "http://example.com/base/",
                    expected.as_deref(),
                ),
            ));
        }
        results.sort_by(|a, b| a.0.cmp(&b.0));
        assert!(results.len() > 50, "{} cases", results.len());
        judge("R2RML", results, EXCLUDED);
    }
}

//! W3C SHACL test suite (the copy vendored in Apache Jena's `jena-shacl`).
//!
//! Set `SPARKLES_SHACL_TESTS` to the suite's `std` directory, otherwise
//! `../../../apache/jena/jena-shacl/src/test/files/std` relative to the workspace is
//! used; the tests are skipped when neither exists. `SPARKLES_SHACL_VERBOSE=1` prints
//! every failure with both reports; `SPARKLES_SHACL_FILTER=substr` restricts the run;
//! `SPARKLES_SHACL_SHAPES_FROM_TEXT=1` always parses the shapes graph from the file
//! (separate blank nodes) instead of reading it back from the store.
//! Failures listed in `tests/known-failures.txt` do not fail the run.
//!
//! Each `sht:Validate` test loads its data graph into an in-memory store. When the
//! shapes graph is the same document, the shapes are read back from the store
//! ([`Shapes::from_store`]) so blank nodes are shared, as in Jena where both are the
//! same `Graph`. Reports are compared on `sh:conforms` and the multiset of
//! (focusNode, resultPath, value, sourceShape, sourceConstraintComponent, severity),
//! ignoring messages, with blank nodes matched by a consistent bijection.

use oxrdf::{Graph, NamedNode, Term, TermRef, Triple};
use sparkles::io::{RdfFormat, Source};
use sparkles::store::{Store, StoreOptions};
use sparkles_shacl::{PropertyPath, Shapes, ValidateOptions, ValidationReport, ValidationResult};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

const MF: &str = "http://www.w3.org/2001/sw/DataAccess/tests/test-manifest#";
const SHT: &str = "http://www.w3.org/ns/shacl-test#";
const RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";

fn nn(s: &str) -> NamedNode {
    NamedNode::new_unchecked(s)
}

fn suite_dir() -> Option<PathBuf> {
    let p = std::env::var("SPARKLES_SHACL_TESTS")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../../../apache/jena/jena-shacl/src/test/files/std")
        });
    p.exists().then_some(p)
}

fn url_to_path(u: &str) -> PathBuf {
    PathBuf::from(u.strip_prefix("file://").unwrap_or(u))
}

fn path_to_url(p: &Path) -> String {
    format!(
        "file://{}",
        std::fs::canonicalize(p)
            .unwrap_or_else(|_| p.to_path_buf())
            .display()
    )
}

fn load_graph(url: &str) -> Graph {
    let data = std::fs::read(url_to_path(url)).unwrap_or_else(|e| panic!("{url}: {e}"));
    let mut g = Graph::new();
    for q in oxrdfio::RdfParser::from_format(RdfFormat::Turtle)
        .with_base_iri(url)
        .unwrap()
        .for_slice(&data)
    {
        let q = q.unwrap_or_else(|e| panic!("{url}: {e}"));
        g.insert(&Triple::new(q.subject, q.predicate, q.object));
    }
    g
}

fn obj(g: &Graph, s: &Term, p: &str) -> Option<Term> {
    let s: oxrdf::NamedOrBlankNodeRef<'_> = match s {
        Term::NamedNode(n) => n.as_ref().into(),
        Term::BlankNode(b) => b.as_ref().into(),
        _ => return None,
    };
    g.object_for_subject_predicate(s, &nn(p))
        .map(TermRef::into_owned)
}

fn list(g: &Graph, head: &Term) -> Vec<Term> {
    let mut out = Vec::new();
    let mut cur = head.clone();
    while cur != Term::NamedNode(nn(&format!("{RDF}nil"))) {
        let Some(f) = obj(g, &cur, &format!("{RDF}first")) else {
            break;
        };
        out.push(f);
        cur = obj(g, &cur, &format!("{RDF}rest")).unwrap();
    }
    out
}

struct Test {
    id: String,
    manifest: String,
    data: String,
    shapes: String,
    /// the mf:result node in the manifest graph
    result: Term,
    mgraph: std::rc::Rc<Graph>,
}

fn collect_tests(manifest: &str, out: &mut Vec<Test>) {
    let g = std::rc::Rc::new(load_graph(manifest));
    let me = Term::NamedNode(nn(manifest));
    let me_ref = nn(manifest);
    for inc in g
        .objects_for_subject_predicate(me_ref.as_ref(), &nn(&format!("{MF}include")))
        .map(TermRef::into_owned)
        .collect::<Vec<_>>()
    {
        if let Term::NamedNode(n) = inc {
            collect_tests(n.as_str(), out);
        }
    }
    let Some(entries) = obj(&g, &me, &format!("{MF}entries")) else {
        return;
    };
    for e in list(&g, &entries) {
        let ty = obj(&g, &e, &format!("{RDF}type"));
        if ty != Some(Term::NamedNode(nn(&format!("{SHT}Validate")))) {
            continue;
        }
        let action = obj(&g, &e, &format!("{MF}action")).unwrap();
        let iri = |p: &str| match obj(&g, &action, p) {
            Some(Term::NamedNode(n)) => n.into_string(),
            other => panic!("{e}: bad {p}: {other:?}"),
        };
        out.push(Test {
            id: e.to_string(),
            manifest: manifest.to_string(),
            data: iri(&format!("{SHT}dataGraph")),
            shapes: iri(&format!("{SHT}shapesGraph")),
            result: obj(&g, &e, &format!("{MF}result")).unwrap(),
            mgraph: g.clone(),
        });
    }
}

/// The store's default graph as an oxrdf graph (store blank node labels).
fn store_graph(store: &Store) -> Graph {
    let snap = store.snapshot();
    let mut g = Graph::new();
    snap.for_each_quad(|q| {
        if q[3] == sparkles::id::Id::DEFAULT_GRAPH
            && let Some(q) = snap.quad_to_terms(q)
        {
            g.insert(&Triple::new(q.subject, q.predicate, q.object));
        }
        Ok(())
    })
    .unwrap();
    g
}

// ------------------------------------------------------------ report comparison

type Key = (
    Term,
    Option<PropertyPath>,
    Option<Term>,
    Term,
    NamedNode,
    NamedNode,
);

fn key(r: &ValidationResult) -> Key {
    (
        r.focus_node.clone(),
        r.result_path.clone(),
        r.value.clone(),
        r.source_shape.clone(),
        r.source_constraint_component.clone(),
        r.severity.clone(),
    )
}

/// Blank node mapping expected → actual (and its inverse), with an undo trail.
#[derive(Default)]
struct BMap {
    pairs: Vec<(String, String)>,
}

impl BMap {
    fn unify(&mut self, e: &Term, a: &Term) -> bool {
        match (e, a) {
            (Term::BlankNode(x), Term::BlankNode(y)) => {
                for (p, q) in &self.pairs {
                    if p == x.as_str() || q == y.as_str() {
                        return p == x.as_str() && q == y.as_str();
                    }
                }
                self.pairs
                    .push((x.as_str().to_string(), y.as_str().to_string()));
                true
            }
            _ => e == a,
        }
    }

    fn unify_opt(&mut self, e: &Option<Term>, a: &Option<Term>) -> bool {
        match (e, a) {
            (None, None) => true,
            (Some(e), Some(a)) => self.unify(e, a),
            _ => false,
        }
    }
}

fn match_key(e: &Key, a: &Key, m: &mut BMap) -> bool {
    e.1 == a.1
        && e.4 == a.4
        && e.5 == a.5
        && m.unify(&e.0, &a.0)
        && m.unify_opt(&e.2, &a.2)
        && m.unify(&e.3, &a.3)
}

fn search(exp: &[Key], act: &[Key], used: &mut Vec<bool>, i: usize, m: &mut BMap) -> bool {
    if i == exp.len() {
        return true;
    }
    for j in 0..act.len() {
        if used[j] {
            continue;
        }
        let mark = m.pairs.len();
        if match_key(&exp[i], &act[j], m) {
            used[j] = true;
            if search(exp, act, used, i + 1, m) {
                return true;
            }
            used[j] = false;
        }
        m.pairs.truncate(mark);
    }
    false
}

fn reports_match(expected: &ValidationReport, actual: &ValidationReport) -> bool {
    if expected.conforms != actual.conforms || expected.results.len() != actual.results.len() {
        return false;
    }
    let exp: Vec<Key> = expected.results.iter().map(key).collect();
    let act: Vec<Key> = actual.results.iter().map(key).collect();
    let mut used = vec![false; act.len()];
    search(&exp, &act, &mut used, 0, &mut BMap::default())
}

// -------------------------------------------------------------------- runner

fn run_test(t: &Test) -> Result<(), String> {
    let store = Store::in_memory(StoreOptions::default());
    let src = Source::from_path(&url_to_path(&t.data), None).map_err(|e| e.to_string())?;
    store.load(&[src]).map_err(|e| format!("load: {e}"))?;
    let snap = store.snapshot();

    let failure_expected = t.result == Term::NamedNode(nn(&format!("{SHT}Failure")));
    let outcome = (|| -> anyhow::Result<ValidationReport> {
        let from_text = std::env::var("SPARKLES_SHACL_SHAPES_FROM_TEXT").is_ok();
        let shapes = if t.shapes == t.data && !from_text {
            Shapes::from_store(&snap, None)?
        } else {
            let text = std::fs::read_to_string(url_to_path(&t.shapes))?;
            Shapes::parse(&text, RdfFormat::Turtle, Some(&t.shapes))?
        };
        sparkles_shacl::validate(&snap, &shapes, &ValidateOptions::default())
    })();
    if failure_expected {
        return match outcome {
            Err(_) => Ok(()),
            Ok(r) if !r.conforms => Ok(()),
            Ok(_) => Err("expected a failure, but the data conforms".into()),
        };
    }
    let actual = outcome.map_err(|e| format!("error: {e:#}"))?;

    // expected report: from the store when the manifest is the data document (shared
    // blank nodes), else from the manifest file
    let (mg, result) = if t.manifest == t.data {
        let g = store_graph(&store);
        // find the mf:result node of this test in the store graph
        let id = Term::NamedNode(nn(t.id.trim_matches(['<', '>'])));
        let r = obj(&g, &id, &format!("{MF}result")).ok_or("no mf:result in store")?;
        (g, r)
    } else {
        ((*t.mgraph).clone(), t.result.clone())
    };
    let expected = ValidationReport::from_rdf(&mg, Some(&result)).map_err(|e| e.to_string())?;
    if reports_match(&expected, &actual) {
        Ok(())
    } else {
        Err(format!(
            "reports differ\n--- expected\n{expected}--- actual\n{actual}"
        ))
    }
}

fn run_suite(name: &str, manifest: &str) {
    let Some(dir) = suite_dir() else {
        eprintln!("W3C SHACL test suite not found; skipping {name}");
        return;
    };
    let mut tests = Vec::new();
    collect_tests(&path_to_url(&dir.join(manifest)), &mut tests);
    let known: BTreeSet<String> = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/known-failures.txt"),
    )
    .unwrap_or_default()
    .lines()
    .map(|l| l.trim().to_string())
    .filter(|l| !l.is_empty() && !l.starts_with('#'))
    .collect();
    let filter = std::env::var("SPARKLES_SHACL_FILTER").ok();
    let verbose = std::env::var("SPARKLES_SHACL_VERBOSE").is_ok();
    let (mut pass, mut fail) = (0, 0);
    let mut new_failures = Vec::new();
    let mut fixed = Vec::new();
    for t in &tests {
        let short =
            t.id.trim_matches(['<', '>'])
                .rsplit_once("/std/")
                .map(|x| x.1.to_string())
                .unwrap_or_else(|| t.id.clone());
        if filter.as_ref().is_some_and(|f| !short.contains(f.as_str())) {
            continue;
        }
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run_test(t)))
            .unwrap_or_else(|_| Err("panic".into()));
        match r {
            Ok(()) => {
                pass += 1;
                if known.contains(&short) {
                    fixed.push(short);
                }
            }
            Err(e) => {
                fail += 1;
                if verbose || !known.contains(&short) {
                    eprintln!("FAIL {short}\n{e}");
                }
                if !known.contains(&short) {
                    new_failures.push(short);
                }
            }
        }
    }
    eprintln!(
        "\n{name}: {pass} passed, {fail} failed ({} tests)",
        tests.len()
    );
    if !fixed.is_empty() {
        eprintln!("now passing (remove from known failures): {fixed:?}");
    }
    assert!(
        new_failures.is_empty(),
        "{} unexpected failures: {new_failures:#?}",
        new_failures.len()
    );
}

#[test]
fn w3c_shacl_core() {
    run_suite("SHACL Core", "core/manifest.ttl");
}

#[test]
fn w3c_shacl_sparql() {
    run_suite("SHACL-SPARQL", "sparql/manifest.ttl");
}

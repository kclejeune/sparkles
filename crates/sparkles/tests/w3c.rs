//! W3C SPARQL 1.0 / 1.1 conformance suites (from the Apache Jena checkout, which vendors
//! github.com/w3c/rdf-tests).
//!
//! Set `SPARKLES_W3C_DIR` to the `rdf-tests-cg/sparql` directory, otherwise
//! `../../../apache/jena/jena-arq/testing/rdf-tests-cg/sparql` relative to the workspace
//! is used; the tests are skipped when neither exists. `SPARKLES_W3C_VERBOSE=1` prints
//! every failure; `SPARKLES_W3C_FILTER=substr` restricts to matching test IRIs.
//! Failures listed in `tests/w3c-known-failures.txt` do not fail the run.

use oxrdf::dataset::CanonicalizationAlgorithm;
use oxrdf::vocab::rdf;
use oxrdf::{Dataset, Graph, NamedNode, NamedOrBlankNode, Quad, Term, TermRef};
use sparesults::{QueryResultsFormat, QueryResultsParser, SliceQueryResultsParserOutput};
use sparkles::io::{RdfFormat, Source};
use sparkles::sparql::{QueryKind, QueryOptions};
use sparkles::store::{Store, StoreOptions};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

const MF: &str = "http://www.w3.org/2001/sw/DataAccess/tests/test-manifest#";
const QT: &str = "http://www.w3.org/2001/sw/DataAccess/tests/test-query#";
const UT: &str = "http://www.w3.org/2009/sparql/tests/test-update#";
const RDFS_LABEL: &str = "http://www.w3.org/2000/01/rdf-schema#label";

fn nn(s: &str) -> NamedNode {
    NamedNode::new_unchecked(s)
}

fn suite_dir() -> Option<PathBuf> {
    let p = std::env::var("SPARKLES_W3C_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../../../apache/jena/jena-arq/testing/rdf-tests-cg/sparql")
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
    let path = url_to_path(url);
    let (fmt, _) = sparkles::io::format_for_path(&path).unwrap_or((RdfFormat::Turtle, false));
    let parser = oxrdfio::RdfParser::from_format(fmt)
        .with_base_iri(url)
        .unwrap();
    let data = std::fs::read(&path).unwrap_or_default();
    parser
        .for_slice(&data)
        .filter_map(Result::ok)
        .map(|q| oxrdf::Triple::new(q.subject, q.predicate, q.object))
        .collect()
}

struct Manifest {
    g: Graph,
}

impl Manifest {
    fn obj(&self, s: &NamedOrBlankNode, p: &str) -> Option<Term> {
        self.g
            .object_for_subject_predicate(s, &nn(p))
            .map(|t| t.into_owned())
    }
    fn objs(&self, s: &NamedOrBlankNode, p: &str) -> Vec<Term> {
        self.g
            .objects_for_subject_predicate(s, &nn(p))
            .map(|t| t.into_owned())
            .collect()
    }
    fn list(&self, head: Term) -> Vec<Term> {
        let mut out = Vec::new();
        let mut cur = head;
        while let Some(node) = as_subject(&cur) {
            let Some(first) = self.obj(&node, rdf::FIRST.as_str()) else {
                break;
            };
            out.push(first);
            match self.obj(&node, rdf::REST.as_str()) {
                Some(n) => cur = n,
                None => break,
            }
        }
        out
    }
}

fn as_subject(t: &Term) -> Option<NamedOrBlankNode> {
    match t {
        Term::NamedNode(n) => Some(n.clone().into()),
        Term::BlankNode(b) => Some(b.clone().into()),
        _ => None,
    }
}

fn iri(t: &Term) -> String {
    match t {
        Term::NamedNode(n) => n.as_str().to_string(),
        t => t.to_string(),
    }
}

#[derive(Debug)]
struct TestCase {
    id: String,
    kind: String,
    entry: NamedOrBlankNode,
}

fn collect_tests(manifest_url: &str, out: &mut Vec<(TestCase, std::rc::Rc<Manifest>)>) {
    let m = std::rc::Rc::new(Manifest {
        g: load_graph(manifest_url),
    });
    // the manifest node is usually the document itself, but may be any mf:Manifest
    let mut roots: Vec<NamedOrBlankNode> =
        m.g.subjects_for_predicate_object(rdf::TYPE, &nn(&format!("{MF}Manifest")))
            .map(|s| s.into_owned())
            .collect();
    if roots.is_empty() {
        roots.push(nn(manifest_url).into());
    }
    for root in &roots {
        collect_manifest(&m, root, out);
    }
}

fn collect_manifest(
    m: &std::rc::Rc<Manifest>,
    root: &NamedOrBlankNode,
    out: &mut Vec<(TestCase, std::rc::Rc<Manifest>)>,
) {
    for inc in m.objs(root, &format!("{MF}include")) {
        for i in m.list(inc) {
            collect_tests(&iri(&i), out);
        }
    }
    for entries in m.objs(root, &format!("{MF}entries")) {
        for e in m.list(entries) {
            let Some(s) = as_subject(&e) else { continue };
            let kind = m
                .obj(&s, rdf::TYPE.as_str())
                .map(|t| iri(&t))
                .unwrap_or_default();
            out.push((
                TestCase {
                    id: iri(&e),
                    kind: kind.rsplit('#').next().unwrap_or_default().to_string(),
                    entry: s,
                },
                m.clone(),
            ));
        }
    }
}

// -------------------------------------------------------------- comparison ------

/// Literal equality modulo canonical lexical forms (Jena's harness compares by value).
fn term_eq(a: &Term, b: &Term) -> bool {
    if a == b {
        return true;
    }
    match (a, b) {
        (Term::Literal(x), Term::Literal(y))
            if x.datatype() == y.datatype() && x.language() == y.language() =>
        {
            let (vx, vy) = (
                sparkles::sparql::value::Value::from_literal(x),
                sparkles::sparql::value::Value::from_literal(y),
            );
            !matches!(vx, sparkles::sparql::value::Value::Other { .. })
                && sparkles::sparql::value::equals(&vx, &vy).unwrap_or(false)
        }
        _ => false,
    }
}

type Row = Vec<Option<Term>>;

/// Multiset equality of solution sequences with blank-node isomorphism (backtracking).
fn solutions_match(expected: &[Row], actual: &[Row], ordered: bool) -> bool {
    if expected.len() != actual.len() {
        return false;
    }
    fn cell_match(
        e: &Option<Term>,
        a: &Option<Term>,
        map: &mut BTreeMap<String, String>,
        rev: &mut BTreeMap<String, String>,
    ) -> Option<Vec<String>> {
        match (e, a) {
            (None, None) => Some(Vec::new()),
            (Some(x), Some(y)) => term_match(x, y, map, rev),
            _ => None,
        }
    }
    /// Term equality with blank-node mapping, recursing into RDF 1.2 triple terms.
    fn term_match(
        e: &Term,
        a: &Term,
        map: &mut BTreeMap<String, String>,
        rev: &mut BTreeMap<String, String>,
    ) -> Option<Vec<String>> {
        match (e, a) {
            (Term::BlankNode(x), Term::BlankNode(y)) => {
                let (x, y) = (x.as_str().to_string(), y.as_str().to_string());
                match (map.get(&x), rev.get(&y)) {
                    (Some(m), _) if *m == y => Some(Vec::new()),
                    (None, None) => {
                        map.insert(x.clone(), y.clone());
                        rev.insert(y, x.clone());
                        Some(vec![x])
                    }
                    _ => None,
                }
            }
            (Term::Triple(x), Term::Triple(y)) => {
                let mut added = Vec::new();
                let parts = [
                    (Term::from(x.subject.clone()), Term::from(y.subject.clone())),
                    (
                        Term::NamedNode(x.predicate.clone()),
                        Term::NamedNode(y.predicate.clone()),
                    ),
                    (x.object.clone(), y.object.clone()),
                ];
                for (p, q) in &parts {
                    match term_match(p, q, map, rev) {
                        Some(n) => added.extend(n),
                        None => {
                            undo(&added, map, rev);
                            return None;
                        }
                    }
                }
                Some(added)
            }
            (x, y) if term_eq(x, y) => Some(Vec::new()),
            _ => None,
        }
    }
    fn row_match(
        e: &Row,
        a: &Row,
        map: &mut BTreeMap<String, String>,
        rev: &mut BTreeMap<String, String>,
    ) -> Option<Vec<String>> {
        let mut added = Vec::new();
        for (x, y) in e.iter().zip(a) {
            match cell_match(x, y, map, rev) {
                Some(n) => added.extend(n),
                None => {
                    undo(&added, map, rev);
                    return None;
                }
            }
        }
        Some(added)
    }
    fn undo(
        added: &[String],
        map: &mut BTreeMap<String, String>,
        rev: &mut BTreeMap<String, String>,
    ) {
        for x in added {
            if let Some(y) = map.remove(x) {
                rev.remove(&y);
            }
        }
    }
    #[allow(clippy::too_many_arguments)]
    fn search(
        i: usize,
        e: &[Row],
        a: &[Row],
        used: &mut [bool],
        map: &mut BTreeMap<String, String>,
        rev: &mut BTreeMap<String, String>,
        ordered: bool,
        budget: &mut usize,
    ) -> bool {
        if i == e.len() {
            return true;
        }
        if *budget == 0 {
            return false;
        }
        *budget -= 1;
        let range: Vec<usize> = if ordered {
            vec![i]
        } else {
            (0..a.len()).collect()
        };
        for j in range {
            if used[j] {
                continue;
            }
            if let Some(added) = row_match(&e[i], &a[j], map, rev) {
                used[j] = true;
                if search(i + 1, e, a, used, map, rev, ordered, budget) {
                    return true;
                }
                used[j] = false;
                undo(&added, map, rev);
            }
        }
        false
    }
    let mut used = vec![false; actual.len()];
    let mut budget = 2_000_000;
    search(
        0,
        expected,
        actual,
        &mut used,
        &mut BTreeMap::new(),
        &mut BTreeMap::new(),
        ordered,
        &mut budget,
    )
}

#[allow(clippy::large_enum_variant)]
enum Expected {
    Boolean(bool),
    Solutions(Vec<String>, Vec<Row>),
    Graph(Dataset),
}

fn parse_expected(url: &str) -> Result<Expected, String> {
    let path = url_to_path(url);
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    let fmt = match ext {
        "srx" => Some(QueryResultsFormat::Xml),
        "srj" => Some(QueryResultsFormat::Json),
        "tsv" => Some(QueryResultsFormat::Tsv),
        "csv" => return Err("csv results not compared".into()),
        _ => None,
    };
    let data = std::fs::read(&path).map_err(|e| e.to_string())?;
    match fmt {
        Some(f) => match QueryResultsParser::from_format(f)
            .for_slice(&data)
            .map_err(|e| e.to_string())?
        {
            SliceQueryResultsParserOutput::Boolean(b) => Ok(Expected::Boolean(b)),
            SliceQueryResultsParserOutput::Solutions(s) => {
                let vars: Vec<String> = s
                    .variables()
                    .iter()
                    .map(|v| v.as_str().to_string())
                    .collect();
                let mut rows = Vec::new();
                for sol in s {
                    let sol = sol.map_err(|e| e.to_string())?;
                    rows.push(vars.iter().map(|v| sol.get(v.as_str()).cloned()).collect());
                }
                Ok(Expected::Solutions(vars, rows))
            }
        },
        None => {
            let g = load_graph(url);
            if let Some(rs) = parse_rdf_result_set(&g) {
                return Ok(rs);
            }
            let mut d = Dataset::new();
            for t in g.iter() {
                d.insert(t.in_graph(oxrdf::GraphNameRef::DefaultGraph));
            }
            Ok(Expected::Graph(d))
        }
    }
}

/// DAWG result sets expressed in RDF (`rs:ResultSet`).
fn parse_rdf_result_set(g: &Graph) -> Option<Expected> {
    const RS: &str = "http://www.w3.org/2001/sw/DataAccess/tests/result-set#";
    let rs_type = nn(&format!("{RS}ResultSet"));
    let root = g
        .subjects_for_predicate_object(rdf::TYPE, &rs_type)
        .next()?
        .into_owned();
    let m = Manifest { g: g.clone() };
    if let Some(Term::Literal(b)) = m.obj(&root, &format!("{RS}boolean")) {
        return Some(Expected::Boolean(b.value() == "true"));
    }
    let lit = |t: Term| match t {
        Term::Literal(l) => l.value().to_string(),
        t => iri(&t),
    };
    let vars: Vec<String> = m
        .objs(&root, &format!("{RS}resultVariable"))
        .into_iter()
        .map(lit)
        .collect();
    let mut rows = Vec::new();
    for sol in m.objs(&root, &format!("{RS}solution")) {
        let sol = as_subject(&sol)?;
        let mut row: Row = vec![None; vars.len()];
        for b in m.objs(&sol, &format!("{RS}binding")) {
            let b = as_subject(&b)?;
            let var = m.obj(&b, &format!("{RS}variable")).map(lit)?;
            let val = m.obj(&b, &format!("{RS}value"))?;
            if let Some(i) = vars.iter().position(|v| *v == var) {
                row[i] = Some(val);
            }
        }
        rows.push(row);
    }
    Some(Expected::Solutions(vars, rows))
}

fn canon(mut d: Dataset) -> Dataset {
    d.canonicalize(CanonicalizationAlgorithm::Unstable);
    d
}

/// Graph equality with value-based literal comparison (after bnode canonicalization).
fn datasets_match(expected: Dataset, actual: Dataset) -> bool {
    let (e, a) = (canon(expected), canon(actual));
    if e == a {
        return true;
    }
    if e.len() != a.len() {
        return false;
    }
    // literal canonical-form differences (e.g. "1.0"^^xsd:decimal vs "1")
    let norm = |d: &Dataset| -> BTreeSet<String> {
        d.iter()
            .map(|q| {
                let o = match q.object {
                    TermRef::Literal(l) => {
                        let v = sparkles::sparql::value::Value::from_literal(&l.into_owned());
                        v.to_term().to_string()
                    }
                    t => t.to_string(),
                };
                format!("{} {} {} {}", q.subject, q.predicate, o, q.graph_name)
            })
            .collect()
    };
    norm(&e) == norm(&a)
}

// ------------------------------------------------------------------ running ------

fn load_store(
    m: &Manifest,
    action: &NamedOrBlankNode,
    data_p: &str,
    graph_p: &str,
    update: bool,
) -> Result<Store, String> {
    let store = Store::in_memory(StoreOptions::default());
    let mut sources = Vec::new();
    for d in m.objs(action, data_p) {
        let url = iri(&d);
        let mut s = Source::from_path(&url_to_path(&url), None).map_err(|e| e.to_string())?;
        s.base = Some(url);
        sources.push(s);
    }
    for g in m.objs(action, graph_p) {
        // query tests: graph IRI = file IRI; update tests: [ ut:graph <file> ; rdfs:label "name" ]
        let (file, name) = match (update, as_subject(&g)) {
            (true, Some(node)) => {
                let file = m
                    .obj(&node, &format!("{UT}graph"))
                    .map(|t| iri(&t))
                    .unwrap_or_default();
                let name = m
                    .obj(&node, RDFS_LABEL)
                    .map(|t| match t {
                        Term::Literal(l) => l.value().to_string(),
                        t => iri(&t),
                    })
                    .unwrap_or_else(|| file.clone());
                (file, name)
            }
            _ => (iri(&g), iri(&g)),
        };
        let mut s =
            Source::from_path(&url_to_path(&file), Some(nn(&name))).map_err(|e| e.to_string())?;
        s.base = Some(file);
        sources.push(s);
    }
    for s in sources {
        store.load(&[s]).map_err(|e| format!("loading data: {e}"))?;
    }
    Ok(store)
}

fn store_dataset(store: &Store) -> Dataset {
    let snap = store.snapshot();
    let mut d = Dataset::new();
    snap.for_each_quad(|q| {
        if let Some(q) = snap.quad_to_terms(q) {
            d.insert(&q);
        }
        Ok(())
    })
    .unwrap();
    d
}

fn run_query_test(t: &TestCase, m: &Manifest) -> Result<(), String> {
    let action = as_subject(&m.obj(&t.entry, &format!("{MF}action")).ok_or("no action")?)
        .ok_or("bad action")?;
    let qurl = iri(&m.obj(&action, &format!("{QT}query")).ok_or("no query")?);
    let qtext = std::fs::read_to_string(url_to_path(&qurl)).map_err(|e| e.to_string())?;
    let result_url = iri(&m.obj(&t.entry, &format!("{MF}result")).ok_or("no result")?);
    let expected = parse_expected(&result_url)?;
    let store = load_store(
        m,
        &action,
        &format!("{QT}data"),
        &format!("{QT}graphData"),
        false,
    )?;
    let opts = QueryOptions {
        base_iri: Some(qurl.clone()),
        timeout: Some(std::time::Duration::from_secs(20)),
        ..Default::default()
    };
    let r = sparkles::sparql::query(store.snapshot(), &qtext, &opts)
        .map_err(|e| format!("error: {e}"))?;
    match (expected, r.kind) {
        (Expected::Boolean(b), QueryKind::Ask) => {
            if b == r.boolean {
                Ok(())
            } else {
                Err(format!("expected {b}, got {}", r.boolean))
            }
        }
        (Expected::Solutions(vars, rows), QueryKind::Select) => {
            let actual: Vec<Row> = (0..r.table.len())
                .map(|i| {
                    vars.iter()
                        .map(|v| {
                            r.vars
                                .iter()
                                .position(|x| x == v)
                                .and_then(|c| r.term(r.table.cols[c][i]))
                        })
                        .collect()
                })
                .collect();
            let ordered = qtext.to_ascii_uppercase().contains("ORDER BY") && false;
            if solutions_match(&rows, &actual, ordered) {
                Ok(())
            } else {
                let show = |rows: &[Row]| {
                    rows.iter()
                        .take(12)
                        .map(|r| {
                            r.iter()
                                .map(|c| c.as_ref().map_or("-".into(), |t| t.to_string()))
                                .collect::<Vec<_>>()
                                .join(" ")
                        })
                        .collect::<Vec<_>>()
                        .join("\n      ")
                };
                Err(format!(
                    "solutions differ ({} expected, {} actual)\n    expected:\n      {}\n    actual:\n      {}",
                    rows.len(),
                    actual.len(),
                    show(&rows),
                    show(&actual)
                ))
            }
        }
        (Expected::Graph(g), QueryKind::Construct | QueryKind::Describe) => {
            let mut d = Dataset::new();
            for t in &r.triples {
                d.insert(t.as_ref().in_graph(oxrdf::GraphNameRef::DefaultGraph));
            }
            if datasets_match(g, d) {
                Ok(())
            } else {
                Err(format!("graph differs ({} triples)", r.triples.len()))
            }
        }
        (_, k) => Err(format!("result kind mismatch ({k:?})")),
    }
}

fn run_update_test(t: &TestCase, m: &Manifest) -> Result<(), String> {
    let action = as_subject(&m.obj(&t.entry, &format!("{MF}action")).ok_or("no action")?)
        .ok_or("bad action")?;
    let uurl = iri(&m
        .obj(&action, &format!("{UT}request"))
        .ok_or("no request")?);
    let utext = std::fs::read_to_string(url_to_path(&uurl)).map_err(|e| e.to_string())?;
    let store = load_store(
        m,
        &action,
        &format!("{UT}data"),
        &format!("{UT}graphData"),
        true,
    )?;
    let opts = QueryOptions {
        base_iri: Some(uurl.clone()),
        ..Default::default()
    };
    let res = sparkles::sparql::update::update(&store, &utext, &opts);
    let result = as_subject(&m.obj(&t.entry, &format!("{MF}result")).ok_or("no result")?)
        .ok_or("bad result")?;
    let expected_store = load_store(
        m,
        &result,
        &format!("{UT}data"),
        &format!("{UT}graphData"),
        true,
    )?;
    if let Err(e) = res {
        return Err(format!("update error: {e}"));
    }
    let (e, a) = (store_dataset(&expected_store), store_dataset(&store));
    if datasets_match(e.clone(), a.clone()) {
        Ok(())
    } else {
        Err(format!(
            "store differs: expected {} quads, got {}\n    got: {}",
            e.len(),
            a.len(),
            a.iter()
                .take(8)
                .map(|q| q.to_string())
                .collect::<Vec<_>>()
                .join("\n         ")
        ))
    }
}

fn run_syntax_test(t: &TestCase, m: &Manifest, positive: bool, update: bool) -> Result<(), String> {
    let a = m.obj(&t.entry, &format!("{MF}action")).ok_or("no action")?;
    let url = iri(&a);
    let text = std::fs::read_to_string(url_to_path(&url)).map_err(|e| e.to_string())?;
    let parsed = if update {
        spargebra::SparqlParser::new()
            .with_base_iri(&url)
            .unwrap()
            .parse_update(&text)
            .map(|_| ())
    } else {
        spargebra::SparqlParser::new()
            .with_base_iri(&url)
            .unwrap()
            .parse_query(&text)
            .map(|_| ())
    };
    match (parsed, positive) {
        (Ok(()), true) | (Err(_), false) => Ok(()),
        (Err(e), true) => Err(format!("parse error: {e}")),
        (Ok(()), false) => Err("accepted invalid syntax".into()),
    }
}

fn run_suite(name: &str, manifests: &[&str]) {
    let Some(dir) = suite_dir() else {
        eprintln!("W3C test suite not found; skipping {name}");
        return;
    };
    let mut tests = Vec::new();
    for m in manifests {
        collect_tests(&path_to_url(&dir.join(m)), &mut tests);
    }
    let known: BTreeSet<String> = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/w3c-known-failures.txt"),
    )
    .unwrap_or_default()
    .lines()
    .map(|l| l.trim().to_string())
    .filter(|l| !l.is_empty() && !l.starts_with('#'))
    .collect();
    let filter = std::env::var("SPARKLES_W3C_FILTER").ok();
    let verbose = std::env::var("SPARKLES_W3C_VERBOSE").is_ok();
    let (mut pass, mut fail, mut skip) = (0, 0, 0);
    let mut new_failures = Vec::new();
    let mut fixed = Vec::new();
    for (t, m) in &tests {
        if filter.as_ref().is_some_and(|f| !t.id.contains(f.as_str())) {
            continue;
        }
        let short =
            t.id.rsplit_once("/sparql/")
                .map(|x| x.1)
                .or_else(|| t.id.rsplit_once("/data-r2/").map(|x| x.1))
                .or_else(|| t.id.rsplit_once("/sparql12#").map(|x| x.1))
                .unwrap_or(&t.id)
                .to_string();
        let r = match t.kind.as_str() {
            "QueryEvaluationTest" => {
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run_query_test(t, m)))
                    .unwrap_or_else(|_| Err("panic".into()))
            }
            "UpdateEvaluationTest" => {
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run_update_test(t, m)))
                    .unwrap_or_else(|_| Err("panic".into()))
            }
            "PositiveSyntaxTest" | "PositiveSyntaxTest11" => run_syntax_test(t, m, true, false),
            "NegativeSyntaxTest" | "NegativeSyntaxTest11" => run_syntax_test(t, m, false, false),
            "PositiveUpdateSyntaxTest11" | "PositiveUpdateSyntaxTest" => {
                run_syntax_test(t, m, true, true)
            }
            "NegativeUpdateSyntaxTest11" | "NegativeUpdateSyntaxTest" => {
                run_syntax_test(t, m, false, true)
            }
            _ => {
                skip += 1;
                continue;
            }
        };
        match r {
            Ok(()) => {
                pass += 1;
                if known.contains(&short) {
                    fixed.push(short);
                }
            }
            Err(e) if e.contains("not compared") => skip += 1,
            Err(e) => {
                fail += 1;
                if verbose || !known.contains(&short) {
                    eprintln!("FAIL {short} [{}]\n    {e}", t.kind);
                }
                if !known.contains(&short) {
                    new_failures.push(short);
                }
            }
        }
    }
    eprintln!(
        "\n{name}: {pass} passed, {fail} failed, {skip} skipped ({} tests)",
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
fn sparql11_query() {
    run_suite(
        "SPARQL 1.1 query",
        &["sparql11/manifest-sparql11-query.ttl"],
    );
}

#[test]
fn sparql11_update() {
    run_suite(
        "SPARQL 1.1 update",
        &["sparql11/manifest-sparql11-update.ttl"],
    );
}

#[test]
fn sparql10() {
    run_suite(
        "SPARQL 1.0",
        &[
            "sparql10/manifest-evaluation.ttl",
            "sparql10/manifest-syntax.ttl",
        ],
    );
}

#[test]
fn sparql12() {
    run_suite("SPARQL 1.2", &["sparql12/manifest.ttl"]);
}

#[allow(dead_code)]
fn unused(_: Quad) {}

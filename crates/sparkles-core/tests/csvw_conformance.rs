//! Conformance of the CSVW converter (spec F19 §13.1) against the RDF tests of the W3C
//! CSV on the Web test suite, vendored under `testsuite/csvw/`.
//!
//! Each test names a CSV file or a metadata document, the options of the processor and
//! the expected Turtle. A test runs in minimal mode when it says `csvt:minimal` and in
//! standard mode otherwise. The converter takes its metadata explicitly, so the harness
//! plays the part of the processor that locates metadata: it applies the user metadata of
//! the test, the `Link` header the test names, `{url}-metadata.json` and
//! `csv-metadata.json`, and keeps a metadata document only when it describes the file.
//! A negative test must fail to read or to convert. Excluded tests are listed with a
//! reason. The test fails when a test outside the list fails, and when a listed test
//! passes, so the list stays honest.

use oxrdf::dataset::CanonicalizationAlgorithm;
use oxrdf::{Dataset, GraphName, NamedOrBlankNode, Quad, Term};
use sparkles_core::tabular::{self, Mapping, Metadata, Options};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

const BASE: &str = "http://www.w3.org/2013/csvw/tests/";
const CSVT: &str = "http://www.w3.org/2013/csvw/tests/vocab#";
const MF: &str = "http://www.w3.org/2001/sw/DataAccess/tests/test-manifest#";
const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";

fn dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testsuite/csvw")
}

type Props = Vec<(String, Term)>;

fn parse_manifest(path: &Path) -> HashMap<NamedOrBlankNode, Props> {
    let text = std::fs::read_to_string(path).unwrap();
    let mut out: HashMap<NamedOrBlankNode, Props> = HashMap::new();
    for t in oxttl::TurtleParser::new()
        .with_base_iri(format!("{BASE}manifest-rdf"))
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

fn value(props: &Props, p: &str) -> Option<String> {
    props.iter().find(|(q, _)| q == p).map(|(_, o)| match o {
        Term::Literal(l) => l.value().to_string(),
        Term::NamedNode(n) => n.as_str().to_string(),
        o => o.to_string(),
    })
}

/// The local file of a URL under the suite's base.
fn local(url: &str) -> PathBuf {
    // the query of a table's URL names no file
    let rel = url
        .split('?')
        .next()
        .unwrap_or(url)
        .strip_prefix(BASE)
        .unwrap_or_else(|| panic!("{url} is not under the suite's base"));
    dir().join(rel)
}

fn resolve(base: &str, rel: &str) -> String {
    oxiri::Iri::parse(base.to_string())
        .unwrap()
        .resolve(rel)
        .unwrap()
        .into_inner()
}

struct Case {
    id: String,
    kind: String,
    action: String,
    result: Option<String>,
    minimal: bool,
    user_metadata: Option<String>,
    link: Option<String>,
}

fn cases() -> Vec<Case> {
    let m = parse_manifest(&dir().join("manifest-rdf.ttl"));
    let mut out = Vec::new();
    for (s, props) in &m {
        let NamedOrBlankNode::NamedNode(name) = s else {
            continue;
        };
        let Some(kind) = value(props, RDF_TYPE).filter(|t| t.starts_with(CSVT)) else {
            continue;
        };
        let opts = props
            .iter()
            .find(|(p, _)| p == &format!("{CSVT}option"))
            .and_then(|(_, o)| match o {
                Term::BlankNode(b) => m.get(&NamedOrBlankNode::BlankNode(b.clone())),
                _ => None,
            });
        let action = value(props, &format!("{MF}action")).unwrap();
        out.push(Case {
            id: name.as_str().rsplit('#').next().unwrap().to_string(),
            kind: kind[CSVT.len()..].to_string(),
            action,
            result: value(props, &format!("{MF}result")),
            minimal: opts
                .and_then(|o| value(o, &format!("{CSVT}minimal")))
                .as_deref()
                == Some("true"),
            user_metadata: opts.and_then(|o| value(o, &format!("{CSVT}metadata"))),
            link: value(props, &format!("{CSVT}httpLink")),
        });
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

/// The metadata, with the schemas it names read from the suite's files.
fn parse(text: &str, location: &str) -> Result<Metadata, String> {
    tabular::csvw::parse_resolving(text, Some(location), true, &|url| {
        std::fs::read_to_string(local(url)).ok()
    })
}

/// Whether a metadata document describes the table at `url`.
fn describes(text: &str, location: &str, url: &str) -> bool {
    match parse(text, location) {
        Ok(m) => m.tables.iter().any(|t| t.url.as_deref() == Some(url)),
        Err(_) => false,
    }
}

/// The metadata a processor finds for the tabular file `case.action`, as its location
/// and text. User metadata comes first and does not have to describe the file. Then the
/// first document that exists among the `Link` header's target, `{url}-metadata.json`
/// and `csv-metadata.json`, which are the locations of the default site-wide
/// configuration, and the two further names that the suite's site-wide configuration
/// adds (`{url}.json` and `csvm.json`), applies when it describes the file.
fn find_metadata(case: &Case) -> Option<(String, String)> {
    let read = |url: &str| {
        std::fs::read_to_string(local(url))
            .ok()
            .map(|t| (url.to_string(), t))
    };
    if let Some(u) = &case.user_metadata {
        return read(u);
    }
    let url = case.action.as_str();
    let mut candidates = Vec::new();
    if let Some(link) = &case.link {
        // `<target>; rel="describedby"`
        if let Some(t) = link
            .trim()
            .strip_prefix('<')
            .and_then(|r| r.split('>').next())
        {
            candidates.push(resolve(&case.action, t));
        }
    }
    candidates.push(format!("{url}-metadata.json"));
    candidates.push(resolve(url, "csv-metadata.json"));
    candidates.push(format!("{url}.json"));
    candidates.push(resolve(url, "csvm.json"));
    candidates
        .iter()
        .filter_map(|u| read(u))
        .find(|(u, t)| describes(t, u, &case.action))
}

/// Convert every table of the metadata that applies (or the table at the file when
/// there is no metadata) into one N-Triples document. Tables of one group share the
/// document, and the blank node of their group is the same label in each, so the
/// document describes one table group.
fn convert(case: &Case) -> Result<Vec<u8>, String> {
    let url = case.action.clone();
    let (metadata, tables): (Metadata, Vec<usize>) = if case.action.ends_with(".json") {
        let text = std::fs::read_to_string(local(&case.action)).map_err(|e| e.to_string())?;
        let m = parse(&text, &case.action)?;
        let n = m.tables.len();
        (m, (0..n).collect())
    } else if let Some((loc, text)) = find_metadata(case) {
        let m = parse(&text, &loc)?;
        let n = m.tables.len();
        match m.tables.iter().position(|t| t.url.as_deref() == Some(&url)) {
            Some(t) => (m, vec![t]),
            // user metadata need not describe the file that starts the conversion
            None => (m, (0..n).collect()),
        }
    } else {
        let doc = serde_json::json!({
            "@context": tabular::csvw::CSVW_CONTEXT,
            "url": url,
        });
        (parse(&doc.to_string(), &url)?, vec![0])
    };
    let metadata = Arc::new(metadata);
    let registry = Arc::new(tabular::KeyRegistry::new(None));
    let mut out = Vec::new();
    for t in tabular::key_order(&metadata, &tables).map_err(|e| e.to_string())? {
        let table_url = metadata.tables[t].url.clone().ok_or("a table has no url")?;
        let path = local(&table_url);
        let input = tabular::open(&path, None).map_err(|e| e.to_string())?;
        let mut o = Options::new(Mapping::Csvw(metadata.clone()), table_url.clone());
        o.table = Some(t);
        o.part = t;
        o.standard = !case.minimal;
        o.lenient = true;
        o.keys = Some(registry.clone());
        o.tsv = tabular::tabular_kind(&path) == Some(tabular::TabularKind::Tsv);
        tabular::write(input, &o, &mut out, oxrdfio::RdfFormat::NTriples, None)
            .map_err(|e| e.to_string())?;
    }
    Ok(out)
}

fn canonical(triples: impl Iterator<Item = oxrdf::Triple>) -> Dataset {
    let mut d = Dataset::new();
    for t in triples {
        d.insert(&Quad::new(
            t.subject,
            t.predicate,
            t.object,
            GraphName::DefaultGraph,
        ));
    }
    d.canonicalize(CanonicalizationAlgorithm::Unstable);
    d
}

fn run(case: &Case) -> Result<(), String> {
    let got = convert(case);
    if case.kind == "NegativeRdfTest" {
        return match got {
            Err(_) => Ok(()),
            Ok(out) => Err(format!(
                "an error was expected, and the run gave:\n{}",
                String::from_utf8_lossy(&out)
            )),
        };
    }
    let got = got?;
    let want_path = local(case.result.as_deref().ok_or("no expected result")?);
    let want_text = std::fs::read_to_string(&want_path).map_err(|e| e.to_string())?;
    let want = canonical(
        oxttl::TurtleParser::new()
            .with_base_iri(case.action.clone())
            .map_err(|e| e.to_string())?
            .for_slice(&want_text)
            .map(|t| t.unwrap()),
    );
    let have = canonical(
        oxttl::NTriplesParser::new()
            .for_slice(&got)
            .map(|t| t.unwrap()),
    );
    if have == want {
        return Ok(());
    }
    // the differences between the two canonical datasets
    let lines = |d: &Dataset| -> std::collections::BTreeSet<String> {
        d.iter().map(|q| q.to_string()).collect()
    };
    let (g, w) = (lines(&have), lines(&want));
    let only = |a: &std::collections::BTreeSet<String>, b: &std::collections::BTreeSet<String>| {
        a.difference(b)
            .take(40)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    };
    Err(format!(
        "different output\n--- only in the output\n{}\n--- only in the expected\n{}",
        only(&g, &w),
        only(&w, &g)
    ))
}

/// Tests that are excluded, with the reason.
const EXCLUDED: &[(&str, &str)] = &[];

#[test]
fn csvw_rdf_tests() {
    let mut cases = cases();
    assert!(cases.len() > 250, "{} cases", cases.len());
    // CSVW_ONLY=test001,test002 runs some tests, with the whole output of those that fail
    if let Ok(only) = std::env::var("CSVW_ONLY") {
        cases.retain(|c| only.split(',').any(|o| o == c.id));
        for c in &cases {
            if let Ok(out) = convert(c) {
                eprintln!("{} output:\n{}", c.id, String::from_utf8_lossy(&out));
            }
        }
    }
    let excluded: HashMap<&str, &str> = EXCLUDED.iter().copied().collect();
    let (mut passed, mut failures, mut unexpected) = (0, Vec::new(), Vec::new());
    for c in &cases {
        match (run(c), excluded.get(c.id.as_str())) {
            (Ok(()), None) => passed += 1,
            (Ok(()), Some(_)) => unexpected.push(c.id.clone()),
            (Err(e), None) => failures.push(format!("{}: {e}", c.id)),
            (Err(_), Some(_)) => {}
        }
    }
    for id in excluded.keys() {
        assert!(
            cases.iter().any(|c| &c.id == id),
            "the excluded test {id} does not exist"
        );
    }
    eprintln!(
        "CSVW RDF: {passed} passed, {} excluded, {} failed, {} total",
        excluded.len(),
        failures.len(),
        cases.len()
    );
    assert!(
        failures.is_empty(),
        "{} tests failed:\n\n{}",
        failures.len(),
        failures.join("\n\n")
    );
    assert!(
        unexpected.is_empty(),
        "excluded tests now pass, remove them from the list: {unexpected:?}"
    );
}

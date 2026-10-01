//! The W3C RDF test suites (`rdf-tests-cg/rdf`: `rdf11` and `rdf12`, the Turtle, TriG,
//! N-Triples and N-Quads directories) classified by their manifests, and the SHACL test
//! files (Turtle shapes and data graphs).
//!
//! `SPARKLES_RDF_TESTS_DIR` names the `rdf` directory; without it, the `rdf` directory next
//! to `SPARKLES_W3C_DIR` (the `sparql` one), else the sibling Jena checkout. The SHACL
//! files are under `SPARKLES_SHACL_TESTS`, else the sibling Jena checkout's
//! `jena-shacl/src/test/files/std`. Suites that are absent are skipped.

use super::{T, TurtleReader};
use sparkles_fmt::Language;
use std::path::{Path, PathBuf};

const RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
const MF: &str = "http://www.w3.org/2001/sw/DataAccess/tests/test-manifest#";
const RDFT: &str = "http://www.w3.org/ns/rdftest#";

/// The `rdf-tests-cg/rdf` directory, `None` when absent.
pub fn suite_dir() -> Option<PathBuf> {
    let p = match (
        std::env::var("SPARKLES_RDF_TESTS_DIR"),
        std::env::var("SPARKLES_W3C_DIR"),
    ) {
        (Ok(rdf), _) => PathBuf::from(rdf),
        (_, Ok(sparql)) => Path::new(&sparql).join("../rdf"),
        _ => Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../../apache/jena/jena-arq/testing/rdf-tests-cg/rdf"),
    };
    p.exists().then_some(p)
}

/// The SHACL test directory, `None` when absent.
pub fn shacl_dir() -> Option<PathBuf> {
    let p = std::env::var("SPARKLES_SHACL_TESTS")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../../../apache/jena/jena-shacl/src/test/files/std")
        });
    p.exists().then_some(p)
}

/// What a manifest says about a file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RdfKind {
    /// a positive syntax test: the input parses
    PositiveSyntax,
    /// a negative syntax test: the reference parser must reject it
    NegativeSyntax,
    /// an evaluation test: the input parses to the graph of `result` (N-Triples or
    /// N-Quads)
    Eval,
    /// a negative evaluation test: the reference parser must reject it
    NegativeEval,
    /// a canonical form test of N-Triples or N-Quads: `result` is the input in canonical
    /// form
    C14n,
}

impl RdfKind {
    /// Whether the input is a valid document.
    pub fn is_positive(self) -> bool {
        matches!(
            self,
            RdfKind::PositiveSyntax | RdfKind::Eval | RdfKind::C14n
        )
    }
}

/// One test of the suite.
#[derive(Clone, Debug)]
pub struct RdfCase {
    pub path: PathBuf,
    /// the path under the suite directory, `/`-separated (the key of
    /// `tests/fmt-known-failures.txt`)
    pub rel: String,
    pub language: Language,
    pub kind: RdfKind,
    /// the expected result of an evaluation or canonical form test
    pub result: Option<PathBuf>,
    /// the base IRI the expected result assumes (the manifest's `mf:assumedTestBase`
    /// and the file name), for inputs with relative IRIs
    pub base: Option<String>,
}

/// Every test of `lang` in the manifests under `dir`, sorted by path. A file several
/// entries use appears once, negative if any of them is.
pub fn cases(dir: &Path, lang: Language) -> Vec<RdfCase> {
    let mut manifests = Vec::new();
    super::walk(dir, &mut |p| {
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or_default();
        if name.starts_with("manifest") && name.ends_with(".ttl") {
            manifests.push(p.to_path_buf());
        }
    });
    let mut out: Vec<RdfCase> = Vec::new();
    for m in manifests {
        for case in manifest_cases(&m, lang) {
            match out.iter_mut().find(|c| c.path == case.path) {
                Some(c) if !case.kind.is_positive() => c.kind = case.kind,
                Some(_) => {}
                None => out.push(case),
            }
        }
    }
    for c in &mut out {
        c.rel = c
            .path
            .strip_prefix(std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf()))
            .unwrap_or(&c.path)
            .to_string_lossy()
            .replace('\\', "/");
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

fn manifest_cases(path: &Path, lang: Language) -> Vec<RdfCase> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let g = TurtleReader::read(&text, path);
    let prefix = match lang {
        Language::Turtle => "TestTurtle",
        Language::TriG => "TestTrig",
        Language::NTriples => "TestNTriples",
        Language::NQuads => "TestNQuads",
        Language::Sparql | Language::JsonLd => return Vec::new(),
    };
    // the directory's base IRI: relative IRIs in evaluation inputs resolve against it
    let test_base = g
        .objects_of(&format!("{MF}assumedTestBase"))
        .into_iter()
        .find_map(|t| match t {
            T::Iri(iri) => Some(iri),
            _ => None,
        });
    let dir = std::fs::canonicalize(path.parent().unwrap_or(Path::new(".")))
        .unwrap_or_else(|_| path.to_path_buf());
    let file = |t: Option<T>| match t {
        Some(T::Iri(iri)) => iri.strip_prefix("file://").map(|f| {
            let f = PathBuf::from(f);
            std::fs::canonicalize(&f).unwrap_or(f)
        }),
        _ => None,
    };
    let mut out = Vec::new();
    for list in g.objects_of(&format!("{MF}entries")) {
        for entry in g.list(&list) {
            let Some(T::Iri(ty)) = g.object(&entry, &format!("{RDF}type")) else {
                continue;
            };
            let Some(test) = ty.strip_prefix(RDFT).and_then(|t| t.strip_prefix(prefix)) else {
                continue;
            };
            let kind = match test {
                "PositiveSyntax" => RdfKind::PositiveSyntax,
                "NegativeSyntax" => RdfKind::NegativeSyntax,
                "Eval" => RdfKind::Eval,
                "NegativeEval" => RdfKind::NegativeEval,
                "PositiveC14N" => RdfKind::C14n,
                _ => continue,
            };
            let Some(path) = file(g.object(&entry, &format!("{MF}action"))) else {
                continue;
            };
            let base = test_base.as_ref().and_then(|b| {
                let name = path.strip_prefix(&dir).ok()?.to_str()?;
                Some(format!("{b}{name}"))
            });
            out.push(RdfCase {
                path,
                rel: String::new(),
                language: lang,
                kind,
                result: file(g.object(&entry, &format!("{MF}result"))),
                base,
            });
        }
    }
    out
}

/// Every `.ttl` file under the SHACL test directory: `(path under it, path)`, sorted.
pub fn shacl_files(dir: &Path) -> Vec<(String, PathBuf)> {
    let mut v = Vec::new();
    super::walk(dir, &mut |p| {
        if p.extension().and_then(|e| e.to_str()) == Some("ttl") {
            let rel = p
                .strip_prefix(dir)
                .unwrap_or(p)
                .to_string_lossy()
                .replace('\\', "/");
            v.push((rel, p.to_path_buf()));
        }
    });
    v.sort();
    v
}

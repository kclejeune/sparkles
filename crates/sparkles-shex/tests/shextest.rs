//! The shexTest conformance suite (the copy vendored in Apache Jena's `jena-shex`, or an
//! upstream shexTest checkout).
//!
//! Set `SPARKLES_SHEX_TESTS` to the suite directory (the one holding `schemas/` and
//! `validation/`), otherwise `../../apache/jena/jena-shex/src/test/files/spec` next to
//! the workspace is used; the tests are skipped when neither exists.
//! `SPARKLES_SHEX_FILTER=substr` restricts the run to test ids containing `substr`;
//! `SPARKLES_SHEX_VERBOSE=1` also prints the known failures.
//!
//! Groups (one test each):
//!
//! * **syntax**: every `syntax/*.shex` (Jena's layout) or `schemas/*.shex` (upstream)
//!   parses;
//! * **negativeSyntax**: every `negativeSyntax/*.shex` fails to parse;
//! * **negativeStructure**: every `negativeStructure/*.shex` parses and fails to
//!   compile;
//! * **representation** (`schemas/manifest.jsonld`, or `manifest.ttl`): ShExC → ShExJ
//!   equals the `.json` file after normalization, and ShExJ → ShExC → ShExJ is stable;
//! * **validation** (`validation/manifest.ttl`): each test runs twice, with the schema
//!   from ShExC (`validation/…`) and from ShExJ (`validation-shexj/…`), on one in-memory
//!   store. The data is inserted term by term so that blank node labels in the manifest
//!   (`sht:focus _:x`) name the data's blank nodes.
//!
//! Test ids are `group/name`. Failures listed in `tests/known-failures.txt` do not fail
//! the run, and listed tests that pass are reported. Tests with `mf:status mf:Proposed`
//! run but never fail it. Tests of ShEx 2.2 features (the `Extends`, `Abstract` and
//! `ExtendsDiamond` traits) are excluded. Validation tests of facets on blank-node
//! labels (`LexicalBNode`) are skipped with a reason and counted, because the store
//! does not keep blank node labels. Parts of the crate that still report "not
//! implemented" are counted as skipped, not failed.

use oxrdf::{BlankNode, Graph, Literal, NamedNode, Term, TermRef};
use serde_json::Value;
use sparkles::id::Id;
use sparkles::store::{Store, StoreOptions};
use sparkles_shex::{
    Association, FileResolver, NodeSelector, ResultMap, Schema, SemAct, ShapeExpr, ShapeLabel,
    ShapeMap, Status, TripleExpr, ValidateOptions,
};
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Once;
use std::time::Duration;

const MF: &str = "http://www.w3.org/2001/sw/DataAccess/tests/test-manifest#";
const SHT: &str = "http://www.w3.org/ns/shacl/test-suite#";
const SX: &str = "https://shexspec.github.io/shexTest/ns#";
const RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";

/// Traits of ShEx 2.2 features, which ShEx 2.1 does not have.
const EXCLUDED_TRAITS: &[&str] = &["Extends", "Abstract", "ExtendsDiamond"];

/// Validation tests skipped by trait, with the reason: they test what Sparkles does
/// not keep. They are counted per reason and left out of the pass rate.
const SKIPPED_TRAITS: &[(&str, &str)] = &[(
    "LexicalBNode",
    "blank node labels are not preserved by the store",
)];

// ------------------------------------------------------------------ the suite ----

fn suite_dir() -> Option<PathBuf> {
    let p = std::env::var("SPARKLES_SHEX_TESTS")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../../../apache/jena/jena-shex/src/test/files/spec")
        });
    p.join("schemas").is_dir().then_some(p)
}

fn suite_or_skip(group: &str) -> Option<PathBuf> {
    let dir = suite_dir();
    if dir.is_none() {
        eprintln!("shexTest suite not found (set SPARKLES_SHEX_TESTS); skipping {group}");
    }
    dir
}

fn url_to_path(u: &str) -> PathBuf {
    let p = u.strip_prefix("file://").unwrap_or(u);
    // percent-decoding, for the few characters IRIs escape
    let b = p.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let Ok(v) = u8::from_str_radix(&p[i + 1..i + 3], 16)
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    PathBuf::from(String::from_utf8_lossy(&out).into_owned())
}

fn path_to_url(p: &Path) -> String {
    format!(
        "file://{}",
        std::fs::canonicalize(p)
            .unwrap_or_else(|_| p.to_path_buf())
            .display()
    )
}

/// The `.shex` files of a directory, sorted.
fn shex_files(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.extension().is_some_and(|x| x == "shex"))
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

fn stem(p: &Path) -> String {
    p.file_stem().unwrap().to_string_lossy().into_owned()
}

fn read(p: &Path) -> Result<String, String> {
    std::fs::read_to_string(p).map_err(|e| format!("{}: {e}", p.display()))
}

fn load_turtle(url: &str) -> Result<Graph, String> {
    let data = std::fs::read(url_to_path(url)).map_err(|e| format!("{url}: {e}"))?;
    let mut g = Graph::new();
    // lenient: some data has language tags that BCP 47 does not allow (`@fr-be-fbcl`)
    for t in oxttl::TurtleParser::new()
        .lenient()
        .with_base_iri(url)
        .map_err(|e| e.to_string())?
        .for_slice(&data)
    {
        g.insert(&t.map_err(|e| format!("{url}: {e}"))?);
    }
    Ok(g)
}

fn nn(s: &str) -> NamedNode {
    NamedNode::new_unchecked(s)
}

fn subject_ref(t: &Term) -> Option<oxrdf::NamedOrBlankNodeRef<'_>> {
    match t {
        Term::NamedNode(n) => Some(n.as_ref().into()),
        Term::BlankNode(b) => Some(b.as_ref().into()),
        _ => None,
    }
}

fn obj(g: &Graph, s: &Term, p: &str) -> Option<Term> {
    g.object_for_subject_predicate(subject_ref(s)?, &nn(p))
        .map(TermRef::into_owned)
}

fn objs(g: &Graph, s: &Term, p: &str) -> Vec<Term> {
    let Some(s) = subject_ref(s) else {
        return Vec::new();
    };
    g.objects_for_subject_predicate(s, &nn(p))
        .map(TermRef::into_owned)
        .collect()
}

fn list(g: &Graph, head: &Term) -> Vec<Term> {
    let nil = Term::NamedNode(nn(&format!("{RDF}nil")));
    let mut out = Vec::new();
    let mut cur = head.clone();
    while cur != nil {
        let Some(f) = obj(g, &cur, &format!("{RDF}first")) else {
            break;
        };
        out.push(f);
        let Some(r) = obj(g, &cur, &format!("{RDF}rest")) else {
            break;
        };
        cur = r;
    }
    out
}

fn local_name(iri: &str) -> &str {
    iri.rsplit(['#', '/']).next().unwrap_or(iri)
}

// --------------------------------------------------------------- bookkeeping ----

enum Outcome {
    Pass,
    Fail(String),
    /// a part of the crate that is not written yet
    NotImplemented,
}

fn is_not_implemented(msg: &str) -> bool {
    msg.contains("not implemented") || msg.contains("not yet implemented")
}

/// `Err(message)` as a failure, or as not implemented.
fn failed(msg: impl Into<String>) -> Outcome {
    let msg = msg.into();
    if is_not_implemented(&msg) {
        Outcome::NotImplemented
    } else {
        Outcome::Fail(msg)
    }
}

fn known_failures() -> BTreeSet<String> {
    std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/known-failures.txt"))
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| l.split_whitespace().next().map(str::to_string))
        .collect()
}

/// Run `f`, turning a panic into a failure (or "not implemented" for `unimplemented!`).
fn guarded(f: impl FnOnce() -> Outcome) -> Outcome {
    static QUIET: Once = Once::new();
    QUIET.call_once(|| {
        // the panics of the tests are reported with the test; the default hook would
        // print each one again
        let default = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if !std::thread::current()
                .name()
                .is_some_and(|n| n.starts_with("group_"))
            {
                default(info);
            }
        }));
    });
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
        Ok(o) => o,
        Err(p) => {
            let msg = p
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| p.downcast_ref::<&str>().map(|s| s.to_string()))
                .unwrap_or_else(|| "panic".into());
            failed(format!("panic: {msg}"))
        }
    }
}

/// The tally of one group.
struct Group {
    name: &'static str,
    known: BTreeSet<String>,
    filter: Option<String>,
    verbose: bool,
    pass: usize,
    fail: usize,
    /// approved tests run (passed or failed)
    approved: usize,
    approved_pass: usize,
    proposed_fail: usize,
    known_fail: usize,
    excluded: usize,
    /// tests skipped, per reason
    skipped: std::collections::BTreeMap<&'static str, usize>,
    not_implemented: usize,
    new_failures: Vec<String>,
    fixed: Vec<String>,
}

impl Group {
    fn new(name: &'static str) -> Group {
        Group {
            name,
            known: known_failures(),
            filter: std::env::var("SPARKLES_SHEX_FILTER").ok(),
            verbose: std::env::var("SPARKLES_SHEX_VERBOSE").is_ok(),
            pass: 0,
            fail: 0,
            approved: 0,
            approved_pass: 0,
            proposed_fail: 0,
            known_fail: 0,
            excluded: 0,
            skipped: Default::default(),
            not_implemented: 0,
            new_failures: Vec::new(),
            fixed: Vec::new(),
        }
    }

    fn id(&self, name: &str) -> String {
        format!("{}/{name}", self.name)
    }

    /// Whether the filter selects the test.
    fn selects(&self, name: &str) -> bool {
        self.filter
            .as_ref()
            .is_none_or(|f| self.id(name).contains(f.as_str()))
    }

    fn exclude(&mut self) {
        self.excluded += 1;
    }

    fn skip(&mut self, why: &'static str) {
        *self.skipped.entry(why).or_default() += 1;
    }

    fn record(&mut self, name: &str, proposed: bool, outcome: Outcome) {
        let id = self.id(name);
        let known = self.known.contains(&id);
        match outcome {
            Outcome::NotImplemented => self.not_implemented += 1,
            Outcome::Pass => {
                self.pass += 1;
                if !proposed {
                    self.approved += 1;
                    self.approved_pass += 1;
                }
                if known {
                    self.fixed.push(id);
                }
            }
            Outcome::Fail(why) => {
                self.fail += 1;
                if !proposed {
                    self.approved += 1;
                }
                if known {
                    self.known_fail += 1;
                } else if proposed {
                    self.proposed_fail += 1;
                }
                if self.verbose || !known {
                    let tag = if proposed { " (proposed)" } else { "" };
                    eprintln!("FAIL {id}{tag}: {why}");
                }
                if !known && !proposed {
                    self.new_failures.push(id);
                }
            }
        }
    }

    fn finish(self) {
        let rate = if self.approved == 0 {
            "-".to_string()
        } else {
            format!(
                "{:.1} %",
                100.0 * self.approved_pass as f64 / self.approved as f64
            )
        };
        eprintln!(
            "\n{}: {} passed, {} failed ({} known, {} proposed), {} excluded, {} skipped, \
             {} not implemented; approved pass rate {rate}",
            self.name,
            self.pass,
            self.fail,
            self.known_fail,
            self.proposed_fail,
            self.excluded,
            self.skipped.values().sum::<usize>(),
            self.not_implemented,
        );
        for (why, n) in &self.skipped {
            eprintln!("{}: {n} skipped: {why}", self.name);
        }
        if !self.fixed.is_empty() {
            eprintln!(
                "{}: now passing (remove from known failures): {:#?}",
                self.name, self.fixed
            );
        }
        assert!(
            self.new_failures.is_empty(),
            "{}: {} unexpected failures: {:#?}",
            self.name,
            self.new_failures.len(),
            self.new_failures
        );
    }
}

/// Run a group's body on a thread named after it (so the quiet panic hook knows its
/// panics) with a large stack (deeply nested schemas).
fn run_group(name: &'static str, body: impl FnOnce(&mut Group) + Send + 'static) {
    let h = std::thread::Builder::new()
        .name(format!("group_{name}"))
        .stack_size(64 << 20)
        .spawn(move || {
            let mut g = Group::new(name);
            body(&mut g);
            g
        })
        .unwrap();
    match h.join() {
        Ok(g) => g.finish(),
        Err(p) => std::panic::resume_unwind(p),
    }
}

// --------------------------------------------------------------------- syntax ----

#[test]
fn syntax() {
    let Some(dir) = suite_or_skip("syntax") else {
        return;
    };
    run_group("syntax", move |g| {
        // Jena's layout has a `syntax/` directory; upstream, the schemas are the
        // syntax tests
        let sdir = if dir.join("syntax").is_dir() {
            dir.join("syntax")
        } else {
            dir.join("schemas")
        };
        for f in shex_files(&sdir) {
            let name = stem(&f);
            if !g.selects(&name) {
                continue;
            }
            let o = guarded(|| {
                let text = match read(&f) {
                    Ok(t) => t,
                    Err(e) => return Outcome::Fail(e),
                };
                match Schema::parse_shexc(&text, Some(&path_to_url(&f))) {
                    Ok(_) => Outcome::Pass,
                    Err(e) => failed(e.to_string()),
                }
            });
            g.record(&name, false, o);
        }
    });
}

#[test]
fn negative_syntax() {
    let Some(dir) = suite_or_skip("negativeSyntax") else {
        return;
    };
    run_group("negativeSyntax", move |g| {
        for f in shex_files(&dir.join("negativeSyntax")) {
            let name = stem(&f);
            if !g.selects(&name) {
                continue;
            }
            let o = guarded(|| {
                let text = match read(&f) {
                    Ok(t) => t,
                    Err(e) => return Outcome::Fail(e),
                };
                match Schema::parse_shexc(&text, Some(&path_to_url(&f))) {
                    Ok(_) => Outcome::Fail("parsed without an error".into()),
                    Err(e) if is_not_implemented(&e.message) => Outcome::NotImplemented,
                    Err(_) => Outcome::Pass,
                }
            });
            g.record(&name, false, o);
        }
    });
}

fn resolver_for(schema_file: &Path) -> FileResolver {
    FileResolver {
        dirs: schema_file
            .parent()
            .map(Path::to_path_buf)
            .into_iter()
            .collect(),
        ..Default::default()
    }
}

#[test]
fn negative_structure() {
    let Some(dir) = suite_or_skip("negativeStructure") else {
        return;
    };
    run_group("negativeStructure", move |g| {
        for f in shex_files(&dir.join("negativeStructure")) {
            let name = stem(&f);
            if !g.selects(&name) {
                continue;
            }
            let o = guarded(|| {
                let text = match read(&f) {
                    Ok(t) => t,
                    Err(e) => return Outcome::Fail(e),
                };
                let schema = match Schema::parse_shexc(&text, Some(&path_to_url(&f))) {
                    Ok(s) => s,
                    Err(e) => return failed(format!("does not parse: {e}")),
                };
                match sparkles_shex::compile(&schema, &resolver_for(&f)) {
                    Ok(_) => Outcome::Fail("compiled without an error".into()),
                    Err(e) if is_not_implemented(&e.message) => Outcome::NotImplemented,
                    Err(_) => Outcome::Pass,
                }
            });
            g.record(&name, false, o);
        }
    });
}

// ------------------------------------------------------------- representation ----

/// ShExJ in a canonical form: numbers compared as doubles, defaulted keys (`min` and
/// `max` of 1, `closed`, `inverse` and `abstract` false) dropped, and 2.next `ShapeDecl`
/// wrappers that are not abstract unwrapped into the ShEx 2.1 form (`id` on the shape
/// expression). Key order never matters to `Value` equality.
fn normalize(v: &Value) -> Value {
    match v {
        Value::Number(n) => n.as_f64().map(Value::from).unwrap_or_else(|| v.clone()),
        Value::Array(a) => Value::Array(a.iter().map(normalize).collect()),
        Value::Object(o) => {
            if o.get("type").and_then(Value::as_str) == Some("ShapeDecl")
                && o.get("abstract").is_none_or(|a| a == &Value::Bool(false))
                && let Some(Value::Object(inner)) = o.get("shapeExpr")
            {
                let mut inner = inner.clone();
                if let Some(id) = o.get("id") {
                    inner.insert("id".into(), id.clone());
                }
                return normalize(&Value::Object(inner));
            }
            let mut m = serde_json::Map::new();
            for (k, x) in o {
                let defaulted = match k.as_str() {
                    "closed" | "inverse" | "abstract" => x == &Value::Bool(false),
                    "min" | "max" => {
                        x.as_f64() == Some(1.0)
                            && o.get(if k == "min" { "max" } else { "min" })
                                .is_none_or(|y| y.as_f64() == Some(1.0))
                    }
                    _ => false,
                };
                if !defaulted {
                    m.insert(k.clone(), normalize(x));
                }
            }
            Value::Object(m)
        }
        _ => v.clone(),
    }
}

/// The ShExJ of a schema, catching the writer's panics.
fn shexj_of(s: &Schema) -> Result<Value, Outcome> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| s.to_shexj())) {
        Ok(v) => Ok(v),
        Err(p) => {
            let msg = p
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| p.downcast_ref::<&str>().map(|s| s.to_string()))
                .unwrap_or_default();
            Err(failed(format!("ShExJ writer panicked: {msg}")))
        }
    }
}

/// A representation test: the `.shex` and `.json` files of one schema.
struct RepTest {
    name: String,
    proposed: bool,
    excluded: bool,
    shex: PathBuf,
    json: PathBuf,
}

fn rep_tests(dir: &Path) -> Vec<RepTest> {
    let sdir = dir.join("schemas");
    let jsonld = sdir.join("manifest.jsonld");
    let mut out = Vec::new();
    if let Ok(text) = std::fs::read_to_string(&jsonld) {
        let m: Value = serde_json::from_str(&text).expect("schemas/manifest.jsonld");
        let entries = m["@graph"]
            .as_array()
            .and_then(|g| g.iter().find_map(|x| x.get("entries")))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for e in entries {
            let (Some(shex), Some(json)) = (e["shex"].as_str(), e["json"].as_str()) else {
                continue;
            };
            let traits: Vec<&str> = match &e["trait"] {
                Value::String(s) => vec![s.as_str()],
                Value::Array(a) => a.iter().filter_map(Value::as_str).collect(),
                _ => Vec::new(),
            };
            out.push(RepTest {
                name: e["name"].as_str().unwrap_or(shex).to_string(),
                proposed: e["status"].as_str().is_some_and(|s| s != "mf:Approved"),
                excluded: traits
                    .iter()
                    .any(|t| EXCLUDED_TRAITS.contains(&local_name(t).trim_start_matches("sht:"))),
                shex: sdir.join(shex),
                json: sdir.join(json),
            });
        }
        return out;
    }
    // the Turtle manifest (`sx:shex`, `sx:json`)
    let url = path_to_url(&sdir.join("manifest.ttl"));
    let Ok(g) = load_turtle(&url) else {
        return out;
    };
    for (e, name, shex, json) in manifest_entries(&g, &url).into_iter().filter_map(|e| {
        let name = lit(&g, &e, &format!("{MF}name"))?;
        let shex = iri(&g, &e, &format!("{SX}shex"))?;
        let json = iri(&g, &e, &format!("{SX}json"))?;
        Some((e, name, shex, json))
    }) {
        out.push(RepTest {
            name,
            proposed: is_proposed(&g, &e),
            excluded: is_excluded(&g, &e),
            shex: url_to_path(&shex),
            json: url_to_path(&json),
        });
    }
    out
}

fn representation(t: &RepTest) -> Outcome {
    let (text, json) = match (read(&t.shex), read(&t.json)) {
        (Ok(a), Ok(b)) => (a, b),
        (Err(e), _) | (_, Err(e)) => return Outcome::Fail(e),
    };
    let expected: Value = match serde_json::from_str(&json) {
        Ok(v) => v,
        Err(e) => return Outcome::Fail(format!("{}: {e}", t.json.display())),
    };
    let base = path_to_url(&t.shex);
    // ShExC → ShExJ is the .json file
    let from_c = match Schema::parse_shexc(&text, Some(&base)) {
        Ok(s) => s,
        Err(e) => return failed(format!("ShExC: {e}")),
    };
    let got = match shexj_of(&from_c) {
        Ok(v) => v,
        Err(o) => return o,
    };
    if normalize(&got) != normalize(&expected) {
        return Outcome::Fail(format!(
            "ShExC → ShExJ differs\n--- expected\n{}\n--- actual\n{}",
            serde_json::to_string_pretty(&expected).unwrap(),
            serde_json::to_string_pretty(&got).unwrap()
        ));
    }
    // ShExJ → ShExC → ShExJ is stable
    let from_j = match Schema::from_shexj(&json) {
        Ok(s) => s,
        Err(e) => return failed(format!("ShExJ: {e}")),
    };
    let j1 = match shexj_of(&from_j) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let c = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| from_j.to_shexc())) {
        Ok(c) => c,
        Err(_) => return failed("ShExC writer panicked (not implemented?)"),
    };
    let again = match Schema::parse_shexc(&c, Some(&base)) {
        Ok(s) => s,
        Err(e) => return failed(format!("written ShExC does not parse: {e}\n{c}")),
    };
    let j2 = match shexj_of(&again) {
        Ok(v) => v,
        Err(o) => return o,
    };
    if normalize(&j1) != normalize(&j2) {
        return Outcome::Fail(format!(
            "ShExJ → ShExC → ShExJ changes the schema\n--- ShExC\n{c}\n--- first\n{}\n--- second\n{}",
            serde_json::to_string_pretty(&j1).unwrap(),
            serde_json::to_string_pretty(&j2).unwrap()
        ));
    }
    Outcome::Pass
}

#[test]
fn representation_tests() {
    let Some(dir) = suite_or_skip("representation") else {
        return;
    };
    run_group("representation", move |g| {
        for t in rep_tests(&dir) {
            if !g.selects(&t.name) {
                continue;
            }
            if t.excluded {
                g.exclude();
                continue;
            }
            let o = guarded(|| representation(&t));
            g.record(&t.name, t.proposed, o);
        }
    });
}

// ----------------------------------------------------------------- validation ----

fn manifest_entries(g: &Graph, url: &str) -> Vec<Term> {
    let me = Term::NamedNode(nn(url));
    let mut out = Vec::new();
    for head in objs(g, &me, &format!("{MF}entries")) {
        out.extend(list(g, &head));
    }
    out
}

fn iri(g: &Graph, s: &Term, p: &str) -> Option<String> {
    match obj(g, s, p)? {
        Term::NamedNode(n) => Some(n.into_string()),
        _ => None,
    }
}

fn lit(g: &Graph, s: &Term, p: &str) -> Option<String> {
    match obj(g, s, p)? {
        Term::Literal(l) => Some(l.value().to_string()),
        _ => None,
    }
}

fn is_proposed(g: &Graph, e: &Term) -> bool {
    iri(g, e, &format!("{MF}status")).is_some_and(|s| !s.ends_with("#Approved"))
}

/// A test of a ShEx 2.2 feature, or one of upstream's contributed tests.
fn is_excluded(g: &Graph, e: &Term) -> bool {
    if matches!(e, Term::NamedNode(n) if n.as_str().contains("/validation-contrib/")) {
        return true;
    }
    objs(g, e, &format!("{SHT}trait")).iter().any(|t| match t {
        Term::NamedNode(n) => EXCLUDED_TRAITS.contains(&local_name(n.as_str())),
        _ => false,
    })
}

/// The reason a validation test is skipped, if one of its traits is skipped.
fn skip_reason(g: &Graph, e: &Term) -> Option<&'static str> {
    objs(g, e, &format!("{SHT}trait"))
        .iter()
        .find_map(|t| match t {
            Term::NamedNode(n) => SKIPPED_TRAITS
                .iter()
                .find(|(trait_, _)| *trait_ == local_name(n.as_str()))
                .map(|(_, why)| *why),
            _ => None,
        })
}

struct ValTest {
    name: String,
    proposed: bool,
    excluded: bool,
    /// why the test is skipped ([`SKIPPED_TRAITS`])
    skip: Option<&'static str>,
    /// `sht:ValidationTest` (else `sht:ValidationFailure`)
    positive: bool,
    schema: String,
    data: String,
    /// `None`: START
    shape: Option<Term>,
    focus: Option<Term>,
    map: Option<String>,
    /// `mf:result` (a JSON result map for shape-map tests)
    result: Option<String>,
    sem_acts: Option<String>,
    externs: Option<String>,
    /// `mf:extensionResults`: the expected `print` output
    prints: Option<Vec<String>>,
}

fn val_tests(dir: &Path) -> Vec<ValTest> {
    let url = path_to_url(&dir.join("validation/manifest.ttl"));
    let g = load_turtle(&url).unwrap_or_else(|e| panic!("{e}"));
    let mut out = Vec::new();
    for e in manifest_entries(&g, &url) {
        let ty = iri(&g, &e, &format!("{RDF}type")).unwrap_or_default();
        let positive = ty == format!("{SHT}ValidationTest");
        if !positive && ty != format!("{SHT}ValidationFailure") {
            continue;
        }
        let name = lit(&g, &e, &format!("{MF}name")).unwrap_or_else(|| e.to_string());
        let Some(action) = obj(&g, &e, &format!("{MF}action")) else {
            continue;
        };
        let a = |p: &str| obj(&g, &action, &format!("{SHT}{p}"));
        let ai = |p: &str| iri(&g, &action, &format!("{SHT}{p}"));
        let prints = obj(&g, &e, &format!("{MF}extensionResults")).map(|head| {
            list(&g, &head)
                .iter()
                .flat_map(|r| objs(&g, r, &format!("{MF}prints")))
                .filter_map(|t| match t {
                    Term::Literal(l) => Some(l.value().to_string()),
                    _ => None,
                })
                .collect()
        });
        out.push(ValTest {
            proposed: is_proposed(&g, &e),
            excluded: is_excluded(&g, &e),
            skip: skip_reason(&g, &e),
            positive,
            schema: ai("schema").unwrap_or_default(),
            data: ai("data").unwrap_or_default(),
            shape: a("shape"),
            focus: a("focus"),
            map: ai("map"),
            result: iri(&g, &e, &format!("{MF}result")),
            sem_acts: ai("semActs"),
            externs: ai("shapeExterns"),
            prints,
            name,
        });
    }
    out
}

/// A store holding the test's data, and the focus node as the store knows it: a blank
/// node of the manifest is the data's blank node with that label (or a fresh one, when
/// the data has no such label).
fn load_data(url: &str, focus: Option<&Term>) -> Result<(Store, Option<Term>), String> {
    let g = load_turtle(url)?;
    let store = Store::in_memory(StoreOptions::default());
    let mut labels = HashMap::new();
    let mut txn = store.write();
    let mut quads = Vec::new();
    for t in g.iter() {
        let mut intern = |t: Term| {
            txn.intern_scoped(&t, &mut labels)
                .map_err(|e| e.to_string())
        };
        quads.push([
            intern(t.subject.into_owned().into())?,
            intern(t.predicate.into_owned().into())?,
            intern(t.object.into_owned())?,
            Id::DEFAULT_GRAPH,
        ]);
    }
    let focus = match focus {
        Some(b @ Term::BlankNode(_)) => {
            let id = txn
                .intern_scoped(b, &mut labels)
                .map_err(|e| e.to_string())?;
            Some(Term::BlankNode(sparkles::store::bnode_for(id)))
        }
        f => f.cloned(),
    };
    for q in quads {
        txn.insert(q).map_err(|e| e.to_string())?;
    }
    txn.commit().map_err(|e| e.to_string())?;
    Ok((store, focus))
}

/// Give the code of external semantic actions (a `.semact` file of start actions) to
/// the schema's actions of the same extension that have none.
fn supply_sem_acts(schema: &mut Schema, ext: &[SemAct]) {
    fn acts(v: &mut [SemAct], ext: &[SemAct]) {
        for a in v.iter_mut().filter(|a| a.code.is_none()) {
            if let Some(e) = ext.iter().find(|e| e.name == a.name) {
                a.code = e.code.clone();
            }
        }
    }
    fn se(s: &mut ShapeExpr, ext: &[SemAct]) {
        match s {
            ShapeExpr::Or(v) | ShapeExpr::And(v) => v.iter_mut().for_each(|x| se(x, ext)),
            ShapeExpr::Not(x) => se(x, ext),
            ShapeExpr::Shape(sh) => {
                acts(&mut sh.sem_acts, ext);
                if let Some(t) = &mut sh.expression {
                    te(t, ext);
                }
            }
            ShapeExpr::Nc(_) | ShapeExpr::External | ShapeExpr::Ref(_) => {}
        }
    }
    fn te(t: &mut TripleExpr, ext: &[SemAct]) {
        match t {
            TripleExpr::EachOf(g) | TripleExpr::OneOf(g) => {
                acts(&mut g.sem_acts, ext);
                g.exprs.iter_mut().for_each(|x| te(x, ext));
            }
            TripleExpr::Tc(tc) => {
                acts(&mut tc.sem_acts, ext);
                if let Some(v) = &mut tc.value_expr {
                    se(v, ext);
                }
            }
            TripleExpr::Include(_) => {}
        }
    }
    acts(&mut schema.start_acts, ext);
    if let Some(s) = &mut schema.start {
        se(s, ext);
    }
    for d in &mut schema.shapes {
        se(&mut d.expr, ext);
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Syntax {
    ShExC,
    ShExJ,
}

/// The file of a schema-like resource in the syntax, if there is one: `x.shex` →
/// `x.json`, `x.shextern` → `x.jsontern`.
fn in_syntax(url: &str, syntax: Syntax) -> Option<PathBuf> {
    let p = url_to_path(url);
    if syntax == Syntax::ShExC {
        return Some(p);
    }
    let ext = match p.extension()?.to_str()? {
        "shex" => "json",
        "shextern" => "jsontern",
        _ => return Some(p),
    };
    let j = p.with_extension(ext);
    j.exists().then_some(j)
}

fn read_schema(url: &str, syntax: Syntax) -> Result<Option<Schema>, Outcome> {
    let Some(p) = in_syntax(url, syntax) else {
        return Ok(None);
    };
    let text = read(&p).map_err(Outcome::Fail)?;
    let parsed = match syntax {
        Syntax::ShExC => Schema::parse_shexc(&text, Some(&path_to_url(&p))),
        // relative IRIs resolve against the schema file, as in ShExC
        Syntax::ShExJ => sparkles_shex::shexj::from_shexj_with_base(&text, Some(&path_to_url(&p))),
    };
    parsed
        .map(Some)
        .map_err(|e| failed(format!("{}: {e}", p.display())))
}

fn label_of(t: &Term) -> Result<ShapeLabel, String> {
    match t {
        Term::NamedNode(n) => Ok(ShapeLabel::Iri(n.as_str().to_string())),
        Term::BlankNode(b) => Ok(ShapeLabel::BNode(b.as_str().to_string())),
        other => Err(format!("bad sht:shape {other}")),
    }
}

/// `(node, shape, conformant)` of a result map, for comparison with a JSON result file.
fn result_triples(r: &ResultMap) -> BTreeSet<(String, String, bool)> {
    r.results
        .iter()
        .map(|x| {
            let node = match &x.node {
                Term::NamedNode(n) => n.as_str().to_string(),
                other => other.to_string(),
            };
            let shape = x.shape.as_shexj().unwrap_or_else(|| "START".into());
            (node, shape, x.status == Status::Conformant)
        })
        .collect()
}

/// A JSON result file: `{node: [{shape, result}]}`.
fn expected_triples(json: &str) -> Result<BTreeSet<(String, String, bool)>, String> {
    let v: Value = serde_json::from_str(json).map_err(|e| e.to_string())?;
    let mut out = BTreeSet::new();
    for (node, rs) in v.as_object().ok_or("not a JSON object")? {
        for r in rs.as_array().ok_or("not a JSON array")? {
            out.insert((
                node.clone(),
                r["shape"].as_str().unwrap_or_default().to_string(),
                r["result"].as_bool().unwrap_or(false),
            ));
        }
    }
    Ok(out)
}

fn validation(t: &ValTest, syntax: Syntax, store: &Store, focus: Option<&Term>) -> Outcome {
    let run = || -> Result<Outcome, Outcome> {
        let Some(mut schema) = read_schema(&t.schema, syntax)? else {
            return Ok(Outcome::Fail("no ShExJ form of the schema".into()));
        };
        if let Some(sa) = &t.sem_acts {
            let text = read(&url_to_path(sa)).map_err(Outcome::Fail)?;
            let ext =
                Schema::parse_shexc(&text, Some(sa)).map_err(|e| failed(format!("{sa}: {e}")))?;
            supply_sem_acts(&mut schema, &ext.start_acts);
        }
        let schema_file = url_to_path(&t.schema);
        let mut resolver = resolver_for(&schema_file);
        if let Some(x) = &t.externs {
            resolver.externs = read_schema(x, syntax)?.or(read_schema(x, Syntax::ShExC)?);
        }
        let compiled = sparkles_shex::compile(&schema, &resolver)
            .map_err(|e| failed(format!("compile: {e}")))?;

        let map = if let Some(m) = &t.map {
            let text = read(&url_to_path(m)).map_err(Outcome::Fail)?;
            ShapeMap::from_json(&text).map_err(|e| failed(format!("{m}: {e}")))?
        } else {
            let focus = focus.ok_or_else(|| Outcome::Fail("no sht:focus".into()))?;
            let shape = match &t.shape {
                Some(s) => label_of(s).map_err(Outcome::Fail)?,
                None => ShapeLabel::Start,
            };
            ShapeMap(vec![Association {
                node: NodeSelector::Term(focus.clone()),
                shape,
            }])
        };
        let opts = ValidateOptions {
            semact_trace: true,
            timeout: Some(Duration::from_secs(30)),
            ..Default::default()
        };
        let r = sparkles_shex::validate(&store.snapshot(), &compiled, &map, &opts)
            .map_err(|e| failed(format!("validate: {e:#}")))?;

        let expected_results = match (&t.map, &t.result) {
            (Some(_), Some(res)) if res.ends_with(".json") => Some(
                expected_triples(&read(&url_to_path(res)).map_err(Outcome::Fail)?)
                    .map_err(|e| Outcome::Fail(format!("{res}: {e}")))?,
            ),
            _ => None,
        };
        if let Some(exp) = expected_results {
            let got = result_triples(&r);
            if got != exp {
                return Ok(Outcome::Fail(format!(
                    "result map differs: expected {exp:?}, got {got:?}"
                )));
            }
        } else if r.conforms != t.positive {
            let why = r
                .results
                .iter()
                .find_map(|x| x.reason.clone())
                .unwrap_or_default();
            return Ok(Outcome::Fail(format!(
                "expected {}, got {} {why}",
                if t.positive {
                    "conformant"
                } else {
                    "nonconformant"
                },
                if r.conforms {
                    "conformant"
                } else {
                    "nonconformant:"
                },
            )));
        }
        if let Some(exp) = &t.prints {
            let got: Vec<String> = r.results.iter().flat_map(|x| x.prints.clone()).collect();
            if &got != exp {
                return Ok(Outcome::Fail(format!(
                    "semantic action output differs: expected {exp:?}, got {got:?}"
                )));
            }
        }
        Ok(Outcome::Pass)
    };
    guarded(|| run().unwrap_or_else(|o| o))
}

#[test]
fn validation_tests() {
    let Some(dir) = suite_or_skip("validation") else {
        return;
    };
    let h = std::thread::Builder::new()
        .name("group_validation".into())
        .stack_size(64 << 20)
        .spawn(move || {
            let mut c = Group::new("validation");
            let mut j = Group::new("validation-shexj");
            for t in val_tests(&dir) {
                let (sc, sj) = (c.selects(&t.name), j.selects(&t.name));
                if !sc && !sj {
                    continue;
                }
                if t.excluded {
                    c.exclude();
                    j.exclude();
                    continue;
                }
                if let Some(why) = t.skip {
                    c.skip(why);
                    j.skip(why);
                    continue;
                }
                let (store, focus) = match load_data(&t.data, t.focus.as_ref()) {
                    Ok(x) => x,
                    Err(e) => {
                        let e = format!("data: {e}");
                        c.record(&t.name, t.proposed, Outcome::Fail(e.clone()));
                        j.record(&t.name, t.proposed, Outcome::Fail(e));
                        continue;
                    }
                };
                for (g, syntax, selected) in
                    [(&mut c, Syntax::ShExC, sc), (&mut j, Syntax::ShExJ, sj)]
                {
                    if !selected {
                        continue;
                    }
                    let o = validation(&t, syntax, &store, focus.as_ref());
                    g.record(&t.name, t.proposed, o);
                }
            }
            (c, j)
        })
        .unwrap();
    let (c, j) = match h.join() {
        Ok(x) => x,
        Err(p) => std::panic::resume_unwind(p),
    };
    // report both before failing on either
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| c.finish()));
    j.finish();
    if let Err(p) = r {
        std::panic::resume_unwind(p);
    }
}

#[test]
fn normalization() {
    let a = serde_json::json!({"type": "Schema", "shapes": [
        {"type": "ShapeDecl", "id": "http://a.example/S", "shapeExpr": {"type": "Shape",
          "expression": {"type": "TripleConstraint", "predicate": "http://a.example/p",
            "min": 1, "max": 1, "inverse": false}}}]});
    let b = serde_json::json!({"shapes": [
        {"id": "http://a.example/S", "type": "Shape",
          "expression": {"predicate": "http://a.example/p", "type": "TripleConstraint"}}],
        "type": "Schema"});
    assert_eq!(normalize(&a), normalize(&b));
    let c = serde_json::json!({"type": "TripleConstraint", "min": 0, "max": 1});
    assert_eq!(normalize(&c)["min"], serde_json::json!(0.0));
    assert_eq!(normalize(&c)["max"], serde_json::json!(1.0));
    assert_eq!(
        normalize(&serde_json::json!({"mininclusive": 5})),
        normalize(&serde_json::json!({"mininclusive": 5.0}))
    );
}

#[test]
fn told_blank_nodes() {
    // the manifest's `_:abcd` is the data's `_:abcd`, whatever the store calls it
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("d.ttl");
    std::fs::write(&f, "_:abcd <http://a.example/p> _:x .\n").unwrap();
    let url = path_to_url(&f);
    let terms = |store: &Store| {
        let snap = store.snapshot();
        let mut v = Vec::new();
        snap.for_each_quad(|q| {
            v.extend([snap.term(q[0]), snap.term(q[2])]);
            Ok(())
        })
        .unwrap();
        v
    };
    let abcd = Term::BlankNode(BlankNode::new_unchecked("abcd"));
    let (store, focus) = load_data(&url, Some(&abcd)).unwrap();
    assert!(focus.is_some());
    assert_eq!(terms(&store)[0], focus);
    // a label the data does not have is a node of its own
    let zz = Term::BlankNode(BlankNode::new_unchecked("zz"));
    let (store, focus) = load_data(&url, Some(&zz)).unwrap();
    assert!(focus.is_some() && !terms(&store).contains(&focus));
    let lit = Term::Literal(Literal::new_simple_literal("ab"));
    assert_eq!(load_data(&url, Some(&lit)).unwrap().1, Some(lit));
}

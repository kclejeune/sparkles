//! The OWL 2 ontology of a dataset's graphs (spec F21 Phase 2): its summary, axioms,
//! entities, profile checks, edits and diffs, and `Axiom`, a frozen axiom with structural equality
//! whose `str()` is functional-style syntax.
//!
//! The handle passes its arguments to the library's JSON operations
//! (`sparkles::handles::ontology::wire::call`), so the answers have the JSON forms of the
//! HTTP routes, as dicts, with each listed axiom as an `Axiom`.
use crate::{admin, handles::handle, interrupt};
use oxrdf::GraphName;
use pyo3::exceptions::{PyTypeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList, PyString, PyTuple};
use serde_json::{Map, Value as J, json};
use sparkles::handles::ontology::wire;
use sparkles::owl::{self, Axiom};
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

handle!(PyOntologies, "Ontologies");

/// An OWL 2 axiom, compared and hashed by its structure and annotations. `Axiom(text)`
/// reads one axiom in functional-style syntax, and `str()` writes it back with the
/// standard prefixes.
#[pyclass(
    frozen,
    eq,
    hash,
    module = "sparkles",
    name = "Axiom",
    skip_from_py_object
)]
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct PyAxiom(pub Axiom);

fn prefix_list(prefixes: Option<BTreeMap<String, String>>) -> Vec<(String, String)> {
    prefixes.unwrap_or_default().into_iter().collect()
}

#[pymethods]
impl PyAxiom {
    #[new]
    #[pyo3(signature=(text, /, prefixes=None))]
    fn new(text: &str, prefixes: Option<BTreeMap<String, String>>) -> PyResult<Self> {
        Self::parse(text, prefixes)
    }
    /// Read one axiom in functional-style syntax, with `prefixes` and the standard ones.
    #[staticmethod]
    #[pyo3(signature=(text, /, prefixes=None))]
    fn parse(text: &str, prefixes: Option<BTreeMap<String, String>>) -> PyResult<Self> {
        let ax = owl::ofn_read::parse_axiom(text, &prefix_list(prefixes))
            .map_err(|e| PyValueError::new_err(format!("invalid axiom: {e}")))?;
        owl::validate::axiom(&ax).map_err(|e| PyValueError::new_err(e.0))?;
        Ok(PyAxiom(ax))
    }
    /// Read an axiom in its JSON form, as the HTTP API and `json()` give it.
    #[staticmethod]
    #[pyo3(signature=(value, /))]
    fn from_json(value: &Bound<'_, PyAny>) -> PyResult<Self> {
        let v: J = admin::from_py(value, "axiom")?;
        let ax = owl::json::read_axiom(&v)
            .map_err(|e| PyValueError::new_err(format!("invalid axiom: {e}")))?;
        owl::validate::axiom(&ax).map_err(|e| PyValueError::new_err(e.0))?;
        Ok(PyAxiom(ax))
    }
    /// The functional-style name of the axiom's type, such as `SubClassOf`.
    #[getter]
    fn r#type(&self) -> &'static str {
        self.0.axiom_type().name()
    }
    /// The axiom in its JSON form.
    fn json<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        admin::to_py(py, &owl::json::axiom(&self.0))
    }
    /// The axiom in functional-style syntax, with `prefixes` and the standard ones.
    #[pyo3(signature=(prefixes=None))]
    fn ofn(&self, prefixes: Option<BTreeMap<String, String>>) -> String {
        owl::ofn::Renderer::new(prefixes.unwrap_or_default()).axiom(&self.0)
    }
    /// Whether the axiom mentions the IRI.
    fn mentions(&self, iri: &str) -> bool {
        self.0.mentions(iri)
    }
    fn __str__(&self) -> String {
        owl::ofn::Renderer::new(Vec::<(String, String)>::new()).axiom(&self.0)
    }
    fn __repr__(&self) -> String {
        let text = self.__str__();
        format!("Axiom({text:?})")
    }
}

/// A graph argument as the operations take it: `"default"`, an IRI, or a list of them.
fn graph_json(ob: Option<&Bound<'_, PyAny>>) -> PyResult<J> {
    let Some(ob) = ob.filter(|o| !o.is_none()) else {
        return Ok(J::Null);
    };
    let one = |g: &Bound<'_, PyAny>| -> PyResult<J> {
        if let Ok(s) = g.cast::<PyString>() {
            let s = s.to_str()?;
            if s == "default" {
                return Ok(J::String(s.into()));
            }
        }
        Ok(match crate::terms::graph_from_py(g)? {
            GraphName::DefaultGraph => J::String("default".into()),
            GraphName::NamedNode(n) => J::String(n.into_string()),
            GraphName::BlankNode(_) => {
                return Err(PyTypeError::new_err("an ontology graph is named by an IRI"));
            }
        })
    };
    if ob.cast::<PyList>().is_ok() || ob.cast::<PyTuple>().is_ok() {
        let mut out = Vec::new();
        for g in ob.try_iter()? {
            out.push(one(&g?)?);
        }
        return Ok(J::Array(out));
    }
    one(ob)
}

/// An axiom argument: an `Axiom`, a string in functional-style syntax, or a dict in
/// the JSON form.
fn axiom_json(ob: &Bound<'_, PyAny>) -> PyResult<J> {
    if let Ok(a) = ob.cast::<PyAxiom>() {
        return Ok(owl::json::axiom(&a.get().0));
    }
    if let Ok(s) = ob.cast::<PyString>() {
        return Ok(J::String(s.to_str()?.to_string()));
    }
    if ob.cast::<PyDict>().is_ok() {
        return admin::from_py(ob, "axiom");
    }
    Err(PyTypeError::new_err(
        "an axiom is an Axiom, a str in functional-style syntax or a dict in its JSON form",
    ))
}

fn axioms_json(ob: Option<&Bound<'_, PyAny>>) -> PyResult<J> {
    let Some(ob) = ob.filter(|o| !o.is_none()) else {
        return Ok(J::Null);
    };
    if ob.cast::<PyString>().is_ok() || ob.cast::<PyAxiom>().is_ok() {
        return Ok(J::Array(vec![axiom_json(ob)?]));
    }
    let mut out = Vec::new();
    for a in ob.try_iter()? {
        out.push(axiom_json(&a?)?);
    }
    Ok(J::Array(out))
}

fn at_json(ob: Option<&Bound<'_, PyAny>>) -> PyResult<J> {
    let Some(ob) = ob.filter(|o| !o.is_none()) else {
        return Ok(J::Null);
    };
    if let Ok(n) = ob.extract::<u64>() {
        return Ok(json!(n));
    }
    let s: String = ob
        .extract()
        .map_err(|_| PyTypeError::new_err("a state is an int commit or a str selector"))?;
    Ok(J::String(s))
}

/// A list of names as one comma-separated string: a str, or an iterable of str.
fn names_json(ob: Option<&Bound<'_, PyAny>>) -> PyResult<J> {
    let Some(ob) = ob.filter(|o| !o.is_none()) else {
        return Ok(J::Null);
    };
    if let Ok(s) = ob.cast::<PyString>() {
        return Ok(J::String(s.to_str()?.to_string()));
    }
    let mut v = Vec::new();
    for x in ob.try_iter()? {
        v.push(x?.extract::<String>()?);
    }
    Ok(J::String(v.join(",")))
}

fn profiles_json(ob: Option<&Bound<'_, PyAny>>) -> PyResult<J> {
    let Some(ob) = ob.filter(|o| !o.is_none()) else {
        return Ok(J::Null);
    };
    if let Ok(s) = ob.cast::<PyString>() {
        return Ok(J::String(s.to_str()?.to_string()));
    }
    let mut out = Vec::new();
    for p in ob.try_iter()? {
        out.push(J::String(p?.extract::<String>()?));
    }
    Ok(J::Array(out))
}

/// The members that name the ontology.
fn base(
    graph: Option<&Bound<'_, PyAny>>,
    imports: bool,
    mode: &str,
    inferences: bool,
) -> PyResult<Map<String, J>> {
    let mut a = Map::new();
    a.insert("graph".into(), graph_json(graph)?);
    a.insert("imports".into(), json!(imports));
    a.insert("mode".into(), json!(mode));
    a.insert("inferences".into(), json!(inferences));
    Ok(a)
}

/// The answer as Python objects, with the axioms of `lists` (members holding lists of
/// entries with an `axiom` member) as `Axiom` objects.
fn answer<'py>(py: Python<'py>, j: J, lists: &[&str]) -> PyResult<Bound<'py, PyAny>> {
    let out = admin::to_py(py, &j)?;
    for key in lists {
        let Some(J::Array(entries)) = j.get(*key) else {
            continue;
        };
        let list = out.get_item(*key)?;
        for (n, e) in entries.iter().enumerate() {
            let Some(v) = e.get("axiom") else { continue };
            let ax = owl::json::read_axiom(v)
                .map_err(|e| PyValueError::new_err(format!("unreadable axiom: {e}")))?;
            list.get_item(n)?
                .set_item("axiom", Bound::new(py, PyAxiom(ax))?)?;
        }
    }
    Ok(out)
}

impl PyOntologies {
    /// Run operation `op` with `args`, under `cancel` and `timeout`. Writes go through
    /// the dataset's write handle.
    fn run<'py>(
        &self,
        py: Python<'py>,
        op: &'static str,
        args: Map<String, J>,
        cancel: Option<&Bound<'py, PyAny>>,
        timeout: Option<f64>,
        write: bool,
    ) -> PyResult<J> {
        let ds = if write { self.write(py)? } else { self.ds(py)? };
        // A partial classification answers with what it proved when its deadline or the
        // caller's token stops it, so both go to the classifier alone and not to the
        // checks around the call, which still stop it on a signal such as Ctrl-C.
        let partial = op == "classify" && args.get("partial") == Some(&J::Bool(true));
        let (timeout, own, cancel, token) = if partial {
            let own = timeout
                .map(|s| {
                    Duration::try_from_secs_f64(s)
                        .ok()
                        .filter(|_| s.is_finite())
                        .and_then(|d| Instant::now().checked_add(d))
                        .ok_or_else(|| {
                            pyo3::exceptions::PyValueError::new_err(
                                "timeout must be finite, nonnegative and not too large",
                            )
                        })
                })
                .transpose()?;
            (None, own, None, Some(interrupt::flag(cancel)?))
        } else {
            (timeout, None, cancel, None)
        };
        let args = J::Object(args);
        interrupt::controlled(py, cancel, None, timeout, move |ctl| {
            let ctl = sparkles::task::Control {
                deadline: ctl.deadline.or(own),
                ..ctl
            };
            let mut opts = sparkles::sparql::QueryOptions {
                cancel: Some(ctl.cancel.flag()),
                cancellation: token.map(|t| sparkles::task::Cancellation::from_flags([t])),
                deadline: ctl.deadline,
                ..Default::default()
            };
            if write {
                opts.write.cancel = Some(ctl.cancel.flag());
                opts.write.deadline = ctl.deadline;
            }
            wire::call(&ds, op, &args, &opts)
        })
    }
}

#[pymethods]
impl PyOntologies {
    /// The ontology header, the counts of entities by kind and of axioms by type, the
    /// graphs read and the diagnostics of the read.
    #[pyo3(signature=(graph=None, *, imports=true, mode="lenient", at=None, inferences=false, cancel=None, timeout=None))]
    #[allow(clippy::too_many_arguments)]
    fn summary<'py>(
        &self,
        py: Python<'py>,
        graph: Option<&Bound<'py, PyAny>>,
        imports: bool,
        mode: &str,
        at: Option<&Bound<'py, PyAny>>,
        inferences: bool,
        cancel: Option<&Bound<'py, PyAny>>,
        timeout: Option<f64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let mut a = base(graph, imports, mode, inferences)?;
        a.insert("at".into(), at_json(at)?);
        let j = self.run(py, "summary", a, cancel, timeout, false)?;
        answer(py, j, &[])
    }

    /// One page of the axioms with a type in `type` and mentioning `about`, with the
    /// assertions the store holds when `assertions` is true. Each entry's `axiom` is an
    /// `Axiom`, and `next` is the offset of the next page.
    #[pyo3(signature=(graph=None, *, r#type=None, about=None, assertions=false, limit=100, offset=0, imports=true, mode="lenient", at=None, inferences=false, cancel=None, timeout=None))]
    #[allow(clippy::too_many_arguments)]
    fn axioms<'py>(
        &self,
        py: Python<'py>,
        graph: Option<&Bound<'py, PyAny>>,
        r#type: Option<&Bound<'py, PyAny>>,
        about: Option<&Bound<'py, PyAny>>,
        assertions: bool,
        limit: usize,
        offset: usize,
        imports: bool,
        mode: &str,
        at: Option<&Bound<'py, PyAny>>,
        inferences: bool,
        cancel: Option<&Bound<'py, PyAny>>,
        timeout: Option<f64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let mut a = base(graph, imports, mode, inferences)?;
        a.insert("at".into(), at_json(at)?);
        let types = match r#type.filter(|t| !t.is_none()) {
            None => J::Null,
            Some(t) if t.cast::<PyString>().is_ok() => J::String(t.extract()?),
            Some(t) => {
                let mut v = Vec::new();
                for x in t.try_iter()? {
                    v.push(x?.extract::<String>()?);
                }
                J::String(v.join(","))
            }
        };
        a.insert("type".into(), types);
        let about = match about.filter(|x| !x.is_none()) {
            None => J::Null,
            Some(x) => J::String(crate::terms::iri_from_py(x)?.into_string()),
        };
        a.insert("about".into(), about);
        a.insert("assertions".into(), json!(assertions));
        a.insert("limit".into(), json!(limit));
        a.insert("offset".into(), json!(offset));
        let j = self.run(py, "axioms", a, cancel, timeout, false)?;
        answer(py, j, &["axioms"])
    }

    /// One page of the entities of the kinds in `kind` (all kinds by default), each a
    /// dict with its `kind` and `iri`, as `GET /$/ontology/{ds}/entities` lists them.
    #[pyo3(signature=(graph=None, *, kind=None, limit=100, offset=0, imports=true, mode="lenient", at=None, inferences=false, cancel=None, timeout=None))]
    #[allow(clippy::too_many_arguments)]
    fn entities<'py>(
        &self,
        py: Python<'py>,
        graph: Option<&Bound<'py, PyAny>>,
        kind: Option<&Bound<'py, PyAny>>,
        limit: usize,
        offset: usize,
        imports: bool,
        mode: &str,
        at: Option<&Bound<'py, PyAny>>,
        inferences: bool,
        cancel: Option<&Bound<'py, PyAny>>,
        timeout: Option<f64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let mut a = base(graph, imports, mode, inferences)?;
        a.insert("at".into(), at_json(at)?);
        a.insert("kind".into(), names_json(kind)?);
        a.insert("limit".into(), json!(limit));
        a.insert("offset".into(), json!(offset));
        let j = self.run(py, "entities", a, cancel, timeout, false)?;
        answer(py, j, &[])
    }

    /// The entities whose labels, synonyms, short forms, IRIs or definitions match `q`,
    /// ranked for a typeahead with typo tolerance, as `GET /$/ontology/{ds}/search`
    /// answers: a dict whose `results` each have the `iri`, `kinds`, `label`,
    /// `shortForm`, `deprecated` and the `match` with its field, text and highlight, and
    /// whose `more` says whether more follow. `kind` keeps some kinds, such as `class` or
    /// `property`, and `lang` names the caller's languages, most preferred first.
    #[pyo3(signature=(q, graph=None, *, kind=None, lang=None, limit=10, offset=0, fuzzy=true, definitions=true, imports=true, cancel=None, timeout=None))]
    #[allow(clippy::too_many_arguments)]
    fn search<'py>(
        &self,
        py: Python<'py>,
        q: &str,
        graph: Option<&Bound<'py, PyAny>>,
        kind: Option<&Bound<'py, PyAny>>,
        lang: Option<&Bound<'py, PyAny>>,
        limit: usize,
        offset: usize,
        fuzzy: bool,
        definitions: bool,
        imports: bool,
        cancel: Option<&Bound<'py, PyAny>>,
        timeout: Option<f64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let mut a = base(graph, imports, "lenient", false)?;
        a.insert("q".into(), json!(q));
        a.insert("kind".into(), names_json(kind)?);
        a.insert("lang".into(), names_json(lang)?);
        a.insert("limit".into(), json!(limit));
        a.insert("offset".into(), json!(offset));
        a.insert("fuzzy".into(), json!(fuzzy));
        a.insert("definitions".into(), json!(definitions));
        let j = self.run(py, "search", a, cancel, timeout, false)?;
        answer(py, j, &[])
    }

    /// One level of the class, property or concept hierarchy, as
    /// `GET /$/ontology/{ds}/hierarchy` answers it, read from the store's indexes without
    /// reading the whole ontology: a dict with the `node`, a page of its `children` (or
    /// of the roots when `iri` is None), each with its own numbers of `children` and
    /// `parents`, and its `ancestors` with `ancestors=True`. `kind` is `class`,
    /// `objectProperty`, `dataProperty`, `annotationProperty` or `concept`.
    #[pyo3(signature=(iri=None, graph=None, *, kind="class", ancestors=false, scheme=None, deprecated=true, lang=None, limit=100, offset=0, imports=true, at=None, cancel=None, timeout=None))]
    #[allow(clippy::too_many_arguments)]
    fn hierarchy<'py>(
        &self,
        py: Python<'py>,
        iri: Option<&str>,
        graph: Option<&Bound<'py, PyAny>>,
        kind: &str,
        ancestors: bool,
        scheme: Option<&str>,
        deprecated: bool,
        lang: Option<&Bound<'py, PyAny>>,
        limit: usize,
        offset: usize,
        imports: bool,
        at: Option<&Bound<'py, PyAny>>,
        cancel: Option<&Bound<'py, PyAny>>,
        timeout: Option<f64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let mut a = base(graph, imports, "lenient", false)?;
        a.insert("at".into(), at_json(at)?);
        a.insert("kind".into(), json!(kind));
        a.insert("iri".into(), json!(iri));
        a.insert("ancestors".into(), json!(ancestors));
        a.insert("scheme".into(), json!(scheme));
        a.insert("deprecated".into(), json!(deprecated));
        a.insert("lang".into(), names_json(lang)?);
        a.insert("limit".into(), json!(limit));
        a.insert("offset".into(), json!(offset));
        let j = self.run(py, "hierarchy", a, cancel, timeout, false)?;
        answer(py, j, &[])
    }

    /// A lexical page of asserted named individuals, their type buckets, or one
    /// individual's memberships. Continue with `after` and the returned `commit` as `at`.
    #[pyo3(signature=(graph=None, *, r#type=None, untyped=false, iri=None, after=None, limit=100, lang=None, imports=true, at=None, cancel=None, timeout=None))]
    #[allow(clippy::too_many_arguments)]
    fn individuals<'py>(
        &self,
        py: Python<'py>,
        graph: Option<&Bound<'py, PyAny>>,
        r#type: Option<&str>,
        untyped: bool,
        iri: Option<&str>,
        after: Option<&str>,
        limit: usize,
        lang: Option<&Bound<'py, PyAny>>,
        imports: bool,
        at: Option<&Bound<'py, PyAny>>,
        cancel: Option<&Bound<'py, PyAny>>,
        timeout: Option<f64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let mut a = base(graph, imports, "lenient", false)?;
        a.insert("at".into(), at_json(at)?);
        a.insert("type".into(), json!(r#type));
        a.insert("untyped".into(), json!(untyped));
        a.insert("iri".into(), json!(iri));
        a.insert("after".into(), json!(after));
        a.insert("limit".into(), json!(limit));
        a.insert("lang".into(), names_json(lang)?);
        let j = self.run(py, "individuals", a, cancel, timeout, false)?;
        answer(py, j, &[])
    }

    /// Read an existing kept classification without classifying on a miss. Page
    /// continuations require its `result_id` and exact returned commit as `at`.
    #[pyo3(signature=(graph=None, *, part="status", about=None, offset=0, limit=100, result_id=None, lang=None, imports=true, mode="lenient", at=None, cancel=None, timeout=None))]
    #[allow(clippy::too_many_arguments)]
    fn kept_classification<'py>(
        &self,
        py: Python<'py>,
        graph: Option<&Bound<'py, PyAny>>,
        part: &str,
        about: Option<&str>,
        offset: usize,
        limit: usize,
        result_id: Option<&str>,
        lang: Option<&Bound<'py, PyAny>>,
        imports: bool,
        mode: &str,
        at: Option<&Bound<'py, PyAny>>,
        cancel: Option<&Bound<'py, PyAny>>,
        timeout: Option<f64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let mut a = base(graph, imports, mode, false)?;
        a.remove("inferences");
        a.insert("at".into(), at_json(at)?);
        a.insert("part".into(), json!(part));
        a.insert("about".into(), json!(about));
        a.insert("offset".into(), json!(offset));
        a.insert("limit".into(), json!(limit));
        a.insert("resultId".into(), json!(result_id));
        a.insert("lang".into(), names_json(lang)?);
        let j = self.run(py, "keptClassification", a, cancel, timeout, false)?;
        answer(py, j, &[])
    }

    /// What the graphs say about the entity `iri`, as `GET /$/ontology/{ds}/term` answers
    /// it, read without reading the whole ontology: a dict with its `statements` and
    /// their annotations, the `axioms` they map to, the axioms of other entities that
    /// mention it (`mentions`), the statements that point at it (`incoming`), and the
    /// label, short form and kinds of every IRI used (`names`).
    #[pyo3(signature=(iri, graph=None, *, mentions=200, incoming=20, lang=None, imports=true, at=None, cancel=None, timeout=None))]
    #[allow(clippy::too_many_arguments)]
    fn term<'py>(
        &self,
        py: Python<'py>,
        iri: &str,
        graph: Option<&Bound<'py, PyAny>>,
        mentions: usize,
        incoming: usize,
        lang: Option<&Bound<'py, PyAny>>,
        imports: bool,
        at: Option<&Bound<'py, PyAny>>,
        cancel: Option<&Bound<'py, PyAny>>,
        timeout: Option<f64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let mut a = base(graph, imports, "lenient", false)?;
        a.insert("at".into(), at_json(at)?);
        a.insert("iri".into(), json!(iri));
        a.insert("mentions".into(), json!(mentions));
        a.insert("incoming".into(), json!(incoming));
        a.insert("lang".into(), names_json(lang)?);
        let j = self.run(py, "term", a, cancel, timeout, false)?;
        answer(py, j, &[])
    }

    /// What the ontology says about the entity `iri`: its `kinds`, `labels` and
    /// `annotations`, and for each kind a dict of its accessors, such as `superClasses`
    /// under `class` or `domains` under `objectProperty`. Each expression is a dict with
    /// its functional-style form `ofn` and its JSON form `expression`. With `assertions`,
    /// the instances of a class and the types and property assertions of an individual
    /// also come from the store, at most `limit` of each. An IRI the ontology does not
    /// have as an entity, or as `kind`, raises `NotFoundError`.
    #[pyo3(signature=(iri, graph=None, *, kind=None, assertions=false, limit=100, imports=true, mode="lenient", at=None, inferences=false, cancel=None, timeout=None))]
    #[allow(clippy::too_many_arguments)]
    fn entity<'py>(
        &self,
        py: Python<'py>,
        iri: &Bound<'py, PyAny>,
        graph: Option<&Bound<'py, PyAny>>,
        kind: Option<String>,
        assertions: bool,
        limit: usize,
        imports: bool,
        mode: &str,
        at: Option<&Bound<'py, PyAny>>,
        inferences: bool,
        cancel: Option<&Bound<'py, PyAny>>,
        timeout: Option<f64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let mut a = base(graph, imports, mode, inferences)?;
        a.insert("at".into(), at_json(at)?);
        a.insert(
            "iri".into(),
            J::String(crate::terms::iri_from_py(iri)?.into_string()),
        );
        a.insert("kind".into(), json!(kind));
        a.insert("assertions".into(), json!(assertions));
        a.insert("limit".into(), json!(limit));
        let j = self.run(py, "entity", a, cancel, timeout, false)?;
        let out = answer(py, j.clone(), &[])?;
        // property assertions carry an `Axiom`, as listings do
        if let Some(J::Array(list)) = j.pointer("/namedIndividual/assertions") {
            let target = out.get_item("namedIndividual")?.get_item("assertions")?;
            for (n, e) in list.iter().enumerate() {
                let Some(v) = e.get("axiom") else { continue };
                let ax = owl::json::read_axiom(v)
                    .map_err(|e| PyValueError::new_err(format!("unreadable axiom: {e}")))?;
                target
                    .get_item(n)?
                    .set_item("axiom", Bound::new(py, PyAxiom(ax))?)?;
            }
        }
        Ok(out)
    }

    /// One report per profile (`el`, `ql`, `rl`, `dl`; `dl` by default), over the
    /// terminology and, unless `assertions` is false, the assertions.
    #[pyo3(signature=(graph=None, *, profiles=None, assertions=true, max_violations=None, imports=true, mode="lenient", at=None, inferences=false, cancel=None, timeout=None))]
    #[allow(clippy::too_many_arguments)]
    fn check<'py>(
        &self,
        py: Python<'py>,
        graph: Option<&Bound<'py, PyAny>>,
        profiles: Option<&Bound<'py, PyAny>>,
        assertions: bool,
        max_violations: Option<usize>,
        imports: bool,
        mode: &str,
        at: Option<&Bound<'py, PyAny>>,
        inferences: bool,
        cancel: Option<&Bound<'py, PyAny>>,
        timeout: Option<f64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let mut a = base(graph, imports, mode, inferences)?;
        a.insert("at".into(), at_json(at)?);
        a.insert("profiles".into(), profiles_json(profiles)?);
        a.insert("assertions".into(), json!(assertions));
        a.insert("maxViolations".into(), json!(max_violations));
        let j = self.run(py, "check", a, cancel, timeout, false)?;
        answer(py, j, &[])
    }

    /// Add and remove axioms in one commit. An axiom is an `Axiom`, a str in
    /// functional-style syntax with the dataset's prefixes, or a dict in its JSON form.
    /// Added axioms go to `target` (the first graph by default) with the declarations
    /// the ontology lacks, unless `declare` is false.
    #[pyo3(signature=(graph=None, *, add=None, remove=None, declare=None, target=None, message=None, imports=true, mode="lenient", cancel=None, timeout=None))]
    #[allow(clippy::too_many_arguments)]
    fn edit<'py>(
        &self,
        py: Python<'py>,
        graph: Option<&Bound<'py, PyAny>>,
        add: Option<&Bound<'py, PyAny>>,
        remove: Option<&Bound<'py, PyAny>>,
        declare: Option<bool>,
        target: Option<&Bound<'py, PyAny>>,
        message: Option<String>,
        imports: bool,
        mode: &str,
        cancel: Option<&Bound<'py, PyAny>>,
        timeout: Option<f64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let mut a = base(graph, imports, mode, false)?;
        a.remove("inferences");
        a.insert("add".into(), axioms_json(add)?);
        a.insert("remove".into(), axioms_json(remove)?);
        a.insert("declare".into(), json!(declare));
        a.insert("target".into(), graph_json(target)?);
        a.insert("message".into(), json!(message));
        let j = self.run(py, "edit", a, cancel, timeout, true)?;
        answer(py, j, &[])
    }

    /// Remove an entity: its declarations, the axioms that mention it and the
    /// assertions about it, in one commit.
    #[pyo3(signature=(iri, graph=None, *, message=None, imports=true, mode="lenient", cancel=None, timeout=None))]
    #[allow(clippy::too_many_arguments)]
    fn remove_entity<'py>(
        &self,
        py: Python<'py>,
        iri: &Bound<'py, PyAny>,
        graph: Option<&Bound<'py, PyAny>>,
        message: Option<String>,
        imports: bool,
        mode: &str,
        cancel: Option<&Bound<'py, PyAny>>,
        timeout: Option<f64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let mut a = base(graph, imports, mode, false)?;
        a.remove("inferences");
        a.insert(
            "iri".into(),
            J::String(crate::terms::iri_from_py(iri)?.into_string()),
        );
        a.insert("message".into(), json!(message));
        let j = self.run(py, "removeEntity", a, cancel, timeout, true)?;
        answer(py, j, &[])
    }

    /// The axioms that differ between two commits (`to_commit` is the head by default),
    /// with the assertions the changed triples carry when `assertions` is true.
    #[pyo3(signature=(from_commit, to_commit=None, graph=None, *, assertions=false, limit=None, imports=true, mode="lenient", cancel=None, timeout=None))]
    #[allow(clippy::too_many_arguments)]
    fn diff<'py>(
        &self,
        py: Python<'py>,
        from_commit: &Bound<'py, PyAny>,
        to_commit: Option<&Bound<'py, PyAny>>,
        graph: Option<&Bound<'py, PyAny>>,
        assertions: bool,
        limit: Option<usize>,
        imports: bool,
        mode: &str,
        cancel: Option<&Bound<'py, PyAny>>,
        timeout: Option<f64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let mut a = base(graph, imports, mode, false)?;
        a.remove("inferences");
        a.insert("from".into(), at_json(Some(from_commit))?);
        a.insert("to".into(), at_json(to_commit)?);
        a.insert("assertions".into(), json!(assertions));
        a.insert("limit".into(), json!(limit));
        let j = self.run(py, "diff", a, cancel, timeout, false)?;
        answer(py, j, &["added", "removed"])
    }

    /// Classify by OWL 2 EL (spec F24): whether the ontology is consistent, its
    /// unsatisfiable classes, the axioms left out or used in part, and on request one
    /// class (`about`) and the taxonomy's nodes (`nodes`, `limit` from `offset`).
    /// `target` writes the taxonomy to that named graph in one commit. `partial` answers
    /// what a run with the disjunctive rules proved when `timeout` or `cancel` stops it,
    /// with `partial` true, the `incomplete` classes and the `consistency`, and
    /// `expansion` (`cautious` or `eager`) picks those rules' expansion strategy.
    #[pyo3(signature=(graph=None, *, assertions=false, about=None, nodes=false, limit=None, offset=None, threads=None, target=None, message=None, partial=false, expansion=None, projection=None, lang=None, imports=true, mode="lenient", at=None, cancel=None, timeout=None))]
    #[allow(clippy::too_many_arguments)]
    fn classify<'py>(
        &self,
        py: Python<'py>,
        graph: Option<&Bound<'py, PyAny>>,
        assertions: bool,
        about: Option<&Bound<'py, PyAny>>,
        nodes: bool,
        limit: Option<usize>,
        offset: Option<usize>,
        threads: Option<usize>,
        target: Option<&Bound<'py, PyAny>>,
        message: Option<String>,
        partial: bool,
        expansion: Option<String>,
        projection: Option<String>,
        lang: Option<&Bound<'py, PyAny>>,
        imports: bool,
        mode: &str,
        at: Option<&Bound<'py, PyAny>>,
        cancel: Option<&Bound<'py, PyAny>>,
        timeout: Option<f64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let mut a = base(graph, imports, mode, false)?;
        a.remove("inferences");
        a.insert("at".into(), at_json(at)?);
        a.insert("assertions".into(), json!(assertions));
        a.insert("partial".into(), json!(partial));
        a.insert("expansion".into(), json!(expansion));
        a.insert("projection".into(), json!(projection));
        a.insert("lang".into(), names_json(lang)?);
        if let Some(c) = about {
            a.insert(
                "about".into(),
                J::String(crate::terms::iri_from_py(c)?.into_string()),
            );
        }
        a.insert("nodes".into(), json!(nodes));
        a.insert("limit".into(), json!(limit));
        a.insert("offset".into(), json!(offset));
        a.insert("threads".into(), json!(threads));
        let writes = target.is_some();
        a.insert("target".into(), graph_json(target)?);
        a.insert("message".into(), json!(message));
        let j = self.run(py, "classify", a, cancel, timeout, writes)?;
        answer(py, j, &["excluded"])
    }

    /// Read Manchester text against the ontology's names and labels (item 608) as `as_`
    /// (`classExpression`, `dataRange`, `individual`, `literal` or `axiom`): the value
    /// in its JSON form, `ofn` and `manchester`. Text that does not read raises
    /// `InvalidInputError` with `reason`, the span `start` and `end` in UTF-16 code
    /// units and, for an ambiguous name, `candidates`.
    #[pyo3(signature=(text, graph=None, *, as_="classExpression", imports=true, mode="lenient", at=None, cancel=None, timeout=None))]
    #[allow(clippy::too_many_arguments)]
    fn parse<'py>(
        &self,
        py: Python<'py>,
        text: String,
        graph: Option<&Bound<'py, PyAny>>,
        as_: &str,
        imports: bool,
        mode: &str,
        at: Option<&Bound<'py, PyAny>>,
        cancel: Option<&Bound<'py, PyAny>>,
        timeout: Option<f64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let mut a = base(graph, imports, mode, false)?;
        a.remove("inferences");
        a.insert("at".into(), at_json(at)?);
        a.insert("text".into(), J::String(text));
        a.insert("as".into(), json!(as_));
        let j = self.run(py, "parse", a, cancel, timeout, false)?;
        answer(py, j, &[])
    }

    /// A DL query (spec U12): the equivalents, superclasses, subclasses and instances
    /// of a class expression in Manchester syntax, functional-style syntax
    /// (`syntax="functional"`) or the JSON form (a dict). The EL classifier answers
    /// inside OWL 2 EL and the RL rules by demand outside it; `reasoning` (`auto`, `el`
    /// or `rl`) chooses, and the answer's `reasoning` names the engine, whether the
    /// answers are complete and the axioms that can make them incomplete.
    #[pyo3(signature=(expression, graph=None, *, syntax=None, reasoning="auto", answers=None, limit=None, threads=None, max_derived=None, imports=true, mode="lenient", at=None, cancel=None, timeout=None))]
    #[allow(clippy::too_many_arguments)]
    fn dl_query<'py>(
        &self,
        py: Python<'py>,
        expression: &Bound<'py, PyAny>,
        graph: Option<&Bound<'py, PyAny>>,
        syntax: Option<&str>,
        reasoning: &str,
        answers: Option<Vec<String>>,
        limit: Option<usize>,
        threads: Option<usize>,
        max_derived: Option<usize>,
        imports: bool,
        mode: &str,
        at: Option<&Bound<'py, PyAny>>,
        cancel: Option<&Bound<'py, PyAny>>,
        timeout: Option<f64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let mut a = base(graph, imports, mode, false)?;
        a.remove("inferences");
        a.insert("at".into(), at_json(at)?);
        let e = if let Ok(s) = expression.extract::<String>() {
            J::String(s)
        } else {
            admin::from_py::<J>(expression, "expression")?
        };
        a.insert("expression".into(), e);
        a.insert("syntax".into(), json!(syntax));
        a.insert("reasoning".into(), json!(reasoning));
        a.insert("answers".into(), json!(answers));
        a.insert("limit".into(), json!(limit));
        a.insert("threads".into(), json!(threads));
        a.insert("maxDerived".into(), json!(max_derived));
        let j = self.run(py, "dlQuery", a, cancel, timeout, false)?;
        answer(py, j, &[])
    }
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyOntologies>()?;
    m.add_class::<PyAxiom>()?;
    Ok(())
}

//! The OWL 2 ontology of a dataset's graphs (spec F21 Phase 2): its summary, axioms,
//! profile checks, edits and diffs, and `Axiom`, a frozen axiom with structural equality
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
        let args = J::Object(args);
        interrupt::controlled(py, cancel, None, timeout, move |ctl| {
            let mut opts = sparkles::sparql::QueryOptions {
                cancel: Some(ctl.cancel.flag()),
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
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyOntologies>()?;
    m.add_class::<PyAxiom>()?;
    Ok(())
}

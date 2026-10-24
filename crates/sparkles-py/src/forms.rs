//! Editing forms generated from SHACL shapes (spec U02): the form model of a resource,
//! and the check and the save of the changes a form makes.
use crate::handles::handle;
use pyo3::prelude::*;

handle!(PyForms, "Forms");

#[cfg(feature = "shacl")]
mod imp {
    use super::*;
    use crate::dataset::at_from_py;
    use crate::errors::{EngineResult, invalid};
    use crate::terms::{iri_from_py, term_from_py};
    use crate::{admin, interrupt};
    use oxrdf::Term;
    use pyo3::types::PyMapping;
    use serde_json::{Value as J, json};
    use sparkles::commit::AnnotatedCommit;
    use sparkles::forms::{FieldChange, FormChange, FormCheck, FormRequest, FormSettings};
    use sparkles::overview::rdf_term_from_json;
    use sparkles::preview::Preview;
    use sparkles::sparql::QueryOptions;
    use std::collections::BTreeMap;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;
    use std::time::{Duration, Instant};

    /// The most changed quads a check lists.
    const LISTED: usize = 1000;

    /// The `forms` settings of a call: the defaults, or the members of `forms.json`.
    pub fn settings(py: Python<'_>, s: Option<&Bound<'_, PyAny>>) -> PyResult<FormSettings> {
        let Some(s) = s.filter(|s| !s.is_none()) else {
            return Ok(FormSettings::default());
        };
        let j: J = admin::from_py(s, "settings")?;
        let s: FormSettings =
            serde_json::from_value(j).map_err(|e| invalid(py, format!("settings: {e}")))?;
        s.validate()
            .map_err(|e| invalid(py, format!("settings: {e}")))?;
        Ok(s)
    }

    /// The options of a call: the deadline and cancellation of a read, and of a write.
    pub fn options(
        timeout: Option<f64>,
        cancel: Option<&Bound<'_, PyAny>>,
        author: Option<String>,
    ) -> PyResult<(QueryOptions, Arc<AtomicBool>)> {
        let flag = interrupt::flag(cancel)?;
        let mut opts = QueryOptions::default();
        if let Some(t) = timeout {
            let d = Duration::try_from_secs_f64(t).map_err(|_| {
                pyo3::exceptions::PyValueError::new_err("timeout must be finite and nonnegative")
            })?;
            opts.timeout = Some(d);
            opts.write.deadline = Instant::now().checked_add(d);
        }
        opts.cancel = Some(flag.clone());
        opts.write.cancel = Some(flag.clone());
        opts.write.author = author.map(Into::into);
        Ok((opts, flag))
    }

    fn string(m: &Bound<'_, PyAny>, key: &str) -> PyResult<Option<String>> {
        match get(m, key)? {
            None => Ok(None),
            Some(v) => v.extract::<String>().map(Some).map_err(|_| {
                pyo3::exceptions::PyTypeError::new_err(format!("{key} must be a str"))
            }),
        }
    }

    fn get<'py>(m: &Bound<'py, PyAny>, key: &str) -> PyResult<Option<Bound<'py, PyAny>>> {
        let v = m.call_method1("get", (key,))?;
        Ok((!v.is_none()).then_some(v))
    }

    /// A term: a dict of the SPARQL JSON results format, as the model lists them, or a
    /// `NamedNode` or `Literal`.
    fn term(ob: &Bound<'_, PyAny>, what: &str) -> PyResult<Term> {
        if ob.cast::<PyMapping>().is_ok() {
            let j: J = admin::from_py(ob, what)?;
            return rdf_term_from_json(&j, what).py(ob.py());
        }
        match term_from_py(ob)? {
            Term::BlankNode(_) => Err(invalid(
                ob.py(),
                format!("{what}: blank nodes cannot be named in a change"),
            )),
            t => Ok(t),
        }
    }

    fn terms(ob: Option<Bound<'_, PyAny>>, what: &str) -> PyResult<Vec<Term>> {
        let Some(ob) = ob else {
            return Ok(Vec::new());
        };
        ob.try_iter()
            .map_err(|_| {
                pyo3::exceptions::PyTypeError::new_err(format!("{what} must be a list of terms"))
            })?
            .enumerate()
            .map(|(i, t)| term(&t?, &format!("{what}[{i}]")))
            .collect()
    }

    /// A change as the body of the HTTP route spells it: `focus` or `class`, `changes`
    /// (each with a `field` and its `remove` and `add` terms), `expect` and `message`.
    pub fn change(
        py: Python<'_>,
        ob: &Bound<'_, PyAny>,
        settings: FormSettings,
    ) -> PyResult<FormChange> {
        let m = ob.cast::<PyMapping>().map_err(|_| {
            pyo3::exceptions::PyTypeError::new_err(
                "change must be a mapping with focus, changes and expect",
            )
        })?;
        let m = m.as_any();
        for k in m.call_method0("keys")?.try_iter()? {
            let k = k?;
            let k = k.extract::<String>().unwrap_or_default();
            if !matches!(
                k.as_str(),
                "focus" | "class" | "changes" | "expect" | "message"
            ) {
                return Err(invalid(
                    py,
                    format!("unknown member '{k}': focus, class, changes, expect and message"),
                ));
            }
        }
        let focus = get(m, "focus")?.map(|f| iri_from_py(&f)).transpose()?;
        let class = get(m, "class")?.map(|c| iri_from_py(&c)).transpose()?;
        if focus.is_some() == class.is_some() {
            return Err(invalid(
                py,
                "name either focus, the IRI of the resource, or class, the class of a new instance",
            ));
        }
        let mut changes = Vec::new();
        let list = get(m, "changes")?
            .ok_or_else(|| invalid(py, "changes: a list of {field, remove, add}"))?;
        for (i, c) in list
            .try_iter()
            .map_err(|_| invalid(py, "changes: a list of {field, remove, add}"))?
            .enumerate()
        {
            let c = c?;
            if c.cast::<PyMapping>().is_err() {
                return Err(invalid(
                    py,
                    format!("changes[{i}]: a mapping of field, remove and add"),
                ));
            }
            let field = string(&c, "field")?
                .ok_or_else(|| invalid(py, format!("changes[{i}].field: a field id")))?;
            changes.push(FieldChange {
                field,
                remove: terms(get(&c, "remove")?, &format!("changes[{i}].remove"))?,
                add: terms(get(&c, "add")?, &format!("changes[{i}].add"))?,
            });
        }
        let mut expect = BTreeMap::new();
        if let Some(e) = get(m, "expect")? {
            if e.cast::<PyMapping>().is_err() {
                return Err(invalid(
                    py,
                    "expect: a mapping of field ids to lists of terms",
                ));
            }
            for k in e.call_method0("keys")?.try_iter()? {
                let k = k?;
                let name: String = k
                    .extract()
                    .map_err(|_| invalid(py, "expect: the keys are field ids"))?;
                let v = e.get_item(&k)?;
                expect.insert(name.clone(), terms(Some(v), &format!("expect[{name}]"))?);
            }
        }
        Ok(FormChange {
            // a new instance takes an IRI minted from the forms setting
            focus: focus
                .map(|f| f.into_string())
                .unwrap_or_else(|| settings.mint_iri()),
            class: class.map(|c| c.into_string()),
            changes,
            expect,
            message: string(m, "message")?,
            settings,
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn model<'py>(
        py: Python<'py>,
        ds: sparkles::Dataset,
        focus: Option<&Bound<'py, PyAny>>,
        class: Option<&Bound<'py, PyAny>>,
        settings: Option<&Bound<'py, PyAny>>,
        at: Option<&Bound<'py, PyAny>>,
        lang: Option<String>,
        writable: bool,
        timeout: Option<f64>,
        cancel: Option<&Bound<'py, PyAny>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let focus = focus.filter(|f| !f.is_none());
        let class = class.filter(|c| !c.is_none());
        let at = at.filter(|a| !a.is_none());
        if focus.is_some() == class.is_some() {
            return Err(invalid(
                py,
                "name either focus, the IRI of a resource, or class_, the class of a new instance",
            ));
        }
        if class.is_some() && at.is_some() {
            return Err(invalid(
                py,
                "a form for a new instance reads the head: leave out at",
            ));
        }
        let settings = self::settings(py, settings)?;
        let class = class
            .map(|c| iri_from_py(c).map(|c| c.into_string()))
            .transpose()?;
        let focus = match focus {
            Some(f) => iri_from_py(f)?.into_string(),
            // a new instance's form reads an IRI minted for it, which holds nothing yet
            None => settings.mint_iri(),
        };
        let mut req = FormRequest::new(focus, settings);
        req.class = class;
        req.at = at.map(at_from_py).transpose()?;
        req.lang = lang;
        req.writable = writable;
        let (opts, flag) = options(timeout, cancel, None)?;
        let model = interrupt::run(py, &flag, move || ds.forms().model(&req, &opts))?;
        admin::to_py(py, &model)
    }

    /// The preview of a check as JSON: what the update would do and how it would end.
    fn preview_json(ds: &sparkles::Dataset, p: &Preview) -> J {
        let outcome = p.outcome();
        let commit = match &p.commit {
            Some(c) => {
                let note = sparkles::annotations::Annotation {
                    message: p.message.clone(),
                    digest: None,
                };
                let mut j = json!(AnnotatedCommit {
                    commit: c,
                    annotation: Some(&note),
                });
                // only known when the commit is made
                if let Some(m) = j.as_object_mut() {
                    m.remove("timestamp");
                }
                j
            }
            None => json!(AnnotatedCommit {
                commit: &p.head,
                annotation: ds.store().annotation(p.head.seq).as_ref(),
            }),
        };
        let graphs: Vec<J> = p
            .graphs
            .iter()
            .map(|g| {
                let name = match &g.graph {
                    oxrdf::GraphName::DefaultGraph => J::Null,
                    oxrdf::GraphName::NamedNode(n) => json!(n.as_str()),
                    oxrdf::GraphName::BlankNode(b) => json!(b.to_string()),
                };
                json!({ "graph": name, "inserted": g.inserted, "deleted": g.deleted })
            })
            .collect();
        let mut doc = json!({
            "dryRun": true,
            "committed": false,
            "wouldCommit": p.commit.is_some(),
            "outcome": outcome.name(),
            "head": p.head.seq,
            "commit": commit,
            "graphs": graphs,
        });
        if let Some(total) = p.changes_total {
            let quads: Vec<J> = p
                .changes
                .iter()
                .take(LISTED)
                .map(|(op, q)| {
                    json!({
                        "op": op.sign().to_string(),
                        "subject": q.subject.to_string(),
                        "predicate": q.predicate.to_string(),
                        "object": q.object.to_string(),
                        "graph": match &q.graph_name {
                            oxrdf::GraphName::DefaultGraph => J::Null,
                            g => json!(g.to_string()),
                        },
                    })
                })
                .collect();
            doc["changes"] = json!({
                "total": total,
                "limit": LISTED,
                "truncated": (quads.len() as u64) < total,
                "quads": quads,
            });
        }
        if let Some(v) = p.validation.as_deref() {
            let mut v = serde_json::to_value(v).unwrap_or(J::Null);
            if p.rejected() {
                v["head"] = json!(p.head.seq);
                v["kind"] = json!(p.kind.name());
            }
            doc["validation"] = v;
        }
        if let Some(e) = p.error() {
            doc["error"] = json!(e);
        }
        doc
    }

    fn check_json(ds: &sparkles::Dataset, c: &FormCheck) -> J {
        json!({
            "branch": ds.store().branch_name(),
            "preview": preview_json(ds, &c.preview),
            "update": c.update,
            "conflicts": c.conflicts,
            "validation": {
                "checked": c.results.is_some(),
                "withheld": c.withheld,
                "timedOut": c.timed_out,
                "conforms": c.conforms,
                "results": c.results,
            },
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn check<'py>(
        py: Python<'py>,
        ds: sparkles::Dataset,
        change_arg: &Bound<'py, PyAny>,
        settings_arg: Option<&Bound<'py, PyAny>>,
        author: Option<String>,
        timeout: Option<f64>,
        cancel: Option<&Bound<'py, PyAny>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let change = change(py, change_arg, settings(py, settings_arg)?)?;
        let (opts, flag) = options(timeout, cancel, author)?;
        let j = interrupt::run(py, &flag, move || {
            let c = ds.forms().check(&change, &opts)?;
            Ok(check_json(&ds, &c))
        })?;
        admin::to_py(py, &j)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn save<'py>(
        py: Python<'py>,
        ds: sparkles::Dataset,
        change_arg: &Bound<'py, PyAny>,
        settings_arg: Option<&Bound<'py, PyAny>>,
        author: Option<String>,
        timeout: Option<f64>,
        cancel: Option<&Bound<'py, PyAny>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let change = change(py, change_arg, settings(py, settings_arg)?)?;
        let (opts, flag) = options(timeout, cancel, author)?;
        let focus = change.focus.clone();
        let saved = interrupt::run(py, &flag, {
            let ds = ds.clone();
            move || ds.forms().save(&change, &opts)
        })?;
        match saved {
            sparkles::forms::FormSaved::Saved(receipt) => {
                let mut j = serde_json::to_value(&*receipt).unwrap_or_else(|_| json!({}));
                j["focus"] = json!(focus);
                j["branch"] = json!(ds.store().branch_name());
                admin::to_py(py, &j)
            }
            sparkles::forms::FormSaved::Conflict(fields) => {
                let ids: Vec<&str> = fields.iter().map(|f| f.id.as_str()).collect();
                let err = crate::errors::new_err(
                    py,
                    "ConflictError",
                    format!(
                        "another commit changed these fields since the form loaded them: {}",
                        ids.join(", ")
                    ),
                );
                let v = err.value(py);
                v.setattr("code", "form-conflict")?;
                v.setattr("fields", admin::to_py(py, &fields)?)?;
                Err(err)
            }
        }
    }
}

#[pymethods]
impl PyForms {
    /// The form of a resource, read under the settings' shapes: its groups, its fields
    /// with their widgets, constraints and values, and the triples no field covers
    /// (spec U02 section 3.1). `focus` is the resource, or `class_` the class of a new
    /// instance, whose form has no values. `settings` holds the members of the `forms`
    /// setting (`shapes`, `graphs`, `targetGraph`, `newIri` and the others), with
    /// `shapes` choosing between `guard`, `graphs` and `drafted`. `at` opens the form at
    /// a past state, which is never editable. `writable=False` makes every field
    /// read-only. Returns the model as a dict.
    #[pyo3(signature = (focus = None, *, class_ = None, settings = None, at = None, lang = None, writable = true, timeout = None, cancel = None))]
    #[allow(clippy::too_many_arguments)]
    fn model<'py>(
        &self,
        py: Python<'py>,
        focus: Option<&Bound<'py, PyAny>>,
        class_: Option<&Bound<'py, PyAny>>,
        settings: Option<&Bound<'py, PyAny>>,
        at: Option<&Bound<'py, PyAny>>,
        lang: Option<String>,
        writable: bool,
        timeout: Option<f64>,
        cancel: Option<&Bound<'py, PyAny>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let ds = self.ds(py)?;
        #[cfg(feature = "shacl")]
        {
            imp::model(
                py, ds, focus, class_, settings, at, lang, writable, timeout, cancel,
            )
        }
        #[cfg(not(feature = "shacl"))]
        {
            let _ = (
                ds, focus, class_, settings, at, lang, writable, timeout, cancel,
            );
            Err(missing(py))
        }
    }

    /// Preview a change as a dry run and validate the nodes it touches against the
    /// form's shapes (spec U02 section 3.2). `change` has `focus` (or `class` for a new
    /// instance), `changes` (a list of `{"field": id, "remove": [term], "add": [term]}`),
    /// `expect` (the values the form loaded, by field id, for every changed field) and
    /// `message`. A term is a dict of the model or a `NamedNode` or `Literal`. Nothing is
    /// written, and the dataset's writer lock is held while the check runs. Returns the
    /// preview, the update the change makes, the fields another commit changed since the
    /// form loaded them (`conflicts`) and the validation as a dict.
    #[pyo3(signature = (change, *, settings = None, author = None, timeout = None, cancel = None))]
    fn check<'py>(
        &self,
        py: Python<'py>,
        change: &Bound<'py, PyAny>,
        settings: Option<&Bound<'py, PyAny>>,
        author: Option<String>,
        timeout: Option<f64>,
        cancel: Option<&Bound<'py, PyAny>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let ds = self.write(py)?;
        #[cfg(feature = "shacl")]
        {
            imp::check(py, ds, change, settings, author, timeout, cancel)
        }
        #[cfg(not(feature = "shacl"))]
        {
            let _ = (ds, change, settings, author, timeout, cancel);
            Err(missing(py))
        }
    }

    /// Commit a change as one update on this dataset's branch, when the stored values of
    /// every changed field are still those `change["expect"]` names. Otherwise it writes
    /// nothing and raises `ConflictError` with `code == "form-conflict"` and `fields`,
    /// the list of `{"id", "now"}` of the fields that differ. `change` is as in `check`.
    /// To save on a session branch, call this on `Dataset.branch(name)`. Returns the
    /// receipt as a dict, with the `focus` and the `branch`.
    #[pyo3(signature = (change, *, settings = None, author = None, timeout = None, cancel = None))]
    fn save<'py>(
        &self,
        py: Python<'py>,
        change: &Bound<'py, PyAny>,
        settings: Option<&Bound<'py, PyAny>>,
        author: Option<String>,
        timeout: Option<f64>,
        cancel: Option<&Bound<'py, PyAny>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let ds = self.write(py)?;
        #[cfg(feature = "shacl")]
        {
            imp::save(py, ds, change, settings, author, timeout, cancel)
        }
        #[cfg(not(feature = "shacl"))]
        {
            let _ = (ds, change, settings, author, timeout, cancel);
            Err(missing(py))
        }
    }
}

#[cfg(not(feature = "shacl"))]
fn missing(py: Python<'_>) -> PyErr {
    crate::errors::new_err(py, "UnsupportedError", "shacl feature is disabled")
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyForms>()?;
    Ok(())
}

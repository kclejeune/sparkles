//! The form model, checks and saves (the `shacl` feature).

use super::{FormSettings, ShapesSource};
use crate::Dataset;
use crate::access::GraphAccess;
use crate::commit::{CommitKind, Receipt};
use crate::error::{Error, Result};
use crate::guard::Precondition;
use crate::history::{At, HistoryOptions};
use crate::id::Id;
use crate::index::Perm;
use crate::preview::{DryRun, Preview};
use crate::sparql::QueryOptions;
use crate::store::{Chunk, Snapshot};
use oxrdf::{GraphName, NamedNode, Quad, Term};
use parking_lot::Mutex;
use serde::Serialize;
use serde_json::{Value as J, json};
use sparkles_core::sparql::results::term_json;
use sparkles_shacl::form::{FieldGroup, FieldPath, FormField, FormLayout, form_layout, pick_lang};
use sparkles_shacl::{Shapes, ValidateOptions, ValidationReport};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// The version of the form model's JSON.
pub const FORM_FORMAT: u32 = 1;
/// The most values one field lists; a field with more is read-only.
pub const MAX_FIELD_VALUES: usize = 200;
/// The most predicates the other properties list, and values per predicate.
pub const MAX_OTHER: usize = 100;
/// The most terms one change may remove and add, over all its fields.
pub const MAX_CHANGED_TERMS: usize = 1000;
/// How long the server check of a change may validate (spec U02 §2.8).
pub const CHECK_TIMEOUT: Duration = Duration::from_secs(10);
/// How many times a save plans its update again when other commits move the values of
/// its fields between graphs while it waits for the writer.
pub const SAVE_ROUNDS: usize = 3;
/// The most additional distinct affected targets admitted beyond directly touched nodes.
pub const MAX_DEPENDENTS: usize = 500;

#[path = "nested.rs"]
mod nested;
pub use nested::{
    MAX_NESTED_ANCHORS, MAX_NESTED_DEPTH, MAX_NESTED_FIELDS, MAX_NESTED_NODES, NestedAnchor,
    NestedFormModel, NestedNode, NestedTemplate,
};

#[path = "edit.rs"]
mod edit;
use edit::{Guard, Plan};
pub use edit::{MAX_LIST_ITEMS, MAX_SUBTREE_QUADS, NodeChange};

#[path = "wire.rs"]
mod wire;
pub use wire::{expect_from_json, field_changes_from_json, nested_from_json, term_from_json};

const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
use sparkles_core::INFERRED_GRAPH as INFERRED;
const LABELS: [&str; 5] = [
    "http://www.w3.org/2004/02/skos/core#prefLabel",
    "http://www.w3.org/2000/01/rdf-schema#label",
    "http://schema.org/name",
    "http://xmlns.com/foaf/0.1/name",
    "http://purl.org/dc/terms/title",
];

// ------------------------------------------------------------------- the model ------

/// A request for the form model of a resource.
#[derive(Clone, Debug)]
pub struct FormRequest {
    /// the resource, an IRI
    pub focus: String,
    /// the state to read (`None`: the head); a form at a past state is read-only
    pub at: Option<At>,
    /// the language labels are picked in (then English, then untagged)
    pub lang: Option<String>,
    pub settings: FormSettings,
    /// whether the caller may write the branch (fields are read-only otherwise)
    pub writable: bool,
    /// a new instance of this class instead of `focus` (Phase 2): the form of the node
    /// shapes that would target it, with no values
    pub class: Option<String>,
    /// the view writes go through, which decides the fields and values that are
    /// writable: `None` takes the view of `opts.graphs` that reads go through, and
    /// `Some(None)` is a view that writes every graph
    pub write_graphs: Option<Option<Arc<GraphAccess>>>,
}

impl FormRequest {
    pub fn new(focus: impl Into<String>, settings: FormSettings) -> FormRequest {
        FormRequest {
            focus: focus.into(),
            at: None,
            lang: None,
            settings,
            writable: true,
            class: None,
            write_graphs: None,
        }
    }
}

/// The shapes a form was built from.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FormShapes {
    pub source: ShapesSource,
    /// the node shapes that target the resource
    pub node_shapes: Vec<String>,
    /// the shapes source has no shapes (no SHACL guard, say), with the reason
    #[serde(skip_serializing_if = "Option::is_none")]
    pub missing: Option<String>,
    /// DASH editors and viewers the form does not know, which fall back to the table
    pub unknown_hints: Vec<String>,
}

/// A group of fields.
#[derive(Clone, Debug, Serialize)]
pub struct FormGroup {
    /// the `sh:PropertyGroup`, or the node shape the fields were reached through
    pub iri: Option<String>,
    pub label: String,
    pub order: Option<f64>,
}

/// A value of a field.
#[derive(Clone, Debug, Serialize)]
pub struct FormValue {
    #[serde(serialize_with = "ser_term")]
    pub term: Term,
    /// the graph that holds it (`None`: the default graph)
    pub graph: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// the caller cannot write the graph that holds it
    #[serde(skip_serializing_if = "std::ops::Not::not", rename = "readOnly")]
    pub read_only: bool,
    /// the members of an `rdf:List` value of a list field, in order (`None`: not a list
    /// field, or the value is not a well-formed list)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub items: Option<Vec<FormValue>>,
}

fn ser_term<S: serde::Serializer>(t: &Term, s: S) -> std::result::Result<S::Ok, S::Error> {
    term_json(t).serialize(s)
}

/// One field of a form.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Field {
    /// the path in SPARQL syntax, stable across models of the same shapes
    pub id: String,
    /// the predicate IRI, or the SPARQL path text of an inverse or complex path
    pub path: String,
    /// an inverse path: the values are subjects
    pub inverse: bool,
    pub editable: bool,
    /// why the field is read-only: `readOnly`, `protected`, `graph`, `complex-path`,
    /// `too-many`, `list` (not exactly one well-formed `rdf:List`) or `state`
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<&'static str>,
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub group: Option<String>,
    pub order: Option<f64>,
    pub widget: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub editor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub viewer: Option<String>,
    /// the constraints the browser checks, and `server`: the components only the
    /// server checks
    pub constraints: J,
    /// the property shapes the field applies
    pub sources: Vec<String>,
    pub values: Vec<FormValue>,
    /// the field holds more values than [`MAX_FIELD_VALUES`]
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
}

/// The values of a predicate no property shape covers.
#[derive(Clone, Debug, Serialize)]
pub struct OtherProperty {
    pub predicate: String,
    pub values: Vec<FormValue>,
}

/// The form of a resource (spec U02 §3.1).
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FormModel {
    pub form_format: u32,
    pub focus: Option<String>,
    pub types: Vec<String>,
    pub commit: u64,
    pub branch: String,
    /// the caller may save changes through this form
    pub editable: bool,
    pub shapes: FormShapes,
    pub groups: Vec<FormGroup>,
    pub fields: Vec<Field>,
    pub other: Vec<OtherProperty>,
    /// where new values go (`None`: the default graph)
    pub graph: Option<String>,
    /// a node shape is `sh:closed true`: other properties are problems
    pub closed: bool,
}

// ------------------------------------------------------------------ changes ------

/// A change to one field.
///
/// A value to `remove` may be a stored blank node of a field that declares `sh:node`:
/// the change then also deletes the child's visible quads when nothing else refers to it.
/// A blank node in `add` names a new child that [`FormChange::nested`] describes.
#[derive(Clone, Debug, Default)]
pub struct FieldChange {
    /// the field's id
    pub field: String,
    pub remove: Vec<Term>,
    pub add: Vec<Term>,
    /// a list field (SHACL 1.2 list constraints): the members its `rdf:List` value takes,
    /// replacing the stored list; `remove` and `add` stay empty
    pub list: Option<Vec<Term>>,
}

/// The changes a form makes to a resource.
#[derive(Clone, Debug, Default)]
pub struct FormChange {
    /// the resource; with `class`, the IRI the new instance takes, which must not be the
    /// subject of any triple yet
    pub focus: String,
    /// a new instance of this class: the change also adds `focus rdf:type class`
    pub class: Option<String>,
    pub changes: Vec<FieldChange>,
    /// the values the form loaded, by field id: one entry for every changed field, and
    /// for a list field the members of its list in order
    pub expect: BTreeMap<String, Vec<Term>>,
    pub message: Option<String>,
    pub settings: FormSettings,
    /// changes to nested blank-node children, which commit atomically with `changes`
    pub nested: Vec<NodeChange>,
}

/// What a check found (spec U02 §3.2).
#[derive(Debug)]
pub struct FormCheck {
    /// the dry run of the update
    pub preview: Preview,
    /// the changed fields another commit changed since the form loaded them, which a
    /// save would answer with `Conflict`
    pub conflicts: Vec<FieldConflict>,
    /// the update the change makes
    pub update: String,
    /// the validation of the nodes the change touches against the form's shapes:
    /// `None` when it was not run (`withheld`) or timed out
    pub results: Option<Vec<J>>,
    pub conforms: Option<bool>,
    /// the validation took longer than [`CHECK_TIMEOUT`]
    pub timed_out: bool,
    /// the caller's view hides part of the data, so no validation ran
    pub withheld: bool,
}

/// A field whose stored values changed since the form loaded them.
#[derive(Clone, Debug, Serialize)]
pub struct FieldConflict {
    pub id: String,
    /// the nested node the field belongs to (`None`: the root)
    #[serde(
        skip_serializing_if = "Option::is_none",
        serialize_with = "ser_opt_term"
    )]
    pub node: Option<Term>,
    /// the stored values now, or for a list field the members of its list
    #[serde(serialize_with = "ser_terms")]
    pub now: Vec<Term>,
}

fn ser_opt_term<S: serde::Serializer>(
    t: &Option<Term>,
    s: S,
) -> std::result::Result<S::Ok, S::Error> {
    match t {
        Some(t) => term_json(t).serialize(s),
        None => s.serialize_none(),
    }
}

fn ser_terms<S: serde::Serializer>(t: &[Term], s: S) -> std::result::Result<S::Ok, S::Error> {
    J::Array(t.iter().map(term_json).collect()).serialize(s)
}

/// What a save did.
#[derive(Debug)]
pub enum FormSaved {
    Saved(Box<Receipt>),
    /// the stored values of these fields are not those the form loaded
    Conflict(Vec<FieldConflict>),
}

/// The editing forms of a dataset (from [`Dataset::forms`]).
///
/// ```
/// use sparkles::Dataset;
/// use sparkles::forms::{FieldChange, FormChange, FormRequest, FormSettings, ShapesSource};
///
/// let ds = Dataset::memory();
/// ds.update(r#"PREFIX ex: <http://ex/> PREFIX sh: <http://www.w3.org/ns/shacl#>
///     INSERT DATA {
///       GRAPH <urn:shapes> { ex:S a sh:NodeShape ; sh:targetClass ex:P ;
///         sh:property [ sh:path ex:name ; sh:name "Name" ; sh:maxCount 1 ] }
///       ex:a a ex:P ; ex:name "A" }"#)?;
/// let settings = FormSettings {
///     shapes: ShapesSource::Graphs,
///     graphs: vec!["urn:shapes".into()],
///     ..Default::default()
/// };
/// let req = FormRequest::new("http://ex/a", settings.clone());
/// let model = ds.forms().model(&req, &Default::default())?;
/// assert_eq!(model.fields[0].label, "Name");
/// let name = oxrdf::Literal::new_simple_literal("A");
/// let change = FormChange {
///     focus: "http://ex/a".into(),
///     class: None,
///     changes: vec![FieldChange {
///         field: model.fields[0].id.clone(),
///         remove: vec![name.clone().into()],
///         add: vec![oxrdf::Literal::new_simple_literal("Ann").into()],
///         list: None,
///     }],
///     expect: [(model.fields[0].id.clone(), vec![name.into()])].into(),
///     message: Some("rename".into()),
///     settings,
///     nested: vec![],
/// };
/// let saved = ds.forms().save(&change, &Default::default())?;
/// assert!(matches!(saved, sparkles::forms::FormSaved::Saved(_)));
/// # Ok::<_, sparkles::Error>(())
/// ```
#[derive(Clone)]
pub struct Forms {
    pub(crate) ds: Dataset,
}

impl Dataset {
    /// Editing forms generated from SHACL shapes.
    pub fn forms(&self) -> Forms {
        Forms { ds: self.clone() }
    }
}

impl Forms {
    /// The form model of a resource at `req.at` (the head when `None`), read under the
    /// view, deadline and cancellation of `opts`.
    pub fn model(&self, req: &FormRequest, opts: &QueryOptions) -> Result<FormModel> {
        let built = self.prepare_model(req, opts, None)?;
        built.model(
            req.writable && req.at.is_none(),
            &self.ds.store().branch_name(),
        )
    }

    fn prepare_model(
        &self,
        req: &FormRequest,
        opts: &QueryOptions,
        preparation: Option<nested::Preparation>,
    ) -> Result<Built> {
        let store = self.ds.store();
        let deadline = deadline(opts);
        let snap = match &req.at {
            None => store.snapshot(),
            Some(at) => {
                let o = HistoryOptions {
                    cancel: opts.cancel.clone(),
                    deadline,
                };
                store.snapshot_at(at, &o)?.0
            }
        };
        let mut built = self.build_prepared(
            &snap,
            &req.focus,
            &req.settings,
            opts,
            (
                req.class.as_deref(),
                match &req.write_graphs {
                    Some(w) => w.as_ref(),
                    None => opts.graphs.as_ref(),
                },
            ),
            preparation,
        )?;
        if let Some(lang) = &req.lang {
            built.limits.lexical(lang)?;
        }
        built.lang = req.lang.clone();
        Ok(built)
    }

    /// Preview a change as a dry run, and validate the nodes it touches at the state it
    /// would make against the form's shapes. The writer lock is held while it runs.
    /// `opts.write` carries the author, the view and the deadline of the write; with a
    /// view that hides part of the data, no validation runs (`withheld`), as the
    /// validation routes refuse such callers.
    pub fn check(&self, change: &FormChange, opts: &QueryOptions) -> Result<FormCheck> {
        let mut controlled = opts.clone();
        controlled.deadline = deadline(opts);
        let opts = &controlled;
        let store = self.ds.store();
        let head = store.snapshot();
        let mut built = self.build_for(&head, change, opts)?;
        let dry = DryRun {
            changes: 1000,
            all_changes: false,
            max_changes: 0,
            retain_states: false,
            state_max_memory: None,
        };
        let mut wopts = opts.write.clone();
        wopts.message = change.message.clone().map(Arc::from);
        wopts.precondition = None;
        wopts.deadline = wopts.deadline.into_iter().chain(opts.deadline).min();
        // a dry run from the start: however the check ends, the terms it added go again
        wopts.dry_run = Some(dry.clone());
        let mut txn = store.try_write_with(CommitKind::Update, wopts)?;
        // the change is planned on the state the transaction starts from
        if txn.base().commit != built.commit {
            let base = txn.base().clone();
            built = self.build_for(&base, change, opts)?;
        }
        let withheld = opts.graphs.as_ref().is_some_and(|a| !a.reads_everything());
        let plan = built.plan(change, !withheld && !built.shapes.is_empty())?;
        let guard = Guard::of(&built, &plan, change)?;
        let mut conflicts = guard
            .differ(txn.base(), opts.graphs.as_deref(), &Limits::of(opts))?
            .0;
        conflicts.extend(plan.lost.iter().cloned());
        let fresh = edit::apply(&mut txn, &plan, &built.limits)?;
        let (mut results, mut conforms, mut timed_out) = (None, None, false);
        if !withheld && !built.shapes.is_empty() {
            let post = Arc::new(txn.view());
            let mut v = built.vopts.clone();
            v.timeout = None;
            v.cancel = opts.cancel.clone();
            v.cancellation = opts.cancellation.clone();
            v.deadline = opts
                .deadline
                .into_iter()
                .chain([Instant::now() + CHECK_TIMEOUT])
                .min();
            v.parallel = false;
            let discovery = sparkles_shacl::incremental::AffectedLimits {
                additional_focus: MAX_DEPENDENTS,
                work: opts
                    .max_memory_bytes
                    .map_or(100_000, |bytes| (bytes / 128).min(100_000)),
            };
            let quads: Vec<Quad> = plan
                .quads
                .iter()
                .map(|q| edit::stored_quad(q, &fresh))
                .collect();
            let touched: Vec<Term> = plan
                .touched
                .iter()
                .map(|t| edit::stored(t, &fresh))
                .collect();
            match sparkles_shacl::incremental::validate_affected(
                txn.base(),
                &post,
                &built.shapes,
                &quads,
                &touched,
                &v,
                discovery,
            ) {
                Ok(report) => {
                    conforms = Some(report.conforms);
                    results = Some(built.map_results(&report, &plan, &fresh));
                }
                Err(e) if format!("{e:#}").contains("timed out") => timed_out = true,
                Err(e) => return Err(crate::handles::from_anyhow(e)),
            }
        }
        let preview = txn.preview(dry)?;
        Ok(FormCheck {
            preview,
            conflicts,
            update: plan.update(),
            results,
            conforms,
            timed_out,
            withheld,
        })
    }

    /// Commit a change as one update with its message, when the stored values of every
    /// changed field are still those `change.expect` names; otherwise answer the fields
    /// that differ and write nothing. The comparison runs under the writer lock, so no
    /// commit comes between it and the write. The update is planned on a state whose
    /// changed fields hold their values in the same graphs as the head the write starts
    /// from: when another commit moved one between graphs, the save plans again on the
    /// new head, up to [`SAVE_ROUNDS`] times. The write guard runs as for any write.
    pub fn save(&self, change: &FormChange, opts: &QueryOptions) -> Result<FormSaved> {
        let store = self.ds.store();
        let limits = Limits::of(opts);
        let mut head = store.snapshot();
        for _ in 0..SAVE_ROUNDS {
            let built = self.build_for(&head, change, opts)?;
            let plan = built.plan(change, false)?;
            // a nested node the form edits no longer hangs from its parent
            if !plan.lost.is_empty() {
                return Ok(FormSaved::Conflict(plan.lost));
            }
            let guard = Arc::new(Guard::of(&built, &plan, change)?);
            let access = opts.graphs.clone();
            let lim = limits.clone();
            let differ =
                Arc::new(move |snap: &Arc<Snapshot>| guard.differ(snap, access.as_deref(), &lim));
            // a conflict already at the head: no need to wait for the writer
            let (early, _) = differ(&head)?;
            if !early.is_empty() {
                return Ok(FormSaved::Conflict(early));
            }
            let found: Arc<Mutex<Vec<FieldConflict>>> = Default::default();
            let moved: Arc<Mutex<bool>> = Default::default();
            let (sink, moved_sink) = (found.clone(), moved.clone());
            let check = differ.clone();
            let mut wopts = opts.write.clone();
            wopts.message = change.message.clone().map(Arc::from);
            wopts.dry_run = None;
            wopts.precondition = Some(Precondition::new(move |head: &Snapshot| {
                let (c, stale) = check(&Arc::new(head.clone()))?;
                if !c.is_empty() {
                    *sink.lock() = c;
                } else if stale {
                    *moved_sink.lock() = true;
                } else {
                    return Ok(());
                }
                Err(Error::PreconditionFailed(
                    "the stored values of a field changed since the form loaded them".into(),
                ))
            }));
            let mut txn = match store.try_write_with(CommitKind::Update, wopts) {
                Ok(t) => t,
                Err(Error::PreconditionFailed(m)) => {
                    let c = std::mem::take(&mut *found.lock());
                    if !c.is_empty() {
                        return Ok(FormSaved::Conflict(c));
                    }
                    if *moved.lock() {
                        head = store.snapshot();
                        continue;
                    }
                    return Err(Error::PreconditionFailed(m));
                }
                Err(e) => return Err(e),
            };
            edit::apply(&mut txn, &plan, &limits)?;
            return Ok(FormSaved::Saved(Box::new(txn.commit()?)));
        }
        Err(Error::PreconditionFailed(format!(
            "other commits kept moving the values of the form between graphs: tried {SAVE_ROUNDS} times"
        )))
    }

    /// The fields of a change's resource at `snap`, writable through the view of `opts`.
    fn build_for(
        &self,
        snap: &Arc<Snapshot>,
        change: &FormChange,
        opts: &QueryOptions,
    ) -> Result<Built> {
        // the nested nodes a change edits come from the same bounded pool discovery as
        // the nested read model
        let preparation = (!change.nested.is_empty()).then(|| nested::Preparation::of(opts));
        self.build_prepared(
            snap,
            &change.focus,
            &change.settings,
            opts,
            (change.class.as_deref(), opts.graphs.as_ref()),
            preparation,
        )
    }

    /// The fields of a resource at `snap`.
    #[cfg(test)]
    fn build(
        &self,
        snap: &Arc<Snapshot>,
        focus: &str,
        settings: &FormSettings,
        opts: &QueryOptions,
        class: Option<&str>,
        write: Option<&Arc<GraphAccess>>,
    ) -> Result<Built> {
        self.build_prepared(snap, focus, settings, opts, (class, write), None)
    }

    fn build_prepared(
        &self,
        snap: &Arc<Snapshot>,
        focus: &str,
        settings: &FormSettings,
        opts: &QueryOptions,
        scope: (Option<&str>, Option<&Arc<GraphAccess>>),
        preparation: Option<nested::Preparation>,
    ) -> Result<Built> {
        let (class, write) = scope;
        let mut limits = Limits::of(opts);
        limits.preparation = preparation;
        limits.check()?;
        limits.lexical(focus)?;
        if let Some(class) = class {
            limits.lexical(class)?;
        }
        let focus = NamedNode::new(focus)
            .map_err(|e| Error::invalid(format!("focus: invalid IRI '{focus}': {e}")))?;
        let focus = Term::NamedNode(focus);
        let new_class = class
            .map(|c| {
                NamedNode::new(c.trim_start_matches('<').trim_end_matches('>'))
                    .map_err(|e| Error::invalid(format!("class: invalid IRI '{c}': {e}")))
            })
            .transpose()?;
        let access = opts.graphs.as_ref().map(|a| a.bound(snap));
        let write_access = write.map(|a| a.bound(snap));
        let mut read = Reader::new(snap, access.as_deref())?;
        read.limits = limits.clone();
        let types: Vec<String> = if let Some(c) = &new_class {
            vec![c.as_str().to_string()]
        } else {
            read.values(
                &focus,
                &FieldPath::Predicate(NamedNode::new_unchecked(RDF_TYPE)),
            )?
            .into_iter()
            .filter_map(|v| match v.term {
                Term::NamedNode(n) => Some(n.into_string()),
                _ => None,
            })
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
        };
        // shapes graphs are read under the view, without the triples it hides
        let (shapes, mut vopts, missing) =
            self.shapes(read.snap(), settings, &types, &access, opts)?;
        vopts.cancel = opts.cancel.clone();
        vopts.cancellation = opts.cancellation.clone();
        vopts.deadline = deadline(opts);
        // a caller limited to some graphs: decide the targets without the others
        if let Some(a) = access.as_deref()
            && !a.reads_all()
        {
            for g in snap.graph_ids()? {
                if let Some(Term::NamedNode(n)) = snap.term(g)
                    && !a.readable(Some(&Term::NamedNode(n.clone())))
                {
                    vopts.exclude_graphs.push(n.into_string());
                }
            }
        }
        let node_shapes = if shapes.is_empty() {
            Vec::new()
        } else if let Some(c) = &new_class {
            sparkles_shacl::shapes_for_class(
                read.snap(),
                &shapes,
                &Term::NamedNode(c.clone()),
                &vopts,
            )
            .map_err(crate::handles::from_anyhow)?
        } else {
            // A form build decides applicability before the check's later discovery.
            // Refuse an unadmitted target-list producer before that first evaluation.
            sparkles_shacl::incremental::check_target_producers(
                &shapes,
                &vopts,
                opts.max_memory_bytes
                    .map_or(100_000, |bytes| (bytes / 128).min(100_000)),
            )
            .map_err(crate::handles::from_anyhow)?;
            sparkles_shacl::shapes_for_node(read.snap(), &shapes, &focus, &vopts)
                .map_err(crate::handles::from_anyhow)?
        };
        let layout = if limits.preparation.is_some() {
            sparkles_shacl::form::form_layout_with(&shapes, &node_shapes, &mut || {
                limits.unit().map_err(anyhow::Error::new)
            })
            .map_err(crate::handles::from_anyhow)?
        } else {
            form_layout(&shapes, &node_shapes)
        };
        let target = target_graph(&read, &focus, &settings.target_graph)?;
        limits.lexical(&settings.target_graph)?;
        let target_policy = settings.target_graph.clone();
        let mut prefixes = crate::io::standard_prefixes();
        prefixes.extend(self.ds.store().prefixes());
        let (read_snap, masked) = (read.snap().clone(), read.masked);
        drop(read);
        Ok(Built {
            raw: snap.clone(),
            focus,
            new_class,
            types,
            commit: snap.commit,
            source: settings.shapes,
            shapes,
            vopts,
            missing,
            node_shapes,
            layout,
            target,
            target_policy,
            access,
            write_access,
            limits,
            snap: read_snap,
            masked,
            lang: None,
            prefixes,
        })
    }

    /// The shapes of `settings` over `snap`, their data graph, and why there are none.
    fn shapes(
        &self,
        snap: &Arc<Snapshot>,
        settings: &FormSettings,
        types: &[String],
        access: &Option<Arc<GraphAccess>>,
        opts: &QueryOptions,
    ) -> Result<(Arc<Shapes>, ValidateOptions, Option<String>)> {
        let mut vopts = ValidateOptions::default();
        let empty = || Arc::new(Shapes::default());
        match settings.shapes {
            ShapesSource::Guard => match self.ds.write_guard() {
                Some(crate::write_guard::WriteGuard::Shacl(g)) => {
                    let Some(o) = g.data_graph_options(snap) else {
                        return Ok((g.shapes(), vopts, None));
                    };
                    Ok((g.shapes(), o, None))
                }
                #[cfg(feature = "shex")]
                Some(crate::write_guard::WriteGuard::Shex(_)) => Ok((
                    empty(),
                    vopts,
                    Some("the write-time guard validates ShEx, and forms read SHACL".into()),
                )),
                None => Ok((
                    empty(),
                    vopts,
                    Some("the dataset has no write-time SHACL validation".into()),
                )),
            },
            ShapesSource::Graphs => {
                // a shapes graph the caller cannot read answers as one that does not exist
                if let Some(a) = access.as_deref()
                    && let Some(g) = settings.graphs.iter().find(|g| {
                        !a.readable(Some(&Term::NamedNode(NamedNode::new_unchecked(g.as_str()))))
                    })
                {
                    return Err(Error::NotFound(format!("no such shapes graph: <{g}>")));
                }
                let shapes = Shapes::from_store_graphs(snap, &settings.graphs)
                    .map_err(crate::handles::from_anyhow)?;
                vopts.exclude_graphs = settings.graphs.clone();
                vopts.exclude_graphs.push(INFERRED.into());
                Ok((Arc::new(shapes), vopts, None))
            }
            ShapesSource::Skos => {
                vopts.exclude_graphs.push(INFERRED.into());
                Ok((crate::skos::shapes(), vopts, None))
            }
            ShapesSource::Drafted => {
                if types.is_empty() {
                    return Ok((empty(), vopts, Some("the resource has no type".into())));
                }
                let mut d = crate::schema::draft::DraftOptions {
                    classes: types.to_vec(),
                    dataset: self.ds.name().unwrap_or("dataset").to_string(),
                    ..Default::default()
                };
                d.schema.deadline = deadline(opts);
                d.schema.graphs = access.clone();
                let draft = crate::schema::draft::draft_shapes(snap, &d).map_err(schema_error)?;
                let shapes = Shapes::parse(&draft.shacl, crate::io::RdfFormat::Turtle, None)
                    .map_err(crate::handles::from_anyhow)?;
                vopts.exclude_graphs.push(INFERRED.into());
                Ok((Arc::new(shapes), vopts, None))
            }
        }
    }
}

fn set(ts: &mut dyn Iterator<Item = Term>) -> BTreeMap<String, Term> {
    ts.map(|t| (t.to_string(), t)).collect()
}

fn schema_error(e: impl std::fmt::Display) -> Error {
    Error::invalid(format!("drafting shapes: {e}"))
}

fn deadline(opts: &QueryOptions) -> Option<Instant> {
    match (opts.deadline, opts.timeout) {
        (Some(d), _) => Some(d),
        (None, Some(t)) => Some(Instant::now() + t),
        _ => None,
    }
}

/// The graph new values go into: `None` is the default graph.
fn target_graph(read: &Reader<'_>, focus: &Term, policy: &str) -> Result<Option<String>> {
    Ok(match policy {
        "default" => None,
        "type" => {
            let typed = read.values(
                focus,
                &FieldPath::Predicate(NamedNode::new_unchecked(RDF_TYPE)),
            )?;
            // the default graph when it holds a type, else the first graph by name
            if typed.iter().any(|v| v.graph.is_none()) {
                None
            } else {
                typed.into_iter().filter_map(|v| v.graph).min()
            }
        }
        iri => {
            read.limits.lexical(iri)?;
            Some(iri.to_string())
        }
    })
}

/// The fields of a resource and what they were built from.
struct Built {
    focus: Term,
    /// a new instance of this class: `focus` is the IRI minted for it
    new_class: Option<NamedNode>,
    types: Vec<String>,
    commit: u64,
    source: ShapesSource,
    shapes: Arc<Shapes>,
    vopts: ValidateOptions,
    missing: Option<String>,
    node_shapes: Vec<usize>,
    layout: FormLayout,
    target: Option<String>,
    target_policy: String,
    access: Option<Arc<GraphAccess>>,
    /// the view writes go through, which decides what is writable
    write_access: Option<Arc<GraphAccess>>,
    limits: Limits,
    /// the state read, masked by the view
    snap: Arc<Snapshot>,
    /// the same state without the view's mask, for the references of removed children
    raw: Arc<Snapshot>,
    masked: bool,
    lang: Option<String>,
    prefixes: BTreeMap<String, String>,
}

/// Known DASH editors and viewers (spec U02 §2.3).
const KNOWN_HINTS: &[&str] = &[
    "TextFieldEditor",
    "TextAreaEditor",
    "TextFieldWithLangEditor",
    "DatePickerEditor",
    "DateTimePickerEditor",
    "BooleanSelectEditor",
    "EnumSelectEditor",
    "InstancesSelectEditor",
    "AutoCompleteEditor",
    "URIEditor",
    "DetailsEditor",
    "LabelViewer",
    "LiteralViewer",
    "URIViewer",
    "DetailsViewer",
];

fn known_hint(n: &NamedNode) -> bool {
    n.as_str()
        .strip_prefix(sparkles_shacl::vocab::DASH_NS)
        .is_some_and(|l| KNOWN_HINTS.contains(&l))
}

impl Built {
    fn read(&self) -> Result<Reader<'_>> {
        Ok(Reader {
            snap: self.snap.clone(),
            access: self.access.as_deref(),
            masked: self.masked,
            limits: self.limits.clone(),
        })
    }

    fn shape_name(&self, t: &Term) -> String {
        match t {
            Term::NamedNode(n) => n.as_str().to_string(),
            Term::BlankNode(b) => format!("_:{}", b.as_str()),
            t => t.to_string(),
        }
    }

    /// The short form of an IRI with the dataset's prefixes, or its local name.
    fn short(&self, iri: &str) -> String {
        let best = self
            .prefixes
            .iter()
            .filter(|(_, ns)| !ns.is_empty() && iri.starts_with(ns.as_str()))
            .max_by_key(|(_, ns)| ns.len());
        match best {
            Some((p, ns)) => format!("{p}:{}", &iri[ns.len()..]),
            None => iri
                .rsplit(['#', '/'])
                .find(|s| !s.is_empty())
                .unwrap_or(iri)
                .to_string(),
        }
    }

    /// The model. A read that the request's cancellation or deadline stops fails the
    /// whole model, rather than leaving fields empty.
    fn model(&self, writable: bool, branch: &str) -> Result<FormModel> {
        let read = self.read()?;
        let lang = self.lang.as_deref();
        let mut groups: Vec<FormGroup> = Vec::new();
        let mut fields = Vec::new();
        let mut covered: BTreeSet<String> = BTreeSet::new();
        let mut unknown: BTreeSet<String> = BTreeSet::new();
        if let Some(target) = &self.target {
            self.limits.lexical(target)?;
        }
        let target_term = self
            .target
            .as_ref()
            .map(|g| Term::NamedNode(NamedNode::new_unchecked(g.as_str())));
        for f in &self.layout.fields {
            if let FieldPath::Predicate(p) = &f.path {
                self.limits.lexical(p.as_str())?;
                covered.insert(p.as_str().to_string());
            }
            if self.hidden(f, target_term.as_ref()) {
                continue;
            }
            self.admit_metadata(f)?;
            for h in [&f.editor, &f.viewer].into_iter().flatten() {
                if !known_hint(h) {
                    unknown.insert(h.as_str().to_string());
                }
            }
            let group = f.group.as_ref().map(|g| {
                let node = g.node();
                let iri = self.shape_name(node);
                if !groups
                    .iter()
                    .any(|x| x.iri.as_deref() == Some(iri.as_str()))
                {
                    groups.push(self.group(g, lang));
                }
                iri
            });
            if let Some(preparation) = &self.limits.preparation {
                preparation.field()?;
            }
            let mut values = read.values(&self.focus, &f.path)?;
            let truncated = values.len() > MAX_FIELD_VALUES;
            values.truncate(MAX_FIELD_VALUES);
            for v in &mut values {
                v.read_only = !self.writable_graph(v.graph.as_deref());
                if let Term::NamedNode(n) = &v.term {
                    v.label = read.label(n, lang)?;
                }
            }
            // a list field edits exactly one well-formed rdf:List
            let mut list_ok = true;
            if f.constraints.list.is_some() {
                list_ok = values.len() <= 1 && !matches!(f.path, FieldPath::Inverse(_));
                for v in &mut values {
                    v.items = read.list_items(&v.term)?;
                    match &mut v.items {
                        None => list_ok = false,
                        Some(items) => {
                            for item in items {
                                item.read_only = v.read_only;
                                if let Term::NamedNode(n) = &item.term {
                                    item.label = read.label(n, lang)?;
                                }
                            }
                        }
                    }
                }
            }
            let reason = if !writable {
                Some("state")
            } else if f.read_only {
                Some("readOnly")
            } else if matches!(f.path, FieldPath::Complex(_)) {
                Some("complex-path")
            } else if !list_ok {
                Some("list")
            } else if self.protected(f, target_term.as_ref()) {
                Some("protected")
            } else if !self.writable_graph(self.target.as_deref()) {
                Some("graph")
            } else if truncated {
                Some("too-many")
            } else {
                None
            };
            let predicate = f.path.predicate().map(|p| p.as_str().to_string());
            let named = pick_lang(&f.names, lang).map(|l| l.value().to_string());
            let named = match (named, f.path.predicate()) {
                (None, Some(p)) => read.label(p, lang)?,
                (n, _) => n,
            };
            let label = named.unwrap_or_else(|| match &f.path {
                FieldPath::Predicate(p) => self.short(p.as_str()),
                FieldPath::Inverse(p) => format!("^{}", self.short(p.as_str())),
                FieldPath::Complex(p) => p.to_sparql(),
            });
            fields.push(Field {
                id: f.id(),
                path: match &f.path {
                    FieldPath::Predicate(_) => predicate.unwrap_or_default(),
                    p => p.sparql(),
                },
                inverse: matches!(f.path, FieldPath::Inverse(_)),
                editable: reason.is_none(),
                reason,
                label,
                description: pick_lang(&f.descriptions, lang).map(|l| l.value().to_string()),
                group,
                order: f.order,
                widget: f.widget(),
                editor: f.editor.as_ref().map(|n| n.as_str().to_string()),
                viewer: f.viewer.as_ref().map(|n| n.as_str().to_string()),
                constraints: constraints_json(f),
                sources: f
                    .sources
                    .iter()
                    .map(|si| self.shape_name(&self.shapes.shapes()[*si].node))
                    .collect(),
                values,
                truncated,
            });
        }
        // by group order (ungrouped first), then by sh:order, then by label
        groups.sort_by(|a, b| cmp_order(a.order, b.order).then_with(|| a.label.cmp(&b.label)));
        let rank = |g: &Option<String>| match g {
            None => 0,
            Some(iri) => {
                1 + groups
                    .iter()
                    .position(|x| x.iri.as_deref() == Some(iri))
                    .unwrap_or(0)
            }
        };
        fields.sort_by(|a, b| {
            rank(&a.group)
                .cmp(&rank(&b.group))
                .then_with(|| cmp_order(a.order, b.order))
                .then_with(|| a.label.cmp(&b.label))
        });
        let other = read
            .others(&self.focus, &covered)?
            .into_iter()
            .map(|(p, mut values)| {
                for v in &mut values {
                    v.read_only = true;
                }
                OtherProperty {
                    predicate: p,
                    values,
                }
            })
            .collect();
        for value in &self.types {
            self.limits.lexical(value)?;
        }
        self.limits.retained_term(&self.focus, 0)?;
        self.limits.lexical(branch)?;
        if let Some(value) = &self.target {
            self.limits.lexical(value)?;
        }
        for shape in &self.node_shapes {
            self.limits
                .retained_term(&self.shapes.shapes()[*shape].node, 0)?;
        }
        Ok(FormModel {
            form_format: FORM_FORMAT,
            focus: match &self.focus {
                Term::NamedNode(n) if self.new_class.is_none() => Some(n.as_str().to_string()),
                _ => None,
            },
            types: self.types.clone(),
            commit: self.commit,
            branch: branch.to_string(),
            editable: writable && !fields.is_empty() && fields.iter().any(|f| f.editable),
            shapes: FormShapes {
                source: self.source,
                node_shapes: self
                    .node_shapes
                    .iter()
                    .map(|si| self.shape_name(&self.shapes.shapes()[*si].node))
                    .collect(),
                missing: self.missing.clone(),
                unknown_hints: unknown.into_iter().collect(),
            },
            groups,
            fields,
            other,
            graph: self.target.clone(),
            closed: self.layout.closed,
        })
    }

    fn group(&self, g: &FieldGroup, lang: Option<&str>) -> FormGroup {
        let node = g.node();
        let iri = Some(self.shape_name(node));
        match g {
            FieldGroup::Group(t) => {
                let info = self.shapes.group(t);
                FormGroup {
                    iri,
                    label: info
                        .and_then(|i| pick_lang(&i.labels, lang))
                        .map(|l| l.value().to_string())
                        .unwrap_or_else(|| match t {
                            Term::NamedNode(n) => self.short(n.as_str()),
                            t => t.to_string(),
                        }),
                    order: info.and_then(|i| i.order),
                }
            }
            FieldGroup::Shape(t) => {
                let info = self.shapes.shape(t).map(|s| s.info());
                FormGroup {
                    iri,
                    label: info
                        .and_then(|i| pick_lang(&i.names, lang))
                        .map(|l| l.value().to_string())
                        .unwrap_or_else(|| match t {
                            Term::NamedNode(n) => self.short(n.as_str()),
                            t => t.to_string(),
                        }),
                    order: info.and_then(|i| i.order),
                }
            }
        }
    }

    /// Whether the view hides the field's predicate from reads in the graph new values go
    /// to, by a protection that names predicates and graphs only.
    fn hidden(&self, f: &FormField, target: Option<&Term>) -> bool {
        let (Some(p), Some(rules)) = (
            f.path.predicate(),
            self.access.as_ref().and_then(|a| a.triples.as_ref()),
        ) else {
            return false;
        };
        rules.rules.iter().any(|r| {
            !r.read.is_all()
                && r.protection.classes.is_none()
                && r.protection.pattern.is_none()
                && r.protection.covers_predicate(p.as_str())
                && r.protection.covers_graph(target)
                && !r.read.allows(target)
        })
    }

    /// Whether the write view's protections keep the caller from writing the field's
    /// predicate in the graph new values go to. Only protections that name predicates
    /// and graphs alone count here: one scoped by classes or a pattern depends on the
    /// data, and the write itself enforces it.
    fn protected(&self, f: &FormField, target: Option<&Term>) -> bool {
        let (Some(p), Some(rules)) = (
            f.path.predicate(),
            self.write_access.as_ref().and_then(|a| a.triples.as_ref()),
        ) else {
            return false;
        };
        rules.rules.iter().any(|r| {
            !r.write.is_all()
                && r.protection.classes.is_none()
                && r.protection.pattern.is_none()
                && r.protection.covers_predicate(p.as_str())
                && r.protection.covers_graph(target)
                && !r.write.allows(target)
        })
    }

    fn writable_graph(&self, g: Option<&str>) -> bool {
        writable_in(self.write_access.as_deref(), g)
    }

    /// The results of a validation as JSON, each with the field it belongs to (`null`
    /// for a result on another focus node or a path no field has), and the nested node
    /// whose field it is.
    fn map_results(
        &self,
        report: &ValidationReport,
        plan: &Plan,
        fresh: &std::collections::HashMap<String, Id>,
    ) -> Vec<J> {
        report
            .results
            .iter()
            .map(|r| {
                let mut j = sparkles_shacl::report::result_json(r);
                let field_of = |layout: &FormLayout| {
                    r.result_path.as_ref().and_then(|p| {
                        layout
                            .fields
                            .iter()
                            .find(|f| f.path.matches(p))
                            .map(|f| f.id())
                    })
                };
                if r.focus_node == self.focus {
                    j["field"] = json!(field_of(&self.layout));
                } else if let Some((_, named, layout)) = plan
                    .nodes
                    .iter()
                    .find(|(t, _, _)| edit::stored(t, fresh) == r.focus_node)
                {
                    // a new child is named as the request names it
                    j["field"] = json!(field_of(layout));
                    j["node"] = term_json(named);
                } else {
                    j["field"] = J::Null;
                }
                j
            })
            .collect()
    }
}

/// Whether a view lets writes into a graph (`None`: the default graph). The inferred
/// graph is never written by a form.
fn writable_in(access: Option<&GraphAccess>, g: Option<&str>) -> bool {
    if g == Some(INFERRED) {
        return false;
    }
    match access {
        None => true,
        Some(a) => {
            let t = g.map(|g| Term::NamedNode(NamedNode::new_unchecked(g)));
            a.writable(t.as_ref())
        }
    }
}

/// Whether `focus` is the subject of a triple the reader sees.
fn describes(read: &Reader<'_>, focus: &Term) -> Result<bool> {
    Ok(match read.snap().lookup_term(focus) {
        None => false,
        Some(id) => !read.scan(Perm::Spo, &[id.0], 1)?.is_empty(),
    })
}

fn used_iri(focus: &Term) -> Error {
    Error::invalid(format!(
        "focus: {focus} already describes a resource; a new instance needs a new IRI"
    ))
}

/// The cancellation and the deadline of a request, checked while forms read.
#[derive(Clone, Default)]
struct Limits {
    cancel: Option<Arc<std::sync::atomic::AtomicBool>>,
    cancellation: Option<sparkles_core::task::Cancellation>,
    deadline: Option<Instant>,
    preparation: Option<nested::Preparation>,
}

impl Limits {
    fn of(opts: &QueryOptions) -> Limits {
        Limits {
            cancel: opts.cancel.clone(),
            cancellation: opts.cancellation.clone(),
            deadline: deadline(opts),
            preparation: None,
        }
    }

    fn unit(&self) -> Result<()> {
        self.check()?;
        if let Some(preparation) = &self.preparation {
            preparation.unit()?;
        }
        Ok(())
    }

    fn lexical(&self, value: &str) -> Result<()> {
        if self.preparation.is_some() {
            for _ in 0..value.len().div_ceil(128) {
                self.unit()?;
            }
        }
        Ok(())
    }

    fn retained_term(&self, value: &Term, depth: usize) -> Result<()> {
        if self.preparation.is_none() {
            return Ok(());
        }
        self.unit()?;
        if depth > 64 {
            return Err(Error::invalid("form term preparation nesting exceeded"));
        }
        match value {
            Term::NamedNode(n) => self.lexical(n.as_str()),
            Term::BlankNode(n) => self.lexical(n.as_str()),
            Term::Literal(l) => {
                self.lexical(l.value())?;
                self.lexical(l.datatype().as_str())?;
                if let Some(language) = l.language() {
                    self.lexical(language)?;
                }
                Ok(())
            }
            Term::Triple(triple) => {
                match &triple.subject {
                    oxrdf::NamedOrBlankNode::NamedNode(n) => self.lexical(n.as_str())?,
                    oxrdf::NamedOrBlankNode::BlankNode(n) => self.lexical(n.as_str())?,
                }
                self.lexical(triple.predicate.as_str())?;
                self.retained_term(&triple.object, depth + 1)
            }
        }
    }

    fn check(&self) -> Result<()> {
        if self
            .cancel
            .as_ref()
            .is_some_and(|c| c.load(std::sync::atomic::Ordering::Relaxed))
        {
            return Err(Error::Cancelled);
        }
        if let Some(c) = &self.cancellation {
            c.check()?;
        }
        if self.deadline.is_some_and(|d| Instant::now() >= d) {
            return Err(Error::Timeout);
        }
        Ok(())
    }
}

fn cmp_order(a: Option<f64>, b: Option<f64>) -> std::cmp::Ordering {
    match (a, b) {
        (Some(a), Some(b)) => a.partial_cmp(&b).unwrap_or(std::cmp::Ordering::Equal),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    }
}

/// The constraints of a field the browser checks, as JSON.
fn constraints_json(f: &FormField) -> J {
    value_constraints_json(&f.constraints, f.single_line, f.default_value.as_ref())
}

/// The label of an option of an alternative: its `sh:name`, else what it constrains.
fn option_label(o: &sparkles_shacl::form::Alternative, n: usize) -> String {
    let local = |iri: &str| {
        iri.rsplit(['#', '/'])
            .find(|s| !s.is_empty())
            .unwrap_or(iri)
            .to_string()
    };
    let c = &o.constraints;
    pick_lang(&o.names, None)
        .map(|l| l.value().to_string())
        .or_else(|| c.classes.first().map(|n| local(n.as_str())))
        .or_else(|| c.datatype.as_ref().map(|n| local(n.as_str())))
        .or_else(|| c.node_kind.map(|k| local(k.iri().as_str())))
        .unwrap_or_else(|| format!("Option {}", n + 1))
}

/// Value constraints as JSON: those of a field, of a list's members or of an option.
fn value_constraints_json(
    c: &sparkles_shacl::form::FieldConstraints,
    single_line: Option<bool>,
    default_value: Option<&Term>,
) -> J {
    let mut o = serde_json::Map::new();
    let mut put = |k: &str, v: J| {
        o.insert(k.into(), v);
    };
    if let Some(n) = c.min_count {
        put("minCount", json!(n));
    }
    if let Some(n) = c.max_count {
        put("maxCount", json!(n));
    }
    if let Some(d) = &c.datatype {
        put("datatype", json!(d.as_str()));
    }
    if let Some(k) = c.node_kind {
        put("nodeKind", json!(k.iri().as_str()));
    }
    if !c.classes.is_empty() {
        put(
            "class",
            json!(c.classes.iter().map(|n| n.as_str()).collect::<Vec<_>>()),
        );
    }
    if let Some(a) = &c.allowed {
        put("in", J::Array(a.iter().map(term_json).collect()));
    }
    if !c.has_value.is_empty() {
        put(
            "hasValue",
            J::Array(c.has_value.iter().map(term_json).collect()),
        );
    }
    for (k, b) in [
        ("minInclusive", &c.min_inclusive),
        ("minExclusive", &c.min_exclusive),
        ("maxInclusive", &c.max_inclusive),
        ("maxExclusive", &c.max_exclusive),
    ] {
        if let Some(b) = b {
            put(k, term_json(&b.term));
        }
    }
    if let Some(n) = c.min_length {
        put("minLength", json!(n));
    }
    if let Some(n) = c.max_length {
        put("maxLength", json!(n));
    }
    if !c.patterns.is_empty() {
        put(
            "pattern",
            J::Array(
                c.patterns
                    .iter()
                    .map(|(p, f)| match f {
                        Some(f) => json!({ "pattern": p, "flags": f }),
                        None => json!({ "pattern": p }),
                    })
                    .collect(),
            ),
        );
    }
    if let Some(l) = &c.language_in {
        put("languageIn", json!(l));
    }
    if c.unique_lang {
        put("uniqueLang", json!(true));
    }
    if let Some(s) = single_line {
        put("singleLine", json!(s));
    }
    if let Some(d) = default_value {
        put("defaultValue", term_json(d));
    }
    if let Some(l) = &c.list {
        let mut list = serde_json::Map::new();
        if let Some(n) = l.min_length {
            list.insert("minLength".into(), json!(n));
        }
        if let Some(n) = l.max_length {
            list.insert("maxLength".into(), json!(n));
        }
        if l.unique {
            list.insert("unique".into(), json!(true));
        }
        list.insert(
            "memberShapes".into(),
            json!(
                l.member_shapes
                    .iter()
                    .map(|t| t.to_string())
                    .collect::<Vec<_>>()
            ),
        );
        list.insert(
            "member".into(),
            value_constraints_json(&l.member, None, None),
        );
        list.insert(
            "memberWidget".into(),
            json!(sparkles_shacl::form::widget_of(&l.member, None)),
        );
        put("list", J::Object(list));
    }
    if let Some(a) = &c.alternatives {
        put(
            "or",
            json!({
                "kind": a.kind.name(),
                "options": a.options.iter().enumerate().map(|(n, o)| json!({
                    "shape": o.shape.to_string(),
                    "label": option_label(o, n),
                    "widget": sparkles_shacl::form::widget_of(&o.constraints, None),
                    "constraints": value_constraints_json(&o.constraints, None, None),
                })).collect::<Vec<_>>(),
            }),
        );
    }
    if !c.node.is_empty() {
        put(
            "node",
            json!(c.node.iter().map(|t| t.to_string()).collect::<Vec<_>>()),
        );
    }
    if !c.server.is_empty() {
        put("server", json!(c.server));
    }
    J::Object(o)
}

// ------------------------------------------------------------------- reading ------

/// Reads of one state under a view: the quads of the graphs the view reads, without the
/// triples its protections hide and without materialized inferences.
struct Reader<'a> {
    snap: Arc<Snapshot>,
    access: Option<&'a GraphAccess>,
    /// `snap` is masked already
    masked: bool,
    limits: Limits,
}

impl<'a> Reader<'a> {
    fn new(snap: &Arc<Snapshot>, access: Option<&'a GraphAccess>) -> Result<Reader<'a>> {
        match access.filter(|a| a.hides_triples()) {
            Some(a) => Ok(Reader {
                snap: a.masked(snap)?,
                access,
                masked: true,
                limits: Limits::default(),
            }),
            None => Ok(Reader {
                snap: snap.clone(),
                access,
                masked: false,
                limits: Limits::default(),
            }),
        }
    }

    fn snap(&self) -> &Arc<Snapshot> {
        &self.snap
    }

    fn graph(&self, g: Id) -> Result<Option<Option<String>>> {
        if g == Id::DEFAULT_GRAPH {
            return Ok(self.readable(None).then_some(None));
        }
        let n = match self.snap.term(g) {
            Some(Term::NamedNode(n)) => n,
            Some(term @ Term::BlankNode(_)) => {
                if self.limits.preparation.is_some() && self.readable(Some(&term)) {
                    return Err(Error::invalid(
                        "blank-named graph context is not admitted by nested forms",
                    ));
                }
                return Ok(None);
            }
            _ => return Ok(None),
        };
        if n.as_str() == INFERRED {
            return Ok(None);
        }
        let t = Term::NamedNode(n);
        if !self.readable(Some(&t)) {
            return Ok(None);
        }
        let Term::NamedNode(n) = t else {
            unreachable!()
        };
        Ok(Some(Some(n.into_string())))
    }

    fn readable(&self, g: Option<&Term>) -> bool {
        self.access.is_none_or(|a| a.readable(g))
    }

    /// The quads whose key in `perm` starts with `prefix`.
    fn scan(&self, perm: Perm, prefix: &[u64], limit: usize) -> Result<Vec<[Id; 4]>> {
        self.limits.check()?;
        let mut out = Vec::new();
        self.snap.scan(perm, prefix, |c| {
            self.limits.check()?;
            match c {
                Chunk::Block(b, s, e) => {
                    for i in s..e {
                        self.limits.unit()?;
                        out.push(perm.to_quad(&b.key(i)));
                    }
                }
                Chunk::Row(k) => {
                    self.limits.unit()?;
                    out.push(perm.to_quad(&k));
                }
            }
            Ok(out.len() < limit)
        })?;
        Ok(out)
    }

    /// The values of a predicate or inverse path at `focus`, with their graphs, in a
    /// stable order. A complex path has none here.
    fn values(&self, focus: &Term, path: &FieldPath) -> Result<Vec<FormValue>> {
        let (Some(f), Some(p)) = (
            self.snap.lookup_term(focus),
            path.predicate()
                .and_then(|p| self.snap.lookup_iri(p.as_str())),
        ) else {
            return Ok(Vec::new());
        };
        let quads = match path {
            FieldPath::Predicate(_) => self.scan(Perm::Spo, &[f.0, p.0], 100_000)?,
            FieldPath::Inverse(_) => self.scan(Perm::Pos, &[p.0, f.0], 100_000)?,
            FieldPath::Complex(_) => return Ok(Vec::new()),
        };
        let mut out = Vec::new();
        for q in quads {
            let Some(graph) = self.graph(q[3])? else {
                continue;
            };
            let id = if matches!(path, FieldPath::Inverse(_)) {
                q[0]
            } else {
                q[2]
            };
            let Some(term) = self.snap.term(id) else {
                continue;
            };
            self.limits.retained_term(&term, 0)?;
            if let Some(graph) = &graph {
                self.limits.lexical(graph)?;
            }
            out.push(FormValue {
                term,
                graph,
                label: None,
                read_only: false,
                items: None,
            });
        }
        if self.limits.preparation.is_none() {
            out.sort_by(|a, b| (a.term.to_string(), &a.graph).cmp(&(b.term.to_string(), &b.graph)));
            return Ok(out);
        }
        let mut keyed = Vec::new();
        for value in out {
            // Admit escaping/render scratch before generating the legacy sort key.
            for _ in 0..6 {
                self.limits.retained_term(&value.term, 0)?;
            }
            self.limits.unit()?;
            keyed.push((value.term.to_string(), value));
        }
        keyed.sort_by(|a, b| (&a.0, &a.1.graph).cmp(&(&b.0, &b.1.graph)));
        Ok(keyed.into_iter().map(|(_, value)| value).collect())
    }

    /// The members of the `rdf:List` that `head` starts, with their graphs, or `None`
    /// when it is not a well-formed list the reader sees: every cell a blank node with
    /// one `rdf:first` and one `rdf:rest`, no cycle, at most [`MAX_LIST_ITEMS`] members.
    fn list_items(&self, head: &Term) -> Result<Option<Vec<FormValue>>> {
        let first = FieldPath::Predicate(NamedNode::new_unchecked(edit::RDF_FIRST));
        let rest = FieldPath::Predicate(NamedNode::new_unchecked(edit::RDF_REST));
        let one = |vs: Vec<FormValue>| {
            let mut distinct: Vec<FormValue> = Vec::new();
            for v in vs {
                if !distinct.iter().any(|d| d.term == v.term) {
                    distinct.push(v);
                }
            }
            (distinct.len() == 1).then(|| distinct.pop()).flatten()
        };
        let mut items = Vec::new();
        let mut seen = BTreeSet::new();
        let mut cell = head.clone();
        loop {
            self.limits.unit()?;
            match &cell {
                Term::NamedNode(n) if n.as_str() == edit::RDF_NIL => return Ok(Some(items)),
                Term::BlankNode(b) => {
                    if !seen.insert(b.as_str().to_string()) || items.len() >= MAX_LIST_ITEMS {
                        return Ok(None);
                    }
                }
                _ => return Ok(None),
            }
            let (Some(item), Some(next)) = (
                one(self.values(&cell, &first)?),
                one(self.values(&cell, &rest)?),
            ) else {
                return Ok(None);
            };
            items.push(item);
            cell = next.term;
        }
    }

    /// A label of an IRI in the readable graphs, in `lang` when it has one.
    fn label(&self, n: &NamedNode, lang: Option<&str>) -> Result<Option<String>> {
        let Some(s) = self.snap.lookup_iri(n.as_str()) else {
            return Ok(None);
        };
        for lp in LABELS {
            let Some(p) = self.snap.lookup_iri(lp) else {
                continue;
            };
            let mut lits = Vec::new();
            for q in self.scan(Perm::Spo, &[s.0, p.0], 50)? {
                if self.graph(q[3])?.is_none() {
                    continue;
                }
                if let Some(Term::Literal(literal)) = self.snap.term(q[2]) {
                    self.limits.lexical(literal.value())?;
                    self.limits.unit()?;
                    lits.push(literal);
                }
            }
            if let Some(l) = pick_lang(&lits, lang) {
                self.limits.lexical(l.value())?;
                return Ok(Some(l.value().to_string()));
            }
        }
        Ok(None)
    }

    /// The outgoing values of `focus` whose predicates no field covers, `rdf:type` aside.
    fn others(
        &self,
        focus: &Term,
        covered: &BTreeSet<String>,
    ) -> Result<Vec<(String, Vec<FormValue>)>> {
        let Some(f) = self.snap.lookup_term(focus) else {
            return Ok(Vec::new());
        };
        let mut by: BTreeMap<String, Vec<FormValue>> = BTreeMap::new();
        for q in self.scan(Perm::Spo, &[f.0], 100_000)? {
            let Some(graph) = self.graph(q[3])? else {
                continue;
            };
            let Some(Term::NamedNode(p)) = self.snap.term(q[1]) else {
                continue;
            };
            if p.as_str() == RDF_TYPE || covered.contains(p.as_str()) {
                continue;
            }
            if !by.contains_key(p.as_str()) && by.len() >= MAX_OTHER {
                continue;
            }
            self.limits.lexical(p.as_str())?;
            let list = by.entry(p.into_string()).or_default();
            if list.len() >= MAX_OTHER {
                continue;
            }
            let Some(term) = self.snap.term(q[2]) else {
                continue;
            };
            self.limits.retained_term(&term, 0)?;
            if let Some(graph) = &graph {
                self.limits.lexical(graph)?;
            }
            self.limits.unit()?;
            list.push(FormValue {
                term,
                graph,
                label: None,
                read_only: true,
                items: None,
            });
        }
        Ok(by.into_iter().collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_model_whose_request_is_cancelled_while_it_reads_fails() {
        let ds = Dataset::memory();
        ds.update(
            r#"PREFIX ex: <http://ex/> PREFIX sh: <http://www.w3.org/ns/shacl#>
            INSERT DATA {
              GRAPH <urn:shapes> { ex:S a sh:NodeShape ; sh:targetClass ex:P ;
                sh:property [ sh:path ex:name ] }
              ex:a a ex:P ; ex:name "A" }"#,
        )
        .unwrap();
        let settings = FormSettings {
            shapes: ShapesSource::Graphs,
            graphs: vec!["urn:shapes".into()],
            ..Default::default()
        };
        let forms = ds.forms();
        let mut built = forms
            .build(
                &ds.store().snapshot(),
                "http://ex/a",
                &settings,
                &QueryOptions::default(),
                None,
                None,
            )
            .unwrap();
        assert!(built.model(true, "main").is_ok());
        // the request is cancelled after the shapes were read: the values are not
        built.limits.cancel = Some(Arc::new(std::sync::atomic::AtomicBool::new(true)));
        assert!(matches!(built.model(true, "main"), Err(Error::Cancelled)));
    }

    #[test]
    fn a_child_whose_type_moved_to_another_graph_makes_the_plan_stale() {
        let ds = Dataset::memory();
        ds.update(
            r#"PREFIX ex: <http://ex/> PREFIX sh: <http://www.w3.org/ns/shacl#>
            INSERT DATA {
              GRAPH <urn:shapes> { ex:S a sh:NodeShape ; sh:targetClass ex:P ;
                sh:property [ sh:path ex:address ; sh:node ex:A ] .
                ex:A sh:property [ sh:path ex:street ] }
              ex:a a ex:P ; ex:address _:x .
              GRAPH <urn:g1> { _:x a ex:Address } }"#,
        )
        .unwrap();
        let settings = FormSettings {
            shapes: ShapesSource::Graphs,
            graphs: vec!["urn:shapes".into()],
            target_graph: "type".into(),
            ..Default::default()
        };
        let req = FormRequest::new("http://ex/a", settings.clone());
        let m = ds.forms().nested_model(&req, &Default::default()).unwrap();
        let Term::BlankNode(child) = m.nested_nodes[0].term.clone() else {
            panic!("a blank child");
        };
        let change = FormChange {
            focus: "http://ex/a".into(),
            settings,
            nested: vec![NodeChange {
                node: child,
                parent: None,
                field: "<http://ex/address>".into(),
                changes: vec![FieldChange {
                    field: "<http://ex/street>".into(),
                    add: vec![Term::Literal(oxrdf::Literal::new_simple_literal("Main"))],
                    ..Default::default()
                }],
                expect: BTreeMap::from([("<http://ex/street>".into(), vec![])]),
            }],
            ..Default::default()
        };
        let opts = QueryOptions::default();
        let forms = ds.forms();
        let head = ds.store().snapshot();
        let built = forms.build_for(&head, &change, &opts).unwrap();
        let plan = built.plan(&change, false).unwrap();
        let guard = Guard::of(&built, &plan, &change).unwrap();
        let limits = Limits::of(&opts);
        let (conflicts, stale) = guard.differ(&head, None, &limits).unwrap();
        assert!(conflicts.is_empty() && !stale);
        // the child's type moves to another graph, so its new values belong there
        ds.update(
            r#"PREFIX ex: <http://ex/>
            DELETE { GRAPH <urn:g1> { ?x a ex:Address } } INSERT { GRAPH <urn:g2> { ?x a ex:Address } }
            WHERE { ex:a ex:address ?x }"#,
        )
        .unwrap();
        let now = ds.store().snapshot();
        let (conflicts, stale) = guard.differ(&now, None, &limits).unwrap();
        assert!(conflicts.is_empty() && stale);
        // the save plans again and writes the street into the new graph
        assert!(matches!(
            forms.save(&change, &opts).unwrap(),
            FormSaved::Saved(_)
        ));
        assert!(
            ds.ask(r#"ASK { GRAPH <urn:g2> { ?x <http://ex/street> "Main" } }"#)
                .unwrap()
        );
    }
}

#[cfg(test)]
#[path = "model_allocations.rs"]
mod model_allocations;

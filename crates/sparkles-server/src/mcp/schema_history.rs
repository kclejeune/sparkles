//! Per-class property profiles (`describe_schema` with `section: "profiles"`) and the
//! `diff_schema` tool: what changed in a dataset's schema between two commits.

use super::Outcome;
use super::errors::ToolError;
use super::render::{Prefixes, Terms};
use super::tools::{Tools, bounded, dataset_prefixes, parse, parse_iri};
use crate::http::INFERRED_GRAPH;
use oxrdf::Term;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use sparkles::history::{At, HistoryOptions};
use sparkles::schema::compare::{EntryChanges, FieldChange};
use sparkles::schema::{
    self, ClassProfiles, GraphSelection, HasIri, ProfileOptions, SchemaOptions,
};
use sparkles::store::Snapshot;
use std::sync::Arc;

/// `graph` as describe_schema takes it: `default`, `union` or an IRI (prefixed names
/// allowed).
pub(super) fn graph_arg(
    g: Option<&str>,
    prefixes: &std::collections::BTreeMap<String, String>,
) -> Result<GraphSelection, ToolError> {
    Ok(match g.map(str::trim) {
        None | Some("default") => GraphSelection::Default,
        Some("union") => GraphSelection::Union,
        Some(g) => match parse_iri(g, prefixes, false)? {
            Term::NamedNode(n) => {
                GraphSelection::parse(n.as_str()).map_err(ToolError::bad_argument)?
            }
            _ => {
                return Err(ToolError::bad_argument(
                    "graph must be default, union or a graph IRI",
                ));
            }
        },
    })
}

/// A compact profile list: the `limit` classes with the most instances, each with its
/// 25 most used predicates and 10 incoming predicates.
pub(super) fn profiles_json(p: &ClassProfiles, limit: usize, terms: &mut Terms) -> Value {
    let mut classes: Vec<_> = p.classes.iter().collect();
    classes.sort_by(|a, b| {
        b.instances
            .cmp(&a.instances)
            .then_with(|| a.class.cmp(&b.class))
    });
    classes
        .into_iter()
        .take(limit)
        .map(|c| {
            let properties: Vec<Value> = c
                .properties
                .iter()
                .take(25)
                .map(|x| {
                    let mut objects = serde_json::Map::new();
                    for (k, n) in [
                        ("iri", x.objects.iri),
                        ("blank", x.objects.blank),
                        ("tripleTerm", x.objects.triple_term),
                    ] {
                        if n > 0 {
                            objects.insert(k.into(), n.into());
                        }
                    }
                    for l in &x.objects.literals {
                        objects.insert(terms.iri(&l.datatype), l.triples.into());
                    }
                    let mut e = json!({
                        "predicate": terms.iri(&x.predicate),
                        "instances": x.instances,
                        "triples": x.triples,
                        "valuesPerInstance": format!("{}..{}", x.min_per_instance, x.max_per_instance),
                        "objects": objects,
                    });
                    if !x.object_classes.is_empty() {
                        e["objectClasses"] = x
                            .object_classes
                            .iter()
                            .take(5)
                            .map(|k| json!({"class": terms.iri(&k.class), "triples": k.triples}))
                            .collect();
                    }
                    e
                })
                .collect();
            let incoming: Vec<Value> = c
                .incoming
                .iter()
                .take(10)
                .map(|i| {
                    json!({"predicate": terms.iri(&i.predicate), "triples": i.triples, "instances": i.instances})
                })
                .collect();
            json!({
                "class": terms.iri(&c.class),
                "instances": c.instances,
                "properties": properties,
                "incoming": incoming,
            })
        })
        .collect()
}

impl Tools<'_> {
    /// The profiles of `classes` (all classes when empty) at `snap`.
    pub(super) fn class_profiles(
        &self,
        ds: &crate::state::Dataset,
        snap: &Arc<Snapshot>,
        schema: SchemaOptions,
        classes: Vec<String>,
        ctx: &super::errors::ErrorContext,
    ) -> Result<ClassProfiles, ToolError> {
        let opts = ProfileOptions {
            schema: SchemaOptions {
                graphs: self
                    .call
                    .principal
                    .view(&ds.name, crate::auth::Endpoint::Info),
                cancel: Some(self.call.cancel.clone()),
                max_entries: self.server.state.schema_max_entries,
                ..schema
            },
            classes,
        };
        schema::profiles(snap, &opts).map_err(|e| ctx.schema(e))
    }

    pub(super) fn diff_schema(&self, args: Map<String, Value>) -> Result<Outcome, ToolError> {
        let a: DiffArgs = parse(args)?;
        let limit = bounded("limit", a.limit, 50, 1, 500)? as usize;
        let timeout = self.timeout(a.timeout_seconds)?;
        let ds = self.dataset(a.dataset.as_deref())?;
        self.info_endpoint(&ds.name)?;
        let prefix_map = dataset_prefixes(&ds);
        let prefixes = Prefixes::new(&prefix_map);
        let names = prefixes.names();
        let ctx = self.ctx(&names, timeout.as_secs_f64());
        let at = |v: &Value| -> Result<At, ToolError> {
            match v {
                Value::Number(n) => n
                    .as_u64()
                    .map(At::Commit)
                    .ok_or_else(|| ToolError::bad_argument("a commit is a non-negative integer")),
                Value::String(s) => s
                    .parse::<At>()
                    .map_err(|e| ToolError::bad_argument(e.to_string())),
                _ => Err(ToolError::bad_argument(
                    "from and to are commits (integers) or strings such as \"time:2026-01-01T00:00:00Z\" or \"snapshot:name\"",
                )),
            }
        };
        let from = at(&a.from)?;
        let to = a.to.as_ref().map(at).transpose()?.unwrap_or(At::Head);
        let graph = graph_arg(a.graph.as_deref(), &prefix_map)?;
        let deadline = self.call.arrived + timeout;
        let opts = SchemaOptions {
            graph,
            inferred_graph: Some(INFERRED_GRAPH.to_string()),
            include_inferred: Self::reasoning(&ds, a.reasoning),
            deadline: Some(deadline),
            cancel: Some(self.call.cancel.clone()),
            max_entries: self.server.state.schema_max_entries,
            graphs: self
                .call
                .principal
                .view(&ds.name, crate::auth::Endpoint::Info),
            ..Default::default()
        };
        let ho = HistoryOptions {
            cancel: Some(self.call.cancel.clone()),
            deadline: Some(deadline),
        };
        let state = |at: &At| -> Result<Arc<Snapshot>, ToolError> {
            ds.store
                .snapshot_at(at, &ho)
                .map(|(s, _)| s)
                .map_err(|e| match e {
                    sparkles::error::Error::NotFound(m) => ToolError::new("unknown-commit", 404, m),
                    sparkles::error::Error::HistoryGone(g) => {
                        ToolError::new("unknown-commit", 410, g.message.clone())
                            .hint("the dataset no longer holds that state; see list_commits")
                    }
                    e => ctx.engine(e),
                })
        };
        let (sa, sb) = (state(&from)?, state(&to)?);
        let ra = schema::discover(&sa, &opts).map_err(|e| ctx.schema(e))?;
        let rb = schema::discover(&sb, &opts).map_err(|e| ctx.schema(e))?;
        let d = schema::compare(&ra, &rb);
        let mut terms = Terms::new(&prefixes, 500);
        let mut truncated = false;
        let classes = entries_json(&d.classes, limit, &mut terms, &mut truncated);
        let predicates = entries_json(&d.predicates, limit, &mut terms, &mut truncated);
        Ok(Outcome::Structured(json!({
            "dataset": ds.name,
            "from": d.from.commit,
            "to": d.to.commit,
            "graph": d.selection.graph,
            "reasoning": d.selection.reasoning,
            "counts": d.counts,
            "report": d.report.iter().map(change_json).collect::<Vec<_>>(),
            "classes": classes,
            "predicates": predicates,
            "truncated": truncated,
            "prefixes": terms.used(),
        })))
    }
}

fn change_json(c: &FieldChange) -> Value {
    serde_json::to_value(c).unwrap_or(Value::Null)
}

/// The IRIs of added and removed entries, and the changes of changed ones, at most
/// `limit` of each.
fn entries_json<T: HasIri>(
    e: &EntryChanges<T>,
    limit: usize,
    terms: &mut Terms,
    truncated: &mut bool,
) -> Value {
    *truncated |= e.added.len() > limit || e.removed.len() > limit || e.changed.len() > limit;
    json!({
        "added": e.added.iter().take(limit).map(|x| terms.iri(x.iri())).collect::<Vec<_>>(),
        "removed": e.removed.iter().take(limit).map(|x| terms.iri(x.iri())).collect::<Vec<_>>(),
        "changed": e.changed.iter().take(limit).map(|c| json!({
            "iri": terms.iri(&c.iri),
            "changes": c.changes.iter().map(change_json).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct DiffArgs {
    dataset: Option<String>,
    from: Value,
    to: Option<Value>,
    graph: Option<String>,
    reasoning: Option<bool>,
    limit: Option<u64>,
    timeout_seconds: Option<f64>,
}

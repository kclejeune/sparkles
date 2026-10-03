//! Schema diffs: what changed between the reports of two states of a dataset.
//!
//! Two reports of the same selection, usually of two commits read with point-in-time
//! queries, are compared entry by entry. A class or predicate that only one of them
//! lists is added or removed, with its whole entry. An entry both list is changed when
//! any of its fields differ, and each difference is reported once with its path in the
//! report's JSON:
//!
//! - a number, string or flag gives its value `from` and `to` (`null` when a field is
//!   absent on one side, as `objects.blank` is for a predicate without blank objects);
//! - a list of IRIs, labels or cycles gives the members `added` and `removed`;
//! - a list of groups (literal groups by `datatype`, languages by `lang`, subject
//!   classes by `class`, ontology headers by `iri`) is compared group by group, and a
//!   path names the group, as in
//!   `observed.objects.literals[datatype=http://www.w3.org/2001/XMLSchema#integer].triples`.
//!
//! The totals, the hierarchy's roots and cycles and the ontology headers are compared
//! the same way.

use super::{ClassEntry, HasIri, PredicateEntry, SchemaReport, Selection, SnapshotInfo};
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeSet;

/// Version of the JSON shape of [`SchemaDiff`].
pub const DIFF_FORMAT: u32 = 1;

/// One difference of a field.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(untagged)]
pub enum FieldChange {
    /// A value that changed, appeared (`from: null`) or disappeared (`to: null`).
    Value {
        path: String,
        from: Value,
        to: Value,
    },
    /// Members of a list that one side has and the other lacks.
    Members {
        path: String,
        added: Vec<Value>,
        removed: Vec<Value>,
    },
}

impl FieldChange {
    pub fn path(&self) -> &str {
        match self {
            FieldChange::Value { path, .. } | FieldChange::Members { path, .. } => path,
        }
    }
}

/// The differences of one entry both reports list.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct EntryChange {
    pub iri: String,
    pub changes: Vec<FieldChange>,
}

/// Entries of one list: added, removed and changed, each sorted by IRI.
#[derive(Clone, Debug, Serialize)]
pub struct EntryChanges<T> {
    pub added: Vec<T>,
    pub removed: Vec<T>,
    pub changed: Vec<EntryChange>,
}

/// The number of entries in each part of [`SchemaDiff`].
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffCounts {
    pub classes_added: usize,
    pub classes_removed: usize,
    pub classes_changed: usize,
    pub predicates_added: usize,
    pub predicates_removed: usize,
    pub predicates_changed: usize,
}

/// What changed from one report to another of the same selection.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SchemaDiff {
    pub diff_format: u32,
    pub from: SnapshotInfo,
    pub to: SnapshotInfo,
    pub selection: Selection,
    pub counts: DiffCounts,
    /// Changes of `totals`, `hierarchy` and `ontology`.
    pub report: Vec<FieldChange>,
    pub classes: EntryChanges<ClassEntry>,
    pub predicates: EntryChanges<PredicateEntry>,
}

impl SchemaDiff {
    /// Whether the two reports describe the same schema.
    pub fn is_empty(&self) -> bool {
        self.counts == DiffCounts::default() && self.report.is_empty()
    }
}

/// The field that keys the groups of the list at `path`, by the list's name: literal
/// groups by datatype, languages by tag, subject classes by class, ontology headers by
/// IRI. Other lists are sets of members.
fn key_field(path: &str) -> Option<&'static str> {
    let name = path.rsplit(['.', ']']).next().unwrap_or(path);
    match name {
        "literals" => Some("datatype"),
        "languages" => Some("lang"),
        "subjectClasses" => Some("class"),
        "ontology" => Some("iri"),
        _ => None,
    }
}

/// The key of one group: its key field, with a language's direction.
fn group_key(field: &str, v: &Value) -> Option<String> {
    let o = v.as_object()?;
    let s = match o.get(field)? {
        Value::String(s) => s.clone(),
        x => x.to_string(),
    };
    Some(match (field, o.get("direction")) {
        ("lang", Some(Value::String(d))) => format!("{s}--{d}"),
        _ => s,
    })
}

fn join(path: &str, key: &str) -> String {
    if path.is_empty() {
        key.to_string()
    } else {
        format!("{path}.{key}")
    }
}

/// The differences between two JSON values at `path`.
fn compare_values(path: &str, a: &Value, b: &Value, out: &mut Vec<FieldChange>) {
    if a == b {
        return;
    }
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => {
            let keys: BTreeSet<&String> = x.keys().chain(y.keys()).collect();
            for k in keys {
                let (va, vb) = (
                    x.get(k).unwrap_or(&Value::Null),
                    y.get(k).unwrap_or(&Value::Null),
                );
                compare_values(&join(path, k), va, vb, out);
            }
        }
        (Value::Array(x), Value::Array(y)) => {
            if let Some(field) = key_field(path) {
                let index = |v: &[Value]| -> Vec<(String, Value)> {
                    v.iter()
                        .filter_map(|e| {
                            Some((format!("{field}={}", group_key(field, e)?), e.clone()))
                        })
                        .collect()
                };
                let (ix, iy) = (index(x), index(y));
                let names: BTreeSet<&String> = ix.iter().chain(&iy).map(|(k, _)| k).collect();
                for n in names {
                    let get = |i: &[(String, Value)]| {
                        i.iter()
                            .find(|(k, _)| k == n)
                            .map_or(Value::Null, |(_, v)| v.clone())
                    };
                    let (va, vb) = (get(&ix), get(&iy));
                    let p = format!("{path}[{n}]");
                    if va.is_null() || vb.is_null() {
                        out.push(FieldChange::Value {
                            path: p,
                            from: va,
                            to: vb,
                        });
                    } else {
                        compare_values(&p, &va, &vb, out);
                    }
                }
            } else {
                let added: Vec<Value> = y.iter().filter(|v| !x.contains(v)).cloned().collect();
                let removed: Vec<Value> = x.iter().filter(|v| !y.contains(v)).cloned().collect();
                if !added.is_empty() || !removed.is_empty() {
                    out.push(FieldChange::Members {
                        path: path.to_string(),
                        added,
                        removed,
                    });
                }
            }
        }
        // a list on one side only (absent, or not computed) counts as empty
        (Value::Array(_), Value::Null) => compare_values(path, a, &Value::Array(Vec::new()), out),
        (Value::Null, Value::Array(_)) => compare_values(path, &Value::Array(Vec::new()), b, out),
        _ => out.push(FieldChange::Value {
            path: path.to_string(),
            from: a.clone(),
            to: b.clone(),
        }),
    }
}

fn to_json<T: Serialize>(x: &T) -> Value {
    serde_json::to_value(x).unwrap_or(Value::Null)
}

fn compare_entries<T: HasIri + Serialize + Clone>(a: &[T], b: &[T]) -> EntryChanges<T> {
    let mut out = EntryChanges {
        added: Vec::new(),
        removed: Vec::new(),
        changed: Vec::new(),
    };
    let (mut i, mut j) = (0, 0);
    // both lists are sorted by IRI
    while i < a.len() || j < b.len() {
        match (a.get(i), b.get(j)) {
            (Some(x), Some(y)) if x.iri() == y.iri() => {
                let mut changes = Vec::new();
                let (mut vx, mut vy) = (to_json(x), to_json(y));
                for v in [&mut vx, &mut vy] {
                    if let Some(o) = v.as_object_mut() {
                        o.remove("iri");
                    }
                }
                compare_values("", &vx, &vy, &mut changes);
                if !changes.is_empty() {
                    out.changed.push(EntryChange {
                        iri: x.iri().to_string(),
                        changes,
                    });
                }
                i += 1;
                j += 1;
            }
            (Some(x), Some(y)) if x.iri() < y.iri() => {
                out.removed.push(x.clone());
                i += 1;
            }
            (Some(x), None) => {
                out.removed.push(x.clone());
                i += 1;
            }
            (_, Some(y)) => {
                out.added.push(y.clone());
                j += 1;
            }
            (None, None) => break,
        }
    }
    out
}

/// What changed from `from` to `to`, two reports of the same selection.
pub fn compare(from: &SchemaReport, to: &SchemaReport) -> SchemaDiff {
    let mut report = Vec::new();
    compare_values(
        "totals",
        &to_json(&from.totals),
        &to_json(&to.totals),
        &mut report,
    );
    compare_values(
        "hierarchy",
        &to_json(&from.hierarchy),
        &to_json(&to.hierarchy),
        &mut report,
    );
    compare_values(
        "ontology",
        &to_json(&from.ontology),
        &to_json(&to.ontology),
        &mut report,
    );
    let classes = compare_entries(&from.classes, &to.classes);
    let predicates = compare_entries(&from.predicates, &to.predicates);
    SchemaDiff {
        diff_format: DIFF_FORMAT,
        from: from.snapshot.clone(),
        to: to.snapshot.clone(),
        selection: to.selection.clone(),
        counts: DiffCounts {
            classes_added: classes.added.len(),
            classes_removed: classes.removed.len(),
            classes_changed: classes.changed.len(),
            predicates_added: predicates.added.len(),
            predicates_removed: predicates.removed.len(),
            predicates_changed: predicates.changed.len(),
        },
        report,
        classes,
        predicates,
    }
}

/// The diff as text: one line per added or removed entry, and per changed entry one line
/// with each of its changes.
pub fn diff_text(d: &SchemaDiff) -> String {
    use std::fmt::Write;
    let mut s = String::new();
    let _ = writeln!(
        s,
        "schema diff from commit {} to commit {} (graph {}, reasoning {})",
        d.from.commit, d.to.commit, d.selection.graph, d.selection.reasoning
    );
    let value = |v: &Value| match v {
        Value::String(s) => s.clone(),
        Value::Null => "-".into(),
        v => v.to_string(),
    };
    let change = |c: &FieldChange| match c {
        FieldChange::Value { path, from, to } => {
            format!("{path} {} -> {}", value(from), value(to))
        }
        FieldChange::Members {
            path,
            added,
            removed,
        } => {
            let mut parts = Vec::new();
            parts.extend(added.iter().map(|v| format!("+{}", value(v))));
            parts.extend(removed.iter().map(|v| format!("-{}", value(v))));
            format!("{path} {}", parts.join(" "))
        }
    };
    for c in &d.report {
        let _ = writeln!(s, "~ {}", change(c));
    }
    for (sign, e) in [("+", &d.classes.added), ("-", &d.classes.removed)] {
        for c in e {
            let _ = writeln!(
                s,
                "{sign} class {} (instances {})",
                c.iri, c.observed.instances
            );
        }
    }
    for c in &d.classes.changed {
        let list: Vec<String> = c.changes.iter().map(change).collect();
        let _ = writeln!(s, "~ class {}: {}", c.iri, list.join("; "));
    }
    for (sign, e) in [("+", &d.predicates.added), ("-", &d.predicates.removed)] {
        for p in e {
            let _ = writeln!(
                s,
                "{sign} predicate {} (triples {})",
                p.iri, p.observed.triples
            );
        }
    }
    for p in &d.predicates.changed {
        let list: Vec<String> = p.changes.iter().map(change).collect();
        let _ = writeln!(s, "~ predicate {}: {}", p.iri, list.join("; "));
    }
    if d.is_empty() {
        s.push_str("no changes\n");
    }
    s
}

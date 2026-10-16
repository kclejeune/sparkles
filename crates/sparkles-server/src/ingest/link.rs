//! Entity linking for the IRI columns of a mapping draft (spec C18 §7.8).
//!
//! A column that the draft maps to IRIs of an existing class has its distinct values
//! linked with `link_entities`, most frequent first, within a budget of calls. An exact
//! link goes into the column's lookup in the CSVW metadata (`urn:x-sparkles:lookup`, C05
//! §3.6), so the converter writes the existing entity's IRI instead of a minted one, and
//! a second upload of the same file with the same mapping links the same way. An
//! ambiguous value is listed with its candidates for a person to choose, and without a
//! choice it stays a minted IRI, because a wrong merge is worse than a duplicate that
//! the duplicate check finds later. Linking is lexical: `link_entities` runs with
//! `vectors: false`, so no cell value reaches an embedding provider.

use super::pipeline::{Failed, Run, expand_iri};
use serde_json::{Value, json};
use sparkles::tabular::Tally;
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

/// The most columns of a draft that are linked.
pub const MAX_COLUMNS: usize = 5;
/// The most distinct values of one column that are tried.
pub const MAX_PER_COLUMN: usize = 500;
/// The most `link_entities` calls of a draft.
pub const MAX_CALLS: usize = 50;
/// Mentions in one call.
const BATCH: usize = 20;
/// Ambiguous values listed for each column.
const MAX_AMBIGUOUS: usize = 50;
/// The time left below which linking stops.
const MIN_LEFT: Duration = Duration::from_secs(3);

/// A column to link and the classes its entities may have.
#[derive(Clone, Debug)]
pub struct Spec {
    pub column: String,
    pub types: Vec<String>,
}

/// The lookup entries of each column and what to tell the person.
#[derive(Default)]
pub struct Linked {
    /// column name to entries of (the cell values, the IRI)
    pub lookups: BTreeMap<String, Vec<(Vec<String>, String)>>,
    pub report: Vec<Value>,
    pub notes: Vec<String>,
}

/// The values that name one entity once case and spacing are set aside.
struct Group {
    /// the cell values and their rows, most frequent first
    variants: Vec<(String, u64)>,
    rows: u64,
}

fn norm(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// The groups of a column's values, most rows first.
fn groups(t: &Tally) -> Vec<Group> {
    let mut by: BTreeMap<String, Vec<(String, u64)>> = BTreeMap::new();
    for (v, n) in &t.values {
        by.entry(norm(v)).or_default().push((v.clone(), *n));
    }
    let mut out: Vec<(String, Group)> = by
        .into_iter()
        .map(|(k, mut variants)| {
            variants.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
            let rows = variants.iter().map(|v| v.1).sum();
            (k, Group { variants, rows })
        })
        .collect();
    out.sort_by(|a, b| b.1.rows.cmp(&a.1.rows).then_with(|| a.0.cmp(&b.0)));
    out.into_iter().map(|(_, g)| g).collect()
}

/// Link the columns of `specs`, whose values `tallies` counted in the same order.
pub fn link(r: &Run, specs: &[Spec], tallies: &[Tally]) -> Result<Linked, Failed> {
    let mut out = Linked::default();
    let mut work: Vec<(&Spec, Vec<Group>, u64)> = specs
        .iter()
        .zip(tallies)
        .map(|(s, t)| (s, groups(t), t.dropped))
        .collect();
    // the shortest columns first, so what they leave over goes to the longer ones
    work.sort_by_key(|(_, g, _)| g.len());
    let mut left = MAX_CALLS * BATCH;
    let total = work.len();
    let mut stopped = false;
    for (i, (spec, groups, dropped)) in work.into_iter().enumerate() {
        let share = MAX_PER_COLUMN.min(left / (total - i).max(1));
        let distinct: usize = groups.iter().map(|g| g.variants.len()).sum();
        let candidates: Vec<&Group> = groups
            .iter()
            .filter(|g| {
                let t = &g.variants[0].0;
                !t.trim().is_empty() && t.chars().count() <= 200
            })
            .collect();
        let tried: Vec<&Group> = candidates
            .iter()
            .take(if stopped { 0 } else { share })
            .copied()
            .collect();
        left -= tried.len().min(left);
        let mut entries: Vec<(Vec<String>, String)> = Vec::new();
        let mut ambiguous: Vec<(u64, Value)> = Vec::new();
        let (mut linked, mut weak, mut done) = (0usize, 0usize, 0usize);
        let mut commit = Value::Null;
        'batches: for batch in tried.chunks(BATCH) {
            r.check()?;
            if r.ctx.deadline.saturating_duration_since(Instant::now()) < MIN_LEFT {
                out.notes.push(
                    "linking stopped early because the ingestion's time was nearly used".into(),
                );
                stopped = true;
                break;
            }
            let mentions: Vec<Value> = batch
                .iter()
                .map(|g| json!({ "text": g.variants[0].0, "types": spec.types }))
                .collect();
            let res = match r.tool(
                "link_entities",
                json!({ "mentions": mentions, "k": 3, "vectors": false }),
                None,
            ) {
                Ok(v) => v,
                Err(e) => {
                    let f = Failed::from(e);
                    let code = f.0["code"].as_str().unwrap_or("");
                    if matches!(code, "cancelled" | "timeout" | "budget-exceeded") {
                        return Err(f);
                    }
                    out.notes.push(format!(
                        "linking the column {} stopped: {}",
                        spec.column,
                        f.0["message"].as_str().unwrap_or(code)
                    ));
                    break 'batches;
                }
            };
            commit = res["commit"].clone();
            let prefixes = &res["prefixes"];
            let ms = res["mentions"].as_array().cloned().unwrap_or_default();
            for (g, m) in batch.iter().zip(&ms) {
                done += 1;
                let cands: Vec<&Value> = m["candidates"].as_array().into_iter().flatten().collect();
                match m["verdict"].as_str() {
                    Some("exact") if !cands.is_empty() => {
                        let iri = expand_iri(cands[0]["iri"].as_str().unwrap_or(""), prefixes);
                        if !iri.is_empty() {
                            linked += g.variants.len();
                            entries.push((g.variants.iter().map(|v| v.0.clone()).collect(), iri));
                        }
                    }
                    Some("ambiguous") if !cands.is_empty() => {
                        let list: Vec<Value> = cands
                            .iter()
                            .map(|c| {
                                let mut j = json!({
                                    "iri": expand_iri(c["iri"].as_str().unwrap_or(""), prefixes),
                                });
                                if let Some(l) = c["label"].as_str() {
                                    j["label"] = l.into();
                                }
                                j["types"] = c["types"]
                                    .as_array()
                                    .into_iter()
                                    .flatten()
                                    .filter_map(Value::as_str)
                                    .map(|t| Value::String(expand_iri(t, prefixes)))
                                    .collect();
                                j
                            })
                            .collect();
                        ambiguous.push((
                            g.rows,
                            json!({ "value": g.variants[0].0, "rows": g.rows, "candidates": list }),
                        ));
                    }
                    Some("candidates") => weak += 1,
                    _ => {}
                }
            }
        }
        if dropped > 0 {
            out.notes.push(format!(
                "the column {} has more distinct values than the draft counts: {dropped} rows were not tallied",
                spec.column
            ));
        }
        if candidates.len() > done && !stopped {
            out.notes.push(format!(
                "the column {} has {} distinct values: the {done} most frequent were linked and the rest are minted",
                spec.column,
                candidates.len()
            ));
        }
        entries.sort();
        let ambiguous_total = ambiguous.len();
        ambiguous.sort_by_key(|a| std::cmp::Reverse(a.0));
        out.report.push(json!({
            "column": spec.column,
            "types": spec.types,
            "commit": commit,
            "distinct": distinct,
            "tried": done,
            "linked": linked,
            "weak": weak,
            "minted": distinct.saturating_sub(linked),
            "ambiguousTotal": ambiguous_total,
            "ambiguous": ambiguous.into_iter().take(MAX_AMBIGUOUS).map(|a| a.1).collect::<Vec<_>>(),
        }));
        if !entries.is_empty() {
            out.lookups.insert(spec.column.clone(), entries);
        }
    }
    out.report.sort_by(|a, b| {
        let pos = |v: &Value| {
            specs
                .iter()
                .position(|s| Some(s.column.as_str()) == v["column"].as_str())
        };
        pos(a).cmp(&pos(b))
    });
    Ok(out)
}

/// Put the lookups into the metadata's columns. A column's entries stop at the limit of
/// the whole table.
pub fn apply(metadata: &mut Value, lookups: &BTreeMap<String, Vec<(Vec<String>, String)>>) {
    let mut room = sparkles::tabular::csvw::MAX_LOOKUP;
    let Some(cols) = metadata["tableSchema"]["columns"].as_array_mut() else {
        return;
    };
    for c in cols {
        let Some(name) = c["name"].as_str().map(str::to_string) else {
            continue;
        };
        let Some(entries) = lookups.get(&name) else {
            continue;
        };
        let mut list = Vec::new();
        for (values, iri) in entries {
            if values.len() > room {
                break;
            }
            room -= values.len();
            let v: Value = if values.len() == 1 {
                values[0].clone().into()
            } else {
                values.clone().into()
            };
            list.push(json!({ "rdf:value": v, "@id": iri }));
        }
        if !list.is_empty() {
            c[sparkles::tabular::csvw::LOOKUP_KEY] = list.into();
        }
    }
}

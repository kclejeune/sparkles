//! Validation: expand the shape map, discover the typing graph, refine it stratum by
//! stratum, and explain the nonconformant results.
//!
//! Semantic actions run during the typing without keeping their output. Each result's
//! `print` output and the counts of actions of unknown extensions come from one more
//! evaluation of that result's own pair with the tracing handlers, so actions of the
//! shapes it references (decided in the typing) print nothing and are not counted.

use crate::ir::{PairKind, Te};
use crate::semact::{ActCtx, Registry, unknown_warnings};
use crate::shapemap;
use crate::typing::{self, Env, Worker};
use crate::{
    CompiledSchema, NodeSelector, ResultMap, ShapeLabel, ShapeMap, ShapeResult, ShexFailure,
    Status, TooManyResults, ValidateOptions, ValidationStats, explain,
};
use oxrdf::Term;
use rustc_hash::FxHashMap;
use sparkles_core::id::Id;
use sparkles_core::store::Snapshot;
use sparkles_core::validation::{DataGraph, ReportBytes};
use std::sync::Arc;
use std::time::Instant;

/// See [`crate::validate`].
pub fn validate(
    snap: &Arc<Snapshot>,
    schema: &CompiledSchema,
    map: &ShapeMap,
    opts: &ValidateOptions,
) -> anyhow::Result<ResultMap> {
    Ok(validate_typed(snap, schema, map, opts, None, &[])?.0)
}

pub(crate) fn validate_controlled(
    snap: &Arc<Snapshot>,
    schema: &CompiledSchema,
    map: &ShapeMap,
    opts: &ValidateOptions,
    max_report_bytes: Option<u64>,
) -> anyhow::Result<ResultMap> {
    let outputs = Outputs {
        root: None,
        max_report_bytes,
    };
    Ok(validate_impl(snap, schema, map, opts, None, &[], outputs)?.0)
}

/// The pairs of a typing and their verdicts, of store nodes only.
pub(crate) type PairValues = Vec<((Id, PairKind), typing::Verdict)>;

/// [`validate`], with the pairs `fixed` answers read as its values, the pairs `extra`
/// typed too, and the typing's pairs of store nodes returned with their values.
pub(crate) fn validate_typed(
    snap: &Arc<Snapshot>,
    schema: &CompiledSchema,
    map: &ShapeMap,
    opts: &ValidateOptions,
    fixed: Option<&typing::Fixed<'_>>,
    extra: &[(Id, PairKind)],
) -> anyhow::Result<(ResultMap, PairValues)> {
    validate_typed_with_output(snap, schema, map, opts, fixed, extra, None)
}

// The incremental guard supplies only the root it created over this same candidate.
// Public validation always creates its own original root above.
pub(crate) fn validate_typed_with_output(
    snap: &Arc<Snapshot>,
    schema: &CompiledSchema,
    map: &ShapeMap,
    opts: &ValidateOptions,
    fixed: Option<&typing::Fixed<'_>>,
    extra: &[(Id, PairKind)],
    output: Option<&sparkles_core::sparql::output::SnapshotOutputRoot>,
) -> anyhow::Result<(ResultMap, PairValues)> {
    let outputs = Outputs {
        root: output,
        max_report_bytes: None,
    };
    validate_impl(snap, schema, map, opts, fixed, extra, outputs)
}

struct Outputs<'a> {
    root: Option<&'a sparkles_core::sparql::output::SnapshotOutputRoot>,
    max_report_bytes: Option<u64>,
}

fn validate_impl(
    snap: &Arc<Snapshot>,
    schema: &CompiledSchema,
    map: &ShapeMap,
    opts: &ValidateOptions,
    fixed: Option<&typing::Fixed<'_>>,
    extra: &[(Id, PairKind)],
    outputs: Outputs<'_>,
) -> anyhow::Result<(ResultMap, PairValues)> {
    let started = Instant::now();
    let timed = opts
        .timeout
        .map(|timeout| {
            started
                .checked_add(timeout)
                .ok_or_else(|| sparkles_core::Error::invalid("validation timeout overflow"))
        })
        .transpose()?;
    let deadline = opts.deadline.into_iter().chain(timed).min();
    if deadline.is_some_and(|limit| Instant::now() >= limit) {
        return Err(sparkles_core::Error::Timeout.into());
    }
    let data = DataGraph::new(
        snap.clone(),
        opts.data_graph.as_deref(),
        &opts.extra_graphs,
        &opts.exclude_graphs,
    )?;
    let owned_root;
    let root = match outputs.root {
        Some(root) => {
            root.budget().check()?;
            root
        }
        None => {
            owned_root = shapemap::output_root(snap, opts, deadline)?;
            &owned_root
        }
    };
    let report_bytes = outputs.max_report_bytes.map(ReportBytes::new);
    let entry_bytes_paid = !opts.only_nonconformant;
    let entry_allowance = report_bytes.as_ref().filter(|_| entry_bytes_paid);
    let (entries, mut warnings) =
        shapemap::expand_in(map, &data, schema, opts, deadline, root, entry_allowance)?;
    let rest;
    let opts = match opts.timeout {
        Some(t) => {
            rest = ValidateOptions {
                timeout: Some(t.saturating_sub(started.elapsed())),
                ..opts.clone()
            };
            &rest
        }
        None => opts,
    };
    // nodes that are not in the store get local ids
    let mut absent = Vec::new();
    #[allow(
        clippy::mutable_key_type,
        reason = "OwnedTerm hashes and compares its immutable term, not its owner"
    )]
    let mut absent_ids: FxHashMap<sparkles_core::sparql::output::OwnedTerm, Id> =
        FxHashMap::default();
    let nodes: Vec<Id> = entries
        .iter()
        .map(|e| match e.id {
            Some(id) => id,
            None => *absent_ids.entry(e.node.clone()).or_insert_with(|| {
                absent.push(e.node.clone());
                Id::local(absent.len() as u64 - 1)
            }),
        })
        .collect();
    let ir = schema.ir();
    let env = Env::new(ir, snap, &data, schema.prefixes(), absent, opts, root);
    let seeds: Vec<(Id, PairKind)> = nodes
        .iter()
        .zip(&entries)
        .map(|(&n, e)| (n, e.kind))
        .collect();
    let mut all_seeds = seeds.clone();
    all_seeds.extend_from_slice(extra);

    // start actions, once per validation
    let tracing = Registry::new(opts.semact_trace);
    let mut start = ActCtx::new(&tracing, snap);
    let started_ok = tracing.start(&ir.start_acts, &mut start)?;
    let start_failure = (!started_ok).then(|| {
        let f = start.failure.take();
        ShexFailure::SemAct {
            extension: f.as_ref().map_or_else(String::new, |f| f.extension.clone()),
            message: f.map_or_else(|| "semantic action failed".to_string(), |f| f.message),
        }
    });

    let t = typing::run_fixed(&env, &all_seeds, fixed)?;
    tracing::debug!(pairs = t.len(), waves = ?t.waves, "shex typing");

    // per result: the verdict, then the prints and the reasons
    let acts = schema.has_semantic_actions();
    let items: Vec<u32> = (0..entries.len() as u32).collect();
    let read = |n: Id, k: PairKind| t.read_with(n, k, fixed);
    let prefixes = schema.prefixes();
    let outcomes = env.par_map(&tracing, &items, |w: &mut Worker<'_>, i| {
        let (node, kind) = seeds[i as usize];
        let idx = t.get(node, kind).expect("seeded pairs are discovered");
        let mut ok = t.value(idx) && start_failure.is_none();
        let mut failures = Vec::new();
        let (mut prints, mut unknown) = (Vec::new(), FxHashMap::default());
        if acts {
            // the pair once more with the tracing handlers (the workers'), for its prints
            w.cx.prints.clear();
            w.cx.unknown.clear();
            env.eval(ir.pairs[kind.index()].se, node, &read, w)?;
            prints = std::mem::take(&mut w.cx.prints);
            unknown = std::mem::take(&mut w.cx.unknown);
        }
        if let Some(f) = &start_failure {
            failures.push(f.clone());
        } else if !ok {
            let mut x = Worker::new(&env.registry, snap);
            failures = explain::explain(&env, &read, prefixes, &mut x, node, kind)?;
            if failures.is_empty() {
                failures.push(ShexFailure::NoMatch {
                    detail: "the shape expression is not satisfied".to_string(),
                });
            }
        }
        ok &= failures.is_empty();
        if let Some(report) = &report_bytes
            && (!opts.only_nonconformant || !ok)
        {
            let entry = &entries[i as usize];
            let reason = failures
                .first()
                .map(|f| explain::reason(entry.node.as_term(), f, prefixes));
            if !entry_bytes_paid {
                report.admit(entry_bytes(&entry.node, &entry.shape))?;
            }
            report.admit(explanation_bytes(&failures, &prints, reason.as_deref()))?;
        }
        Ok((ok, failures, prints, unknown))
    })?;

    let mut out = ResultMap {
        conforms: true,
        ..Default::default()
    };
    let mut unknown: FxHashMap<String, usize> = FxHashMap::default();
    let mut first = true;
    for (e, (ok, failures, prints, u)) in entries.into_iter().zip(outcomes) {
        for (k, n) in u {
            *unknown.entry(k).or_default() += n;
        }
        if ok {
            out.conformant += 1;
        } else {
            out.nonconformant += 1;
            out.conforms = false;
        }
        if opts.only_nonconformant && ok {
            continue;
        }
        if let Some(limit) = opts.max_results
            && out.results.len() >= limit
        {
            return Err(TooManyResults { limit }.into());
        }
        let mut all_prints = Vec::new();
        if first {
            if let Some(report) = &report_bytes {
                report.admit(print_bytes(&start.prints))?;
            }
            all_prints.append(&mut start.prints);
            first = false;
        }
        all_prints.extend(prints);
        out.results.push(ShapeResult {
            reason: failures
                .first()
                .map(|f| explain::reason(e.node.as_term(), f, prefixes)),
            node: e.node,
            shape: e.shape,
            status: if ok {
                Status::Conformant
            } else {
                Status::Nonconformant
            },
            failures,
            prints: all_prints,
        });
    }
    for (k, n) in start.unknown {
        *unknown.entry(k).or_default() += n;
    }
    warnings.extend(unknown_warnings(&unknown));
    out.warnings = warnings;
    out.millis = started.elapsed().as_millis() as u64;
    out.stats = ValidationStats {
        pairs: t.len(),
        evaluations: t.evaluations,
        waves: t.waves.clone(),
    };
    let values = t
        .values()
        .filter(|((n, _), _)| n.tag() != sparkles_core::id::Tag::Local)
        .collect();
    Ok((out, values))
}

fn print_bytes(prints: &[String]) -> u64 {
    prints
        .iter()
        .map(|print| (std::mem::size_of::<String>() as u64).saturating_add(print.len() as u64))
        .fold(0, u64::saturating_add)
}

pub(crate) fn entry_bytes(
    node: &sparkles_core::sparql::output::OwnedTerm,
    shape: &ShapeLabel,
) -> u64 {
    let label = match shape {
        ShapeLabel::Iri(label) | ShapeLabel::BNode(label) => label.len() as u64,
        ShapeLabel::Start => 0,
    };
    (std::mem::size_of::<ShapeResult>() as u64)
        .saturating_add(
            sparkles_core::sparql::output::term_bytes(node.as_term()).unwrap_or(u64::MAX),
        )
        .saturating_add(label)
}

fn explanation_bytes(failures: &[ShexFailure], prints: &[String], reason: Option<&str>) -> u64 {
    use sparkles_core::sparql::output::term_bytes;
    let failure_bytes = failures
        .iter()
        .map(|failure| {
            let scalar = std::mem::size_of::<ShexFailure>() as u64;
            let payload = |value: &sparkles_core::sparql::output::OwnedTerm| {
                term_bytes(value.as_term()).unwrap_or(u64::MAX)
            };
            match failure {
                ShexFailure::NodeKind { value, constraint }
                | ShexFailure::Datatype { value, constraint }
                | ShexFailure::Facet { value, constraint }
                | ShexFailure::ValueSet { value, constraint } => scalar
                    .saturating_add(payload(value))
                    .saturating_add(constraint.len() as u64),
                ShexFailure::Closed { predicate, value }
                | ShexFailure::Extra { predicate, value }
                | ShexFailure::Reference {
                    shape: predicate,
                    value,
                } => scalar
                    .saturating_add(payload(value))
                    .saturating_add(predicate.len() as u64),
                ShexFailure::Cardinality { predicate, .. } => {
                    scalar.saturating_add(predicate.len() as u64)
                }
                ShexFailure::NoMatch { detail } => scalar.saturating_add(detail.len() as u64),
                ShexFailure::Not { shape } | ShexFailure::External { shape } => {
                    scalar.saturating_add(shape.len() as u64)
                }
                ShexFailure::SemAct { extension, message } => scalar
                    .saturating_add(extension.len() as u64)
                    .saturating_add(message.len() as u64),
            }
        })
        .fold(0, u64::saturating_add);
    (reason.map_or(0, str::len) as u64)
        .saturating_add(failure_bytes)
        .saturating_add(print_bytes(prints))
}

/// See [`crate::validate_node`].
pub fn validate_node(
    snap: &Arc<Snapshot>,
    schema: &CompiledSchema,
    node: &Term,
    shape: &ShapeLabel,
    opts: &ValidateOptions,
) -> anyhow::Result<ShapeResult> {
    let map = ShapeMap(vec![crate::Association {
        node: NodeSelector::Term(node.clone()),
        shape: shape.clone(),
    }]);
    let opts = ValidateOptions {
        only_nonconformant: false,
        max_results: None,
        ..opts.clone()
    };
    let mut r = validate(snap, schema, &map, &opts)?;
    r.results
        .pop()
        .ok_or_else(|| anyhow::anyhow!("no result for the node"))
}

/// Does the schema have semantic actions anywhere?
impl CompiledSchema {
    /// Whether the compiled schema has semantic actions, including START actions,
    /// referenced shapes, triple constraints and groups.
    pub fn has_semantic_actions(&self) -> bool {
        let ir = self.ir();
        !ir.start_acts.is_empty()
            || ir.shapes.iter().any(|s| {
                !s.sem_acts.is_empty()
                    || s.tcs.iter().any(|t| !t.sem_acts.is_empty())
                    || s.te.iter().any(|t| match t {
                        Te::Tc(_) => false,
                        Te::EachOf { acts, .. } | Te::OneOf { acts, .. } => !acts.is_empty(),
                    })
            })
    }
}

#[cfg(test)]
mod tests;

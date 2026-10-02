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
use sparkles::id::Id;
use sparkles::store::Snapshot;
use sparkles::validation::DataGraph;
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
    let started = Instant::now();
    let data = DataGraph::new(
        snap.clone(),
        opts.data_graph.as_deref(),
        &opts.extra_graphs,
        &opts.exclude_graphs,
    )?;
    // the timeout covers the expansion (SPARQL selectors) and the typing together
    let deadline = opts.timeout.map(|t| started + t);
    let (entries, mut warnings) = shapemap::expand(map, &data, schema, opts, deadline)?;
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
    if let Some(limit) = opts.max_results
        && !opts.only_nonconformant
        && entries.len() > limit
    {
        return Err(TooManyResults { limit }.into());
    }

    // nodes that are not in the store get local ids
    let mut absent: Vec<Term> = Vec::new();
    let mut absent_ids: FxHashMap<Term, Id> = FxHashMap::default();
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
    let env = Env::new(ir, snap, &data, schema.prefixes(), absent, opts);
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
    let acts = has_acts(schema);
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
        let mut all_prints = Vec::new();
        if first {
            all_prints.append(&mut start.prints);
            first = false;
        }
        all_prints.extend(prints);
        out.results.push(ShapeResult {
            reason: failures
                .first()
                .map(|f| explain::reason(&e.node, f, prefixes)),
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
    if let Some(limit) = opts.max_results
        && out.results.len() > limit
    {
        return Err(TooManyResults { limit }.into());
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
        .filter(|((n, _), _)| n.tag() != sparkles::id::Tag::Local)
        .collect();
    Ok((out, values))
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
fn has_acts(schema: &CompiledSchema) -> bool {
    let ir = schema.ir();
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

#[cfg(test)]
mod tests;

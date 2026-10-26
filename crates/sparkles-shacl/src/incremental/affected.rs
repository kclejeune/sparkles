//! Bounded affected-target validation over one writer's before/after snapshots.

use super::bounded::{self, Work};
use super::targets::Targets;
use super::{Fallback, Model};
use crate::data::DataGraph;
use crate::validate::{Engine, Sel, validate_selected};
use crate::{Shapes, ValidateOptions, ValidationReport};
use oxrdf::{GraphName, Quad, Term};
use rustc_hash::FxHashSet;
use sparkles_core::error::{Budget, BudgetKind, Error};
use sparkles_core::id::Id;
use sparkles_core::store::Snapshot;
use std::sync::Arc;

/// Discovery allowances shared across shapes, snapshots, paths and fallback targets.
#[derive(Clone, Copy, Debug)]
pub struct AffectedLimits {
    /// Distinct actual target nodes in addition to directly touched nodes.
    pub additional_focus: usize,
    /// Scanned rows, structural/copy work and retained discovery entries.
    /// Analyzer lexical copies charge each 128-byte chunk before allocation.
    pub work: u64,
}

impl Default for AffectedLimits {
    fn default() -> Self {
        Self {
            additional_focus: 500,
            work: 100_000,
        }
    }
}

/// Admit target-expression producers before form model construction can evaluate
/// them to decide field applicability. This performs no data query or target output.
pub fn check_target_producers(
    shapes: &Shapes,
    opts: &ValidateOptions,
    max_work: u64,
) -> anyhow::Result<()> {
    let mut work = Work::new(opts, max_work);
    for shape in &shapes.shapes {
        work.charge()?;
        if shape.deactivated {
            continue;
        }
        for target in &shape.targets {
            match target {
                crate::Target::Expr(e) => {
                    super::targets::static_expression(shapes, *e, 0, &mut work)?
                }
                crate::Target::Where(w) => super::targets::local_filter(shapes, *w, 0, &mut work)?,
                _ => {}
            }
        }
    }
    Ok(())
}

fn deadline_options(opts: &ValidateOptions, start: std::time::Instant) -> ValidateOptions {
    let mut normalized = opts.clone();
    normalized.deadline = opts
        .deadline
        .into_iter()
        .chain(opts.timeout.map(|timeout| start + timeout))
        .min();
    normalized.timeout = None;
    normalized
}

/// Validate directly touched and potentially affected targets of the changed quads.
/// Snapshots must share one dataset family and the same generation/vocabulary owner,
/// as the base and dry-run view of a writer do. Incomplete admission is an error,
/// never a successful partial conformance report. No write or baseline report is made.
pub fn validate_affected(
    before: &Arc<Snapshot>,
    after: &Arc<Snapshot>,
    shapes: &Shapes,
    changes: &[Quad],
    touched: &[Term],
    opts: &ValidateOptions,
    limits: AffectedLimits,
) -> anyhow::Result<ValidationReport> {
    let normalized = deadline_options(opts, std::time::Instant::now());
    let opts = &normalized;
    if before.dataset_id != after.dataset_id || !Arc::ptr_eq(&before.generation, &after.generation)
    {
        return Err(Error::invalid(
            "affected validation requires one writer generation and dataset family",
        )
        .into());
    }
    let mut work = Work::new(opts, limits.work);
    work.check()?;
    // Admit analysis tables before allocation, then propagate controls through the
    // existing dependency analyzer rather than treating an interrupted plan as global.
    for _ in &shapes.shapes {
        work.charge()?;
    }
    let mut failure = None;
    let model = {
        let mut control = || {
            if failure.is_some() {
                return Err(Fallback::Budget);
            }
            match work.charge() {
                Ok(()) => Ok(()),
                Err(e) => {
                    failure = Some(e);
                    Err(Fallback::Budget)
                }
            }
        };
        Model::new_controlled(shapes, Some(&mut control))
    };
    if let Some(e) = failure {
        return Err(e.into());
    }
    if model.producer_refused {
        return Err(Error::unsupported(
            "form query dependency requires an early-bounded localization producer",
        )
        .into());
    }
    let (mut post, ids) = DataGraph::new(
        after.clone(),
        opts.data_graph.as_deref(),
        &opts.extra_graphs,
        &opts.exclude_graphs,
        shapes,
    )?;
    let (pre, _) = DataGraph::new(
        before.clone(),
        opts.data_graph.as_deref(),
        &opts.extra_graphs,
        &opts.exclude_graphs,
        shapes,
    )?;
    let mut triples = Vec::new();
    let mut triple_seen = FxHashSet::default();
    for quad in changes {
        work.charge()?;
        let graph = match &quad.graph_name {
            GraphName::DefaultGraph => Some(Id::DEFAULT_GRAPH),
            GraphName::NamedNode(n) => after.lookup_iri(n.as_str()),
            GraphName::BlankNode(b) => after.lookup_term(&Term::BlankNode(b.clone())),
        };
        if !graph.is_some_and(|g| post.sel.accepts(g.0) || pre.sel.accepts(g.0)) {
            continue;
        }
        let terms = [
            Term::from(quad.subject.clone()),
            Term::NamedNode(quad.predicate.clone()),
            quad.object.clone(),
        ];
        let resolved = terms.each_ref().map(|t| after.lookup_term(t));
        let [Some(s), Some(p), Some(o)] = resolved else {
            continue;
        };
        if !triple_seen.contains(&[s, p, o]) {
            work.charge()?;
            triple_seen.insert([s, p, o]);
            triples.push([s, p, o]);
        }
    }
    let mut direct = Vec::new();
    let mut direct_seen = FxHashSet::default();
    for term in touched {
        work.charge()?;
        let id = crate::validate::resolve_focus(&mut post, term, shapes);
        if !direct_seen.contains(&id) {
            work.charge()?;
            direct_seen.insert(id);
            direct.push(id);
        }
    }
    let candidates = bounded::candidates(&model, [&post, &pre], &triples, &direct, &mut work)?;
    let engine = Engine::new(shapes, &post, ids.clone(), opts)?;
    let targets = Targets::new(&post, &engine);
    let mut extra_seen = FxHashSet::default();
    let mut selections = Vec::new();
    for (si, candidate_nodes) in candidates.into_iter().enumerate() {
        work.charge()?;
        if !model.validated(si) {
            selections.push(Sel::Skip);
            continue;
        }
        let mut selected = Vec::new();
        let mut seen = FxHashSet::default();
        let mut admit = |id, work: &mut Work<'_>| {
            if seen.contains(&id) {
                return Ok(());
            }
            if !direct_seen.contains(&id) && !extra_seen.contains(&id) {
                let requested = extra_seen.len().saturating_add(1);
                if requested > limits.additional_focus {
                    return Err(Error::BudgetExceeded(Budget {
                        kind: BudgetKind::ValidationWork,
                        limit: limits.additional_focus as u64,
                        requested: requested as u64,
                    }));
                }
                work.charge()?;
                extra_seen.insert(id);
            }
            work.charge()?;
            seen.insert(id);
            selected.push(id);
            Ok(())
        };
        for &id in &direct {
            if targets.contains(si, id, &mut work)? {
                admit(id, &mut work)?;
            }
        }
        match candidate_nodes {
            Some(nodes) => {
                for id in nodes {
                    if targets.contains(si, id, &mut work)? {
                        admit(id, &mut work)?;
                    }
                }
            }
            None => targets.visit_all(si, &mut work, &mut admit)?,
        }
        selections.push(Sel::Admitted(selected));
    }
    work.check()?;
    let runs = validate_selected(&mut post, &ids, shapes, opts, &selections, None)?;
    let results: Vec<_> = runs.into_iter().flat_map(|r| r.results).collect();
    Ok(ValidationReport {
        conforms: results.is_empty(),
        results,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use sparkles_core::store::{Store, StoreOptions};
    use std::time::{Duration, Instant};

    #[test]
    fn relative_timeout_is_one_absolute_bound_for_every_engine() {
        let start = Instant::now() - Duration::from_secs(1);
        let opts = ValidateOptions {
            timeout: Some(Duration::from_secs(10)),
            ..Default::default()
        };
        let normalized = deadline_options(&opts, start);
        assert_eq!(normalized.deadline, Some(start + Duration::from_secs(10)));
        assert_eq!(normalized.timeout, None);
        let store = Store::in_memory(StoreOptions::default());
        let shapes = Shapes::parse("", sparkles_core::io::RdfFormat::Turtle, None).unwrap();
        let (data, ids) = DataGraph::new(store.snapshot(), None, &[], &[], &shapes).unwrap();
        for _ in 0..2 {
            let engine = Engine::new(&shapes, &data, ids.clone(), &normalized).unwrap();
            assert_eq!(engine.deadline, normalized.deadline);
        }
        let earlier = start + Duration::from_secs(5);
        let opts = ValidateOptions {
            deadline: Some(earlier),
            ..opts
        };
        assert_eq!(deadline_options(&opts, start).deadline, Some(earlier));
    }
}

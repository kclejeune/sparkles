//! Consistency annotations for verified why-not hypotheses on the caller's view.

use super::analysis::{self, AnalysisControl, AnalysisOptions, AnalysisProgram};
use super::diff::{check_complete, finding_key};
use crate::store::Snapshot;
use crate::{Dataset, Error, Result};
use oxrdf::{NamedNode, NamedOrBlankNode, Term, Triple};
use sparkles_reasoner::diagnostics::{CHECKS, Severity};
use sparkles_reasoner::whynot::WhyNot;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

fn replace(term: &Term, placeholders: &HashMap<String, Term>) -> Term {
    match term {
        Term::BlankNode(node) => placeholders
            .get(node.as_str())
            .cloned()
            .unwrap_or_else(|| term.clone()),
        Term::Triple(triple) => {
            let subject = replace(&triple.subject.clone().into(), placeholders);
            Term::Triple(Box::new(Triple::new(
                NamedOrBlankNode::try_from(subject)
                    .expect("replacing a blank preserves subject kind"),
                triple.predicate.clone(),
                replace(&triple.object, placeholders),
            )))
        }
        _ => term.clone(),
    }
}

fn bounded(error: &Error) -> bool {
    matches!(
        error.code(),
        "reasoning-budget" | "budget-exceeded" | "timeout" | "storage-full"
    )
}

fn copy_visible(
    store: &crate::store::Store,
    snapshot: &Arc<Snapshot>,
    options: crate::store::StoreOptions,
    control: &AnalysisControl,
    include: impl FnMut(&[crate::id::Id; 4]) -> Result<bool>,
) -> Result<crate::store::Store> {
    store
        .copy_view_to_memory(snapshot, options, &control.write_options(), include)
        .map_err(|error| control.error(error))
}

/// Annotate new inconsistencies without publishing a hypothesis or altering its terms.
/// Budget exhaustion leaves unchecked hypotheses unknown. Cancellation propagates.
pub fn check_consistency(
    dataset: &Dataset,
    snapshot: &Arc<Snapshot>,
    reasoner: &sparkles_reasoner::query::QueryReasoner,
    options: &AnalysisOptions,
    answer: &mut WhyNot,
) -> Result<()> {
    let WhyNot::NotEntailed {
        rules, hypotheses, ..
    } = answer
    else {
        return Ok(());
    };
    for hypothesis in hypotheses.iter_mut() {
        hypothesis.inconsistent = None;
    }
    if hypotheses.is_empty() {
        return Ok(());
    }
    let retained = hypotheses
        .iter()
        .map(|hypothesis| {
            128 + hypothesis.tree.heap_bytes()
                + hypothesis
                    .missing
                    .iter()
                    .flatten()
                    .map(crate::explain::term_bytes)
                    .sum::<usize>()
                + hypothesis
                    .placeholders
                    .iter()
                    .map(|label| 32 + label.len())
                    .sum::<usize>()
        })
        .sum::<usize>()
        + rules
            .iter()
            .map(|rule| {
                128 + rule.rule.name.len()
                    + rule
                        .unifier
                        .iter()
                        .map(|(name, term)| 32 + name.len() + crate::explain::term_bytes(term))
                        .sum::<usize>()
                    + rule
                        .blocked_by
                        .iter()
                        .map(|block| {
                            64 + block.call.len()
                                + block.fact.as_ref().map_or(0, |fact| {
                                    fact.iter().map(crate::explain::term_bytes).sum()
                                })
                        })
                        .sum::<usize>()
            })
            .sum::<usize>();
    let mut options = options.clone();
    options.max_memory = options
        .max_memory
        .map(|limit| limit.saturating_sub(retained));
    let mut run = || -> Result<()> {
        let control = AnalysisControl::new(&options)?;
        let extras = reasoner.extras().clone();
        options.program = Some(AnalysisProgram {
            profile: reasoner.profile().clone(),
            extras,
            inputs: reasoner.inputs().clone(),
        });
        let mut program = analysis::program(dataset, &options)?;
        let view = analysis::view(snapshot, &options)?;
        let graphs = analysis::input_graphs(&view, &program, &options);
        let copy_options = crate::store::StoreOptions {
            max_memory_bytes: options.max_memory.map(|bytes| (bytes / 8) as u64),
            result_cache_bytes: 0,
            service_cache_bytes: 0,
            ..Default::default()
        };
        let base = Dataset::from_store(copy_visible(
            dataset.store(),
            &view,
            copy_options.clone(),
            &control,
            |quad| Ok(graphs.contains(&quad[3])),
        )?);

        let graph = loop {
            let graph = NamedNode::new_unchecked(format!(
                "urn:x-sparkles:hypothesis:{}",
                uuid::Uuid::new_v4()
            ));
            if view.lookup_iri(graph.as_str()).is_none() {
                break graph;
            }
        };
        program.inputs = sparkles_reasoner::Inputs {
            data_graphs: graphs
                .iter()
                .filter_map(|id| {
                    if *id == crate::id::Id::DEFAULT_GRAPH {
                        Some(sparkles_reasoner::GraphRef::Default)
                    } else {
                        match view.term(*id) {
                            Some(Term::NamedNode(node)) => {
                                Some(sparkles_reasoner::GraphRef::Named(node.into_string()))
                            }
                            _ => None,
                        }
                    }
                })
                .chain(std::iter::once(sparkles_reasoner::GraphRef::Named(
                    graph.as_str().into(),
                )))
                .collect(),
            ontology_graphs: Vec::new(),
            imports: sparkles_reasoner::ImportMode::None,
            location_mapping: Default::default(),
        };
        let checks: Vec<_> = CHECKS
            .iter()
            .filter(|check| check.severity == Severity::Inconsistency)
            .map(|check| check.id.to_string())
            .collect();
        let base_snapshot = base.store().snapshot();
        let mut private_options = options.clone();
        private_options.access = None;
        private_options.max_inferred = private_options
            .max_inferred
            .or(Some(reasoner.max_derived()));
        let before = analysis::analyze(
            &base,
            &base_snapshot,
            &program,
            &private_options,
            &control,
            0,
            None,
            &checks,
        )?;
        let known: HashSet<_> = before
            .diagnostics
            .findings
            .iter()
            .map(finding_key)
            .collect();
        let before_complete = checks
            .iter()
            .all(|check| check_complete(&before.diagnostics, check));
        for hypothesis in hypotheses.iter_mut() {
            control.check()?;
            let candidate = Dataset::from_store(copy_visible(
                base.store(),
                &base_snapshot,
                copy_options.clone(),
                &control,
                |_| Ok(true),
            )?);
            (|| -> Result<()> {
                let mut transaction = candidate
                    .store()
                    .write_with(crate::commit::CommitKind::Update, control.write_options());
                let placeholders: HashMap<_, _> = hypothesis
                    .placeholders
                    .iter()
                    .map(|label| {
                        (
                            label.clone(),
                            Term::BlankNode(crate::store::bnode_for(transaction.new_bnode())),
                        )
                    })
                    .collect();
                let graph = transaction.intern(&graph.clone().into())?;
                for triple in &hypothesis.missing {
                    let triple = triple.each_ref().map(|term| replace(term, &placeholders));
                    let ids = [
                        transaction.intern(&triple[0])?,
                        transaction.intern(&triple[1])?,
                        transaction.intern(&triple[2])?,
                        graph,
                    ];
                    transaction.insert(ids)?;
                }
                transaction.commit()?;
                Ok(())
            })()
            .map_err(|error| control.error(error))?;
            let after = analysis::analyze(
                &candidate,
                &candidate.store().snapshot(),
                &program,
                &private_options,
                &control,
                before.bytes,
                None,
                &checks,
            )?;
            let new = after.diagnostics.findings.iter().any(|finding| {
                !known.contains(&finding_key(finding))
                    && check_complete(&before.diagnostics, finding.check)
            });
            hypothesis.inconsistent = if new {
                Some(true)
            } else if before_complete
                && checks
                    .iter()
                    .all(|check| check_complete(&after.diagnostics, check))
            {
                Some(false)
            } else {
                None
            };
        }
        Ok(())
    };
    match run() {
        Err(error) if bounded(&error) => Ok(()),
        outcome => outcome,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    #[test]
    fn private_copy_timeout_is_bounded_but_caller_cancel_propagates() {
        let dataset = Dataset::memory();
        dataset.update("INSERT DATA {<urn:s> <urn:p> 1}").unwrap();
        let snapshot = dataset.store().snapshot();
        let mut control = AnalysisControl::new(&AnalysisOptions::default()).unwrap();
        control.deadline = Some(std::time::Instant::now());
        control.cancel.store(true, Ordering::Relaxed);
        let result = copy_visible(
            dataset.store(),
            &snapshot,
            Default::default(),
            &control,
            |_| Ok(true),
        );
        let error = match result {
            Err(error) => error,
            Ok(_) => panic!("copy must stop"),
        };
        assert!(bounded(&error), "{error}");
        let options = AnalysisOptions {
            cancel: Some(Arc::new(AtomicBool::new(false))),
            ..Default::default()
        };
        let control = AnalysisControl::new(&options).unwrap();
        options.cancel.unwrap().store(true, Ordering::Relaxed);
        control.cancel.store(true, Ordering::Relaxed);
        let result = copy_visible(
            dataset.store(),
            &snapshot,
            Default::default(),
            &control,
            |_| Ok(true),
        );
        let error = match result {
            Err(error) => error,
            Ok(_) => panic!("copy must stop"),
        };
        assert!(matches!(error, Error::Cancelled), "{error}");
    }
}

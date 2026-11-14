//! The `shacl-af` profile: SHACL Advanced Features rules read from a shapes graph
//! (spec F23, Phase 5, roadmap item 838).
//!
//! The rules come from [`sparkles_shacl::rules`], with SPIN rules translated to SPARQL
//! rules ([`sparkles_shacl::spin`]). They run in groups of equal (shape `sh:order`, rule
//! `sh:order`), in ascending order. A group runs in rounds: each round evaluates every
//! rule of the group over the input graphs and the facts derived before the round, and
//! adds what it inferred at its end. A group ends when a round adds nothing, or after
//! one round with [`ShaclAf::one_pass`], which is the evaluation of TopBraid's rule
//! engine. A group that has not reached a fixed point after [`ShaclAf::max_rounds`]
//! rounds fails the run with an error that names its order.
//!
//! The rules read the state through an overlay of the run's snapshot that holds the
//! derived facts in the first input graph, so that targets, conditions, node
//! expressions and SPARQL queries see them as they see the input. The terms the rules
//! compute are interned in the run's write transaction, whose delta vocabulary the
//! overlay reads, and a template blank node of a SPARQL rule becomes one new blank node
//! per rule, template label and solution, so a rule that makes blank nodes reaches a
//! fixed point. A run reads the shapes graph into memory, and each round decodes the
//! nodes the rules produce, so the profile suits rule sets over data that fits a SHACL
//! validation. An incremental run of the profile materializes in full.

use crate::budget::{Budget, Held, WorkingWarnings};
use crate::terms::Terms;
use crate::{Derivation, InferredLimitExceeded, ReasonOptions, facts, graph, progress};
use anyhow::{Context as _, Result, bail};
use oxrdf::Term;
use rustc_hash::FxHashMap;
use sparkles_core::id::Id;
use sparkles_core::sparql::ctx::DEFAULT_GRAPH_IRI;
use sparkles_core::store::{Snapshot, WriteTxn, parse_bnode_label};
use sparkles_shacl::rules::{Node, RuleData};
use sparkles_shacl::{Shapes, ValidateOptions};
use std::mem::size_of;
use std::sync::Arc;
use std::sync::atomic::Ordering;

/// The default of [`ShaclAf::max_rounds`].
pub const DEFAULT_MAX_ROUNDS: usize = 100;

/// The options of the `shacl-af` profile.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ShaclAf {
    /// The graphs the shapes and their rules are read from, by IRI
    /// (`urn:x-arq:DefaultGraph` is the default graph). Empty reads them from the input
    /// graphs, as TopBraid's `shaclinfer` does without a shapes file.
    pub shapes: Vec<String>,
    /// Run each group of rules once, so that its rules do not see each other's facts,
    /// rather than to a fixed point.
    pub one_pass: bool,
    /// The most rounds a group of rules may take to reach its fixed point.
    pub max_rounds: usize,
}

impl Default for ShaclAf {
    fn default() -> Self {
        ShaclAf {
            shapes: Vec::new(),
            one_pass: false,
            max_rounds: DEFAULT_MAX_ROUNDS,
        }
    }
}

impl ShaclAf {
    /// The profile's text form: `shacl-af`, then its options that differ from the
    /// defaults as `?shapes=IRI&mode=one-pass&max-rounds=N`, which
    /// [`Profile::from_str`](crate::Profile) reads back.
    pub fn text(&self) -> String {
        let mut params = Vec::new();
        for s in &self.shapes {
            params.push(format!("shapes={s}"));
        }
        if self.one_pass {
            params.push("mode=one-pass".into());
        }
        if self.max_rounds != DEFAULT_MAX_ROUNDS {
            params.push(format!("max-rounds={}", self.max_rounds));
        }
        if params.is_empty() {
            "shacl-af".into()
        } else {
            format!("shacl-af?{}", params.join("&"))
        }
    }

    /// Read the options after `shacl-af?`.
    pub fn parse_params(params: &str) -> Result<ShaclAf, String> {
        let mut out = ShaclAf::default();
        for p in params.split('&').filter(|p| !p.is_empty()) {
            let (k, v) = p.split_once('=').unwrap_or((p, ""));
            match k {
                "shapes" if !v.is_empty() => out.shapes.push(v.to_string()),
                "mode" => match v {
                    "one-pass" => out.one_pass = true,
                    "fixpoint" | "fixed-point" => out.one_pass = false,
                    _ => return Err(format!("mode must be one-pass or fixpoint, not '{v}'")),
                },
                "max-rounds" => match v.parse::<usize>() {
                    Ok(n) if n > 0 => out.max_rounds = n,
                    _ => return Err(format!("max-rounds must be a positive number, not '{v}'")),
                },
                _ => return Err(format!("unknown option '{p}'")),
            }
        }
        Ok(out)
    }
}

/// One group of rules of a `shacl-af` run ([`crate::ReasonReport::rule_groups`]).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RuleGroup {
    /// the shapes' `sh:order`
    pub shape_order: f64,
    /// the rules' `sh:order`
    pub order: f64,
    /// the rules of the group
    pub rules: usize,
    /// the rounds the group took, the last of which added nothing unless the run was
    /// one pass
    pub rounds: usize,
}

/// The IRI of an input graph, as the SHACL data graph names it.
fn graph_iri(snap: &Snapshot, g: Id) -> Result<String> {
    if g == Id::DEFAULT_GRAPH {
        return Ok(DEFAULT_GRAPH_IRI.to_string());
    }
    match snap.term(g) {
        Some(Term::NamedNode(n)) => Ok(n.into_string()),
        other => bail!("an input graph has no IRI: {other:?}"),
    }
}

/// The bytes one derived fact holds in the overlay the rules read: a key in each of the
/// seven permutations of the snapshot's delta, with the nodes of its persistent sets.
const OVERLAY_FACT_BYTES: usize = 7 * 64;

/// Turns the nodes the rules produce into ids of the run's write transaction.
struct Interner<'t, 's> {
    txn: &'t mut WriteTxn<'s>,
    /// the blank node of each (rule, template label, solution)
    blanks: FxHashMap<(usize, String, Vec<u64>), Id>,
    /// the blank nodes that computed terms named
    labels: FxHashMap<String, Id>,
}

impl Interner<'_, '_> {
    fn id(&mut self, rule: usize, n: &Node) -> Result<u64> {
        Ok(match n {
            Node::Id(id) => id.0,
            Node::Term(Term::BlankNode(b)) => match parse_bnode_label(b.as_str()) {
                Some(id) => id.0,
                None => match self.labels.get(b.as_str()) {
                    Some(id) => id.0,
                    None => {
                        let id = self.txn.new_bnode();
                        self.labels.insert(b.as_str().to_string(), id);
                        id.0
                    }
                },
            },
            Node::Term(t) => self.txn.intern(t)?.0,
            Node::Blank { label, solution } => {
                let mut key = Vec::with_capacity(solution.len());
                for s in solution {
                    key.push(self.id(rule, s)?);
                }
                let k = (rule, label.clone(), key);
                match self.blanks.get(&k) {
                    Some(id) => id.0,
                    None => {
                        let id = self.txn.new_bnode();
                        self.blanks.insert(k, id);
                        id.0
                    }
                }
            }
        })
    }
}

/// Derive the closure of the `shacl-af` rules over the input graphs `graphs` of `snap`,
/// the state `txn` holds.
pub(crate) fn derive(
    txn: &mut WriteTxn<'_>,
    snap: Arc<Snapshot>,
    cfg: &ShaclAf,
    graphs: &[Id],
    opts: &ReasonOptions,
) -> Result<(Derivation, Vec<RuleGroup>)> {
    progress(opts, 0.0, "reading the shapes graph");
    // the terms the rules compute go to the transaction's delta vocabulary, which a
    // snapshot reads only up to its length when it was taken: the run's view reads all
    let snap = {
        let mut s = (*snap).clone();
        s.dvocab_len = u64::MAX;
        Arc::new(s)
    };
    let budget = Budget::new(opts.max_memory);
    let base = Arc::new(facts::Base::new(snap.clone(), graphs)?);
    let mut g = graph::Graph::with_base(base.clone());
    g.set_budget(&budget)?;
    let terms = Terms::new(snap.clone());
    terms.set_budget(&budget)?;
    let warnings = WorkingWarnings::new(&budget);
    let Some(&first) = graphs.first() else {
        bail!("the shacl-af profile has no input graph to read");
    };
    let iris = graphs
        .iter()
        .map(|&g| graph_iri(&snap, g))
        .collect::<Result<Vec<_>>>()?;
    let shape_graphs = if cfg.shapes.is_empty() {
        iris.clone()
    } else {
        cfg.shapes.clone()
    };
    let shapes = Shapes::for_rules(&snap, &shape_graphs, &[]).context("reading the shapes")?;
    let rules = shapes.rules().context("reading the SHACL rules")?;
    // each query of a rule gets the run's memory budget, and the cancel flag that a
    // deadline of the caller sets stops it
    let vopts = ValidateOptions {
        cancel: opts.cancel.clone(),
        query_memory: opts.max_memory.map(|m| m as u64),
        ..Default::default()
    };
    let mut interner = Interner {
        txn,
        blanks: FxHashMap::default(),
        labels: FxHashMap::default(),
    };
    let cancelled = || {
        opts.cancel
            .as_ref()
            .is_some_and(|c| c.load(Ordering::Relaxed))
    };
    let mut overlay = snap.clone();
    let mut overlay_held = Held::new(&budget);
    let mut rounds = 0usize;
    let mut groups = Vec::new();
    let mut at = 0usize;
    while at < rules.len() {
        let key = rules[at].key();
        let end = at + rules[at..].iter().take_while(|r| r.key() == key).count();
        let group = at..end;
        at = end;
        let order = if key.0 == 0.0 {
            format!("{}", key.1)
        } else {
            format!("{} of shapes of order {}", key.1, key.0)
        };
        let mut round = 0usize;
        loop {
            if round == cfg.max_rounds {
                bail!(
                    "the SHACL rules of order {order} did not reach a fixed point within {} \
                     rounds ({} triples derived so far); raise max-rounds or check the rules \
                     for ones that keep making new nodes",
                    cfg.max_rounds,
                    g.derived_len()
                );
            }
            if cancelled() {
                bail!("reasoning cancelled");
            }
            budget.check()?;
            progress(
                opts,
                0.1 + 0.7 * (1.0 - 1.0 / (rounds as f32 + 1.0)),
                &format!(
                    "SHACL rules of order {order}, round {} ({} derived)",
                    round + 1,
                    g.derived_len()
                ),
            );
            let data = RuleData::new(overlay.clone(), &shapes, Some(&iris[0]), &iris[1..], &vopts)?;
            let mut found: Vec<[u64; 3]> = Vec::new();
            let mut found_held = Held::new(&budget);
            for ri in group.clone() {
                let rule = &rules[ri];
                let mut nodes: Vec<[Node; 3]> = Vec::new();
                data.run(&shapes, rule, &mut |t| {
                    nodes.push(t);
                    Ok(())
                })
                .with_context(|| format!("the SHACL rule {}", rule.node))?;
                for [s, p, o] in &nodes {
                    let t = [
                        interner.id(ri, s)?,
                        interner.id(ri, p)?,
                        interner.id(ri, o)?,
                    ];
                    if !g.contains(&t) {
                        found_held.try_grow(size_of::<[u64; 3]>())?;
                        found.push(t);
                    }
                }
            }
            drop(data);
            found.sort_unstable();
            found.dedup();
            round += 1;
            rounds += 1;
            if g.derived_len() + found.len() > opts.max_inferred {
                return Err(InferredLimitExceeded {
                    limit: opts.max_inferred,
                    context: format!(" (SHACL rules of order {order})"),
                }
                .into());
            }
            let added = g.try_add_batch(found.iter().copied())?;
            if added > 0 {
                overlay_held.try_grow(added * OVERLAY_FACT_BYTES)?;
                let quads: Vec<[Id; 4]> = found
                    .iter()
                    .map(|t| [Id(t[0]), Id(t[1]), Id(t[2]), first])
                    .collect();
                overlay = Arc::new(overlay.with_overlay(&quads, false)?);
            }
            drop(found_held);
            if added == 0 || cfg.one_pass {
                break;
            }
        }
        groups.push(RuleGroup {
            shape_order: key.0,
            order: key.1,
            rules: group.len(),
            rounds: round,
        });
    }
    budget.check_peak()?;
    let d = Derivation {
        graph: g,
        terms,
        rules: rules.len(),
        iterations: rounds,
        warnings,
        budget,
        defaults: None,
        base_scans: base.scans(),
        threads: 1,
    };
    Ok((d, groups))
}

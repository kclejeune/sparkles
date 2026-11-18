//! Operators that map each input row to any number of output rows on their own: index
//! joins, LET, UNFOLD and triple-term decomposition in input order, transitive paths
//! from the start nodes of their input, LATERAL, ARQ property functions, registered
//! property functions that declare a prefix output over an input, and text, hybrid,
//! history and spatial calls whose arguments their input binds. Each input
//! batch runs through the eager kernel. A path over a sequence or alternative first
//! reads its edge plan into a charged edge relation, which every batch then walks. Its output is charged state that later pulls
//! resume, so one batch that expands into many solutions leaves the following batches
//! bounded by the caller's cap. The input demand follows the expansion seen so far.
use super::{Buffer, CursorOptions, Operator, copy_rows, exec};
use crate::error::Result;
use crate::id::Id;
use crate::sparql::ctx::{Ctx, OwnedCharge};
use crate::sparql::plan::{Kind, Node};
use crate::sparql::table::{Table, VarId};
use rustc_hash::FxHashSet;
use std::sync::Arc;

/// Whether a plan node runs as an expanding operator over its input's batches.
pub(super) fn eligible(node: &Node) -> bool {
    auto_eligible(node)
        || match &node.kind {
            // A remote LATERAL sends its distinct inputs in bulk requests. It holds the
            // rows of an unfilled request until more input arrives and keeps the answers
            // of the inputs it has sent, so it sends the requests one eager call sends.
            Kind::Lateral(spec) => spec.service.is_some() && node.children.len() == 1,
            // A text or hybrid search over its group searches once per distinct query
            // value and joins the hits to the rows with that value, and `maxHits` bounds
            // each search. A hybrid search whose vector candidates come from its input
            // ranks them all at once, so it reads its whole input.
            Kind::TextSearch(spec) => spec.needs_input() && node.children.len() == 1,
            Kind::HybridSearch(spec) => {
                spec.needs_input()
                    && !spec.vector.as_ref().is_some_and(|v| v.candidates)
                    && node.children.len() == 1
            }
            // A vector search whose query vector the group binds searches once per
            // distinct query value, and `k` bounds each search. With `candidates:join`
            // it ranks the subjects of all of its rows at once, so it reads its whole
            // input.
            Kind::VectorSearch(spec) => {
                spec.needs_input()
                    && !spec.candidates
                    && !spec.order_fallback
                    && node.children.len() == 1
            }
            // History changes over the group look up the changes of each row's bound
            // values, and `hist:limit` bounds each lookup.
            Kind::HistoryChanges(_) => node.children.len() == 1,
            // A spatial call whose arguments the group binds searches once per distinct
            // binding of its arguments, and the limit bounds each search.
            Kind::SpatialPf(spec) => spec.needs_input() && node.children.len() == 1,
            _ => false,
        }
}

/// Whether automatic execution counts a plan node as an expanding operator. A kind
/// joins this set only in its own change, after a same-host comparison of its eager and
/// streamed runs, so the kinds that [`eligible`] adds beyond it run as expanding
/// operators only under an explicit streaming request.
pub(super) fn auto_eligible(node: &Node) -> bool {
    match &node.kind {
        Kind::IndexJoin(_) | Kind::Assign(..) | Kind::Unfold { .. } | Kind::Unpack { .. } => {
            node.children.len() == 1
        }
        // LATERAL evaluates its right side for each left row, and an ARQ property
        // function solves each input row, so the output of an input batch is the
        // output of those rows, and a prefix of the input gives a subset of the
        // output.
        Kind::Lateral(spec) => spec.service.is_none() && node.children.len() == 1,
        Kind::PropertyFn(_) => node.children.len() == 1,
        // A registered property function only when its registration declares that a
        // prefix of its input gives a prefix of its output.
        Kind::RegisteredProperty(spec) => spec.prefix_output() && node.children.len() == 1,
        // A path from the start nodes of its input (the last child), after the edge
        // plan when it has one.
        Kind::Path {
            bound_from_left: true,
            spec,
        } => node.children.len() == 1 + usize::from(spec.edge_vars.is_some()),
        _ => false,
    }
}

/// The variables an expanding operator may bind in a row whose input leaves them
/// unbound, which moves the row out of its input's order on them.
pub(super) fn fills(node: &Node) -> Vec<VarId> {
    match &node.kind {
        Kind::IndexJoin(spec) => spec
            .probes
            .iter()
            .flat_map(|probe| probe.scan.cols.iter().map(|&(_, v)| v))
            .collect(),
        Kind::Assign(v, _) => vec![*v],
        Kind::Unfold { var, second, .. } => std::iter::once(*var).chain(*second).collect(),
        Kind::Unpack { parts, .. } => parts
            .iter()
            .filter_map(|p| match p {
                crate::sparql::plan::PathEnd::Var(v) => Some(*v),
                crate::sparql::plan::PathEnd::Const(_) => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

pub(super) struct Expand {
    node: Node,
    pending: Option<Buffer>,
    at: usize,
    /// Output rows per input row in the batches so far, at least one.
    ratio: f64,
    /// A path's edge relation and its charge, once its edge plan has been read.
    edges: Option<(crate::sparql::exec::PathEdges, OwnedCharge)>,
    /// The kernel's counters, added up over the batches it ran.
    counters: Option<serde_json::Map<String, serde_json::Value>>,
    /// The distinct calls a search over its group has made, held against the cap that
    /// one eager call holds over its whole input.
    calls: Option<Calls>,
    /// A remote LATERAL's loop, once it has read input.
    remote: Option<Box<Remote>>,
    /// The note of the operator's plan node, from the batches so far.
    note: Option<String>,
}

/// A remote LATERAL across its input's batches.
struct Remote {
    run: crate::sparql::enhancer::RemoteRun,
    /// The input rows whose inputs a request has still to answer.
    held: Option<Buffer>,
    /// The charge of the answers the loop keeps for inputs that later rows repeat.
    answers: OwnedCharge,
}

/// The distinct argument values of a search over its group across all of its batches.
struct Calls {
    seen: FxHashSet<Vec<Id>>,
    /// The most distinct values the eager kernel searches.
    limit: usize,
    charge: OwnedCharge,
}

/// The bytes a set of distinct calls holds per value of `width` identifiers, with the
/// hash table's slot and the vector's header.
fn call_bytes(width: usize) -> u64 {
    (width * std::mem::size_of::<Id>() + 2 * std::mem::size_of::<Vec<Id>>()) as u64
}

impl Expand {
    pub(super) fn new(node: Node) -> Self {
        Self {
            node,
            pending: None,
            at: 0,
            ratio: 1.0,
            edges: None,
            counters: None,
            calls: None,
            remote: None,
            note: None,
        }
    }

    /// The description of the operator's plan node with its note over the batches so
    /// far, as one eager run describes it, when the kernel notes its runs.
    pub(super) fn description(&self) -> Option<String> {
        self.note
            .as_ref()
            .map(|note| format!("{} {note}", self.node.desc))
    }

    /// Count the distinct calls of a search's input batch, and refuse the batch, as the
    /// eager kernel refuses its whole input, once they pass the kernel's cap. A batch
    /// runs one search per distinct value, so without the count a stream of batches
    /// would search more distinct values than one eager call admits.
    fn count_calls(&mut self, ctx: &Arc<Ctx>, input: &Table) -> Result<()> {
        let (limit, keys): (usize, Box<dyn Fn(usize) -> Option<Vec<Id>> + '_>) =
            match &self.node.kind {
                Kind::TextSearch(spec) => {
                    let Some(col) = spec.query_var.and_then(|v| input.col_of(v)) else {
                        return Ok(());
                    };
                    (
                        exec::MAX_TEXT_QUERIES,
                        Box::new(move |r| {
                            let id = input.get(r, col);
                            (id != Id::UNDEF).then(|| vec![id])
                        }),
                    )
                }
                Kind::VectorSearch(spec) => {
                    let crate::sparql::plan::VectorQuery::Var(var) = spec.query else {
                        return Ok(());
                    };
                    let Some(col) = input.col_of(var) else {
                        return Ok(());
                    };
                    (
                        exec::MAX_QUERY_VECTORS,
                        Box::new(move |r| {
                            let id = input.get(r, col);
                            (id != Id::UNDEF).then(|| vec![id])
                        }),
                    )
                }
                Kind::HybridSearch(spec) => {
                    let key = crate::sparql::hybrid::query_key(spec, input);
                    (
                        crate::sparql::hybrid::MAX_QUERIES,
                        Box::new(move |r| key(r).map(|(t, v)| vec![t, v])),
                    )
                }
                #[cfg(feature = "geo")]
                Kind::SpatialPf(spec) => {
                    let Some(slots) = spec.deferred.as_deref() else {
                        return Ok(());
                    };
                    (
                        crate::geo::exec::MAX_ARGUMENT_BINDINGS,
                        Box::new(crate::geo::exec::argument_key(slots, input)),
                    )
                }
                _ => return Ok(()),
            };
        let calls = match &mut self.calls {
            Some(calls) => calls,
            None => self.calls.insert(Calls {
                seen: FxHashSet::default(),
                limit,
                charge: OwnedCharge::new(ctx, 0)?,
            }),
        };
        let mut width = 0;
        for r in 0..input.len {
            let Some(key) = keys(r) else { continue };
            width = key.len();
            if calls.seen.insert(key) && calls.seen.len() > calls.limit {
                return Err(crate::Error::BudgetExceeded(crate::Budget {
                    kind: crate::BudgetKind::Rows,
                    limit: calls.limit as u64,
                    requested: calls.seen.len() as u64,
                }));
            }
        }
        calls
            .charge
            .resize(calls.seen.capacity() as u64 * call_bytes(width.max(1)))
    }

    /// The kernel's counters over the batches so far, as one eager run reports them.
    pub(super) fn counters(&self) -> Option<&serde_json::Map<String, serde_json::Value>> {
        self.counters.as_ref()
    }

    /// Add one batch's counters: numbers add up, and other values keep the latest.
    fn add_counters(&mut self, batch: Option<serde_json::Map<String, serde_json::Value>>) {
        let Some(batch) = batch else { return };
        let total = self.counters.get_or_insert_with(Default::default);
        for (key, value) in batch {
            let sum = match (total.get(&key).and_then(|v| v.as_u64()), value.as_u64()) {
                (Some(a), Some(b)) => Some(a.saturating_add(b).into()),
                _ => None,
            };
            total.insert(key, sum.unwrap_or(value));
        }
    }

    pub(super) fn done(&self, child_done: bool) -> bool {
        child_done
            && self.pending.is_none()
            && self.remote.as_ref().is_none_or(|r| r.held.is_none())
    }

    /// Read one input batch into a remote LATERAL, send the requests its unanswered
    /// inputs fill, and join the rows whose inputs have answers. A request carries
    /// `bulk` inputs, so the loop holds the rows of a partial request until the input
    /// brings more inputs or ends.
    fn remote(
        &mut self,
        ctx: &Arc<Ctx>,
        child: &mut Operator,
        options: &CursorOptions,
    ) -> Result<Option<Buffer>> {
        let Kind::Lateral(spec) = &self.node.kind else {
            unreachable!("a remote loop is a LATERAL")
        };
        let rl = spec.service.as_deref().expect("a remote loop");
        let remote = match &mut self.remote {
            Some(remote) => remote,
            None => self.remote.insert(Box::new(Remote {
                run: crate::sparql::enhancer::RemoteRun::new(ctx, spec, rl)?,
                held: None,
                answers: OwnedCharge::new(ctx, 0)?,
            })),
        };
        if !child.done
            && let Some(batch) = child.next(ctx, options, options.batch_rows.max(1))?
        {
            remote.run.add(ctx, spec, rl, &batch.table);
            match &mut remote.held {
                Some(held) => {
                    super::merge::append(ctx, held, &batch.table, 0..batch.table.len)?;
                }
                None => remote.held = Some(batch),
            }
        }
        remote.run.send(ctx, spec, rl, child.done)?;
        remote.answers.resize(remote.run.held_bytes())?;
        self.counters = Some(remote.run.stats.counters());
        self.note = Some(remote.run.stats.note());
        let Some(held) = remote.held.take() else {
            return Ok(None);
        };
        let n = remote.run.answered(spec, &held.table);
        if n == 0 {
            remote.held = Some(held);
            return Ok(None);
        }
        let len = held.table.len;
        let prefix = match n == len {
            true => held,
            false => {
                let prefix = copy_rows(ctx, &held.table, 0, n)?.expect("answered rows");
                remote.held = copy_rows(ctx, &held.table, n, len - n)?;
                prefix
            }
        };
        let table = remote.run.join(ctx, spec, &prefix.table, &self.node.vars)?;
        drop(prefix);
        // The loop's kernel runs outside the eager executor, which counts its rows.
        ctx.produced(table.len())?;
        self.counters = Some(remote.run.stats.counters());
        let charge = OwnedCharge::new(ctx, super::capacity_bytes(&table))?;
        Ok(Some(Buffer { table, charge }))
    }

    pub(super) fn next(
        &mut self,
        ctx: &Arc<Ctx>,
        children: &mut [Operator],
        options: &CursorOptions,
        vars: &[VarId],
        cap: usize,
    ) -> Result<Option<Buffer>> {
        let (child, edge_plan) = match children {
            [edges, input] => (input, Some(edges)),
            [input] => (input, None),
            _ => unreachable!("an expanding operator has one input"),
        };
        if let (Some(edge_plan), Kind::Path { spec, .. }) = (edge_plan, &self.node.kind)
            && self.edges.is_none()
        {
            self.edges = Some(super::walk::read_edges(ctx, spec, edge_plan, options)?);
        }
        loop {
            ctx.check()?;
            if let Some(buffer) = &self.pending {
                if self.at == 0 && buffer.table.len <= cap {
                    return Ok(self.pending.take());
                }
                let batch = copy_rows(ctx, &buffer.table, self.at, cap)?;
                self.at += batch.as_ref().map_or(0, |b| b.table.len);
                if self.at >= buffer.table.len {
                    self.pending = None;
                    self.at = 0;
                }
                return Ok(batch);
            }
            if child.done && self.remote.as_ref().is_none_or(|r| r.held.is_none()) {
                return Ok(None);
            }
            if matches!(&self.node.kind, Kind::Lateral(spec) if spec.service.is_some()) {
                // A remote LATERAL's requests carry many inputs each, so it reads whole
                // batches and keeps what the caller did not take for the next pulls.
                if let Some(mut output) = self.remote(ctx, child, options)? {
                    output.project(vars)?;
                    if !output.table.is_empty() {
                        self.pending = Some(output);
                        self.at = 0;
                    }
                }
                continue;
            }
            // Ask for about as many input rows as fill the cap at the expansion seen
            // so far, so that the caller's demand bounds the input too.
            let want = ((cap as f64 / self.ratio) as usize).clamp(1, cap);
            let Some(input) = child.next(ctx, options, want)? else {
                return Ok(None);
            };
            let rows = input.table.len;
            self.count_calls(ctx, &input.table)?;
            let Buffer { table, charge } = input;
            let mut input = Some(crate::sparql::table::Materialized::with_owned(
                table, charge,
            )?);
            let mut output = match (&self.edges, &self.node.kind) {
                (Some((edges, _)), Kind::Path { spec, .. }) => {
                    let input = input.take().expect("one input table per batch");
                    let table = exec::path_batch(ctx, spec, edges, &input.table, &self.node.vars)?;
                    // Keep the actual input allocation paid through output admission.
                    // The path kernel's own output admission remains a separate edge.
                    let charge = OwnedCharge::new(ctx, super::capacity_bytes(&table))?;
                    Buffer { table, charge }
                }
                _ => {
                    let info = child.info.clone();
                    let (mut output, ran) =
                        exec::execute_with_inputs(ctx, &self.node, &mut |_| {
                            Ok((
                                input.take().expect("one input table per batch"),
                                info.clone(),
                            ))
                        })?;
                    self.add_counters(ran.counters);
                    if output.lease.is_none() {
                        return Err(crate::Error::invalid(
                            "expansion output lost its admitted input credit",
                        ));
                    }
                    let charge = OwnedCharge::adopt(ctx, &mut output.lease)?;
                    Buffer {
                        table: output.table,
                        charge,
                    }
                }
            };
            output.project(vars)?;
            if rows > 0 {
                self.ratio = (output.table.len as f64 / rows as f64).max(1.0);
            }
            if !output.table.is_empty() {
                self.pending = Some(output);
                self.at = 0;
            }
        }
    }
}

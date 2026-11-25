//! Native cursor operators for GeoSPARQL plan nodes (item 829, groups 4 and 6 / 8c):
//! - `SpatialScan`: window search + exact refinement + residual filters
//! - `SpatialPf` without limit and without input: streaming feature search
//! - `SpatialRelate`: asserted and derived topological relations
//! - `SpatialJoin`: streaming spatial join over 0, 1, or 2 plan inputs
//! - `SpatialKnn`: streaming k-nearest-neighbour search with cursor-executed template
//!
//! Nothing runs before the first pull, because operators are also built only to test
//! whether an EXISTS pattern streams. Every operator compiles with and without the `geo`
//! feature flag.
use super::{Buffer, CursorOptions, CursorPlan, FallbackPolicy, Operator};
use crate::error::Result;
use crate::sparql::ctx::Ctx;
use crate::sparql::plan::{Kind, Node};
use crate::sparql::table::VarId;
use std::sync::Arc;

/// Whether `node` runs as a native streaming spatial leaf (`SpatialScan`, unlimited
/// `SpatialPf` without input, or `SpatialRelate`).
pub(super) fn leaf_eligible(node: &Node) -> bool {
    cfg!(feature = "geo")
        && node.children.is_empty()
        && (matches!(&node.kind, Kind::SpatialScan(_) | Kind::SpatialRelate(_))
            || matches!(&node.kind, Kind::SpatialPf(spec) if spec.limit.is_none()))
}

/// Whether `node` runs as a native spatial join cursor (0, 1, or 2 plan inputs).
pub(super) fn join_eligible(node: &Node) -> bool {
    cfg!(feature = "geo") && matches!(&node.kind, Kind::SpatialJoin(_))
}

/// Whether `node` runs as a native k-nearest-neighbour cursor (`SpatialKnn`).
pub(super) fn knn_eligible(node: &Node) -> bool {
    cfg!(feature = "geo") && matches!(&node.kind, Kind::SpatialKnn(_)) && node.children.len() == 1
}

/// Whether a `SpatialJoin` needs to read a full build input before emitting its first
/// row (two plan children, or an unindexed index side — conservatively reported for
/// two plan children).
pub(super) fn join_full_input(node: &Node) -> bool {
    matches!(&node.kind, Kind::SpatialJoin(_)) && node.children.len() == 2
}

// ------------------------------------------------------------ SpatialLeaf --

pub(super) struct SpatialLeaf {
    #[cfg_attr(not(feature = "geo"), allow(dead_code))]
    node: Node,
    opened: bool,
    #[cfg(feature = "geo")]
    reader: Option<LeafReader>,
    pub(super) counters: Option<serde_json::Map<String, serde_json::Value>>,
}

#[cfg(feature = "geo")]
enum LeafReader {
    Scan(Box<crate::geo::cursor::SpatialScanReader>),
    Pf(Box<crate::geo::cursor::SpatialPfReader>),
    Relate(Box<crate::geo::cursor::SpatialRelateReader>),
}

impl SpatialLeaf {
    pub(super) fn new(node: Node) -> Self {
        Self {
            node,
            opened: false,
            #[cfg(feature = "geo")]
            reader: None,
            counters: None,
        }
    }

    pub(super) fn release(&mut self) {
        self.opened = true;
        #[cfg(feature = "geo")]
        {
            self.reader = None;
        }
    }

    pub(super) fn done(&self) -> bool {
        #[cfg(feature = "geo")]
        {
            self.opened && self.reader.is_none()
        }
        #[cfg(not(feature = "geo"))]
        {
            self.opened
        }
    }

    #[cfg(feature = "geo")]
    pub(super) fn next(&mut self, ctx: &Arc<Ctx>, cap: usize) -> Result<Option<Buffer>> {
        let r = self.pull(ctx, cap);
        if r.is_err() {
            self.release();
        }
        r
    }

    #[cfg(feature = "geo")]
    fn pull(&mut self, ctx: &Arc<Ctx>, cap: usize) -> Result<Option<Buffer>> {
        if !self.opened {
            self.opened = true;
            let reader = match &self.node.kind {
                Kind::SpatialScan(spec) => LeafReader::Scan(Box::new(
                    crate::geo::cursor::SpatialScanReader::open(ctx, spec, &self.node.vars)?,
                )),
                Kind::SpatialPf(spec) => LeafReader::Pf(Box::new(
                    crate::geo::cursor::SpatialPfReader::open(ctx, spec, &self.node.vars)?,
                )),
                Kind::SpatialRelate(spec) => LeafReader::Relate(Box::new(
                    crate::geo::cursor::SpatialRelateReader::open(ctx, spec, &self.node.vars)?,
                )),
                _ => unreachable!("a spatial leaf node"),
            };
            self.reader = Some(reader);
        }
        let Some(reader) = &mut self.reader else {
            return Ok(None);
        };
        let want = cap.max(1);
        let mut b = Buffer::new(ctx, &self.node.vars, want.min(64))?;
        let ended = match reader {
            LeafReader::Scan(r) => {
                let ended = r.next(ctx, &mut b.table, want)?;
                self.counters = Some(r.counters());
                ended
            }
            LeafReader::Pf(r) => {
                let ended = r.next(ctx, &mut b.table, want)?;
                self.counters = Some(r.counters());
                ended
            }
            LeafReader::Relate(r) => {
                let ended = r.next(ctx, &mut b.table, want)?;
                self.counters = Some(r.counters());
                ended
            }
        };
        if ended {
            self.reader = None;
        }
        b.reconcile()?;
        Ok((b.table.len > 0).then_some(b))
    }

    #[cfg(not(feature = "geo"))]
    pub(super) fn next(&mut self, _: &Arc<Ctx>, _: usize) -> Result<Option<Buffer>> {
        unreachable!("spatial leaf requires geo feature")
    }
}

// ------------------------------------------------------------ SpatialJoin --

pub(super) struct SpatialJoin {
    #[cfg_attr(not(feature = "geo"), allow(dead_code))]
    node: Node,
    opened: bool,
    #[cfg(feature = "geo")]
    engine: Option<crate::geo::cursor::SpatialJoinEngine>,
    pub(super) counters: Option<serde_json::Map<String, serde_json::Value>>,
}

impl SpatialJoin {
    pub(super) fn new(node: Node) -> Self {
        Self {
            node,
            opened: false,
            #[cfg(feature = "geo")]
            engine: None,
            counters: None,
        }
    }

    pub(super) fn release(&mut self) {
        self.opened = true;
        #[cfg(feature = "geo")]
        {
            self.engine = None;
        }
    }

    pub(super) fn done(&self) -> bool {
        #[cfg(feature = "geo")]
        {
            self.opened && self.engine.is_none()
        }
        #[cfg(not(feature = "geo"))]
        {
            self.opened
        }
    }

    #[cfg(feature = "geo")]
    pub(super) fn next(
        &mut self,
        ctx: &Arc<Ctx>,
        children: &mut [Operator],
        options: &CursorOptions,
        vars: &[VarId],
        cap: usize,
    ) -> Result<Option<Buffer>> {
        let r = self.pull(ctx, children, options, vars, cap);
        if r.is_err() {
            self.release();
        }
        r
    }

    #[cfg(feature = "geo")]
    fn pull(
        &mut self,
        ctx: &Arc<Ctx>,
        children: &mut [Operator],
        options: &CursorOptions,
        vars: &[VarId],
        cap: usize,
    ) -> Result<Option<Buffer>> {
        let Kind::SpatialJoin(spec) = &self.node.kind else {
            unreachable!("a spatial join node")
        };
        if !self.opened {
            self.opened = true;
            let engine = match children.len() {
                0 => crate::geo::cursor::SpatialJoinEngine::open_no_children(
                    ctx,
                    spec,
                    &self.node.vars,
                )?,
                1 => crate::geo::cursor::SpatialJoinEngine::open_one_child(
                    ctx,
                    spec,
                    &self.node.vars,
                    &children[0].vars,
                )?,
                2 => {
                    let build_is_left = self.node.children[0].est <= self.node.children[1].est;
                    let left_vars = children[0].vars.clone();
                    let right_vars = children[1].vars.clone();
                    let engine = crate::geo::cursor::SpatialJoinEngine::open_two_children(
                        ctx,
                        spec,
                        &self.node.vars,
                        &left_vars,
                        &right_vars,
                        build_is_left,
                        &mut |child_idx, sink| {
                            while let Some(batch) =
                                children[child_idx].next(ctx, options, options.batch_rows)?
                            {
                                sink(&batch.table)?;
                            }
                            Ok(())
                        },
                    )?;
                    if let Some(probe_idx) = engine.empty_build_probe_child() {
                        children[probe_idx].mark_skipped("build side has no geometries");
                    }
                    engine
                }
                _ => unreachable!("spatial join has at most two children"),
            };
            self.counters = Some(engine.counters());
            self.engine = Some(engine);
        }
        let Some(engine) = &mut self.engine else {
            return Ok(None);
        };
        let want = cap.max(1);
        let mut b = Buffer::new(ctx, &self.node.vars, want.min(64))?;
        let ended = engine.next(ctx, &mut b.table, want, &mut |child_idx| {
            Ok(children[child_idx]
                .next(ctx, options, options.batch_rows)?
                .map(|buf| buf.table))
        })?;
        self.counters = Some(engine.counters());
        if ended {
            self.engine = None;
        }
        b.reconcile()?;
        if b.table.vars != vars {
            b.project(vars)?;
        }
        Ok((b.table.len > 0).then_some(b))
    }

    #[cfg(not(feature = "geo"))]
    pub(super) fn next(
        &mut self,
        _: &Arc<Ctx>,
        _: &mut [Operator],
        _: &CursorOptions,
        _: &[VarId],
        _: usize,
    ) -> Result<Option<Buffer>> {
        unreachable!("spatial join requires geo feature")
    }
}

// ------------------------------------------------------------- SpatialKnn --

#[cfg(feature = "geo")]
#[derive(Clone, Copy, PartialEq)]
struct DistKey(f64);

#[cfg(feature = "geo")]
impl Eq for DistKey {}
#[cfg(feature = "geo")]
impl PartialOrd for DistKey {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
#[cfg(feature = "geo")]
impl Ord for DistKey {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0.total_cmp(&other.0)
    }
}

/// Native streaming k-nearest-neighbour cursor (`SpatialKnn`, item 829, group 6 / 8c).
///
/// Streams candidate batches in increasing lower-bound distance through cursor-executed
/// template operators, forwarding each batch straight to the `TopK` above it and stopping
/// as soon as `k` rows are proven nearer than the next lower bound.
pub(super) struct SpatialKnn {
    #[cfg_attr(not(feature = "geo"), allow(dead_code))]
    node: Node,
    #[cfg_attr(not(feature = "geo"), allow(dead_code))]
    template: Node,
    #[cfg_attr(not(feature = "geo"), allow(dead_code))]
    fallback: FallbackPolicy,
    opened: bool,
    finished: bool,
    #[cfg(feature = "geo")]
    state: Option<Box<KnnState>>,
    pub(super) counters: Option<serde_json::Map<String, serde_json::Value>>,
    /// Merged template plan info from executed batches, for EXPLAIN.
    pub(super) template_plan: Option<Box<CursorPlan>>,
}

#[cfg(feature = "geo")]
struct KnnState {
    subj: Option<crate::id::Id>,
    state_name: String,
    error_rows: Vec<[crate::id::Id; 3]>,
    error_at: usize,
    errors_found: usize,
    error_rows_total: u64,
    taken: rustc_hash::FxHashSet<crate::id::Id>,
    nearest: Option<crate::geo::search::NearestIter>,
    nearest_opened: bool,
    pending_cands: Vec<[crate::id::Id; 3]>,
    current_bound: f64,
    best: std::collections::BinaryHeap<DistKey>,
    active_op: Option<Box<Operator>>,
    active_phase_error: bool,
    active_rows: usize,
    batches: u64,
    empty_batches: u64,
    keyed: u64,
    matched: u64,
    runs: Vec<CursorPlan>,
    charge: crate::sparql::ctx::OwnedCharge,
}

#[cfg(feature = "geo")]
impl KnnState {
    fn retained_bytes_without_nearest(&self) -> u64 {
        let errs = self.error_rows.capacity() as u64 * size_of::<[crate::id::Id; 3]>() as u64;
        let taken = self.taken.capacity() as u64 * (size_of::<crate::id::Id>() + 1) as u64;
        let cands = self.pending_cands.capacity() as u64 * size_of::<[crate::id::Id; 3]>() as u64;
        let best = self.best.capacity() as u64 * size_of::<DistKey>() as u64;
        errs.saturating_add(taken)
            .saturating_add(cands)
            .saturating_add(best)
    }

    fn retained_bytes(&self) -> u64 {
        let near = self
            .nearest
            .as_ref()
            .map_or(0, crate::geo::search::NearestIter::retained_bytes);
        self.retained_bytes_without_nearest().saturating_add(near)
    }

    fn update_charge(&mut self) -> Result<()> {
        let bytes = self.retained_bytes();
        self.charge.resize(bytes)
    }

    fn gather_nearest_batch(&mut self, ctx: &Ctx, batch_target: usize) -> Result<f64> {
        use crate::id::Id;
        self.pending_cands.clear();
        let mut bound = f64::INFINITY;
        while self.pending_cands.len() < batch_target {
            let base_bytes = self.retained_bytes_without_nearest();
            let Some(iter) = &mut self.nearest else {
                break;
            };
            let mut chunk = Vec::new();
            let charge = &mut self.charge;
            match iter.next_chunk(ctx, &mut chunk, &mut |b| {
                charge.resize(base_bytes.saturating_add(b))
            })? {
                Some(b) => {
                    bound = b;
                    let keep = |h: &&crate::geo::search::Hit| {
                        self.subj.is_none_or(|s| s == h.s) && !self.taken.contains(&h.o)
                    };
                    let kept = chunk.iter().filter(keep).count();
                    let elem = size_of::<[Id; 3]>();
                    let capacity = self.pending_cands.capacity();
                    if self.pending_cands.len().saturating_add(kept) > capacity {
                        let next = sparkles_budget::sizes::vec_grown(
                            capacity,
                            self.pending_cands.len(),
                            kept,
                            elem,
                        );
                        let chunk_bytes =
                            chunk.capacity() as u64 * size_of::<crate::geo::search::Hit>() as u64;
                        self.charge.resize(
                            self.retained_bytes()
                                .saturating_add(chunk_bytes)
                                .saturating_add(next as u64 * elem as u64),
                        )?;
                        self.pending_cands.reserve(kept);
                    }
                    self.pending_cands
                        .extend(chunk.iter().filter(keep).map(|h| [h.s, h.o, h.g]));
                    drop(chunk);
                    self.update_charge()?;
                    if !bound.is_finite() {
                        break;
                    }
                }
                None => {
                    bound = f64::INFINITY;
                    break;
                }
            }
        }
        self.update_charge()?;
        Ok(bound)
    }
}

impl SpatialKnn {
    pub(super) fn new(node: Node, template: Node, fallback: FallbackPolicy) -> Self {
        Self {
            node,
            template,
            fallback,
            opened: false,
            finished: false,
            #[cfg(feature = "geo")]
            state: None,
            counters: None,
            template_plan: None,
        }
    }

    pub(super) fn template(&self) -> &Node {
        &self.template
    }

    pub(super) fn release(&mut self) {
        self.opened = true;
        self.finished = true;
        #[cfg(feature = "geo")]
        {
            if let Some(st) = &mut self.state
                && let Some(op) = &mut st.active_op
            {
                op.release_finished();
            }
            self.state = None;
        }
    }

    pub(super) fn done(&self) -> bool {
        self.finished
    }

    #[cfg(feature = "geo")]
    pub(super) fn next(
        &mut self,
        ctx: &Arc<Ctx>,
        options: &CursorOptions,
        vars: &[VarId],
        cap: usize,
    ) -> Result<Option<Buffer>> {
        let r = self.pull(ctx, options, vars, cap);
        if r.is_err() {
            self.release();
        }
        r
    }

    #[cfg(feature = "geo")]
    fn pull(
        &mut self,
        ctx: &Arc<Ctx>,
        options: &CursorOptions,
        vars: &[VarId],
        cap: usize,
    ) -> Result<Option<Buffer>> {
        use crate::error::Error;
        use crate::geo::exec::state;
        use crate::geo::knn::rows_without_distance_charged;
        use crate::geo::search::NearestIter;
        use crate::sparql::ctx::OwnedCharge;
        use crate::sparql::expr::{Row as ExprRow, eval};
        use crate::sparql::geopf::{ScanShape, number};
        use crate::sparql::plan::PathEnd;

        const RUNS_SHOWN: usize = 4;
        const ERROR_BATCH: usize = 4096;

        let Kind::SpatialKnn(spec) = &self.node.kind else {
            unreachable!("a spatial knn node")
        };
        if self.finished {
            return Ok(None);
        }
        if !self.opened {
            self.opened = true;
            let shape = ScanShape::of(&spec.scan).ok_or_else(|| {
                Error::invalid("nearest-neighbour search of an unexpected pattern")
            })?;
            let subj = match shape.subj {
                Some(PathEnd::Const(s)) => Some(s),
                _ => None,
            };
            let mut charge = OwnedCharge::new(ctx, 0)?;
            let (error_rows, taken) = if spec.errors {
                rows_without_distance_charged(ctx, spec, subj, &mut |b| charge.resize(b))?
            } else {
                (Vec::new(), rustc_hash::FxHashSet::default())
            };
            let error_rows_total = error_rows.len() as u64;
            let mut st = Box::new(KnnState {
                subj,
                state_name: state(ctx).to_string(),
                error_rows,
                error_at: 0,
                errors_found: 0,
                error_rows_total,
                taken,
                nearest: None,
                nearest_opened: false,
                pending_cands: Vec::new(),
                current_bound: f64::INFINITY,
                best: std::collections::BinaryHeap::new(),
                active_op: None,
                active_phase_error: false,
                active_rows: 0,
                batches: 0,
                empty_batches: 0,
                keyed: 0,
                matched: 0,
                runs: Vec::new(),
                charge,
            });
            st.update_charge()?;
            self.state = Some(st);
        }

        let Some(st) = &mut self.state else {
            self.finished = true;
            return Ok(None);
        };
        let batch_target = spec.k.saturating_mul(2).max(64);

        loop {
            ctx.check()?;
            // 1. Drain currently active substituted template operator.
            if let Some(op) = &mut st.active_op {
                if let Some(mut batch) = op.next(ctx, options, cap)? {
                    st.active_rows += batch.table.len;
                    // Evaluate ordering key to track errors / k-best bound.
                    let map = batch.table.var_map(ctx.nvars());
                    let need = spec.k.saturating_sub(st.errors_found);
                    for i in 0..batch.table.len {
                        let row = ExprRow {
                            table: &batch.table,
                            i,
                            map: &map,
                            dec: None,
                            exists: None,
                        };
                        let key = eval(&spec.order, &row, ctx)
                            .ok()
                            .and_then(|v| v.value(ctx).ok())
                            .as_ref()
                            .and_then(number);
                        st.keyed += 1;
                        if st.active_phase_error {
                            if key.is_none() {
                                st.errors_found += 1;
                            }
                        } else if let Some(d) = key
                            && need > 0
                        {
                            if st.best.len() < need {
                                if st.best.len() == st.best.capacity() {
                                    let next_best = st.best.capacity().saturating_mul(2).max(8);
                                    let add = next_best.saturating_sub(st.best.capacity()) as u64
                                        * size_of::<DistKey>() as u64;
                                    let projected = st.retained_bytes().saturating_add(add);
                                    st.charge.resize(projected)?;
                                }
                                st.best.push(DistKey(d));
                            } else if st.best.peek().is_some_and(|m| d < m.0) {
                                st.best.pop();
                                st.best.push(DistKey(d));
                            }
                        }
                    }
                    st.update_charge()?;
                    if batch.table.vars != vars {
                        batch.project(vars)?;
                    }
                    st.matched += batch.table.len as u64;
                    Self::record_counters(st, &mut self.counters);
                    return Ok(Some(batch));
                }
                // Active template finished.
                let mut done_op = st.active_op.take().expect("active op");
                st.batches += 1;
                if st.active_rows == 0 {
                    st.empty_batches += 1;
                }
                if st.runs.len() < RUNS_SHOWN {
                    let mut cp = done_op.describe(ctx);
                    done_op.update_plan(&mut cp);
                    st.runs.push(cp);
                }
                done_op.release_finished();
                // Check early-stop condition after a nearest-phase batch.
                if !st.active_phase_error {
                    let need = spec.k.saturating_sub(st.errors_found);
                    let proven = st.best.len() >= need
                        && st
                            .best
                            .peek()
                            .is_some_and(|m| m.0 < st.current_bound / spec.metres_per_unit);
                    if proven {
                        Self::finalize(
                            ctx,
                            &self.template,
                            st,
                            &mut self.counters,
                            &mut self.template_plan,
                        );
                        self.state = None;
                        self.finished = true;
                        return Ok(None);
                    }
                }
                continue;
            }

            // 2. Start next error-rows batch if any remain.
            if st.error_at < st.error_rows.len() {
                let end = (st.error_at + ERROR_BATCH).min(st.error_rows.len());
                let slice = &st.error_rows[st.error_at..end];
                st.error_at = end;
                let t = build_template_batch(&self.template, spec, slice)?;
                let op = Operator::build(ctx, t, self.fallback.at_pull(ctx))?;
                st.active_op = Some(Box::new(op));
                st.active_phase_error = true;
                st.active_rows = 0;
                continue;
            }
            if !st.error_rows.is_empty() {
                st.error_rows = Vec::new();
                st.update_charge()?;
            }

            // 3. Nearest-first phase.
            let need = spec.k.saturating_sub(st.errors_found);
            if need == 0 {
                Self::finalize(
                    ctx,
                    &self.template,
                    st,
                    &mut self.counters,
                    &mut self.template_plan,
                );
                self.state = None;
                self.finished = true;
                return Ok(None);
            }
            if !st.nearest_opened {
                st.nearest_opened = true;
                let shape = ScanShape::of(&spec.scan).ok_or_else(|| {
                    Error::invalid("nearest-neighbour search of an unexpected pattern")
                })?;
                let graph = shape.graph_filter(&spec.scan);
                let base_bytes = st.retained_bytes();
                let charge = &mut st.charge;
                let iter = NearestIter::open(
                    ctx,
                    vec![spec.pred],
                    &spec.q,
                    graph,
                    spec.scan.dedup,
                    &mut |b| charge.resize(base_bytes.saturating_add(b)),
                )?;
                st.nearest = Some(iter);
                st.update_charge()?;
            }
            // Gather next batch of candidates from NearestIter.
            let bound = st.gather_nearest_batch(ctx, batch_target)?;
            if st.pending_cands.is_empty() {
                Self::finalize(
                    ctx,
                    &self.template,
                    st,
                    &mut self.counters,
                    &mut self.template_plan,
                );
                self.state = None;
                self.finished = true;
                return Ok(None);
            }
            let t = build_template_batch(&self.template, spec, &st.pending_cands)?;
            st.pending_cands.clear();
            st.current_bound = bound;
            let op = Operator::build(ctx, t, self.fallback.at_pull(ctx))?;
            st.active_op = Some(Box::new(op));
            st.active_phase_error = false;
            st.active_rows = 0;
        }
    }

    #[cfg(feature = "geo")]
    fn record_counters(
        st: &KnnState,
        out: &mut Option<serde_json::Map<String, serde_json::Value>>,
    ) {
        let mut c = crate::geo::exec::Counters::new();
        let near = st.nearest.as_ref().map(|n| &n.st);
        c.insert("candidates".into(), near.map_or(0, |s| s.candidates).into());
        c.insert("refined".into(), st.keyed.into());
        c.insert("matched".into(), st.matched.into());
        c.insert(
            "treeNodesVisited".into(),
            near.map_or(0, |s| s.nodes).into(),
        );
        c.insert("rechecked".into(), near.map_or(0, |s| s.rechecked).into());
        c.insert("batches".into(), st.batches.into());
        c.insert("emptyBatches".into(), st.empty_batches.into());
        c.insert("errorRows".into(), st.error_rows_total.into());
        c.insert("index".into(), st.state_name.clone().into());
        c.insert("fallback".into(), near.is_some_and(|s| s.fallback).into());
        *out = Some(c);
    }

    #[cfg(feature = "geo")]
    fn finalize(
        ctx: &Ctx,
        template: &Node,
        st: &mut KnnState,
        counters: &mut Option<serde_json::Map<String, serde_json::Value>>,
        template_plan: &mut Option<Box<CursorPlan>>,
    ) {
        fn plan_info_to_cursor(mut info: crate::sparql::PlanInfo) -> CursorPlan {
            let children = std::mem::take(&mut info.children)
                .into_iter()
                .map(plan_info_to_cursor)
                .collect();
            CursorPlan {
                operator: info,
                materializes: false,
                full_input_before_output: false,
                growing_state: false,
                complete: true,
                reason: None,
                children,
            }
        }
        fn merge_cursor_run(a: CursorPlan, mut b: CursorPlan) -> CursorPlan {
            b.operator = a.operator.merge_run(b.operator);
            b.materializes |= a.materializes;
            b.full_input_before_output |= a.full_input_before_output;
            b.growing_state |= a.growing_state;
            b.complete &= a.complete;
            if b.reason.is_none() {
                b.reason = a.reason;
            }
            if a.children.len() == b.children.len()
                && a.children
                    .iter()
                    .zip(&b.children)
                    .all(|(x, y)| x.operator.operator == y.operator.operator)
            {
                let b_children = std::mem::take(&mut b.children);
                b.children = a
                    .children
                    .into_iter()
                    .zip(b_children)
                    .map(|(x, y)| {
                        let xs = x
                            .operator
                            .fidelity
                            .as_ref()
                            .and_then(|f| f.skipped.as_ref());
                        let ys = y
                            .operator
                            .fidelity
                            .as_ref()
                            .and_then(|f| f.skipped.as_ref());
                        match (xs, ys) {
                            (None, Some(_)) => x,
                            (Some(_), _) => y,
                            (None, None) => merge_cursor_run(x, y),
                        }
                    })
                    .collect();
            }
            b
        }
        Self::record_counters(st, counters);
        let merged = std::mem::take(&mut st.runs)
            .into_iter()
            .reduce(merge_cursor_run)
            .unwrap_or_else(|| {
                let mut d = crate::sparql::exec::describe(ctx, template);
                if ctx.graphs.is_some() {
                    d.redact();
                }
                plan_info_to_cursor(d.skip("no candidate needed it"))
            });
        *template_plan = Some(Box::new(merged));
    }

    #[cfg(not(feature = "geo"))]
    pub(super) fn next(
        &mut self,
        _: &Arc<Ctx>,
        _: &CursorOptions,
        _: &[VarId],
        _: usize,
    ) -> Result<Option<Buffer>> {
        unreachable!("spatial knn requires geo feature")
    }
}

#[cfg(feature = "geo")]
fn build_template_batch(
    template: &Node,
    spec: &crate::sparql::geojoin::SpatialKnnSpec,
    rows: &[[crate::id::Id; 3]],
) -> Result<Node> {
    let mut t = template.clone();
    let mut leaf = &mut t;
    for &i in &spec.placeholder {
        leaf = leaf.children.get_mut(i).ok_or_else(|| {
            crate::error::Error::invalid("nearest-neighbour template without its input")
        })?;
    }
    let cols: Vec<usize> = leaf
        .vars
        .iter()
        .map(|v| {
            if Some(*v) == spec.subj_var {
                0
            } else if *v == spec.geom_var {
                1
            } else {
                2
            }
        })
        .collect();
    let mut batch = crate::sparql::table::Table::new(leaf.vars.clone());
    let mut row = vec![crate::id::Id::UNDEF; cols.len()];
    for r in rows {
        for (x, &c) in row.iter_mut().zip(&cols) {
            *x = r[c];
        }
        batch.push_row(&row);
    }
    leaf.est = rows.len() as f64;
    leaf.kind = Kind::Values(batch);
    Ok(t)
}

#[cfg(all(test, feature = "geo"))]
mod tests {
    use super::*;
    use crate::error::{BudgetKind, Error};
    use crate::geo::index::Row;
    use crate::geo::search::{Hit, NearestIter};
    use crate::id::Id;
    use crate::sparql::ctx::OwnedCharge;
    use crate::store::{Store, StoreOptions};

    #[test]
    fn r3_knn_candidate_transfer_reserves_admitted_capacity_across_two_chunks() {
        let store = Store::in_memory(StoreOptions::default());
        let entry = crate::geo::wgs84::point(0.0, 1.0).unwrap();
        let make_hits = || {
            (0..48u64)
                .map(|i| {
                    Hit::new(
                        &Row {
                            s: i + 1,
                            p: 1,
                            o: i + 100,
                            g: 0,
                        },
                        entry.clone(),
                    )
                })
                .collect::<Vec<_>>()
        };
        let make_state = |ctx: &Arc<Ctx>| -> KnnState {
            let nearest = NearestIter::fallback_for_test(ctx.snap.clone(), make_hits(), 16);
            let mut charge = OwnedCharge::new(ctx, 0).unwrap();
            charge.resize(nearest.retained_bytes()).unwrap();
            KnnState {
                subj: None,
                state_name: String::new(),
                error_rows: Vec::new(),
                error_at: 0,
                errors_found: 0,
                error_rows_total: 0,
                taken: rustc_hash::FxHashSet::default(),
                nearest: Some(nearest),
                nearest_opened: true,
                pending_cands: Vec::new(),
                current_bound: f64::INFINITY,
                best: std::collections::BinaryHeap::new(),
                active_op: None,
                active_phase_error: false,
                active_rows: 0,
                batches: 0,
                empty_batches: 0,
                keyed: 0,
                matched: 0,
                runs: Vec::new(),
                charge,
            }
        };

        let elem = size_of::<[Id; 3]>() as u64;
        let hit_bytes = size_of::<Hit>() as u64;
        let probe_ctx = Arc::new(Ctx::new(store.snapshot()));
        let nearest_bytes = NearestIter::fallback_for_test(probe_ctx.snap.clone(), make_hits(), 16)
            .retained_bytes();
        let second_chunk_peak = nearest_bytes + 32 * hit_bytes + (16 + 48) * elem;

        // Exact peak budget succeeds and keeps capacity at the admitted 48 slots (never doubling to 64).
        let mut ctx_ok = Ctx::new(store.snapshot());
        ctx_ok.mem_limit = second_chunk_peak;
        let ctx_ok = Arc::new(ctx_ok);
        let mut st = make_state(&ctx_ok);
        let bound = st.gather_nearest_batch(&ctx_ok, 48).unwrap();
        assert!(!bound.is_finite());
        assert_eq!(st.pending_cands.len(), 48);
        assert_eq!(st.pending_cands.capacity(), 48);
        drop(st);
        assert_eq!(ctx_ok.live_bytes(), 0);

        // One byte below the second-chunk peak refuses before growing `pending_cands` past 16.
        let mut ctx_tight = Ctx::new(store.snapshot());
        ctx_tight.mem_limit = second_chunk_peak - 1;
        let ctx_tight = Arc::new(ctx_tight);
        let mut st = make_state(&ctx_tight);
        let err = st.gather_nearest_batch(&ctx_tight, 48).unwrap_err();
        assert!(matches!(
            err,
            Error::BudgetExceeded(b) if b.kind == BudgetKind::Memory && b.requested == second_chunk_peak
        ));
        assert_eq!(st.pending_cands.len(), 16);
        assert_eq!(st.pending_cands.capacity(), 16);
        drop(st);
        assert_eq!(ctx_tight.live_bytes(), 0);
    }
}

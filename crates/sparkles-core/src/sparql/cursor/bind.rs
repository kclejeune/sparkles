//! Query-owned exact-ID reuse for pure, repeated single-input BIND expressions.
//! Dense inline integers retain their cheap admission. Other inputs need observed
//! repetition in a bounded pilot. Optional cache entries never weaken query budgets.
use crate::error::Result;
use crate::id::{Id, PAYLOAD_MASK, Tag};
use crate::index::{O, P, Perm, S};
use crate::sparql::ctx::RetainedCharge;
use crate::sparql::expr::Expr;
use crate::sparql::plan::{Kind, Node};
use crate::sparql::table::VarId;
use crate::sparql::{Ctx, Table};
use rustc_hash::{FxHashMap, FxHashSet};

pub(super) struct Admission {
    input: VarId,
    expected_reuse: bool,
}

/// Entries of a general map, and of the sampled-key sketch times [`SAMPLE`].
const MAP_ENTRIES: usize = 1 << 16;
/// Entries of a map admitted by dense inline integers.
const INTEGER_ENTRIES: usize = 1 << 14;
/// One key in `1 << SAMPLE_SHIFT` is observed by the sketch, chosen by its hash, so
/// the sketch sees every occurrence of the keys it samples.
const SAMPLE_SHIFT: u32 = 4;
/// Most keys the sketch holds: it covers as many distinct keys as a full map.
const SKETCH_KEYS: usize = MAP_ENTRIES >> SAMPLE_SHIFT;
/// Sampled rows per admission decision, about 4,096 input rows.
const WINDOW: u32 = 256;
/// Rows per decision whether an admitted map still pays. The first decision waits
/// until the map has seen [`MAP_ENTRIES`] rows or filled, since a map admitted late in a
/// cyclic input misses until one period has passed.
const MAP_WINDOW: u32 = 16384;
/// Charged bytes per map entry and per sketch key, beside a fixed 512.
const ENTRY_BYTES: u64 = 96;
const SKETCH_BYTES: u64 = 32;
/// Charged bytes per row of a batch the map answers, while it runs.
const SCRATCH_BYTES: u64 = 28 + 40;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    /// Waiting for the first batch of at least [`exprcache::MIN_ROWS`] rows.
    Pilot,
    /// Observing sampled keys across batches.
    Probe,
    /// Answering from the map, evaluating misses once per distinct key.
    Map,
    /// Ordinary evaluation for the rest of the query.
    Off,
}

/// Admission needs one repeated row in `ADMIT`, and an admitted map is released once
/// it answers fewer than one row in `2 * ADMIT` without an evaluation.
const ADMIT: u32 = 8;
/// The tag bits of a map entry whose value the current batch has not yet evaluated.
/// No term carries this tag, so a pending entry cannot be mistaken for a value.
const PENDING: u64 = !PAYLOAD_MASK;

pub(super) struct Reuse {
    input: VarId,
    mode: Mode,
    values: FxHashMap<Id, Id>,
    charge: Option<RetainedCharge>,
    expected_reuse: bool,
    max_entries: usize,
    sketch: FxHashSet<Id>,
    /// The sketch filled without admitting the map. The next batch declines, so the
    /// exact count of the batch that filled it can still admit through [`Reuse::counted`].
    exhausted: bool,
    /// Sampled rows, and those whose key the sketch held, in the current window.
    window: (u32, u32),
    /// Rows, and those answered without an evaluation, since the map's last check.
    map_window: (u32, u32),
    /// Rows the map has seen since it was admitted.
    map_rows: u64,
    hits: u64,
    misses: u64,
    #[cfg(test)]
    evaluations: usize,
}
impl Reuse {
    pub(super) fn prepare(ctx: &Ctx, expr: &Expr, source: Option<&Node>) -> Option<Admission> {
        if !ctx.opt.expr_cache {
            return None;
        }
        let input = crate::sparql::exprcache::eligible(&[expr]).ok().flatten()?;
        Some(Admission {
            input,
            expected_reuse: source.is_some_and(|source| {
                source.est.is_finite()
                    && source.est > 0.0
                    && distinct_bound(ctx, source, input, &mut 64)
                        .is_some_and(|distinct| distinct as f64 <= source.est * 0.8)
            }),
        })
    }
    pub(super) fn new(ctx: &Ctx, admission: Admission) -> Option<Self> {
        // Compute the hint before building children, but retain optional memory
        // only afterward. Optional caching must not starve child construction.
        let charge = ctx.retained_charge(512).ok()?;
        Some(Self {
            input: admission.input,
            mode: Mode::Pilot,
            values: Default::default(),
            charge,
            expected_reuse: admission.expected_reuse,
            max_entries: INTEGER_ENTRIES,
            sketch: Default::default(),
            exhausted: false,
            window: (0, 0),
            map_window: (0, 0),
            map_rows: 0,
            hits: 0,
            misses: 0,
            #[cfg(test)]
            evaluations: 0,
        })
    }

    /// The exact count of a batch that the per-batch distinct pass made after
    /// [`Reuse::apply`] declined it. Repetition within one batch is exact where the
    /// sampled sketch can miss a few hot keys, so it admits the map for later batches.
    pub(super) fn counted(&mut self, rows: usize, distinct: usize) {
        if self.mode == Mode::Probe
            && rows >= crate::sparql::exprcache::MIN_ROWS
            && (rows - distinct) * ADMIT as usize >= rows
        {
            self.admit();
        }
    }

    /// Rows answered from the map and evaluations made for it, for EXPLAIN.
    pub(super) fn counters(&self) -> Option<(u64, u64)> {
        (self.hits + self.misses > 0).then_some((self.hits, self.misses))
    }

    pub(super) fn apply(
        &mut self,
        ctx: &Ctx,
        table: &mut Table,
        target: VarId,
        expr: &Expr,
    ) -> Result<bool> {
        let Some(column) = table.col_of(self.input) else {
            return Ok(false);
        };
        match self.mode {
            Mode::Off => return Ok(false),
            Mode::Map => {}
            Mode::Probe if self.exhausted => {
                self.decline();
                return Ok(false);
            }
            Mode::Pilot | Mode::Probe => {
                if table.len < crate::sparql::exprcache::MIN_ROWS {
                    // Small batches only feed the sketch of a probing map.
                    if self.mode == Mode::Pilot || !self.probe(ctx, &table.cols[column])? {
                        return Ok(false);
                    }
                } else if let Some(dense) = dense_integers(ctx, &table.cols[column])? {
                    if !dense {
                        return Ok(false);
                    }
                    self.max_entries = INTEGER_ENTRIES;
                    self.admit();
                } else if self.mode == Mode::Pilot {
                    if !self.pilot(ctx, &table.cols[column])? {
                        return Ok(false);
                    }
                } else if !self.probe(ctx, &table.cols[column])? {
                    return Ok(false);
                }
            }
        }
        self.apply_map(ctx, table, column, target, expr)
    }

    fn admit(&mut self) {
        self.mode = Mode::Map;
        self.sketch = Default::default();
        self.exhausted = false;
        self.map_window = (0, 0);
        self.map_rows = 0;
    }

    fn decline(&mut self) {
        self.mode = Mode::Off;
        self.sketch = Default::default();
        self.values = Default::default();
        if let Some(charge) = &mut self.charge {
            // A shrink only releases.
            let _ = charge.resize(512);
        }
    }

    /// The first batch: substantial exact repetition, or a repeated key with the
    /// snapshot hint, admits the map at once. Otherwise the sampled keys of the batch
    /// seed the sketch.
    fn pilot(&mut self, ctx: &Ctx, keys: &[Id]) -> Result<bool> {
        // Cheap, mostly distinct inputs do not repay per-row map work.
        // The estimate affects profit only, never physical budget admission.
        let sample = keys.len().min(4096);
        let Ok(_pilot) = ctx.retained_charge(512 + sample as u64 * 32) else {
            self.decline();
            return Ok(false);
        };
        let mut seen = FxHashSet::with_capacity_and_hasher(sample, Default::default());
        let mut repeats = 0;
        for (row, id) in keys.iter().take(sample).enumerate() {
            if row.is_multiple_of(1024) {
                ctx.check()?;
            }
            if !seen.insert(*id) {
                repeats += 1;
            }
        }
        if repeats > 0 && (repeats * ADMIT as usize >= sample || self.expected_reuse) {
            self.max_entries = MAP_ENTRIES;
            self.admit();
            return Ok(true);
        }
        self.mode = Mode::Probe;
        self.max_entries = MAP_ENTRIES;
        drop(seen);
        self.observe(ctx, &keys[sample..])?;
        // The pilot's own rows enter the sketch without a window: their repetition was
        // counted exactly above and did not admit.
        let window = self.window;
        self.observe(ctx, &keys[..sample])?;
        self.window = window;
        Ok(false)
    }

    /// Later batches: admit once a window of sampled rows repeats one time in
    /// [`ADMIT`]. A sketch that fills without that marks itself exhausted, and the next
    /// batch declines.
    fn probe(&mut self, ctx: &Ctx, keys: &[Id]) -> Result<bool> {
        self.observe(ctx, keys)?;
        let (rows, repeats) = self.window;
        if rows < WINDOW {
            return Ok(false);
        }
        self.window = (0, 0);
        if repeats * ADMIT >= rows {
            self.admit();
            return Ok(true);
        }
        if self.sketch.len() >= SKETCH_KEYS {
            self.exhausted = true;
        }
        Ok(false)
    }

    fn observe(&mut self, ctx: &Ctx, keys: &[Id]) -> Result<()> {
        let (mut rows, mut repeats) = self.window;
        for (row, id) in keys.iter().enumerate() {
            if row.is_multiple_of(4096) {
                ctx.check()?;
            }
            if id.0.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> (64 - SAMPLE_SHIFT) != 0 {
                continue;
            }
            rows += 1;
            if self.sketch.contains(id) {
                repeats += 1;
            } else if self.sketch.len() < SKETCH_KEYS
                && self.charge.as_mut().is_some_and(|charge| {
                    charge
                        .cover(
                            512 + (self.sketch.len() as u64 + 1) * SKETCH_BYTES,
                            ctx.reservation_step(),
                        )
                        .is_ok()
                })
            {
                self.sketch.insert(*id);
            }
        }
        self.window = (rows, repeats);
        Ok(())
    }

    /// Keep the evaluated values of a batch's pending entries while the map is below
    /// its cap and its retained charge covers them, and remove the rest. A removal
    /// shrinks the table, so that its allocation stays within the retained charge once
    /// the batch's scratch is released.
    fn retain(&mut self, ctx: &Ctx, keys: &[Id], values: &[Id]) {
        let step = ctx.reservation_step();
        // Entries the map held before this batch, which the charge already covers.
        let mut held = self.values.len() - keys.len();
        let mut room = true;
        for (key, value) in keys.iter().zip(values) {
            room = room
                && held < self.max_entries
                && self.charge.as_mut().is_some_and(|charge| {
                    charge
                        .cover(512 + (held as u64 + 1) * ENTRY_BYTES, step)
                        .is_ok()
                });
            if room {
                held += 1;
                if let Some(entry) = self.values.get_mut(key) {
                    *entry = *value;
                }
            } else {
                self.values.remove(key);
            }
        }
        if !room {
            self.values.shrink_to_fit();
        }
    }

    /// The map pass of [`Reuse::apply_map`]. Hits go to `output`. A miss gets a pending
    /// slot in `output` and its row goes to `missed`. Below the entry cap the miss also
    /// enters the map with that slot, so the batch's later rows of the key need one
    /// lookup, and the batch's scratch charge covers these entries until retention is
    /// decided. A full map keeps the pending slots in a side map instead, since entries
    /// it inserted and then removed would leave tombstones that force rehashes. This
    /// returns whether the pending slots entered the map.
    fn answer(
        &mut self,
        ctx: &Ctx,
        keys: &[Id],
        output: &mut Vec<Id>,
        missed: &mut Vec<u32>,
        distinct: &mut Vec<Id>,
    ) -> Result<bool> {
        let mut slots: Option<FxHashMap<Id, Id>> =
            (self.values.len() >= self.max_entries).then(FxHashMap::default);
        for (row, key) in keys.iter().enumerate() {
            if row.is_multiple_of(4096) {
                ctx.check()?;
            }
            if let Some(value) = self.values.get(key) {
                if value.0 & !PAYLOAD_MASK == PENDING {
                    missed.push(row as u32);
                }
                output.push(*value);
                continue;
            }
            let next = Id(PENDING | distinct.len() as u64);
            let slot = match &mut slots {
                Some(slots) => *slots.entry(*key).or_insert(next),
                None => {
                    self.values.insert(*key, next);
                    next
                }
            };
            if slot == next {
                distinct.push(*key);
            }
            missed.push(row as u32);
            output.push(slot);
        }
        Ok(slots.is_none())
    }

    /// Answer the batch from the map. Misses are evaluated once per distinct key, as one
    /// column through the ordinary batch evaluation, and retained while the map has room.
    fn apply_map(
        &mut self,
        ctx: &Ctx,
        table: &mut Table,
        column: usize,
        target: VarId,
        expr: &Expr,
    ) -> Result<bool> {
        let n = table.len;
        // The batch's transient memory, beside the input. The output, the missed rows,
        // the distinct misses and their values take 28 bytes a row at their exact
        // capacities. Pending slots take at most 39 bytes a row of hash table, since a
        // table of `c` entries has at most `2c * 8 / 7` buckets of 17 bytes, whether
        // they grow the map or fill the side map. The map's retained charge of
        // `ENTRY_BYTES` an entry covers its existing entries in both the old and the new
        // table while the table grows or shrinks.
        let Ok(_scratch) = ctx.charge(n as u64 * SCRATCH_BYTES + 256) else {
            return Ok(false);
        };
        let mut output = Vec::with_capacity(n);
        let mut missed: Vec<u32> = Vec::with_capacity(n);
        let mut distinct = Vec::with_capacity(n);
        // Every pending entry is resolved before this returns, and an error releases the
        // whole map.
        let pending = match self.answer(
            ctx,
            &table.cols[column],
            &mut output,
            &mut missed,
            &mut distinct,
        ) {
            Ok(pending) => pending,
            Err(error) => {
                self.decline();
                return Err(error);
            }
        };
        let evaluated = distinct.len();
        if !distinct.is_empty() {
            #[cfg(test)]
            {
                self.evaluations += distinct.len();
            }
            let mut misses = Table::new(vec![self.input]);
            misses.len = distinct.len();
            misses.cols[0] = distinct;
            let values = match crate::sparql::exec::column_rows(ctx, &misses, expr)
                .and_then(|values| ctx.check().map(|()| values))
            {
                Ok(values) => values,
                Err(error) => {
                    self.decline();
                    return Err(error);
                }
            };
            for row in &missed {
                let at = &mut output[*row as usize];
                *at = values[(at.0 & PAYLOAD_MASK) as usize];
            }
            if pending {
                self.retain(ctx, &misses.cols[0], &values);
            }
            self.misses += misses.len as u64;
        }
        // Rows that needed no evaluation of their own.
        let answered = n - evaluated;
        self.hits += answered as u64;
        table.vars.push(target);
        table.cols.push(output);
        self.map_window.0 += n as u32;
        self.map_window.1 += answered as u32;
        self.map_rows += n as u64;
        if self.map_window.0 >= MAP_WINDOW
            && (self.map_rows >= MAP_ENTRIES as u64 || self.values.len() >= self.max_entries)
        {
            let (rows, answered) = std::mem::take(&mut self.map_window);
            // A map that answers fewer than one row in `2 * ADMIT` costs more than it saves.
            if answered * 2 * ADMIT < rows {
                self.decline();
            }
        }
        Ok(true)
    }
}

/// Whether a batch of inline integers spans few enough values for the integer map, or
/// `None` when the batch holds another kind of term.
fn dense_integers(ctx: &Ctx, keys: &[Id]) -> Result<Option<bool>> {
    let (mut lo, mut hi) = (u64::MAX, 0);
    for (row, id) in keys.iter().enumerate() {
        if row.is_multiple_of(1024) {
            ctx.check()?;
        }
        if id.tag() != Tag::Int {
            return Ok(None);
        }
        lo = lo.min(id.payload());
        hi = hi.max(id.payload());
    }
    Ok(Some(
        hi.saturating_sub(lo) < (keys.len() / 2).min(65536) as u64,
    ))
}

/// A bounded profitability hint, not a resource or uniqueness proof. Unknown
/// shapes and changed snapshots decline the hint; dense observed reuse remains
/// eligible without statistics. Inner joins cannot add new values to a bound key.
fn distinct_bound(ctx: &Ctx, node: &Node, input: VarId, left: &mut usize) -> Option<u64> {
    if *left == 0 || !ctx.snap.delta.is_empty() {
        return None;
    }
    *left -= 1;
    match &node.kind {
        Kind::Scan(scan) if scan.cols.len() <= 4 && scan.prefix.len() <= 4 => {
            let order = scan.perm.order();
            let pcol = order.iter().position(|col| *col == P)?;
            let predicate = *scan.prefix.get(pcol)?;
            ctx.snap.predicate_count(Perm::Pso, &[predicate])?;
            let stats = ctx.snap.generation.stats.predicate(predicate)?;
            let (col, _) = scan.cols.iter().find(|(_, var)| *var == input)?;
            let distinct = match *order.get(*col)? {
                S => stats.distinct_subjects,
                O => stats.distinct_objects,
                _ => return None,
            };
            (distinct > 0 && distinct <= stats.count).then_some(distinct)
        }
        Kind::Join { .. } if node.children.len() == 2 => {
            let a = distinct_bound(ctx, &node.children[0], input, left);
            let b = distinct_bound(ctx, &node.children[1], input, left);
            match (a, b) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            }
        }
        Kind::Project(vars)
            if vars.len() <= 64 && vars.contains(&input) && node.children.len() == 1 =>
        {
            distinct_bound(ctx, &node.children[0], input, left)
        }
        Kind::Filter(_)
        | Kind::Sort(_)
        | Kind::OrderBy { .. }
        | Kind::Slice { .. }
        | Kind::Distinct
            if node.children.len() == 1 =>
        {
            distinct_bound(ctx, &node.children[0], input, left)
        }
        Kind::Extend(var, _) | Kind::Assign(var, _)
            if *var != input && node.children.len() == 1 =>
        {
            distinct_bound(ctx, &node.children[0], input, left)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::io::Source;
    use crate::sparql::expr::Func;
    use crate::store::{Store, StoreOptions};
    use oxrdf::{Literal, Term, vocab::xsd};
    use oxrdfio::RdfFormat;
    use spargebra::algebra::Function;

    fn new_reuse(ctx: &Ctx, expr: &Expr, source: Option<&Node>) -> Option<Reuse> {
        Reuse::new(ctx, Reuse::prepare(ctx, expr, source)?)
    }

    #[test]
    fn stored_temporal_bind_reuses_exact_ids_across_morsels() {
        let store = Store::in_memory(StoreOptions::default());
        let lexicals = (1950..1982)
            .map(|year| format!("{year}-01-01+00:00"))
            .collect::<Vec<_>>();
        let data = lexicals
            .iter()
            .enumerate()
            .map(|(i, date)| {
                format!(
                    "<urn:s{i}> <urn:p> \"{date}\"^^<http://www.w3.org/2001/XMLSchema#date> .\n"
                )
            })
            .collect::<String>();
        store
            .load(&[Source::from_bytes(
                data.into_bytes(),
                RdfFormat::Turtle,
                None,
            )])
            .unwrap();
        let mut ctx = Ctx::new(store.snapshot());
        ctx.enable_cursor_accounting().unwrap();
        let ctx = std::sync::Arc::new(ctx);
        ctx.attach_owner();
        let input = ctx.var("date");
        let target = ctx.var("year");
        let expr = Expr::Call(Func::Builtin(Function::Year), vec![Expr::Var(input)]);
        let ids = lexicals
            .iter()
            .map(|date| {
                ctx.snap
                    .lookup_term(&Term::Literal(Literal::new_typed_literal(date, xsd::DATE)))
                    .unwrap()
            })
            .collect::<Vec<_>>();
        assert!(ids.iter().all(|id| id.tag() == Tag::Vocab));
        let mut reuse = new_reuse(&ctx, &expr, None).unwrap();
        for epoch in 0..3 {
            let mut table = Table::new(vec![input]);
            table.cols[0] = (0..4096)
                .map(|i| ids[(i * 7 + epoch * 3) % ids.len()])
                .collect();
            table.len = 4096;
            assert!(
                reuse.apply(&ctx, &mut table, target, &expr).unwrap(),
                "stored temporal keys must reuse their expression results"
            );
            for row in 0..table.len {
                let index = (row * 7 + epoch * 3) % ids.len();
                assert_eq!(
                    ctx.term(table.cols[1][row]),
                    Some(Term::Literal(Literal::from(1950 + index as i64)))
                );
            }
        }
        assert_eq!(
            reuse.evaluations, 32,
            "each exact date ID is evaluated only once across three morsels"
        );
    }

    fn cursor_context(store: &Store, memory: u64) -> std::sync::Arc<Ctx> {
        let mut ctx = Ctx::new(store.snapshot());
        ctx.mem_limit = memory;
        ctx.enable_cursor_accounting().unwrap();
        let ctx = std::sync::Arc::new(ctx);
        ctx.attach_owner();
        ctx
    }

    #[test]
    fn generic_reuse_keeps_optional_refusal_identity_and_effect_boundaries() {
        let store = Store::in_memory(StoreOptions::default());
        let ctx = cursor_context(&store, 16 << 10);
        let input = ctx.var("date");
        let target = ctx.var("year");
        let expr = Expr::Call(Func::Builtin(Function::Year), vec![Expr::Var(input)]);
        let date = ctx.intern_term(&Term::Literal(Literal::new_typed_literal(
            "2000-01-01+00:00",
            xsd::DATE,
        )));
        assert_eq!(date.tag(), Tag::Local);
        let mut table = Table::new(vec![input]);
        table.cols[0] = vec![date; 4096];
        table.len = 4096;
        let mut reuse = new_reuse(&ctx, &expr, None).unwrap();
        assert!(!reuse.apply(&ctx, &mut table, target, &expr).unwrap());
        assert!(reuse.values.is_empty());
        assert_eq!(reuse.evaluations, 0);
        ctx.check().unwrap();
        assert!(
            new_reuse(&ctx, &Expr::Var(input), None).is_none(),
            "an identity copy does not need a map"
        );
        let effect = Expr::Arith(
            Box::new(Expr::Var(input)),
            Box::new(Expr::Call(Func::Builtin(Function::Rand), vec![])),
            crate::sparql::expr::ArithOp::Add,
        );
        assert!(new_reuse(&ctx, &effect, None).is_none());

        let ctx = cursor_context(&store, 16 << 20);
        let input = ctx.var("date");
        let target = ctx.var("year");
        let expr = Expr::Call(Func::Builtin(Function::Year), vec![Expr::Var(input)]);
        let mut table = Table::new(vec![input]);
        table.cols[0] = (10000..14096)
            .map(|year| {
                ctx.intern_term(&Term::Literal(Literal::new_typed_literal(
                    format!("{year}-01-01+00:00"),
                    xsd::DATE,
                )))
            })
            .collect();
        table.len = 4096;
        let mut reuse = new_reuse(&ctx, &expr, None).unwrap();
        assert!(!reuse.apply(&ctx, &mut table, target, &expr).unwrap());
        assert!(reuse.values.is_empty());
        assert_eq!(reuse.mode, Mode::Probe);
        // An all-distinct pilot does not retain results, but later skew that the
        // sketch observes admits the map for the rest of the query.
        let first = *table.cols[0].iter().find(|id| sampled(**id)).unwrap();
        table.cols[0].fill(first);
        assert!(reuse.apply(&ctx, &mut table, target, &expr).unwrap());
        assert_eq!(reuse.evaluations, 1);
        assert_eq!(reuse.values.len(), 1);
    }

    fn sampled(id: Id) -> bool {
        id.0.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> (64 - SAMPLE_SHIFT) == 0
    }

    fn local_strings(ctx: &Ctx, keys: impl Iterator<Item = usize>) -> Vec<Id> {
        keys.map(|i| {
            ctx.intern_term(&Term::Literal(Literal::new_simple_literal(format!(
                "value{i:09}"
            ))))
        })
        .collect()
    }

    fn batch(input: VarId, keys: &[Id]) -> Table {
        let mut table = Table::new(vec![input]);
        table.cols[0] = keys.to_vec();
        table.len = keys.len();
        table
    }

    /// Runs `keys` through `reuse` in batches of 4,096 and checks every answer against
    /// row-by-row evaluation of the same rows.
    fn run(ctx: &Ctx, reuse: &mut Reuse, input: VarId, target: VarId, expr: &Expr, keys: &[Id]) {
        for chunk in keys.chunks(4096) {
            let mut table = batch(input, chunk);
            let expected = crate::sparql::exec::column_rows(ctx, &table, expr).unwrap();
            if reuse.apply(ctx, &mut table, target, expr).unwrap() {
                assert_eq!(table.cols[1], expected);
            }
            ctx.check().unwrap();
        }
    }

    #[test]
    fn sparse_reuse_across_batches_admits_after_observed_repetition() {
        let store = Store::in_memory(StoreOptions::default());
        let ctx = cursor_context(&store, 64 << 20);
        let input = ctx.var("value");
        let target = ctx.var("bound");
        let expr = Expr::Call(Func::Builtin(Function::Str), vec![Expr::Var(input)]);
        // Every key is distinct within the first eight batches and repeats after them,
        // the global-repeat shape that a one-shot pilot declines.
        let domain = local_strings(&ctx, 0..32768);
        let keys = (0..131072usize)
            .map(|i| domain[i.wrapping_mul(2654435761) % 32768])
            .collect::<Vec<_>>();
        let mut reuse = new_reuse(&ctx, &expr, None).unwrap();
        run(&ctx, &mut reuse, input, target, &expr, &keys);
        assert_eq!(reuse.mode, Mode::Map);
        assert!(
            reuse.evaluations <= 32768 + 8192,
            "admitted soon after repetition starts: {} evaluations",
            reuse.evaluations
        );
        // A map admitted after the first period fills during the second and then
        // answers every row.
        assert!(
            reuse.hits >= 131072 - 2 * 32768 - 8192,
            "{} rows answered",
            reuse.hits
        );
    }

    #[test]
    fn hot_keys_the_sketch_does_not_sample_admit_from_the_exact_batch_count() {
        let store = Store::in_memory(StoreOptions::default());
        let ctx = cursor_context(&store, 64 << 20);
        let input = ctx.var("value");
        let target = ctx.var("bound");
        let expr = Expr::Call(Func::Builtin(Function::Str), vec![Expr::Var(input)]);
        let mut reuse = new_reuse(&ctx, &expr, None).unwrap();
        run(
            &ctx,
            &mut reuse,
            input,
            target,
            &expr,
            &local_strings(&ctx, 0..4096),
        );
        assert_eq!(reuse.mode, Mode::Probe);
        // Eight hot keys, none of which the sketch samples.
        let hot = local_strings(&ctx, 10_000..20_000)
            .into_iter()
            .filter(|id| !sampled(*id))
            .take(8)
            .collect::<Vec<_>>();
        let keys = (0..4096).map(|i| hot[i % 8]).collect::<Vec<_>>();
        let mut table = batch(input, &keys);
        assert!(!reuse.apply(&ctx, &mut table, target, &expr).unwrap());
        assert_eq!(reuse.mode, Mode::Probe, "the sketch sees no repetition");
        // The per-batch distinct pass counted 8 distinct of 4,096 rows.
        reuse.counted(4096, 8);
        assert_eq!(reuse.mode, Mode::Map);
        run(&ctx, &mut reuse, input, target, &expr, &keys);
        assert_eq!(reuse.evaluations, 8);
        reuse.counted(4096, 4096);
        assert_eq!(
            reuse.mode,
            Mode::Map,
            "an admitted map keeps its own release rule"
        );
    }

    fn pending_entries(reuse: &Reuse) -> usize {
        reuse
            .values
            .values()
            .filter(|value| value.0 & !PAYLOAD_MASK == PENDING)
            .count()
    }

    #[test]
    fn a_map_that_fills_during_a_batch_keeps_no_pending_entries() {
        let store = Store::in_memory(StoreOptions::default());
        let ctx = cursor_context(&store, 64 << 20);
        let input = ctx.var("value");
        let target = ctx.var("bound");
        let expr = Expr::Call(Func::Builtin(Function::Str), vec![Expr::Var(input)]);
        let mut reuse = new_reuse(&ctx, &expr, None).unwrap();
        // A dense first batch admits the map with room for 100 entries.
        let dense = local_strings(&ctx, (0..4096).map(|i| i % 64));
        run(&ctx, &mut reuse, input, target, &expr, &dense);
        assert_eq!(reuse.mode, Mode::Map);
        reuse.max_entries = 100;
        // This batch repeats 200 keys, so it fills the map partway through and its
        // keys beyond the room are answered but not retained.
        let keys = local_strings(&ctx, (0..4096).map(|i| 1000 + i % 200));
        run(&ctx, &mut reuse, input, target, &expr, &keys);
        assert_eq!(reuse.values.len(), 100);
        assert_eq!(pending_entries(&reuse), 0);
        // A full map keeps the slots of its misses apart and still answers every row.
        let before = reuse.evaluations;
        run(&ctx, &mut reuse, input, target, &expr, &keys);
        assert_eq!(reuse.values.len(), 100);
        assert_eq!(pending_entries(&reuse), 0);
        // The 64 dense keys left 36 places, so 164 of the 200 keys miss again.
        assert_eq!(reuse.evaluations - before, 164);
    }

    #[test]
    fn unique_inputs_stop_probing_once_the_sketch_is_full() {
        let store = Store::in_memory(StoreOptions::default());
        let ctx = cursor_context(&store, 64 << 20);
        let input = ctx.var("value");
        let target = ctx.var("bound");
        let expr = Expr::Call(Func::Builtin(Function::Str), vec![Expr::Var(input)]);
        let keys = local_strings(&ctx, 0..90000);
        let mut reuse = new_reuse(&ctx, &expr, None).unwrap();
        run(&ctx, &mut reuse, input, target, &expr, &keys);
        assert_eq!(reuse.mode, Mode::Off);
        assert_eq!(reuse.evaluations, 0);
        assert!(reuse.sketch.is_empty() && reuse.values.is_empty());
        assert_eq!(reuse.charge.as_ref().unwrap().bytes(), 512);
    }

    #[test]
    fn the_batch_that_fills_the_sketch_can_still_admit_from_its_exact_count() {
        let store = Store::in_memory(StoreOptions::default());
        let ctx = cursor_context(&store, 64 << 20);
        let input = ctx.var("value");
        let target = ctx.var("bound");
        let expr = Expr::Call(Func::Builtin(Function::Str), vec![Expr::Var(input)]);
        let all = local_strings(&ctx, 0..120_000);
        let (sampled_keys, unsampled): (Vec<Id>, Vec<Id>) =
            all.into_iter().partition(|id| sampled(*id));
        assert!(sampled_keys.len() > SKETCH_KEYS + WINDOW as usize);
        // A pilot of 4,095 sampled keys and one unsampled key leaves the sketch one
        // window short of full.
        let mut first = sampled_keys[..SKETCH_KEYS - 1].to_vec();
        first.push(unsampled[0]);
        let mut reuse = new_reuse(&ctx, &expr, None).unwrap();
        assert!(
            !reuse
                .apply(&ctx, &mut batch(input, &first), target, &expr)
                .unwrap()
        );
        assert_eq!(reuse.mode, Mode::Probe);
        assert_eq!(reuse.sketch.len(), SKETCH_KEYS - 1);
        // The next batch fills the sketch with new sampled keys, while one unsampled
        // key repeats in the rest of it.
        let hot = unsampled[1];
        let mut keys = sampled_keys[SKETCH_KEYS..SKETCH_KEYS + WINDOW as usize].to_vec();
        keys.resize(4096, hot);
        assert!(
            !reuse
                .apply(&ctx, &mut batch(input, &keys), target, &expr)
                .unwrap()
        );
        assert!(reuse.exhausted);
        reuse.counted(4096, WINDOW as usize + 1);
        assert_eq!(reuse.mode, Mode::Map, "the exact count admits the map");
        let before = reuse.evaluations;
        run(&ctx, &mut reuse, input, target, &expr, &vec![hot; 4096]);
        assert_eq!(reuse.evaluations - before, 1);
        // Without that count, the next batch declines.
        let mut reuse = new_reuse(&ctx, &expr, None).unwrap();
        assert!(
            !reuse
                .apply(&ctx, &mut batch(input, &first), target, &expr)
                .unwrap()
        );
        assert!(
            !reuse
                .apply(&ctx, &mut batch(input, &keys), target, &expr)
                .unwrap()
        );
        reuse.counted(4096, 4096);
        assert!(
            !reuse
                .apply(&ctx, &mut batch(input, &keys), target, &expr)
                .unwrap()
        );
        assert_eq!(reuse.mode, Mode::Off);
    }

    #[test]
    fn an_admitted_map_that_stops_answering_rows_is_released() {
        let store = Store::in_memory(StoreOptions::default());
        let ctx = cursor_context(&store, 64 << 20);
        let input = ctx.var("value");
        let target = ctx.var("bound");
        let expr = Expr::Call(Func::Builtin(Function::Str), vec![Expr::Var(input)]);
        // A dense first batch admits the map, and unique keys follow.
        let mut keys = local_strings(&ctx, (0..4096).map(|i| i % 64));
        keys.extend(local_strings(&ctx, 100..100_000));
        let mut reuse = new_reuse(&ctx, &expr, None).unwrap();
        run(&ctx, &mut reuse, input, target, &expr, &keys[..4096]);
        assert_eq!(reuse.mode, Mode::Map);
        run(&ctx, &mut reuse, input, target, &expr, &keys[4096..]);
        assert_eq!(reuse.mode, Mode::Off);
        assert!(reuse.values.is_empty());
        assert_eq!(reuse.charge.as_ref().unwrap().bytes(), 512);
        assert!(
            reuse.evaluations <= 64 + MAP_ENTRIES + MAP_WINDOW as usize,
            "the map is judged once it has seen as many rows as it holds"
        );
    }

    #[test]
    fn exact_id_cache_keeps_errors_and_term_identity_and_checks_all_hit_cancellation() {
        let store = Store::in_memory(StoreOptions::default());
        let ctx = cursor_context(&store, 16 << 20);
        let input = ctx.var("date");
        let target = ctx.var("year");
        let expr = Expr::Call(Func::Builtin(Function::Year), vec![Expr::Var(input)]);
        let terms = [
            Literal::new_typed_literal("2000-01-01+00:00", xsd::DATE),
            Literal::new_typed_literal("2000-01-01Z", xsd::DATE),
            Literal::new_typed_literal("bad-date", xsd::DATE),
            Literal::new_simple_literal("2000-01-01"),
        ];
        let keys = terms
            .iter()
            .map(|t| ctx.intern_term(&Term::Literal(t.clone())))
            .chain([Id::UNDEF])
            .collect::<Vec<_>>();
        assert_ne!(
            keys[0], keys[1],
            "equal date values with different lexical forms are distinct terms"
        );
        let mut reuse = new_reuse(&ctx, &expr, None).unwrap();
        for epoch in 0..2 {
            let mut table = Table::new(vec![input]);
            table.cols[0] = (0..4096)
                .map(|row| keys[(row + epoch) % keys.len()])
                .collect();
            table.len = 4096;
            assert!(reuse.apply(&ctx, &mut table, target, &expr).unwrap());
            for row in 0..table.len {
                if (row + epoch) % keys.len() < 2 {
                    assert_eq!(
                        ctx.term(table.cols[1][row]),
                        Some(Term::Literal(Literal::from(2000)))
                    );
                } else {
                    assert_eq!(table.cols[1][row], Id::UNDEF);
                }
            }
        }
        assert_eq!(reuse.evaluations, keys.len());
        let mut hits = Table::new(vec![input]);
        hits.cols[0] = vec![keys[0]; 4096];
        hits.len = 4096;
        ctx.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        assert!(matches!(
            reuse.apply(&ctx, &mut hits, target, &expr),
            Err(crate::Error::Cancelled)
        ));
    }

    #[test]
    fn near_unique_generic_inputs_decline_optional_retention() {
        let store = Store::in_memory(StoreOptions::default());
        let ctx = cursor_context(&store, 16 << 20);
        let input = ctx.var("value");
        let target = ctx.var("bound");
        let expr = Expr::Call(Func::Builtin(Function::Str), vec![Expr::Var(input)]);
        let keys = (0..4096)
            .map(|i| {
                ctx.intern_term(&Term::Literal(Literal::new_simple_literal(format!(
                    "value{i}"
                ))))
            })
            .collect::<Vec<_>>();
        for stride in [usize::MAX, 64] {
            let mut table = Table::new(vec![input]);
            table.cols[0] = (0..4096)
                .map(|i| {
                    if i == 1 || (i > 0 && i % stride == 0) {
                        keys[i - 1]
                    } else {
                        keys[i]
                    }
                })
                .collect();
            table.len = 4096;
            let mut reuse = new_reuse(&ctx, &expr, None).unwrap();
            assert!(
                !reuse.apply(&ctx, &mut table, target, &expr).unwrap(),
                "isolated or infrequent repetition must not retain mostly distinct results"
            );
            assert!(reuse.values.is_empty());
            assert_eq!(reuse.evaluations, 0);
            assert_eq!(table.cols.len(), 1);
            ctx.check().unwrap();
        }
        let mut table = Table::new(vec![input]);
        table.cols[0] = (0..4096).map(|i| keys[i % 3072]).collect();
        table.len = 4096;
        let mut reuse = new_reuse(&ctx, &expr, None).unwrap();
        assert!(reuse.apply(&ctx, &mut table, target, &expr).unwrap());
        assert_eq!(
            reuse.evaluations, 3072,
            "substantial general reuse remains eligible"
        );
        assert_eq!(
            table.cols[0], table.cols[1],
            "STR of strings keeps the exact values"
        );
    }

    #[test]
    fn domain_hint_is_snapshot_local_and_plan_walk_is_bounded() {
        use crate::sparql::plan::{GraphFilter, ScanSpec};
        let store = Store::in_memory(StoreOptions::default());
        let data = (0..4096)
            .map(|i| {
                format!(
                    "<urn:s{i:05}> <urn:p> \"value{}\" .\n",
                    if i == 1 { 0 } else { i % 3000 }
                )
            })
            .collect::<String>();
        store
            .load(&[Source::from_bytes(
                data.into_bytes(),
                RdfFormat::Turtle,
                None,
            )])
            .unwrap();
        let ctx = cursor_context(&store, 16 << 20);
        assert!(ctx.snap.delta.is_empty());
        let input = ctx.var("value");
        let target = ctx.var("bound");
        let subject = ctx.var("subject");
        let predicate = ctx
            .snap
            .lookup_term(&Term::NamedNode(oxrdf::NamedNode::new("urn:p").unwrap()))
            .unwrap();
        let source = Node::leaf(
            Kind::Scan(ScanSpec {
                perm: Perm::Pso,
                prefix: vec![predicate.0],
                cols: vec![(1, subject), (2, input)],
                eqs: vec![],
                graph: GraphFilter::Default,
                graph_col: 3,
                dedup: false,
                src: None,
            }),
            vec![subject, input],
            4096.0,
            String::new(),
        );
        let expr = Expr::Call(Func::Builtin(Function::Str), vec![Expr::Var(input)]);
        let before = ctx.live_bytes();
        let admission = Reuse::prepare(&ctx, &expr, Some(&source)).unwrap();
        assert_eq!(
            ctx.live_bytes(),
            before,
            "metadata preparation must not retain optional memory before child construction"
        );
        let mut reuse = Reuse::new(&ctx, admission).unwrap();
        assert!(reuse.expected_reuse);
        let mut bind = Node::leaf(
            Kind::Extend(target, expr.clone()),
            vec![subject, input, target],
            4096.0,
            String::new(),
        );
        bind.children.push(source.clone());
        let mut operator = super::super::Operator::build(
            &ctx,
            bind,
            super::super::FallbackPolicy::RejectMaterialization,
        )
        .unwrap();
        assert!(
            operator.bind_reuse.as_ref().unwrap().expected_reuse,
            "operator construction must inspect the input before replacing child plans with shells"
        );
        let options = super::super::CursorOptions {
            batch_rows: 1024,
            ..Default::default()
        };
        let mut rows = 0;
        while let Some(batch) = operator.next(&ctx, &options, 1024).unwrap() {
            rows += batch.table.len;
            assert_eq!(batch.table.cols[1], batch.table.cols[2]);
        }
        assert_eq!(rows, 4096);
        assert_eq!(
            operator.bind_reuse.as_ref().unwrap().evaluations,
            3000,
            "a sparse first pilot must still reuse the bounded global domain across morsels"
        );
        let table = || {
            let mut table = Table::new(vec![input]);
            table.cols[0] = (0..1024)
                .map(|i| {
                    let i = if i == 1 { 0 } else { i };
                    ctx.snap
                        .lookup_term(&Term::Literal(Literal::new_simple_literal(format!(
                            "value{i}"
                        ))))
                        .unwrap()
                })
                .collect();
            table.len = 1024;
            table
        };
        assert!(reuse.apply(&ctx, &mut table(), target, &expr).unwrap());
        let mut deep = source.clone();
        for _ in 0..64 {
            let mut parent =
                Node::leaf(Kind::Sort(vec![input]), vec![input], 4096.0, String::new());
            parent.children.push(deep);
            deep = parent;
        }
        assert!(!new_reuse(&ctx, &expr, Some(&deep)).unwrap().expected_reuse);
        let mut hidden = Node::leaf(Kind::Project(vec![]), vec![], 4096.0, String::new());
        hidden.children.push(source.clone());
        assert!(
            !new_reuse(&ctx, &expr, Some(&hidden))
                .unwrap()
                .expected_reuse
        );
        crate::sparql::update::update(
            &store,
            "INSERT DATA { <urn:new> <urn:p> \"new value\" }",
            &crate::sparql::QueryOptions::default(),
        )
        .unwrap();
        let changed = cursor_context(&store, 16 << 20);
        assert!(!changed.snap.delta.is_empty());
        assert_eq!(changed.var("value"), input);
        let mut declined = new_reuse(&changed, &expr, Some(&source)).unwrap();
        assert!(!declined.expected_reuse);
        assert!(
            !declined
                .apply(&changed, &mut table(), target, &expr)
                .unwrap()
        );
        assert!(
            new_reuse(&ctx, &expr, Some(&source))
                .unwrap()
                .expected_reuse,
            "the retained old snapshot keeps its own unchanged metadata"
        );
    }

    #[test]
    fn optional_cache_does_not_starve_mandatory_child_construction() {
        use super::super::{CursorOptions, FallbackPolicy, Operator};
        let store = Store::in_memory(StoreOptions::default());
        let ctx = cursor_context(&store, 768);
        let input = ctx.var("input");
        let target = ctx.var("bound");
        let plan = || {
            let mut distinct = Node::leaf(Kind::Distinct, vec![input], 0.0, String::new());
            distinct
                .children
                .push(Node::leaf(Kind::Empty, vec![input], 0.0, String::new()));
            let expr = Expr::Call(Func::Builtin(Function::Str), vec![Expr::Var(input)]);
            let mut bind = Node::leaf(
                Kind::Extend(target, expr),
                vec![input, target],
                0.0,
                String::new(),
            );
            bind.children.push(distinct);
            bind
        };
        let early = ctx.retained_charge(512).unwrap();
        assert!(
            Operator::build(&ctx, plan(), FallbackPolicy::RejectMaterialization).is_err(),
            "an early optional reservation can starve the child's mandatory264-byte state"
        );
        drop(early);
        let mut operator = Operator::build(&ctx, plan(), FallbackPolicy::RejectMaterialization)
            .expect(
                "optional BIND state must decline after mandatory children use the memory budget",
            );
        assert!(operator.bind_reuse.is_none());
        assert!(
            operator
                .next(&ctx, &CursorOptions::default(), 1024)
                .unwrap()
                .is_none()
        );
        ctx.check().unwrap();
    }

    #[test]
    fn cache_growth_refusal_retains_a_bounded_partial_map_and_normal_evaluation() {
        let store = Store::in_memory(StoreOptions::default());
        let ctx = cursor_context(&store, 128 << 10);
        let input = ctx.var("date");
        let target = ctx.var("year");
        let expr = Expr::Call(Func::Builtin(Function::Year), vec![Expr::Var(input)]);
        let date = ctx.intern_term(&Term::Literal(Literal::new_typed_literal(
            "2000-01-01+00:00",
            xsd::DATE,
        )));
        let mut reuse = new_reuse(&ctx, &expr, None).unwrap();
        let mut retained = None;
        for epoch in 0..4 {
            let mut table = Table::new(vec![input]);
            table.cols[0] = (0..1024)
                .map(|row| {
                    if row < 257 {
                        date
                    } else {
                        Id::from_i64((epoch * 1024 + row) as i64).unwrap()
                    }
                })
                .collect();
            table.len = 1024;
            assert!(reuse.apply(&ctx, &mut table, target, &expr).unwrap());
            assert_eq!(
                ctx.term(table.cols[1][0]),
                Some(Term::Literal(Literal::from(2000)))
            );
            assert!(table.cols[1][257..].iter().all(|id| *id == Id::UNDEF));
            assert!(ctx.live_bytes() <= 128 << 10);
            ctx.check().unwrap();
            // The table the map allocated, a key and a value per slot, stays within its
            // retained charge after the batch's scratch is released.
            assert_eq!(pending_entries(&reuse), 0);
            assert!(
                reuse.values.capacity() as u64 * 16 <= reuse.charge.as_ref().unwrap().bytes(),
                "capacity {} against a charge of {}",
                reuse.values.capacity(),
                reuse.charge.as_ref().unwrap().bytes()
            );
            if let Some(entries) = retained {
                assert_eq!(
                    reuse.values.len(),
                    entries,
                    "a refused optional growth keeps the existing bounded entries"
                );
            } else {
                assert!(reuse.values.len() < 768);
                retained = Some(reuse.values.len());
            }
        }
        assert_eq!(
            reuse.evaluations,
            1 + 4 * 767,
            "uncached keys still evaluate normally after the cache fills"
        );
    }
}

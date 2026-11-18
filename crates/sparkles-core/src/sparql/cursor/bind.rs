//! Query-owned exact-ID reuse for pure, repeated single-input BIND expressions.
//! Dense inline integers retain their cheap admission. Other inputs need observed
//! repetition in a bounded pilot. Optional cache entries never weaken query budgets.
use crate::error::Result;
use crate::id::{Id, Tag};
use crate::index::{O, P, Perm, S};
use crate::sparql::ctx::RetainedCharge;
use crate::sparql::expr::{Expr, Row, eval};
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

/// The experiment switch `SPARKLES_BIND_REUSE`: `observed` (the default), `legacy` (the
/// one-shot pilot) or `off`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Policy {
    Observed,
    Legacy,
    Off,
}
fn policy() -> Policy {
    static POLICY: std::sync::OnceLock<Policy> = std::sync::OnceLock::new();
    *POLICY.get_or_init(|| match std::env::var("SPARKLES_BIND_REUSE").as_deref() {
        Ok("legacy") => Policy::Legacy,
        Ok("off") => Policy::Off,
        _ => Policy::Observed,
    })
}

pub(super) struct Reuse {
    input: VarId,
    mode: Mode,
    values: FxHashMap<Id, Id>,
    charge: Option<RetainedCharge>,
    expected_reuse: bool,
    max_entries: usize,
    sketch: FxHashSet<Id>,
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
        if !ctx.opt.expr_cache || policy() == Policy::Off {
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
            window: (0, 0),
            map_window: (0, 0),
            map_rows: 0,
            hits: 0,
            misses: 0,
            #[cfg(test)]
            evaluations: 0,
        })
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
        if policy() == Policy::Legacy {
            return self.apply_rows(ctx, table, column, target, expr);
        }
        self.apply_map(ctx, table, column, target, expr)
    }

    fn admit(&mut self) {
        self.mode = Mode::Map;
        self.sketch = Default::default();
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
    /// seed the sketch, or the legacy policy declines for the query.
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
        if repeats > 0 && (repeats * 4 >= sample || self.expected_reuse) {
            self.max_entries = MAP_ENTRIES;
            self.admit();
            return Ok(true);
        }
        if policy() == Policy::Legacy {
            self.decline();
            return Ok(false);
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

    /// Later batches: admit once a window of sampled rows repeats a quarter of the
    /// time, and decline once the sketch is full without that.
    fn probe(&mut self, ctx: &Ctx, keys: &[Id]) -> Result<bool> {
        self.observe(ctx, keys)?;
        let (rows, repeats) = self.window;
        if rows < WINDOW {
            return Ok(false);
        }
        self.window = (0, 0);
        if repeats * 4 >= rows {
            self.admit();
            return Ok(true);
        }
        if self.sketch.len() >= SKETCH_KEYS {
            self.decline();
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
        // Output, miss rows, distinct miss keys and their slots, beside the input.
        let Ok(_scratch) = ctx.charge(n as u64 * 40 + 256) else {
            return Ok(false);
        };
        let keys = &table.cols[column];
        let mut output = Vec::with_capacity(n);
        let mut missed: Vec<u32> = Vec::new();
        let mut slots: FxHashMap<Id, u32> = FxHashMap::default();
        let mut distinct = Vec::new();
        for (row, key) in keys.iter().enumerate() {
            if row.is_multiple_of(4096) {
                ctx.check()?;
            }
            if let Some(value) = self.values.get(key) {
                output.push(*value);
            } else {
                let next = distinct.len() as u32;
                let slot = *slots.entry(*key).or_insert(next);
                if slot == next {
                    distinct.push(*key);
                }
                missed.push(row as u32);
                // the slot until the values are known
                output.push(Id(slot as u64));
            }
        }
        if !distinct.is_empty() {
            #[cfg(test)]
            {
                self.evaluations += distinct.len();
            }
            let mut misses = Table::new(vec![self.input]);
            misses.len = distinct.len();
            misses.cols[0] = distinct;
            let values = crate::sparql::exec::column_rows(ctx, &misses, expr)?;
            ctx.check()?;
            for row in &missed {
                let at = &mut output[*row as usize];
                *at = values[at.0 as usize];
            }
            let step = ctx.reservation_step();
            for (key, value) in misses.cols[0].iter().zip(&values) {
                if self.values.len() >= self.max_entries
                    || !self.charge.as_mut().is_some_and(|charge| {
                        charge
                            .cover(512 + (self.values.len() as u64 + 1) * ENTRY_BYTES, step)
                            .is_ok()
                    })
                {
                    break;
                }
                self.values.insert(*key, *value);
            }
            self.misses += misses.len as u64;
        }
        // Rows that needed no evaluation of their own.
        let answered = n - slots.len();
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
            // A map that answers fewer than an eighth of its rows costs more than it saves.
            if answered * 8 < rows {
                self.decline();
            }
        }
        Ok(true)
    }

    /// The legacy evaluation of an admitted map: misses one row at a time.
    fn apply_rows(
        &mut self,
        ctx: &Ctx,
        table: &mut Table,
        column: usize,
        target: VarId,
        expr: &Expr,
    ) -> Result<bool> {
        let map = table.var_map(ctx.nvars());
        let mut output = Vec::with_capacity(table.len);
        for row in 0..table.len {
            if row.is_multiple_of(1024) {
                ctx.check()?;
            }
            let key = table.cols[column][row];
            let value = if let Some(value) = self.values.get(&key) {
                self.hits += 1;
                *value
            } else {
                #[cfg(test)]
                {
                    self.evaluations += 1;
                }
                self.misses += 1;
                let value = eval(
                    expr,
                    &Row {
                        table,
                        i: row,
                        map: &map,
                        dec: None,
                    },
                    ctx,
                )
                .ok()
                .map_or(Id::UNDEF, |value| value.into_id(ctx));
                ctx.check()?;
                if self.values.len() < self.max_entries
                    && self.charge.as_mut().is_some_and(|charge| {
                        charge
                            .resize(512 + (self.values.len() as u64 + 1) * ENTRY_BYTES)
                            .is_ok()
                    })
                {
                    self.values.insert(key, value);
                }
                value
            };
            output.push(value);
        }
        table.vars.push(target);
        table.cols.push(output);
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
        let ctx = cursor_context(&store, 64 << 10);
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
            assert!(ctx.live_bytes() <= 64 << 10);
            ctx.check().unwrap();
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

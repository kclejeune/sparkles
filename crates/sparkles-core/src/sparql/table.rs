//! Column-major result tables (QLever `IdTable`).

use crate::id::Id;

pub type VarId = u32;

#[derive(Clone, Debug, Default)]
pub struct Table {
    pub vars: Vec<VarId>,
    pub cols: Vec<Vec<Id>>,
    /// number of rows (needed for zero-column tables)
    pub len: usize,
    /// the table is sorted by these variables (lexicographically, by raw id)
    pub sorted: Vec<VarId>,
}

/// A read-only borrow of an immutable query result's identifiers. No Table or
/// column allocation can be detached from this view's result lifetime.
#[derive(Clone, Copy)]
pub struct TableView<'a> {
    table: &'a Table,
}
impl<'a> TableView<'a> {
    pub(super) fn new(table: &'a Table) -> Self {
        Self { table }
    }
    pub fn len(self) -> usize {
        self.table.len()
    }
    pub fn is_empty(self) -> bool {
        self.table.is_empty()
    }
    pub fn width(self) -> usize {
        self.table.width()
    }
    pub fn variables(self) -> &'a [VarId] {
        &self.table.vars
    }
    pub fn sorted(self) -> &'a [VarId] {
        &self.table.sorted
    }
    pub fn columns(self) -> impl ExactSizeIterator<Item = &'a [Id]> {
        self.table.cols.iter().map(Vec::as_slice)
    }
    pub fn column(self, index: usize) -> Option<&'a [Id]> {
        self.table.cols.get(index).map(Vec::as_slice)
    }
    pub fn col_of(self, var: VarId) -> Option<usize> {
        self.table.col_of(var)
    }
    pub fn get(self, row: usize, column: usize) -> Result<Id, crate::Error> {
        self.table
            .cols
            .get(column)
            .and_then(|values| values.get(row))
            .copied()
            .ok_or_else(|| crate::Error::invalid("query table cell is out of bounds"))
    }
}

impl Table {
    pub fn new(vars: Vec<VarId>) -> Table {
        let cols = vars.iter().map(|_| Vec::new()).collect();
        Table {
            vars,
            cols,
            len: 0,
            sorted: Vec::new(),
        }
    }

    /// The table with one empty solution (join identity).
    pub fn unit() -> Table {
        Table {
            vars: Vec::new(),
            cols: Vec::new(),
            len: 1,
            sorted: Vec::new(),
        }
    }

    pub fn empty(vars: Vec<VarId>) -> Table {
        Table::new(vars)
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.len
    }
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
    #[inline]
    pub fn width(&self) -> usize {
        self.vars.len()
    }
    #[inline]
    pub fn col_of(&self, v: VarId) -> Option<usize> {
        self.vars.iter().position(|&x| x == v)
    }
    #[inline]
    pub fn get(&self, row: usize, col: usize) -> Id {
        self.cols[col][row]
    }

    pub fn push_row(&mut self, row: &[Id]) {
        for (c, v) in row.iter().enumerate() {
            self.cols[c].push(*v);
        }
        self.len += 1;
    }

    pub fn row(&self, i: usize) -> Vec<Id> {
        self.cols.iter().map(|c| c[i]).collect()
    }

    pub fn bytes(&self) -> usize {
        self.len * self.width() * 8
    }

    /// Estimated memory for the query memory budget: the ids plus a fixed overhead.
    /// The bytes the table's vectors have allocated, which can exceed
    /// [`Table::mem_bytes`] after a filter compacted its columns in place.
    pub fn capacity_bytes(&self) -> u64 {
        let ids = self
            .cols
            .iter()
            .map(|c| c.capacity() as u64)
            .sum::<u64>()
            .saturating_mul(std::mem::size_of::<Id>() as u64);
        let vars = (self.vars.capacity() + self.sorted.capacity()) as u64
            * std::mem::size_of::<VarId>() as u64;
        ids.saturating_add(vars)
            .saturating_add(self.cols.capacity() as u64 * std::mem::size_of::<Vec<Id>>() as u64)
            .saturating_add(64)
    }

    pub fn mem_bytes(&self) -> u64 {
        super::ctx::table_bytes(self.len, self.width()) + 64
    }

    /// Map from variable id to column (indexed by var id).
    pub fn var_map(&self, nvars: usize) -> Vec<Option<usize>> {
        let mut m =
            vec![None; nvars.max(self.vars.iter().map(|&v| v as usize + 1).max().unwrap_or(0))];
        for (c, &v) in self.vars.iter().enumerate() {
            m[v as usize] = Some(c);
        }
        m
    }

    /// Keep only rows where `keep[i]`.
    pub fn filter_rows(&mut self, keep: &[bool]) {
        let keep = &keep[..self.len];
        for col in &mut self.cols {
            // branch-free compaction: masks are often unpredictable
            let mut w = 0;
            for r in 0..keep.len() {
                col[w] = col[r];
                w += keep[r] as usize;
            }
            col.truncate(w);
        }
        self.len = keep.iter().filter(|&&k| k).count();
    }

    /// Reorder rows by an index permutation.
    pub fn take_rows(&self, idx: &[usize]) -> Table {
        Table {
            vars: self.vars.clone(),
            cols: self
                .cols
                .iter()
                .map(|c| idx.iter().map(|&i| c[i]).collect())
                .collect(),
            len: idx.len(),
            sorted: Vec::new(),
        }
    }

    /// Select / reorder columns by variable, adding UNDEF columns for missing vars.
    pub fn project(mut self, vars: &[VarId]) -> Table {
        let len = self.len;
        let mut cols = Vec::with_capacity(vars.len());
        for v in vars {
            match self.col_of(*v) {
                Some(c) => cols.push(std::mem::take(&mut self.cols[c])),
                None => cols.push(vec![Id::UNDEF; len]),
            }
        }
        let sorted: Vec<VarId> = self
            .sorted
            .iter()
            .take_while(|v| vars.contains(v))
            .copied()
            .collect();
        Table {
            vars: vars.to_vec(),
            cols,
            len,
            sorted,
        }
    }

    pub fn truncate(&mut self, n: usize) {
        if n < self.len {
            for c in &mut self.cols {
                c.truncate(n);
            }
            self.len = n;
        }
    }

    pub fn slice(mut self, offset: usize, limit: Option<usize>) -> Table {
        let start = offset.min(self.len);
        let end = limit.map_or(self.len, |l| start.saturating_add(l).min(self.len));
        if start == 0 && end == self.len {
            return self;
        }
        for c in &mut self.cols {
            *c = c[start..end].to_vec();
        }
        self.len = end - start;
        self
    }

    /// Sort rows by the given columns (raw id order, UNDEF first). Rows with equal
    /// keys keep their order, which a grouping whose aggregates see a group's rows in
    /// input order relies on. The row position breaks ties in the unstable sort, so the
    /// sort stays in place.
    pub fn sort_by_vars(&mut self, vars: &[VarId]) {
        if self.sorted.starts_with(vars) || self.len <= 1 {
            self.sorted = vars.to_vec();
            return;
        }
        let cols: Vec<usize> = vars.iter().filter_map(|v| self.col_of(*v)).collect();
        let mut idx: Vec<usize> = (0..self.len).collect();
        use rayon::prelude::*;
        if cols.len() == 1 {
            let c = &self.cols[cols[0]];
            idx.par_sort_unstable_by_key(|&i| (c[i], i));
        } else {
            idx.par_sort_unstable_by(|&a, &b| {
                for &c in &cols {
                    match self.cols[c][a].cmp(&self.cols[c][b]) {
                        std::cmp::Ordering::Equal => continue,
                        o => return o,
                    }
                }
                a.cmp(&b)
            });
        }
        let mut t = self.take_rows(&idx);
        t.sorted = vars.to_vec();
        *self = t;
    }

    /// Append the rows of another table with the same variables (in any column order).
    pub fn append(&mut self, other: Table) {
        let other = if other.vars == self.vars {
            other
        } else {
            other.project(&self.vars.clone())
        };
        for (c, col) in other.cols.into_iter().enumerate() {
            self.cols[c].extend(col);
        }
        self.len += other.len;
        self.sorted.clear();
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum InputCredit {
    Plain,
    Fast,
    Lease,
}

pub(super) enum FinalTableCredit {
    Fast {
        _credit: super::ctx::OutputCredit,
    },
    Lease {
        _credit: super::query_memory::QueryMemoryLease,
    },
}

/// Protect final construction, including errors before QueryResult is published.
pub(super) struct RetainedMaterialized {
    pub(super) table: Table,
    _ctx: std::sync::Arc<super::ctx::Ctx>,
    pub(super) credit: Option<FinalTableCredit>,
}

/// Private materialization ownership. Payload and context precede admission credit.
pub(crate) struct Materialized<'ctx> {
    pub(super) table: Table,
    _ctx: Option<std::sync::Arc<super::ctx::Ctx>>,
    fast: Option<super::ctx::Charge<'ctx>>,
    pub(super) lease: Option<super::query_memory::QueryMemoryLease>,
}
impl<'ctx> Materialized<'ctx> {
    pub(crate) fn as_table(&self) -> &Table {
        &self.table
    }

    /// Only an explicitly raw application facade may detach this legacy table.
    /// Engine consumers keep this actual owner or make a prepaid receiving copy.
    #[cfg(feature = "geo")]
    pub(crate) fn into_application_table(self) -> Table {
        self.table
    }

    /// Copy into an existing prepaid destination while the real source stays
    /// protected. Capacity growth is admitted before allocation; a failed copy
    /// restores row lengths and keeps all surviving capacity covered.
    #[cfg(any(feature = "geo", test))]
    pub(crate) fn append_copy(
        &mut self,
        ctx: &super::ctx::Ctx,
        source: &Materialized<'_>,
    ) -> crate::Result<()> {
        ctx.check()?;
        if (source.fast.is_some() && source.lease.is_some())
            || source
                .lease
                .as_ref()
                .is_some_and(|credit| credit.service.is_some())
            || self.table.vars != source.table.vars
            || self.table.cols.len() != self.table.vars.len()
            || source.table.cols.len() != source.table.vars.len()
            || self
                .table
                .cols
                .iter()
                .any(|column| column.len() != self.table.len)
            || source
                .table
                .cols
                .iter()
                .any(|column| column.len() != source.table.len)
            || source
                ._ctx
                .as_ref()
                .is_some_and(|owner| !std::ptr::eq(owner.as_ref(), ctx))
            || source
                .fast
                .as_ref()
                .is_some_and(|credit| !credit.belongs_to(ctx))
            || source
                .lease
                .as_ref()
                .is_some_and(|credit| !ctx.owns_ledger(&credit.ledger))
        {
            return Err(crate::Error::invalid(
                "incompatible materialized append input",
            ));
        }
        let source_capacity = super::algosvc::table_capacity(&source.table)?;
        if source
            .fast
            .as_ref()
            .is_some_and(|credit| credit.bytes() < source_capacity)
            || source
                .lease
                .as_ref()
                .is_some_and(|credit| credit.bytes < source_capacity)
        {
            return Err(crate::Error::invalid(
                "materialized append source exceeds its actual credit",
            ));
        }
        let old_len = self.table.len;
        let len = old_len
            .checked_add(source.table.len)
            .ok_or_else(|| crate::Error::invalid("materialized append rows overflow"))?;
        ctx.check_output_rows(len)?;
        self.reserve_rows(ctx, len)?;
        let copied = (|| {
            for (destination, source) in self.table.cols.iter_mut().zip(&source.table.cols) {
                for part in source.chunks(1024) {
                    ctx.check()?;
                    destination.extend_from_slice(part);
                    #[cfg(test)]
                    crate::algo::memory::probe::at(crate::algo::memory::probe::Point::Work(
                        "materialized-append-chunk",
                        part.len(),
                    ));
                }
            }
            ctx.check()
        })();
        if let Err(error) = copied {
            for column in &mut self.table.cols {
                column.truncate(old_len);
            }
            return Err(error);
        }
        self.table.len = len;
        self.table.sorted.clear();
        Ok(())
    }

    pub(super) fn has_credit(&self) -> bool {
        self.fast.is_some() || self.lease.is_some()
    }

    /// Slice paid backing in place. The original capacities and their actual
    /// owner stay together; no unadmitted replacement columns overlap the input.
    pub(super) fn slice(
        mut self,
        ctx: &'ctx super::ctx::Ctx,
        offset: usize,
        limit: Option<usize>,
    ) -> crate::Result<Self> {
        ctx.check()?;
        if self.fast.is_none() && self.lease.is_none() {
            self.table = self.table.slice(offset, limit);
            return Ok(self);
        }
        if self
            ._ctx
            .as_ref()
            .is_some_and(|owner| !std::ptr::eq(owner.as_ref(), ctx))
            || (self.fast.is_some() && self.lease.is_some())
            || self
                .fast
                .as_ref()
                .is_some_and(|credit| !credit.belongs_to(ctx))
            || self
                .lease
                .as_ref()
                .is_some_and(|credit| !ctx.owns_ledger(&credit.ledger))
        {
            return Err(crate::Error::invalid("foreign materialized slice owner"));
        }
        let capacity = super::algosvc::table_capacity(&self.table)?;
        if self
            .fast
            .as_ref()
            .is_some_and(|credit| credit.bytes() < capacity)
            || self
                .lease
                .as_ref()
                .is_some_and(|credit| credit.bytes < capacity)
        {
            return Err(crate::Error::invalid("slice exceeds actual input credit"));
        }
        self.table = Self::slice_columns(ctx, self.table, offset, limit)?;
        Ok(self)
    }

    pub(super) fn slice_paid(
        ctx: &super::ctx::Ctx,
        table: Table,
        credit: &super::ctx::Charge<'_>,
        offset: usize,
        limit: Option<usize>,
    ) -> crate::Result<Table> {
        ctx.check()?;
        if !credit.belongs_to(ctx) || credit.bytes() < super::algosvc::table_capacity(&table)? {
            return Err(crate::Error::invalid("slice lacks actual input credit"));
        }
        Self::slice_columns(ctx, table, offset, limit)
    }

    fn slice_columns(
        ctx: &super::ctx::Ctx,
        mut table: Table,
        offset: usize,
        limit: Option<usize>,
    ) -> crate::Result<Table> {
        let start = offset.min(table.len);
        let end = limit.map_or(table.len, |n| start.saturating_add(n).min(table.len));
        for column in &mut table.cols {
            ctx.check()?;
            // Id is Copy. Moving to a lower address cannot grow this allocation.
            column.copy_within(start..end, 0);
            column.truncate(end - start);
        }
        table.len = end - start;
        Ok(table)
    }

    pub(crate) fn project(
        mut self,
        ctx: &'ctx super::ctx::Ctx,
        vars: &[VarId],
    ) -> crate::Result<Self> {
        // A table that already has these columns in this order keeps its backing and
        // its owner unchanged, as a paid projection would after moving them twice.
        if self.table.vars == vars {
            return Ok(self);
        }
        let held = ctx.charge(0)?;
        let kind = self.adopt_input(ctx, &held)?;
        if kind == InputCredit::Plain {
            return Ok(Self::plain(self.table.project(vars)));
        }
        let table = Self::project_paid(ctx, self.table, &held, vars)?;
        let mut output = Self::with_fast(table, held)?;
        if kind == InputCredit::Lease {
            let credit = output.fast.as_ref().expect("actual projected owner");
            output.lease = Some(credit.output_lease(credit.bytes())?);
            output.fast.take();
        }
        Ok(output)
    }

    pub(super) fn into_retained(
        self,
        ctx: &std::sync::Arc<super::ctx::Ctx>,
    ) -> crate::Result<RetainedMaterialized> {
        let foreign_context = self
            ._ctx
            .as_ref()
            .is_some_and(|owner| !std::sync::Arc::ptr_eq(owner, ctx));
        if foreign_context
            || (self.fast.is_some() && self.lease.is_some())
            || self
                .fast
                .as_ref()
                .is_some_and(|credit| !credit.belongs_to(ctx))
        {
            return Err(crate::Error::invalid("foreign final materialization owner"));
        }
        let capacity = super::algosvc::table_capacity(&self.table)?;
        if self
            .fast
            .as_ref()
            .is_some_and(|credit| credit.bytes() < capacity)
        {
            return Err(crate::Error::invalid(
                "final table exceeds prepaid fast credit",
            ));
        }
        if let Some(lease) = &self.lease
            && (!ctx.owns_ledger(&lease.ledger)
                || lease.service.is_some()
                || lease.bytes < capacity)
        {
            return Err(crate::Error::invalid(
                "foreign or active-service final table credit",
            ));
        }
        let Self {
            table, fast, lease, ..
        } = self;
        let credit = match fast {
            Some(credit) => Some(FinalTableCredit::Fast {
                _credit: credit.into_output(ctx),
            }),
            None => lease.map(|credit| FinalTableCredit::Lease { _credit: credit }),
        };
        Ok(RetainedMaterialized {
            table,
            _ctx: ctx.clone(),
            credit,
        })
    }

    /// Project an already paid input. Metadata overlaps the original backing;
    /// retained columns move without copying or releasing their actual credit.
    pub(super) fn project_paid(
        ctx: &super::ctx::Ctx,
        mut source: Table,
        credit: &super::ctx::Charge<'_>,
        vars: &[VarId],
    ) -> crate::Result<Table> {
        use crate::algo::memory::{bytes, sum};
        ctx.check()?;
        if !credit.belongs_to(ctx) || credit.bytes() < super::algosvc::table_capacity(&source)? {
            return Err(crate::Error::invalid(
                "projection lacks actual input credit",
            ));
        }
        if vars == source.vars {
            return Ok(source);
        }
        if vars.iter().enumerate().any(|(i, v)| vars[..i].contains(v)) {
            return Err(crate::Error::invalid("duplicate paid projection variable"));
        }
        let sorted_len = source
            .sorted
            .iter()
            .take_while(|v| vars.contains(v))
            .count();
        let missing = vars.iter().filter(|v| source.col_of(**v).is_none()).count();
        let cells = missing
            .checked_mul(source.len)
            .ok_or_else(|| crate::Error::invalid("projection cells overflow"))?;
        let metadata = sum(&[
            128,
            bytes::<VarId>(vars.len())?,
            bytes::<Vec<Id>>(vars.len())?,
            bytes::<VarId>(sorted_len)?,
            bytes::<Id>(cells)?,
            if vars.is_empty() {
                bytes::<Id>(source.len)?
            } else {
                0
            },
        ])?;
        let mut additional = Some(ctx.charge(metadata)?);
        let projected = Self::copied(ctx, "eager-project-vars", vars)?;
        let mut columns = Self::allocate("eager-project-columns", vars.len())?;
        let mut sorted = Self::allocate("eager-project-sorted", sorted_len)?;
        sorted.extend_from_slice(&source.sorted[..sorted_len]);
        for &var in vars {
            ctx.check()?;
            match source.col_of(var) {
                Some(c) => columns.push(std::mem::take(&mut source.cols[c])),
                None => {
                    let mut column = Self::allocate("eager-project-missing", source.len)?;
                    for start in (0..source.len).step_by(1024) {
                        ctx.check()?;
                        column.resize(start.saturating_add(1024).min(source.len), Id::UNDEF);
                    }
                    columns.push(column);
                }
            }
        }
        let output = Table {
            vars: projected,
            cols: columns,
            len: source.len,
            sorted,
        };
        credit.adopt_charge(&mut additional)?;
        drop(source);
        let retained = super::algosvc::table_capacity(&output)?;
        if retained > credit.bytes() {
            return Err(crate::Error::invalid(
                "projected backing exceeds its prepaid credit",
            ));
        }
        credit.resize(retained)?;
        Ok(output)
    }

    fn allocate<T>(name: &'static str, capacity: usize) -> crate::Result<Vec<T>> {
        #[cfg(test)]
        {
            use crate::algo::memory::probe;
            probe::at(probe::Point::Allocation(name, capacity));
            if probe::fails(name) {
                return Err(crate::Error::invalid("injected eager allocation failure"));
            }
        }
        let _ = name;
        let mut values = Vec::new();
        values
            .try_reserve_exact(capacity)
            .map_err(|e| crate::Error::invalid(format!("eager allocation failed: {e}")))?;
        if std::mem::size_of::<T>() != 0 && values.capacity() != capacity {
            return Err(crate::Error::invalid(
                "unsupported eager allocation capacity",
            ));
        }
        Ok(values)
    }

    fn copied<T: Copy>(
        ctx: &super::ctx::Ctx,
        name: &'static str,
        values: &[T],
    ) -> crate::Result<Vec<T>> {
        let mut copy = Self::allocate(name, values.len())?;
        // Callers check before they copy, so the copy polls between its chunks only.
        for (index, part) in values.chunks(1024).enumerate() {
            if index > 0 {
                ctx.check()?;
            }
            copy.extend_from_slice(part);
        }
        Ok(copy)
    }

    /// Clone only after paying the receiving allocation, independently of its source.
    /// It stays out of line, so a VALUES operator's run does not spread its checks
    /// across the executor's code.
    #[inline(never)]
    pub(super) fn clone_fast(ctx: &'ctx super::ctx::Ctx, source: &Table) -> crate::Result<Self> {
        use crate::algo::memory::{bytes, sum};
        ctx.check()?;
        ctx.check_output_rows(source.len)?;
        if source.vars.len() != source.cols.len()
            || source.cols.iter().any(|c| c.len() != source.len)
        {
            return Err(crate::Error::invalid("inconsistent eager source table"));
        }
        let data = source
            .cols
            .iter()
            .try_fold(0, |n, c| sum(&[n, bytes::<Id>(c.len())?]))?;
        let size = sum(&[
            128,
            bytes::<VarId>(source.vars.len())?,
            bytes::<Vec<Id>>(source.cols.len())?,
            bytes::<VarId>(source.sorted.len())?,
            data,
            if source.width() == 0 {
                bytes::<Id>(source.len)?
            } else {
                0
            },
        ])?;
        let credit = ctx.charge(size)?;
        let vars = Self::copied(ctx, "eager-vars", &source.vars)?;
        let mut cols = Self::allocate("eager-columns", source.cols.len())?;
        for column in &source.cols {
            cols.push(Self::copied(ctx, "eager-column", column)?);
        }
        let sorted = Self::copied(ctx, "eager-sorted", &source.sorted)?;
        Self::with_fast(
            Table {
                vars,
                cols,
                len: source.len,
                sorted,
            },
            credit,
        )
    }

    pub(crate) fn empty_fast(
        ctx: &'ctx super::ctx::Ctx,
        vars: &[VarId],
        sorted: &[VarId],
    ) -> crate::Result<Self> {
        use crate::algo::memory::{bytes, sum};
        ctx.check()?;
        let credit = ctx.charge(sum(&[
            128,
            bytes::<VarId>(vars.len())?,
            bytes::<Vec<Id>>(vars.len())?,
            bytes::<VarId>(sorted.len())?,
        ])?)?;
        let vars = Self::copied(ctx, "eager-vars", vars)?;
        let mut cols = Self::allocate("eager-columns", vars.len())?;
        for _ in &vars {
            cols.push(Vec::new());
        }
        let sorted = Self::copied(ctx, "eager-sorted", sorted)?;
        Self::with_fast(
            Table {
                vars,
                cols,
                len: 0,
                sorted,
            },
            credit,
        )
    }

    /// Admit the old and replacement column simultaneously, then retire the old Vec.
    pub(super) fn reserve_rows(&mut self, ctx: &super::ctx::Ctx, rows: usize) -> crate::Result<()> {
        use crate::algo::memory::{bytes, sum};
        ctx.check()?;
        let credit = self
            .fast
            .as_ref()
            .filter(|c| c.belongs_to(ctx))
            .ok_or_else(|| crate::Error::invalid("missing actual fast table owner"))?;
        if self.table.width() == 0 {
            let size = sum(&[
                128,
                bytes::<VarId>(self.table.vars.capacity())?,
                bytes::<Vec<Id>>(self.table.cols.capacity())?,
                bytes::<VarId>(self.table.sorted.capacity())?,
                bytes::<Id>(rows.max(self.table.len))?,
            ])?;
            return credit.resize(size);
        }
        for column in &mut self.table.cols {
            if rows <= column.capacity() {
                continue;
            }
            let capacity = if column.capacity() == 0 {
                rows
            } else {
                column
                    .capacity()
                    .checked_mul(2)
                    .map(|n| n.max(rows))
                    .ok_or_else(|| crate::Error::invalid("eager column growth overflow"))?
            };
            let mut additional = Some(ctx.charge(bytes::<Id>(capacity)?)?);
            let mut replacement = Self::allocate("eager-column-grow", capacity)?;
            for part in column.chunks(1024) {
                ctx.check()?;
                replacement.extend_from_slice(part);
            }
            credit.adopt_charge(&mut additional)?;
            let old = std::mem::replace(column, replacement);
            let retired = bytes::<Id>(old.capacity())?;
            drop(old);
            #[cfg(test)]
            crate::algo::memory::probe::at(crate::algo::memory::probe::Point::Work(
                "eager-column-old-dropped",
                capacity,
            ));
            credit.resize(credit.bytes() - retired)?;
        }
        Ok(())
    }

    pub(super) fn into_fast_parts(self) -> crate::Result<(Table, super::ctx::Charge<'ctx>)> {
        if self.fast.is_none() || self.lease.is_some() || self._ctx.is_some() {
            return Err(crate::Error::invalid(
                "not an exclusively fast materialization",
            ));
        }
        let Self { table, fast, .. } = self;
        Ok((table, fast.expect("checked actual fast owner")))
    }

    /// Explicit legacy owning conversion. Prepared fast output must use its ArcCtx.
    pub(super) fn into_lease_owned(
        mut self,
        ctx: &super::ctx::Ctx,
    ) -> crate::Result<Materialized<'static>> {
        if let Some(credit) = &self.fast {
            if !credit.belongs_to(ctx) || self.lease.is_some() || self._ctx.is_some() {
                return Err(crate::Error::invalid("foreign fast materialization owner"));
            }
            self.lease = Some(credit.output_lease(credit.bytes())?);
            self.fast.take();
        }
        self.into_unborrowed()
    }

    /// Leave the borrowed execution scope only when no fast-account borrow remains.
    /// Move the physical backing and its existing owners together, without recharging.
    pub(super) fn into_unborrowed(self) -> crate::Result<Materialized<'static>> {
        if self.fast.is_some() {
            return Err(crate::Error::invalid(
                "borrowed materialized credit cannot leave its query context",
            ));
        }
        let Self {
            table,
            _ctx,
            fast: _,
            lease,
        } = self;
        Ok(Materialized {
            table,
            _ctx,
            fast: None,
            lease,
        })
    }

    pub(super) fn plain(table: Table) -> Self {
        Self::with_lease(table, None)
    }

    pub(super) fn with_lease(
        table: Table,
        lease: Option<super::query_memory::QueryMemoryLease>,
    ) -> Self {
        Self {
            table,
            _ctx: None,
            fast: None,
            lease,
        }
    }

    pub(super) fn with_owned(table: Table, charge: super::ctx::OwnedCharge) -> crate::Result<Self> {
        let bytes = charge.bytes();
        let (ctx, lease) = charge.into_parts();
        // Protect the physical drop order before any checked validation can refuse.
        let owned = Self {
            table,
            _ctx: Some(ctx),
            fast: None,
            lease: Some(lease),
        };
        if bytes < super::algosvc::table_capacity(&owned.table)? {
            return Err(crate::Error::invalid(
                "materialized table exceeds its owned credit",
            ));
        }
        Ok(owned)
    }

    /// The caller admitted this storage before allocating it on the fast account.
    pub(super) fn with_fast(table: Table, charge: super::ctx::Charge<'ctx>) -> crate::Result<Self> {
        let bytes = charge.bytes();
        let owned = Self {
            table,
            _ctx: None,
            fast: Some(charge),
            lease: None,
        };
        if bytes < super::algosvc::table_capacity(&owned.table)? {
            return Err(crate::Error::invalid(
                "materialized table exceeds its fast credit",
            ));
        }
        Ok(owned)
    }

    /// Move an actual input owner before its table enters an eager kernel.
    pub(super) fn adopt_input(
        &mut self,
        ctx: &super::ctx::Ctx,
        held: &super::ctx::Charge<'_>,
    ) -> crate::Result<InputCredit> {
        if !held.belongs_to(ctx)
            || self
                ._ctx
                .as_ref()
                .is_some_and(|owner| !std::ptr::eq(std::sync::Arc::as_ptr(owner), ctx))
            || (self.fast.is_some() && self.lease.is_some())
        {
            return Err(crate::Error::invalid(
                "materialized input belongs to another query context",
            ));
        }
        if self.fast.is_some() {
            held.adopt_charge(&mut self.fast)?;
            Ok(InputCredit::Fast)
        } else if self.lease.is_some() {
            held.adopt(&mut self.lease)?;
            Ok(InputCredit::Lease)
        } else {
            Ok(InputCredit::Plain)
        }
    }
}

#[cfg(test)]
mod materialized_credit_tests {
    use super::*;
    use crate::sparql::ctx::Ctx;
    use crate::store::{Store, StoreOptions};

    fn paid_spare_table(ctx: &Ctx) -> Materialized<'_> {
        // Admit the exact outer and spare column capacities before allocating them.
        let credit = ctx.charge(128 + 4 * 4 + 4 * 24 + 8 * 8 + 4 * 4).unwrap();
        let mut vars = Vec::with_capacity(4);
        vars.push(0);
        let mut column = Vec::with_capacity(8);
        column.push(Id::UNDEF);
        let mut cols = Vec::with_capacity(4);
        cols.push(column);
        let mut sorted = Vec::with_capacity(4);
        sorted.push(0);
        Materialized::with_fast(
            Table {
                vars,
                cols,
                len: 1,
                sorted,
            },
            credit,
        )
        .unwrap()
    }

    #[test]
    fn spare_table_input_moves_actual_credit_without_reserving_again() {
        let store = Store::in_memory(StoreOptions::default());
        let mut ctx = Ctx::new(store.snapshot());
        ctx.mem_limit = 320;
        let mut input = paid_spare_table(&ctx);
        assert_eq!(ctx.live_bytes(), 320);
        let held = ctx.charge(0).unwrap();
        assert!(matches!(
            input.adopt_input(&ctx, &held).unwrap(),
            InputCredit::Fast
        ));
        assert!(input.fast.is_none());
        assert_eq!(held.bytes(), 320);
        assert_eq!(ctx.live_bytes(), 320);
        drop(input);
        assert_eq!(ctx.live_bytes(), 320, "the operator still owns the credit");
        drop(held);
        assert_eq!(ctx.live_bytes(), 0);
    }

    #[test]
    fn paid_append_pays_receiving_copy_overlap_before_allocating_and_keeps_source() {
        use crate::algo::memory::probe;
        use std::{cell::Cell, rc::Rc, sync::Arc};
        let store = Store::in_memory(StoreOptions::default());
        for short in [false, true] {
            let mut ctx = Ctx::new(store.snapshot());
            ctx.mem_limit = 484 - u64::from(short);
            let ctx = Arc::new(ctx);
            let source = paid_spare_table(&ctx);
            let mut destination = Materialized::empty_fast(&ctx, &[0], &[]).unwrap();
            assert_eq!(ctx.live_bytes(), 476);
            let observed = Rc::new(Cell::new(0));
            let seen = observed.clone();
            let owner = ctx.clone();
            let hook = probe::install(
                move |point| {
                    if let probe::Point::Allocation("eager-column-grow", 1) = point {
                        assert_eq!(
                            owner.live_bytes(),
                            484,
                            "source320 and complete destination164 coexist"
                        );
                        seen.set(seen.get() + 1);
                    }
                },
                None,
            );
            let result = destination.append_copy(&ctx, &source);
            if short {
                assert!(matches!(result, Err(crate::Error::BudgetExceeded(_))));
                assert_eq!(destination.table.len, 0);
                assert_eq!(ctx.live_bytes(), 476);
                assert_eq!(observed.get(), 0);
            } else {
                result.unwrap();
                assert_eq!(destination.table.cols[0], source.table.cols[0]);
                assert_eq!(ctx.live_bytes(), 484);
                assert_eq!(observed.get(), 1);
            }
            assert_eq!(source.table.cols[0].capacity(), 8);
            drop(hook);
            drop(source);
            assert_eq!(ctx.live_bytes(), if short { 156 } else { 164 });
            drop(destination);
            assert_eq!(ctx.live_bytes(), 0);
        }
    }

    #[test]
    fn paid_append_refuses_foreign_lease_without_promoting_and_rolls_back_partial_rows() {
        use crate::algo::memory::probe;
        use std::{cell::Cell, rc::Rc, sync::Arc};
        let store = Store::in_memory(StoreOptions::default());
        let foreign = Ctx::new(store.snapshot());
        let source = paid_spare_table(&foreign)
            .into_lease_owned(&foreign)
            .unwrap();
        let ctx = Arc::new(Ctx::new(store.snapshot()));
        let mut destination = Materialized::empty_fast(&ctx, &[0], &[]).unwrap();
        let before = super::super::query_memory::LEDGER_OPS.with(|n| n.get());
        assert!(destination.append_copy(&ctx, &source).is_err());
        assert_eq!(
            super::super::query_memory::LEDGER_OPS.with(|n| n.get()),
            before
        );
        assert_eq!(ctx.live_bytes(), 156);
        drop(source);
        let table = Table {
            vars: vec![0],
            cols: vec![vec![Id::UNDEF; 2048]],
            len: 2048,
            sorted: Vec::new(),
        };
        let source = Materialized::clone_fast(&ctx, &table).unwrap();
        let polls = Rc::new(Cell::new(0usize));
        let seen = polls.clone();
        let owner = ctx.clone();
        let hook = probe::install(
            move |point| {
                if let probe::Point::Work("materialized-append-chunk", _) = point {
                    seen.set(seen.get() + 1);
                    if seen.get() == 1 {
                        owner
                            .cancel
                            .store(true, std::sync::atomic::Ordering::Release);
                    }
                }
            },
            None,
        );
        assert!(matches!(
            destination.append_copy(&ctx, &source),
            Err(crate::Error::Cancelled)
        ));
        assert_eq!(destination.table.len, 0);
        assert!(destination.table.cols[0].is_empty());
        assert_eq!(destination.table.cols[0].capacity(), 2048);
        assert_eq!(source.table.len, 2048);
        drop(hook);
        drop(source);
        drop(destination);
        assert_eq!(ctx.live_bytes(), 0);
    }

    #[test]
    fn paid_append_nonempty_destination_funds_old_new_and_source_overlap() {
        use crate::algo::memory::probe;
        use std::{cell::Cell, rc::Rc, sync::Arc};
        let store = Store::in_memory(StoreOptions::default());
        for short in [false, true] {
            let mut context = Ctx::new(store.snapshot());
            context.mem_limit = 344 - u64::from(short);
            let ctx = Arc::new(context);
            let left = Table {
                vars: vec![0],
                cols: vec![vec![Id::from_i64(1).unwrap()]],
                len: 1,
                sorted: Vec::new(),
            };
            let right = Table {
                vars: vec![0],
                cols: vec![vec![Id::from_i64(2).unwrap()]],
                len: 1,
                sorted: Vec::new(),
            };
            let mut destination = Materialized::clone_fast(&ctx, &left).unwrap();
            let source = Materialized::clone_fast(&ctx, &right).unwrap();
            assert_eq!(ctx.live_bytes(), 328);
            let original = destination.table.cols[0].as_ptr();
            let source_pointer = source.table.cols[0].as_ptr();
            let seen = Rc::new(Cell::new(0));
            let observed = seen.clone();
            let account = ctx.clone();
            let hook = probe::install(
                move |point| {
                    if let probe::Point::Allocation("eager-column-grow", 2) = point {
                        assert_eq!(
                            account.live_bytes(),
                            344,
                            "real nonempty destination164+replacement16+source164 overlap"
                        );
                        observed.set(observed.get() + 1);
                    }
                },
                None,
            );
            let result = destination.append_copy(&ctx, &source);
            drop(hook);
            if short {
                assert!(
                    matches!(result, Err(crate::Error::BudgetExceeded(b)) if b.kind == crate::BudgetKind::Memory)
                );
                assert_eq!(seen.get(), 0);
                assert_eq!(destination.table.cols[0].as_ptr(), original);
                assert_eq!(destination.table.cols[0], left.cols[0]);
                assert_eq!(destination.table.len, 1);
                assert_eq!(ctx.live_bytes(), 328);
            } else {
                result.unwrap();
                assert_eq!(seen.get(), 1);
                assert_eq!(
                    destination.table.cols[0],
                    [Id::from_i64(1).unwrap(), Id::from_i64(2).unwrap()]
                );
                assert_eq!(destination.table.cols[0].capacity(), 2);
                assert_eq!(ctx.live_bytes(), 336);
            }
            assert_eq!(source.table.cols[0].as_ptr(), source_pointer);
            assert_eq!(source.table.cols[0], right.cols[0]);
            drop(source);
            drop(destination);
            assert_eq!(ctx.live_bytes(), 0);
        }
    }

    #[test]
    fn paid_append_cancellation_rolls_back_both_fully_and_partly_copied_columns() {
        use crate::algo::memory::probe;
        use std::{cell::Cell, rc::Rc, sync::Arc};
        let store = Store::in_memory(StoreOptions::default());
        let ctx = Arc::new(Ctx::new(store.snapshot()));
        let old = Table {
            vars: vec![0, 1],
            cols: vec![
                vec![Id::from_i64(10).unwrap()],
                vec![Id::from_i64(20).unwrap()],
            ],
            len: 1,
            sorted: vec![0, 1],
        };
        let input = Table {
            vars: vec![0, 1],
            cols: vec![
                vec![Id::from_i64(1).unwrap(); 2048],
                vec![Id::from_i64(2).unwrap(); 2048],
            ],
            len: 2048,
            sorted: Vec::new(),
        };
        let mut destination = Materialized::clone_fast(&ctx, &old).unwrap();
        let source = Materialized::clone_fast(&ctx, &input).unwrap();
        let copies = Rc::new(Cell::new(0));
        let seen = copies.clone();
        let cancel = ctx.cancel.clone();
        let hook = probe::install(
            move |point| {
                if let probe::Point::Work("materialized-append-chunk", 1024) = point {
                    seen.set(seen.get() + 1);
                    if seen.get() == 3 {
                        cancel.store(true, std::sync::atomic::Ordering::Relaxed);
                    }
                }
            },
            None,
        );
        assert!(matches!(
            destination.append_copy(&ctx, &source),
            Err(crate::Error::Cancelled)
        ));
        drop(hook);
        assert_eq!(
            copies.get(),
            3,
            "column0 completed before column1 partially copied"
        );
        assert_eq!(destination.table.len, 1);
        assert_eq!(destination.table.cols, old.cols);
        assert_eq!(destination.table.sorted, old.sorted);
        assert!(
            destination
                .table
                .cols
                .iter()
                .all(|column| column.capacity() == 2049)
        );
        assert_eq!(source.table.cols, input.cols);
        assert_eq!(source.table.len, 2048);
        assert_eq!(
            ctx.live_bytes(),
            destination.fast.as_ref().unwrap().bytes() + source.fast.as_ref().unwrap().bytes()
        );
        drop(destination);
        assert_eq!(ctx.live_bytes(), source.fast.as_ref().unwrap().bytes());
        drop(source);
        assert_eq!(ctx.live_bytes(), 0);
    }

    #[test]
    fn paid_slice_keeps_original_allocation_and_capacity_credit_at_limit() {
        let store = Store::in_memory(StoreOptions::default());
        for lease in [false, true] {
            let mut ctx = Ctx::new(store.snapshot());
            ctx.mem_limit = 320;
            let mut input = paid_spare_table(&ctx);
            input.table.cols[0].extend([Id::from_i64(1).unwrap(), Id::from_i64(2).unwrap()]);
            input.table.len = 3;
            let pointer = input.table.cols[0].as_ptr();
            if lease {
                input = input.into_lease_owned(&ctx).unwrap();
            }
            let output = input.slice(&ctx, 1, Some(1)).unwrap();
            assert_eq!(output.table.cols[0], [Id::from_i64(1).unwrap()]);
            assert_eq!(output.table.cols[0].as_ptr(), pointer);
            assert_eq!(output.table.cols[0].capacity(), 8);
            assert_eq!(ctx.live_bytes(), 320);
            assert_eq!(output.lease.is_some(), lease);
            drop(output);
            assert_eq!(ctx.live_bytes(), 0);
        }
    }

    #[test]
    fn paid_slice_preserves_zero_width_rows_and_refuses_foreign_or_cancelled_context() {
        use std::sync::atomic::Ordering;
        let store = Store::in_memory(StoreOptions::default());
        let source = Ctx::new(store.snapshot());
        let receiving = Ctx::new(store.snapshot());
        let input = paid_spare_table(&source);
        assert!(input.slice(&receiving, 0, Some(1)).is_err());
        assert_eq!(source.live_bytes(), 0);
        let input = paid_spare_table(&source);
        source.cancel.store(true, Ordering::Release);
        assert!(matches!(
            input.slice(&source, 0, Some(1)),
            Err(crate::Error::Cancelled)
        ));
        assert_eq!(source.live_bytes(), 0);
        source.cancel.store(false, Ordering::Release);
        let table = Table {
            len: 7,
            ..Table::default()
        };
        let input = Materialized::clone_fast(&source, &table).unwrap();
        let credit = source.live_bytes();
        let output = input.slice(&source, 3, Some(2)).unwrap();
        assert_eq!(output.table.len, 2);
        assert_eq!(output.table.width(), 0);
        assert_eq!(source.live_bytes(), credit);
        drop(output);
        assert_eq!(source.live_bytes(), 0);
    }

    #[test]
    fn foreign_operator_refusal_keeps_the_actual_input_and_credit_together() {
        let store = Store::in_memory(StoreOptions::default());
        let source = Ctx::new(store.snapshot());
        let receiving = Ctx::new(store.snapshot());
        let mut input = paid_spare_table(&source);
        let held = receiving.charge(0).unwrap();
        assert!(input.adopt_input(&receiving, &held).is_err());
        assert_eq!(input.table.cols[0].capacity(), 8);
        assert_eq!(input.fast.as_ref().unwrap().bytes(), 320);
        assert_eq!(source.live_bytes(), 320);
        assert_eq!(receiving.live_bytes(), 0);
        drop(input);
        assert_eq!(source.live_bytes(), 0);
    }

    #[test]
    fn undersized_fast_credit_refuses_and_retires_the_protected_partial_backing() {
        let store = Store::in_memory(StoreOptions::default());
        let ctx = Ctx::new(store.snapshot());
        let credit = ctx.charge(127).unwrap();
        assert!(Materialized::with_fast(Table::default(), credit).is_err());
        assert_eq!(ctx.live_bytes(), 0);
    }

    #[test]
    fn borrowed_fast_backing_cannot_escape_the_context_lifetime() {
        let store = Store::in_memory(StoreOptions::default());
        let mut ctx = Ctx::new(store.snapshot());
        ctx.mem_limit = 320;
        let input = paid_spare_table(&ctx);
        assert_eq!(ctx.live_bytes(), 320);
        assert!(input.into_unborrowed().is_err());
        assert_eq!(ctx.live_bytes(), 0);
    }

    #[test]
    fn unborrowed_output_moves_its_real_owner_and_credit_at_the_limit() {
        use std::sync::Arc;
        let store = Store::in_memory(StoreOptions::default());
        let mut ctx = Ctx::new(store.snapshot());
        ctx.mem_limit = 128;
        let ctx = Arc::new(ctx);
        let credit = super::super::ctx::OwnedCharge::new(&ctx, 128).unwrap();
        let input = Materialized::with_owned(Table::default(), credit).unwrap();
        let output = input.into_unborrowed().unwrap();
        assert_eq!(ctx.live_bytes(), 128);
        assert_eq!(output.lease.as_ref().unwrap().bytes, 128);
        let owner = Arc::downgrade(&ctx);
        drop(ctx);
        assert_eq!(owner.upgrade().unwrap().live_bytes(), 128);
        assert_eq!(output.table.len, 0);
        drop(output);
        assert!(owner.upgrade().is_none());
    }

    #[test]
    fn fast_column_growth_pays_old_and_new_before_allocation() {
        use crate::algo::memory::probe;
        use std::{cell::Cell, rc::Rc, sync::Arc};
        for short in [false, true] {
            let store = Store::in_memory(StoreOptions::default());
            let mut ctx = Ctx::new(store.snapshot());
            ctx.mem_limit = 252 - u64::from(short);
            let ctx = Arc::new(ctx);
            let mut output = Materialized::empty_fast(&ctx, &[0], &[]).unwrap();
            output.reserve_rows(&ctx, 4).unwrap();
            output.table.cols[0].push(Id::UNDEF);
            output.table.len = 1;
            assert_eq!(ctx.live_bytes(), 188);
            let seen = Rc::new(Cell::new(false));
            let observed = seen.clone();
            let owner = ctx.clone();
            let hook = probe::install(
                move |point| {
                    if let probe::Point::Allocation("eager-column-grow", 8) = point {
                        assert_eq!(owner.live_bytes(), 252, "old32 and new64 coexist");
                        observed.set(true);
                    }
                },
                None,
            );
            let result = output.reserve_rows(&ctx, 5);
            if short {
                assert!(matches!(result, Err(crate::Error::BudgetExceeded(_))));
                assert!(!seen.get());
                assert_eq!(output.table.cols[0].capacity(), 4);
                assert_eq!(ctx.live_bytes(), 188);
            } else {
                result.unwrap();
                assert!(seen.get());
                assert_eq!(output.table.cols[0].capacity(), 8);
                assert_eq!(ctx.live_bytes(), 220);
            }
            assert_eq!(output.table.cols[0], [Id::UNDEF]);
            drop(hook);
            drop(output);
            assert_eq!(ctx.live_bytes(), 0);
        }
    }

    #[test]
    fn failed_fast_replacement_keeps_original_backing_and_credit() {
        use crate::algo::memory::probe;
        let store = Store::in_memory(StoreOptions::default());
        let ctx = Ctx::new(store.snapshot());
        let mut output = Materialized::empty_fast(&ctx, &[0], &[]).unwrap();
        output.reserve_rows(&ctx, 4).unwrap();
        output.table.cols[0].push(Id::UNDEF);
        output.table.len = 1;
        let hook = probe::install(|_| {}, Some("eager-column-grow"));
        assert!(output.reserve_rows(&ctx, 5).is_err());
        assert_eq!(output.table.cols[0], [Id::UNDEF]);
        assert_eq!(output.table.cols[0].capacity(), 4);
        assert_eq!(ctx.live_bytes(), 188);
        drop(hook);
        drop(output);
        assert_eq!(ctx.live_bytes(), 0);
    }

    #[test]
    fn paid_projection_pays_metadata_overlap_and_missing_columns() {
        use crate::algo::memory::probe;
        use std::{cell::Cell, rc::Rc, sync::Arc};
        for short in [false, true] {
            let store = Store::in_memory(StoreOptions::default());
            let mut ctx = Ctx::new(store.snapshot());
            ctx.mem_limit = 420 - u64::from(short);
            let ctx = Arc::new(ctx);
            let mut input = Materialized::empty_fast(&ctx, &[0], &[]).unwrap();
            input.reserve_rows(&ctx, 8).unwrap();
            input.table.cols[0].extend([Id::UNDEF, Id(7)]);
            input.table.len = 2;
            let held = ctx.charge(0).unwrap();
            input.adopt_input(&ctx, &held).unwrap();
            let observed = Rc::new(Cell::new(false));
            let seen = observed.clone();
            let owner = ctx.clone();
            let hook = probe::install(
                move |point| {
                    if let probe::Point::Allocation("eager-project-vars", _) = point {
                        assert_eq!(owner.live_bytes(), 420, "old220 and new200 coexist");
                        seen.set(true);
                    }
                },
                None,
            );
            let output = Materialized::project_paid(&ctx, input.table, &held, &[0, 1]);
            if short {
                assert!(matches!(output, Err(crate::Error::BudgetExceeded(_))));
                assert!(!observed.get());
                assert_eq!(ctx.live_bytes(), 220);
                drop(held);
            } else {
                let output = Materialized::with_fast(output.unwrap(), held).unwrap();
                assert!(observed.get());
                assert_eq!(output.table.cols[0], [Id::UNDEF, Id(7)]);
                assert_eq!(output.table.cols[0].capacity(), 8);
                assert_eq!(output.table.cols[1], [Id::UNDEF, Id::UNDEF]);
                assert_eq!(ctx.live_bytes(), 264);
                drop(output);
            }
            drop(hook);
            assert_eq!(ctx.live_bytes(), 0);
        }
    }

    #[test]
    fn failed_paid_projection_keeps_credit_until_partial_backing_retires() {
        use crate::algo::memory::probe;
        let store = Store::in_memory(StoreOptions::default());
        let ctx = Ctx::new(store.snapshot());
        let mut input = Materialized::empty_fast(&ctx, &[0], &[]).unwrap();
        input.reserve_rows(&ctx, 8).unwrap();
        input.table.cols[0].extend([Id::UNDEF, Id(7)]);
        input.table.len = 2;
        let held = ctx.charge(0).unwrap();
        input.adopt_input(&ctx, &held).unwrap();
        let hook = probe::install(|_| {}, Some("eager-project-missing"));
        assert!(Materialized::project_paid(&ctx, input.table, &held, &[0, 1]).is_err());
        assert_eq!(ctx.live_bytes(), 220);
        drop(hook);
        drop(held);
        assert_eq!(ctx.live_bytes(), 0);
    }

    #[test]
    fn zero_width_projection_retires_columns_before_credit_shrink() {
        let store = Store::in_memory(StoreOptions::default());
        let ctx = Ctx::new(store.snapshot());
        let mut input = Materialized::empty_fast(&ctx, &[0], &[]).unwrap();
        input.reserve_rows(&ctx, 8).unwrap();
        input.table.cols[0].extend([Id::UNDEF, Id(7)]);
        input.table.len = 2;
        let held = ctx.charge(0).unwrap();
        input.adopt_input(&ctx, &held).unwrap();
        let table = Materialized::project_paid(&ctx, input.table, &held, &[]).unwrap();
        let output = Materialized::with_fast(table, held).unwrap();
        assert!(output.table.cols.is_empty());
        assert_eq!(output.table.len, 2);
        assert_eq!(ctx.live_bytes(), 144);
        drop(output);
        assert_eq!(ctx.live_bytes(), 0);
    }

    #[test]
    fn final_fast_output_refuses_a_different_context_on_the_same_grant() {
        use std::sync::Arc;
        let store = Store::in_memory(StoreOptions::default());
        let pool = super::super::pool::QueryPool::new(1 << 20);
        let grant = Arc::new(pool.empty_grant());
        let mut first = Ctx::new(store.snapshot());
        first.set_grant(Some(grant.clone()));
        let mut second = Ctx::new(store.snapshot());
        second.set_grant(Some(grant.clone()));
        let first = Arc::new(first);
        let second = Arc::new(second);
        let input = paid_spare_table(&first);
        assert_eq!(first.live_bytes(), 320);
        assert!(input.into_retained(&second).is_err());
        assert_eq!(first.live_bytes(), 0);
        assert_eq!(second.live_bytes(), 0);
        drop(first);
        drop(second);
        assert_eq!(grant.used(), 0);
    }

    #[test]
    fn final_retained_guard_keeps_real_fast_context_without_ledger() {
        use std::sync::Arc;
        let store = Store::in_memory(StoreOptions::default());
        let mut ctx = Ctx::new(store.snapshot());
        ctx.mem_limit = 320;
        let ctx = Arc::new(ctx);
        super::super::query_memory::LEDGER_OPS.with(|n| n.set(0));
        let input = paid_spare_table(&ctx);
        let retained = input.into_retained(&ctx).unwrap();
        super::super::query_memory::LEDGER_OPS.with(|n| assert_eq!(n.get(), 0));
        let owner = Arc::downgrade(&ctx);
        drop(ctx);
        assert_eq!(retained.table.cols[0], [Id::UNDEF]);
        assert_eq!(owner.upgrade().unwrap().live_bytes(), 320);
        drop(retained);
        assert!(owner.upgrade().is_none());
    }
}

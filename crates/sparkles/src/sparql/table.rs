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

    /// Map from variable id to column (indexed by var id).
    pub fn var_map(&self, nvars: usize) -> Vec<Option<usize>> {
        let mut m = vec![None; nvars.max(self.vars.iter().map(|&v| v as usize + 1).max().unwrap_or(0))];
        for (c, &v) in self.vars.iter().enumerate() {
            m[v as usize] = Some(c);
        }
        m
    }

    /// Keep only rows where `keep[i]`.
    pub fn filter_rows(&mut self, keep: &[bool]) {
        for col in &mut self.cols {
            let mut i = 0;
            col.retain(|_| {
                let k = keep[i];
                i += 1;
                k
            });
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
        let end = limit.map_or(self.len, |l| (start + l).min(self.len));
        if start == 0 && end == self.len {
            return self;
        }
        for c in &mut self.cols {
            *c = c[start..end].to_vec();
        }
        self.len = end - start;
        self
    }

    /// Sort rows by the given columns (raw id order, UNDEF first).
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
            idx.par_sort_unstable_by_key(|&i| c[i]);
        } else {
            idx.par_sort_unstable_by(|&a, &b| {
                for &c in &cols {
                    match self.cols[c][a].cmp(&self.cols[c][b]) {
                        std::cmp::Ordering::Equal => continue,
                        o => return o,
                    }
                }
                std::cmp::Ordering::Equal
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

//! The linear operators: filter, extend (`BIND`), projection, union and `VALUES`. Each
//! applies to the change rows as it applies to the rows, and keeps no state.

use super::super::pass::Pass;
use super::super::zset::{Changes, Row};
use super::Node;
use crate::error::Result;
use crate::id::Id;
use oxrdf::Term;
use spargebra::algebra::Expression;

#[derive(Clone, Debug)]
pub struct Filter {
    pub inner: Node,
    pub expr: Expression,
}

impl Filter {
    fn apply(&self, p: &mut Pass, c: Changes) -> Result<Changes> {
        if c.is_empty() {
            return Ok(c);
        }
        let rows: Vec<&[Id]> = c.iter().map(|(r, _)| &r[..]).collect();
        let keep = p.eval(&self.inner.schema, &rows, &self.expr, true)?;
        Ok(c.into_iter()
            .zip(keep)
            .filter(|(_, k)| *k == Some(Id::from_bool(true)))
            .map(|(c, _)| c)
            .collect())
    }

    pub fn init(&mut self, p: &mut Pass) -> Result<Changes> {
        let c = self.inner.init(p)?;
        self.apply(p, c)
    }

    pub fn delta(&mut self, p: &mut Pass) -> Result<Changes> {
        let c = self.inner.delta(p)?;
        self.apply(p, c)
    }
}

#[derive(Clone, Debug)]
pub struct Extend {
    pub inner: Node,
    pub var: u32,
    pub expr: Expression,
}

impl Extend {
    fn apply(&self, p: &mut Pass, c: Changes) -> Result<Changes> {
        if c.is_empty() {
            return Ok(c);
        }
        let rows: Vec<&[Id]> = c.iter().map(|(r, _)| &r[..]).collect();
        let vals = p.eval(&self.inner.schema, &rows, &self.expr, false)?;
        Ok(c.into_iter()
            .zip(vals)
            .map(|((r, w), v)| {
                let mut row = r.into_vec();
                row.push(v.unwrap_or(Id::UNDEF));
                (row.into_boxed_slice(), w)
            })
            .collect())
    }

    pub fn init(&mut self, p: &mut Pass) -> Result<Changes> {
        let c = self.inner.init(p)?;
        self.apply(p, c)
    }

    pub fn delta(&mut self, p: &mut Pass) -> Result<Changes> {
        let c = self.inner.delta(p)?;
        self.apply(p, c)
    }
}

#[derive(Clone, Debug)]
pub struct Project {
    pub inner: Node,
    /// for each output column, its position in the input (`None`: never bound)
    pub cols: Vec<Option<usize>>,
}

impl Project {
    fn apply(&self, c: Changes) -> Changes {
        c.into_iter()
            .map(|(r, w)| {
                let row: Row = self
                    .cols
                    .iter()
                    .map(|c| c.map_or(Id::UNDEF, |i| r[i]))
                    .collect();
                (row, w)
            })
            .collect()
    }

    pub fn init(&mut self, p: &mut Pass) -> Result<Changes> {
        let c = self.inner.init(p)?;
        Ok(self.apply(c))
    }

    pub fn delta(&mut self, p: &mut Pass) -> Result<Changes> {
        let c = self.inner.delta(p)?;
        Ok(self.apply(c))
    }
}

#[derive(Clone, Debug)]
pub struct Union {
    pub left: Node,
    pub right: Node,
    /// for each output column, its position on the left and on the right
    pub lmap: Vec<Option<usize>>,
    pub rmap: Vec<Option<usize>>,
}

fn pad(c: Changes, map: &[Option<usize>]) -> impl Iterator<Item = (Row, i64)> + '_ {
    c.into_iter().map(move |(r, w)| {
        let row: Row = map.iter().map(|c| c.map_or(Id::UNDEF, |i| r[i])).collect();
        (row, w)
    })
}

impl Union {
    pub fn init(&mut self, p: &mut Pass) -> Result<Changes> {
        let l = self.left.init(p)?;
        let r = self.right.init(p)?;
        Ok(pad(l, &self.lmap).chain(pad(r, &self.rmap)).collect())
    }

    pub fn delta(&mut self, p: &mut Pass) -> Result<Changes> {
        let l = self.left.delta(p)?;
        let r = self.right.delta(p)?;
        Ok(pad(l, &self.lmap).chain(pad(r, &self.rmap)).collect())
    }
}

#[derive(Clone, Debug)]
pub struct Values {
    /// the rows, aligned to the node's schema (`None`: unbound)
    pub rows: Vec<Vec<Option<Term>>>,
}

impl Values {
    pub fn init(&mut self, p: &mut Pass) -> Result<Changes> {
        let mut out = Changes::new();
        for r in &self.rows {
            let row: Row = r
                .iter()
                .map(|t| t.as_ref().map_or(Id::UNDEF, |t| p.intern(t)))
                .collect();
            out.push((row, 1));
        }
        Ok(out)
    }
}

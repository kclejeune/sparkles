//! The incremental operators of spec F13 §3.2, one file per operator.
//!
//! Every operator has two entry points. `init` evaluates the operator at the pass's new
//! snapshot from scratch, fills its state and returns its whole output. `delta` returns
//! the change of its output from the change of its inputs, reading the snapshots before
//! and after the commit, and updates its state. Rows are aligned to the node's schema.

pub mod bgp;
pub mod distinct;
pub mod group;
pub mod join;
pub mod linear;
pub mod matching;
pub mod recompute;
pub mod template;

use super::pass::Pass;
use super::persist::{Reader, Writer};
use super::zset::{Changes, Row, row_bytes};
use crate::error::Result;
use crate::id::Id;
use rustc_hash::FxHashMap;
use serde::Serialize;

/// Whether a node is maintained from the changes or evaluated again.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Incremental,
    Recompute,
}

/// A node of a view's plan, as EXPLAIN shows it.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanNode {
    pub operator: String,
    pub mode: Mode,
    pub vars: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<PlanNode>,
}

impl PlanNode {
    /// Whether any node of the plan is a `recompute` node.
    pub fn has_recompute(&self) -> bool {
        self.mode == Mode::Recompute || self.children.iter().any(|c| c.has_recompute())
    }
}

#[derive(Clone, Debug)]
pub struct Node {
    pub op: Op,
    /// the circuit variables of the node's rows, in order
    pub schema: Vec<u32>,
    /// the variables bound in every row
    pub certain: Vec<u32>,
}

#[derive(Clone, Debug)]
pub enum Op {
    Bgp(Box<bgp::Bgp>),
    Filter(Box<linear::Filter>),
    Extend(Box<linear::Extend>),
    Project(Box<linear::Project>),
    Union(Box<linear::Union>),
    Values(Box<linear::Values>),
    Join(Box<join::Join>),
    Match(Box<matching::MatchOp>),
    Distinct(Box<distinct::Distinct>),
    Group(Box<group::Group>),
    Recompute(Box<recompute::Recompute>),
    Template(Box<template::Template>),
}

impl Node {
    pub fn init(&mut self, p: &mut Pass) -> Result<Changes> {
        p.control.check()?;
        let schema = self.schema.clone();
        match &mut self.op {
            Op::Bgp(o) => o.init(p, &schema),
            Op::Filter(o) => o.init(p),
            Op::Extend(o) => o.init(p),
            Op::Project(o) => o.init(p),
            Op::Union(o) => o.init(p),
            Op::Values(o) => o.init(p),
            Op::Join(o) => o.init(p),
            Op::Match(o) => o.init(p),
            Op::Distinct(o) => o.init(p),
            Op::Group(o) => o.init(p),
            Op::Recompute(o) => o.init(p, &schema),
            Op::Template(o) => o.init(p),
        }
    }

    pub fn delta(&mut self, p: &mut Pass) -> Result<Changes> {
        p.control.check()?;
        let schema = self.schema.clone();
        match &mut self.op {
            Op::Bgp(o) => o.delta(p),
            Op::Filter(o) => o.delta(p),
            Op::Extend(o) => o.delta(p),
            Op::Project(o) => o.delta(p),
            Op::Union(o) => o.delta(p),
            Op::Values(_) => Ok(Vec::new()),
            Op::Join(o) => o.delta(p),
            Op::Match(o) => o.delta(p),
            Op::Distinct(o) => o.delta(p),
            Op::Group(o) => o.delta(p),
            Op::Recompute(o) => o.delta(p, &schema),
            Op::Template(o) => o.delta(p),
        }
    }

    /// The children of the node.
    pub fn children(&self) -> Vec<&Node> {
        match &self.op {
            Op::Bgp(_) | Op::Values(_) | Op::Recompute(_) => vec![],
            Op::Filter(o) => vec![&o.inner],
            Op::Extend(o) => vec![&o.inner],
            Op::Project(o) => vec![&o.inner],
            Op::Union(o) => vec![&o.left, &o.right],
            Op::Join(o) => vec![&o.left, &o.right],
            Op::Match(o) => vec![&o.left, &o.right],
            Op::Distinct(o) => vec![&o.inner],
            Op::Group(o) => vec![&o.inner],
            Op::Template(o) => vec![&o.inner],
        }
    }

    fn children_mut(&mut self) -> Vec<&mut Node> {
        match &mut self.op {
            Op::Bgp(_) | Op::Values(_) | Op::Recompute(_) => vec![],
            Op::Filter(o) => vec![&mut o.inner],
            Op::Extend(o) => vec![&mut o.inner],
            Op::Project(o) => vec![&mut o.inner],
            Op::Union(o) => vec![&mut o.left, &mut o.right],
            Op::Join(o) => vec![&mut o.left, &mut o.right],
            Op::Match(o) => vec![&mut o.left, &mut o.right],
            Op::Distinct(o) => vec![&mut o.inner],
            Op::Group(o) => vec![&mut o.inner],
            Op::Template(o) => vec![&mut o.inner],
        }
    }

    /// The approximate memory of the node's state and its children's.
    pub fn bytes(&self) -> u64 {
        let own = match &self.op {
            Op::Join(o) => o.bytes(),
            Op::Match(o) => o.bytes(),
            Op::Distinct(o) => o.bytes(),
            Op::Group(o) => o.bytes(),
            Op::Recompute(o) => o.bytes(self.schema.len()),
            _ => 0,
        };
        own + self.children().iter().map(|c| c.bytes()).sum::<u64>()
    }

    /// Write the state of the node and its children, depth first.
    pub fn save(&self, w: &mut Writer) -> Result<()> {
        match &self.op {
            Op::Join(o) => o.save(w)?,
            Op::Match(o) => o.save(w)?,
            Op::Distinct(o) => o.save(w)?,
            Op::Group(o) => o.save(w)?,
            Op::Recompute(o) => o.save(w)?,
            _ => {}
        }
        for c in self.children() {
            c.save(w)?;
        }
        Ok(())
    }

    /// Read what [`save`](Self::save) wrote.
    pub fn load(&mut self, r: &mut Reader) -> Result<()> {
        match &mut self.op {
            Op::Join(o) => o.load(r)?,
            Op::Match(o) => o.load(r)?,
            Op::Distinct(o) => o.load(r)?,
            Op::Group(o) => o.load(r)?,
            Op::Recompute(o) => o.load(r)?,
            _ => {}
        }
        for c in self.children_mut() {
            c.load(r)?;
        }
        Ok(())
    }

    /// Whether the node or a node under it is a `recompute` node.
    pub fn has_recompute(&self) -> bool {
        matches!(self.op, Op::Recompute(_)) || self.children().iter().any(|c| c.has_recompute())
    }

    pub fn plan(&self, vars: &[String]) -> PlanNode {
        let names = |s: &[u32]| s.iter().map(|&v| vars[v as usize].clone()).collect();
        let (operator, detail, mode) = match &self.op {
            Op::Bgp(o) => ("BGP", Some(o.describe()), Mode::Incremental),
            Op::Filter(o) => ("Filter", Some(o.expr.to_string()), Mode::Incremental),
            Op::Extend(o) => (
                "Extend",
                Some(format!("?{} = {}", vars[o.var as usize], o.expr)),
                Mode::Incremental,
            ),
            Op::Project(_) => ("Project", None, Mode::Incremental),
            Op::Union(_) => ("Union", None, Mode::Incremental),
            Op::Values(o) => (
                "Values",
                Some(format!("{} rows", o.rows.len())),
                Mode::Incremental,
            ),
            Op::Join(_) => ("Join", None, Mode::Incremental),
            Op::Match(o) => (
                o.mode.name(),
                o.expr.as_ref().map(|e| e.to_string()),
                Mode::Incremental,
            ),
            Op::Distinct(_) => ("Distinct", None, Mode::Incremental),
            Op::Group(o) => ("Group", Some(o.describe(vars)), Mode::Incremental),
            Op::Recompute(o) => ("Recompute", Some(o.reason.clone()), Mode::Recompute),
            Op::Template(_) => ("Template", None, Mode::Incremental),
        };
        PlanNode {
            operator: operator.to_string(),
            mode,
            vars: names(&self.schema),
            detail,
            children: self.children().iter().map(|c| c.plan(vars)).collect(),
        }
    }

    /// The node's state as a `Bgp`, when it is one (a probe side of a join).
    pub fn as_bgp(&self) -> Option<&bgp::Bgp> {
        match &self.op {
            Op::Bgp(b) => Some(b),
            _ => None,
        }
    }
}

/// How the rows of two inputs combine: SPARQL's compatible mappings and their merge.
#[derive(Clone, Debug)]
pub struct Merge {
    /// the output schema: the left's variables, then the right's others
    pub out: Vec<u32>,
    /// for each output column, its position on the left and on the right
    pub take: Vec<(Option<usize>, Option<usize>)>,
    /// the positions of the shared variables on the left and the right
    pub shared: Vec<(usize, usize)>,
    /// the shared variables bound in every row of both sides: the join key
    pub key: Vec<(usize, usize)>,
}

impl Merge {
    pub fn new(l: &Node, r: &Node) -> Merge {
        let mut out = l.schema.clone();
        for v in &r.schema {
            if !out.contains(v) {
                out.push(*v);
            }
        }
        let pos = |s: &[u32], v: u32| s.iter().position(|&x| x == v);
        let take = out
            .iter()
            .map(|&v| (pos(&l.schema, v), pos(&r.schema, v)))
            .collect();
        let mut shared = Vec::new();
        let mut key = Vec::new();
        for (i, &v) in l.schema.iter().enumerate() {
            if let Some(j) = pos(&r.schema, v) {
                shared.push((i, j));
                if l.certain.contains(&v) && r.certain.contains(&v) {
                    key.push((i, j));
                }
            }
        }
        Merge {
            out,
            take,
            shared,
            key,
        }
    }

    pub fn compatible(&self, a: &[Id], b: &[Id]) -> bool {
        self.shared
            .iter()
            .all(|&(i, j)| a[i].is_undef() || b[j].is_undef() || a[i] == b[j])
    }

    /// Whether the rows share a variable bound in both (MINUS removes only those).
    pub fn overlap(&self, a: &[Id], b: &[Id]) -> bool {
        self.shared
            .iter()
            .any(|&(i, j)| !a[i].is_undef() && !b[j].is_undef())
    }

    pub fn merge(&self, a: &[Id], b: &[Id]) -> Row {
        self.take
            .iter()
            .map(|&(i, j)| {
                let x = i.map_or(Id::UNDEF, |i| a[i]);
                if x.is_undef() {
                    j.map_or(Id::UNDEF, |j| b[j])
                } else {
                    x
                }
            })
            .collect()
    }

    /// A left row with the right's other variables unbound.
    pub fn pad_left(&self, a: &[Id]) -> Row {
        self.take
            .iter()
            .map(|&(i, _)| i.map_or(Id::UNDEF, |i| a[i]))
            .collect()
    }

    pub fn key_left(&self, a: &[Id]) -> Row {
        self.key.iter().map(|&(i, _)| a[i]).collect()
    }

    pub fn key_right(&self, b: &[Id]) -> Row {
        self.key.iter().map(|&(_, j)| b[j]).collect()
    }

    /// The right's positions bound by a left row, for probing the right.
    pub fn seed_right(&self, a: &[Id]) -> Vec<(usize, Id)> {
        self.shared
            .iter()
            .filter(|&&(i, _)| !a[i].is_undef())
            .map(|&(i, j)| (j, a[i]))
            .collect()
    }

    /// The left's positions bound by a right row, for probing the left.
    pub fn seed_left(&self, b: &[Id]) -> Vec<(usize, Id)> {
        self.shared
            .iter()
            .filter(|&&(_, j)| !b[j].is_undef())
            .map(|&(i, j)| (i, b[j]))
            .collect()
    }
}

/// Rows with their weights, indexed by a key of some of their columns (spec F13 §3.5).
#[derive(Clone, Debug, Default)]
pub struct Arrangement {
    key: Vec<usize>,
    width: usize,
    map: FxHashMap<Row, FxHashMap<Row, i64>>,
    len: usize,
}

impl Arrangement {
    pub fn new(key: Vec<usize>, width: usize) -> Arrangement {
        Arrangement {
            key,
            width,
            map: FxHashMap::default(),
            len: 0,
        }
    }

    pub fn key_of(&self, r: &[Id]) -> Row {
        self.key.iter().map(|&i| r[i]).collect()
    }

    pub fn add(&mut self, r: &Row, w: i64) {
        if w == 0 {
            return;
        }
        let k = self.key_of(r);
        let bucket = self.map.entry(k.clone()).or_default();
        let e = bucket.entry(r.clone()).or_insert(0);
        if *e == 0 {
            self.len += 1;
        }
        *e += w;
        if *e == 0 {
            bucket.remove(r);
            self.len -= 1;
            if bucket.is_empty() {
                self.map.remove(&k);
            }
        }
    }

    pub fn apply(&mut self, c: &[(Row, i64)]) {
        for (r, w) in c {
            self.add(r, *w);
        }
    }

    /// The rows with key `k`.
    pub fn get(&self, k: &[Id]) -> impl Iterator<Item = (&Row, i64)> + '_ {
        self.map
            .get(k)
            .into_iter()
            .flat_map(|b| b.iter().map(|(r, w)| (r, *w)))
    }

    pub fn iter(&self) -> impl Iterator<Item = (&Row, i64)> + '_ {
        self.map
            .values()
            .flat_map(|b| b.iter().map(|(r, w)| (r, *w)))
    }

    pub fn bytes(&self) -> u64 {
        self.len as u64 * row_bytes(self.width) + self.map.len() as u64 * row_bytes(self.key.len())
    }

    pub fn save(&self, w: &mut Writer) -> Result<()> {
        w.u64(self.len as u64);
        for (r, n) in self.iter() {
            w.row(r)?;
            w.i64(n);
        }
        Ok(())
    }

    pub fn load(&mut self, r: &mut Reader) -> Result<()> {
        self.map.clear();
        self.len = 0;
        let n = r.u64()?;
        for _ in 0..n {
            let row = r.row()?;
            let w = r.i64()?;
            self.add(&row, w);
        }
        Ok(())
    }
}

/// The rows of one side of a join that can match a row of the other side: from the
/// side's arrangement, or by probing the side's patterns at a snapshot.
pub fn side_matches(
    p: &mut Pass,
    node: &Node,
    arr: Option<&Arrangement>,
    which: super::pass::Which,
    key: &[Id],
    seed: &[(usize, Id)],
) -> Result<Changes> {
    match arr {
        Some(a) => Ok(a.get(key).map(|(r, w)| (r.clone(), w)).collect()),
        None => match node.as_bgp() {
            Some(b) => b.probe(p, which, seed),
            None => Err(crate::error::Error::Corrupt(
                "a join side without an arrangement is not a pattern".into(),
            )),
        },
    }
}

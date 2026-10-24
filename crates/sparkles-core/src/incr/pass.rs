//! One maintenance pass: the snapshots before and after the changes, the changes
//! themselves, and what the operators read through them (spec F13 §3.2, §3.3).
//!
//! The pass reads two snapshots. `new` is the store's snapshot at the commit the pass
//! reaches. `old` is the same snapshot with the pass's changes undone
//! ([`Snapshot::before_changes`]), which costs a few tree insertions per changed quad and
//! never materializes a past state. Operators probe the store's permutations at either
//! snapshot instead of keeping copies of their inputs.
//!
//! The changes are filtered by the circuit's access view before the pass sees them, and
//! every scan applies the same filter, so a circuit reads what a query of its principal
//! reads. Protections that depend on the data never reach a pass: such a circuit is one
//! `recompute` node over its masked snapshot.

use super::terms::Terms;
use super::zset::{Changes, Row};
use crate::access::GraphAccess;
use crate::error::{Error, Result};
use crate::id::{self, Id, Tag};
use crate::index::Perm;
use crate::sparql::ctx::Ctx;
use crate::sparql::expr;
use crate::sparql::table::{Table, VarId};
use crate::sparql::value::Value;
use crate::store::{Chunk, Snapshot};
use crate::task::Control;
use oxrdf::{Term, Variable};
use rustc_hash::{FxHashMap, FxHashSet};
use spargebra::Query;
use spargebra::algebra::{Expression, GraphPattern};
use std::cell::RefCell;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Which snapshot a probe reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Which {
    New,
    Old,
}

/// The graph a scan matches.
#[derive(Clone, Copy, Debug)]
pub enum GraphSel {
    /// exactly this graph
    Exact(Id),
    /// the default graph (not the union: callers handle that)
    Default,
    /// any named graph
    Named,
}

/// What a circuit's principal may read, checked quad by quad.
pub struct Vis {
    access: Option<Arc<GraphAccess>>,
    graphs: RefCell<FxHashMap<Id, bool>>,
    preds: RefCell<FxHashMap<(Id, Id), bool>>,
}

impl Vis {
    /// The view of `access`, bound to `snap` (`None`: everything).
    pub fn new(access: Option<&Arc<GraphAccess>>, snap: &Arc<Snapshot>) -> Vis {
        let access = access
            .filter(|a| !a.reads_everything())
            .map(|a| a.bound(snap));
        Vis {
            access,
            graphs: Default::default(),
            preds: Default::default(),
        }
    }

    pub fn everything(&self) -> bool {
        self.access.is_none()
    }

    pub fn visible(&self, snap: &Snapshot, q: &[Id; 4]) -> bool {
        let Some(a) = &self.access else {
            return true;
        };
        let g_ok = *self
            .graphs
            .borrow_mut()
            .entry(q[3])
            .or_insert_with(|| a.readable_id(snap, q[3]));
        if !g_ok {
            return false;
        }
        // protections that depend on the data make the whole query one `recompute`
        // node, which reads the masked snapshot: the delta is not filtered by them
        let Some(t) = a
            .triples
            .as_ref()
            .filter(|t| t.hides() && t.state_independent())
        else {
            return true;
        };
        *self
            .preds
            .borrow_mut()
            .entry((q[1], q[3]))
            .or_insert_with(|| {
                let Some(Term::NamedNode(p)) = snap.term(q[1]) else {
                    return true;
                };
                let g = crate::access::graph_term(snap, q[3]);
                !t.hides_quad(p.as_str(), g.as_ref())
            })
    }
}

/// Counters of one pass.
#[derive(Clone, Copy, Debug, Default)]
pub struct PassStats {
    /// time spent evaluating `recompute` nodes
    pub recompute: Duration,
    /// `recompute` nodes evaluated
    pub recomputes: u64,
}

pub struct Pass<'a> {
    pub new: Arc<Snapshot>,
    /// `new` before the changes (absent when the pass builds from scratch)
    pub old: Option<Arc<Snapshot>>,
    /// the changes the principal can see
    pub added: &'a [[Id; 4]],
    pub removed: &'a [[Id; 4]],
    pub terms: &'a mut Terms,
    pub vis: &'a Vis,
    pub access: Option<Arc<GraphAccess>>,
    pub control: &'a Control,
    /// the circuit's variable names
    pub vars: &'a [String],
    pub stats: PassStats,
    /// every `recompute` node evaluates (a build or a reset)
    pub full: bool,
    /// the budget of one evaluation's intermediate results
    pub max_memory_bytes: Option<u64>,
    /// the query's base IRI
    pub base: Option<oxiri::Iri<String>>,
    ctx: Option<Ctx>,
    union_changes: Option<Vec<([Id; 3], i64)>>,
    work: u64,
}

impl<'a> Pass<'a> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        new: Arc<Snapshot>,
        old: Option<Arc<Snapshot>>,
        added: &'a [[Id; 4]],
        removed: &'a [[Id; 4]],
        terms: &'a mut Terms,
        vis: &'a Vis,
        access: Option<Arc<GraphAccess>>,
        control: &'a Control,
        vars: &'a [String],
    ) -> Pass<'a> {
        let full = old.is_none();
        Pass {
            new,
            old,
            added,
            removed,
            terms,
            vis,
            access,
            control,
            vars,
            stats: PassStats::default(),
            full,
            max_memory_bytes: None,
            base: None,
            ctx: None,
            union_changes: None,
            work: 0,
        }
    }

    /// Check the deadline and the cancel flag every so often.
    pub fn tick(&mut self) -> Result<()> {
        self.work += 1;
        if self.work.is_multiple_of(1024) {
            self.control.check()?;
        }
        Ok(())
    }

    pub fn snap(&self, w: Which) -> &Arc<Snapshot> {
        match w {
            Which::New => &self.new,
            Which::Old => self.old.as_ref().unwrap_or(&self.new),
        }
    }

    /// The id of a constant at this pass (`None`: no quad holds it).
    pub fn constant(&self, t: &Term) -> Option<Id> {
        self.new.lookup_term(t)
    }

    /// Visit the visible quads of snapshot `w` with the given positions bound, in graph
    /// `g`. The callback returns `false` to stop.
    pub fn scan(
        &self,
        w: Which,
        s: Option<Id>,
        p: Option<Id>,
        o: Option<Id>,
        g: GraphSel,
        f: &mut dyn FnMut([Id; 4]) -> Result<bool>,
    ) -> Result<()> {
        let snap = self.snap(w);
        let (perm, prefix): (Perm, Vec<u64>) = match (s, p, o, g) {
            (Some(s), Some(p), Some(o), _) => (Perm::Spo, vec![s.0, p.0, o.0]),
            (Some(s), Some(p), None, _) => (Perm::Spo, vec![s.0, p.0]),
            (Some(s), None, Some(o), _) => (Perm::Sop, vec![s.0, o.0]),
            (None, Some(p), Some(o), _) => (Perm::Pos, vec![p.0, o.0]),
            (Some(s), None, None, _) => (Perm::Spo, vec![s.0]),
            (None, Some(p), None, _) => (Perm::Pso, vec![p.0]),
            (None, None, Some(o), _) => (Perm::Osp, vec![o.0]),
            (None, None, None, GraphSel::Exact(g)) => (Perm::Gspo, vec![g.0]),
            (None, None, None, GraphSel::Default) => (Perm::Gspo, vec![Id::DEFAULT_GRAPH.0]),
            (None, None, None, GraphSel::Named) => (Perm::Spo, vec![]),
        };
        let mut stop = false;
        let mut each = |q: [Id; 4]| -> Result<bool> {
            if s.is_some_and(|x| x != q[0])
                || p.is_some_and(|x| x != q[1])
                || o.is_some_and(|x| x != q[2])
            {
                return Ok(true);
            }
            let g_ok = match g {
                GraphSel::Exact(x) => q[3] == x,
                GraphSel::Default => q[3] == Id::DEFAULT_GRAPH,
                GraphSel::Named => q[3] != Id::DEFAULT_GRAPH,
            };
            if !g_ok || !self.vis.visible(&self.new, &q) {
                return Ok(true);
            }
            f(q)
        };
        snap.scan(perm, &prefix, |c| {
            match c {
                Chunk::Block(b, from, to) => {
                    for i in from..to {
                        if !each(perm.to_quad(&b.key(i)))? {
                            stop = true;
                            break;
                        }
                    }
                }
                Chunk::Row(k) => {
                    if !each(perm.to_quad(&k))? {
                        stop = true;
                    }
                }
            }
            Ok(!stop)
        })
    }

    /// Whether snapshot `w` holds a visible quad `(s, p, o)` in a named graph: the
    /// triple is in the union default graph.
    pub fn in_union(&self, w: Which, t: [Id; 3]) -> Result<bool> {
        let mut found = false;
        self.scan(
            w,
            Some(t[0]),
            Some(t[1]),
            Some(t[2]),
            GraphSel::Named,
            &mut |_| {
                found = true;
                Ok(false)
            },
        )?;
        Ok(found)
    }

    /// The changes of the union default graph: each triple whose presence in some
    /// visible named graph changed, with `+1` or `−1`.
    pub fn union_changes(&mut self) -> Result<&[([Id; 3], i64)]> {
        if self.union_changes.is_none() {
            let mut seen: FxHashSet<[Id; 3]> = FxHashSet::default();
            let mut out = Vec::new();
            for q in self.added.iter().chain(self.removed.iter()) {
                if q[3] == Id::DEFAULT_GRAPH {
                    continue;
                }
                let t = [q[0], q[1], q[2]];
                if !seen.insert(t) {
                    continue;
                }
                let before = self.in_union(Which::Old, t)?;
                let after = self.in_union(Which::New, t)?;
                if before != after {
                    out.push((t, if after { 1 } else { -1 }));
                }
            }
            self.union_changes = Some(out);
        }
        Ok(self.union_changes.as_deref().unwrap_or_default())
    }

    fn ctx(&mut self) -> &Ctx {
        if self.ctx.is_none() {
            let mut c = Ctx::new(self.new.clone());
            c.use_cache = false;
            c.base_iri = self.base.clone();
            self.ctx = Some(c);
        }
        self.ctx.as_ref().expect("made above")
    }

    /// Evaluate `e` over rows of `schema` (circuit variables): the id of each value, or
    /// `None` for an error or an unbound result. With `ebv`, the effective boolean
    /// value as `true`/`false` ids.
    pub fn eval(
        &mut self,
        schema: &[u32],
        rows: &[&[Id]],
        e: &Expression,
        ebv: bool,
    ) -> Result<Vec<Option<Id>>> {
        if rows.is_empty() {
            return Ok(Vec::new());
        }
        let vars = self.vars;
        let ctx_vars: Vec<VarId> = {
            let ctx = self.ctx();
            schema.iter().map(|&v| ctx.var(&vars[v as usize])).collect()
        };
        // the rows as ids of the expression's context
        let mut local: Vec<(Id, Term)> = Vec::new();
        for r in rows {
            for &i in r.iter() {
                if i.tag() == Tag::Local
                    && let Some(t) = self.terms.term(&self.new, i)
                {
                    local.push((i, t));
                }
            }
        }
        let ctx = self.ctx();
        let to_ctx: FxHashMap<Id, Id> = local
            .into_iter()
            .map(|(i, t)| (i, ctx.intern_term(&t)))
            .collect();
        let mut table = Table::new(ctx_vars);
        for r in rows {
            let row: Vec<Id> = r
                .iter()
                .map(|i| to_ctx.get(i).copied().unwrap_or(*i))
                .collect();
            table.push_row(&row);
        }
        let empty = FxHashMap::default();
        let exists = |p: &GraphPattern| {
            Arc::new(expr::ExistsSpec::new(
                p.clone(),
                crate::sparql::plan::ActiveGraph::Default,
                Vec::new(),
                Vec::new(),
            ))
        };
        let compiled = expr::Compiler {
            ctx,
            subst: &empty,
            exists: &exists,
        }
        .compile(e);
        let map = table.var_map(ctx.nvars());
        enum Out {
            Id(Option<Id>),
            Term(Term),
        }
        let mut outs: Vec<Out> = Vec::with_capacity(rows.len());
        for i in 0..rows.len() {
            let row = expr::Row {
                table: &table,
                i,
                map: &map,
                dec: None,
            };
            if ebv {
                let b = expr::ebv(&compiled, &row, ctx).unwrap_or(false);
                outs.push(Out::Id(Some(Id::from_bool(b))));
                continue;
            }
            outs.push(match expr::eval(&compiled, &row, ctx) {
                Err(_) => Out::Id(None),
                Ok(expr::Val::Id(i)) | Ok(expr::Val::Dec(i, _)) => {
                    if i.tag() == Tag::Local
                        || (i.tag() == Tag::BNode && i.payload() & Id::LOCAL_BNODE_BIT != 0)
                    {
                        ctx.term(i).map_or(Out::Id(None), Out::Term)
                    } else {
                        Out::Id(Some(i))
                    }
                }
                Ok(expr::Val::V(Value::Integer(n))) => match Id::from_i64(i64::from(n)) {
                    Some(i) => Out::Id(Some(i)),
                    None => Out::Term(Value::Integer(n).to_term()),
                },
                Ok(expr::Val::V(Value::Bool(b))) => Out::Id(Some(Id::from_bool(b))),
                Ok(expr::Val::V(v)) => Out::Term(v.to_term()),
            });
        }
        ctx.check()?;
        let snap = self.new.clone();
        Ok(outs
            .into_iter()
            .map(|o| match o {
                Out::Id(x) => x,
                Out::Term(t) => Some(self.terms.intern(&snap, &t)),
            })
            .collect())
    }

    /// The value of an id of this circuit's space.
    pub fn value(&self, i: Id) -> Option<Value> {
        match i.tag() {
            Tag::Undef => None,
            Tag::Vocab | Tag::Delta => self.new.key(i).map(|k| Value::from_key(&k)),
            _ if i.is_inline() => id::inline_to_literal(i).map(|l| Value::from_literal(&l)),
            _ => self.terms.term(&self.new, i).map(|t| Value::from_term(&t)),
        }
    }

    /// The id of a computed value.
    pub fn value_id(&mut self, v: &Value) -> Id {
        if let Value::Integer(n) = v
            && let Some(i) = Id::from_i64(i64::from(*n))
        {
            return i;
        }
        if let Value::Bool(b) = v {
            return Id::from_bool(*b);
        }
        let snap = self.new.clone();
        self.terms.intern(&snap, &v.to_term())
    }

    /// The id of a term.
    pub fn intern(&mut self, t: &Term) -> Id {
        let snap = self.new.clone();
        self.terms.intern(&snap, t)
    }

    /// Evaluate `pattern` with the query engine at the new snapshot, as the circuit's
    /// principal: its solutions over the variables `vars`, as rows of this circuit's
    /// id space with their multiplicities.
    pub fn evaluate(
        &mut self,
        pattern: &GraphPattern,
        vars: &[u32],
        dataset: Option<&spargebra::algebra::QueryDataset>,
    ) -> Result<Changes> {
        let t0 = Instant::now();
        let variables: Vec<Variable> = vars
            .iter()
            .map(|&v| Variable::new_unchecked(self.vars[v as usize].clone()))
            .collect();
        let q = Query::Select {
            dataset: dataset.cloned(),
            pattern: GraphPattern::Project {
                inner: Box::new(pattern.clone()),
                variables,
            },
            base_iri: self.base.clone(),
        };
        let opts = crate::sparql::QueryOptions {
            graphs: self.access.clone(),
            deadline: self.control.deadline,
            cancel: Some(self.control.cancel.flag()),
            no_cache: true,
            max_memory_bytes: self.max_memory_bytes,
            allow_service: false,
            forbid_service: true,
            defaults_applied: true,
            ..Default::default()
        };
        let r = crate::sparql::execute_query(self.new.clone(), &q, &opts, 0.0)?;
        let snap = self.new.clone();
        let width = r.table.width();
        let mut z = super::zset::ZSet::new();
        let mut decoded: FxHashMap<Id, Id> = FxHashMap::default();
        for i in 0..r.table.len() {
            let mut row: Vec<Id> = Vec::with_capacity(width);
            for c in 0..width {
                let id = r.table.cols[c][i];
                let local = id.tag() == Tag::Local
                    || (id.tag() == Tag::BNode && id.payload() & Id::LOCAL_BNODE_BIT != 0);
                if !local {
                    row.push(id);
                    continue;
                }
                let m = match decoded.get(&id) {
                    Some(m) => *m,
                    None => {
                        let m = match r.term(id) {
                            Some(t) => self.terms.intern(&snap, &t),
                            None => Id::UNDEF,
                        };
                        decoded.insert(id, m);
                        m
                    }
                };
                row.push(m);
            }
            let row: Row = row.into();
            z.add(row, 1);
            if i % 4096 == 4095 {
                self.control.check()?;
            }
        }
        self.stats.recompute += t0.elapsed();
        self.stats.recomputes += 1;
        if r.table.width() != vars.len() {
            return Err(Error::Corrupt(
                "a recompute node's evaluation returned other columns".into(),
            ));
        }
        Ok(z.into_changes())
    }
}

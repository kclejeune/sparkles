//! Compiled SPARQL expressions and the function library (ARQ `expr` + `function`).

use super::ctx::{Ctx, TermKind};
use super::fnlib;
use super::table::{Table, VarId};
use super::value::{EvalResult, Num, NumOp, TypeError, Value, arith, compare, equals};
use crate::id::Id;
use oxrdf::vocab::xsd;
use oxrdf::{Literal, NamedNode, Term};
use oxsdatatypes::*;
use parking_lot::Mutex;
use rustc_hash::FxHashMap;
use spargebra::algebra::{Expression, Function, GraphPattern};
use std::borrow::Cow;
use std::cmp::Ordering;
use std::fmt::Write as _;
use std::str::FromStr;
use std::sync::Arc;

/// Pattern of an `EXISTS` / `NOT EXISTS`, evaluated by substitution (with memoization).
pub struct ExistsSpec {
    pub pattern: GraphPattern,
    pub graph: super::plan::ActiveGraph,
    /// variables of the pattern that may be substituted from the outer row
    pub vars: Vec<VarId>,
    /// constants already substituted for some of `vars` where the EXISTS appears
    /// (initial bindings, filter equalities, the row of an enclosing EXISTS): the outer
    /// table no longer holds them as columns, but they are part of the outer solution
    pub bound: Vec<(VarId, Id)>,
    pub memo: Mutex<FxHashMap<Vec<Id>, bool>>,
    /// the key set answering the EXISTS for every outer row (see [`super::exists`])
    pub decor: super::exists::Decor,
}

impl ExistsSpec {
    pub fn new(
        pattern: GraphPattern,
        graph: super::plan::ActiveGraph,
        vars: Vec<VarId>,
        bound: Vec<(VarId, Id)>,
    ) -> ExistsSpec {
        ExistsSpec {
            pattern,
            graph,
            vars,
            bound,
            memo: Mutex::new(FxHashMap::default()),
            decor: Default::default(),
        }
    }

    /// The same EXISTS with `v` substituted by `c` as well.
    pub fn with_bound(&self, v: VarId, c: Id) -> ExistsSpec {
        let mut bound = self.bound.clone();
        bound.push((v, c));
        ExistsSpec::new(
            self.pattern.clone(),
            self.graph.clone(),
            self.vars.clone(),
            bound,
        )
    }

    /// The value of `v` in the outer solution of `row`: its column, else the constant
    /// substituted for it (`UNDEF` if neither).
    #[inline]
    pub fn value(&self, row: &Row<'_>, v: VarId) -> Id {
        let id = row.get(v);
        if !id.is_undef() {
            return id;
        }
        self.bound
            .iter()
            .find(|b| b.0 == v)
            .map_or(Id::UNDEF, |b| b.1)
    }
}

/// The deepest expression evaluated on rayon's threads (see [`Expr::parallel`]): their
/// stack is 2 MiB unless the application sets another, and a debug build's evaluator
/// takes up to 30 KiB a level.
const PARALLEL_DEPTH: usize = 32;

#[derive(Clone)]
pub enum Expr {
    Const(Id),
    /// constant term with its value decoded at compile time
    Lit(Id, Value),
    Var(VarId),
    Or(Box<Expr>, Box<Expr>),
    And(Box<Expr>, Box<Expr>),
    Not(Box<Expr>),
    Eq(Box<Expr>, Box<Expr>),
    SameTerm(Box<Expr>, Box<Expr>),
    Cmp(Box<Expr>, Box<Expr>, CmpOp),
    In(Box<Expr>, Vec<Expr>),
    Arith(Box<Expr>, Box<Expr>, ArithOp),
    Neg(Box<Expr>),
    Pos(Box<Expr>),
    Bound(VarId),
    If(Box<Expr>, Box<Expr>, Box<Expr>),
    Coalesce(Vec<Expr>),
    Exists(Arc<ExistsSpec>),
    Call(Func, Vec<Expr>),
}

#[derive(Clone, Copy, Debug)]
pub enum CmpOp {
    Lt,
    Le,
    Gt,
    Ge,
}
#[derive(Clone, Copy, Debug)]
pub enum ArithOp {
    Add,
    Sub,
    Mul,
    Div,
}

#[derive(Clone, Debug)]
pub enum Func {
    Builtin(Function),
    Cast(NamedNode),
    Ext(String),
}

impl Expr {
    /// The constant term id, if this is a constant.
    pub fn const_id(&self) -> Option<Id> {
        match self {
            Expr::Const(id) | Expr::Lit(id, _) => Some(*id),
            _ => None,
        }
    }

    pub fn vars(&self, out: &mut Vec<VarId>) {
        match self {
            Expr::Const(_) | Expr::Lit(..) => {}
            Expr::Var(v) | Expr::Bound(v) => out.push(*v),
            Expr::Or(a, b)
            | Expr::And(a, b)
            | Expr::Eq(a, b)
            | Expr::SameTerm(a, b)
            | Expr::Cmp(a, b, _)
            | Expr::Arith(a, b, _) => {
                a.vars(out);
                b.vars(out);
            }
            Expr::Not(a) | Expr::Neg(a) | Expr::Pos(a) => a.vars(out),
            Expr::In(a, l) => {
                a.vars(out);
                l.iter().for_each(|e| e.vars(out));
            }
            Expr::If(a, b, c) => {
                a.vars(out);
                b.vars(out);
                c.vars(out);
            }
            Expr::Coalesce(l) | Expr::Call(_, l) => l.iter().for_each(|e| e.vars(out)),
            Expr::Exists(s) => out.extend(&s.vars),
        }
    }

    pub fn var_set(&self) -> Vec<VarId> {
        let mut v = Vec::new();
        self.vars(&mut v);
        v.sort_unstable();
        v.dedup();
        v
    }

    /// Split top-level conjunctions.
    pub fn conjuncts(self) -> Vec<Expr> {
        match self {
            Expr::And(a, b) => {
                let mut v = a.conjuncts();
                v.extend(b.conjuncts());
                v
            }
            e => vec![e],
        }
    }

    pub fn has_exists(&self) -> bool {
        match self {
            Expr::Exists(_) => true,
            Expr::Const(_) | Expr::Lit(..) | Expr::Var(_) | Expr::Bound(_) => false,
            Expr::Or(a, b)
            | Expr::And(a, b)
            | Expr::Eq(a, b)
            | Expr::SameTerm(a, b)
            | Expr::Cmp(a, b, _)
            | Expr::Arith(a, b, _) => a.has_exists() || b.has_exists(),
            Expr::Not(a) | Expr::Neg(a) | Expr::Pos(a) => a.has_exists(),
            Expr::In(a, l) => a.has_exists() || l.iter().any(|e| e.has_exists()),
            Expr::If(a, b, c) => a.has_exists() || b.has_exists() || c.has_exists(),
            Expr::Coalesce(l) | Expr::Call(_, l) => l.iter().any(|e| e.has_exists()),
        }
    }

    /// Whether rows may evaluate this expression on rayon's threads: it holds no EXISTS
    /// (whose pattern is planned and run on the query's thread) and nests at most
    /// [`PARALLEL_DEPTH`] levels, so the evaluator's recursion fits a rayon thread's stack.
    /// Deeper expressions are evaluated on the query's thread, whose stack
    /// [`super::depth::with_stack`] sized for the whole algebra.
    pub fn parallel(&self) -> bool {
        !self.has_exists() && self.within(PARALLEL_DEPTH)
    }

    /// Whether this expression nests at most `levels` levels (recursing no deeper).
    fn within(&self, levels: usize) -> bool {
        if levels == 0 {
            return false;
        }
        let n = levels - 1;
        match self {
            Expr::Const(_) | Expr::Lit(..) | Expr::Var(_) | Expr::Bound(_) | Expr::Exists(_) => {
                true
            }
            Expr::Or(a, b)
            | Expr::And(a, b)
            | Expr::Eq(a, b)
            | Expr::SameTerm(a, b)
            | Expr::Cmp(a, b, _)
            | Expr::Arith(a, b, _) => a.within(n) && b.within(n),
            Expr::Not(a) | Expr::Neg(a) | Expr::Pos(a) => a.within(n),
            Expr::In(a, l) => a.within(n) && l.iter().all(|e| e.within(n)),
            Expr::If(a, b, c) => a.within(n) && b.within(n) && c.within(n),
            Expr::Coalesce(l) | Expr::Call(_, l) => l.iter().all(|e| e.within(n)),
        }
    }

    pub fn display(&self, ctx: &Ctx) -> String {
        let mut s = String::new();
        self.fmt_into(ctx, &mut s);
        s
    }

    fn fmt_into(&self, ctx: &Ctx, s: &mut String) {
        let bin = |s: &mut String, a: &Expr, op: &str, b: &Expr| {
            s.push('(');
            a.fmt_into(ctx, s);
            let _ = write!(s, " {op} ");
            b.fmt_into(ctx, s);
            s.push(')');
        };
        match self {
            Expr::Const(id) | Expr::Lit(id, _) => match ctx.term(*id) {
                Some(t) => {
                    let _ = write!(s, "{t}");
                }
                None => s.push_str("UNDEF"),
            },
            Expr::Var(v) => {
                let _ = write!(s, "?{}", ctx.var_name(*v));
            }
            Expr::Or(a, b) => bin(s, a, "||", b),
            Expr::And(a, b) => bin(s, a, "&&", b),
            Expr::Eq(a, b) => bin(s, a, "=", b),
            Expr::Cmp(a, b, op) => bin(
                s,
                a,
                match op {
                    CmpOp::Lt => "<",
                    CmpOp::Le => "<=",
                    CmpOp::Gt => ">",
                    CmpOp::Ge => ">=",
                },
                b,
            ),
            Expr::Arith(a, b, op) => bin(
                s,
                a,
                match op {
                    ArithOp::Add => "+",
                    ArithOp::Sub => "-",
                    ArithOp::Mul => "*",
                    ArithOp::Div => "/",
                },
                b,
            ),
            Expr::Not(a) => {
                s.push('!');
                a.fmt_into(ctx, s)
            }
            Expr::Neg(a) => {
                s.push('-');
                a.fmt_into(ctx, s)
            }
            Expr::Pos(a) => a.fmt_into(ctx, s),
            Expr::Bound(v) => {
                let _ = write!(s, "BOUND(?{})", ctx.var_name(*v));
            }
            Expr::Exists(_) => s.push_str("EXISTS{…}"),
            Expr::SameTerm(a, b) => {
                s.push_str("sameTerm");
                bin(s, a, ",", b)
            }
            Expr::In(a, l) => {
                a.fmt_into(ctx, s);
                s.push_str(" IN (");
                for (i, e) in l.iter().enumerate() {
                    if i > 0 {
                        s.push_str(", ");
                    }
                    e.fmt_into(ctx, s);
                }
                s.push(')');
            }
            Expr::If(a, b, c) => {
                s.push_str("IF(");
                a.fmt_into(ctx, s);
                s.push_str(", ");
                b.fmt_into(ctx, s);
                s.push_str(", ");
                c.fmt_into(ctx, s);
                s.push(')');
            }
            Expr::Coalesce(l) | Expr::Call(_, l) => {
                match self {
                    Expr::Coalesce(_) => s.push_str("COALESCE"),
                    Expr::Call(Func::Builtin(f), _) => {
                        let _ = write!(s, "{}", format!("{f:?}").to_uppercase());
                    }
                    Expr::Call(Func::Cast(n), _) => {
                        let _ = write!(s, "{n}");
                    }
                    Expr::Call(Func::Ext(n), _) => {
                        let _ = write!(s, "<{n}>");
                    }
                    _ => {}
                }
                s.push('(');
                for (i, e) in l.iter().enumerate() {
                    if i > 0 {
                        s.push_str(", ");
                    }
                    e.fmt_into(ctx, s);
                }
                s.push(')');
            }
        }
    }
}

/// Access to the current solution.
pub struct Row<'a> {
    pub table: &'a Table,
    pub i: usize,
    pub map: &'a [Option<usize>],
    /// values decoded up front for this operator, per table column and row
    pub dec: Option<&'a DecodedCols>,
}

/// Row-aligned decoded values for some columns of a table (`None` = not decoded).
pub type DecodedCols = Vec<Option<Vec<Option<Value>>>>;

impl Row<'_> {
    #[inline]
    pub fn get(&self, v: VarId) -> Id {
        match self.map.get(v as usize).copied().flatten() {
            Some(c) => self.table.cols[c][self.i],
            None => Id::UNDEF,
        }
    }
}

/// An evaluated expression: either an existing term id or a computed value.
#[derive(Clone, Debug)]
pub enum Val {
    Id(Id),
    V(Value),
    /// an existing term together with its already decoded value
    Dec(Id, Value),
}

impl Val {
    pub(crate) fn value(self, ctx: &Ctx) -> EvalResult<Value> {
        match self {
            Val::Id(id) => ctx.value(id).ok_or(TypeError),
            Val::V(v) | Val::Dec(_, v) => Ok(v),
        }
    }
    pub fn into_id(self, ctx: &Ctx) -> Id {
        match self {
            Val::Id(id) | Val::Dec(id, _) => id,
            Val::V(v) => ctx.intern_value(&v),
        }
    }
    /// The term id, if this is an existing term (not a computed value).
    #[inline]
    fn id(&self) -> Option<Id> {
        match self {
            Val::Id(id) | Val::Dec(id, _) => Some(*id),
            Val::V(_) => None,
        }
    }
}

fn b(v: bool) -> Val {
    Val::Id(Id::from_bool(v))
}

pub fn eval(e: &Expr, row: &Row<'_>, ctx: &Ctx) -> EvalResult<Val> {
    match e {
        Expr::Const(id) => Ok(Val::Id(*id)),
        Expr::Lit(id, v) => Ok(Val::Dec(*id, v.clone())),
        Expr::Var(v) => {
            let id = row.get(*v);
            if id.is_undef() {
                Err(TypeError)
            } else if let Some(dec) = row.dec
                && let Some(c) = row.map.get(*v as usize).copied().flatten()
                && let Some(Some(col)) = dec.get(c)
                && let Some(val) = &col[row.i]
            {
                Ok(Val::Dec(id, val.clone()))
            } else {
                Ok(Val::Id(id))
            }
        }
        Expr::Bound(v) => Ok(b(!row.get(*v).is_undef())),
        Expr::Or(a, c) => {
            let x = ebv(a, row, ctx);
            if x == Ok(true) {
                return Ok(b(true));
            }
            let y = ebv(c, row, ctx);
            match (x, y) {
                (_, Ok(true)) => Ok(b(true)),
                (Ok(false), Ok(false)) => Ok(b(false)),
                _ => Err(TypeError),
            }
        }
        Expr::And(a, c) => {
            let x = ebv(a, row, ctx);
            if x == Ok(false) {
                return Ok(b(false));
            }
            let y = ebv(c, row, ctx);
            match (x, y) {
                (_, Ok(false)) => Ok(b(false)),
                (Ok(true), Ok(true)) => Ok(b(true)),
                _ => Err(TypeError),
            }
        }
        Expr::Not(a) => Ok(b(!ebv(a, row, ctx)?)),
        Expr::Eq(a, c) => {
            let x = eval(a, row, ctx)?;
            let y = eval(c, row, ctx)?;
            Ok(b(val_eq(x, y, ctx)?))
        }
        Expr::SameTerm(a, c) => {
            let x = eval(a, row, ctx)?.into_id(ctx);
            let y = eval(c, row, ctx)?.into_id(ctx);
            Ok(b(x == y))
        }
        Expr::Cmp(a, c, op) => {
            let x = eval(a, row, ctx)?;
            let y = eval(c, row, ctx)?;
            let o = val_cmp(x, y, ctx)?;
            Ok(b(match o {
                None => false,
                Some(o) => match op {
                    CmpOp::Lt => o == Ordering::Less,
                    CmpOp::Le => o != Ordering::Greater,
                    CmpOp::Gt => o == Ordering::Greater,
                    CmpOp::Ge => o != Ordering::Less,
                },
            }))
        }
        Expr::In(a, list) => {
            let x = eval(a, row, ctx)?;
            let mut err = false;
            for e in list {
                match eval(e, row, ctx).and_then(|y| val_eq(x.clone(), y, ctx)) {
                    Ok(true) => return Ok(b(true)),
                    Ok(false) => {}
                    Err(_) => err = true,
                }
            }
            if err { Err(TypeError) } else { Ok(b(false)) }
        }
        Expr::Arith(a, c, op) => {
            let x = eval(a, row, ctx)?.value(ctx)?;
            let y = eval(c, row, ctx)?.value(ctx)?;
            let op = match op {
                ArithOp::Add => NumOp::Add,
                ArithOp::Sub => NumOp::Sub,
                ArithOp::Mul => NumOp::Mul,
                ArithOp::Div => NumOp::Div,
            };
            // date/time arithmetic
            match (&x, &y, &op) {
                (Value::DateTime(d), Value::DateTime(e), NumOp::Sub) => {
                    return d
                        .checked_sub(*e)
                        .map(|r| Val::V(Value::DayTime(r)))
                        .ok_or(TypeError);
                }
                (Value::DateTime(d), Value::DayTime(e), NumOp::Add) => {
                    return d
                        .checked_add_day_time_duration(*e)
                        .map(|r| Val::V(Value::DateTime(r)))
                        .ok_or(TypeError);
                }
                (Value::DateTime(d), Value::DayTime(e), NumOp::Sub) => {
                    return d
                        .checked_sub_day_time_duration(*e)
                        .map(|r| Val::V(Value::DateTime(r)))
                        .ok_or(TypeError);
                }
                (Value::DateTime(d), Value::YearMonth(e), NumOp::Add) => {
                    return d
                        .checked_add_year_month_duration(*e)
                        .map(|r| Val::V(Value::DateTime(r)))
                        .ok_or(TypeError);
                }
                (Value::DateTime(d), Value::Duration(e), NumOp::Add) => {
                    return d
                        .checked_add_duration(*e)
                        .map(|r| Val::V(Value::DateTime(r)))
                        .ok_or(TypeError);
                }
                (Value::DateTime(d), Value::Duration(e), NumOp::Sub) => {
                    return d
                        .checked_sub_duration(*e)
                        .map(|r| Val::V(Value::DateTime(r)))
                        .ok_or(TypeError);
                }
                (Value::Date(d), Value::Date(e), NumOp::Sub) => {
                    return d
                        .checked_sub(*e)
                        .map(|r| Val::V(Value::DayTime(r)))
                        .ok_or(TypeError);
                }
                (Value::DayTime(d), Value::DayTime(e), NumOp::Add) => {
                    return d
                        .checked_add(*e)
                        .map(|r| Val::V(Value::DayTime(r)))
                        .ok_or(TypeError);
                }
                (Value::DayTime(d), Value::DayTime(e), NumOp::Sub) => {
                    return d
                        .checked_sub(*e)
                        .map(|r| Val::V(Value::DayTime(r)))
                        .ok_or(TypeError);
                }
                (Value::YearMonth(d), Value::YearMonth(e), NumOp::Add) => {
                    return d
                        .checked_add(*e)
                        .map(|r| Val::V(Value::YearMonth(r)))
                        .ok_or(TypeError);
                }
                (Value::YearMonth(d), Value::YearMonth(e), NumOp::Sub) => {
                    return d
                        .checked_sub(*e)
                        .map(|r| Val::V(Value::YearMonth(r)))
                        .ok_or(TypeError);
                }
                _ => {}
            }
            arith(op, &x, &y).map(Val::V)
        }
        Expr::Neg(a) => {
            let x = eval(a, row, ctx)?.value(ctx)?;
            Ok(Val::V(match Num::of(&x)? {
                Num::Integer(i) => Value::Integer(i.checked_neg().ok_or(TypeError)?),
                Num::Decimal(d) => Value::Decimal(d.checked_neg().ok_or(TypeError)?),
                Num::Float(f) => Value::Float(-f),
                Num::Double(d) => Value::Double(-d),
            }))
        }
        Expr::Pos(a) => {
            let x = eval(a, row, ctx)?.value(ctx)?;
            Num::of(&x)?;
            Ok(Val::V(x))
        }
        Expr::If(c, t, f) => {
            if ebv(c, row, ctx)? {
                eval(t, row, ctx)
            } else {
                eval(f, row, ctx)
            }
        }
        Expr::Coalesce(l) => {
            for e in l {
                if let Ok(v) = eval(e, row, ctx) {
                    return Ok(v);
                }
            }
            Err(TypeError)
        }
        Expr::Exists(spec) => {
            let key: Vec<Id> = spec.vars.iter().map(|&v| spec.value(row, v)).collect();
            if let Some(&r) = spec.memo.lock().get(&key) {
                return Ok(b(r));
            }
            let r = super::exists::per_row(|| super::plan::eval_exists(ctx, spec, &key))
                .map_err(|_| TypeError)?;
            let mut m = spec.memo.lock();
            if m.len() < 100_000 {
                m.insert(key, r);
            }
            Ok(b(r))
        }
        Expr::Call(f, args) => call(f, args, row, ctx),
    }
}

/// Does evaluating `e` require decoded values (as opposed to only ids / term kinds)?
pub fn needs_values(e: &Expr) -> bool {
    let simple = |x: &Expr| matches!(x, Expr::Var(_) | Expr::Const(_) | Expr::Lit(..));
    match e {
        Expr::Const(_) | Expr::Lit(..) | Expr::Bound(_) => false,
        Expr::SameTerm(a, b) => !(simple(a) && simple(b)),
        Expr::Not(a) => needs_values(a),
        Expr::And(a, b) | Expr::Or(a, b) => needs_values(a) || needs_values(b),
        Expr::Call(
            Func::Builtin(Function::IsIri | Function::IsBlank | Function::IsLiteral),
            args,
        ) => !args.iter().all(simple),
        _ => true,
    }
}

pub fn ebv(e: &Expr, row: &Row<'_>, ctx: &Ctx) -> EvalResult<bool> {
    match eval(e, row, ctx)? {
        Val::Id(id) if id.tag() == crate::id::Tag::Bool => Ok(id.as_bool()),
        v => v.value(ctx)?.ebv(),
    }
}

fn val_eq(x: Val, y: Val, ctx: &Ctx) -> EvalResult<bool> {
    if let (Some(a), Some(c)) = (x.id(), y.id()) {
        if a == c {
            return Ok(true);
        }
        // IRIs / bnodes with different ids are different terms
        let (ka, kc) = (ctx.kind(a), ctx.kind(c));
        let atomic = |k| matches!(k, TermKind::Iri | TermKind::BNode);
        if atomic(ka) || atomic(kc) || ka != kc {
            return Ok(false);
        }
    }
    equals(&x.value(ctx)?, &y.value(ctx)?)
}

fn val_cmp(x: Val, y: Val, ctx: &Ctx) -> EvalResult<Option<Ordering>> {
    if let (Some(a), Some(c)) = (x.id(), y.id()) {
        use crate::id::Tag;
        match (a.tag(), c.tag()) {
            (Tag::Int, Tag::Int) => return Ok(Some(a.as_i64().cmp(&c.as_i64()))),
            (Tag::Double, Tag::Double) => return Ok(a.as_f64().partial_cmp(&c.as_f64())),
            _ => {}
        }
    }
    compare(&x.value(ctx)?, &y.value(ctx)?)
}

// ------------------------------------------------------------------------------
// function library
// ------------------------------------------------------------------------------

fn s(v: impl Into<Arc<str>>) -> Val {
    Val::V(Value::Str(v.into()))
}

/// Result string with the same "string-ness" (lang tag) as the argument.
fn same_kind(lang: Option<&str>, v: String) -> Val {
    match lang {
        Some(l) => Val::V(Value::Lang(v.into(), l.into())),
        None => s(v),
    }
}

/// Evaluate a function argument to a value, borrowing constants and pre-decoded columns
/// instead of cloning them (cloning a shared `Arc` from every thread contends on its
/// reference count).
pub(crate) fn arg<'r>(
    args: &'r [Expr],
    i: usize,
    row: &'r Row<'_>,
    ctx: &Ctx,
) -> EvalResult<Cow<'r, Value>> {
    let e = args.get(i).ok_or(TypeError)?;
    match e {
        Expr::Lit(_, v) => Ok(Cow::Borrowed(v)),
        Expr::Var(var) => {
            if let Some(dec) = row.dec
                && let Some(c) = row.map.get(*var as usize).copied().flatten()
                && let Some(Some(col)) = dec.get(c)
                && let Some(val) = &col[row.i]
            {
                return Ok(Cow::Borrowed(val));
            }
            Ok(Cow::Owned(eval(e, row, ctx)?.value(ctx)?))
        }
        _ => Ok(Cow::Owned(eval(e, row, ctx)?.value(ctx)?)),
    }
}

/// SPARQL 17.4.3.1.2 argument compatibility.
pub(crate) fn compatible(a: Option<&str>, b: Option<&str>) -> bool {
    match (a, b) {
        (_, None) => true,
        (Some(x), Some(y)) => x.eq_ignore_ascii_case(y),
        (None, Some(_)) => false,
    }
}

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// A compiled pattern, shared by the calls of one thread: reusing the instance keeps its
/// match cache (a clone of a `Regex` starts an empty one).
type SharedRegex = std::rc::Rc<regex::Regex>;

thread_local! {
    static REGEX_CACHE: std::cell::RefCell<FxHashMap<(String, String), Option<SharedRegex>>> =
        std::cell::RefCell::new(FxHashMap::default());
    /// the last pattern looked up: a constant pattern is found without hashing or
    /// allocating on every row
    static LAST_REGEX: std::cell::RefCell<Option<(String, String, SharedRegex)>> =
        const { std::cell::RefCell::new(None) };
}

pub fn compile_regex(pattern: &str, flags: &str) -> EvalResult<regex::Regex> {
    shared_regex(pattern, flags).map(|r| (*r).clone())
}

/// The compiled pattern, from this thread's cache.
fn shared_regex(pattern: &str, flags: &str) -> EvalResult<SharedRegex> {
    if let Some(r) = LAST_REGEX.with(|l| {
        l.borrow()
            .as_ref()
            .filter(|(p, f, _)| p == pattern && f == flags)
            .map(|(_, _, r)| r.clone())
    }) {
        return Ok(r);
    }
    let r = REGEX_CACHE.with(|c| {
        let mut c = c.borrow_mut();
        if c.len() > 1000 {
            c.clear();
        }
        c.entry((pattern.to_string(), flags.to_string()))
            .or_insert_with(|| {
                let mut b = String::new();
                let mut literal = false;
                for f in flags.chars() {
                    match f {
                        'i' => b.push_str("(?i)"),
                        's' => b.push_str("(?s)"),
                        'm' => b.push_str("(?m)"),
                        'x' => b.push_str("(?x)"),
                        'q' => literal = true,
                        _ => return None,
                    }
                }
                if literal {
                    b.push_str(&regex::escape(pattern));
                } else {
                    b.push_str(pattern);
                }
                regex::RegexBuilder::new(&b)
                    .size_limit(1 << 22)
                    .build()
                    .ok()
                    .map(std::rc::Rc::new)
            })
            .clone()
            .ok_or(TypeError)
    })?;
    LAST_REGEX.with(|l| *l.borrow_mut() = Some((pattern.into(), flags.into(), r.clone())));
    Ok(r)
}

/// XPath replacement string → Rust regex replacement.
fn xpath_replacement(r: &str) -> EvalResult<String> {
    let mut out = String::new();
    let mut chars = r.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => match chars.next() {
                Some('\\') => out.push('\\'),
                Some('$') => out.push_str("$$"),
                _ => return Err(TypeError),
            },
            '$' => {
                let mut n = String::new();
                while let Some(d) = chars.peek().filter(|d| d.is_ascii_digit()) {
                    n.push(*d);
                    chars.next();
                }
                if n.is_empty() {
                    return Err(TypeError);
                }
                let _ = write!(out, "${{{n}}}");
            }
            c => out.push(c),
        }
    }
    Ok(out)
}

/// SPARQL `langMatches` (RFC 4647 basic filtering): `range` is `*`, the tag itself, or
/// a prefix of it ending at a `-`, compared case-insensitively.
pub fn lang_matches(tag: &str, range: &str) -> bool {
    if range == "*" {
        return !tag.is_empty();
    }
    let (t, r) = (tag.as_bytes(), range.as_bytes());
    t.len() >= r.len()
        && t[..r.len()].eq_ignore_ascii_case(r)
        && (t.len() == r.len() || t[r.len()] == b'-')
}

fn round_half_up_double(d: f64) -> f64 {
    (d + 0.5).floor()
}

fn call(f: &Func, args: &[Expr], row: &Row<'_>, ctx: &Ctx) -> EvalResult<Val> {
    match f {
        Func::Builtin(f) => builtin(f, args, row, ctx),
        Func::Cast(dt) => cast(dt, arg(args, 0, row, ctx)?.into_owned(), ctx),
        Func::Ext(iri) => extension(iri, args, row, ctx),
    }
}

fn builtin(f: &Function, args: &[Expr], row: &Row<'_>, ctx: &Ctx) -> EvalResult<Val> {
    use Function as F;
    let a0 = || arg(args, 0, row, ctx);
    let a1 = || arg(args, 1, row, ctx);
    let a2 = || arg(args, 2, row, ctx);
    let id0 =
        || -> EvalResult<Id> { Ok(eval(args.first().ok_or(TypeError)?, row, ctx)?.into_id(ctx)) };
    Ok(match f {
        F::Str => {
            let v = eval(&args[0], row, ctx)?;
            if let Some(id) = v.id()
                && let Some(Term::Literal(l)) = ctx.term(id)
            {
                return Ok(s(l.value()));
            }
            s(v.value(ctx)?.lexical()?)
        }
        F::Lang => match a0()?.into_owned() {
            Value::Lang(_, l) | Value::LangDir(_, l, _) => s(l),
            v if v.is_literal() => s(""),
            _ => return Err(TypeError),
        },
        F::LangMatches => {
            let t = a0()?;
            let r = a1()?;
            b(lang_matches(
                t.as_str().ok_or(TypeError)?,
                r.as_str().ok_or(TypeError)?,
            ))
        }
        F::Datatype => {
            let v = eval(&args[0], row, ctx)?;
            let dt = match &v {
                Val::Id(id) | Val::Dec(id, _) => match ctx.term(*id) {
                    Some(Term::Literal(l)) => l.datatype().into_owned(),
                    _ => return Err(TypeError),
                },
                Val::V(v) => v.datatype()?,
            };
            Val::V(Value::Iri(dt.as_str().into()))
        }
        F::Iri => match a0()?.into_owned() {
            Value::Iri(i) => Val::V(Value::Iri(i)),
            Value::Str(st) => {
                let iri = match &ctx.base_iri {
                    Some(base) => base.resolve(&st).map_err(|_| TypeError)?.into_inner(),
                    None => oxiri::Iri::parse(st.to_string())
                        .map_err(|_| TypeError)?
                        .into_inner(),
                };
                Val::V(Value::Iri(iri.into()))
            }
            _ => return Err(TypeError),
        },
        F::BNode => {
            if args.is_empty() {
                Val::Id(ctx.fresh_bnode())
            } else {
                let st = a0()?;
                let st = st.as_str().ok_or(TypeError)?;
                // same string → same blank node within one solution
                // key: the solution's bindings, ignoring blank nodes minted by this query
                let key: Vec<Id> = row
                    .table
                    .cols
                    .iter()
                    .map(|c| c[row.i])
                    .filter(|id| {
                        !(id.tag() == crate::id::Tag::BNode
                            && id.payload() & Id::LOCAL_BNODE_BIT != 0)
                    })
                    .collect();
                Val::Id(ctx.bnode_for_row(key, st))
            }
        }
        F::Rand => Val::V(Value::Double(rand::random::<f64>().into())),
        F::Abs => Val::V(match Num::of(&*a0()?)? {
            Num::Integer(i) => Value::Integer(i.checked_abs().ok_or(TypeError)?),
            Num::Decimal(d) => Value::Decimal(d.checked_abs().ok_or(TypeError)?),
            Num::Float(f) => Value::Float(f.abs()),
            Num::Double(d) => Value::Double(d.abs()),
        }),
        F::Ceil => Val::V(match Num::of(&*a0()?)? {
            Num::Integer(i) => Value::Integer(i),
            Num::Decimal(d) => Value::Decimal(d.checked_ceil().ok_or(TypeError)?),
            Num::Float(f) => Value::Float(f.ceil()),
            Num::Double(d) => Value::Double(d.ceil()),
        }),
        F::Floor => Val::V(match Num::of(&*a0()?)? {
            Num::Integer(i) => Value::Integer(i),
            Num::Decimal(d) => Value::Decimal(d.checked_floor().ok_or(TypeError)?),
            Num::Float(f) => Value::Float(f.floor()),
            Num::Double(d) => Value::Double(d.floor()),
        }),
        F::Round => Val::V(match Num::of(&*a0()?)? {
            Num::Integer(i) => Value::Integer(i),
            Num::Decimal(d) => Value::Decimal(d.checked_round().ok_or(TypeError)?),
            Num::Float(f) => Value::Float((round_half_up_double(f64::from(f)) as f32).into()),
            Num::Double(d) => Value::Double(round_half_up_double(d.into()).into()),
        }),
        F::Concat => {
            // the result keeps a language tag (and base direction) only if every
            // argument has the same one
            let mut out = String::new();
            type Tag = (Option<Arc<str>>, Option<oxrdf::BaseDirection>);
            let mut tag: Option<Tag> = None;
            for i in 0..args.len() {
                let v = arg(args, i, row, ctx)?;
                let (st, l) = v.string_arg()?;
                out.push_str(st);
                let dir = match &*v {
                    Value::LangDir(_, _, d) => Some(*d),
                    _ => None,
                };
                let t: Tag = (l.map(Into::into), dir);
                tag = match tag {
                    None => Some(t),
                    Some(prev) if prev == t => Some(prev),
                    Some(_) => Some((None, None)),
                };
            }
            match tag {
                Some((Some(l), Some(d))) => Val::V(Value::LangDir(out.into(), l, d)),
                Some((l, _)) => same_kind(l.as_deref(), out),
                None => s(out),
            }
        }
        F::SubStr => {
            let v = a0()?;
            let (st, l) = v.string_arg()?;
            let start = Num::of(&*a1()?)?.to_double();
            let len = if args.len() > 2 {
                Some(f64::from(Num::of(&*a2()?)?.to_double()))
            } else {
                None
            };
            // XPath fn:substring semantics with rounding
            let start = round_half_up_double(start.into());
            let chars: Vec<char> = st.chars().collect();
            let out: String = chars
                .iter()
                .enumerate()
                .filter(|(i, _)| {
                    let p = (*i + 1) as f64;
                    p >= start
                        && match len {
                            Some(l) => p < start + round_half_up_double(l),
                            None => true,
                        }
                })
                .map(|(_, c)| *c)
                .collect();
            same_kind(l, out)
        }
        F::StrLen => {
            let v = a0()?;
            Val::V(Value::Integer(
                (v.string_arg()?.0.chars().count() as i64).into(),
            ))
        }
        F::Replace => {
            let v = a0()?;
            let (st, l) = v.string_arg()?;
            let p = a1()?;
            let r = a2()?;
            let f = if args.len() > 3 {
                Some(arg(args, 3, row, ctx)?)
            } else {
                None
            };
            let flags = f
                .as_deref()
                .map_or(Some(""), Value::as_str)
                .ok_or(TypeError)?;
            let re = shared_regex(p.as_str().ok_or(TypeError)?, flags)?;
            if re.is_match("") {
                return Err(TypeError);
            }
            let rep = xpath_replacement(r.as_str().ok_or(TypeError)?)?;
            same_kind(l, re.replace_all(st, rep.as_str()).into_owned())
        }
        F::UCase => {
            let v = a0()?;
            let (st, l) = v.string_arg()?;
            same_kind(l, st.to_uppercase())
        }
        F::LCase => {
            let v = a0()?;
            let (st, l) = v.string_arg()?;
            same_kind(l, st.to_lowercase())
        }
        F::EncodeForUri => {
            let v = a0()?;
            let (st, _) = v.string_arg()?;
            const SET: &percent_encoding::AsciiSet = &percent_encoding::NON_ALPHANUMERIC
                .remove(b'-')
                .remove(b'_')
                .remove(b'.')
                .remove(b'~');
            s(percent_encoding::utf8_percent_encode(st, SET).to_string())
        }
        F::Contains | F::StrStarts | F::StrEnds | F::StrBefore | F::StrAfter => {
            let x = a0()?;
            let y = a1()?;
            let (xs, xl) = x.string_arg()?;
            let (ys, yl) = y.string_arg()?;
            if !compatible(xl, yl) {
                return Err(TypeError);
            }
            match f {
                F::Contains => b(xs.contains(ys)),
                F::StrStarts => b(xs.starts_with(ys)),
                F::StrEnds => b(xs.ends_with(ys)),
                F::StrBefore => match xs.find(ys) {
                    Some(i) => same_kind(xl, xs[..i].to_string()),
                    None => s(""),
                },
                _ => match xs.find(ys) {
                    Some(i) => same_kind(xl, xs[i + ys.len()..].to_string()),
                    None => s(""),
                },
            }
        }
        F::Year | F::Month | F::Day | F::Hours | F::Minutes | F::Seconds | F::Timezone | F::Tz => {
            let v = a0()?;
            let (y, mo, d, h, mi, se, tz) = match &*v {
                Value::DateTime(dt) => (
                    dt.year(),
                    dt.month(),
                    dt.day(),
                    dt.hour(),
                    dt.minute(),
                    dt.second(),
                    dt.timezone(),
                ),
                Value::Date(dt) => (
                    dt.year(),
                    dt.month(),
                    dt.day(),
                    0,
                    0,
                    Decimal::from(0),
                    dt.timezone(),
                ),
                Value::Time(t) => (0, 0, 0, t.hour(), t.minute(), t.second(), t.timezone()),
                _ => return Err(TypeError),
            };
            let int = |i: i64| Val::V(Value::Integer(i.into()));
            match f {
                F::Year => int(y),
                F::Month => int(mo as i64),
                F::Day => int(d as i64),
                F::Hours => int(h as i64),
                F::Minutes => int(mi as i64),
                F::Seconds => Val::V(Value::Decimal(se)),
                F::Timezone => Val::V(Value::DayTime(tz.ok_or(TypeError)?)),
                _ => s(match &*v {
                    Value::DateTime(dt) => dt
                        .timezone_offset()
                        .map(|t| t.to_string())
                        .unwrap_or_default(),
                    Value::Date(dt) => dt
                        .timezone_offset()
                        .map(|t| t.to_string())
                        .unwrap_or_default(),
                    Value::Time(dt) => dt
                        .timezone_offset()
                        .map(|t| t.to_string())
                        .unwrap_or_default(),
                    _ => String::new(),
                }),
            }
        }
        F::Now => Val::V(Value::DateTime(ctx.now)),
        F::Uuid => Val::V(Value::Iri(
            format!("urn:uuid:{}", uuid::Uuid::new_v4()).into(),
        )),
        F::StrUuid => s(uuid::Uuid::new_v4().to_string()),
        F::Md5 | F::Sha1 | F::Sha256 | F::Sha384 | F::Sha512 => {
            let v = a0()?;
            let st = v.as_str().ok_or(TypeError)?.as_bytes();
            use sha2::Digest;
            s(match f {
                F::Md5 => hex(&md5::Md5::digest(st)),
                F::Sha1 => hex(&sha1::Sha1::digest(st)),
                F::Sha256 => hex(&sha2::Sha256::digest(st)),
                F::Sha384 => hex(&sha2::Sha384::digest(st)),
                _ => hex(&sha2::Sha512::digest(st)),
            })
        }
        F::StrLang => {
            let v = a0()?;
            let l = a1()?;
            let st = v.as_str().ok_or(TypeError)?;
            let l = l.as_str().ok_or(TypeError)?;
            if l.is_empty() {
                return Err(TypeError);
            }
            Val::V(Value::Lang(st.into(), l.to_ascii_lowercase().into()))
        }
        F::StrDt => {
            let v = a0()?;
            let dt = a1()?;
            let st = v.as_str().ok_or(TypeError)?;
            let Value::Iri(dt) = dt.into_owned() else {
                return Err(TypeError);
            };
            Val::V(Value::from_literal(&Literal::new_typed_literal(
                st,
                NamedNode::new_unchecked(&*dt),
            )))
        }
        F::IsIri => b(ctx.kind(id0()?) == TermKind::Iri),
        F::IsBlank => b(ctx.kind(id0()?) == TermKind::BNode),
        F::IsLiteral => b(ctx.kind(id0()?) == TermKind::Literal),
        F::IsNumeric => {
            let v = a0()?;
            b(v.is_numeric())
        }
        F::Regex => {
            let v = a0()?;
            let (st, _) = v.string_arg()?;
            let p = a1()?;
            let f = if args.len() > 2 { Some(a2()?) } else { None };
            let flags = f
                .as_deref()
                .map_or(Some(""), Value::as_str)
                .ok_or(TypeError)?;
            b(shared_regex(p.as_str().ok_or(TypeError)?, flags)?.is_match(st))
        }
        F::Custom(iri) => return extension(iri.as_str(), args, row, ctx),
        // ---- SPARQL 1.2 -------------------------------------------------------------
        F::Triple => {
            let term = |i: usize| -> EvalResult<Term> {
                let v = eval(&args[i], row, ctx)?;
                match v.id() {
                    Some(id) => ctx.term(id).ok_or(TypeError),
                    None => Ok(v.value(ctx)?.to_term()),
                }
            };
            let s = match term(0)? {
                Term::NamedNode(n) => oxrdf::NamedOrBlankNode::NamedNode(n),
                Term::BlankNode(b) => oxrdf::NamedOrBlankNode::BlankNode(b),
                _ => return Err(TypeError),
            };
            let Term::NamedNode(p) = term(1)? else {
                return Err(TypeError);
            };
            let o = term(2)?;
            Val::V(Value::Triple(Arc::new(oxrdf::Triple::new(s, p, o))))
        }
        F::Subject | F::Predicate | F::Object => {
            let v = eval(&args[0], row, ctx)?;
            let t = match v.id() {
                Some(id) => match ctx.term(id) {
                    Some(Term::Triple(t)) => *t,
                    _ => return Err(TypeError),
                },
                None => match v.value(ctx)? {
                    Value::Triple(t) => (*t).clone(),
                    _ => return Err(TypeError),
                },
            };
            let c: Term = match f {
                F::Subject => t.subject.into(),
                F::Predicate => Term::NamedNode(t.predicate),
                _ => t.object,
            };
            Val::Id(ctx.intern_term(&c))
        }
        F::IsTriple => {
            let v = eval(&args[0], row, ctx)?;
            b(match v.id() {
                Some(id) => ctx.kind(id) == TermKind::Triple,
                None => matches!(v.value(ctx)?, Value::Triple(_)),
            })
        }
        F::LangDir => match &*a0()? {
            Value::LangDir(_, _, d) => s(match d {
                oxrdf::BaseDirection::Ltr => "ltr",
                oxrdf::BaseDirection::Rtl => "rtl",
            }),
            v if v.is_literal() && !matches!(v, Value::Triple(_)) => s(""),
            _ => return Err(TypeError),
        },
        F::HasLang => b(matches!(&*a0()?, Value::Lang(..) | Value::LangDir(..))),
        F::HasLangDir => b(matches!(&*a0()?, Value::LangDir(..))),
        F::StrLangDir => {
            let (v, l, d) = (a0()?, a1()?, a2()?);
            let st = v.as_str().ok_or(TypeError)?;
            let l = l.as_str().ok_or(TypeError)?;
            let dir = match d.as_str().ok_or(TypeError)? {
                "ltr" => oxrdf::BaseDirection::Ltr,
                "rtl" => oxrdf::BaseDirection::Rtl,
                _ => return Err(TypeError),
            };
            if l.is_empty() {
                return Err(TypeError);
            }
            Val::V(Value::LangDir(
                st.into(),
                l.to_ascii_lowercase().into(),
                dir,
            ))
        }
        #[allow(unreachable_patterns)]
        _ => return Err(TypeError),
    })
}

pub const XSD: &str = "http://www.w3.org/2001/XMLSchema#";
const FN: &str = "http://www.w3.org/2005/xpath-functions#";
const MATH: &str = "http://www.w3.org/2005/xpath-functions/math#";
const AFN: &str = "http://jena.apache.org/ARQ/function#";

/// Is `iri` a supported XSD cast function?
pub fn is_cast(iri: &str) -> bool {
    iri.strip_prefix(XSD).is_some_and(|l| {
        matches!(
            l,
            "string"
                | "boolean"
                | "integer"
                | "decimal"
                | "float"
                | "double"
                | "dateTime"
                | "date"
                | "time"
                | "duration"
                | "dayTimeDuration"
                | "yearMonthDuration"
                | "int"
                | "long"
                | "short"
                | "byte"
                | "nonPositiveInteger"
                | "negativeInteger"
                | "nonNegativeInteger"
                | "positiveInteger"
                | "unsignedLong"
                | "unsignedInt"
                | "unsignedShort"
                | "unsignedByte"
                | "anyURI"
                | "gYear"
                | "gYearMonth"
                | "gMonth"
                | "gMonthDay"
                | "gDay"
        )
    })
}

/// The range of an integer type derived from `xsd:integer` (XSD 1.1 §3.4.13–3.4.25),
/// as inclusive `i128` bounds.
fn integer_range(local: &str) -> Option<(i128, i128)> {
    Some(match local {
        "long" => (i64::MIN.into(), i64::MAX.into()),
        "int" => (i32::MIN.into(), i32::MAX.into()),
        "short" => (i16::MIN.into(), i16::MAX.into()),
        "byte" => (i8::MIN.into(), i8::MAX.into()),
        "nonPositiveInteger" => (i128::MIN, 0),
        "negativeInteger" => (i128::MIN, -1),
        "nonNegativeInteger" => (0, i128::MAX),
        "positiveInteger" => (1, i128::MAX),
        "unsignedLong" => (0, u64::MAX.into()),
        "unsignedInt" => (0, u32::MAX.into()),
        "unsignedShort" => (0, u16::MAX.into()),
        "unsignedByte" => (0, u8::MAX.into()),
        _ => return None,
    })
}

/// A cast to `xsd:gYear`, `gYearMonth`, `gMonth`, `gMonthDay` or `gDay` (`T`): from a
/// dateTime or a date, from the type itself, or from a string. The value is kept as a
/// literal of the type.
fn gregorian_cast<T>(v: &Value, lex: &str, from_str: bool, dt: &NamedNode) -> EvalResult<Val>
where
    T: FromStr + std::fmt::Display + TryFrom<DateTime> + TryFrom<Date>,
{
    let t: T = match v {
        Value::DateTime(d) => T::try_from(*d).map_err(|_| TypeError)?,
        Value::Date(d) => T::try_from(*d).map_err(|_| TypeError)?,
        _ if from_str => T::from_str(lex.trim()).map_err(|_| TypeError)?,
        _ => return Err(TypeError),
    };
    Ok(Val::V(Value::Other {
        lex: t.to_string().into(),
        dt: dt.as_str().into(),
    }))
}

/// XSD casts (SPARQL 17.5).
pub fn cast(dt: &NamedNode, v: Value, ctx: &Ctx) -> EvalResult<Val> {
    let local = dt.as_str().strip_prefix(XSD).ok_or(TypeError)?;
    if let Value::BNode(_) = v {
        return Err(TypeError);
    }
    if let Value::Iri(i) = &v {
        return if local == "string" {
            Ok(s(i.clone()))
        } else {
            Err(TypeError)
        };
    }
    let lex = v.lexical()?;
    let from_str = matches!(v, Value::Str(_) | Value::Other { .. });
    let parsed = |dt: NamedNodeRef<'_>| -> EvalResult<Val> {
        let val = Value::from_literal(&Literal::new_typed_literal(lex.trim(), dt));
        if matches!(val, Value::Other { .. }) {
            Err(TypeError)
        } else {
            Ok(Val::V(val))
        }
    };
    use oxrdf::NamedNodeRef;
    let num = Num::of(&v).ok();
    Ok(match local {
        "string" => s(lex),
        "boolean" => match (&v, num) {
            (Value::Bool(x), _) => b(*x),
            (_, Some(n)) => b(Value::Double(n.to_double()).ebv().unwrap_or(false)),
            _ if from_str => return parsed(xsd::BOOLEAN),
            _ => return Err(TypeError),
        },
        "integer" | "int" | "long" | "short" | "byte" | "nonPositiveInteger"
        | "negativeInteger" | "nonNegativeInteger" | "positiveInteger" | "unsignedLong"
        | "unsignedInt" | "unsignedShort" | "unsignedByte" => {
            let r = match (&v, num) {
                (Value::Bool(x), _) => Value::Integer((*x as i64).into()),
                (_, Some(Num::Integer(i))) => Value::Integer(i),
                (_, Some(Num::Decimal(d))) => {
                    Value::Integer(Integer::try_from(d).map_err(|_| TypeError)?)
                }
                (_, Some(Num::Float(f))) => {
                    Value::Integer(Integer::try_from(f).map_err(|_| TypeError)?)
                }
                (_, Some(Num::Double(d))) => {
                    Value::Integer(Integer::try_from(d).map_err(|_| TypeError)?)
                }
                _ if from_str => match parsed(xsd::INTEGER)? {
                    Val::V(v) => v,
                    _ => return Err(TypeError),
                },
                _ => return Err(TypeError),
            };
            match integer_range(local) {
                None => Val::V(r),
                Some((lo, hi)) => {
                    // the derived type's range, and the literal keeps its datatype
                    let Value::Integer(i) = r else {
                        return Err(TypeError);
                    };
                    if !(lo..=hi).contains(&i128::from(i64::from(i))) {
                        return Err(TypeError);
                    }
                    let lit = Literal::new_typed_literal(i.to_string(), dt.clone());
                    Val::Dec(ctx.intern_term(&Term::Literal(lit)), r)
                }
            }
        }
        "anyURI" => match &v {
            Value::Str(x) => Val::V(Value::Other {
                lex: x.trim().into(),
                dt: dt.as_str().into(),
            }),
            Value::Other { lex, dt: d } if &**d == dt.as_str() => Val::V(Value::Other {
                lex: lex.clone(),
                dt: d.clone(),
            }),
            _ => return Err(TypeError),
        },
        "gYear" => return gregorian_cast::<GYear>(&v, &lex, from_str, dt),
        "gYearMonth" => return gregorian_cast::<GYearMonth>(&v, &lex, from_str, dt),
        "gMonth" => return gregorian_cast::<GMonth>(&v, &lex, from_str, dt),
        "gMonthDay" => return gregorian_cast::<GMonthDay>(&v, &lex, from_str, dt),
        "gDay" => return gregorian_cast::<GDay>(&v, &lex, from_str, dt),
        "decimal" => match (&v, num) {
            (Value::Bool(x), _) => Val::V(Value::Decimal((*x as i64).into())),
            (_, Some(Num::Integer(i))) => Val::V(Value::Decimal(i.into())),
            (_, Some(Num::Decimal(d))) => Val::V(Value::Decimal(d)),
            (_, Some(Num::Float(f))) => {
                Val::V(Value::Decimal(Decimal::try_from(f).map_err(|_| TypeError)?))
            }
            (_, Some(Num::Double(d))) => {
                Val::V(Value::Decimal(Decimal::try_from(d).map_err(|_| TypeError)?))
            }
            _ if from_str => return parsed(xsd::DECIMAL),
            _ => return Err(TypeError),
        },
        "float" => match (&v, num) {
            (Value::Bool(x), _) => Val::V(Value::Float(if *x { 1.0f32 } else { 0.0 }.into())),
            (_, Some(n)) => Val::V(Value::Float(Float::from(n.to_double()))),
            _ if from_str => return parsed(xsd::FLOAT),
            _ => return Err(TypeError),
        },
        "double" => match (&v, num) {
            (Value::Bool(x), _) => Val::V(Value::Double(if *x { 1.0 } else { 0.0 }.into())),
            (_, Some(n)) => Val::V(Value::Double(n.to_double())),
            _ if from_str => return parsed(xsd::DOUBLE),
            _ => return Err(TypeError),
        },
        "dateTime" => match &v {
            Value::DateTime(d) => Val::V(Value::DateTime(*d)),
            Value::Date(d) => Val::V(Value::DateTime(
                DateTime::try_from(*d).map_err(|_| TypeError)?,
            )),
            _ if from_str => return parsed(xsd::DATE_TIME),
            _ => return Err(TypeError),
        },
        "date" => match &v {
            Value::Date(d) => Val::V(Value::Date(*d)),
            Value::DateTime(d) => Val::V(Value::Date(Date::try_from(*d).map_err(|_| TypeError)?)),
            _ if from_str => return parsed(xsd::DATE),
            _ => return Err(TypeError),
        },
        "time" => match &v {
            Value::Time(d) => Val::V(Value::Time(*d)),
            Value::DateTime(d) => Val::V(Value::Time(Time::from(*d))),
            _ if from_str => return parsed(xsd::TIME),
            _ => return Err(TypeError),
        },
        "duration" => match &v {
            Value::Duration(d) => Val::V(Value::Duration(*d)),
            Value::DayTime(d) => Val::V(Value::Duration((*d).into())),
            Value::YearMonth(d) => Val::V(Value::Duration((*d).into())),
            _ if from_str => return parsed(xsd::DURATION),
            _ => return Err(TypeError),
        },
        "dayTimeDuration" => match &v {
            Value::DayTime(d) => Val::V(Value::DayTime(*d)),
            Value::Duration(d) => Val::V(Value::DayTime(
                DayTimeDuration::try_from(*d).map_err(|_| TypeError)?,
            )),
            _ if from_str => return parsed(xsd::DAY_TIME_DURATION),
            _ => return Err(TypeError),
        },
        "yearMonthDuration" => match &v {
            Value::YearMonth(d) => Val::V(Value::YearMonth(*d)),
            Value::Duration(d) => Val::V(Value::YearMonth(
                YearMonthDuration::try_from(*d).map_err(|_| TypeError)?,
            )),
            _ if from_str => return parsed(xsd::YEAR_MONTH_DURATION),
            _ => return Err(TypeError),
        },
        _ => return Err(TypeError),
    })
}

/// Is `iri` a supported extension function (fn:, math:, afn:, cdt:, and with the `geo`
/// feature geof:, spatialF:)?
pub fn is_extension(iri: &str) -> bool {
    iri.starts_with(FN)
        || iri.starts_with(MATH)
        || iri.starts_with(AFN)
        || iri.starts_with(super::cdt::NS)
        || iri.starts_with(crate::vector::NS)
        || (cfg!(feature = "geo")
            && (iri.starts_with(crate::geo::vocab::GEOF)
                || iri.starts_with(crate::geo::vocab::SPATIALF)))
}

fn extension(iri: &str, args: &[Expr], row: &Row<'_>, ctx: &Ctx) -> EvalResult<Val> {
    #[cfg(feature = "geo")]
    if let Some(r) = crate::geo::functions::call(iri, args, row, ctx) {
        return r;
    }
    if let Some(l) = iri.strip_prefix(super::cdt::NS) {
        return super::cdt::call(l, args, row, ctx);
    }
    let a = |i: usize| arg(args, i, row, ctx);
    let dbl = |i: usize| -> EvalResult<f64> { Ok(Num::of(&*a(i)?)?.to_double().into()) };
    let d = |x: f64| Ok(Val::V(Value::Double(x.into())));
    if let Some(l) = iri.strip_prefix(MATH) {
        return match l {
            "pi" => d(std::f64::consts::PI),
            "e" => d(std::f64::consts::E),
            "sqrt" => d(dbl(0)?.sqrt()),
            "exp" => d(dbl(0)?.exp()),
            "exp10" => d(10f64.powf(dbl(0)?)),
            "log" => d(dbl(0)?.ln()),
            "log10" => d(dbl(0)?.log10()),
            "pow" => d(dbl(0)?.powf(dbl(1)?)),
            "sin" => d(dbl(0)?.sin()),
            "cos" => d(dbl(0)?.cos()),
            "tan" => d(dbl(0)?.tan()),
            "asin" => d(dbl(0)?.asin()),
            "acos" => d(dbl(0)?.acos()),
            "atan" => d(dbl(0)?.atan()),
            "atan2" => d(dbl(0)?.atan2(dbl(1)?)),
            _ => Err(TypeError),
        };
    }
    let fb = |f: Function| builtin(&f, args, row, ctx);
    // an xsd:integer argument (a precision, an index)
    let int = |i: usize| -> EvalResult<i64> {
        match Num::of(&*a(i)?)? {
            Num::Integer(n) => Ok(i64::from(n)),
            _ => Err(TypeError),
        }
    };
    // the timezone argument of the adjust functions: absent, a dayTimeDuration, or ""
    let tz = |i: usize| -> EvalResult<fnlib::TzArg> {
        if args.len() <= i {
            return Ok(fnlib::TzArg::Implicit);
        }
        Ok(match &*a(i)? {
            Value::DayTime(d) => fnlib::TzArg::Offset(*d),
            Value::Duration(d) => {
                fnlib::TzArg::Offset(DayTimeDuration::try_from(*d).map_err(|_| TypeError)?)
            }
            Value::Str(x) if x.is_empty() => fnlib::TzArg::Remove,
            _ => return Err(TypeError),
        })
    };
    let v = |x: Value| Ok(Val::V(x));
    if let Some(l) = iri.strip_prefix(FN) {
        if let Some(part) = l.strip_suffix("-from-duration") {
            return v(fnlib::duration_part(&*a(0)?, part)?);
        }
        if let Some(kind) = l
            .strip_prefix("adjust-")
            .and_then(|k| k.strip_suffix("-to-timezone"))
        {
            return v(fnlib::adjust(&*a(0)?, tz(1)?, Some(kind))?);
        }
        return match l {
            "round" if args.len() == 2 => v(fnlib::round(Num::of(&*a(0)?)?, int(1)?, false)?),
            "round-half-to-even" => {
                let p = if args.len() > 1 { int(1)? } else { 0 };
                v(fnlib::round(Num::of(&*a(0)?)?, p, true)?)
            }
            "numeric-mod" => v(fnlib::numeric_mod(Num::of(&*a(0)?)?, Num::of(&*a(1)?)?)?),
            "numeric-integer-divide" => v(fnlib::numeric_integer_divide(
                Num::of(&*a(0)?)?,
                Num::of(&*a(1)?)?,
            )?),
            "dateTime" => v(fnlib::date_time(&*a(0)?, &*a(1)?)?),
            "timezone-from-date" | "timezone-from-time" => fb(Function::Timezone),
            // ARQ's names for the date and dateTime accessors
            "years-from-date" | "years-from-dateTime" => fb(Function::Year),
            "months-from-date" | "months-from-dateTime" => fb(Function::Month),
            "days-from-date" | "days-from-dateTime" => fb(Function::Day),
            "implicit-timezone" => v(Value::DayTime(DayTimeDuration::default())),
            "normalize-unicode" => {
                let x = a(0)?;
                let form = if args.len() > 1 {
                    Some(a(1)?.string_arg()?.0.to_string())
                } else {
                    None
                };
                Ok(s(fnlib::normalize_unicode(
                    x.string_arg()?.0,
                    form.as_deref(),
                )?))
            }
            "error" => Err(TypeError),
            "string-length" => fb(Function::StrLen),
            "substring" => fb(Function::SubStr),
            "upper-case" => fb(Function::UCase),
            "lower-case" => fb(Function::LCase),
            "contains" => fb(Function::Contains),
            "starts-with" => fb(Function::StrStarts),
            "ends-with" => fb(Function::StrEnds),
            "substring-before" => fb(Function::StrBefore),
            "substring-after" => fb(Function::StrAfter),
            "concat" => fb(Function::Concat),
            "matches" => fb(Function::Regex),
            "replace" => fb(Function::Replace),
            "encode-for-uri" => fb(Function::EncodeForUri),
            "abs" => fb(Function::Abs),
            "ceiling" => fb(Function::Ceil),
            "floor" => fb(Function::Floor),
            "round" => fb(Function::Round),
            "year-from-dateTime" | "year-from-date" => fb(Function::Year),
            "month-from-dateTime" | "month-from-date" => fb(Function::Month),
            "day-from-dateTime" | "day-from-date" => fb(Function::Day),
            "hours-from-dateTime" | "hours-from-time" => fb(Function::Hours),
            "minutes-from-dateTime" | "minutes-from-time" => fb(Function::Minutes),
            "seconds-from-dateTime" | "seconds-from-time" => fb(Function::Seconds),
            "timezone-from-dateTime" => fb(Function::Timezone),
            "normalize-space" => {
                let v = a(0)?;
                let (st, l) = v.string_arg()?;
                Ok(same_kind(
                    l,
                    st.split_whitespace().collect::<Vec<_>>().join(" "),
                ))
            }
            "string-join" => {
                let sep = if args.len() > 1 {
                    a(args.len() - 1)?.lexical()?.to_string()
                } else {
                    String::new()
                };
                let mut parts = Vec::new();
                for i in 0..args.len().saturating_sub(1).max(1) {
                    parts.push(a(i)?.lexical()?.to_string());
                }
                Ok(s(parts.join(&sep)))
            }
            "not" => Ok(b(!a(0)?.ebv()?)),
            "boolean" => Ok(b(a(0)?.ebv()?)),
            _ => Err(TypeError),
        };
    }
    if let Some(l) = iri.strip_prefix(crate::vector::NS) {
        use crate::vector::{self, Metric};
        // a well-typed spk:vector literal, or a type error
        let vec_arg = |i: usize| -> EvalResult<Vec<f32>> {
            match &*a(i)? {
                Value::Other { lex, dt } if &**dt == vector::DATATYPE => {
                    vector::parse(lex).map_err(|_| TypeError)
                }
                _ => Err(TypeError),
            }
        };
        let pair = |m: Metric| -> EvalResult<Val> {
            let (x, y) = (vec_arg(0)?, vec_arg(1)?);
            let s =
                vector::score(m, &x, vector::norm(&x), &y, vector::norm(&y)).ok_or(TypeError)?;
            d(s as f64)
        };
        return match l {
            "cosine" => pair(Metric::Cosine),
            "dot" => pair(Metric::Dot),
            "euclidean" => pair(Metric::Euclidean),
            "dimension" => Ok(Val::V(Value::Integer((vec_arg(0)?.len() as i64).into()))),
            _ => Err(TypeError),
        };
    }
    if let Some(l) = iri.strip_prefix(AFN) {
        return match l {
            "strlen" => fb(Function::StrLen),
            "substr" | "substring" => {
                let x = a(0)?;
                let end = if args.len() > 2 { Some(int(2)?) } else { None };
                Ok(s(fnlib::java_substring(x.string_arg()?.0, int(1)?, end)?))
            }
            "sha1sum" => {
                use sha1::Digest;
                Ok(s(hex(&sha1::Sha1::digest(a(0)?.lexical()?.as_bytes()))))
            }
            "uuid" => fb(Function::Uuid),
            "struuid" => fb(Function::StrUuid),
            "evenInteger" => match Num::of(&*a(0)?)? {
                Num::Integer(n) => Ok(b(i64::from(n) % 2 == 0)),
                _ => Err(TypeError),
            },
            "langeq" => {
                let x = a(0)?;
                let tag = match &*x {
                    Value::Lang(_, t) | Value::LangDir(_, t, _) => t.to_string(),
                    v if v.is_literal() => String::new(),
                    _ => return Err(TypeError),
                };
                let range = a(1)?;
                Ok(b(lang_matches(&tag, range.as_str().ok_or(TypeError)?)))
            }
            "date" => {
                let x = a(0)?;
                let lex = x.as_str().ok_or(TypeError)?;
                let b = lex.as_bytes();
                let shape = b.len() == 10
                    && b[4] == b'-'
                    && b[7] == b'-'
                    && b.iter()
                        .enumerate()
                        .all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit());
                if !shape {
                    return Err(TypeError);
                }
                DateTime::from_str(&format!("{lex}T00:00:00Z"))
                    .map(|d| Val::V(Value::DateTime(d)))
                    .map_err(|_| TypeError)
            }
            "timezone" => v(Value::DayTime(DayTimeDuration::default())),
            "adjust-to-timezone" => v(fnlib::adjust(&*a(0)?, tz(1)?, None)?),
            "localname" | "namespace" => {
                let Value::Iri(i) = a(0)?.into_owned() else {
                    return Err(TypeError);
                };
                let cut = i.rfind(['#', '/', ':']).map_or(0, |p| p + 1);
                Ok(s(if l == "localname" {
                    &i[cut..]
                } else {
                    &i[..cut]
                }))
            }
            "now" => fb(Function::Now),
            "sqrt" => d(dbl(0)?.sqrt()),
            "pi" => d(std::f64::consts::PI),
            "e" => d(std::f64::consts::E),
            "min" | "max" => {
                let (x, y) = (a(0)?.into_owned(), a(1)?.into_owned());
                let o = compare(&x, &y)?.ok_or(TypeError)?;
                let pick_x = if l == "min" {
                    o != Ordering::Greater
                } else {
                    o != Ordering::Less
                };
                Ok(Val::V(if pick_x { x } else { y }))
            }
            "strjoin" => {
                let sep = a(0)?.lexical()?.to_string();
                let mut parts = Vec::new();
                for i in 1..args.len() {
                    parts.push(a(i)?.lexical()?.to_string());
                }
                Ok(s(parts.join(&sep)))
            }
            "bnode" => {
                let id = eval(&args[0], row, ctx)?.into_id(ctx);
                if ctx.kind(id) != TermKind::BNode {
                    return Err(TypeError);
                }
                Ok(s(crate::store::bnode_for(id).as_str()))
            }
            _ => Err(TypeError),
        };
    }
    if let Some(Ok(v)) = is_cast(iri).then(|| a(0)) {
        return cast(&NamedNode::new_unchecked(iri), v.into_owned(), ctx);
    }
    Err(TypeError)
}

/// Compile a spargebra expression. `var` maps variable names to ids; `subst` provides
/// constant substitutions (EXISTS evaluation); `exists` builds EXISTS specs.
pub struct Compiler<'a> {
    pub ctx: &'a Ctx,
    pub subst: &'a FxHashMap<VarId, Id>,
    pub exists: &'a dyn Fn(&GraphPattern) -> Arc<ExistsSpec>,
}

impl Compiler<'_> {
    pub fn compile(&self, e: &Expression) -> Expr {
        use Expression as E;
        let bx = |e: &Expression| Box::new(self.compile(e));
        match e {
            E::NamedNode(n) => Expr::Const(self.ctx.intern_term(&Term::NamedNode(n.clone()))),
            E::Literal(l) => {
                let t = Term::Literal(l.clone());
                let id = self.ctx.intern_term(&t);
                if id.is_inline() {
                    Expr::Const(id)
                } else {
                    Expr::Lit(id, Value::from_term(&t))
                }
            }
            E::Variable(v) => {
                let id = self.ctx.var(v.as_str());
                match self.subst.get(&id) {
                    Some(c) => Expr::Const(*c),
                    None => Expr::Var(id),
                }
            }
            E::Or(a, c) => Expr::Or(bx(a), bx(c)),
            E::And(a, c) => Expr::And(bx(a), bx(c)),
            E::Equal(a, c) => Expr::Eq(bx(a), bx(c)),
            E::SameTerm(a, c) => Expr::SameTerm(bx(a), bx(c)),
            E::Greater(a, c) => Expr::Cmp(bx(a), bx(c), CmpOp::Gt),
            E::GreaterOrEqual(a, c) => Expr::Cmp(bx(a), bx(c), CmpOp::Ge),
            E::Less(a, c) => Expr::Cmp(bx(a), bx(c), CmpOp::Lt),
            E::LessOrEqual(a, c) => Expr::Cmp(bx(a), bx(c), CmpOp::Le),
            E::In(a, l) => Expr::In(bx(a), l.iter().map(|x| self.compile(x)).collect()),
            E::Add(a, c) => Expr::Arith(bx(a), bx(c), ArithOp::Add),
            E::Subtract(a, c) => Expr::Arith(bx(a), bx(c), ArithOp::Sub),
            E::Multiply(a, c) => Expr::Arith(bx(a), bx(c), ArithOp::Mul),
            E::Divide(a, c) => Expr::Arith(bx(a), bx(c), ArithOp::Div),
            E::UnaryPlus(a) => Expr::Pos(bx(a)),
            E::UnaryMinus(a) => Expr::Neg(bx(a)),
            E::Not(a) => Expr::Not(bx(a)),
            E::Exists(p) => Expr::Exists((self.exists)(p)),
            E::Bound(v) => {
                let id = self.ctx.var(v.as_str());
                if self.subst.contains_key(&id) {
                    Expr::Const(Id::from_bool(true))
                } else {
                    Expr::Bound(id)
                }
            }
            E::If(a, c, d) => Expr::If(bx(a), bx(c), bx(d)),
            E::Coalesce(l) => Expr::Coalesce(l.iter().map(|x| self.compile(x)).collect()),
            E::FunctionCall(f, args) => {
                let args: Vec<Expr> = args.iter().map(|x| self.compile(x)).collect();
                let f = match f {
                    Function::Custom(n) if is_cast(n.as_str()) => Func::Cast(n.clone()),
                    Function::Custom(n) => Func::Ext(n.as_str().to_string()),
                    f => Func::Builtin(f.clone()),
                };
                Expr::Call(f, args)
            }
        }
    }
}

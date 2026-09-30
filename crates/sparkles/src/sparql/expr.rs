//! Compiled SPARQL expressions and the function library (ARQ `expr` + `function`).

use super::ctx::{Ctx, TermKind};
use super::table::{Table, VarId};
use super::value::{EvalResult, Num, NumOp, TypeError, Value, arith, compare, equals};
use crate::id::Id;
use oxrdf::vocab::xsd;
use oxrdf::{Literal, NamedNode, Term};
use oxsdatatypes::*;
use parking_lot::Mutex;
use rustc_hash::FxHashMap;
use spargebra::algebra::{Expression, Function, GraphPattern};
use std::cmp::Ordering;
use std::fmt::Write as _;
use std::sync::Arc;

/// Pattern of an `EXISTS` / `NOT EXISTS`, evaluated by substitution (with memoization).
pub struct ExistsSpec {
    pub pattern: GraphPattern,
    pub graph: super::plan::ActiveGraph,
    /// variables of the pattern that may be substituted from the outer row
    pub vars: Vec<VarId>,
    pub memo: Mutex<FxHashMap<Vec<Id>, bool>>,
}

#[derive(Clone)]
pub enum Expr {
    Const(Id),
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
    pub fn vars(&self, out: &mut Vec<VarId>) {
        match self {
            Expr::Const(_) => {}
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
            Expr::Const(_) | Expr::Var(_) | Expr::Bound(_) => false,
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
            Expr::Const(id) => match ctx.term(*id) {
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
}

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
}

impl Val {
    fn value(self, ctx: &Ctx) -> EvalResult<Value> {
        match self {
            Val::Id(id) => ctx.value(id).ok_or(TypeError),
            Val::V(v) => Ok(v),
        }
    }
    pub fn into_id(self, ctx: &Ctx) -> Id {
        match self {
            Val::Id(id) => id,
            Val::V(v) => ctx.intern_value(&v),
        }
    }
}

fn b(v: bool) -> Val {
    Val::Id(Id::from_bool(v))
}

pub fn eval(e: &Expr, row: &Row<'_>, ctx: &Ctx) -> EvalResult<Val> {
    match e {
        Expr::Const(id) => Ok(Val::Id(*id)),
        Expr::Var(v) => {
            let id = row.get(*v);
            if id.is_undef() {
                Err(TypeError)
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
                    return d.checked_sub(*e).map(|r| Val::V(Value::DayTime(r))).ok_or(TypeError);
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
                    return d.checked_add_duration(*e).map(|r| Val::V(Value::DateTime(r))).ok_or(TypeError);
                }
                (Value::DateTime(d), Value::Duration(e), NumOp::Sub) => {
                    return d.checked_sub_duration(*e).map(|r| Val::V(Value::DateTime(r))).ok_or(TypeError);
                }
                (Value::Date(d), Value::Date(e), NumOp::Sub) => {
                    return d.checked_sub(*e).map(|r| Val::V(Value::DayTime(r))).ok_or(TypeError);
                }
                (Value::DayTime(d), Value::DayTime(e), NumOp::Add) => {
                    return d.checked_add(*e).map(|r| Val::V(Value::DayTime(r))).ok_or(TypeError);
                }
                (Value::DayTime(d), Value::DayTime(e), NumOp::Sub) => {
                    return d.checked_sub(*e).map(|r| Val::V(Value::DayTime(r))).ok_or(TypeError);
                }
                (Value::YearMonth(d), Value::YearMonth(e), NumOp::Add) => {
                    return d.checked_add(*e).map(|r| Val::V(Value::YearMonth(r))).ok_or(TypeError);
                }
                (Value::YearMonth(d), Value::YearMonth(e), NumOp::Sub) => {
                    return d.checked_sub(*e).map(|r| Val::V(Value::YearMonth(r))).ok_or(TypeError);
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
            let key: Vec<Id> = spec.vars.iter().map(|&v| row.get(v)).collect();
            if let Some(&r) = spec.memo.lock().get(&key) {
                return Ok(b(r));
            }
            let r = super::plan::eval_exists(ctx, spec, &key).map_err(|_| TypeError)?;
            let mut m = spec.memo.lock();
            if m.len() < 100_000 {
                m.insert(key, r);
            }
            Ok(b(r))
        }
        Expr::Call(f, args) => call(f, args, row, ctx),
    }
}

pub fn ebv(e: &Expr, row: &Row<'_>, ctx: &Ctx) -> EvalResult<bool> {
    match eval(e, row, ctx)? {
        Val::Id(id) if id.tag() == crate::id::Tag::Bool => Ok(id.as_bool()),
        v => v.value(ctx)?.ebv(),
    }
}

fn val_eq(x: Val, y: Val, ctx: &Ctx) -> EvalResult<bool> {
    if let (Val::Id(a), Val::Id(c)) = (&x, &y) {
        if a == c {
            return Ok(true);
        }
        // IRIs / bnodes with different ids are different terms
        let (ka, kc) = (ctx.kind(*a), ctx.kind(*c));
        if ka != TermKind::Literal || kc != TermKind::Literal {
            return Ok(false);
        }
    }
    equals(&x.value(ctx)?, &y.value(ctx)?)
}

fn val_cmp(x: Val, y: Val, ctx: &Ctx) -> EvalResult<Option<Ordering>> {
    if let (Val::Id(a), Val::Id(c)) = (&x, &y) {
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

fn arg(args: &[Expr], i: usize, row: &Row<'_>, ctx: &Ctx) -> EvalResult<Value> {
    eval(args.get(i).ok_or(TypeError)?, row, ctx)?.value(ctx)
}

/// SPARQL 17.4.3.1.2 argument compatibility.
fn compatible(a: Option<&str>, b: Option<&str>) -> bool {
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

thread_local! {
    static REGEX_CACHE: std::cell::RefCell<FxHashMap<(String, String), Option<regex::Regex>>> =
        std::cell::RefCell::new(FxHashMap::default());
}

pub fn compile_regex(pattern: &str, flags: &str) -> EvalResult<regex::Regex> {
    REGEX_CACHE.with(|c| {
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
            })
            .clone()
            .ok_or(TypeError)
    })
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

fn lang_matches(tag: &str, range: &str) -> bool {
    if range == "*" {
        return !tag.is_empty();
    }
    let (t, r) = (tag.to_ascii_lowercase(), range.to_ascii_lowercase());
    t == r || (t.starts_with(&r) && t.as_bytes().get(r.len()) == Some(&b'-'))
}

fn round_half_up_double(d: f64) -> f64 {
    (d + 0.5).floor()
}

fn call(f: &Func, args: &[Expr], row: &Row<'_>, ctx: &Ctx) -> EvalResult<Val> {
    match f {
        Func::Builtin(f) => builtin(f, args, row, ctx),
        Func::Cast(dt) => cast(dt, arg(args, 0, row, ctx)?),
        Func::Ext(iri) => extension(iri, args, row, ctx),
    }
}

fn builtin(f: &Function, args: &[Expr], row: &Row<'_>, ctx: &Ctx) -> EvalResult<Val> {
    use Function as F;
    let a0 = || arg(args, 0, row, ctx);
    let a1 = || arg(args, 1, row, ctx);
    let a2 = || arg(args, 2, row, ctx);
    let id0 = || -> EvalResult<Id> { Ok(eval(args.first().ok_or(TypeError)?, row, ctx)?.into_id(ctx)) };
    Ok(match f {
        F::Str => {
            let v = eval(&args[0], row, ctx)?;
            if let Val::Id(id) = &v
                && let Some(Term::Literal(l)) = ctx.term(*id)
            {
                return Ok(s(l.value()));
            }
            s(v.value(ctx)?.lexical()?)
        }
        F::Lang => match a0()? {
            Value::Lang(_, l) => s(l),
            v if v.is_literal() => s(""),
            _ => return Err(TypeError),
        },
        F::LangMatches => {
            let t = a0()?;
            let r = a1()?;
            b(lang_matches(t.as_str().ok_or(TypeError)?, r.as_str().ok_or(TypeError)?))
        }
        F::Datatype => {
            let v = eval(&args[0], row, ctx)?;
            let dt = match &v {
                Val::Id(id) => match ctx.term(*id) {
                    Some(Term::Literal(l)) => l.datatype().into_owned(),
                    _ => return Err(TypeError),
                },
                Val::V(v) => v.datatype()?,
            };
            Val::V(Value::Iri(dt.as_str().into()))
        }
        F::Iri => match a0()? {
            Value::Iri(i) => Val::V(Value::Iri(i)),
            Value::Str(st) => {
                let iri = match &ctx.base_iri {
                    Some(base) => base.resolve(&st).map_err(|_| TypeError)?.into_inner(),
                    None => oxiri::Iri::parse(st.to_string()).map_err(|_| TypeError)?.into_inner(),
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
                st.as_str().ok_or(TypeError)?;
                Val::Id(ctx.fresh_bnode())
            }
        }
        F::Rand => Val::V(Value::Double(rand::random::<f64>().into())),
        F::Abs => Val::V(match Num::of(&a0()?)? {
            Num::Integer(i) => Value::Integer(i.checked_abs().ok_or(TypeError)?),
            Num::Decimal(d) => Value::Decimal(d.checked_abs().ok_or(TypeError)?),
            Num::Float(f) => Value::Float(f.abs()),
            Num::Double(d) => Value::Double(d.abs()),
        }),
        F::Ceil => Val::V(match Num::of(&a0()?)? {
            Num::Integer(i) => Value::Integer(i),
            Num::Decimal(d) => Value::Decimal(d.checked_ceil().ok_or(TypeError)?),
            Num::Float(f) => Value::Float(f.ceil()),
            Num::Double(d) => Value::Double(d.ceil()),
        }),
        F::Floor => Val::V(match Num::of(&a0()?)? {
            Num::Integer(i) => Value::Integer(i),
            Num::Decimal(d) => Value::Decimal(d.checked_floor().ok_or(TypeError)?),
            Num::Float(f) => Value::Float(f.floor()),
            Num::Double(d) => Value::Double(d.floor()),
        }),
        F::Round => Val::V(match Num::of(&a0()?)? {
            Num::Integer(i) => Value::Integer(i),
            Num::Decimal(d) => Value::Decimal(d.checked_round().ok_or(TypeError)?),
            Num::Float(f) => Value::Float((round_half_up_double(f64::from(f)) as f32).into()),
            Num::Double(d) => Value::Double(round_half_up_double(d.into()).into()),
        }),
        F::Concat => {
            let mut out = String::new();
            let mut lang: Option<Option<Arc<str>>> = None;
            for i in 0..args.len() {
                let v = arg(args, i, row, ctx)?;
                let (st, l) = v.string_arg()?;
                out.push_str(st);
                let l: Option<Arc<str>> = l.map(Into::into);
                lang = match lang {
                    None => Some(l),
                    Some(prev) if prev == l => Some(prev),
                    Some(_) => Some(None),
                };
            }
            same_kind(lang.flatten().as_deref(), out)
        }
        F::SubStr => {
            let v = a0()?;
            let (st, l) = v.string_arg()?;
            let start = Num::of(&a1()?)?.to_double();
            let len = if args.len() > 2 {
                Some(f64::from(Num::of(&a2()?)?.to_double()))
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
            Val::V(Value::Integer((v.string_arg()?.0.chars().count() as i64).into()))
        }
        F::Replace => {
            let v = a0()?;
            let (st, l) = v.string_arg()?;
            let p = a1()?;
            let r = a2()?;
            let flags = if args.len() > 3 {
                arg(args, 3, row, ctx)?.as_str().ok_or(TypeError)?.to_string()
            } else {
                String::new()
            };
            let re = compile_regex(p.as_str().ok_or(TypeError)?, &flags)?;
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
                    Some(i) => same_kind(if i == 0 && ys.is_empty() { xl } else { xl }, xs[..i].to_string()),
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
            let (y, mo, d, h, mi, se, tz) = match &v {
                Value::DateTime(dt) => (
                    dt.year(),
                    dt.month(),
                    dt.day(),
                    dt.hour(),
                    dt.minute(),
                    dt.second(),
                    dt.timezone(),
                ),
                Value::Date(dt) => (dt.year(), dt.month(), dt.day(), 0, 0, Decimal::from(0), dt.timezone()),
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
                _ => s(match &v {
                    Value::DateTime(dt) => dt.timezone_offset().map(|t| t.to_string()).unwrap_or_default(),
                    Value::Date(dt) => dt.timezone_offset().map(|t| t.to_string()).unwrap_or_default(),
                    Value::Time(dt) => dt.timezone_offset().map(|t| t.to_string()).unwrap_or_default(),
                    _ => String::new(),
                }),
            }
        }
        F::Now => Val::V(Value::DateTime(ctx.now)),
        F::Uuid => Val::V(Value::Iri(format!("urn:uuid:{}", uuid::Uuid::new_v4()).into())),
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
            let Value::Iri(dt) = dt else { return Err(TypeError) };
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
            let flags = if args.len() > 2 {
                a2()?.as_str().ok_or(TypeError)?.to_string()
            } else {
                String::new()
            };
            b(compile_regex(p.as_str().ok_or(TypeError)?, &flags)?.is_match(st))
        }
        F::Custom(iri) => return extension(iri.as_str(), args, row, ctx),
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
        )
    })
}

/// XSD casts (SPARQL 17.5).
pub fn cast(dt: &NamedNode, v: Value) -> EvalResult<Val> {
    let local = dt.as_str().strip_prefix(XSD).ok_or(TypeError)?;
    if let Value::BNode(_) = v {
        return Err(TypeError);
    }
    if let Value::Iri(i) = &v {
        return if local == "string" { Ok(s(i.clone())) } else { Err(TypeError) };
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
        "integer" | "int" | "long" | "short" | "byte" => {
            let r = match (&v, num) {
                (Value::Bool(x), _) => Value::Integer((*x as i64).into()),
                (_, Some(Num::Integer(i))) => Value::Integer(i),
                (_, Some(Num::Decimal(d))) => Value::Integer(Integer::try_from(d).map_err(|_| TypeError)?),
                (_, Some(Num::Float(f))) => Value::Integer(Integer::try_from(f).map_err(|_| TypeError)?),
                (_, Some(Num::Double(d))) => Value::Integer(Integer::try_from(d).map_err(|_| TypeError)?),
                _ if from_str => match parsed(xsd::INTEGER)? {
                    Val::V(v) => v,
                    _ => return Err(TypeError),
                },
                _ => return Err(TypeError),
            };
            if local == "integer" {
                Val::V(r)
            } else {
                Val::V(Value::from_literal(&Literal::new_typed_literal(
                    r.lexical()?.to_string(),
                    dt.clone(),
                )))
            }
        }
        "decimal" => match (&v, num) {
            (Value::Bool(x), _) => Val::V(Value::Decimal((*x as i64).into())),
            (_, Some(Num::Integer(i))) => Val::V(Value::Decimal(i.into())),
            (_, Some(Num::Decimal(d))) => Val::V(Value::Decimal(d)),
            (_, Some(Num::Float(f))) => Val::V(Value::Decimal(Decimal::try_from(f).map_err(|_| TypeError)?)),
            (_, Some(Num::Double(d))) => Val::V(Value::Decimal(Decimal::try_from(d).map_err(|_| TypeError)?)),
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
            Value::Date(d) => Val::V(Value::DateTime(DateTime::try_from(*d).map_err(|_| TypeError)?)),
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
            Value::Duration(d) => Val::V(Value::DayTime(DayTimeDuration::try_from(*d).map_err(|_| TypeError)?)),
            _ if from_str => return parsed(xsd::DAY_TIME_DURATION),
            _ => return Err(TypeError),
        },
        "yearMonthDuration" => match &v {
            Value::YearMonth(d) => Val::V(Value::YearMonth(*d)),
            Value::Duration(d) => {
                Val::V(Value::YearMonth(YearMonthDuration::try_from(*d).map_err(|_| TypeError)?))
            }
            _ if from_str => return parsed(xsd::YEAR_MONTH_DURATION),
            _ => return Err(TypeError),
        },
        _ => return Err(TypeError),
    })
}

/// Is `iri` a supported extension function (fn:, math:, afn:)?
pub fn is_extension(iri: &str) -> bool {
    iri.starts_with(FN) || iri.starts_with(MATH) || iri.starts_with(AFN)
}

fn extension(iri: &str, args: &[Expr], row: &Row<'_>, ctx: &Ctx) -> EvalResult<Val> {
    let a = |i: usize| arg(args, i, row, ctx);
    let dbl = |i: usize| -> EvalResult<f64> { Ok(Num::of(&a(i)?)?.to_double().into()) };
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
    if let Some(l) = iri.strip_prefix(FN) {
        return match l {
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
                Ok(same_kind(l, st.split_whitespace().collect::<Vec<_>>().join(" ")))
            }
            "string-join" => {
                let sep = if args.len() > 1 { a(args.len() - 1)?.lexical()?.to_string() } else { String::new() };
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
    if let Some(l) = iri.strip_prefix(AFN) {
        return match l {
            "localname" | "namespace" => {
                let Value::Iri(i) = a(0)? else { return Err(TypeError) };
                let cut = i.rfind(['#', '/', ':']).map_or(0, |p| p + 1);
                Ok(s(if l == "localname" { &i[cut..] } else { &i[..cut] }))
            }
            "now" => fb(Function::Now),
            "sqrt" => d(dbl(0)?.sqrt()),
            "pi" => d(std::f64::consts::PI),
            "e" => d(std::f64::consts::E),
            "min" | "max" => {
                let (x, y) = (a(0)?, a(1)?);
                let o = compare(&x, &y)?.ok_or(TypeError)?;
                let pick_x = if l == "min" { o != Ordering::Greater } else { o != Ordering::Less };
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
        return cast(&NamedNode::new_unchecked(iri), v);
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
            E::Literal(l) => Expr::Const(self.ctx.intern_term(&Term::Literal(l.clone()))),
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

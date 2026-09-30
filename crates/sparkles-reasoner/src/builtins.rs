//! Jena rule builtins (`org.apache.jena.reasoner.rulesys.builtins`).

use crate::engine::{Eval, Slot};
use crate::terms::{Kind, Terms};
use oxrdf::{Literal, NamedNode, Term};
use sparkles::sparql::value::{self, NumOp, Value};
use std::cmp::Ordering;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BuiltinKind {
    Equal,
    NotEqual,
    LessThan,
    GreaterThan,
    Le,
    Ge,
    Sum,
    Difference,
    Product,
    Quotient,
    Min,
    Max,
    AddOne,
    StrConcat,
    UriConcat,
    Regex,
    IsLiteral,
    NotLiteral,
    IsBNode,
    NotBNode,
    IsDType,
    NotDType,
    IsFunctor,
    NotFunctor,
    NoValue,
    MakeTemp,
    MakeSkolem,
    Now,
    ListMember,
    ListContains,
    ListNotContains,
    ListLength,
    ListEntry,
    ListForAll,
}

use BuiltinKind::*;

impl BuiltinKind {
    pub fn from_name(name: &str) -> Option<BuiltinKind> {
        Some(match name {
            "equal" => Equal,
            "notEqual" => NotEqual,
            "lessThan" => LessThan,
            "greaterThan" => GreaterThan,
            "le" => Le,
            "ge" => Ge,
            "sum" => Sum,
            "difference" => Difference,
            "product" => Product,
            "quotient" => Quotient,
            "min" => Min,
            "max" => Max,
            "addOne" => AddOne,
            "strConcat" => StrConcat,
            "uriConcat" => UriConcat,
            "regex" => Regex,
            "isLiteral" => IsLiteral,
            "notLiteral" => NotLiteral,
            "isBNode" => IsBNode,
            "notBNode" => NotBNode,
            "isDType" => IsDType,
            "notDType" => NotDType,
            "isFunctor" => IsFunctor,
            "notFunctor" => NotFunctor,
            "noValue" => NoValue,
            "makeTemp" => MakeTemp,
            "makeInstance" => MakeSkolem,
            "makeSkolem" => MakeSkolem,
            "now" => Now,
            "listMember" => ListMember,
            "listContains" => ListContains,
            "listNotContains" => ListNotContains,
            "listLength" => ListLength,
            "listEntry" => ListEntry,
            "listForAll" => ListForAll,
            _ => return None,
        })
    }

    pub fn check_arity(self, n: usize) -> Result<(), String> {
        let ok = match self {
            Equal | NotEqual | LessThan | GreaterThan | Le | Ge | IsDType | NotDType | ListMember
            | ListContains | ListNotContains | ListLength | AddOne => n == 2,
            Sum | Difference | Product | Quotient | Min | Max | ListEntry | ListForAll => n == 3,
            IsLiteral | NotLiteral | IsBNode | NotBNode | IsFunctor | NotFunctor | Now => n == 1,
            NoValue => n == 2 || n == 3,
            StrConcat | UriConcat | MakeTemp | MakeSkolem => n >= 1,
            Regex => n >= 2,
        };
        if ok { Ok(()) } else { Err(format!("wrong number of arguments ({n})")) }
    }

    pub fn input_positions(self, n: usize) -> Vec<usize> {
        match self {
            Sum | Difference | Product | Quotient | Min | Max => vec![0, 1],
            AddOne | ListMember | ListLength => vec![0],
            ListEntry => vec![0, 1],
            StrConcat | UriConcat => (0..n.saturating_sub(1)).collect(),
            Regex => vec![0, 1],
            MakeTemp | Now => vec![],
            MakeSkolem => (1..n).collect(),
            _ => (0..n).collect(),
        }
    }

    pub fn output_positions(self, n: usize) -> Vec<usize> {
        match self {
            Sum | Difference | Product | Quotient | Min | Max | ListEntry => vec![2],
            AddOne | ListMember | ListLength => vec![1],
            StrConcat | UriConcat => vec![n - 1],
            Regex => (2..n).collect(),
            MakeTemp => (0..n).collect(),
            MakeSkolem | Now => vec![0],
            _ => vec![],
        }
    }
}

/// Builtins allowed in heads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HeadAction {
    /// `listMapAsSubject(?list, ?p, ?o)`: assert `(m ?p ?o)` for each member m
    ListMapAsSubject,
    /// `listMapAsObject(?s, ?p, ?list)`: assert `(?s ?p m)` for each member m
    ListMapAsObject,
}

impl HeadAction {
    /// `Some(Ok)` = supported, `Some(Err)` = known but ignored (no effect on
    /// materialization, or non-monotonic), `None` = unknown.
    pub fn from_name(name: &str) -> Option<Result<HeadAction, ()>> {
        Some(match name {
            "listMapAsSubject" => Ok(HeadAction::ListMapAsSubject),
            "listMapAsObject" => Ok(HeadAction::ListMapAsObject),
            "print" | "drop" | "remove" | "hide" | "table" | "tableAll" => Err(()),
            _ => return None,
        })
    }
}

pub(crate) fn head_action(ev: &mut Eval<'_>, action: HeadAction, vals: &[u64]) {
    let t = ev.terms;
    let list_of = |ev: &Eval<'_>, l: u64| ev.g.list(l, t.rdf_first, t.rdf_rest, t.rdf_nil, ev.end);
    match action {
        HeadAction::ListMapAsSubject => {
            if let Some(ms) = list_of(ev, vals[0]) {
                ev.out.extend(ms.into_iter().map(|m| [m, vals[1], vals[2]]));
            }
        }
        HeadAction::ListMapAsObject => {
            if let Some(ms) = list_of(ev, vals[2]) {
                ev.out.extend(ms.into_iter().map(|m| [vals[0], vals[1], m]));
            }
        }
    }
}

// ------------------------------------------------------------------ helpers ----

/// Jena `equal`: same term, or equal values for comparable literals.
pub(crate) fn same_value(t: &Terms, a: u64, b: u64) -> bool {
    if a == b {
        return true;
    }
    if a == 0 || b == 0 || t.kind(a) != Kind::Literal || t.kind(b) != Kind::Literal {
        return false;
    }
    match (t.value(a), t.value(b)) {
        (Some(x), Some(y)) => value::equals(&x, &y).unwrap_or(false),
        _ => false,
    }
}

fn compare(t: &Terms, a: u64, b: u64) -> Option<Ordering> {
    if t.kind(a) != Kind::Literal || t.kind(b) != Kind::Literal {
        return None;
    }
    value::compare(&t.value(a)?, &t.value(b)?).ok().flatten()
}

fn num(t: &Terms, a: u64) -> Option<Value> {
    if t.kind(a) != Kind::Literal {
        return None;
    }
    t.value(a).filter(|v| v.is_numeric())
}

fn arith(t: &Terms, kind: BuiltinKind, a: u64, b: u64) -> Option<u64> {
    let (x, y) = (num(t, a)?, num(t, b)?);
    let r = match (kind, &x, &y) {
        // Jena: long division for integers
        (Quotient, Value::Integer(i), Value::Integer(j)) => Value::Integer(i.checked_div(*j)?),
        _ => {
            let op = match kind {
                Sum => NumOp::Add,
                Difference => NumOp::Sub,
                Product => NumOp::Mul,
                Quotient => NumOp::Div,
                _ => return None,
            };
            value::arith(op, &x, &y).ok()?
        }
    };
    if let Value::Double(d) = &r
        && !f64::from(*d).is_finite()
    {
        return None;
    }
    Some(t.id_for(&r.to_term()))
}

fn is_dtype(t: &Terms, v: u64, dt: u64) -> bool {
    if t.kind(v) != Kind::Literal {
        return false;
    }
    let (Some(Term::Literal(l)), Some(Term::NamedNode(d))) = (t.term(v), t.term(dt)) else {
        return false;
    };
    let d = d.as_str();
    if d == "http://www.w3.org/2000/01/rdf-schema#Literal" || l.datatype().as_str() == d {
        return true;
    }
    if l.language().is_some() {
        return d == "http://www.w3.org/1999/02/22-rdf-syntax-ns#langString";
    }
    // derived / compatible XSD types: the lexical form must be valid for the target
    // type and the value spaces must be related (integer family ⊂ decimal)
    let src = Value::from_literal(&l);
    let target = Value::from_literal(&Literal::new_typed_literal(l.value(), NamedNode::new_unchecked(d)));
    match (&src, &target) {
        (Value::Integer(_), Value::Integer(_)) | (Value::Integer(_), Value::Decimal(_)) => {
            integer_in_range(l.value(), d)
        }
        (Value::Decimal(_), Value::Decimal(_)) => true,
        _ => false,
    }
}

fn integer_in_range(lex: &str, dt: &str) -> bool {
    let Ok(v) = lex.trim().parse::<i128>() else {
        // larger than i128: only unbounded types
        return dt.ends_with("#integer") || dt.ends_with("#decimal");
    };
    let local = dt.rsplit('#').next().unwrap_or("");
    match local {
        "integer" | "decimal" => true,
        "long" => i64::try_from(v).is_ok(),
        "int" => i32::try_from(v).is_ok(),
        "short" => i16::try_from(v).is_ok(),
        "byte" => i8::try_from(v).is_ok(),
        "nonNegativeInteger" => v >= 0,
        "positiveInteger" => v > 0,
        "nonPositiveInteger" => v <= 0,
        "negativeInteger" => v < 0,
        "unsignedLong" => u64::try_from(v).is_ok(),
        "unsignedInt" => u32::try_from(v).is_ok(),
        "unsignedShort" => u16::try_from(v).is_ok(),
        "unsignedByte" => u8::try_from(v).is_ok(),
        _ => false,
    }
}

fn string_id(t: &Terms, s: String) -> u64 {
    t.id_for(&Term::Literal(Literal::new_simple_literal(s)))
}

// --------------------------------------------------------------- evaluation ----

/// Evaluate builtin `j` at plan step `k`, continuing the plan for each solution.
pub(crate) fn eval(ev: &mut Eval<'_>, j: usize, k: usize, b: &mut [u64]) {
    let rule = ev.rule;
    let bi = &rule.builtins[j];
    let t = ev.terms;
    let a = |i: usize| ev.val(bi.args[i], b);
    let pass = match bi.kind {
        Equal => same_value(t, a(0), a(1)),
        NotEqual => !same_value(t, a(0), a(1)),
        LessThan => compare(t, a(0), a(1)) == Some(Ordering::Less),
        GreaterThan => compare(t, a(0), a(1)) == Some(Ordering::Greater),
        Le => matches!(compare(t, a(0), a(1)), Some(Ordering::Less | Ordering::Equal)),
        Ge => matches!(compare(t, a(0), a(1)), Some(Ordering::Greater | Ordering::Equal)),
        IsLiteral => t.kind(a(0)) == Kind::Literal,
        NotLiteral => t.kind(a(0)) != Kind::Literal,
        IsBNode => t.kind(a(0)) == Kind::BNode,
        NotBNode => t.kind(a(0)) != Kind::BNode,
        IsFunctor => false,
        NotFunctor => true,
        IsDType => is_dtype(t, a(0), a(1)),
        NotDType => !is_dtype(t, a(0), a(1)),
        NoValue => {
            let opt = |i: usize| bi.args.get(i).map(|s| ev.val(*s, b)).filter(|&v| v != 0);
            let (s, p, o) = (opt(0), opt(1), opt(2));
            let c = ev.g.cands(s, p, o, 0, ev.end);
            !(0..c.len()).any(|i| {
                let tr = ev.g.triples[c.get(i) as usize];
                s.is_none_or(|x| x == tr[0]) && p.is_none_or(|x| x == tr[1]) && o.is_none_or(|x| x == tr[2])
            })
        }
        ListContains | ListNotContains => {
            let found = ev
                .g
                .list(a(0), t.rdf_first, t.rdf_rest, t.rdf_nil, ev.end)
                .is_some_and(|ms| ms.iter().any(|&m| same_value(t, m, a(1))));
            found == (bi.kind == ListContains)
        }
        ListForAll => {
            let (s, p) = (a(1), a(2));
            ev.g.list(a(0), t.rdf_first, t.rdf_rest, t.rdf_nil, ev.end).is_some_and(|ms| {
                ms.iter().all(|&m| ev.g.position(&[s, p, m]).is_some_and(|i| i < ev.end))
            })
        }
        // ---- binders / generators
        Sum | Difference | Product | Quotient => {
            if let Some(r) = arith(t, bi.kind, a(0), a(1)) {
                ev.bind_and_continue(bi.args[2], r, k, b);
            }
            return;
        }
        Min | Max => {
            let (x, y) = (a(0), a(1));
            if num(t, x).is_some() && num(t, y).is_some() {
                let ord = compare(t, x, y);
                let r = match (bi.kind, ord) {
                    (Min, Some(Ordering::Greater)) | (Max, Some(Ordering::Less)) => y,
                    (_, Some(_)) => x,
                    (_, None) => return,
                };
                ev.bind_and_continue(bi.args[2], r, k, b);
            }
            return;
        }
        AddOne => {
            let one = sparkles::id::Id::from_i64(1).unwrap().0;
            if let Some(r) = arith(t, Sum, a(0), one) {
                ev.bind_and_continue(bi.args[1], r, k, b);
            }
            return;
        }
        StrConcat | UriConcat => {
            let n = bi.args.len();
            let mut s = String::new();
            for i in 0..n - 1 {
                match t.lexical(a(i)) {
                    Some(x) => s.push_str(&x),
                    None => return,
                }
            }
            let r = if bi.kind == StrConcat {
                string_id(t, s)
            } else {
                match NamedNode::new(s) {
                    Ok(n) => t.id_for(&Term::NamedNode(n)),
                    Err(_) => return,
                }
            };
            ev.bind_and_continue(bi.args[n - 1], r, k, b);
            return;
        }
        Regex => {
            let Some(text) = t.lexical(a(0)) else { return };
            let re = match &bi.regex {
                Some(r) => r.clone(),
                None => match t.lexical(a(1)).and_then(|p| t.regex(&p)) {
                    Some(r) => r,
                    None => return,
                },
            };
            let Some(caps) = re.captures(&text) else { return };
            let groups: Vec<u64> = (1..bi.args.len() - 1)
                .map(|g| string_id(t, caps.get(g).map_or(String::new(), |m| m.as_str().to_string())))
                .collect();
            bind_all(ev, &bi.args[2..], &groups, k, b);
            return;
        }
        MakeTemp => {
            let vals: Vec<u64> = (0..bi.args.len()).map(|_| t.fresh_bnode()).collect();
            bind_all(ev, &bi.args, &vals, k, b);
            return;
        }
        MakeSkolem => {
            let inputs: Vec<u64> = (1..bi.args.len()).map(a).collect();
            let v = t.skolem(&inputs);
            ev.bind_and_continue(bi.args[0], v, k, b);
            return;
        }
        Now => {
            ev.bind_and_continue(bi.args[0], t.now, k, b);
            return;
        }
        ListMember => {
            let Some(ms) = ev.g.list(a(0), t.rdf_first, t.rdf_rest, t.rdf_nil, ev.end) else { return };
            let mut seen = rustc_hash::FxHashSet::default();
            for m in ms {
                if seen.insert(m) {
                    ev.bind_and_continue(bi.args[1], m, k, b);
                }
            }
            return;
        }
        ListLength => {
            let Some(ms) = ev.g.list(a(0), t.rdf_first, t.rdf_rest, t.rdf_nil, ev.end) else { return };
            let n = sparkles::id::Id::from_i64(ms.len() as i64).unwrap().0;
            ev.bind_and_continue(bi.args[1], n, k, b);
            return;
        }
        ListEntry => {
            let Some(Value::Integer(i)) = num(t, a(1)) else { return };
            let i: i64 = i.into();
            let Some(ms) = ev.g.list(a(0), t.rdf_first, t.rdf_rest, t.rdf_nil, ev.end) else { return };
            if let Some(&m) = usize::try_from(i).ok().and_then(|i| ms.get(i)) {
                ev.bind_and_continue(bi.args[2], m, k, b);
            }
            return;
        }
    };
    if pass {
        ev.run(k + 1, b, None);
    }
}

/// Bind several output slots at once, then continue.
fn bind_all(ev: &mut Eval<'_>, slots: &[Slot], vals: &[u64], k: usize, b: &mut [u64]) {
    let mut newly = Vec::new();
    let mut ok = true;
    for (s, &v) in slots.iter().zip(vals) {
        match *s {
            Slot::Var(x) if b[x] == 0 => {
                b[x] = v;
                newly.push(x);
            }
            Slot::Any => {}
            s => {
                let cur = ev.val(s, b);
                if cur != v && !same_value(ev.terms, cur, v) {
                    ok = false;
                    break;
                }
            }
        }
    }
    if ok {
        ev.run(k + 1, b, None);
    }
    for x in newly {
        b[x] = 0;
    }
}

//! SPARQL expressions: a typed builder (the equivalent of Jena's `ExprFactory`) plus raw
//! expression strings.
//!
//! Anything implementing [`IntoExpr`] can be used where an expression is expected:
//!
//! * `&str` / `String` — SPARQL expression text such as `"?age > 30"` or
//!   `"STRLEN(?name)"`. A string that is a single term (`"?x"`, `"ex:p"`, `"42"`) becomes
//!   that term. Raw text is *syntax*: never splice untrusted data into it, use [`lit`]
//!   and friends instead.
//! * [`Node`] and everything that converts into one (`var("x")`, `iri(..)`, `lit(..)`,
//!   numbers, booleans, `oxrdf` terms).
//! * [`Expr`] values built with the functions in this module.
//!
//! Rendering adds the parentheses required by operator precedence, so
//! `and(or(a, b), c)` renders as `(a || b) && c`.
//!
//! ```
//! use sparkles::querybuilder::{expr, var, lit};
//! let e = expr::and(expr::gt(var("age"), 30), expr::regex(var("name"), "^A"));
//! assert_eq!(e.to_string(), r#"?age > 30 && REGEX(?name, "^A")"#);
//! let e = expr::mul(expr::add(var("a"), 1), var("b"));
//! assert_eq!(e.to_string(), "(?a + 1) * ?b");
//! ```

use std::fmt;

use super::pattern::WhereBuilder;
use super::term::{IntoNode, N, Node, lit, parse_term};

/// A SPARQL expression.
#[derive(Clone, Debug)]
pub struct Expr(pub(crate) E);

#[derive(Clone, Debug)]
pub(crate) enum E {
    /// Raw SPARQL expression text.
    Raw(String),
    Node(Node),
    Binary(BinOp, Box<Expr>, Box<Expr>),
    Unary(char, Box<Expr>),
    In(Box<Expr>, Vec<Expr>, bool),
    /// Built-in call (validated keyword) or IRI function call.
    Call(Callee, Vec<Expr>),
    Exists(Box<WhereBuilder>, bool),
    Aggregate {
        name: &'static str,
        distinct: bool,
        /// `None` is `*` (COUNT only).
        arg: Option<Box<Expr>>,
        separator: Option<String>,
    },
    Invalid(String),
}

#[derive(Clone, Debug)]
pub(crate) enum Callee {
    Builtin(String),
    Iri(Node),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BinOp {
    Or,
    And,
    Eq,
    Ne,
    Lt,
    Gt,
    Le,
    Ge,
    Add,
    Sub,
    Mul,
    Div,
}

impl BinOp {
    pub(crate) fn symbol(self) -> &'static str {
        match self {
            BinOp::Or => "||",
            BinOp::And => "&&",
            BinOp::Eq => "=",
            BinOp::Ne => "!=",
            BinOp::Lt => "<",
            BinOp::Gt => ">",
            BinOp::Le => "<=",
            BinOp::Ge => ">=",
            BinOp::Add => "+",
            BinOp::Sub => "-",
            BinOp::Mul => "*",
            BinOp::Div => "/",
        }
    }
    /// Precedence level and the minimum levels of the left / right operands.
    pub(crate) fn prec(self) -> (u8, u8, u8) {
        match self {
            BinOp::Or => (1, 1, 2),
            BinOp::And => (2, 2, 3),
            // Relational expressions are not associative in the grammar.
            BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge => (3, 4, 4),
            BinOp::Add | BinOp::Sub => (4, 4, 5),
            BinOp::Mul | BinOp::Div => (5, 5, 6),
        }
    }
}

/// Precedence levels: 0 raw text (always bracketed as an operand), 1 `||`, 2 `&&`,
/// 3 relational / `IN`, 4 additive, 5 multiplicative, 6 unary, 7 primary.
pub(crate) const PREC_UNARY: u8 = 6;
pub(crate) const PREC_PRIMARY: u8 = 7;

impl Expr {
    /// Raw SPARQL expression text (not validated until `build()`).
    pub fn raw(text: impl Into<String>) -> Expr {
        Expr(E::Raw(text.into()))
    }

    /// Marks an aggregate as `DISTINCT` (`COUNT(DISTINCT ?x)`); no effect on other
    /// expressions.
    pub fn distinct(mut self) -> Expr {
        if let E::Aggregate { distinct, .. } = &mut self.0 {
            *distinct = true;
        }
        self
    }

    /// Is this a bare variable?
    pub(crate) fn as_var(&self) -> Option<&Node> {
        match &self.0 {
            E::Node(n) if n.is_var() => Some(n),
            _ => None,
        }
    }
}

/// Renders the expression without resolving prefixes (for debugging; `build()` on a
/// query builder is the authoritative rendering).
impl fmt::Display for Expr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut r = super::render::Renderer::detached();
        f.write_str(&r.expr(self))
    }
}

// ------------------------------------------------------------------- IntoExpr ----

/// Conversion into an [`Expr`]; see the [module docs](self).
pub trait IntoExpr {
    fn into_expr(self) -> Expr;
}

impl IntoExpr for Expr {
    fn into_expr(self) -> Expr {
        self
    }
}
impl IntoExpr for &Expr {
    fn into_expr(self) -> Expr {
        self.clone()
    }
}

fn str_expr(s: &str) -> Expr {
    let n = parse_term(s);
    match n.0 {
        N::Path(_) | N::Invalid(_) | N::A | N::Anon | N::Undef => Expr(E::Raw(s.to_string())),
        _ => Expr(E::Node(n)),
    }
}
impl IntoExpr for &str {
    fn into_expr(self) -> Expr {
        str_expr(self)
    }
}
impl IntoExpr for String {
    fn into_expr(self) -> Expr {
        str_expr(&self)
    }
}
impl IntoExpr for &String {
    fn into_expr(self) -> Expr {
        str_expr(self)
    }
}

macro_rules! node_into_expr {
    ($($t:ty),* $(,)?) => {$(
        impl IntoExpr for $t {
            fn into_expr(self) -> Expr {
                Expr(E::Node(self.into_node()))
            }
        }
    )*};
}
node_into_expr!(
    Node,
    &Node,
    oxrdf::NamedNode,
    &oxrdf::NamedNode,
    oxrdf::NamedNodeRef<'_>,
    oxrdf::BlankNode,
    &oxrdf::BlankNode,
    oxrdf::Literal,
    &oxrdf::Literal,
    oxrdf::LiteralRef<'_>,
    oxrdf::Term,
    &oxrdf::Term,
    oxrdf::Variable,
    &oxrdf::Variable,
    bool,
    i8,
    i16,
    i32,
    i64,
    i128,
    isize,
    u8,
    u16,
    u32,
    u64,
    u128,
    usize,
    f32,
    f64,
);

// ------------------------------------------------------------------ operators ----

fn bin(op: BinOp, a: impl IntoExpr, b: impl IntoExpr) -> Expr {
    Expr(E::Binary(
        op,
        Box::new(a.into_expr()),
        Box::new(b.into_expr()),
    ))
}

/// `a || b`
pub fn or(a: impl IntoExpr, b: impl IntoExpr) -> Expr {
    bin(BinOp::Or, a, b)
}
/// `a && b`
pub fn and(a: impl IntoExpr, b: impl IntoExpr) -> Expr {
    bin(BinOp::And, a, b)
}
/// `e1 && e2 && …` (`true` when empty).
pub fn and_all<T: IntoExpr>(es: impl IntoIterator<Item = T>) -> Expr {
    fold(es, BinOp::And, true)
}
/// `e1 || e2 || …` (`false` when empty).
pub fn or_any<T: IntoExpr>(es: impl IntoIterator<Item = T>) -> Expr {
    fold(es, BinOp::Or, false)
}
fn fold<T: IntoExpr>(es: impl IntoIterator<Item = T>, op: BinOp, empty: bool) -> Expr {
    es.into_iter()
        .map(IntoExpr::into_expr)
        .reduce(|a, b| bin(op, a, b))
        .unwrap_or_else(|| empty.into_expr())
}
/// `!e`
pub fn not(e: impl IntoExpr) -> Expr {
    Expr(E::Unary('!', Box::new(e.into_expr())))
}
/// `a = b`
pub fn eq(a: impl IntoExpr, b: impl IntoExpr) -> Expr {
    bin(BinOp::Eq, a, b)
}
/// `a != b`
pub fn ne(a: impl IntoExpr, b: impl IntoExpr) -> Expr {
    bin(BinOp::Ne, a, b)
}
/// `a < b`
pub fn lt(a: impl IntoExpr, b: impl IntoExpr) -> Expr {
    bin(BinOp::Lt, a, b)
}
/// `a > b`
pub fn gt(a: impl IntoExpr, b: impl IntoExpr) -> Expr {
    bin(BinOp::Gt, a, b)
}
/// `a <= b`
pub fn le(a: impl IntoExpr, b: impl IntoExpr) -> Expr {
    bin(BinOp::Le, a, b)
}
/// `a >= b`
pub fn ge(a: impl IntoExpr, b: impl IntoExpr) -> Expr {
    bin(BinOp::Ge, a, b)
}
/// `a + b`
pub fn add(a: impl IntoExpr, b: impl IntoExpr) -> Expr {
    bin(BinOp::Add, a, b)
}
/// `a - b`
pub fn sub(a: impl IntoExpr, b: impl IntoExpr) -> Expr {
    bin(BinOp::Sub, a, b)
}
/// `a * b`
pub fn mul(a: impl IntoExpr, b: impl IntoExpr) -> Expr {
    bin(BinOp::Mul, a, b)
}
/// `a / b`
pub fn div(a: impl IntoExpr, b: impl IntoExpr) -> Expr {
    bin(BinOp::Div, a, b)
}
/// `-e`
pub fn neg(e: impl IntoExpr) -> Expr {
    Expr(E::Unary('-', Box::new(e.into_expr())))
}
/// `e IN (v1, v2, …)`
pub fn in_<T: IntoExpr>(e: impl IntoExpr, list: impl IntoIterator<Item = T>) -> Expr {
    Expr(E::In(
        Box::new(e.into_expr()),
        list.into_iter().map(IntoExpr::into_expr).collect(),
        false,
    ))
}
/// `e NOT IN (v1, v2, …)`
pub fn not_in<T: IntoExpr>(e: impl IntoExpr, list: impl IntoIterator<Item = T>) -> Expr {
    Expr(E::In(
        Box::new(e.into_expr()),
        list.into_iter().map(IntoExpr::into_expr).collect(),
        true,
    ))
}
/// `EXISTS { … }`
pub fn exists(f: impl FnOnce(WhereBuilder) -> WhereBuilder) -> Expr {
    Expr(E::Exists(Box::new(f(WhereBuilder::new())), false))
}
/// `NOT EXISTS { … }`
pub fn not_exists(f: impl FnOnce(WhereBuilder) -> WhereBuilder) -> Expr {
    Expr(E::Exists(Box::new(f(WhereBuilder::new())), true))
}

// ------------------------------------------------------------------ functions ----

/// A function call. `name` is either a built-in function keyword (`"STRLEN"`,
/// `"COALESCE"`, …) or a function IRI (`"<http://…>"`, `"xsd:integer"`, `"ex:fn"`).
pub fn func<T: IntoExpr>(name: &str, args: impl IntoIterator<Item = T>) -> Expr {
    let args = args.into_iter().map(IntoExpr::into_expr).collect();
    let callee = if name.contains(':') || name.starts_with('<') {
        let n = parse_term(name);
        match n.0 {
            N::Iri(_) | N::Prefixed(..) => Callee::Iri(n),
            _ => return Expr(E::Invalid(format!("invalid function name {name:?}"))),
        }
    } else if !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        Callee::Builtin(name.to_ascii_uppercase())
    } else {
        return Expr(E::Invalid(format!("invalid function name {name:?}")));
    };
    Expr(E::Call(callee, args))
}

fn call(name: &str, args: Vec<Expr>) -> Expr {
    Expr(E::Call(Callee::Builtin(name.to_string()), args))
}

macro_rules! fn1 {
    ($($(#[$m:meta])* $f:ident => $name:literal),* $(,)?) => {$(
        $(#[$m])*
        pub fn $f(e: impl IntoExpr) -> Expr {
            call($name, vec![e.into_expr()])
        }
    )*};
}
macro_rules! fn2 {
    ($($(#[$m:meta])* $f:ident => $name:literal),* $(,)?) => {$(
        $(#[$m])*
        pub fn $f(a: impl IntoExpr, b: impl IntoExpr) -> Expr {
            call($name, vec![a.into_expr(), b.into_expr()])
        }
    )*};
}

fn1! {
    /// `BOUND(?v)`
    bound => "BOUND",
    /// `isIRI(e)`
    is_iri => "isIRI",
    /// `isBLANK(e)`
    is_blank => "isBLANK",
    /// `isLITERAL(e)`
    is_literal => "isLITERAL",
    /// `isNUMERIC(e)`
    is_numeric => "isNUMERIC",
    /// `STR(e)`
    str => "STR",
    /// `LANG(e)`
    lang => "LANG",
    /// `DATATYPE(e)`
    datatype => "DATATYPE",
    /// `STRLEN(e)`
    strlen => "STRLEN",
    /// `UCASE(e)`
    ucase => "UCASE",
    /// `LCASE(e)`
    lcase => "LCASE",
    /// `ABS(e)`
    abs => "ABS",
    /// `ROUND(e)`
    round => "ROUND",
    /// `CEIL(e)`
    ceil => "CEIL",
    /// `FLOOR(e)`
    floor => "FLOOR",
    /// `YEAR(e)`
    year => "YEAR",
    /// `IRI(e)`
    to_iri => "IRI",
}

fn2! {
    /// `langMatches(a, b)`
    lang_matches => "langMatches",
    /// `CONTAINS(a, b)`
    contains => "CONTAINS",
    /// `STRSTARTS(a, b)`
    strstarts => "STRSTARTS",
    /// `STRENDS(a, b)`
    strends => "STRENDS",
    /// `sameTerm(a, b)`
    same_term => "sameTerm",
    /// `STRDT(a, b)`
    strdt => "STRDT",
    /// `STRLANG(a, b)`
    strlang => "STRLANG",
}

/// `REGEX(e, "pattern")`; the pattern is a plain string (escaped as a literal).
pub fn regex(e: impl IntoExpr, pattern: &str) -> Expr {
    call("REGEX", vec![e.into_expr(), lit(pattern).into_expr()])
}
/// `REGEX(e, "pattern", "flags")`
pub fn regex_flags(e: impl IntoExpr, pattern: &str, flags: &str) -> Expr {
    call(
        "REGEX",
        vec![
            e.into_expr(),
            lit(pattern).into_expr(),
            lit(flags).into_expr(),
        ],
    )
}
/// `CONCAT(e1, e2, …)`
pub fn concat<T: IntoExpr>(es: impl IntoIterator<Item = T>) -> Expr {
    call("CONCAT", es.into_iter().map(IntoExpr::into_expr).collect())
}
/// `COALESCE(e1, e2, …)`
pub fn coalesce<T: IntoExpr>(es: impl IntoIterator<Item = T>) -> Expr {
    call(
        "COALESCE",
        es.into_iter().map(IntoExpr::into_expr).collect(),
    )
}
/// `IF(cond, then, else)`
pub fn if_(c: impl IntoExpr, then: impl IntoExpr, otherwise: impl IntoExpr) -> Expr {
    call(
        "IF",
        vec![c.into_expr(), then.into_expr(), otherwise.into_expr()],
    )
}
/// `SUBSTR(e, start)` / `SUBSTR(e, start, len)`
pub fn substr(e: impl IntoExpr, start: impl IntoExpr, len: Option<i64>) -> Expr {
    let mut args = vec![e.into_expr(), start.into_expr()];
    args.extend(len.map(IntoExpr::into_expr));
    call("SUBSTR", args)
}
/// `REPLACE(e, "pattern", "replacement")`; pattern and replacement are plain strings.
pub fn replace(e: impl IntoExpr, pattern: &str, replacement: &str) -> Expr {
    call(
        "REPLACE",
        vec![
            e.into_expr(),
            lit(pattern).into_expr(),
            lit(replacement).into_expr(),
        ],
    )
}

// ----------------------------------------------------------------- aggregates ----

fn agg(name: &'static str, e: impl IntoExpr) -> Expr {
    Expr(E::Aggregate {
        name,
        distinct: false,
        arg: Some(Box::new(e.into_expr())),
        separator: None,
    })
}
/// `COUNT(*)`
pub fn count_star() -> Expr {
    Expr(E::Aggregate {
        name: "COUNT",
        distinct: false,
        arg: None,
        separator: None,
    })
}
/// `COUNT(e)`; `count(e).distinct()` for `COUNT(DISTINCT e)`.
pub fn count(e: impl IntoExpr) -> Expr {
    agg("COUNT", e)
}
/// `COUNT(DISTINCT e)`
pub fn count_distinct(e: impl IntoExpr) -> Expr {
    agg("COUNT", e).distinct()
}
/// `SUM(e)`
pub fn sum(e: impl IntoExpr) -> Expr {
    agg("SUM", e)
}
/// `AVG(e)`
pub fn avg(e: impl IntoExpr) -> Expr {
    agg("AVG", e)
}
/// `MIN(e)`
pub fn min(e: impl IntoExpr) -> Expr {
    agg("MIN", e)
}
/// `MAX(e)`
pub fn max(e: impl IntoExpr) -> Expr {
    agg("MAX", e)
}
/// `SAMPLE(e)`
pub fn sample(e: impl IntoExpr) -> Expr {
    agg("SAMPLE", e)
}
/// `GROUP_CONCAT(e)`; `separator` adds `; SEPARATOR = "…"` (escaped).
pub fn group_concat(e: impl IntoExpr, separator: Option<&str>) -> Expr {
    Expr(E::Aggregate {
        name: "GROUP_CONCAT",
        distinct: false,
        arg: Some(Box::new(e.into_expr())),
        separator: separator.map(str::to_string),
    })
}

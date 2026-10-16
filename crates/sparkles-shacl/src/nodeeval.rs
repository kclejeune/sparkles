//! Evaluation of SHACL 1.2 node expressions (see [`crate::nodeexpr`]): the function
//! `evalExpr(expr, focusGraph, focusNode, scope) -> outputNodes` of the draft, over the
//! data graph of a validation as the focus graph.
//!
//! Nodes are store ids (terms the data graph does not have get local ids), so path
//! values, instances and shape conformance work on ids and a term is decoded only where
//! a value is needed: ordering, aggregates and SPARQL functions.
//!
//! Where the draft leaves a choice, an argument of a custom function is evaluated with
//! the focus node and scope of the call, not an empty scope, so `shnex:var` in an
//! argument sees the variables of the expression that passes it.

use crate::nodeexpr::{ArgKey, Expr, ExprId, FnKind, SparqlText};
use crate::validate::{Cx, Engine, ValidateOptions};
use crate::{ExprFailure, Shapes};
use anyhow::{Result, bail};
use oxrdf::vocab::xsd;
use oxrdf::{Graph, Literal, Term};
use rustc_hash::FxHashSet;
use sparkles_core::id::Id;
use sparkles_core::sparql::value::{self, NumOp, Value};
use sparkles_core::store::Snapshot;
use std::sync::Arc;

/// An argument of a custom function call: the expression, and the focus node and scope it
/// is evaluated in.
#[derive(Clone, Debug)]
pub(crate) struct ArgBinding {
    pub key: ArgKey,
    pub expr: ExprId,
    pub focus: Id,
    pub scope: Arc<Scope>,
}

/// The variables in scope: `shnex:var` reads `vars`, `shnex:arg` reads `args`.
#[derive(Clone, Debug, Default)]
pub(crate) struct Scope {
    pub vars: Vec<(String, Id)>,
    pub args: Vec<ArgBinding>,
}

impl Scope {
    pub fn with_var(name: &str, v: Id) -> Scope {
        Scope {
            vars: vec![(name.to_string(), v)],
            args: Vec::new(),
        }
    }
}

/// The most nested evaluations (a custom function that calls itself ends here).
const MAX_EVAL_DEPTH: usize = 192;

/// Units of work between two looks at the timeout and cancellation.
const EXPR_CHECK_EVERY: u64 = 4096;

/// A failure of a node expression evaluation.
fn fail<T>(msg: impl Into<String>) -> Result<T> {
    Err(ExprFailure(msg.into()).into())
}

impl Engine<'_> {
    fn bool_id(&self, b: bool) -> Id {
        self.lit_id(&Term::Literal(Literal::from(b)))
    }

    pub(crate) fn lit_id(&self, t: &Term) -> Id {
        self.data.resolve_shared(t, self.shapes.bnodes_in_store)
    }

    fn is_true(&self, id: Id) -> bool {
        matches!(self.data.term(id), Some(Term::Literal(l)) if l.datatype() == xsd::BOOLEAN && matches!(l.value(), "true" | "1"))
    }

    /// Is `ids` exactly the list `( true )`?
    pub(crate) fn only_true(&self, ids: &[Id]) -> bool {
        matches!(ids, [x] if self.is_true(*x))
    }

    /// The single node of an evaluation that may give at most one (the arguments of list
    /// parameter functions).
    fn single_of(&self, ids: Vec<Id>, what: &str) -> Result<Option<Id>> {
        match ids.len() {
            0 => Ok(None),
            1 => Ok(Some(ids[0])),
            n => fail(format!(
                "{what} produced {n} nodes where at most one is allowed"
            )),
        }
    }

    /// `evalExpr`: the output nodes of expression `e` at `focus`.
    pub(crate) fn eval(&self, e: ExprId, focus: Id, scope: &Scope, cx: &mut Cx) -> Result<Vec<Id>> {
        if cx.depth >= MAX_EVAL_DEPTH {
            return fail("node expressions nested too deeply (a function calls itself?)");
        }
        if cx.depth == 0 {
            cx.expr_work = 0;
        }
        cx.depth += 1;
        // each level is a big frame; grow the stack for a deep expression
        let r = stacker::maybe_grow(128 << 10, 4 << 20, || self.eval_inner(e, focus, scope, cx));
        cx.depth -= 1;
        let out = r?;
        self.charge(cx, out.len())?;
        Ok(out)
    }

    /// Count one evaluation that produced `n` nodes against the budget of the top-level
    /// evaluation, and look at the timeout and cancellation every few thousand units.
    fn charge(&self, cx: &mut Cx, n: usize) -> Result<()> {
        let before = cx.expr_work;
        cx.expr_work = before.saturating_add(1 + n as u64);
        if cx.expr_work > self.max_expr_nodes {
            return fail(format!(
                "a node expression worked through more than {} nodes",
                self.max_expr_nodes
            ));
        }
        if before / EXPR_CHECK_EVERY != cx.expr_work / EXPR_CHECK_EVERY {
            self.check_limits()?;
        }
        Ok(())
    }

    /// Append `next` to `out`, unless the result would pass the budget.
    fn append(&self, out: &mut Vec<Id>, next: Vec<Id>) -> Result<()> {
        if (out.len() as u64).saturating_add(next.len() as u64) > self.max_expr_nodes {
            return fail(format!(
                "a node expression produced more than {} nodes",
                self.max_expr_nodes
            ));
        }
        out.extend(next);
        Ok(())
    }

    /// Look at the timeout and cancellation every few hundred nodes of a loop.
    fn tick(&self, i: usize) -> Result<()> {
        if i.is_multiple_of(256) {
            self.check_limits()?;
        }
        Ok(())
    }

    fn nodes_or_focus(
        &self,
        nodes: Option<ExprId>,
        focus: Id,
        scope: &Scope,
        cx: &mut Cx,
    ) -> Result<Vec<Id>> {
        match nodes {
            Some(n) => self.eval(n, focus, scope, cx),
            None => Ok(vec![focus]),
        }
    }

    fn eval_inner(&self, e: ExprId, focus: Id, scope: &Scope, cx: &mut Cx) -> Result<Vec<Id>> {
        match &self.shapes.exprs[e] {
            Expr::Const(t) => Ok(vec![self.id(*t)]),
            Expr::Empty => Ok(Vec::new()),
            Expr::List(ts) => Ok(ts.iter().map(|t| self.id(*t)).collect()),
            Expr::Var(name) => {
                if name == "focusNode" {
                    return Ok(vec![focus]);
                }
                Ok(scope
                    .vars
                    .iter()
                    .find(|(n, _)| n == name)
                    .map(|(_, v)| vec![*v])
                    .unwrap_or_default())
            }
            Expr::PathValues { focus: f, .. } => {
                let node = match f {
                    None => focus,
                    Some(f) => {
                        let n = self.eval(*f, focus, scope, cx)?;
                        match self.single_of(n, "shnex:focusNode")? {
                            Some(n) => n,
                            None => return Ok(Vec::new()),
                        }
                    }
                };
                match &self.expr_paths[e] {
                    Some(p) => p.eval(self.data, node),
                    None => Ok(Vec::new()),
                }
            }
            Expr::Exists(x) => {
                let n = self.eval(*x, focus, scope, cx)?;
                Ok(vec![self.bool_id(!n.is_empty())])
            }
            Expr::If { cond, then, els } => {
                let c = self.eval(*cond, focus, scope, cx)?;
                let branch = if self.only_true(&c) { then } else { els };
                match branch {
                    Some(b) => self.eval(*b, focus, scope, cx),
                    None => Ok(Vec::new()),
                }
            }
            Expr::Distinct(x) => {
                let n = self.eval(*x, focus, scope, cx)?;
                Ok(dedup(n))
            }
            Expr::Intersection(xs) => {
                let mut it = xs.iter();
                let Some(first) = it.next() else {
                    return Ok(Vec::new());
                };
                let mut acc = dedup(self.eval(*first, focus, scope, cx)?);
                for x in it {
                    let other: FxHashSet<Id> =
                        self.eval(*x, focus, scope, cx)?.into_iter().collect();
                    acc.retain(|n| other.contains(n));
                }
                Ok(acc)
            }
            Expr::Concat(xs) => {
                let mut out = Vec::new();
                for x in xs {
                    let next = self.eval(*x, focus, scope, cx)?;
                    self.append(&mut out, next)?;
                }
                Ok(out)
            }
            Expr::Remove { nodes, remove } => {
                let m: FxHashSet<Id> = self.eval(*remove, focus, scope, cx)?.into_iter().collect();
                let n = self.eval(*nodes, focus, scope, cx)?;
                Ok(n.into_iter().filter(|x| !m.contains(x)).collect())
            }
            Expr::FilterShape { nodes, shape } => {
                let n = self.eval(*nodes, focus, scope, cx)?;
                let mut out = Vec::new();
                for (i, x) in n.into_iter().enumerate() {
                    self.tick(i)?;
                    if self.conforms(*shape, x, cx)? {
                        out.push(x);
                    }
                }
                Ok(out)
            }
            Expr::Limit { nodes, n } => {
                let mut v = self.eval(*nodes, focus, scope, cx)?;
                v.truncate(usize::try_from(*n).unwrap_or(usize::MAX));
                Ok(v)
            }
            Expr::Offset { nodes, n } => {
                let v = self.eval(*nodes, focus, scope, cx)?;
                let skip = usize::try_from(*n).unwrap_or(usize::MAX);
                Ok(v.into_iter().skip(skip).collect())
            }
            Expr::OrderBy { nodes, by, desc } => {
                let n = self.eval(*nodes, focus, scope, cx)?;
                let mut keyed: Vec<(Id, Option<Value>)> = Vec::with_capacity(n.len());
                for (i, x) in n.into_iter().enumerate() {
                    self.tick(i)?;
                    let k = self.eval(*by, x, scope, cx)?;
                    let key = k.first().and_then(|k| self.data.value(*k));
                    keyed.push((x, key));
                }
                keyed.sort_by(|a, b| value::order_cmp(a.1.as_ref(), b.1.as_ref()));
                let mut out: Vec<Id> = keyed.into_iter().map(|(x, _)| x).collect();
                if *desc {
                    out.reverse();
                }
                Ok(out)
            }
            Expr::FlatMap { nodes, map } => {
                let n = self.nodes_or_focus(*nodes, focus, scope, cx)?;
                let mut out = Vec::new();
                for (i, x) in n.into_iter().enumerate() {
                    self.tick(i)?;
                    let next = self.eval(*map, x, scope, cx)?;
                    self.append(&mut out, next)?;
                }
                Ok(out)
            }
            Expr::FindFirst { nodes, shape } => {
                let n = self.nodes_or_focus(*nodes, focus, scope, cx)?;
                for (i, x) in n.into_iter().enumerate() {
                    self.tick(i)?;
                    if self.conforms(*shape, x, cx)? {
                        return Ok(vec![x]);
                    }
                }
                Ok(Vec::new())
            }
            Expr::MatchAll { nodes, shape } => {
                let n = self.nodes_or_focus(*nodes, focus, scope, cx)?;
                for (i, x) in n.into_iter().enumerate() {
                    self.tick(i)?;
                    if !self.conforms(*shape, x, cx)? {
                        return Ok(vec![self.bool_id(false)]);
                    }
                }
                Ok(vec![self.bool_id(true)])
            }
            Expr::Count(x) => {
                let n = self.eval(*x, focus, scope, cx)?;
                let t = Term::Literal(Literal::new_typed_literal(
                    n.len().to_string(),
                    xsd::INTEGER,
                ));
                Ok(vec![self.lit_id(&t)])
            }
            Expr::Min(x) | Expr::Max(x) => {
                let max = matches!(self.shapes.exprs[e], Expr::Max(_));
                let n = self.eval(*x, focus, scope, cx)?;
                let mut best: Option<(Id, Value)> = None;
                for id in n {
                    let Some(v) = self.data.value(id) else {
                        continue;
                    };
                    best = match best {
                        None => Some((id, v)),
                        Some((bid, bv)) => {
                            let ord = value::order_cmp(Some(&v), Some(&bv));
                            if (max && ord.is_gt()) || (!max && ord.is_lt()) {
                                Some((id, v))
                            } else {
                                Some((bid, bv))
                            }
                        }
                    };
                }
                Ok(best.map(|(id, _)| vec![id]).unwrap_or_default())
            }
            Expr::Sum(x) => {
                let n = self.eval(*x, focus, scope, cx)?;
                let mut acc = Value::from_term(&Term::Literal(Literal::new_typed_literal(
                    "0",
                    xsd::INTEGER,
                )));
                for id in n {
                    let Some(v) = self.data.value(id) else {
                        return fail("shnex:sum of a node that has no value");
                    };
                    acc = value::arith(NumOp::Add, &acc, &v).map_err(|_| {
                        ExprFailure(format!("shnex:sum cannot add {}", self.show(id)))
                    })?;
                }
                Ok(vec![self.lit_id(&acc.to_term())])
            }
            Expr::InstancesOf(x) => {
                let types = self.eval(*x, focus, scope, cx)?;
                let mut out = Vec::new();
                let mut seen = FxHashSet::default();
                for t in types {
                    if !matches!(self.data.term(t), Some(Term::NamedNode(_))) {
                        return fail(format!(
                            "shnex:instancesOf expects classes that are IRIs, found {}",
                            self.show(t)
                        ));
                    }
                    for i in self.data.instances(t)? {
                        if seen.insert(i) {
                            out.push(i);
                        }
                    }
                }
                Ok(out)
            }
            Expr::NodesMatching(shape) => {
                let mut out = Vec::new();
                for (i, n) in self.candidates(*shape)?.into_iter().enumerate() {
                    if i.is_multiple_of(256) {
                        self.check_limits()?;
                    }
                    if self.conforms(*shape, n, cx)? {
                        out.push(n);
                    }
                }
                Ok(out)
            }
            Expr::ConformsToShape { node, shape } => {
                let n = self.eval(*node, focus, scope, cx)?;
                let n = self.single_of(n, "the node of shnex:conformsToShape")?;
                let s = self.eval(*shape, focus, scope, cx)?;
                let s = self.single_of(s, "the shape of shnex:conformsToShape")?;
                let (Some(n), Some(s)) = (n, s) else {
                    return Ok(Vec::new());
                };
                let Some(term) = self.data.term(s) else {
                    return fail("shnex:conformsToShape: the shape is not a node");
                };
                if !matches!(term, Term::NamedNode(_) | Term::BlankNode(_)) {
                    return fail(format!("shnex:conformsToShape: {term} is not a shape"));
                }
                // a node that no shape declaration describes has no constraints
                let ok = match self.shapes.by_node.get(&term) {
                    Some(&si) => self.conforms(si, n, cx)?,
                    None => true,
                };
                Ok(vec![self.bool_id(ok)])
            }
            Expr::SparqlFn { name, args, query } => {
                let mut vals: Vec<Option<Id>> = Vec::with_capacity(args.len());
                for a in args {
                    let r = self.eval(*a, focus, scope, cx)?;
                    vals.push(self.single_of(r, &format!("an argument of sparql:{name}"))?);
                }
                if name == "bound" {
                    return Ok(vec![
                        self.bool_id(vals.first().is_some_and(Option::is_some)),
                    ]);
                }
                let mut names = Vec::new();
                let mut terms = Vec::new();
                for (i, v) in vals.iter().enumerate() {
                    if let Some(v) = v {
                        names.push(format!("a{i}"));
                        terms.push(self.term(*v)?);
                    }
                }
                let binds: Vec<(&str, Term)> =
                    names.iter().map(String::as_str).zip(terms).collect();
                let (table, ctx) = self.run_query(query, &binds)?;
                self.table_column(&table, &ctx)
            }
            Expr::Select(q) | Expr::SparqlExpr(q) => self.sparql_expression(q, focus, scope, cx),
            Expr::Custom { func, args } => {
                let f = &self.shapes.functions[*func];
                let caller = Arc::new(scope.clone());
                let bound = args
                    .iter()
                    .map(|a| ArgBinding {
                        key: a.key.clone(),
                        expr: a.expr,
                        focus,
                        scope: caller.clone(),
                    })
                    .collect();
                let inner = Scope {
                    vars: Vec::new(),
                    args: bound,
                };
                let out = self.eval(f.body, focus, &inner, cx)?;
                if matches!(f.kind, FnKind::List) && out.len() > 1 {
                    return fail(format!(
                        "the list parameter function {} produced {} nodes",
                        f.iri,
                        out.len()
                    ));
                }
                Ok(out)
            }
            Expr::Arg(key) => match scope.args.iter().find(|a| &a.key == key) {
                Some(a) => {
                    let (expr, f, sc) = (a.expr, a.focus, a.scope.clone());
                    self.eval(expr, f, &sc, cx)
                }
                None => Ok(Vec::new()),
            },
        }
    }

    /// The nodes of the first column of a query result, unbound values left out.
    fn table_column(
        &self,
        table: &sparkles_core::sparql::table::Table,
        ctx: &sparkles_core::sparql::Ctx,
    ) -> Result<Vec<Id>> {
        let mut out = Vec::new();
        if table.width() == 0 {
            return Ok(out);
        }
        for row in 0..table.len() {
            let id = table.get(row, 0);
            if let Some(t) = Self::result_term(ctx, id) {
                out.push(self.lit_id(&t));
            }
        }
        Ok(out)
    }

    /// `sh:select` and `sh:sparqlExpr`: `$this` is the focus node, and the scope
    /// variables are pre-bound under their own names (`arg` plus the position for the
    /// arguments of a list parameter function).
    fn sparql_expression(
        &self,
        q: &SparqlText,
        focus: Id,
        scope: &Scope,
        cx: &mut Cx,
    ) -> Result<Vec<Id>> {
        let mut names: Vec<String> = vec!["this".into()];
        let mut terms: Vec<Term> = vec![self.term(focus)?];
        for (n, v) in &scope.vars {
            if n == "this" {
                return fail("a scope variable is called this");
            }
            names.push(n.clone());
            terms.push(self.term(*v)?);
        }
        for a in &scope.args {
            let name = match &a.key {
                ArgKey::Index(i) => format!("arg{i}"),
                ArgKey::Param(p) => {
                    let s = p.as_str();
                    s[s.rfind(['#', '/']).map_or(0, |i| i + 1)..].to_string()
                }
            };
            let sc = a.scope.clone();
            let r = self.eval(a.expr, a.focus, &sc, cx)?;
            if let Some(v) = self.single_of(r, "an argument")? {
                names.push(name);
                terms.push(self.term(v)?);
            }
        }
        let binds: Vec<(&str, Term)> = names.iter().map(String::as_str).zip(terms).collect();
        let (table, ctx) = self.run_query(&q.query, &binds)?;
        self.table_column(&table, &ctx)
    }

    /// The node expression of a shape's `sh:deactivated` at a focus node.
    pub(crate) fn deactivated_at(&self, si: usize, focus: Id, cx: &mut Cx) -> Result<bool> {
        let shape = &self.shapes.shapes[si];
        if shape.deactivated {
            return Ok(true);
        }
        match shape.deactivated_expr {
            Some(e) => {
                let r = self.eval(e, focus, &Scope::default(), cx)?;
                Ok(self.only_true(&r))
            }
            None => Ok(false),
        }
    }

    /// The value nodes of a property shape with `sh:values` and `sh:defaultValue`.
    pub(crate) fn extend_values(
        &self,
        si: usize,
        focus: Id,
        mut values: Vec<Id>,
        cx: &mut Cx,
    ) -> Result<Vec<Id>> {
        let shape = &self.shapes.shapes[si];
        if let Some(e) = shape.values {
            let extra = self.eval(e, focus, &Scope::default(), cx)?;
            let mut seen: FxHashSet<Id> = values.iter().copied().collect();
            for x in extra {
                if seen.insert(x) {
                    values.push(x);
                }
            }
        }
        if values.is_empty()
            && let Some(e) = shape.default_value
        {
            values = dedup(self.eval(e, focus, &Scope::default(), cx)?);
        }
        Ok(values)
    }
}

fn dedup(v: Vec<Id>) -> Vec<Id> {
    let mut seen = FxHashSet::default();
    v.into_iter().filter(|x| seen.insert(*x)).collect()
}

/// A query computing the SPARQL function `name` of the variables `a0`, `a1`, …
pub(crate) fn sparql_function_text(name: &str, arity: usize) -> Result<String> {
    let a = |i: usize| format!("?a{i}");
    let args = (0..arity).map(a).collect::<Vec<_>>();
    let binary = |op: &str| -> Result<String> {
        if arity != 2 {
            bail!("sparql:{name} takes two arguments, found {arity}");
        }
        Ok(format!("({} {op} {})", a(0), a(1)))
    };
    let unary = |op: &str| -> Result<String> {
        if arity != 1 {
            bail!("sparql:{name} takes one argument, found {arity}");
        }
        Ok(format!("({op}{})", a(0)))
    };
    let expr = match name {
        "plus" => binary("+")?,
        "subtract" => binary("-")?,
        "multiply" => binary("*")?,
        "divide" => binary("/")?,
        "equals" => binary("=")?,
        "not-equals" => binary("!=")?,
        "less-than" => binary("<")?,
        "greater-than" => binary(">")?,
        "less-than-or-equal" => binary("<=")?,
        "greater-than-or-equal" => binary(">=")?,
        "logical-and" => binary("&&")?,
        "logical-or" => binary("||")?,
        "unary-plus" => unary("+")?,
        "unary-minus" => unary("-")?,
        "logical-not" => unary("!")?,
        // the builtin calls: sparql:NAME is the SPARQL function NAME
        "str" | "lang" | "langdir" | "datatype" | "bound" | "iri" | "uri" | "bnode" | "rand"
        | "abs" | "ceil" | "floor" | "round" | "concat" | "strlen" | "ucase" | "lcase"
        | "contains" | "strstarts" | "strends" | "strbefore" | "strafter" | "year" | "month"
        | "day" | "hours" | "minutes" | "seconds" | "timezone" | "tz" | "now" | "uuid"
        | "struuid" | "md5" | "sha1" | "sha256" | "sha384" | "sha512" | "coalesce" | "if"
        | "strlang" | "strdt" | "strlangdir" | "sameTerm" | "isIRI" | "isURI" | "isBlank"
        | "isLiteral" | "isNumeric" | "hasLangdir" | "langMatches" | "regex" | "substr"
        | "replace" | "isTriple" | "triple" | "subject" | "predicate" | "object" => {
            format!("{}({})", name.to_ascii_uppercase(), args.join(", "))
        }
        "encode" => format!("ENCODE_FOR_URI({})", args.join(", ")),
        // SPARQL 1.2 has no sameValue function, and its hasLANG takes one term; the test
        // suite calls them with two arguments and one that is the language range
        "sameValue" if arity == 2 => format!("COALESCE(({} = {}), false)", a(0), a(1)),
        "hasLang" if arity == 2 => {
            format!("COALESCE(LANGMATCHES(LANG({}), {}), false)", a(0), a(1))
        }
        "hasLang" => format!("HASLANG({})", args.join(", ")),
        _ => bail!("unknown SPARQL function sparql:{name}"),
    };
    Ok(format!("SELECT ({expr} AS ?r) WHERE {{}}"))
}

/// Evaluate the node expression `expr` of a shapes graph with the focus node `focus`
/// (none: no focus node) over the default graph of a snapshot, with `scope` as the
/// variables `shnex:var` reads. The result is the list of output nodes.
pub fn eval_expression(
    snap: &Arc<Snapshot>,
    shapes: &Shapes,
    expr: ExprId,
    focus: Option<&Term>,
    scope: &[(String, Term)],
    opts: &ValidateOptions,
) -> Result<Vec<Term>> {
    let (mut data, ids) = crate::data::DataGraph::new(
        snap.clone(),
        opts.data_graph.as_deref(),
        &opts.extra_graphs,
        &opts.exclude_graphs,
        shapes,
    )?;
    let focus_id = match focus {
        Some(t) => data.resolve(t, shapes.bnodes_in_store),
        None => Id::UNDEF,
    };
    let vars: Vec<(String, Id)> = scope
        .iter()
        .map(|(n, t)| (n.clone(), data.resolve(t, shapes.bnodes_in_store)))
        .collect();
    let engine = Engine::new_for_expressions(shapes, &data, ids, opts)?;
    let sc = Scope {
        vars,
        args: Vec::new(),
    };
    let out = engine.eval(expr, focus_id, &sc, &mut Cx::default())?;
    out.into_iter()
        .map(|id| engine.term(id))
        .collect::<Result<Vec<_>>>()
}

/// Parse a node expression out of a graph and evaluate it (see [`eval_expression`]).
pub fn eval_expression_in_graph(
    snap: &Arc<Snapshot>,
    shapes_graph: Graph,
    bnodes_in_store: bool,
    expr: &Term,
    focus: Option<&Term>,
    scope: &[(String, Term)],
    opts: &ValidateOptions,
) -> Result<Vec<Term>> {
    let (shapes, id) = Shapes::from_graph_with_expression(shapes_graph, bnodes_in_store, expr)?;
    eval_expression(snap, &shapes, id, focus, scope, opts)
}

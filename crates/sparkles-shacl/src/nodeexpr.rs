//! SHACL 1.2 node expressions: the expression tree and its parser.
//!
//! A node expression computes a list of nodes from a focus node (SHACL 1.2 Core,
//! "Node Expressions", and the Working Draft of SHACL 1.2 Node Expressions, whose
//! `shnex:` vocabulary this module follows). IRIs, literals and triple terms are
//! constants. A blank node is an expression that is a *named parameter function*
//! (identified by its key parameter, such as `shnex:pathValues`), a *list parameter
//! function* (one triple whose predicate is the function, such as `sparql:plus`), a list
//! expression (`rdf:first` and `rdf:rest`), or the empty expression (no triples).
//! Custom functions declared in the shapes graph with `sh:bodyExpression` are called the
//! same way. The evaluator is in [`crate::nodeeval`].
//!
//! The draft this follows is the `w3c/data-shapes` repository's `shacl12-node-expr` as of
//! 2026-10-13, with its test suite under `shacl12-test-suite/tests/node-expr`.

use crate::path::PropertyPath;
use crate::shapes::{Parser, ShapeId, Tid, as_subject};
use crate::vocab::{SHNEX_NS, SPARQL_FN_NS, rdf, sh, shnex};
use anyhow::{Context as _, Result, anyhow, bail};
use oxrdf::vocab::xsd;
use oxrdf::{NamedNode, NamedNodeRef, Term};
use rustc_hash::FxHashMap;
use spargebra::Query;

/// Index of an expression in [`crate::Shapes`].
pub type ExprId = usize;

/// The key of an argument in a custom function call (`shnex:arg`).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ArgKey {
    /// the IRI of a parameter of a custom named parameter function
    Param(NamedNode),
    /// the position of an argument of a custom list parameter function, from 0
    Index(u64),
}

/// A node expression.
#[derive(Clone, Debug)]
pub enum Expr {
    /// an IRI, a literal or a triple term: evaluates to itself
    Const(Tid),
    /// a blank node without triples: no nodes
    Empty,
    /// `shnex:var`
    Var(String),
    /// a list expression: the members of an `rdf:first`/`rdf:rest` list of IRIs and
    /// literals
    List(Vec<Tid>),
    /// `shnex:pathValues`, with the optional `shnex:focusNode`
    PathValues {
        path: PropertyPath,
        focus: Option<ExprId>,
    },
    /// `shnex:exists`
    Exists(ExprId),
    /// `shnex:if` with `shnex:then` and `shnex:else`
    If {
        cond: ExprId,
        then: Option<ExprId>,
        els: Option<ExprId>,
    },
    /// `shnex:distinct`
    Distinct(ExprId),
    /// `shnex:intersection`
    Intersection(Vec<ExprId>),
    /// `shnex:concat`
    Concat(Vec<ExprId>),
    /// `shnex:remove` with `shnex:nodes`
    Remove { nodes: ExprId, remove: ExprId },
    /// `shnex:filterShape` with `shnex:nodes`
    FilterShape { nodes: ExprId, shape: ShapeId },
    /// `shnex:limit` with `shnex:nodes`
    Limit { nodes: ExprId, n: u64 },
    /// `shnex:offset` with `shnex:nodes`
    Offset { nodes: ExprId, n: u64 },
    /// `shnex:orderBy` with `shnex:nodes` and `shnex:desc`
    OrderBy {
        nodes: ExprId,
        by: ExprId,
        desc: bool,
    },
    /// `shnex:flatMap`; `nodes` defaults to the focus node
    FlatMap { nodes: Option<ExprId>, map: ExprId },
    /// `shnex:findFirst`
    FindFirst {
        nodes: Option<ExprId>,
        shape: ShapeId,
    },
    /// `shnex:matchAll`
    MatchAll {
        nodes: Option<ExprId>,
        shape: ShapeId,
    },
    /// `shnex:count`
    Count(ExprId),
    /// `shnex:min`
    Min(ExprId),
    /// `shnex:max`
    Max(ExprId),
    /// `shnex:sum`
    Sum(ExprId),
    /// `shnex:instancesOf`
    InstancesOf(ExprId),
    /// `shnex:nodesMatching`
    NodesMatching(ShapeId),
    /// `shnex:conformsToShape ( node shape )`
    ConformsToShape { node: ExprId, shape: ExprId },
    /// `sparql:NAME ( args )`: a SPARQL function applied to the single node each argument
    /// evaluates to
    SparqlFn {
        name: String,
        args: Vec<ExprId>,
        /// the function as a query over the variables `a0`, `a1`, …
        query: Box<Query>,
    },
    /// `sh:select` (SHACL-SPARQL): the bindings of the one projected variable
    Select(Box<SparqlText>),
    /// `sh:sparqlExpr` (SHACL-SPARQL): the value of a SPARQL expression
    SparqlExpr(Box<SparqlText>),
    /// a call of a custom function
    Custom { func: usize, args: Vec<CallArg> },
    /// `shnex:arg`
    Arg(ArgKey),
}

/// A SPARQL query or expression of a node expression.
#[derive(Clone, Debug)]
pub struct SparqlText {
    pub text: String,
    pub query: Query,
}

/// An argument of a custom function call.
#[derive(Clone, Debug)]
pub struct CallArg {
    pub key: ArgKey,
    pub expr: ExprId,
}

/// A custom node expression function (`sh:NamedParameterExpressionFunction` or
/// `sh:ListParameterExpressionFunction`).
#[derive(Clone, Debug)]
pub struct CustomFn {
    pub iri: NamedNode,
    pub kind: FnKind,
    /// the body, evaluated with the call's arguments in scope
    pub body: ExprId,
}

/// How a custom function is called.
#[derive(Clone, Debug)]
pub enum FnKind {
    /// a blank node with a value for at least one key parameter
    Named { params: Vec<FnParam> },
    /// a blank node with one triple whose predicate is the function's IRI
    List,
}

/// A parameter of a custom named parameter function.
#[derive(Clone, Debug)]
pub struct FnParam {
    pub path: NamedNode,
    pub key: bool,
    pub optional: bool,
}

/// A function declaration while the bodies are not parsed yet.
struct Declared {
    iri: NamedNode,
    body: Term,
    kind: FnKind,
}

/// What the parser knows about custom functions.
#[derive(Default)]
pub(crate) struct FnIndex {
    /// key parameter IRI → function index
    keys: FxHashMap<NamedNode, usize>,
    /// function IRI → function index, for list parameter functions
    lists: FxHashMap<NamedNode, usize>,
}

/// The most nested expressions a shapes graph may have.
const MAX_DEPTH: usize = 128;

/// The key parameters of the built-in named parameter functions.
const KEYS: &[NamedNodeRef<'static>] = &[
    shnex::VAR,
    shnex::PATH_VALUES,
    shnex::EXISTS,
    shnex::IF,
    shnex::DISTINCT,
    shnex::INTERSECTION,
    shnex::CONCAT,
    shnex::REMOVE,
    shnex::FILTER_SHAPE,
    shnex::LIMIT,
    shnex::OFFSET,
    shnex::ORDER_BY,
    shnex::FLAT_MAP,
    shnex::FIND_FIRST,
    shnex::MATCH_ALL,
    shnex::COUNT,
    shnex::MIN,
    shnex::MAX,
    shnex::SUM,
    shnex::INSTANCES_OF,
    shnex::NODES_MATCHING,
    shnex::ARG,
    sh::SELECT,
    sh::SPARQL_EXPR,
];

fn integer(t: &Term, what: &str) -> Result<u64> {
    if let Term::Literal(l) = t
        && l.datatype() == xsd::INTEGER
        && let Ok(v) = l.value().trim().parse::<i64>()
        && v >= 0
    {
        return Ok(v as u64);
    }
    bail!("{what} expects a non-negative xsd:integer, found {t}")
}

fn boolean(t: &Term, what: &str) -> Result<bool> {
    match t {
        Term::Literal(l) if l.datatype() == xsd::BOOLEAN => match l.value() {
            "true" | "1" => Ok(true),
            "false" | "0" => Ok(false),
            v => bail!("{what}: invalid boolean {v:?}"),
        },
        _ => bail!("{what} expects an xsd:boolean, found {t}"),
    }
}

impl Parser<'_> {
    /// Declare the custom node expression functions of the shapes graph and parse
    /// their bodies.
    pub(crate) fn parse_functions(&mut self) -> Result<()> {
        let mut declared: Vec<Declared> = Vec::new();
        for (class, list) in [
            (sh::NAMED_PARAMETER_EXPRESSION_FUNCTION, false),
            (sh::LIST_PARAMETER_EXPRESSION_FUNCTION, true),
        ] {
            let class_term: Term = class.into_owned().into();
            for s in self.g.subjects(rdf::TYPE, &class_term) {
                let Term::NamedNode(iri) = s else { continue };
                let Some(body) = self
                    .g
                    .object(&Term::NamedNode(iri.clone()), sh::BODY_EXPRESSION)
                else {
                    bail!("the function {iri} has no sh:bodyExpression");
                };
                let kind = if list {
                    FnKind::List
                } else {
                    let mut params = Vec::new();
                    for p in self.g.objects(&Term::NamedNode(iri.clone()), sh::PARAMETER) {
                        let Some(Term::NamedNode(path)) = self.g.object(&p, sh::PATH) else {
                            bail!("a parameter of {iri} has no IRI as its sh:path");
                        };
                        let key = self
                            .g
                            .object(&p, sh::KEY_PARAMETER)
                            .is_some_and(|t| boolean(&t, "sh:keyParameter").unwrap_or(false));
                        let optional = self
                            .g
                            .object(&p, sh::OPTIONAL)
                            .is_some_and(|t| boolean(&t, "sh:optional").unwrap_or(false));
                        params.push(FnParam {
                            path,
                            key,
                            optional,
                        });
                    }
                    if !params.iter().any(|p| p.key) {
                        bail!("the function {iri} has no sh:keyParameter");
                    }
                    FnKind::Named { params }
                };
                declared.push(Declared { iri, body, kind });
            }
        }
        declared.sort_by(|a, b| a.iri.cmp(&b.iri));
        declared.dedup_by(|a, b| a.iri == b.iri);
        for (i, d) in declared.iter().enumerate() {
            match &d.kind {
                FnKind::List => {
                    self.fns.lists.insert(d.iri.clone(), i);
                }
                FnKind::Named { params } => {
                    for p in params.iter().filter(|p| p.key) {
                        if p.path.as_str().starts_with(SHNEX_NS)
                            && KEYS.iter().any(|k| k.as_str() == p.path.as_str())
                        {
                            bail!(
                                "the key parameter {} of {} is also a key parameter of a built-in function",
                                p.path,
                                d.iri
                            );
                        }
                        if self.fns.keys.insert(p.path.clone(), i).is_some() {
                            bail!("the key parameter {} belongs to two functions", p.path);
                        }
                    }
                }
            }
            self.out.functions.push(CustomFn {
                iri: d.iri.clone(),
                kind: d.kind.clone(),
                // set below
                body: usize::MAX,
            });
        }
        for (i, d) in declared.iter().enumerate() {
            let body = self
                .expr(&d.body)
                .with_context(|| format!("the body of {}", d.iri))?;
            self.out.functions[i].body = body;
        }
        Ok(())
    }

    fn push_expr(&mut self, node: &Term, e: Expr) -> ExprId {
        let id = self.out.exprs.len();
        self.out.exprs.push(e);
        self.out.expr_nodes.push(node.clone());
        id
    }

    /// Parse the node expression `node` (shared when it is a blank node used twice).
    pub(crate) fn expr(&mut self, node: &Term) -> Result<ExprId> {
        if let Term::BlankNode(_) = node {
            if let Some(&id) = self.expr_memo.get(node) {
                return Ok(id);
            }
            if self.expr_stack.contains(node) {
                bail!("the node expression {node} contains itself");
            }
            if self.expr_stack.len() >= MAX_DEPTH {
                bail!("node expressions nested more than {MAX_DEPTH} deep at {node}");
            }
            self.expr_stack.push(node.clone());
            let r = stacker::maybe_grow(128 << 10, 4 << 20, || self.blank_expr(node));
            self.expr_stack.pop();
            let id = r.with_context(|| format!("node expression {node}"))?;
            self.expr_memo.insert(node.clone(), id);
            return Ok(id);
        }
        let t = self.out.intern(node.clone());
        Ok(self.push_expr(node, Expr::Const(t)))
    }

    /// Is `node` a blank node that the node expression parser reads as a function?
    pub(crate) fn is_function_expr(&self, node: &Term) -> bool {
        matches!(node, Term::BlankNode(_)) && self.has_triples(node)
    }

    fn has_triples(&self, node: &Term) -> bool {
        as_subject(node).is_some_and(|s| self.g.g.triples_for_subject(s).next().is_some())
    }

    fn predicates(&self, node: &Term) -> Vec<NamedNode> {
        let Some(s) = as_subject(node) else {
            return Vec::new();
        };
        let mut out: Vec<NamedNode> = Vec::new();
        for t in self.g.g.triples_for_subject(s) {
            let p = t.predicate.into_owned();
            if !out.contains(&p) {
                out.push(p);
            }
        }
        out.sort();
        out
    }

    /// The only value of `p` at `node`.
    fn single(&self, node: &Term, p: NamedNodeRef<'_>) -> Result<Option<Term>> {
        let mut v = self.g.objects(node, p);
        match v.len() {
            0 => Ok(None),
            1 => Ok(v.pop()),
            _ => bail!("{} has more than one value", p.as_str()),
        }
    }

    fn required(&self, node: &Term, p: NamedNodeRef<'_>) -> Result<Term> {
        self.single(node, p)?
            .ok_or_else(|| anyhow!("{} is missing", p.as_str()))
    }

    fn expr_param(&mut self, node: &Term, p: NamedNodeRef<'_>) -> Result<ExprId> {
        let t = self.required(node, p)?;
        self.expr(&t)
    }

    fn opt_expr_param(&mut self, node: &Term, p: NamedNodeRef<'_>) -> Result<Option<ExprId>> {
        match self.single(node, p)? {
            Some(t) => Ok(Some(self.expr(&t)?)),
            None => Ok(None),
        }
    }

    fn expr_list(&mut self, head: &Term) -> Result<Vec<ExprId>> {
        let items = self.g.list(head)?;
        items.iter().map(|i| self.expr(i)).collect()
    }

    fn blank_expr(&mut self, node: &Term) -> Result<ExprId> {
        let preds = self.predicates(node);
        if preds.is_empty() {
            return Ok(self.push_expr(node, Expr::Empty));
        }
        if preds
            .iter()
            .any(|p| p.as_ref() == rdf::FIRST || p.as_ref() == rdf::REST)
        {
            let items = self.g.list(node)?;
            let mut ts = Vec::new();
            for i in items {
                if !matches!(i, Term::NamedNode(_) | Term::Literal(_)) {
                    bail!("a list expression holds only IRIs and literals, found {i}");
                }
                ts.push(self.out.intern(i));
            }
            return Ok(self.push_expr(node, Expr::List(ts)));
        }
        // the function: built-in key parameters, SPARQL expressions, custom keys, then the
        // predicate of a list parameter function
        let mut keys: Vec<NamedNode> = preds
            .iter()
            .filter(|p| KEYS.iter().any(|k| k.as_str() == p.as_str()))
            .cloned()
            .collect();
        keys.extend(
            preds
                .iter()
                .filter(|p| self.fns.keys.contains_key(*p))
                .cloned(),
        );
        if keys.len() > 1 {
            bail!(
                "more than one key parameter: {}",
                keys.iter()
                    .map(NamedNode::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        if let Some(k) = keys.pop() {
            return self.named_function(node, &k);
        }
        let listy: Vec<&NamedNode> = preds
            .iter()
            .filter(|p| {
                p.as_str() == shnex::CONFORMS_TO_SHAPE.as_str()
                    || p.as_str().starts_with(SPARQL_FN_NS)
                    || self.fns.lists.contains_key(*p)
            })
            .collect();
        match listy.as_slice() {
            [p] => {
                let p = (*p).clone();
                self.list_function(node, &p)
            }
            [] => bail!(
                "not a well-formed node expression: no key parameter or function among {}",
                preds
                    .iter()
                    .map(NamedNode::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            _ => bail!("more than one function predicate"),
        }
    }

    fn named_function(&mut self, node: &Term, key: &NamedNode) -> Result<ExprId> {
        if let Some(&func) = self.fns.keys.get(key) {
            return self.custom_call(node, func);
        }
        let is = |p: NamedNodeRef<'_>| key.as_str() == p.as_str();
        let e = if is(shnex::VAR) {
            let v = self.required(node, shnex::VAR)?;
            match &v {
                Term::Literal(l) if l.datatype() == xsd::STRING && !l.value().is_empty() => {
                    Expr::Var(l.value().to_string())
                }
                _ => bail!("shnex:var expects a non-empty xsd:string, found {v}"),
            }
        } else if is(shnex::PATH_VALUES) {
            let p = self.required(node, shnex::PATH_VALUES)?;
            let path = PropertyPath::from_graph(&self.g, &p).context("shnex:pathValues")?;
            let focus = self.opt_expr_param(node, shnex::FOCUS_NODE)?;
            Expr::PathValues { path, focus }
        } else if is(shnex::EXISTS) {
            Expr::Exists(self.expr_param(node, shnex::EXISTS)?)
        } else if is(shnex::IF) {
            let cond = self.expr_param(node, shnex::IF)?;
            let then = self.opt_expr_param(node, shnex::THEN)?;
            let els = self.opt_expr_param(node, shnex::ELSE)?;
            if then.is_none() && els.is_none() {
                bail!("shnex:if needs shnex:then or shnex:else");
            }
            Expr::If { cond, then, els }
        } else if is(shnex::DISTINCT) {
            Expr::Distinct(self.expr_param(node, shnex::DISTINCT)?)
        } else if is(shnex::INTERSECTION) {
            let l = self.required(node, shnex::INTERSECTION)?;
            Expr::Intersection(self.expr_list(&l)?)
        } else if is(shnex::CONCAT) {
            let l = self.required(node, shnex::CONCAT)?;
            Expr::Concat(self.expr_list(&l)?)
        } else if is(shnex::REMOVE) {
            let remove = self.expr_param(node, shnex::REMOVE)?;
            let nodes = self.expr_param(node, shnex::NODES)?;
            Expr::Remove { nodes, remove }
        } else if is(shnex::FILTER_SHAPE) {
            let s = self.required(node, shnex::FILTER_SHAPE)?;
            let shape = self.shape(&s)?;
            let nodes = self.expr_param(node, shnex::NODES)?;
            Expr::FilterShape { nodes, shape }
        } else if is(shnex::LIMIT) {
            let n = integer(&self.required(node, shnex::LIMIT)?, "shnex:limit")?;
            let nodes = self.expr_param(node, shnex::NODES)?;
            Expr::Limit { nodes, n }
        } else if is(shnex::OFFSET) {
            let n = integer(&self.required(node, shnex::OFFSET)?, "shnex:offset")?;
            let nodes = self.expr_param(node, shnex::NODES)?;
            Expr::Offset { nodes, n }
        } else if is(shnex::ORDER_BY) {
            let by = self.expr_param(node, shnex::ORDER_BY)?;
            let nodes = self.expr_param(node, shnex::NODES)?;
            let desc = match self.single(node, shnex::DESC)? {
                Some(t) => boolean(&t, "shnex:desc")?,
                None => false,
            };
            Expr::OrderBy { nodes, by, desc }
        } else if is(shnex::FLAT_MAP) {
            let map = self.expr_param(node, shnex::FLAT_MAP)?;
            let nodes = self.opt_expr_param(node, shnex::NODES)?;
            Expr::FlatMap { nodes, map }
        } else if is(shnex::FIND_FIRST) {
            let s = self.required(node, shnex::FIND_FIRST)?;
            let shape = self.shape(&s)?;
            let nodes = self.opt_expr_param(node, shnex::NODES)?;
            Expr::FindFirst { nodes, shape }
        } else if is(shnex::MATCH_ALL) {
            let s = self.required(node, shnex::MATCH_ALL)?;
            let shape = self.shape(&s)?;
            let nodes = self.opt_expr_param(node, shnex::NODES)?;
            Expr::MatchAll { nodes, shape }
        } else if is(shnex::COUNT) {
            Expr::Count(self.expr_param(node, shnex::COUNT)?)
        } else if is(shnex::MIN) {
            Expr::Min(self.expr_param(node, shnex::MIN)?)
        } else if is(shnex::MAX) {
            Expr::Max(self.expr_param(node, shnex::MAX)?)
        } else if is(shnex::SUM) {
            Expr::Sum(self.expr_param(node, shnex::SUM)?)
        } else if is(shnex::INSTANCES_OF) {
            Expr::InstancesOf(self.expr_param(node, shnex::INSTANCES_OF)?)
        } else if is(shnex::NODES_MATCHING) {
            let s = self.required(node, shnex::NODES_MATCHING)?;
            Expr::NodesMatching(self.shape(&s)?)
        } else if is(shnex::ARG) {
            match self.required(node, shnex::ARG)? {
                Term::NamedNode(n) => Expr::Arg(ArgKey::Param(n)),
                t => Expr::Arg(ArgKey::Index(integer(&t, "shnex:arg")?)),
            }
        } else if is(sh::SELECT) || is(sh::SPARQL_EXPR) {
            let select = is(sh::SELECT);
            let p = if select { sh::SELECT } else { sh::SPARQL_EXPR };
            let t = self.required(node, p)?;
            let text = match &t {
                Term::Literal(l) if l.datatype() == xsd::STRING => l.value().to_string(),
                _ => bail!("{} expects an xsd:string, found {t}", p.as_str()),
            };
            let prefixes = crate::sparql::prefixes(&self.g, node);
            let text = if select {
                text
            } else {
                format!("SELECT ({text} AS ?result) WHERE {{}}")
            };
            let query = sparkles_core::sparql::parse_query(&text, None, &prefixes)
                .map_err(|e| anyhow!("{e}"))
                .with_context(|| format!("invalid SPARQL in {}:\n{text}", p.as_str()))?;
            if !matches!(query, Query::Select { .. }) {
                bail!("{} is not a SELECT query", p.as_str());
            }
            let st = Box::new(SparqlText { text, query });
            if select {
                Expr::Select(st)
            } else {
                Expr::SparqlExpr(st)
            }
        } else {
            bail!("unknown key parameter {key}");
        };
        Ok(self.push_expr(node, e))
    }

    /// The arguments of a list parameter function: the members of the list, none for
    /// `rdf:nil`, or the one argument itself.
    fn list_args(&mut self, o: &Term) -> Result<Vec<ExprId>> {
        let is_nil = matches!(o, Term::NamedNode(n) if n.as_ref() == rdf::NIL);
        if is_nil {
            return Ok(Vec::new());
        }
        if matches!(o, Term::BlankNode(_))
            && self.g.has(o, rdf::FIRST)
            && let Ok(items) = self.g.list(o)
        {
            return items.iter().map(|i| self.expr(i)).collect();
        }
        Ok(vec![self.expr(o)?])
    }

    fn list_function(&mut self, node: &Term, p: &NamedNode) -> Result<ExprId> {
        let o = self.required(node, p.as_ref())?;
        let args = self.list_args(&o)?;
        if let Some(&func) = self.fns.lists.get(p) {
            let args = args
                .into_iter()
                .enumerate()
                .map(|(i, expr)| CallArg {
                    key: ArgKey::Index(i as u64),
                    expr,
                })
                .collect();
            return Ok(self.push_expr(node, Expr::Custom { func, args }));
        }
        if p.as_str() == shnex::CONFORMS_TO_SHAPE.as_str() {
            let [n, s] = args[..] else {
                bail!(
                    "shnex:conformsToShape takes two arguments, found {}",
                    args.len()
                );
            };
            return Ok(self.push_expr(node, Expr::ConformsToShape { node: n, shape: s }));
        }
        let name = p
            .as_str()
            .strip_prefix(SPARQL_FN_NS)
            .unwrap_or("")
            .to_string();
        let text = crate::nodeeval::sparql_function_text(&name, args.len())?;
        let query = sparkles_core::sparql::parse_query(&text, None, &[])
            .map_err(|e| anyhow!("{e}"))
            .with_context(|| format!("sparql:{name} with {} arguments", args.len()))?;
        Ok(self.push_expr(
            node,
            Expr::SparqlFn {
                name,
                args,
                query: Box::new(query),
            },
        ))
    }

    fn custom_call(&mut self, node: &Term, func: usize) -> Result<ExprId> {
        let FnKind::Named { params } = self.out.functions[func].kind.clone() else {
            unreachable!("only named functions have key parameters")
        };
        // exactly one value for the key parameter that selected the function
        let mut args = Vec::new();
        for p in &params {
            let vals = self.g.objects(node, p.path.as_ref());
            match vals.len() {
                0 if p.key || p.optional => {}
                0 => bail!("the parameter {} is missing", p.path),
                1 => {
                    let expr = self.expr(&vals[0])?;
                    args.push(CallArg {
                        key: ArgKey::Param(p.path.clone()),
                        expr,
                    });
                }
                _ => bail!("the parameter {} has more than one value", p.path),
            }
        }
        Ok(self.push_expr(node, Expr::Custom { func, args }))
    }
}

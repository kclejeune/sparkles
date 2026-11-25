//! SHACL Advanced Features rules (the W3C Working Group Note of 2017, §8): `sh:rule` with
//! `sh:TripleRule` and `sh:SPARQLRule`, `sh:condition`, `sh:order` and `sh:deactivated`.
//!
//! [`Shapes::rules`] gives the rules of a shapes graph in the order they run. A rule
//! belongs to a shape, and its focus nodes are the shape's target nodes that conform to
//! every shape of its `sh:condition`. A triple rule evaluates its subject, predicate and
//! object node expressions at each focus node and infers every combination whose subject
//! is an IRI or a blank node and whose predicate is an IRI, as TopBraid's engine does. A
//! SPARQL rule runs its CONSTRUCT query once per focus node with `$this` pre-bound.
//!
//! The iteration of the rules, and where the inferred triples go, belong to the caller.
//! [`RuleData`] evaluates one rule over the state of a snapshot, and the reasoner's
//! `shacl-af` profile adds the inferred triples to the snapshot it reads next, through an
//! overlay of the facts derived so far.

use crate::data::DataGraph;
use crate::nodeeval::Scope;
use crate::nodeexpr::ExprId;
use crate::shapes::{Parser, ShapeId, Shapes, as_subject, literal_bool};
use crate::validate::{Cx, Engine, ValidateOptions};
use crate::vocab::{rdf, sh};
use anyhow::{Context as _, Result, anyhow, bail};
use oxrdf::Term;
use spargebra::Query;
use spargebra::term::TermPattern;
use sparkles_core::id::Id;
use sparkles_core::store::Snapshot;
use std::sync::Arc;

/// A rule of a shapes graph.
#[derive(Clone, Debug)]
pub struct Rule {
    /// the rule's node in the shapes graph
    pub node: Term,
    /// the shape whose targets are the rule's focus nodes
    pub shape: ShapeId,
    /// the shape's `sh:order`, 0 when it has none
    pub shape_order: f64,
    /// the rule's `sh:order`, 0 when it has none
    pub order: f64,
    /// the shapes of `sh:condition`
    pub conditions: Vec<ShapeId>,
    pub kind: RuleKind,
}

/// What a rule infers.
#[derive(Clone, Debug)]
pub enum RuleKind {
    /// `sh:TripleRule`: the node expressions of `sh:subject`, `sh:predicate` and
    /// `sh:object`
    Triple {
        subject: ExprId,
        predicate: ExprId,
        object: ExprId,
    },
    /// `sh:SPARQLRule`: the CONSTRUCT query of `sh:construct`
    Sparql { text: String, query: Box<Query> },
}

impl Rule {
    /// The key that orders the rules: the shape's order, then the rule's. Rules with
    /// equal keys run together.
    pub fn key(&self) -> (f64, f64) {
        (self.shape_order, self.order)
    }
}

impl Shapes {
    /// The shapes of the store graphs `graphs` (a graph that does not exist gives
    /// nothing) for their rules, with their SPIN rules translated to SPARQL rules
    /// ([`crate::spin`], whose queries get the declarations of `prefixes`). Blank nodes
    /// keep their store identity.
    pub fn for_rules(
        snap: &Snapshot,
        graphs: &[String],
        prefixes: &[(String, String)],
    ) -> Result<Shapes> {
        let mut g = oxrdf::Graph::new();
        for iri in graphs {
            if sparkles_core::validation::graph_id(snap, iri).is_none() {
                continue;
            }
            g.extend(crate::data::read_graph(snap, Some(iri))?.iter());
        }
        if crate::spin::has_rules(&g) {
            let spin = crate::spin::translate(&g, prefixes)?;
            g.extend(spin.iter());
        }
        let mut shapes = Parser::new(&g).parse(true)?;
        shapes.source_graph = graphs.first().cloned();
        Ok(shapes)
    }

    /// The active SHACL-AF rules of the shapes graph, ordered by [`Rule::key`] and then by
    /// their nodes. The rules of deactivated shapes and deactivated rules are left out.
    /// A rule the parser could not read fails here, and not when the shapes are read for
    /// validation.
    pub fn rules(&self) -> Result<&[Rule]> {
        match &self.rule_error {
            Some(e) => Err(anyhow!("{e}")),
            None => Ok(&self.rules),
        }
    }
}

fn number(t: &Term, what: &str) -> Result<f64> {
    match t {
        Term::Literal(l) => l
            .value()
            .trim()
            .parse::<f64>()
            .ok()
            .filter(|n| n.is_finite())
            .ok_or_else(|| anyhow!("{what} {t} is not a number")),
        _ => bail!("{what} {t} is not a number"),
    }
}

impl Parser<'_> {
    /// Read every `sh:rule` of the shapes graph into [`Shapes::rules`].
    pub(crate) fn parse_rules(&mut self) -> Result<()> {
        let mut pairs = Vec::new();
        for t in self.g.g.triples_for_predicate(sh::RULE) {
            self.check_deadline()?;
            pairs.push((Term::from(t.subject.into_owned()), t.object.into_owned()));
        }
        pairs.sort_by_cached_key(|(s, r)| (s.to_string(), r.to_string()));
        self.check_deadline()?;
        for (s, r) in pairs {
            let shape = self
                .shape(&s)
                .with_context(|| format!("the shape {s} of a rule"))?;
            if self.out.shapes[shape].deactivated {
                continue;
            }
            if let Some(rule) = self
                .rule(shape, &r)
                .with_context(|| format!("the rule {r}"))?
            {
                self.out.rules.push(rule);
            }
        }
        self.out.rules.sort_by(|a, b| {
            let (x, y) = (a.key(), b.key());
            x.0.total_cmp(&y.0)
                .then(x.1.total_cmp(&y.1))
                .then_with(|| a.node.to_string().cmp(&b.node.to_string()))
        });
        Ok(())
    }

    fn rule(&mut self, shape: ShapeId, r: &Term) -> Result<Option<Rule>> {
        if as_subject(r).is_none() {
            bail!("a rule must be an IRI or a blank node");
        }
        if self
            .g
            .objects(r, sh::DEACTIVATED)
            .iter()
            .any(|t| literal_bool(t) == Some(true))
        {
            return Ok(None);
        }
        let s_node = self.out.shapes[shape].node.clone();
        let shape_order = match self.g.object(&s_node, sh::ORDER) {
            Some(t) => number(&t, "sh:order")?,
            None => 0.0,
        };
        let order = match self.g.objects(r, sh::ORDER).as_slice() {
            [] => 0.0,
            [t] => number(t, "sh:order")?,
            _ => bail!("sh:order has more than one value"),
        };
        let mut conditions = Vec::new();
        for c in self.g.objects(r, sh::CONDITION) {
            conditions.push(self.shape(&c).context("sh:condition")?);
        }
        let triple = self.g.is_instance(r, sh::TRIPLE_RULE);
        let sparql = self.g.is_instance(r, sh::SPARQL_RULE);
        let kind = match (triple, sparql) {
            (true, false) => {
                let mut part = |p| -> Result<ExprId> {
                    match self.g.objects(r, p).as_slice() {
                        [t] => self.af_expr(t).with_context(|| p.as_str().to_string()),
                        [] => bail!("a triple rule needs one {}", p.as_str()),
                        _ => bail!("{} has more than one value", p.as_str()),
                    }
                };
                RuleKind::Triple {
                    subject: part(sh::SUBJECT)?,
                    predicate: part(sh::PREDICATE)?,
                    object: part(sh::OBJECT)?,
                }
            }
            (false, true) => {
                let text = match self.g.objects(r, sh::CONSTRUCT).as_slice() {
                    [Term::Literal(l)] => l.value().to_string(),
                    [_] => bail!("sh:construct is not a string"),
                    [] => bail!("a SPARQL rule needs sh:construct"),
                    _ => bail!("sh:construct has more than one value"),
                };
                let prefixes = crate::sparql::prefixes(&self.g, r, self.deadline)?;
                let query = crate::sparql::parse(&text, &prefixes, &["this".to_string()])?;
                if !matches!(query, Query::Construct { .. }) {
                    bail!("sh:construct is not a CONSTRUCT query");
                }
                RuleKind::Sparql {
                    text,
                    query: Box::new(query),
                }
            }
            (true, true) => bail!("a rule is both a triple rule and a SPARQL rule"),
            (false, false) => {
                let types = self.g.objects(r, rdf::TYPE);
                bail!(
                    "unsupported rule type{}",
                    match types.first() {
                        Some(t) => format!(" {t}"),
                        None => " (the rule has no rdf:type)".into(),
                    }
                )
            }
        };
        Ok(Some(Rule {
            node: r.clone(),
            shape,
            shape_order,
            order,
            conditions,
            kind,
        }))
    }
}

/// A node a rule inferred.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Node {
    /// a term of the store
    Id(Id),
    /// a term the rule computed, which the store does not have
    Term(Term),
    /// a blank node of a CONSTRUCT template, named by the template's label and the
    /// solution's values of the query's variables, `None` where one is unbound: the same
    /// solution gives the same node, so that a rule with blank nodes reaches a fixed point
    Blank {
        label: String,
        solution: Vec<Option<Node>>,
    },
}

/// The data graph rules read: a snapshot and the graphs that make up the data graph.
pub struct RuleData {
    data: DataGraph,
    ids: Vec<Id>,
    opts: ValidateOptions,
    /// the CONSTRUCT queries run so far, which name the new blank nodes of each apart
    executions: std::sync::atomic::AtomicU64,
}

impl RuleData {
    /// The data graph of `snap` for the rules of `shapes`: the graph `data_graph` (see
    /// [`ValidateOptions::data_graph`]) with the graphs `extra`. Validation of the
    /// conditions runs on the caller's thread.
    pub fn new(
        snap: Arc<Snapshot>,
        shapes: &Shapes,
        data_graph: Option<&str>,
        extra: &[String],
        opts: &ValidateOptions,
    ) -> Result<RuleData> {
        let (data, ids) = DataGraph::new(snap, data_graph, extra, &[], shapes)?;
        let opts = ValidateOptions {
            parallel: false,
            ..opts.clone()
        };
        Ok(RuleData {
            data,
            ids,
            opts,
            executions: Default::default(),
        })
    }

    fn node(&self, id: Id) -> Result<Node> {
        if id.tag() == sparkles_core::id::Tag::Local {
            match self.data.term(id) {
                Some(t) => Ok(Node::Term(t)),
                None => bail!("cannot decode id {id:?}"),
            }
        } else {
            Ok(Node::Id(id))
        }
    }

    fn term_of(&self, id: Id) -> Result<Term> {
        self.data
            .term(id)
            .ok_or_else(|| anyhow!("cannot decode id {id:?}"))
    }

    /// Evaluate `rule` over the data graph and pass each triple it infers to `emit`. A
    /// triple rule passes each triple once per focus node, a SPARQL rule once per solution
    /// and template triple, and the caller removes the duplicates, within its budget.
    pub fn run(
        &self,
        shapes: &Shapes,
        rule: &Rule,
        emit: &mut dyn FnMut([Node; 3]) -> Result<()>,
    ) -> Result<()> {
        let engine = Engine::new(shapes, &self.data, self.ids.clone(), &self.opts)?;
        let mut cx = Cx::default();
        let focus = engine.focus_nodes(rule.shape)?;
        'focus: for (i, f) in focus.into_iter().enumerate() {
            if i % 64 == 0 {
                engine.check_limits()?;
            }
            for &c in &rule.conditions {
                if !engine.conforms(c, f, &mut cx)? {
                    continue 'focus;
                }
            }
            match &rule.kind {
                RuleKind::Triple {
                    subject,
                    predicate,
                    object,
                } => {
                    let scope = Scope::default();
                    let os = engine.eval(*object, f, &scope, &mut cx)?;
                    if os.is_empty() {
                        continue;
                    }
                    // the subjects that are not literals and the predicates that are
                    // IRIs, each once, so that every combination is a new triple
                    let mut ss = engine.eval(*subject, f, &scope, &mut cx)?;
                    ss.sort_unstable();
                    ss.dedup();
                    let mut keep = Vec::with_capacity(ss.len());
                    for s in ss {
                        if !matches!(self.term_of(s)?, Term::Literal(_)) {
                            keep.push(s);
                        }
                    }
                    let ss = keep;
                    let mut ps = engine.eval(*predicate, f, &scope, &mut cx)?;
                    ps.sort_unstable();
                    ps.dedup();
                    let mut keep = Vec::with_capacity(ps.len());
                    for p in ps {
                        if matches!(self.term_of(p)?, Term::NamedNode(_)) {
                            keep.push(p);
                        }
                    }
                    let ps = keep;
                    let mut os = os;
                    os.sort_unstable();
                    os.dedup();
                    let mut n = 0u32;
                    for s in &ss {
                        for p in &ps {
                            for o in &os {
                                n = n.wrapping_add(1);
                                if n.is_multiple_of(4096) {
                                    engine.check_limits()?;
                                }
                                emit([self.node(*s)?, self.node(*p)?, self.node(*o)?])?;
                            }
                        }
                    }
                }
                RuleKind::Sparql { query, .. } => {
                    self.construct(&engine, query, f, emit)?;
                }
            }
        }
        Ok(())
    }

    /// The triples of a CONSTRUCT query with `$this` pre-bound to `focus`.
    fn construct(
        &self,
        engine: &Engine<'_>,
        query: &Query,
        focus: Id,
        emit: &mut dyn FnMut([Node; 3]) -> Result<()>,
    ) -> Result<()> {
        let Query::Construct { template, .. } = query else {
            bail!("not a CONSTRUCT query");
        };
        let this = self.term_of(focus)?;
        let (table, ctx) = engine.run_query(query, &[("this", this)])?;
        let exec = self
            .executions
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        // a blank node the query made is new in each execution: its label is kept apart
        // from those of the other executions of the rule
        let tag = format!("e{exec}");
        // a blank node is named by every variable of the solution, so that solutions
        // that differ only in a variable the template does not use still differ
        let all: Vec<(String, usize)> = table_vars(&table, &ctx);
        for row in 0..table.len() {
            if row % 4096 == 4095 {
                engine.check_limits()?;
            }
            let value = |name: &str| -> Result<Option<Node>> {
                if name == "this" {
                    return Ok(Some(Node::Id(focus)));
                }
                let Some(&(_, col)) = all.iter().find(|(n, _)| n == name) else {
                    return Ok(None);
                };
                match Engine::fresh_result_term(&ctx, table.get(row, col), &tag)? {
                    Some(t) => Ok(Some(self.node(self.data.resolve_shared(&t, true))?)),
                    None => Ok(None),
                }
            };
            let mut solution: Option<Vec<Option<Node>>> = None;
            let mut node = |tp: &TermPattern| -> Result<Option<Node>> {
                Ok(match tp {
                    TermPattern::Variable(v) => value(v.as_str())?,
                    TermPattern::NamedNode(n) => Some(
                        self.node(self.data.resolve_shared(&Term::NamedNode(n.clone()), true))?,
                    ),
                    TermPattern::Literal(l) => {
                        Some(self.node(self.data.resolve_shared(&Term::Literal(l.clone()), true))?)
                    }
                    TermPattern::BlankNode(b) => {
                        if solution.is_none() {
                            let mut s = vec![Some(Node::Id(focus))];
                            for (n, _) in &all {
                                s.push(value(n)?);
                            }
                            solution = Some(s);
                        }
                        Some(Node::Blank {
                            label: b.as_str().to_string(),
                            solution: solution.clone().unwrap_or_default(),
                        })
                    }
                    TermPattern::Triple(_) => bail!("triple terms in a rule's template"),
                })
            };
            for t in template {
                let Some(s) = node(&t.subject)? else { continue };
                let p = match &t.predicate {
                    spargebra::term::NamedNodePattern::NamedNode(n) => {
                        node(&TermPattern::NamedNode(n.clone()))?
                    }
                    spargebra::term::NamedNodePattern::Variable(v) => {
                        node(&TermPattern::Variable(v.clone()))?
                    }
                };
                let Some(p) = p else { continue };
                let Some(o) = node(&t.object)? else { continue };
                if !self.valid_subject(&s)? || !self.valid_predicate(&p)? {
                    continue;
                }
                emit([s, p, o])?;
            }
        }
        Ok(())
    }

    fn valid_subject(&self, n: &Node) -> Result<bool> {
        Ok(match n {
            Node::Id(id) => !matches!(self.term_of(*id)?, Term::Literal(_)),
            Node::Term(t) => !matches!(t, Term::Literal(_)),
            Node::Blank { .. } => true,
        })
    }

    fn valid_predicate(&self, n: &Node) -> Result<bool> {
        Ok(match n {
            Node::Id(id) => matches!(self.term_of(*id)?, Term::NamedNode(_)),
            Node::Term(t) => matches!(t, Term::NamedNode(_)),
            Node::Blank { .. } => false,
        })
    }
}

/// The variables of a query's result table that the query names, with their columns,
/// sorted by name. The engine's own variables start with a space.
fn table_vars(
    table: &sparkles_core::sparql::table::Table,
    ctx: &sparkles_core::sparql::Ctx,
) -> Vec<(String, usize)> {
    let mut out: Vec<(String, usize)> = (0..table.width())
        .map(|c| (ctx.var_name(table.vars[c]), c))
        .filter(|(n, _)| !n.starts_with(' ') && !n.is_empty())
        .collect();
    out.sort();
    out
}

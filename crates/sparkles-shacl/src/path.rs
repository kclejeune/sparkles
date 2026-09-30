//! SHACL property paths (SHACL §2.3.1): parsing from the shapes graph, SPARQL syntax,
//! RDF serialization and evaluation over the data graph.

use crate::data::DataGraph;
use crate::shapes::G;
use crate::vocab::{rdf, sh};
use anyhow::{Result, anyhow, bail};
use oxrdf::{BlankNode, NamedNode, Term, Triple};
use rustc_hash::FxHashSet;
use sparkles::id::Id;
use std::fmt;

/// A SHACL property path.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum PropertyPath {
    Predicate(NamedNode),
    Inverse(Box<PropertyPath>),
    Sequence(Vec<PropertyPath>),
    Alternative(Vec<PropertyPath>),
    ZeroOrMore(Box<PropertyPath>),
    OneOrMore(Box<PropertyPath>),
    ZeroOrOne(Box<PropertyPath>),
}

impl PropertyPath {
    /// Parse the path rooted at `node` of a shapes graph.
    pub(crate) fn from_graph(g: &G<'_>, node: &Term) -> Result<PropertyPath> {
        Self::parse(g, node, 0)
    }

    fn parse(g: &G<'_>, node: &Term, depth: usize) -> Result<PropertyPath> {
        if depth > 64 {
            bail!("property path nested too deeply at {node}");
        }
        match node {
            Term::NamedNode(n) => return Ok(PropertyPath::Predicate(n.clone())),
            Term::Literal(_) => bail!("a literal is not a property path: {node}"),
            Term::Triple(_) => bail!("a triple term is not a property path: {node}"),
            Term::BlankNode(_) => {}
        }
        if g.has(node, rdf::FIRST) {
            let items = g.list(node)?;
            if items.len() < 2 {
                bail!("a sequence path needs at least two members: {node}");
            }
            return Ok(PropertyPath::Sequence(
                items
                    .iter()
                    .map(|i| Self::parse(g, i, depth + 1))
                    .collect::<Result<_>>()?,
            ));
        }
        let one = |p| -> Result<Option<Box<PropertyPath>>> {
            match g.object(node, p) {
                Some(t) => Ok(Some(Box::new(Self::parse(g, &t, depth + 1)?))),
                None => Ok(None),
            }
        };
        if let Some(p) = one(sh::INVERSE_PATH)? {
            return Ok(PropertyPath::Inverse(p));
        }
        if let Some(l) = g.object(node, sh::ALTERNATIVE_PATH) {
            let items = g.list(&l)?;
            if items.len() < 2 {
                bail!("an alternative path needs at least two members: {node}");
            }
            return Ok(PropertyPath::Alternative(
                items
                    .iter()
                    .map(|i| Self::parse(g, i, depth + 1))
                    .collect::<Result<_>>()?,
            ));
        }
        if let Some(p) = one(sh::ZERO_OR_MORE_PATH)? {
            return Ok(PropertyPath::ZeroOrMore(p));
        }
        if let Some(p) = one(sh::ONE_OR_MORE_PATH)? {
            return Ok(PropertyPath::OneOrMore(p));
        }
        if let Some(p) = one(sh::ZERO_OR_ONE_PATH)? {
            return Ok(PropertyPath::ZeroOrOne(p));
        }
        Err(anyhow!("not a SHACL property path: {node}"))
    }

    /// The predicate if this is a plain predicate path.
    pub fn as_predicate(&self) -> Option<&NamedNode> {
        match self {
            PropertyPath::Predicate(p) => Some(p),
            _ => None,
        }
    }

    /// SPARQL 1.1 property path syntax (used for `$PATH` substitution).
    pub fn to_sparql(&self) -> String {
        fn wrap(p: &PropertyPath) -> String {
            match p {
                PropertyPath::Predicate(_) => p.to_sparql(),
                _ => format!("({})", p.to_sparql()),
            }
        }
        match self {
            PropertyPath::Predicate(p) => p.to_string(),
            PropertyPath::Inverse(p) => format!("^{}", wrap(p)),
            PropertyPath::Sequence(ps) => ps.iter().map(wrap).collect::<Vec<_>>().join("/"),
            PropertyPath::Alternative(ps) => ps.iter().map(wrap).collect::<Vec<_>>().join("|"),
            PropertyPath::ZeroOrMore(p) => format!("{}*", wrap(p)),
            PropertyPath::OneOrMore(p) => format!("{}+", wrap(p)),
            PropertyPath::ZeroOrOne(p) => format!("{}?", wrap(p)),
        }
    }

    /// Serialize as SHACL path RDF (fresh blank nodes); returns the path node.
    pub fn to_rdf(&self, out: &mut Vec<Triple>) -> Term {
        fn list(items: &[PropertyPath], out: &mut Vec<Triple>) -> Term {
            let mut head: Term = rdf::NIL.into_owned().into();
            for it in items.iter().rev() {
                let cell = BlankNode::default();
                let v = it.to_rdf(out);
                out.push(Triple::new(cell.clone(), rdf::FIRST.into_owned(), v));
                out.push(Triple::new(cell.clone(), rdf::REST.into_owned(), head));
                head = cell.into();
            }
            head
        }
        let unary = |p: oxrdf::NamedNodeRef<'_>, inner: &PropertyPath, out: &mut Vec<Triple>| {
            let b = BlankNode::default();
            let v = inner.to_rdf(out);
            out.push(Triple::new(b.clone(), p.into_owned(), v));
            Term::from(b)
        };
        match self {
            PropertyPath::Predicate(p) => p.clone().into(),
            PropertyPath::Inverse(p) => unary(sh::INVERSE_PATH, p, out),
            PropertyPath::ZeroOrMore(p) => unary(sh::ZERO_OR_MORE_PATH, p, out),
            PropertyPath::OneOrMore(p) => unary(sh::ONE_OR_MORE_PATH, p, out),
            PropertyPath::ZeroOrOne(p) => unary(sh::ZERO_OR_ONE_PATH, p, out),
            PropertyPath::Sequence(ps) => list(ps, out),
            PropertyPath::Alternative(ps) => {
                let b = BlankNode::default();
                let l = list(ps, out);
                out.push(Triple::new(b.clone(), sh::ALTERNATIVE_PATH.into_owned(), l));
                b.into()
            }
        }
    }

    /// Parse a path from an arbitrary graph node (e.g. `sh:resultPath` of a report).
    pub fn from_rdf(graph: &oxrdf::Graph, node: &Term) -> Result<PropertyPath> {
        Self::parse(&G { g: graph }, node, 0)
    }
}

impl fmt::Display for PropertyPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_sparql())
    }
}

// ============================================================================ evaluation

/// A path with predicates resolved to store ids.
#[derive(Clone, Debug)]
pub(crate) enum CPath {
    Pred(Id),
    Inverse(Box<CPath>),
    Seq(Vec<CPath>),
    Alt(Vec<CPath>),
    ZeroOrMore(Box<CPath>),
    OneOrMore(Box<CPath>),
    ZeroOrOne(Box<CPath>),
}

impl CPath {
    pub fn compile(p: &PropertyPath, resolve: &mut impl FnMut(&NamedNode) -> Id) -> CPath {
        let mut c = |x: &PropertyPath| Box::new(CPath::compile(x, resolve));
        match p {
            PropertyPath::Predicate(n) => CPath::Pred(resolve(n)),
            PropertyPath::Inverse(x) => CPath::Inverse(c(x)),
            PropertyPath::ZeroOrMore(x) => CPath::ZeroOrMore(c(x)),
            PropertyPath::OneOrMore(x) => CPath::OneOrMore(c(x)),
            PropertyPath::ZeroOrOne(x) => CPath::ZeroOrOne(c(x)),
            PropertyPath::Sequence(xs) => {
                CPath::Seq(xs.iter().map(|x| CPath::compile(x, resolve)).collect())
            }
            PropertyPath::Alternative(xs) => {
                CPath::Alt(xs.iter().map(|x| CPath::compile(x, resolve)).collect())
            }
        }
    }

    /// Value nodes reachable from `node` (distinct, in discovery order).
    pub fn eval(&self, data: &DataGraph, node: Id) -> anyhow::Result<Vec<Id>> {
        let mut out = Vec::new();
        let mut seen = FxHashSet::default();
        self.eval_into(data, node, false, &mut |v| {
            if seen.insert(v) {
                out.push(v);
            }
        })?;
        Ok(out)
    }

    fn eval_into(
        &self,
        data: &DataGraph,
        node: Id,
        inverse: bool,
        emit: &mut dyn FnMut(Id),
    ) -> anyhow::Result<()> {
        match self {
            CPath::Pred(p) => {
                let vs = if inverse {
                    data.subjects(*p, node)?
                } else {
                    data.objects(node, *p)?
                };
                vs.into_iter().for_each(emit);
            }
            CPath::Inverse(x) => x.eval_into(data, node, !inverse, emit)?,
            CPath::Alt(xs) => {
                for x in xs {
                    x.eval_into(data, node, inverse, emit)?;
                }
            }
            CPath::Seq(xs) => {
                let mut cur = vec![node];
                let order: Box<dyn Iterator<Item = &CPath>> = if inverse {
                    Box::new(xs.iter().rev())
                } else {
                    Box::new(xs.iter())
                };
                for x in order {
                    let mut next = Vec::new();
                    let mut seen = FxHashSet::default();
                    for n in cur {
                        x.eval_into(data, n, inverse, &mut |v| {
                            if seen.insert(v) {
                                next.push(v);
                            }
                        })?;
                    }
                    cur = next;
                }
                cur.into_iter().for_each(emit);
            }
            CPath::ZeroOrOne(x) => {
                emit(node);
                x.eval_into(data, node, inverse, emit)?;
            }
            CPath::ZeroOrMore(x) | CPath::OneOrMore(x) => {
                let zero = matches!(self, CPath::ZeroOrMore(_));
                let mut seen = FxHashSet::default();
                if zero {
                    seen.insert(node);
                    emit(node);
                }
                let mut frontier = vec![node];
                while let Some(n) = frontier.pop() {
                    let mut step = Vec::new();
                    x.eval_into(data, n, inverse, &mut |v| step.push(v))?;
                    for v in step {
                        if seen.insert(v) {
                            emit(v);
                            frontier.push(v);
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

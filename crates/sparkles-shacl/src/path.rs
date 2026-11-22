//! SHACL property paths (SHACL §2.3.1): parsing from the shapes graph, SPARQL syntax,
//! RDF serialization and evaluation over the data graph.

use crate::data::DataGraph;
use crate::shapes::G;
use crate::vocab::{rdf, sh};
use anyhow::{Result, anyhow, bail};
use oxrdf::{BlankNode, NamedNode, Term, Triple};
use rustc_hash::FxHashSet;
use sparkles_core::id::Id;
use std::fmt;
use std::time::Instant;

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
    pub(crate) fn from_graph(
        g: &G<'_>,
        node: &Term,
        deadline: Option<Instant>,
    ) -> Result<PropertyPath> {
        Self::parse(g, node, 0, deadline)
    }

    fn parse(
        g: &G<'_>,
        node: &Term,
        depth: usize,
        deadline: Option<Instant>,
    ) -> Result<PropertyPath> {
        crate::data::check_deadline(deadline)?;
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
            let items = g.list(node, deadline)?;
            if items.len() < 2 {
                bail!("a sequence path needs at least two members: {node}");
            }
            return Ok(PropertyPath::Sequence(
                items
                    .iter()
                    .map(|i| Self::parse(g, i, depth + 1, deadline))
                    .collect::<Result<_>>()?,
            ));
        }
        let one = |p| -> Result<Option<Box<PropertyPath>>> {
            match g.object(node, p) {
                Some(t) => Ok(Some(Box::new(Self::parse(g, &t, depth + 1, deadline)?))),
                None => Ok(None),
            }
        };
        if let Some(p) = one(sh::INVERSE_PATH)? {
            return Ok(PropertyPath::Inverse(p));
        }
        if let Some(l) = g.object(node, sh::ALTERNATIVE_PATH) {
            let items = g.list(&l, deadline)?;
            if items.len() < 2 {
                bail!("an alternative path needs at least two members: {node}");
            }
            return Ok(PropertyPath::Alternative(
                items
                    .iter()
                    .map(|i| Self::parse(g, i, depth + 1, deadline))
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
        self.to_sparql_checked(|| Ok(()))
            .expect("uncontrolled rendering")
    }

    pub(crate) fn to_sparql_checked(
        &self,
        check: impl Fn() -> sparkles_core::Result<()>,
    ) -> sparkles_core::Result<String> {
        let mut out = String::new();
        self.write_sparql(&mut out, false, &check)?;
        Ok(out)
    }

    fn write_sparql(
        &self,
        out: &mut String,
        wrap: bool,
        check: &impl Fn() -> sparkles_core::Result<()>,
    ) -> sparkles_core::Result<()> {
        use PropertyPath::*;
        check()?;
        let wrap = wrap && !matches!(self, Predicate(_));
        if wrap {
            out.push('(');
        }
        match self {
            Predicate(predicate) => {
                #[cfg(test)]
                ObservePathRender::predicate();
                out.push('<');
                append_checked(out, predicate.as_str(), check)?;
                out.push('>');
            }
            Inverse(path) => {
                out.push('^');
                path.write_sparql(out, true, check)?;
            }
            Sequence(paths) | Alternative(paths) => {
                let separator = if matches!(self, Sequence(_)) {
                    '/'
                } else {
                    '|'
                };
                for (i, path) in paths.iter().enumerate() {
                    if i != 0 {
                        out.push(separator);
                    }
                    path.write_sparql(out, true, check)?;
                }
            }
            ZeroOrMore(path) | OneOrMore(path) | ZeroOrOne(path) => {
                path.write_sparql(out, true, check)?;
                out.push(match self {
                    ZeroOrMore(_) => '*',
                    OneOrMore(_) => '+',
                    _ => '?',
                });
            }
        }
        if wrap {
            out.push(')');
        }
        Ok(())
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
        Self::from_graph(&G { g: graph }, node, None)
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
        Self::compile_checked(p, resolve, &|| Ok(()))
            .expect("an uncontrolled path translation cannot time out")
    }

    pub fn compile_checked(
        p: &PropertyPath,
        resolve: &mut impl FnMut(&NamedNode) -> Id,
        check: &impl Fn() -> sparkles_core::Result<()>,
    ) -> sparkles_core::Result<CPath> {
        check()?;
        let mut child = |x: &PropertyPath| Self::compile_checked(x, resolve, check).map(Box::new);
        Ok(match p {
            PropertyPath::Predicate(n) => {
                #[cfg(test)]
                ObservePathCompile::predicate();
                CPath::Pred(resolve(n))
            }
            PropertyPath::Inverse(x) => CPath::Inverse(child(x)?),
            PropertyPath::ZeroOrMore(x) => CPath::ZeroOrMore(child(x)?),
            PropertyPath::OneOrMore(x) => CPath::OneOrMore(child(x)?),
            PropertyPath::ZeroOrOne(x) => CPath::ZeroOrOne(child(x)?),
            PropertyPath::Sequence(xs) => CPath::Seq(
                xs.iter()
                    .map(|x| Self::compile_checked(x, resolve, check))
                    .collect::<sparkles_core::Result<_>>()?,
            ),
            PropertyPath::Alternative(xs) => CPath::Alt(
                xs.iter()
                    .map(|x| Self::compile_checked(x, resolve, check))
                    .collect::<sparkles_core::Result<_>>()?,
            ),
        })
    }

    /// Value nodes reachable from `node` (distinct, in discovery order).
    pub fn eval(&self, data: &DataGraph, node: Id) -> anyhow::Result<Vec<Id>> {
        self.eval_checked(data, node, || Ok(()))
    }

    pub fn eval_checked(
        &self,
        data: &DataGraph,
        node: Id,
        check: impl Fn() -> sparkles_core::Result<()>,
    ) -> anyhow::Result<Vec<Id>> {
        let mut out = Vec::new();
        let mut seen = FxHashSet::default();
        self.eval_into(data, node, false, &check, &mut |v| {
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
        check: &impl Fn() -> sparkles_core::Result<()>,
        emit: &mut dyn FnMut(Id),
    ) -> anyhow::Result<()> {
        check()?;
        match self {
            CPath::Pred(p) => {
                let vs = if inverse {
                    data.subjects_checked(*p, node, check)?
                } else {
                    data.objects_checked(node, *p, check)?
                };
                for value in vs {
                    check()?;
                    emit(value);
                }
            }
            CPath::Inverse(x) => x.eval_into(data, node, !inverse, check, emit)?,
            CPath::Alt(xs) => {
                for x in xs {
                    x.eval_into(data, node, inverse, check, emit)?;
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
                    check()?;
                    let mut next = Vec::new();
                    let mut seen = FxHashSet::default();
                    for n in cur {
                        x.eval_into(data, n, inverse, check, &mut |v| {
                            if seen.insert(v) {
                                next.push(v);
                            }
                        })?;
                    }
                    cur = next;
                }
                for value in cur {
                    check()?;
                    emit(value);
                }
            }
            CPath::ZeroOrOne(x) => {
                emit(node);
                x.eval_into(data, node, inverse, check, emit)?;
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
                    x.eval_into(data, n, inverse, check, &mut |v| step.push(v))?;
                    for v in step {
                        check()?;
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

/// Append UTF-8 text while checking between bounded copies.
pub(crate) fn append_checked(
    out: &mut String,
    mut text: &str,
    check: &impl Fn() -> sparkles_core::Result<()>,
) -> sparkles_core::Result<()> {
    while !text.is_empty() {
        check()?;
        let mut end = text.len().min(4096);
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        out.push_str(&text[..end]);
        text = &text[end..];
    }
    Ok(())
}

#[cfg(test)]
thread_local! {
    static PATH_PREDICATES: std::cell::RefCell<Option<PathCompilationProbe>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
struct PathCompilationProbe {
    visited: usize,
    cancel: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
}

#[cfg(test)]
pub(crate) struct ObservePathCompile;

#[cfg(test)]
impl ObservePathCompile {
    pub(crate) fn start() -> Self {
        Self::observe(None)
    }

    pub(crate) fn cancel_during_compile(
        flag: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) -> Self {
        Self::observe(Some(flag))
    }

    fn observe(cancel: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>) -> Self {
        PATH_PREDICATES
            .with(|probe| probe.replace(Some(PathCompilationProbe { visited: 0, cancel })));
        Self
    }

    pub(crate) fn count(&self) -> usize {
        PATH_PREDICATES.with(|probe| probe.borrow().as_ref().unwrap().visited)
    }

    fn predicate() {
        PATH_PREDICATES.with(|probe| {
            if let Some(probe) = probe.borrow_mut().as_mut() {
                probe.visited += 1;
                if probe.visited == 5 {
                    if let Some(flag) = &probe.cancel {
                        flag.store(true, std::sync::atomic::Ordering::Relaxed);
                    } else {
                        crate::data::set_deadline_checks_left(Some(0));
                    }
                }
            }
        })
    }
}

#[cfg(test)]
impl Drop for ObservePathCompile {
    fn drop(&mut self) {
        PATH_PREDICATES.with(|probe| probe.replace(None));
        crate::data::set_deadline_checks_left(None);
    }
}

#[cfg(test)]
thread_local! {
    static RENDERED_PATH_PREDICATES: std::cell::Cell<Option<(usize, bool)>> =
        const { std::cell::Cell::new(None) };
}

#[cfg(test)]
pub(crate) struct ObservePathRender;

#[cfg(test)]
impl ObservePathRender {
    pub(crate) fn start(expire: bool) -> Self {
        RENDERED_PATH_PREDICATES.set(Some((0, expire)));
        Self
    }

    pub(crate) fn count(&self) -> usize {
        RENDERED_PATH_PREDICATES.get().unwrap().0
    }

    fn predicate() {
        if let Some((visited, expire)) = RENDERED_PATH_PREDICATES.get() {
            RENDERED_PATH_PREDICATES.set(Some((visited + 1, expire)));
            if expire && visited == 4 {
                crate::data::set_deadline_checks_left(Some(0));
            }
        }
    }
}

#[cfg(test)]
impl Drop for ObservePathRender {
    fn drop(&mut self) {
        RENDERED_PATH_PREDICATES.set(None);
        crate::data::set_deadline_checks_left(None);
    }
}

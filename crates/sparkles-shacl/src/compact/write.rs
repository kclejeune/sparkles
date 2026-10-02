//! The SHACLC writer (spec G03 §5): it walks a shapes graph from its node shapes,
//! claims each triple a compact construct produces, and fails when a triple is left
//! unclaimed, so a written document always reads back to the same graph.

use super::read::STANDARD_PREFIXES;
use super::{NODE_PARAMS, NotCompact, PROPERTY_PARAMS, is_datatype_iri};
use crate::vocab::{SH_NS, owl, rdf, rdfs, sh};
use oxrdf::vocab::xsd;
use oxrdf::{BlankNode, Graph, Literal, NamedNode, NamedOrBlankNode, Term, Triple};
use rustc_hash::{FxHashMap, FxHashSet};
use std::fmt::Write as _;

const NODE_KINDS: [&str; 6] = [
    "BlankNode",
    "IRI",
    "Literal",
    "BlankNodeOrIRI",
    "BlankNodeOrLiteral",
    "IRIOrLiteral",
];

const INDENT: &str = "    ";

pub(crate) fn write(graph: &Graph, prefixes: &[(String, String)]) -> Result<String, NotCompact> {
    let mut w = Writer::new(graph, prefixes);
    let body = w.document();
    let unclaimed: Vec<Triple> = graph
        .iter()
        .map(|t| t.into_owned())
        .filter(|t| !w.claimed.contains(t))
        .collect();
    if !unclaimed.is_empty() {
        let mut triples = unclaimed.clone();
        triples.sort_by_key(|t| t.to_string());
        triples.truncate(10);
        return Err(NotCompact {
            triples,
            total: unclaimed.len(),
        });
    }
    let mut out = String::new();
    if let Some((base, imports)) = &w.ontology {
        let _ = writeln!(out, "BASE <{base}>\n");
        for i in imports {
            let _ = writeln!(out, "IMPORTS <{i}>");
        }
        if !imports.is_empty() {
            out.push('\n');
        }
    }
    let mut lines: Vec<String> = Vec::new();
    for (i, (p, ns)) in w.names.iter().enumerate() {
        let standard = STANDARD_PREFIXES.contains(&(p.as_str(), ns.as_str()));
        if w.used.contains(&i) && !standard {
            lines.push(format!("PREFIX {p}: <{ns}>"));
        }
    }
    lines.sort();
    if !lines.is_empty() {
        out.push_str(&lines.join("\n"));
        out.push_str("\n\n");
    }
    out.push_str(&body);
    Ok(out)
}

type Subject = NamedOrBlankNode;

struct Writer<'g> {
    /// outgoing (predicate, object) pairs per subject, in a stable order
    out_edges: FxHashMap<Subject, Vec<(NamedNode, Term)>>,
    /// how many triples have each blank node as their object
    incoming: FxHashMap<BlankNode, usize>,
    claimed: FxHashSet<Triple>,
    /// blank nodes already written (a second visit would duplicate them)
    visited: FxHashSet<BlankNode>,
    /// (prefix, namespace): the standard prefixes unless rebound, then the given ones
    names: Vec<(String, String)>,
    used: FxHashSet<usize>,
    ontology: Option<(String, Vec<String>)>,
    _g: std::marker::PhantomData<&'g Graph>,
}

/// A part of a property shape line.
enum Atom {
    Text(String),
    /// a nested node shape body
    Body(String),
}

impl<'g> Writer<'g> {
    fn new(graph: &'g Graph, prefixes: &[(String, String)]) -> Writer<'g> {
        let mut out_edges: FxHashMap<Subject, Vec<(NamedNode, Term)>> = FxHashMap::default();
        let mut incoming: FxHashMap<BlankNode, usize> = FxHashMap::default();
        for t in graph.iter() {
            let t = t.into_owned();
            if let Term::BlankNode(b) = &t.object {
                *incoming.entry(b.clone()).or_default() += 1;
            }
            out_edges
                .entry(t.subject)
                .or_default()
                .push((t.predicate, t.object));
        }
        for v in out_edges.values_mut() {
            v.sort_by_cached_key(|(p, o)| (p.as_str().to_string(), o.to_string()));
        }
        let mut names: Vec<(String, String)> = Vec::new();
        for (p, ns) in prefixes {
            if valid_prefix(p) && !names.iter().any(|(q, _)| q == p) {
                names.push((p.clone(), ns.clone()));
            }
        }
        for (p, ns) in STANDARD_PREFIXES {
            if !names.iter().any(|(q, _)| q == p) {
                names.push((p.to_string(), ns.to_string()));
            }
        }
        Writer {
            out_edges,
            incoming,
            claimed: FxHashSet::default(),
            visited: FxHashSet::default(),
            names,
            used: FxHashSet::default(),
            ontology: None,
            _g: std::marker::PhantomData,
        }
    }

    fn edges(&self, s: &Subject) -> Vec<(NamedNode, Term)> {
        self.out_edges.get(s).cloned().unwrap_or_default()
    }

    fn claim(&mut self, s: &Subject, p: &NamedNode, o: &Term) {
        self.claimed
            .insert(Triple::new(s.clone(), p.clone(), o.clone()));
    }

    fn is_claimed(&self, s: &Subject, p: &NamedNode, o: &Term) -> bool {
        self.claimed
            .contains(&Triple::new(s.clone(), p.clone(), o.clone()))
    }

    /// A blank node that only this one triple refers to, and that was not written yet.
    fn private(&self, o: &Term) -> Option<BlankNode> {
        match o {
            Term::BlankNode(b) if self.incoming.get(b) == Some(&1) && !self.visited.contains(b) => {
                Some(b.clone())
            }
            _ => None,
        }
    }

    // ------------------------------------------------------------------- terms ----

    fn iri(&mut self, iri: &str) -> String {
        let mut best: Option<(usize, usize)> = None;
        for (i, (_, ns)) in self.names.iter().enumerate() {
            if let Some(local) = iri.strip_prefix(ns.as_str())
                && simple_local(local)
                && best.is_none_or(|(_, len)| ns.len() > len)
            {
                best = Some((i, ns.len()));
            }
        }
        match best {
            Some((i, len)) => {
                self.used.insert(i);
                format!("{}:{}", self.names[i].0, &iri[len..])
            }
            None => format!("<{iri}>"),
        }
    }

    fn literal(&mut self, l: &Literal) -> String {
        let v = l.value();
        let dt = l.datatype();
        let bare = (dt == xsd::BOOLEAN && (v == "true" || v == "false"))
            || (dt == xsd::INTEGER && is_integer(v))
            || (dt == xsd::DECIMAL && is_decimal(v))
            || (dt == xsd::DOUBLE && is_double(v));
        if bare {
            return v.to_string();
        }
        let s = quote(v);
        if let Some(lang) = l.language() {
            format!("{s}@{lang}")
        } else if dt == xsd::STRING {
            s
        } else {
            format!("{s}^^{}", self.iri(dt.as_str()))
        }
    }

    /// An `iriOrLiteral`.
    fn simple(&mut self, t: &Term) -> Option<String> {
        match t {
            Term::NamedNode(n) => Some(self.iri(n.as_str())),
            Term::Literal(l) => Some(self.literal(l)),
            _ => None,
        }
    }

    /// The members of a list whose cells are private blank nodes with exactly
    /// `rdf:first` and `rdf:rest`; `None` when it is not one. Claims nothing.
    fn list(&self, head: &Term) -> Option<Vec<(Subject, Term, Term)>> {
        let mut cells = Vec::new();
        let mut cur = head.clone();
        let mut seen = FxHashSet::default();
        loop {
            if let Term::NamedNode(n) = &cur
                && n.as_ref() == rdf::NIL
            {
                return Some(cells);
            }
            let b = self.private(&cur)?;
            if !seen.insert(b.clone()) {
                return None;
            }
            let s: Subject = b.into();
            let edges = self.edges(&s);
            let [(p1, first), (p2, rest)] = &edges[..] else {
                return None;
            };
            if p1.as_ref() != rdf::FIRST || p2.as_ref() != rdf::REST {
                return None;
            }
            cells.push((s, first.clone(), rest.clone()));
            cur = rest.clone();
        }
    }

    fn claim_list(&mut self, cells: &[(Subject, Term, Term)]) {
        for (s, first, rest) in cells {
            if let NamedOrBlankNode::BlankNode(b) = s {
                self.visited.insert(b.clone());
            }
            self.claim(s, &rdf::FIRST.into_owned(), first);
            self.claim(s, &rdf::REST.into_owned(), rest);
        }
    }

    /// An `iriOrLiteralOrArray`; claims the cells of an array.
    fn value(&mut self, t: &Term) -> Option<String> {
        if let Some(s) = match t {
            Term::NamedNode(n) if n.as_ref() == rdf::NIL => Some("[]".to_string()),
            Term::NamedNode(_) | Term::Literal(_) => self.simple(t),
            _ => None,
        } {
            return Some(s);
        }
        let cells = self.list(t)?;
        if cells
            .iter()
            .any(|(_, f, _)| !matches!(f, Term::NamedNode(_) | Term::Literal(_)))
        {
            return None;
        }
        self.claim_list(&cells);
        let items: Vec<String> = cells
            .iter()
            .map(|(_, f, _)| self.simple(f).expect("checked"))
            .collect();
        Some(format!("[{}]", items.join(" ")))
    }

    // --------------------------------------------------------------- document ----

    fn document(&mut self) -> String {
        self.find_ontology();
        let mut shapes: Vec<NamedNode> = self
            .out_edges
            .iter()
            .filter_map(|(s, edges)| match s {
                NamedOrBlankNode::NamedNode(n)
                    if edges
                        .iter()
                        .any(|(p, o)| p.as_ref() == rdf::TYPE && is(o, sh::NODE_SHAPE)) =>
                {
                    Some(n.clone())
                }
                _ => None,
            })
            .collect();
        shapes.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        let mut out = String::new();
        for (i, s) in shapes.iter().enumerate() {
            if i > 0 {
                out.push('\n');
            }
            out.push_str(&self.top_shape(s));
        }
        out
    }

    /// `BASE` and `IMPORTS` from the one `owl:Ontology` whose only other triples are
    /// `owl:imports` of IRIs.
    fn find_ontology(&mut self) {
        let onts: Vec<NamedNode> = self
            .out_edges
            .iter()
            .filter(|(_, edges)| {
                edges
                    .iter()
                    .any(|(p, o)| p.as_ref() == rdf::TYPE && is(o, owl::ONTOLOGY))
            })
            .filter_map(|(s, _)| match s {
                NamedOrBlankNode::NamedNode(n) => Some(n.clone()),
                _ => None,
            })
            .collect();
        let [ont] = &onts[..] else { return };
        let s: Subject = ont.clone().into();
        let edges = self.edges(&s);
        let mut imports = Vec::new();
        for (p, o) in &edges {
            match o {
                Term::NamedNode(n) if p.as_ref() == owl::IMPORTS => imports.push(n.clone()),
                o if p.as_ref() == rdf::TYPE && is(o, owl::ONTOLOGY) => {}
                _ => return,
            }
        }
        for (p, o) in &edges {
            self.claim(&s, p, o);
        }
        imports.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        self.ontology = Some((
            ont.as_str().to_string(),
            imports.into_iter().map(NamedNode::into_string).collect(),
        ));
    }

    fn top_shape(&mut self, shape: &NamedNode) -> String {
        let s: Subject = shape.clone().into();
        let edges = self.edges(&s);
        let class = edges
            .iter()
            .any(|(p, o)| p.as_ref() == rdf::TYPE && is(o, rdfs::CLASS));
        let mut head = if class {
            format!("shapeClass {}", self.iri(shape.as_str()))
        } else {
            format!("shape {}", self.iri(shape.as_str()))
        };
        for (p, o) in &edges {
            if p.as_ref() == rdf::TYPE && (is(o, sh::NODE_SHAPE) || (class && is(o, rdfs::CLASS))) {
                self.claim(&s, p, o);
            }
        }
        if !class {
            let targets: Vec<NamedNode> = edges
                .iter()
                .filter(|(p, _)| p.as_ref() == sh::TARGET_CLASS)
                .filter_map(|(_, o)| match o {
                    Term::NamedNode(n) => Some(n.clone()),
                    _ => None,
                })
                .collect();
            if !targets.is_empty() {
                head.push_str(" ->");
                for t in targets {
                    let _ = write!(head, " {}", self.iri(t.as_str()));
                    self.claim(&s, &sh::TARGET_CLASS.into_owned(), &t.into());
                }
            }
        }
        let body = self.node_body(&s, 1);
        format!("{head} {{\n{body}}}\n")
    }

    /// The constraints of a node shape body, one per line at `depth`.
    fn node_body(&mut self, s: &Subject, depth: usize) -> String {
        let pad = INDENT.repeat(depth);
        let edges = self.edges(s);
        let mut params: Vec<(usize, String)> = Vec::new();
        let mut refs: Vec<String> = Vec::new();
        let mut props: Vec<(String, String)> = Vec::new();
        for (p, o) in &edges {
            if self.is_claimed(s, p, o) {
                continue;
            }
            if p.as_ref() == sh::PROPERTY {
                if let Some(b) = self.private(o)
                    && let Some((path, line)) = self.property_shape(&b, depth)
                {
                    self.claim(s, p, o);
                    props.push((path, line));
                }
            } else if p.as_ref() == sh::NODE && matches!(o, Term::NamedNode(_)) {
                let Term::NamedNode(n) = o else {
                    unreachable!()
                };
                refs.push(format!("@{}", self.iri(n.as_str())));
                self.claim(s, p, o);
            } else if p.as_ref() == sh::NOT {
                if let Some(text) = self.node_not(o) {
                    self.claim(s, p, o);
                    params.push((NODE_PARAMS.len(), text));
                }
            } else if p.as_ref() == sh::OR {
                if let Some(text) = self.node_or(o) {
                    self.claim(s, p, o);
                    params.push((NODE_PARAMS.len() + 1, text));
                }
            } else if let Some(name) = sh_name(p)
                && let Some(rank) = NODE_PARAMS.iter().position(|n| *n == name)
                && let Some(v) = self.value(o)
            {
                self.claim(s, p, o);
                params.push((rank, format!("{name}={v}")));
            }
        }
        params.sort();
        refs.sort();
        props.sort_by(|a, b| a.0.cmp(&b.0));
        let mut out = String::new();
        for (_, text) in params {
            let _ = writeln!(out, "{pad}{text} .");
        }
        for r in refs {
            let _ = writeln!(out, "{pad}{r} .");
        }
        for (_, line) in props {
            let _ = writeln!(out, "{pad}{line} .");
        }
        out
    }

    /// One `param=value` of a private blank node with that one triple (a `nodeNot`
    /// operand or a `nodeOr` member); claims it.
    fn single_param(&mut self, o: &Term, params: &[&str]) -> Option<String> {
        let b = self.private(o)?;
        let s: Subject = b.clone().into();
        let edges = self.edges(&s);
        let [(p, v)] = &edges[..] else { return None };
        let name = sh_name(p).filter(|n| params.contains(n))?;
        let text = self.value(v)?;
        self.claim(&s, p, v);
        self.visited.insert(b);
        Some(format!("{name}={text}"))
    }

    fn node_not(&mut self, o: &Term) -> Option<String> {
        Some(format!("!{}", self.single_param(o, &NODE_PARAMS)?))
    }

    /// `a=1|!b=2`: an `sh:or` list of private blank nodes with one parameter each (or
    /// `sh:not` of one).
    fn node_or(&mut self, o: &Term) -> Option<String> {
        self.or_members(o, &|w, m| {
            if let Some(t) = w.single_param(m, &NODE_PARAMS) {
                return Some(t);
            }
            let b = w.private(m)?;
            let s: Subject = b.clone().into();
            let edges = w.edges(&s);
            let [(p, n)] = &edges[..] else { return None };
            if p.as_ref() != sh::NOT {
                return None;
            }
            let t = w.node_not(n)?;
            w.claim(&s, p, n);
            w.visited.insert(b);
            Some(t)
        })
    }

    /// The members of an `sh:or` list of two or more, each written by `member`; claims
    /// the list when every member can be written. A member that cannot leaves its own
    /// triples unclaimed, so the write fails anyway.
    fn or_members(
        &mut self,
        o: &Term,
        member: &dyn Fn(&mut Writer<'g>, &Term) -> Option<String>,
    ) -> Option<String> {
        let cells = self.list(o)?;
        if cells.len() < 2 {
            return None;
        }
        let mut parts = Vec::new();
        for (_, m, _) in &cells {
            parts.push(member(self, m)?);
        }
        self.claim_list(&cells);
        Some(parts.join("|"))
    }

    // --------------------------------------------------------- property shapes ----

    /// A property shape line (`path atoms…`, without the final dot) and its path text,
    /// for sorting. `None` when the blank node has no single, expressible path.
    fn property_shape(&mut self, b: &BlankNode, depth: usize) -> Option<(String, String)> {
        let s: Subject = b.clone().into();
        let edges = self.edges(&s);
        let paths: Vec<&Term> = edges
            .iter()
            .filter(|(p, _)| p.as_ref() == sh::PATH)
            .map(|(_, o)| o)
            .collect();
        let [path_node] = &paths[..] else { return None };
        let path_node = (*path_node).clone();
        let mut claims = Vec::new();
        let path = self.path(&path_node, 0, &mut claims)?;
        self.visited.insert(b.clone());
        for (cs, cp, co) in claims {
            self.claim(&cs, &cp, &co);
        }
        self.claim(&s, &sh::PATH.into_owned(), &path_node);
        let mut atoms: Vec<(usize, Atom)> = Vec::new();
        // the count
        let count = |name| {
            let vals: Vec<&Term> = edges
                .iter()
                .filter(|(p, _)| sh_name(p) == Some(name))
                .map(|(_, o)| o)
                .collect();
            match &vals[..] {
                [Term::Literal(l)] if l.datatype() == xsd::INTEGER && is_integer(l.value()) => {
                    Some(Some(l.clone()))
                }
                [] => Some(None),
                _ => None,
            }
        };
        if let (Some(min), Some(max)) = (count("minCount"), count("maxCount")) {
            let min_ok = min
                .as_ref()
                .is_none_or(|l| l.value().parse::<i64>() != Ok(0));
            if min_ok && (min.is_some() || max.is_some()) {
                let min_text = min.as_ref().map_or("0".into(), |l| l.value().to_string());
                let max_text = max.as_ref().map_or("*".into(), |l| l.value().to_string());
                for (p, l) in [(sh::MIN_COUNT, &min), (sh::MAX_COUNT, &max)] {
                    if let Some(l) = l {
                        self.claim(&s, &p.into_owned(), &l.clone().into());
                    }
                }
                atoms.push((2, Atom::Text(format!("[{min_text}..{max_text}]"))));
            }
        }
        for (p, o) in &edges {
            if self.is_claimed(&s, p, o) {
                continue;
            }
            if let Some((rank, atom)) = self.atom(p, o, depth) {
                self.claim(&s, p, o);
                atoms.push((rank, atom));
            }
        }
        atoms.sort_by_key(|(rank, _)| *rank);
        let mut line = path.clone();
        let pad = INDENT.repeat(depth);
        for (_, a) in atoms {
            match a {
                Atom::Text(t) => {
                    line.push(' ');
                    line.push_str(&t);
                }
                Atom::Body(body) => {
                    let _ = write!(line, " {{\n{body}{pad}}}");
                }
            }
        }
        Some((path, line))
    }

    /// One property atom for a triple `(_, p, o)` of a property shape (or of an
    /// `sh:or` member or `sh:not` operand in one), with its rank in the line: the node
    /// kind, the type, the count, shape references, parameters, `!` and `|`, and nested
    /// bodies last, as in the Note's examples.
    fn atom(&mut self, p: &NamedNode, o: &Term, depth: usize) -> Option<(usize, Atom)> {
        let name = sh_name(p)?;
        match (name, o) {
            ("nodeKind", Term::NamedNode(k)) => {
                if let Some(kind) = k
                    .as_str()
                    .strip_prefix(SH_NS)
                    .filter(|k| NODE_KINDS.contains(k))
                {
                    return Some((0, Atom::Text(kind.to_string())));
                }
            }
            ("datatype", Term::NamedNode(d)) if is_datatype_iri(d.as_str()) => {
                return Some((1, Atom::Text(self.iri(d.as_str()))));
            }
            ("class", Term::NamedNode(c)) if !is_datatype_iri(c.as_str()) => {
                return Some((1, Atom::Text(self.iri(c.as_str()))));
            }
            ("node", Term::NamedNode(n)) => {
                return Some((3, Atom::Text(format!("@{}", self.iri(n.as_str())))));
            }
            ("node", Term::BlankNode(_)) => {
                let b = self.private(o)?;
                self.visited.insert(b.clone());
                let body = self.node_body(&b.into(), depth + 1);
                return Some((PROPERTY_PARAMS.len() + 10, Atom::Body(body)));
            }
            ("not", _) => {
                let b = self.private(o)?;
                let s: Subject = b.clone().into();
                let edges = self.edges(&s);
                let [(ip, io)] = &edges[..] else { return None };
                self.visited.insert(b);
                let Some((_, Atom::Text(t))) = self.atom(ip, io, depth) else {
                    return None;
                };
                self.claim(&s, ip, io);
                return Some((PROPERTY_PARAMS.len() + 4, Atom::Text(format!("!{t}"))));
            }
            ("or", _) => {
                let text = self.or_members(o, &|w, m| {
                    let b = w.private(m)?;
                    let s: Subject = b.clone().into();
                    let edges = w.edges(&s);
                    let [(ip, io)] = &edges[..] else { return None };
                    w.visited.insert(b);
                    let Some((_, Atom::Text(t))) = w.atom(ip, io, depth) else {
                        return None;
                    };
                    w.claim(&s, ip, io);
                    Some(t)
                })?;
                return Some((PROPERTY_PARAMS.len() + 5, Atom::Text(text)));
            }
            _ => {}
        }
        let rank = PROPERTY_PARAMS.iter().position(|n| *n == name)?;
        let v = self.value(o)?;
        Some((4 + rank, Atom::Text(format!("{name}={v}"))))
    }

    // ------------------------------------------------------------------- paths ----

    /// A path at precedence `level` (0: anywhere, 1: a member of an alternative,
    /// 2: a member of a sequence, 3: the operand of `^` or a modifier). The triples it
    /// needs are pushed to `claims` (claimed by the caller once the whole path fits).
    fn path(
        &mut self,
        t: &Term,
        level: u8,
        claims: &mut Vec<(Subject, NamedNode, Term)>,
    ) -> Option<String> {
        if let Term::NamedNode(n) = t {
            if n.as_ref() == rdf::NIL {
                return None;
            }
            return Some(self.iri(n.as_str()));
        }
        // a sequence: a list of two or more
        if let Some(cells) = self.list(t) {
            if cells.len() < 2 {
                return None;
            }
            let mut parts = Vec::new();
            for (_, m, _) in &cells {
                parts.push(self.path(m, 2, claims)?);
            }
            for (s, f, r) in &cells {
                claims.push((s.clone(), rdf::FIRST.into_owned(), f.clone()));
                claims.push((s.clone(), rdf::REST.into_owned(), r.clone()));
            }
            let text = parts.join("/");
            return Some(if level >= 2 {
                format!("({text})")
            } else {
                text
            });
        }
        let b = self.private(t)?;
        let s: Subject = b.into();
        let edges = self.edges(&s);
        let [(p, o)] = &edges[..] else { return None };
        let name = sh_name(p)?;
        let text = match name {
            "alternativePath" => {
                let cells = self.list(o)?;
                if cells.len() < 2 {
                    return None;
                }
                let mut parts = Vec::new();
                for (_, m, _) in &cells {
                    parts.push(self.path(m, 1, claims)?);
                }
                for (cs, f, r) in &cells {
                    claims.push((cs.clone(), rdf::FIRST.into_owned(), f.clone()));
                    claims.push((cs.clone(), rdf::REST.into_owned(), r.clone()));
                }
                let text = parts.join("|");
                if level >= 1 {
                    format!("({text})")
                } else {
                    text
                }
            }
            // the operand of `^` or a modifier is written at level 3, so it is an IRI or
            // in parentheses
            "inversePath" => {
                let text = format!("^{}", self.path(o, 3, claims)?);
                if level >= 3 {
                    format!("({text})")
                } else {
                    text
                }
            }
            "zeroOrMorePath" | "oneOrMorePath" | "zeroOrOnePath" => {
                let m = match name {
                    "zeroOrMorePath" => "*",
                    "oneOrMorePath" => "+",
                    _ => "?",
                };
                let text = format!("{}{m}", self.path(o, 3, claims)?);
                if level >= 3 {
                    format!("({text})")
                } else {
                    text
                }
            }
            _ => return None,
        };
        claims.push((s, p.clone(), o.clone()));
        Some(text)
    }
}

fn is(t: &Term, n: oxrdf::NamedNodeRef<'_>) -> bool {
    matches!(t, Term::NamedNode(x) if x.as_ref() == n)
}

/// The local name of an `sh:` IRI.
fn sh_name(p: &NamedNode) -> Option<&str> {
    p.as_str().strip_prefix(SH_NS)
}

fn valid_prefix(p: &str) -> bool {
    p.is_empty()
        || (p.starts_with(|c: char| c.is_ascii_alphabetic())
            && !p.ends_with('.')
            && p.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.'))
}

/// A local name that needs no escapes (a conservative subset of `PN_LOCAL`).
fn simple_local(l: &str) -> bool {
    !l.is_empty()
        && l.starts_with(|c: char| c.is_ascii_alphanumeric() || c == '_')
        && l.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

fn is_integer(v: &str) -> bool {
    let d = v.strip_prefix(['+', '-']).unwrap_or(v);
    !d.is_empty() && d.chars().all(|c| c.is_ascii_digit())
}

fn is_decimal(v: &str) -> bool {
    let d = v.strip_prefix(['+', '-']).unwrap_or(v);
    match d.split_once('.') {
        Some((a, b)) => {
            a.chars().all(|c| c.is_ascii_digit())
                && !b.is_empty()
                && b.chars().all(|c| c.is_ascii_digit())
        }
        None => false,
    }
}

fn is_double(v: &str) -> bool {
    let d = v.strip_prefix(['+', '-']).unwrap_or(v);
    let Some((mantissa, exp)) = d.split_once(['e', 'E']) else {
        return false;
    };
    let exp = exp.strip_prefix(['+', '-']).unwrap_or(exp);
    if exp.is_empty() || !exp.chars().all(|c| c.is_ascii_digit()) {
        return false;
    }
    match mantissa.split_once('.') {
        Some((a, b)) => {
            (!a.is_empty() || !b.is_empty())
                && a.chars().all(|c| c.is_ascii_digit())
                && b.chars().all(|c| c.is_ascii_digit())
        }
        None => !mantissa.is_empty() && mantissa.chars().all(|c| c.is_ascii_digit()),
    }
}

/// A string literal in double quotes, with Turtle's escapes.
fn quote(v: &str) -> String {
    let mut s = String::with_capacity(v.len() + 2);
    s.push('"');
    for c in v.chars() {
        match c {
            '"' => s.push_str("\\\""),
            '\\' => s.push_str("\\\\"),
            '\n' => s.push_str("\\n"),
            '\r' => s.push_str("\\r"),
            '\t' => s.push_str("\\t"),
            c if (c as u32) < 0x20 || c == '\u{7f}' => {
                let _ = write!(s, "\\u{:04X}", c as u32);
            }
            c => s.push(c),
        }
    }
    s.push('"');
    s
}

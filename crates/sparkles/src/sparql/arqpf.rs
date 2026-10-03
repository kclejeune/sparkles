//! Jena ARQ's property function library: `list:member`, `list:index` and `list:length`
//! over RDF collections, `rdfs:member`, `apf:container`, `apf:bag`, `apf:seq` and
//! `apf:alt` over RDF containers, and `apf:strSplit`, `apf:concat`, `apf:str`,
//! `apf:splitIRI` (also `apf:splitURI`), `apf:assign`, `apf:bnode` (also
//! `apf:blankNode`) and `apf:versionARQ`.
//!
//! # Containers
//!
//! A container is a resource typed `rdf:Bag`, `rdf:Seq` or `rdf:Alt`, and its members are
//! the objects of its `rdf:_1`, `rdf:_2`, … triples. `apf:container` gives the members of
//! any container, and `apf:bag`, `apf:seq` and `apf:alt` those of one type, in the order
//! of their numbers. ARQ registers `rdfs:member` as the same function as `apf:container`,
//! plus the stored `rdfs:member` triples. Sparkles does the same, but only while the
//! store holds a container: without one the two readings give the same solutions, so
//! `rdfs:member` stays an ordinary triple pattern there and keeps its plans
//! ([`container_members`]).
//!
//! # Plans
//!
//! A call is a [`Kind::PropertyFn`] operator. ARQ evaluates a property function for each
//! solution of the patterns written before it, with their values substituted, so a call
//! reads a variable from the rest of its group only when a pattern before it binds it:
//! `?x :p ?list . ?list list:member ?m` walks the lists `?list` is bound to, while
//! `?list list:member 4 . ?x :p ?list` finds the heads of the lists that hold 4 and joins
//! them with `?x :p ?list`. A call that reads no variable is a leaf of the join order. A
//! call that reads some is attached to the rest of its group: it evaluates once per
//! distinct value of the variables it reads, and each solution must agree with the input
//! row on every variable the row binds.
//!
//! In an OPTIONAL whose calls read variables of the left side, the planner evaluates the
//! right side per left row, as ARQ substitutes them ([`super::lateral`]).

use super::ctx::Ctx;
use super::plan::{ActiveGraph, GraphFilter, Kind, Node, PathEnd, Planner};
use super::table::{Table, VarId};
use super::value::Value;
use crate::error::{Error, Result};
use crate::id::Id;
use crate::index::{G, Perm, pad};
use crate::store::Chunk;
use oxrdf::vocab::{rdf, xsd};
use oxrdf::{NamedNode, Term};
use rustc_hash::{FxHashMap, FxHashSet};
use spargebra::algebra::GraphPattern;
use spargebra::term::{NamedNodePattern, TermPattern, TriplePattern};

/// Jena's list property functions.
pub const LIST: &str = "http://jena.apache.org/ARQ/list#";
/// The list functions' namespace before Jena moved to Apache.
pub const LIST_OLD: &str = "http://jena.hpl.hp.com/ARQ/list#";
/// ARQ's property function library.
pub const APF: &str = "http://jena.apache.org/ARQ/property#";
/// The library's namespace before Jena moved to Apache.
pub const APF_OLD: &str = "http://jena.hpl.hp.com/ARQ/property#";

/// The IRI that `apf:versionARQ` gives its subject.
pub const VERSION_SUBJECT: &str = "urn:x-sparkles:";

/// A property function of the library.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PfKind {
    Member,
    Index,
    Length,
    StrSplit,
    Concat,
    Str,
    SplitIri,
    Assign,
    BNode,
    Version,
    /// the members of a container of this type (any type: `None`)
    Container(Option<ContainerType>),
    /// `rdfs:member`: the stored triples, and the members of every container
    RdfsMember,
}

/// The type of an RDF container.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ContainerType {
    Bag,
    Seq,
    Alt,
}

impl ContainerType {
    const ALL: [ContainerType; 3] = [ContainerType::Bag, ContainerType::Seq, ContainerType::Alt];

    fn iri(self) -> NamedNode {
        let local = match self {
            ContainerType::Bag => "Bag",
            ContainerType::Seq => "Seq",
            ContainerType::Alt => "Alt",
        };
        NamedNode::new_unchecked(format!("{RDF_NS}{local}"))
    }
}

const RDF_NS: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
/// `rdfs:member`
pub const RDFS_MEMBER: &str = "http://www.w3.org/2000/01/rdf-schema#member";
/// The predicate that stands for `rdfs:member` once it is read as ARQ's property
/// function ([`container_members`]).
const RDFS_MEMBER_CALL: &str = "urn:x-sparkles:arq#rdfsMember";

/// The local names of the list functions.
pub const LIST_FUNCTIONS: [&str; 3] = ["member", "index", "length"];
/// The local names of the `apf:` functions. ARQ finds them by class name, so the list
/// functions are there too.
pub const APF_FUNCTIONS: [&str; 19] = [
    "strSplit",
    "concat",
    "str",
    "splitIRI",
    "splitURI",
    "assign",
    "bnode",
    "blankNode",
    "versionARQ",
    "listMember",
    "listIndex",
    "listLength",
    "member",
    "index",
    "length",
    "container",
    "bag",
    "seq",
    "alt",
];

impl PfKind {
    /// The property function `iri` names, if it is one of the library.
    pub fn of(iri: &str) -> Option<PfKind> {
        use PfKind::*;
        if iri == RDFS_MEMBER_CALL {
            return Some(RdfsMember);
        }
        if let Some(l) = iri
            .strip_prefix(LIST)
            .or_else(|| iri.strip_prefix(LIST_OLD))
        {
            return match l {
                "member" => Some(Member),
                "index" => Some(Index),
                "length" => Some(Length),
                _ => None,
            };
        }
        let l = iri
            .strip_prefix(APF)
            .or_else(|| iri.strip_prefix(APF_OLD))?;
        Some(match l {
            "strSplit" => StrSplit,
            "concat" => Concat,
            "str" => Str,
            "splitIRI" | "splitURI" => SplitIri,
            "assign" => Assign,
            "bnode" | "blankNode" => BNode,
            "versionARQ" => Version,
            "listMember" | "member" => Member,
            "listIndex" | "index" => Index,
            "listLength" | "length" => Length,
            "container" => Container(None),
            "bag" => Container(Some(ContainerType::Bag)),
            "seq" => Container(Some(ContainerType::Seq)),
            "alt" => Container(Some(ContainerType::Alt)),
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            PfKind::Member => "list:member",
            PfKind::Index => "list:index",
            PfKind::Length => "list:length",
            PfKind::StrSplit => "apf:strSplit",
            PfKind::Concat => "apf:concat",
            PfKind::Str => "apf:str",
            PfKind::SplitIri => "apf:splitIRI",
            PfKind::Assign => "apf:assign",
            PfKind::BNode => "apf:bnode",
            PfKind::Version => "apf:versionARQ",
            PfKind::Container(None) => "apf:container",
            PfKind::Container(Some(ContainerType::Bag)) => "apf:bag",
            PfKind::Container(Some(ContainerType::Seq)) => "apf:seq",
            PfKind::Container(Some(ContainerType::Alt)) => "apf:alt",
            PfKind::RdfsMember => "rdfs:member",
        }
    }

    /// The number of object arguments, when the object must be a list of that length.
    fn object_list(self) -> Option<usize> {
        match self {
            PfKind::Index | PfKind::StrSplit | PfKind::SplitIri => Some(2),
            _ => None,
        }
    }

    /// The slots the function reads: (subject, object) inputs. `apf:assign` reads
    /// whichever side is bound.
    fn reads(self) -> (bool, bool) {
        match self {
            PfKind::Member | PfKind::Index | PfKind::Length | PfKind::SplitIri => (true, false),
            PfKind::BNode => (true, false),
            PfKind::StrSplit | PfKind::Concat | PfKind::Str => (false, true),
            PfKind::Assign | PfKind::Container(_) | PfKind::RdfsMember => (true, true),
            PfKind::Version => (false, false),
        }
    }
}

/// Every property function IRI of the library, for the service description.
pub fn iris() -> Vec<String> {
    let mut out: Vec<String> = LIST_FUNCTIONS
        .iter()
        .map(|l| format!("{LIST}{l}"))
        .collect();
    out.extend(APF_FUNCTIONS.iter().map(|l| format!("{APF}{l}")));
    out.push(RDFS_MEMBER.to_string());
    out
}

/// `patterns` with `rdfs:member` read as ARQ's container property function, when they
/// use it and the store holds a container (`None` otherwise). Without a container the
/// function's solutions are those of the stored triples, so the patterns are left as
/// they are.
pub(super) fn container_members(
    ctx: &Ctx,
    patterns: &[TriplePattern],
) -> Option<Vec<TriplePattern>> {
    let is_member = |t: &TriplePattern| matches!(&t.predicate, NamedNodePattern::NamedNode(p) if p.as_str() == RDFS_MEMBER);
    if !patterns.iter().any(is_member) || !has_containers(ctx) {
        return None;
    }
    Some(
        patterns
            .iter()
            .map(|t| {
                let mut t = t.clone();
                if is_member(&t) {
                    t.predicate =
                        NamedNodePattern::NamedNode(NamedNode::new_unchecked(RDFS_MEMBER_CALL));
                }
                t
            })
            .collect(),
    )
}

/// Whether any graph of the store types a resource `rdf:Bag`, `rdf:Seq` or `rdf:Alt`.
fn has_containers(ctx: &Ctx) -> bool {
    let ty = ctx.intern_term(&Term::NamedNode(rdf::TYPE.into_owned()));
    ContainerType::ALL.iter().any(|c| {
        let t = ctx.intern_term(&Term::NamedNode(c.iri()));
        if [ty, t].iter().any(|x| x.tag() == crate::id::Tag::Local) {
            return false;
        }
        let mut found = false;
        let prefix = [ty.0, t.0];
        let _ = ctx.snap.scan_between_cols(
            Perm::Pos,
            pad(&prefix, 0),
            pad(&prefix, u64::MAX),
            crate::index::ALL_COLS,
            |c| {
                found = match c {
                    Chunk::Block(_, s, e) => e > s,
                    Chunk::Row(_) => true,
                };
                Ok(!found)
            },
        );
        found
    })
}

/// Whether `patterns` call a property function of the library.
pub fn has_calls(patterns: &[TriplePattern]) -> bool {
    patterns.iter().any(|t| call_kind(t).is_some())
}

fn call_kind(t: &TriplePattern) -> Option<PfKind> {
    match &t.predicate {
        NamedNodePattern::NamedNode(p) => PfKind::of(p.as_str()),
        NamedNodePattern::Variable(_) => None,
    }
}

/// A call taken out of a basic graph pattern.
pub struct PfCall {
    pub kind: PfKind,
    pub subject: TermPattern,
    pub object: Vec<TermPattern>,
    /// the variables and blank nodes of the patterns before the call in its basic
    /// graph pattern
    pub before: Vec<TermPattern>,
}

/// Take the calls of the library out of `patterns`, with their list arguments.
pub fn take_calls(patterns: &[TriplePattern]) -> Result<(Vec<PfCall>, Vec<TriplePattern>)> {
    if !has_calls(patterns) {
        return Ok((Vec::new(), patterns.to_vec()));
    }
    let positions: Vec<usize> = patterns
        .iter()
        .enumerate()
        .filter(|(_, t)| call_kind(t).is_some())
        .map(|(i, _)| i)
        .collect();
    let (calls, rest) = super::textpf::take_calls_where(
        patterns,
        |iri| PfKind::of(iri).is_some(),
        |iri| PfKind::of(iri).map_or_else(|| iri.to_string(), |k| k.name().to_string()),
    )?;
    let is_cell = |t: &TriplePattern| {
        matches!(t.subject, TermPattern::BlankNode(_))
            && matches!(&t.predicate, NamedNodePattern::NamedNode(p)
                if *p == rdf::FIRST || *p == rdf::REST)
    };
    let mut out = Vec::new();
    for ((iri, subjects, objects), pos) in calls.into_iter().zip(positions) {
        let kind =
            PfKind::of(iri.as_str()).ok_or_else(|| Error::invalid("not a property function"))?;
        let bad = |m: &str| Error::invalid(format!("{}: {m}", kind.name()));
        let [subject] = <[TermPattern; 1]>::try_from(subjects)
            .map_err(|_| bad("the subject must be a single term"))?;
        match kind.object_list() {
            Some(n) if objects.len() != n => {
                return Err(bad(&format!("the object must be a list of {n} elements")));
            }
            None if !matches!(kind, PfKind::Concat) && objects.len() != 1 => {
                return Err(bad("the object must be a single term"));
            }
            _ => {}
        }
        let mut before = Vec::new();
        for t in &patterns[..pos] {
            if call_kind(t).is_some() || is_cell(t) {
                continue;
            }
            before.push(t.subject.clone());
            if let NamedNodePattern::Variable(v) = &t.predicate {
                before.push(TermPattern::Variable(v.clone()));
            }
            before.push(t.object.clone());
        }
        out.push(PfCall {
            kind,
            subject,
            object: objects,
            before,
        });
    }
    Ok((out, rest))
}

/// The variables a library call reads in `gp`, by name: the input slots of the calls in
/// its basic graph patterns, at any depth outside sub-selects.
pub fn read_vars(gp: &GraphPattern, out: &mut Vec<String>) {
    use GraphPattern as GP;
    match gp {
        GP::Bgp { patterns } => {
            for t in patterns {
                let Some(kind) = call_kind(t) else {
                    continue;
                };
                let (s, o) = kind.reads();
                if s && let TermPattern::Variable(v) = &t.subject {
                    out.push(v.as_str().to_string());
                }
                if o {
                    // the object or the elements of its list
                    let mut stack = vec![t.object.clone()];
                    while let Some(x) = stack.pop() {
                        match x {
                            TermPattern::Variable(v) => out.push(v.as_str().to_string()),
                            TermPattern::BlankNode(b) => {
                                for c in patterns {
                                    if matches!(&c.subject, TermPattern::BlankNode(x) if *x == b)
                                        && matches!(&c.predicate, NamedNodePattern::NamedNode(p)
                                            if *p == rdf::FIRST || *p == rdf::REST)
                                    {
                                        stack.push(c.object.clone());
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
        GP::Join { left, right }
        | GP::Lateral { left, right }
        | GP::Union { left, right }
        | GP::Minus { left, right }
        | GP::SemiJoin { left, right }
        | GP::AntiJoin { left, right }
        | GP::LeftJoin { left, right, .. } => {
            read_vars(left, out);
            read_vars(right, out);
        }
        GP::Filter { inner, .. }
        | GP::Graph { inner, .. }
        | GP::Extend { inner, .. }
        | GP::Assign { inner, .. }
        | GP::Unfold { inner, .. }
        | GP::Distinct { inner }
        | GP::Reduced { inner }
        | GP::Slice { inner, .. }
        | GP::OrderBy { inner, .. } => read_vars(inner, out),
        _ => {}
    }
}

/// Whether a call of the library in `right` reads a variable that `left` binds: ARQ then
/// evaluates `OPTIONAL { right }` per solution of `left`.
pub(super) fn reads_left(p: &Planner<'_>, left: &GraphPattern, right: &GraphPattern) -> bool {
    let mut reads = Vec::new();
    read_vars(right, &mut reads);
    if reads.is_empty() {
        return false;
    }
    let mut found = false;
    left.on_in_scope_variable(|v| {
        found |=
            reads.iter().any(|r| r == v.as_str()) && !p.subst.contains_key(&p.ctx.var(v.as_str()));
    });
    found
}

// --------------------------------------------------------------------- planning ----

/// A call planned as an operator.
#[derive(Clone, Debug)]
pub struct PfSpec {
    pub kind: PfKind,
    /// the subject slot, then the object slots
    pub slots: Vec<PathEnd>,
    pub graph: GraphFilter,
    /// the variables of the slots that patterns before the call bind: read from the
    /// input rows
    pub inputs: Vec<VarId>,
}

impl PfSpec {
    /// Whether the call reads the rest of its group (child 0).
    pub fn needs_input(&self) -> bool {
        !self.inputs.is_empty()
    }
}

/// Plan a call: a leaf, or an operator to attach to the rest of its group when patterns
/// before it bind a variable it reads (`bound` holds the variables bound before it).
pub(super) fn leaf(
    p: &Planner<'_>,
    c: PfCall,
    g: &ActiveGraph,
    bound: &FxHashSet<VarId>,
) -> Result<Node> {
    let slot = |t: &TermPattern| match p.term_pattern(t) {
        super::plan::PT::C(id) => PathEnd::Const(id),
        super::plan::PT::V(v) => PathEnd::Var(v),
    };
    let mut slots = vec![slot(&c.subject)];
    slots.extend(c.object.iter().map(slot));
    let mut before: FxHashSet<VarId> = bound.clone();
    for t in &c.before {
        if let super::plan::PT::V(v) = p.term_pattern(t) {
            before.insert(v);
        }
    }
    let (rs, ro) = c.kind.reads();
    let mut inputs = Vec::new();
    for (i, s) in slots.iter().enumerate() {
        let reads = if i == 0 { rs } else { ro };
        if let PathEnd::Var(v) = s
            && reads
            && before.contains(v)
            && !inputs.contains(v)
        {
            inputs.push(*v);
        }
    }
    let mut vars: Vec<VarId> = Vec::new();
    for s in &slots {
        if let PathEnd::Var(v) = s
            && !vars.contains(v)
        {
            vars.push(*v);
        }
    }
    let desc = format!(
        "{} {}",
        c.kind.name(),
        slots
            .iter()
            .map(|s| match s {
                PathEnd::Var(v) => format!("?{}", p.ctx.var_name(*v)),
                PathEnd::Const(id) => p
                    .ctx
                    .term(*id)
                    .map_or_else(|| "UNDEF".into(), |t| t.to_string()),
            })
            .collect::<Vec<_>>()
            .join(" ")
    );
    let Some((graph, _)) = p.graph_filter(g) else {
        return Ok(Node::empty(vars));
    };
    let est = match c.kind {
        PfKind::Member | PfKind::Index | PfKind::Length if inputs.is_empty() => {
            let first = p.ctx.intern_term(&Term::NamedNode(rdf::FIRST.into_owned()));
            (p.ctx.snap.estimate(Perm::Pso, &[first.0]) as f64).max(1.0)
        }
        PfKind::StrSplit => 8.0,
        PfKind::Container(_) | PfKind::RdfsMember if inputs.is_empty() => 100.0,
        _ => 1.0,
    };
    let mut n = Node::leaf(
        Kind::PropertyFn(Box::new(PfSpec {
            kind: c.kind,
            slots,
            graph,
            inputs,
        })),
        vars,
        est,
        desc,
    );
    // a variable a call cannot bind (a null member, a failed split) is still bound in
    // every solution it gives
    n.cost = est * 4.0;
    Ok(n)
}

/// Attach a call that reads the rest of its group to that group's plan `left`.
pub(super) fn attach(left: Node, call: Node) -> Node {
    let mut vars = left.vars.clone();
    for v in &call.vars {
        if !vars.contains(v) {
            vars.push(*v);
        }
    }
    let mut certain = left.certain.clone();
    certain.extend(call.vars.iter().copied().filter(|v| !left.vars.contains(v)));
    let est = (left.est * 4.0).max(1.0);
    Node {
        dist: vars.iter().map(|&v| (v, est)).collect(),
        cost: left.cost + est * 8.0,
        vars,
        certain,
        sorted: Vec::new(),
        est,
        desc: call.desc,
        kind: call.kind,
        children: vec![left],
    }
}

// -------------------------------------------------------------------- execution ----

/// Run a call over its input rows (`input`, or one empty row for a leaf): each row's
/// values of the slots' variables are substituted, the call is solved once per distinct
/// substitution, and every solution that agrees with the row extends it.
pub(super) fn run(
    ctx: &Ctx,
    spec: &PfSpec,
    input: Option<&Table>,
    vars: &[VarId],
) -> Result<Table> {
    let unit = Table::unit();
    let input = input.unwrap_or(&unit);
    let lists = Lists::new(ctx, &spec.graph);
    let slot_cols: Vec<Option<usize>> = spec
        .slots
        .iter()
        .map(|s| match s {
            PathEnd::Var(v) if spec.inputs.contains(v) => input.col_of(*v),
            _ => None,
        })
        .collect();
    let var_cols: Vec<Option<usize>> = spec
        .slots
        .iter()
        .map(|s| match s {
            PathEnd::Var(v) => input.col_of(*v),
            PathEnd::Const(_) => None,
        })
        .collect();
    let mut out = Table::new(vars.to_vec());
    // where each output column comes from: an input column, or a slot
    let from: Vec<Result<usize, usize>> = vars
        .iter()
        .map(|v| match input.col_of(*v) {
            Some(c) => Ok(c),
            None => Err(spec
                .slots
                .iter()
                .position(|s| matches!(s, PathEnd::Var(x) if x == v))
                .expect("a slot variable")),
        })
        .collect();
    let mut memo: FxHashMap<Vec<Id>, Vec<Vec<Id>>> = FxHashMap::default();
    let mut row = Vec::with_capacity(vars.len());
    for i in 0..input.len() {
        if i % 1024 == 0 {
            ctx.check()?;
            ctx.check_output(out.len(), out.width())?;
        }
        let key: Vec<Id> = slot_cols
            .iter()
            .map(|c| c.map_or(Id::UNDEF, |c| input.cols[c][i]))
            .collect();
        if !memo.contains_key(&key) {
            let args: Vec<Option<Id>> = spec
                .slots
                .iter()
                .zip(&key)
                .map(|(s, k)| match s {
                    PathEnd::Const(id) => Some(*id),
                    PathEnd::Var(_) => (!k.is_undef()).then_some(*k),
                })
                .collect();
            let sols = solve(ctx, &lists, spec.kind, &args)?;
            memo.insert(key.clone(), sols);
        }
        'sol: for sol in &memo[&key] {
            // the solution agrees with the row and with itself on each variable
            for (j, s) in spec.slots.iter().enumerate() {
                let PathEnd::Var(v) = s else {
                    continue;
                };
                if let Some(c) = var_cols[j] {
                    let have = input.cols[c][i];
                    if !have.is_undef() && have != sol[j] {
                        continue 'sol;
                    }
                }
                for (k, t) in spec.slots.iter().enumerate().skip(j + 1) {
                    if matches!(t, PathEnd::Var(x) if x == v) && sol[k] != sol[j] {
                        continue 'sol;
                    }
                }
            }
            row.clear();
            for (c, f) in from.iter().enumerate() {
                row.push(match f {
                    Ok(ic) => {
                        let have = input.cols[*ic][i];
                        if have.is_undef() {
                            // a variable the row leaves unbound takes the solution's value
                            match spec
                                .slots
                                .iter()
                                .position(|s| matches!(s, PathEnd::Var(x) if *x == vars[c]))
                            {
                                Some(j) => sol[j],
                                None => have,
                            }
                        } else {
                            have
                        }
                    }
                    Err(j) => sol[*j],
                });
            }
            out.push_row(&row);
        }
    }
    Ok(out)
}

/// The values of every slot in each solution of a call whose slots hold `args` (`None`:
/// an unbound variable).
fn solve(ctx: &Ctx, lists: &Lists<'_>, kind: PfKind, args: &[Option<Id>]) -> Result<Vec<Vec<Id>>> {
    let int = |n: i64| ctx.intern_value(&Value::Integer(n.into()));
    let string = |s: &str| ctx.intern_value(&Value::Str(s.into()));
    let bad = |m: &str| Error::invalid(format!("{}: {m}", kind.name()));
    Ok(match kind {
        PfKind::Member => {
            let (s, o) = (args[0], args[1]);
            match (s, o) {
                (Some(l), None) => lists.members(l)?.into_iter().map(|m| vec![l, m]).collect(),
                (Some(l), Some(m)) => {
                    let n = lists.members(l)?.into_iter().filter(|x| *x == m).count();
                    vec![vec![l, m]; n]
                }
                (None, None) => {
                    let mut out = Vec::new();
                    for h in lists.heads()? {
                        out.extend(lists.members(h)?.into_iter().map(|m| vec![h, m]));
                    }
                    out
                }
                (None, Some(m)) => lists
                    .heads_holding(m)?
                    .into_iter()
                    .map(|h| vec![h, m])
                    .collect(),
            }
        }
        PfKind::Length => match (args[0], args[1]) {
            (Some(l), o) => match lists.length(l)? {
                Some(n) => {
                    let n = int(n as i64);
                    match o {
                        None => vec![vec![l, n]],
                        Some(x) if integer(ctx, x) == integer(ctx, n) => vec![vec![l, x]],
                        Some(_) => Vec::new(),
                    }
                }
                None => Vec::new(),
            },
            (None, None) => {
                let mut out = Vec::new();
                for h in lists.heads()? {
                    if let Some(n) = lists.length(h)? {
                        out.push(vec![h, int(n as i64)]);
                    }
                }
                out
            }
            // ARQ finds no list by its length
            (None, Some(_)) => Vec::new(),
        },
        PfKind::Index => {
            let (i, m) = (args[1], args[2]);
            let one = |l: Id| -> Result<Vec<Vec<Id>>> {
                Ok(match (i, m) {
                    (None, Some(m)) => {
                        // the first position only, as ARQ's `GraphList.index`
                        match lists.members(l)?.iter().position(|x| *x == m) {
                            Some(p) => vec![vec![l, int(p as i64), m]],
                            None => Vec::new(),
                        }
                    }
                    (Some(i), m) => match integer(ctx, i) {
                        Some(p) if p >= 0 => match lists.members(l)?.get(p as usize) {
                            Some(&x) if m.is_none_or(|m| m == x) => vec![vec![l, i, x]],
                            _ => Vec::new(),
                        },
                        _ => Vec::new(),
                    },
                    (None, None) => lists
                        .members(l)?
                        .into_iter()
                        .enumerate()
                        .map(|(p, x)| vec![l, int(p as i64), x])
                        .collect(),
                })
            };
            match args[0] {
                Some(l) => one(l)?,
                None => {
                    let heads = match m {
                        Some(m) => lists.heads_holding(m)?,
                        None => lists.heads()?,
                    };
                    let mut out = Vec::new();
                    for h in dedup(heads) {
                        out.extend(one(h)?);
                    }
                    out
                }
            }
        }
        PfKind::StrSplit => {
            let (Some(s), Some(re)) = (args[1], args[2]) else {
                return Ok(Vec::new());
            };
            let (Some(text), Some(pattern)) = (literal_lexical(ctx, s), literal_lexical(ctx, re))
            else {
                return Ok(Vec::new());
            };
            let re = regex::Regex::new(&pattern).map_err(|e| bad(&e.to_string()))?;
            let tokens = java_split(&re, &text);
            match args[0] {
                None => tokens
                    .iter()
                    .map(|t| vec![string(t), s, re_id(args[2])])
                    .collect(),
                Some(x) => match ctx.value(x) {
                    Some(Value::Str(v)) if tokens.iter().any(|t| **t == *v) => {
                        vec![vec![x, s, re_id(args[2])]]
                    }
                    _ => Vec::new(),
                },
            }
        }
        PfKind::Concat => {
            let mut text = String::new();
            for a in &args[1..] {
                let Some(a) = a else {
                    return Ok(Vec::new());
                };
                match ctx.term(*a) {
                    Some(Term::NamedNode(n)) => text.push_str(n.as_str()),
                    Some(Term::Literal(l)) => text.push_str(l.value()),
                    _ => return Ok(Vec::new()),
                }
            }
            let v = string(&text);
            match args[0] {
                None => vec![
                    std::iter::once(v)
                        .chain(args[1..].iter().flatten().copied())
                        .collect(),
                ],
                Some(x) if x == v => vec![args.iter().flatten().copied().collect()],
                Some(_) => Vec::new(),
            }
        }
        PfKind::Str => {
            if let Some(s) = args[0]
                && !matches!(ctx.term(s), Some(Term::Literal(_)))
            {
                return Ok(Vec::new());
            }
            let Some(o) = args[1] else {
                return Err(bad("the object is an unbound variable"));
            };
            let v = match ctx.term(o) {
                Some(Term::NamedNode(n)) => string(n.as_str()),
                Some(Term::Literal(l)) => string(l.value()),
                _ => return Err(bad("the object is a blank node")),
            };
            match args[0] {
                None => vec![vec![v, o]],
                Some(s) if s == v => vec![vec![s, o]],
                Some(_) => Vec::new(),
            }
        }
        PfKind::SplitIri => {
            let Some(s) = args[0] else {
                return Ok(Vec::new());
            };
            let Some(Term::NamedNode(iri)) = ctx.term(s) else {
                return Ok(Vec::new());
            };
            let at = super::fnlib::split_xml(iri.as_str());
            let (ns, local) = iri.as_str().split_at(at);
            let nsv = match args[1] {
                None => ctx.intern_term(&Term::NamedNode(NamedNode::new_unchecked(ns))),
                Some(x) => {
                    let text = match ctx.term(x) {
                        Some(Term::NamedNode(n)) => Some(n.as_str().to_string()),
                        Some(Term::Literal(_)) => string_literal(ctx, x),
                        _ => None,
                    };
                    if text.as_deref() != Some(ns) {
                        return Ok(Vec::new());
                    }
                    x
                }
            };
            let lv = match args[2] {
                None => string(local),
                Some(x) => {
                    if string_literal(ctx, x).as_deref() != Some(local) {
                        return Ok(Vec::new());
                    }
                    x
                }
            };
            vec![vec![s, nsv, lv]]
        }
        PfKind::Assign => match (args[0], args[1]) {
            (None, None) => return Err(bad("both the subject and the object are unbound")),
            (None, Some(o)) => vec![vec![o, o]],
            (Some(s), None) => vec![vec![s, s]],
            (Some(s), Some(o)) => {
                let same = s == o
                    || matches!((ctx.term(s), ctx.term(o)), (Some(a), Some(b))
                        if super::cdt::same_value(&a, &b).unwrap_or(false));
                if same { vec![vec![s, o]] } else { Vec::new() }
            }
        },
        PfKind::BNode => {
            let Some(s) = args[0] else {
                return Err(bad("the subject is an unbound variable"));
            };
            let Some(Term::BlankNode(b)) = ctx.term(s) else {
                return Ok(Vec::new());
            };
            let label = string(b.as_str());
            match args[1] {
                None => vec![vec![s, label]],
                Some(o) if o == label => vec![vec![s, o]],
                Some(_) => Vec::new(),
            }
        }
        PfKind::Container(ty) => containers(lists, ty, args[0], args[1])?,
        PfKind::RdfsMember => {
            // the stored triples, then the members of the containers, as ARQ concatenates
            let member = ctx.intern_term(&Term::NamedNode(NamedNode::new_unchecked(RDFS_MEMBER)));
            let mut out = lists.pairs(member, args[0], args[1])?;
            out.extend(containers(lists, None, args[0], args[1])?);
            out
        }
        PfKind::Version => {
            let subject =
                ctx.intern_term(&Term::NamedNode(NamedNode::new_unchecked(VERSION_SUBJECT)));
            let version = string(env!("CARGO_PKG_VERSION"));
            match (args[0], args[1]) {
                (s, o) if s.is_none_or(|s| s == subject) && o.is_none_or(|o| o == version) => {
                    vec![vec![subject, version]]
                }
                _ => Vec::new(),
            }
        }
    })
}

/// The (container, member) solutions of a container function of type `ty` (`None`: any
/// type) with subject `c` and object `m` (`None`: unbound), as ARQ's `container`
/// computes them: a bound container's members in order, or each container's.
fn containers(
    lists: &Lists<'_>,
    ty: Option<ContainerType>,
    c: Option<Id>,
    m: Option<Id>,
) -> Result<Vec<Vec<Id>>> {
    let one = |c: Id| -> Result<Vec<Vec<Id>>> {
        if !lists.is_container(c, ty)? {
            return Ok(Vec::new());
        }
        let members = lists.container_members(c)?;
        Ok(match m {
            // once per numbered triple that holds it
            Some(m) => vec![vec![c, m]; members.iter().filter(|x| x.1 == m).count()],
            None => {
                // in the order of the numbers, the last triple of a number winning
                let mut by_number: std::collections::BTreeMap<u64, Id> = Default::default();
                for (n, x) in members {
                    by_number.insert(n, x);
                }
                by_number.into_values().map(|x| vec![c, x]).collect()
            }
        })
    };
    let mut out = Vec::new();
    match c {
        Some(c) => out = one(c)?,
        None => {
            let candidates = match m {
                None => lists.containers(ty)?,
                // the subjects of the triples whose object is the member
                Some(m) => dedup(lists.subjects_of(m)?),
            };
            for c in candidates {
                out.extend(one(c)?);
            }
        }
    }
    Ok(out)
}

fn re_id(a: Option<Id>) -> Id {
    a.unwrap_or(Id::UNDEF)
}

fn dedup(mut v: Vec<Id>) -> Vec<Id> {
    let mut seen = FxHashSet::default();
    v.retain(|x| seen.insert(*x));
    v
}

/// An `xsd:integer`'s value, as ARQ's `NodeFactoryExtra.nodeToInt` reads it.
fn integer(ctx: &Ctx, id: Id) -> Option<i64> {
    match ctx.value(id)? {
        Value::Integer(i) => Some(i64::from(i)),
        _ => None,
    }
}

/// The lexical form of a literal.
fn literal_lexical(ctx: &Ctx, id: Id) -> Option<String> {
    match ctx.term(id)? {
        Term::Literal(l) => Some(l.value().to_string()),
        _ => None,
    }
}

/// The lexical form of a string literal (ARQ's `NodeUtils.stringLiteral`): a simple
/// literal, an `xsd:string` or a language-tagged string.
fn string_literal(ctx: &Ctx, id: Id) -> Option<String> {
    match ctx.term(id)? {
        Term::Literal(l) if l.datatype() == xsd::STRING || l.language().is_some() => {
            Some(l.value().to_string())
        }
        _ => None,
    }
}

/// Java's `String.split(regex)` followed by trimming each token, as ARQ's
/// `StrUtils.split`: trailing empty strings are dropped, and a leading empty string is
/// kept unless the first match is empty.
fn java_split(re: &regex::Regex, s: &str) -> Vec<String> {
    if s.is_empty() {
        return vec![String::new()];
    }
    let mut parts: Vec<&str> = Vec::new();
    let mut last = 0;
    for m in re.find_iter(s) {
        if m.end() == 0 {
            // a zero-length match at the start gives no leading empty string
            continue;
        }
        if m.start() == m.end() && m.start() >= s.len() {
            break;
        }
        parts.push(&s[last..m.start()]);
        last = m.end();
    }
    parts.push(&s[last..]);
    while parts.len() > 1 && parts.last().is_some_and(|p| p.is_empty()) {
        parts.pop();
    }
    if parts.len() == 1 && parts[0].is_empty() && !s.is_empty() {
        // every part was empty
        return Vec::new();
    }
    parts.into_iter().map(|p| p.trim().to_string()).collect()
}

/// RDF collections in the active graph, read as ARQ's `GraphList` reads them.
struct Lists<'a> {
    ctx: &'a Ctx,
    graph: &'a GraphFilter,
    first: Id,
    rest: Id,
    nil: Id,
    /// the number of each predicate that is an `rdf:_n` (`None`: another predicate)
    numbers: std::cell::RefCell<FxHashMap<Id, Option<u64>>>,
}

/// The most cells a list walk follows (a cycle stops it earlier).
const MAX_CELLS: usize = 10_000_000;

impl<'a> Lists<'a> {
    fn new(ctx: &'a Ctx, graph: &'a GraphFilter) -> Self {
        let iri = |n: oxrdf::NamedNodeRef<'_>| ctx.intern_term(&Term::NamedNode(n.into_owned()));
        Lists {
            ctx,
            graph,
            first: iri(rdf::FIRST),
            rest: iri(rdf::REST),
            nil: iri(rdf::NIL),
            numbers: Default::default(),
        }
    }

    /// The columns `cols` (of the permutation's order) of the quads with `prefix`, in
    /// the active graph, without the repeats of one triple in several graphs.
    fn scan_pairs(&self, perm: Perm, prefix: &[u64], cols: [usize; 2]) -> Result<Vec<(Id, Id)>> {
        let mut out = Vec::new();
        if prefix.iter().any(|&x| Id(x).tag() == crate::id::Tag::Local) {
            return Ok(out);
        }
        let gc = perm.col_of(G);
        let (lo, hi) = (pad(prefix, 0), pad(prefix, u64::MAX));
        self.ctx
            .snap
            .scan_between_cols(perm, lo, hi, crate::index::ALL_COLS, |c| {
                match c {
                    Chunk::Block(b, s, e) => {
                        for i in s..e {
                            if self.graph.accepts(b.cols[gc][i]) {
                                out.push((Id(b.cols[cols[0]][i]), Id(b.cols[cols[1]][i])));
                            }
                        }
                    }
                    Chunk::Row(k) => {
                        if self.graph.accepts(k[gc]) {
                            out.push((Id(k[cols[0]]), Id(k[cols[1]])));
                        }
                    }
                }
                Ok(true)
            })?;
        out.dedup();
        Ok(out)
    }

    /// The (subject, object) pairs of predicate `p`'s triples, with the subject `s` and
    /// the object `o` when they are given.
    fn pairs(&self, p: Id, s: Option<Id>, o: Option<Id>) -> Result<Vec<Vec<Id>>> {
        Ok(match (s, o) {
            (Some(s), Some(o)) => self
                .scan(Perm::Pso, &[p.0, s.0, o.0], 2)?
                .into_iter()
                .map(|o| vec![s, o])
                .collect(),
            (Some(s), None) => self
                .scan(Perm::Pso, &[p.0, s.0], 2)?
                .into_iter()
                .map(|o| vec![s, o])
                .collect(),
            (None, Some(o)) => self
                .scan(Perm::Pos, &[p.0, o.0], 2)?
                .into_iter()
                .map(|s| vec![s, o])
                .collect(),
            (None, None) => self
                .scan_pairs(Perm::Pso, &[p.0], [1, 2])?
                .into_iter()
                .map(|(s, o)| vec![s, o])
                .collect(),
        })
    }

    /// The number `n` of a predicate `rdf:_n`.
    fn number(&self, p: Id) -> Option<u64> {
        *self
            .numbers
            .borrow_mut()
            .entry(p)
            .or_insert_with(|| match self.ctx.term(p) {
                Some(Term::NamedNode(n)) => n
                    .as_str()
                    .strip_prefix(RDF_NS)
                    .and_then(|l| l.strip_prefix('_'))
                    .filter(|d| !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit()))
                    .and_then(|d| d.parse().ok()),
                _ => None,
            })
    }

    /// Whether `c` is typed as a container of type `ty` (any type: `None`).
    fn is_container(&self, c: Id, ty: Option<ContainerType>) -> Result<bool> {
        let rdf_type = self
            .ctx
            .intern_term(&Term::NamedNode(rdf::TYPE.into_owned()));
        for t in ContainerType::ALL {
            if ty.is_some_and(|x| x != t) {
                continue;
            }
            let t = self.ctx.intern_term(&Term::NamedNode(t.iri()));
            if !self.scan(Perm::Pso, &[rdf_type.0, c.0, t.0], 2)?.is_empty() {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// The numbered members of `c`: (number, member) for each `rdf:_n` triple.
    fn container_members(&self, c: Id) -> Result<Vec<(u64, Id)>> {
        Ok(self
            .scan_pairs(Perm::Spo, &[c.0], [1, 2])?
            .into_iter()
            .filter_map(|(p, o)| self.number(p).map(|n| (n, o)))
            .collect())
    }

    /// The resources typed as containers of type `ty` (any type: `None`), each once.
    fn containers(&self, ty: Option<ContainerType>) -> Result<Vec<Id>> {
        let rdf_type = self
            .ctx
            .intern_term(&Term::NamedNode(rdf::TYPE.into_owned()));
        let mut out = Vec::new();
        for t in ContainerType::ALL {
            if ty.is_some_and(|x| x != t) {
                continue;
            }
            let t = self.ctx.intern_term(&Term::NamedNode(t.iri()));
            out.extend(self.scan(Perm::Pos, &[rdf_type.0, t.0], 2)?);
        }
        Ok(dedup(out))
    }

    /// The subjects of the triples whose object is `o`.
    fn subjects_of(&self, o: Id) -> Result<Vec<Id>> {
        self.scan(Perm::Osp, &[o.0], 1)
    }

    /// The column `col` (of the permutation's order) of the quads with `prefix`, in the
    /// active graph, without the repeats of one triple in several graphs.
    fn scan(&self, perm: Perm, prefix: &[u64], col: usize) -> Result<Vec<Id>> {
        let mut out = Vec::new();
        if prefix.iter().any(|&x| Id(x).tag() == crate::id::Tag::Local) {
            return Ok(out);
        }
        let gc = perm.col_of(G);
        let (lo, hi) = (pad(prefix, 0), pad(prefix, u64::MAX));
        self.ctx
            .snap
            .scan_between_cols(perm, lo, hi, crate::index::ALL_COLS, |c| {
                match c {
                    Chunk::Block(b, s, e) => {
                        for i in s..e {
                            if self.graph.accepts(b.cols[gc][i]) {
                                out.push(Id(b.cols[col][i]));
                            }
                        }
                    }
                    Chunk::Row(k) => {
                        if self.graph.accepts(k[gc]) {
                            out.push(Id(k[col]));
                        }
                    }
                }
                Ok(true)
            })?;
        if col < 3 {
            out.dedup();
        }
        Ok(out)
    }

    fn object(&self, s: Id, p: Id) -> Result<Option<Id>> {
        Ok(self.scan(Perm::Pso, &[p.0, s.0], 2)?.first().copied())
    }

    fn subjects(&self, p: Id, o: Id) -> Result<Vec<Id>> {
        self.scan(Perm::Pos, &[p.0, o.0], 2)
    }

    /// `nil`, or a node with an `rdf:rest`.
    fn is_list(&self, n: Id) -> Result<bool> {
        Ok(n == self.nil || self.object(n, self.rest)?.is_some())
    }

    /// The members of the list at `n`, in order (none unless `n` is a list).
    fn members(&self, n: Id) -> Result<Vec<Id>> {
        let mut out = Vec::new();
        if !self.is_list(n)? {
            return Ok(out);
        }
        let mut cell = n;
        let mut seen = FxHashSet::default();
        while cell != self.nil && seen.insert(cell) && seen.len() <= MAX_CELLS {
            if seen.len() % 4096 == 0 {
                self.ctx.check()?;
            }
            if let Some(m) = self.object(cell, self.first)? {
                out.push(m);
            }
            match self.object(cell, self.rest)? {
                Some(next) => cell = next,
                None => break,
            }
        }
        Ok(out)
    }

    /// The number of cells of the list at `n`, `None` unless `n` is a list.
    fn length(&self, n: Id) -> Result<Option<usize>> {
        if !self.is_list(n)? {
            return Ok(None);
        }
        let mut len = 0;
        let mut cell = n;
        let mut seen = FxHashSet::default();
        while cell != self.nil && seen.insert(cell) && seen.len() <= MAX_CELLS {
            len += 1;
            match self.object(cell, self.rest)? {
                Some(next) => cell = next,
                None => break,
            }
        }
        Ok(Some(len))
    }

    /// Every list's head (ARQ's `GraphList.findAllLists`): the subjects of `rdf:rest`
    /// that are no list's rest, and `rdf:nil` when a triple other than an `rdf:rest`
    /// mentions it.
    fn heads(&self) -> Result<Vec<Id>> {
        let mut out = Vec::new();
        for s in dedup(self.scan(Perm::Pso, &[self.rest.0], 1)?) {
            if self.subjects(self.rest, s)?.is_empty() {
                out.push(s);
            }
        }
        let nil = self.nil.0;
        let as_object = self
            .scan(Perm::Osp, &[nil], 2)?
            .into_iter()
            .any(|p| p != self.rest);
        let as_subject = !self.scan(Perm::Spo, &[nil], 1)?.is_empty();
        if as_object || as_subject {
            out.push(self.nil);
        }
        Ok(out)
    }

    /// The heads of the lists that hold `m`, once per cell that holds it (ARQ's
    /// `GraphList.listFromMember`).
    fn heads_holding(&self, m: Id) -> Result<Vec<Id>> {
        let mut out = Vec::new();
        for cell in self.subjects(self.first, m)? {
            let mut c = cell;
            let mut seen = FxHashSet::default();
            while seen.insert(c) && seen.len() <= MAX_CELLS {
                match self.subjects(self.rest, c)?.first() {
                    Some(&prev) => c = prev,
                    None => break,
                }
            }
            out.push(c);
        }
        Ok(out)
    }
}

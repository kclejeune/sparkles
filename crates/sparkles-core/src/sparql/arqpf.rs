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
                    .plan_term(*id)
                    .map_or_else(|| "UNDEF".into(), |t| t.to_string()),
            })
            .collect::<Vec<_>>()
            .join(" ")
    );
    let Some((graph, _)) = p.graph_filter(g)? else {
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
                        Some(x) if integer(ctx, x)? == integer(ctx, n)? => vec![vec![l, x]],
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
                    (Some(i), m) => match integer(ctx, i)? {
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
            let decode = ctx.charge(0)?;
            let (Some(text), Some(pattern)) = (
                literal_lexical(ctx, s, &decode)?,
                literal_lexical(ctx, re, &decode)?,
            ) else {
                return Ok(Vec::new());
            };
            let re = regex::Regex::new(&pattern).map_err(|e| bad(&e.to_string()))?;
            let tokens = java_split(&re, &text);
            match args[0] {
                None => tokens
                    .iter()
                    .map(|t| vec![string(t), s, re_id(args[2])])
                    .collect(),
                Some(x) => match ctx.value_checked_into(x, &decode)? {
                    Some(Value::Str(v)) if tokens.iter().any(|t| **t == *v) => {
                        vec![vec![x, s, re_id(args[2])]]
                    }
                    _ => Vec::new(),
                },
            }
        }
        PfKind::Concat => {
            let decode = ctx.charge(0)?;
            let mut text = String::new();
            for a in &args[1..] {
                let Some(a) = a else {
                    return Ok(Vec::new());
                };
                match ctx.term_checked_into(*a, &decode)? {
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
                && !matches!(ctx.term_checked(s)?, Some(Term::Literal(_)))
            {
                return Ok(Vec::new());
            }
            let Some(o) = args[1] else {
                return Err(bad("the object is an unbound variable"));
            };
            let decode = ctx.charge(0)?;
            let v = match ctx.term_checked_into(o, &decode)? {
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
            let decode = ctx.charge(0)?;
            let Some(Term::NamedNode(iri)) = ctx.term_checked_into(s, &decode)? else {
                return Ok(Vec::new());
            };
            let at = super::fnlib::split_xml(iri.as_str());
            let (ns, local) = iri.as_str().split_at(at);
            let nsv = match args[1] {
                None => ctx.intern_term(&Term::NamedNode(NamedNode::new_unchecked(ns))),
                Some(x) => {
                    let text = match ctx.term_checked_into(x, &decode)? {
                        Some(Term::NamedNode(n)) => Some(n.as_str().to_string()),
                        Some(Term::Literal(l))
                            if l.datatype() == xsd::STRING || l.language().is_some() =>
                        {
                            Some(l.value().to_string())
                        }
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
                    if string_literal(ctx, x, &decode)?.as_deref() != Some(local) {
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
                let same = if s == o {
                    true
                } else {
                    let decode = ctx.charge(0)?;
                    match (
                        ctx.term_checked_into(s, &decode)?,
                        ctx.term_checked_into(o, &decode)?,
                    ) {
                        (Some(a), Some(b)) => super::cdt::same_value(&a, &b).unwrap_or(false),
                        _ => false,
                    }
                };
                if same { vec![vec![s, o]] } else { Vec::new() }
            }
        },
        PfKind::BNode => {
            let Some(s) = args[0] else {
                return Err(bad("the subject is an unbound variable"));
            };
            let Some(Term::BlankNode(b)) = ctx.term_checked(s)? else {
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
fn integer(ctx: &Ctx, id: Id) -> Result<Option<i64>> {
    Ok(match ctx.value_checked(id)? {
        Some(Value::Integer(i)) => Some(i64::from(i)),
        _ => None,
    })
}

/// The lexical form of a literal.
fn literal_lexical(ctx: &Ctx, id: Id, credit: &super::ctx::Charge<'_>) -> Result<Option<String>> {
    Ok(match ctx.term_checked_into(id, credit)? {
        Some(Term::Literal(l)) => Some(l.value().to_string()),
        _ => None,
    })
}

/// The lexical form of a string literal (ARQ's `NodeUtils.stringLiteral`): a simple
/// literal, an `xsd:string` or a language-tagged string.
fn string_literal(ctx: &Ctx, id: Id, credit: &super::ctx::Charge<'_>) -> Result<Option<String>> {
    Ok(match ctx.term_checked_into(id, credit)? {
        Some(Term::Literal(l)) if l.datatype() == xsd::STRING || l.language().is_some() => {
            Some(l.value().to_string())
        }
        _ => None,
    })
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
    fn number(&self, p: Id) -> Result<Option<u64>> {
        if let Some(&cached) = self.numbers.borrow().get(&p) {
            return Ok(cached);
        }
        let n = match self.ctx.term_checked(p)? {
            Some(Term::NamedNode(n)) => n
                .as_str()
                .strip_prefix(RDF_NS)
                .and_then(|l| l.strip_prefix('_'))
                .filter(|d| !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit()))
                .and_then(|d| d.parse().ok()),
            _ => None,
        };
        self.numbers.borrow_mut().insert(p, n);
        Ok(n)
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
        let mut out = Vec::new();
        for (p, o) in self.scan_pairs(Perm::Spo, &[c.0], [1, 2])? {
            if let Some(n) = self.number(p)? {
                out.push((n, o));
            }
        }
        Ok(out)
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

// ------------------------------------------------------------ streaming leaf reader ----

const RANGE_CHUNK: usize = 256;
const ID_BUF_BYTES: u64 = (RANGE_CHUNK * std::mem::size_of::<Id>()) as u64;
const PAIR_BUF_BYTES: u64 = (RANGE_CHUNK * std::mem::size_of::<(Id, Id)>()) as u64;

/// Conservative SIMD control-group width for `hashbrown::RawTable` (covers 16 B SSE2/NEON
/// and 32 B AVX2 groups).
const HASH_GROUP_WIDTH: usize = 32;

fn hashtable_buckets(cap: usize) -> usize {
    if cap == 0 {
        0
    } else {
        cap.saturating_mul(8).div_ceil(7).max(4).next_power_of_two()
    }
}

fn hashtable_bytes_for_buckets<T>(buckets: usize) -> u64 {
    if buckets == 0 {
        return 0;
    }
    let size = std::mem::size_of::<T>();
    let ctrl_align = std::mem::align_of::<T>().max(HASH_GROUP_WIDTH);
    let data_bytes = (buckets as u64).saturating_mul(size as u64);
    let mask = (ctrl_align as u64).saturating_sub(1);
    let ctrl_offset = data_bytes.saturating_add(mask) & !mask;
    ctrl_offset
        .saturating_add(buckets as u64)
        .saturating_add(HASH_GROUP_WIDTH as u64)
}

fn hashtable_bytes<T>(cap: usize) -> u64 {
    hashtable_bytes_for_buckets::<T>(hashtable_buckets(cap))
}

fn hashtable_next_bytes<T>(cap: usize) -> u64 {
    let next_buckets = if cap == 0 {
        8
    } else {
        hashtable_buckets(cap).saturating_mul(2)
    };
    hashtable_bytes_for_buckets::<T>(next_buckets)
}

fn hashtable_growth_bytes<T>(cap: usize) -> u64 {
    hashtable_bytes::<T>(cap).saturating_add(hashtable_next_bytes::<T>(cap))
}

fn hashset_id_bytes(cap: usize) -> u64 {
    hashtable_bytes::<Id>(cap)
}

fn hashset_id_growth_bytes(cap: usize) -> u64 {
    hashtable_growth_bytes::<Id>(cap)
}

fn numbers_map_bytes(cap: usize) -> u64 {
    hashtable_bytes::<(Id, Option<u64>)>(cap)
}

fn numbers_map_next_bytes(cap: usize) -> u64 {
    hashtable_next_bytes::<(Id, Option<u64>)>(cap)
}

/// Conservative heap estimate for `BTreeMap<u64, Id>`: each leaf node (B=6, up to 11
/// entries) is 192 B and each internal node is 288 B; non-root nodes keep at least 5
/// entries, so `ceil(len / 5) * 288` bounds leaf, internal, and partially filled nodes
/// during growth and `pop_first` draining, and `288` keeps the empty root leaf charged
/// when `allocated` is true until the map drops.
fn btree_map_bytes(len: usize, allocated: bool) -> u64 {
    if len == 0 {
        if allocated { 288 } else { 0 }
    } else {
        (len.div_ceil(5) as u64).saturating_mul(288)
    }
}

fn prepare_id_buf(buf: &mut Vec<Id>, admit: &mut dyn FnMut(u64) -> Result<()>) -> Result<()> {
    if buf.capacity() < RANGE_CHUNK {
        admit(ID_BUF_BYTES)?;
        buf.reserve_exact(RANGE_CHUNK - buf.len());
    }
    Ok(())
}

fn prepare_pair_buf(
    buf: &mut Vec<(Id, Id)>,
    admit: &mut dyn FnMut(u64) -> Result<()>,
) -> Result<()> {
    if buf.capacity() < RANGE_CHUNK {
        admit(PAIR_BUF_BYTES)?;
        buf.reserve_exact(RANGE_CHUNK - buf.len());
    }
    Ok(())
}

fn key_successor(mut key: [u64; 4]) -> Option<[u64; 4]> {
    for i in (0..4).rev() {
        if key[i] != u64::MAX {
            key[i] += 1;
            return Some(key);
        }
        key[i] = 0;
    }
    None
}

fn has_local(prefix: &[u64]) -> bool {
    prefix.iter().any(|&x| Id(x).tag() == crate::id::Tag::Local)
}

fn has_any(ctx: &Ctx, graph: &GraphFilter, perm: Perm, prefix: &[u64]) -> Result<bool> {
    if has_local(prefix) {
        return Ok(false);
    }
    let gc = perm.col_of(G);
    let (lo, hi) = (pad(prefix, 0), pad(prefix, u64::MAX));
    let mut found = false;
    let mut scanned = 0usize;
    ctx.snap.scan_between_cols_checked(
        perm,
        lo,
        hi,
        crate::index::ALL_COLS,
        || ctx.check(),
        |c| {
            match c {
                Chunk::Block(b, s, e) => {
                    for i in s..e {
                        scanned += 1;
                        if scanned.is_multiple_of(1024) {
                            ctx.check()?;
                        }
                        if graph.accepts(b.cols[gc][i]) {
                            found = true;
                            return Ok(false);
                        }
                    }
                }
                Chunk::Row(k) => {
                    scanned += 1;
                    if scanned.is_multiple_of(1024) {
                        ctx.check()?;
                    }
                    if graph.accepts(k[gc]) {
                        found = true;
                        return Ok(false);
                    }
                }
            }
            Ok(true)
        },
    )?;
    Ok(found)
}

fn first_col(
    ctx: &Ctx,
    graph: &GraphFilter,
    perm: Perm,
    prefix: &[u64],
    col: usize,
) -> Result<Option<Id>> {
    if has_local(prefix) {
        return Ok(None);
    }
    let gc = perm.col_of(G);
    let (lo, hi) = (pad(prefix, 0), pad(prefix, u64::MAX));
    let mut found = None;
    let mut scanned = 0usize;
    ctx.snap.scan_between_cols_checked(
        perm,
        lo,
        hi,
        crate::index::ALL_COLS,
        || ctx.check(),
        |c| {
            match c {
                Chunk::Block(b, s, e) => {
                    for i in s..e {
                        scanned += 1;
                        if scanned.is_multiple_of(1024) {
                            ctx.check()?;
                        }
                        if graph.accepts(b.cols[gc][i]) {
                            found = Some(Id(b.cols[col][i]));
                            return Ok(false);
                        }
                    }
                }
                Chunk::Row(k) => {
                    scanned += 1;
                    if scanned.is_multiple_of(1024) {
                        ctx.check()?;
                    }
                    if graph.accepts(k[gc]) {
                        found = Some(Id(k[col]));
                        return Ok(false);
                    }
                }
            }
            Ok(true)
        },
    )?;
    Ok(found)
}

fn has_any_col_ne(
    ctx: &Ctx,
    graph: &GraphFilter,
    perm: Perm,
    prefix: &[u64],
    col: usize,
    excluded: u64,
) -> Result<bool> {
    if has_local(prefix) {
        return Ok(false);
    }
    let gc = perm.col_of(G);
    let (lo, hi) = (pad(prefix, 0), pad(prefix, u64::MAX));
    let mut found = false;
    let mut scanned = 0usize;
    ctx.snap.scan_between_cols_checked(
        perm,
        lo,
        hi,
        crate::index::ALL_COLS,
        || ctx.check(),
        |c| {
            match c {
                Chunk::Block(b, s, e) => {
                    for i in s..e {
                        scanned += 1;
                        if scanned.is_multiple_of(1024) {
                            ctx.check()?;
                        }
                        if graph.accepts(b.cols[gc][i]) && b.cols[col][i] != excluded {
                            found = true;
                            return Ok(false);
                        }
                    }
                }
                Chunk::Row(k) => {
                    scanned += 1;
                    if scanned.is_multiple_of(1024) {
                        ctx.check()?;
                    }
                    if graph.accepts(k[gc]) && k[col] != excluded {
                        found = true;
                        return Ok(false);
                    }
                }
            }
            Ok(true)
        },
    )?;
    Ok(found)
}

/// Resumable chunked range scan over a single index permutation, deduplicating projected
/// columns across graphs in O(1) memory.
struct RangeCursor {
    perm: Perm,
    lo: Option<[u64; 4]>,
    hi: [u64; 4],
    last_id: Option<Id>,
    last_pair: Option<(Id, Id)>,
}

impl RangeCursor {
    fn new(perm: Perm, prefix: &[u64]) -> Self {
        let lo = (!has_local(prefix)).then(|| pad(prefix, 0));
        Self {
            perm,
            lo,
            hi: pad(prefix, u64::MAX),
            last_id: None,
            last_pair: None,
        }
    }

    fn is_done(&self) -> bool {
        self.lo.is_none()
    }

    fn next_ids(
        &mut self,
        ctx: &Ctx,
        graph: &GraphFilter,
        col: usize,
        skip_after_col1: bool,
        out: &mut Vec<Id>,
        limit: usize,
    ) -> Result<()> {
        self.next_ids_with_check(ctx, graph, col, skip_after_col1, out, limit, || ctx.check())
    }

    #[allow(clippy::too_many_arguments)]
    fn next_ids_with_check(
        &mut self,
        ctx: &Ctx,
        graph: &GraphFilter,
        col: usize,
        skip_after_col1: bool,
        out: &mut Vec<Id>,
        limit: usize,
        check: impl Fn() -> Result<()>,
    ) -> Result<()> {
        debug_assert!(out.capacity() >= limit);
        out.clear();
        let Some(lo) = self.lo else {
            return Ok(());
        };
        let gc = self.perm.col_of(G);
        let mut last_key: Option<[u64; 4]> = None;
        let mut stopped = false;
        let mut scanned = 0usize;
        let mut last_id = self.last_id;
        ctx.snap.scan_between_cols_checked(
            self.perm,
            lo,
            self.hi,
            crate::index::ALL_COLS,
            &check,
            |c| {
                match c {
                    Chunk::Block(b, s, e) => {
                        for i in s..e {
                            scanned += 1;
                            if scanned.is_multiple_of(1024) {
                                check()?;
                            }
                            if !graph.accepts(b.cols[gc][i]) {
                                continue;
                            }
                            let k = [b.cols[0][i], b.cols[1][i], b.cols[2][i], b.cols[3][i]];
                            let id = Id(k[col]);
                            last_key = Some(k);
                            if col < 3 && last_id == Some(id) {
                                continue;
                            }
                            last_id = Some(id);
                            out.push(id);
                            if out.len() >= limit {
                                stopped = true;
                                return Ok(false);
                            }
                        }
                    }
                    Chunk::Row(k) => {
                        scanned += 1;
                        if scanned.is_multiple_of(1024) {
                            check()?;
                        }
                        if !graph.accepts(k[gc]) {
                            return Ok(true);
                        }
                        let id = Id(k[col]);
                        last_key = Some(k);
                        if col < 3 && last_id == Some(id) {
                            return Ok(true);
                        }
                        last_id = Some(id);
                        out.push(id);
                        if out.len() >= limit {
                            stopped = true;
                            return Ok(false);
                        }
                    }
                }
                Ok(true)
            },
        )?;
        self.last_id = last_id;
        self.lo = if stopped {
            last_key
                .and_then(|k| {
                    if skip_after_col1 {
                        k[1].checked_add(1).map(|next| [k[0], next, 0, 0])
                    } else {
                        key_successor(k)
                    }
                })
                .filter(|k| *k <= self.hi)
        } else {
            None
        };
        Ok(())
    }

    fn next_pairs(
        &mut self,
        ctx: &Ctx,
        graph: &GraphFilter,
        cols: [usize; 2],
        out: &mut Vec<(Id, Id)>,
        limit: usize,
    ) -> Result<()> {
        debug_assert!(out.capacity() >= limit);
        out.clear();
        let Some(lo) = self.lo else {
            return Ok(());
        };
        let gc = self.perm.col_of(G);
        let mut last_key: Option<[u64; 4]> = None;
        let mut stopped = false;
        let mut scanned = 0usize;
        let mut last_pair = self.last_pair;
        ctx.snap.scan_between_cols_checked(
            self.perm,
            lo,
            self.hi,
            crate::index::ALL_COLS,
            || ctx.check(),
            |c| {
                match c {
                    Chunk::Block(b, s, e) => {
                        for i in s..e {
                            scanned += 1;
                            if scanned.is_multiple_of(1024) {
                                ctx.check()?;
                            }
                            if !graph.accepts(b.cols[gc][i]) {
                                continue;
                            }
                            let k = [b.cols[0][i], b.cols[1][i], b.cols[2][i], b.cols[3][i]];
                            let pair = (Id(k[cols[0]]), Id(k[cols[1]]));
                            last_key = Some(k);
                            if last_pair == Some(pair) {
                                continue;
                            }
                            last_pair = Some(pair);
                            out.push(pair);
                            if out.len() >= limit {
                                stopped = true;
                                return Ok(false);
                            }
                        }
                    }
                    Chunk::Row(k) => {
                        scanned += 1;
                        if scanned.is_multiple_of(1024) {
                            ctx.check()?;
                        }
                        if !graph.accepts(k[gc]) {
                            return Ok(true);
                        }
                        let pair = (Id(k[cols[0]]), Id(k[cols[1]]));
                        last_key = Some(k);
                        if last_pair == Some(pair) {
                            return Ok(true);
                        }
                        last_pair = Some(pair);
                        out.push(pair);
                        if out.len() >= limit {
                            stopped = true;
                            return Ok(false);
                        }
                    }
                }
                Ok(true)
            },
        )?;
        self.last_pair = last_pair;
        self.lo = if stopped {
            last_key.and_then(key_successor).filter(|k| *k <= self.hi)
        } else {
            None
        };
        Ok(())
    }
}

/// Constants interned once for store-scanning list and container readers.
#[derive(Clone, Copy)]
struct StoreIds {
    first: Id,
    rest: Id,
    nil: Id,
    rdf_type: Id,
    container_types: [Id; 3],
}

impl StoreIds {
    fn new(ctx: &Ctx) -> Result<Self> {
        let iri = |n: oxrdf::NamedNodeRef<'_>| ctx.intern_term(&Term::NamedNode(n.into_owned()));
        let first = iri(rdf::FIRST);
        let rest = iri(rdf::REST);
        let nil = iri(rdf::NIL);
        let rdf_type = iri(rdf::TYPE);
        let container_types = [
            ctx.intern_term(&Term::NamedNode(ContainerType::Bag.iri())),
            ctx.intern_term(&Term::NamedNode(ContainerType::Seq.iri())),
            ctx.intern_term(&Term::NamedNode(ContainerType::Alt.iri())),
        ];
        ctx.check()?;
        Ok(Self {
            first,
            rest,
            nil,
            rdf_type,
            container_types,
        })
    }

    fn is_list(&self, ctx: &Ctx, graph: &GraphFilter, n: Id) -> Result<bool> {
        Ok(n == self.nil || has_any(ctx, graph, Perm::Pso, &[self.rest.0, n.0])?)
    }

    fn is_container(
        &self,
        ctx: &Ctx,
        graph: &GraphFilter,
        c: Id,
        ty: Option<ContainerType>,
    ) -> Result<bool> {
        for (idx, t) in ContainerType::ALL.iter().enumerate() {
            if ty.is_some_and(|x| x != *t) {
                continue;
            }
            if has_any(
                ctx,
                graph,
                Perm::Pso,
                &[self.rdf_type.0, c.0, self.container_types[idx].0],
            )? {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

/// Resumable walker over one RDF collection starting at `head`, charging its cycle set
/// before growth.
struct ListWalk {
    head: Id,
    cell: Option<Id>,
    seen: FxHashSet<Id>,
    pos: i64,
}

impl ListWalk {
    fn new(ctx: &Ctx, graph: &GraphFilter, ids: &StoreIds, head: Id) -> Result<Self> {
        let cell = ids.is_list(ctx, graph, head)?.then_some(head);
        Ok(Self {
            head,
            cell,
            seen: FxHashSet::default(),
            pos: 0,
        })
    }

    fn bytes(&self) -> u64 {
        hashset_id_bytes(self.seen.capacity())
    }

    fn step(
        &mut self,
        ctx: &Ctx,
        graph: &GraphFilter,
        ids: &StoreIds,
        admit_extra: &mut dyn FnMut(u64) -> Result<()>,
    ) -> Result<Option<(i64, Id)>> {
        while let Some(c) = self.cell {
            if c == ids.nil || self.seen.len() >= MAX_CELLS {
                self.cell = None;
                return Ok(None);
            }
            if !self.seen.contains(&c) {
                if self.seen.len() == self.seen.capacity() {
                    admit_extra(hashset_id_growth_bytes(self.seen.capacity()))?;
                    self.seen.insert(c);
                    admit_extra(self.bytes())?;
                } else {
                    self.seen.insert(c);
                }
            } else {
                self.cell = None;
                return Ok(None);
            }
            if self.seen.len().is_multiple_of(1024) {
                ctx.check()?;
            }
            let member = first_col(ctx, graph, Perm::Pso, &[ids.first.0, c.0], 2)?;
            self.cell = first_col(ctx, graph, Perm::Pso, &[ids.rest.0, c.0], 2)?;
            if let Some(m) = member {
                let p = self.pos;
                self.pos += 1;
                return Ok(Some((p, m)));
            }
        }
        Ok(None)
    }
}

/// Resumable stream of all list heads in the active graph, matching `Lists::heads`.
struct HeadScan {
    rest_scan: RangeCursor,
    buf: Vec<Id>,
    at: usize,
    checked_nil: bool,
}

impl HeadScan {
    fn new(ids: &StoreIds) -> Self {
        Self {
            rest_scan: RangeCursor::new(Perm::Pso, &[ids.rest.0]),
            buf: Vec::new(),
            at: 0,
            checked_nil: false,
        }
    }

    fn bytes(&self) -> u64 {
        (self.buf.capacity() as u64).saturating_mul(std::mem::size_of::<Id>() as u64)
    }

    fn next_head(
        &mut self,
        ctx: &Ctx,
        graph: &GraphFilter,
        ids: &StoreIds,
        admit_buf: &mut dyn FnMut(u64) -> Result<()>,
    ) -> Result<Option<Id>> {
        loop {
            while self.at < self.buf.len() {
                let s = self.buf[self.at];
                self.at += 1;
                if !has_any(ctx, graph, Perm::Pos, &[ids.rest.0, s.0])? {
                    return Ok(Some(s));
                }
            }
            if !self.rest_scan.is_done() {
                ctx.check()?;
                prepare_id_buf(&mut self.buf, admit_buf)?;
                self.rest_scan
                    .next_ids(ctx, graph, 1, true, &mut self.buf, RANGE_CHUNK)?;
                self.at = 0;
                continue;
            }
            if !self.checked_nil {
                ctx.check()?;
                self.checked_nil = true;
                let as_object = has_any_col_ne(ctx, graph, Perm::Osp, &[ids.nil.0], 2, ids.rest.0)?;
                let as_subject = has_any(ctx, graph, Perm::Spo, &[ids.nil.0])?;
                if as_object || as_subject {
                    return Ok(Some(ids.nil));
                }
            }
            return Ok(None);
        }
    }
}

/// Walk backwards from a list cell to its list head, charging the cycle set until dropped.
fn head_of_cell(
    ctx: &Ctx,
    graph: &GraphFilter,
    ids: &StoreIds,
    cell: Id,
    admit: &mut dyn FnMut(u64) -> Result<()>,
) -> Result<Id> {
    let head = {
        let mut c = cell;
        let mut seen = FxHashSet::default();
        while seen.len() < MAX_CELLS && !seen.contains(&c) {
            if seen.len() == seen.capacity() {
                admit(hashset_id_growth_bytes(seen.capacity()))?;
                seen.insert(c);
                admit(hashset_id_bytes(seen.capacity()))?;
            } else {
                seen.insert(c);
            }
            if seen.len().is_multiple_of(1024) {
                ctx.check()?;
            }
            match first_col(ctx, graph, Perm::Pos, &[ids.rest.0, c.0], 2)? {
                Some(prev) => c = prev,
                None => break,
            }
        }
        c
    };
    admit(0)?;
    Ok(head)
}

/// Compute the length of one list starting at `head`, charging its cycle set until dropped.
fn list_length(
    ctx: &Ctx,
    graph: &GraphFilter,
    ids: &StoreIds,
    head: Id,
    admit: &mut dyn FnMut(u64) -> Result<()>,
) -> Result<Option<usize>> {
    if !ids.is_list(ctx, graph, head)? {
        return Ok(None);
    }
    let len = {
        let mut len = 0usize;
        let mut cell = head;
        let mut seen = FxHashSet::default();
        while cell != ids.nil && seen.len() < MAX_CELLS && !seen.contains(&cell) {
            if seen.len() == seen.capacity() {
                admit(hashset_id_growth_bytes(seen.capacity()))?;
                seen.insert(cell);
                admit(hashset_id_bytes(seen.capacity()))?;
            } else {
                seen.insert(cell);
            }
            len += 1;
            if seen.len().is_multiple_of(1024) {
                ctx.check()?;
            }
            match first_col(ctx, graph, Perm::Pso, &[ids.rest.0, cell.0], 2)? {
                Some(next) => cell = next,
                None => break,
            }
        }
        len
    };
    admit(0)?;
    Ok(Some(len))
}

/// Eager `regex::Regex::new` Thompson NFA size limit (10 MiB). Preserved when query
/// credit permits so budgeted `apf:strSplit` accepts every pattern eager accepts.
const EAGER_SPLIT_NFA_SIZE_LIMIT: usize = 10 * (1 << 20);

/// Compile a `regex_automata::meta::Regex` for `apf:strSplit`, pre-admitting its
/// compilation envelope and retained program/cache bytes against the query memory budget.
fn compile_split_regex(
    ctx: &Ctx,
    pattern: &str,
    charge: &mut super::ctx::OwnedCharge,
    base_bytes: u64,
    bad: &dyn Fn(&str) -> Error,
) -> Result<(
    regex_automata::meta::Regex,
    regex_automata::meta::Cache,
    u64,
)> {
    let pat_envelope = (pattern.len() as u64)
        .saturating_mul(256)
        .saturating_add(4096);
    let available = ctx.memory_remaining();
    let maximum = ((available.saturating_sub(pat_envelope) / 48)
        .min(EAGER_SPLIT_NFA_SIZE_LIMIT as u64)) as usize;
    if maximum < 1024 {
        return Err(ctx.memory_exceeded(ctx.mem_limit.saturating_add(1)));
    }
    charge.resize(base_bytes.saturating_add(pat_envelope))?;
    let mut nfa = maximum.min(pattern.len().saturating_mul(128).saturating_add(8192));
    let (program, hybrid) = loop {
        ctx.check()?;
        let hybrid = (nfa * 2).min(2 << 20);
        let build_bytes = pat_envelope
            .saturating_add((nfa as u64).saturating_mul(32))
            .saturating_add((hybrid as u64).saturating_mul(4));
        charge.resize(base_bytes.saturating_add(build_bytes))?;
        let built = regex_automata::meta::Regex::builder()
            .configure(
                regex_automata::meta::Regex::config()
                    .nfa_size_limit(Some(nfa))
                    .hybrid_cache_capacity(hybrid)
                    .dfa(false)
                    .pool_capacity(0)
                    .onepass(false)
                    .backtrack(false),
            )
            .build(pattern);
        match built {
            Ok(program) => break (program, hybrid),
            Err(e) if e.size_limit().is_some() && nfa < maximum => {
                nfa = nfa.saturating_mul(2).min(maximum);
            }
            Err(e) if e.size_limit().is_some() && maximum < EAGER_SPLIT_NFA_SIZE_LIMIT => {
                return Err(ctx.memory_exceeded(ctx.mem_limit.saturating_add(1)));
            }
            Err(e) => return Err(bad(&e.to_string())),
        }
    };
    let cache_bytes = (program.memory_usage() as u64)
        .saturating_mul(
            (program.captures_len() as u64)
                .saturating_mul(8)
                .saturating_add(16),
        )
        .saturating_add((hybrid as u64).saturating_mul(2))
        .saturating_add((program.captures_len() as u64).saturating_mul(512))
        .saturating_add(4096);
    let retained = (program.memory_usage() as u64)
        .saturating_add(cache_bytes)
        .saturating_add(4096);
    charge.resize(base_bytes.saturating_add(retained))?;
    let cache = program.create_cache();
    ctx.check()?;
    Ok((program, cache, retained))
}

/// Resumable O(1)-extra-memory iterator reproducing [`java_split`] verbatim.
struct StrSplitIter {
    re: regex_automata::meta::Regex,
    cache: regex_automata::meta::Cache,
    regex_bytes: u64,
    text: String,
    search_at: usize,
    last_match_end: Option<usize>,
    last: usize,
    pending_empty: usize,
    next_non_empty: Option<(usize, usize)>,
    empty_input: bool,
    finished_scan: bool,
}

impl StrSplitIter {
    fn new(
        re: regex_automata::meta::Regex,
        cache: regex_automata::meta::Cache,
        regex_bytes: u64,
        text: String,
    ) -> Self {
        let empty_input = text.is_empty();
        Self {
            re,
            cache,
            regex_bytes,
            text,
            search_at: 0,
            last_match_end: None,
            last: 0,
            pending_empty: 0,
            next_non_empty: None,
            empty_input,
            finished_scan: empty_input,
        }
    }

    fn bytes(&self) -> u64 {
        (self.text.capacity() as u64).saturating_add(self.regex_bytes)
    }

    fn next_token(&mut self, ctx: &Ctx) -> Result<Option<&str>> {
        self.next_token_checked(|| ctx.check())
    }

    fn next_token_checked(
        &mut self,
        mut check: impl FnMut() -> Result<()>,
    ) -> Result<Option<&str>> {
        if self.empty_input {
            self.empty_input = false;
            return Ok(Some(""));
        }
        let mut steps = 0usize;
        loop {
            if let Some((start, end)) = self.next_non_empty {
                if self.pending_empty > 0 {
                    self.pending_empty -= 1;
                    return Ok(Some(""));
                }
                self.next_non_empty = None;
                return Ok(Some(self.text[start..end].trim()));
            }
            if self.finished_scan {
                // Any remaining pending_empty are trailing empty strings and are dropped.
                self.pending_empty = 0;
                return Ok(None);
            }
            let (start, end) = loop {
                steps += 1;
                if steps.is_multiple_of(1024) {
                    check()?;
                }
                if self.search_at > self.text.len() {
                    self.finished_scan = true;
                    break (self.last, self.text.len());
                }
                let input = regex_automata::Input::new(&self.text).range(self.search_at..);
                let Some(mut m) = self.re.search_with(&mut self.cache, &input) else {
                    self.finished_scan = true;
                    break (self.last, self.text.len());
                };
                // Reproduce `regex::Regex::find_iter` (`Searcher::try_advance`): skip a
                // zero-width match immediately following the previous match by advancing
                // one UTF-8 scalar value.
                if m.is_empty() && Some(m.end()) == self.last_match_end {
                    if m.end() >= self.text.len() {
                        self.finished_scan = true;
                        break (self.last, self.text.len());
                    }
                    let next_at = m.end()
                        + self.text[m.end()..]
                            .chars()
                            .next()
                            .map_or(1, char::len_utf8);
                    let next_input = regex_automata::Input::new(&self.text).range(next_at..);
                    let Some(m2) = self.re.search_with(&mut self.cache, &next_input) else {
                        self.finished_scan = true;
                        break (self.last, self.text.len());
                    };
                    m = m2;
                }
                self.search_at = m.end();
                self.last_match_end = Some(m.end());
                if m.end() == 0 {
                    // A zero-length match at the start gives no leading empty string.
                    continue;
                }
                if m.start() == m.end() && m.start() >= self.text.len() {
                    self.finished_scan = true;
                    break (self.last, self.text.len());
                }
                let range = (self.last, m.start());
                self.last = m.end();
                break range;
            };
            if start == end {
                self.pending_empty += 1;
            } else {
                self.next_non_empty = Some((start, end));
            }
        }
    }
}

/// Resumable stream of the members of one container `c`.
enum OneContainer {
    /// Bound member `m`: stream predicates from `Perm::Sop [c, m]`, emitting `(c, m)` for
    /// each `rdf:_n`.
    BoundMember {
        c: Id,
        m: Id,
        scan: RangeCursor,
        buf: Vec<Id>,
        at: usize,
    },
    /// Unbound member: scan `Perm::Spo [c]` into a charged `BTreeMap<u64, Id>`, then drain
    /// in numeric order.
    UnboundMember {
        c: Id,
        scan: RangeCursor,
        pair_buf: Vec<(Id, Id)>,
        by_number: std::collections::BTreeMap<u64, Id>,
        map_allocated: bool,
    },
}

impl OneContainer {
    fn new(c: Id, m: Option<Id>) -> Self {
        match m {
            Some(m) => Self::BoundMember {
                c,
                m,
                scan: RangeCursor::new(Perm::Sop, &[c.0, m.0]),
                buf: Vec::new(),
                at: 0,
            },
            None => Self::UnboundMember {
                c,
                scan: RangeCursor::new(Perm::Spo, &[c.0]),
                pair_buf: Vec::new(),
                by_number: std::collections::BTreeMap::new(),
                map_allocated: false,
            },
        }
    }

    fn bytes(&self) -> u64 {
        match self {
            Self::BoundMember { buf, .. } => {
                (buf.capacity() as u64).saturating_mul(std::mem::size_of::<Id>() as u64)
            }
            Self::UnboundMember {
                pair_buf,
                by_number,
                map_allocated,
                ..
            } => (pair_buf.capacity() as u64)
                .saturating_mul(std::mem::size_of::<(Id, Id)>() as u64)
                .saturating_add(btree_map_bytes(by_number.len(), *map_allocated)),
        }
    }
}

/// Resumable stream of container `(c, m)` solutions matching [`containers`].
struct ContainerStream {
    ty: Option<ContainerType>,
    m: Option<Id>,
    /// Fixed single container when subject is bound, or `None` when scanning candidates.
    bound_c: Option<Id>,
    /// Candidate container scanner when subject is unbound.
    candidates: ContainerCandidates,
    current: Option<OneContainer>,
}

enum ContainerCandidates {
    Done,
    ByType {
        ty_idx: usize,
        scan: Option<RangeCursor>,
        buf: Vec<Id>,
        at: usize,
        seen: FxHashSet<Id>,
    },
    ByMember {
        scan: RangeCursor,
        buf: Vec<Id>,
        at: usize,
    },
}

impl ContainerStream {
    fn new(ty: Option<ContainerType>, c: Option<Id>, m: Option<Id>) -> Self {
        let (bound_c, candidates) = match (c, m) {
            (Some(c), _) => (Some(c), ContainerCandidates::Done),
            (None, None) => (
                None,
                ContainerCandidates::ByType {
                    ty_idx: 0,
                    scan: None,
                    buf: Vec::new(),
                    at: 0,
                    seen: FxHashSet::default(),
                },
            ),
            (None, Some(m)) => (
                None,
                ContainerCandidates::ByMember {
                    scan: RangeCursor::new(Perm::Osp, &[m.0]),
                    buf: Vec::new(),
                    at: 0,
                },
            ),
        };
        Self {
            ty,
            m,
            bound_c,
            candidates,
            current: None,
        }
    }

    fn bytes(&self) -> u64 {
        let cand = match &self.candidates {
            ContainerCandidates::Done => 0,
            ContainerCandidates::ByType { buf, seen, .. } => (buf.capacity() as u64)
                .saturating_mul(std::mem::size_of::<Id>() as u64)
                .saturating_add(hashset_id_bytes(seen.capacity())),
            ContainerCandidates::ByMember { buf, .. } => {
                (buf.capacity() as u64).saturating_mul(std::mem::size_of::<Id>() as u64)
            }
        };
        cand.saturating_add(self.current.as_ref().map_or(0, OneContainer::bytes))
    }
}

/// One candidate slot tuple produced by [`LeafReader::next_solution`].
/// Every built-in function except `apf:concat` has at most 3 slots and stays stack-only.
enum Solution {
    Fixed([Id; 3], u8),
    Wide(Vec<Id>),
}

impl Solution {
    fn pair(a: Id, b: Id) -> Self {
        Self::Fixed([a, b, Id::UNDEF], 2)
    }

    fn triple(a: Id, b: Id, c: Id) -> Self {
        Self::Fixed([a, b, c], 3)
    }

    fn as_slice(&self) -> &[Id] {
        match self {
            Self::Fixed(arr, len) => &arr[..usize::from(*len)],
            Self::Wide(v) => v.as_slice(),
        }
    }
}

enum ReaderMode {
    Done,
    /// Pre-evaluated 0-or-1-row constant result (`Version`, `Assign`, `BNode`, `Str`,
    /// `Concat`, `SplitIri`, or bound-subject `StrSplit`).
    Single(Option<Solution>),
    StrSplit {
        iter: Box<StrSplitIter>,
        s: Id,
        re: Id,
    },
    MemberBound {
        walk: ListWalk,
        filter_m: Option<Id>,
    },
    MemberAll {
        heads: HeadScan,
        walk: Option<ListWalk>,
    },
    MemberByObject {
        m: Id,
        cells: RangeCursor,
        buf: Vec<Id>,
        at: usize,
    },
    LengthAll {
        heads: HeadScan,
    },
    IndexBound {
        walk: ListWalk,
        i_arg: Option<Id>,
        target_pos: Option<i64>,
        m_arg: Option<Id>,
    },
    IndexAll {
        heads: IndexHeads,
        seen_heads: FxHashSet<Id>,
        walk: Option<ListWalk>,
        i_arg: Option<Id>,
        target_pos: Option<i64>,
        m_arg: Option<Id>,
    },
    Container(ContainerStream),
    RdfsMember {
        stored: RdfsStored,
        m_arg: Option<Id>,
        s_arg: Option<Id>,
    },
}

enum IndexHeads {
    All(HeadScan),
    ByMember {
        cells: RangeCursor,
        buf: Vec<Id>,
        at: usize,
    },
}

enum RdfsStored {
    Single(Option<(Id, Id)>),
    SingleCol {
        fixed: Id,
        fixed_is_subject: bool,
        scan: RangeCursor,
        buf: Vec<Id>,
        at: usize,
    },
    Pairs {
        scan: RangeCursor,
        buf: Vec<(Id, Id)>,
        at: usize,
    },
}

/// Resumable, budget-charged streaming reader for a leaf [`Kind::PropertyFn`] (one whose
/// `needs_input()` is false and which has no plan children).
pub(crate) struct LeafReader {
    graph: GraphFilter,
    same_var_pairs: Vec<(usize, usize)>,
    out_slot: Vec<usize>,
    row_buf: Vec<Id>,
    ids: Option<StoreIds>,
    numbers: FxHashMap<Id, Option<u64>>,
    mode: ReaderMode,
    charge: super::ctx::OwnedCharge,
}

impl LeafReader {
    pub(crate) fn open(ctx: &std::sync::Arc<Ctx>, spec: &PfSpec, vars: &[VarId]) -> Result<Self> {
        ctx.check()?;
        let mut charge = super::ctx::OwnedCharge::new(ctx, 0)?;

        // Build open-time metadata with exact-capacity vectors and zero regrowth:
        // first pre-admit `args` and temporary `var_slots`, sort `var_slots` in-place
        // with O(1) stack space (`sort_unstable`), pre-admit exact `same_var_pairs`,
        // drop `var_slots`, then pre-admit exact `out_slot` and `row_buf`.
        let args_bytes =
            (spec.slots.len() as u64).saturating_mul(std::mem::size_of::<Option<Id>>() as u64);
        let var_count = spec
            .slots
            .iter()
            .filter(|s| matches!(s, PathEnd::Var(_)))
            .count();
        let var_slots_bytes =
            (var_count as u64).saturating_mul(std::mem::size_of::<(VarId, usize)>() as u64);
        charge.resize(args_bytes.saturating_add(var_slots_bytes))?;

        let mut args = Vec::with_capacity(spec.slots.len());
        let mut var_slots = Vec::with_capacity(var_count);
        for (j, s) in spec.slots.iter().enumerate() {
            match s {
                PathEnd::Const(id) => args.push(Some(*id)),
                PathEnd::Var(v) => {
                    args.push(None);
                    var_slots.push((*v, j));
                }
            }
        }
        var_slots.sort_unstable();
        let dup_count = var_slots.windows(2).filter(|w| w[0].0 == w[1].0).count();
        let pairs_bytes =
            (dup_count as u64).saturating_mul(std::mem::size_of::<(usize, usize)>() as u64);
        charge.resize(
            args_bytes
                .saturating_add(var_slots_bytes)
                .saturating_add(pairs_bytes),
        )?;
        let mut same_var_pairs = Vec::with_capacity(dup_count);
        if let Some(&(mut cur_var, mut first_j)) = var_slots.first() {
            for &(v, j) in &var_slots[1..] {
                if v == cur_var {
                    same_var_pairs.push((first_j, j));
                } else {
                    cur_var = v;
                    first_j = j;
                }
            }
        }
        drop(var_slots);

        let out_slot_bytes =
            (vars.len() as u64).saturating_mul(std::mem::size_of::<usize>() as u64);
        let row_buf_bytes = (vars.len() as u64).saturating_mul(std::mem::size_of::<Id>() as u64);
        let args_cap_bytes =
            (args.capacity() as u64).saturating_mul(std::mem::size_of::<Option<Id>>() as u64);
        let pairs_cap_bytes = (same_var_pairs.capacity() as u64)
            .saturating_mul(std::mem::size_of::<(usize, usize)>() as u64);
        charge.resize(
            args_cap_bytes
                .saturating_add(pairs_cap_bytes)
                .saturating_add(out_slot_bytes)
                .saturating_add(row_buf_bytes),
        )?;

        let mut out_slot = Vec::with_capacity(vars.len());
        for v in vars {
            out_slot.push(
                spec.slots
                    .iter()
                    .position(|s| matches!(s, PathEnd::Var(x) if x == v))
                    .expect("a leaf property-function variable belongs to a slot"),
            );
        }
        let row_buf = vec![Id::UNDEF; vars.len()];

        let retained_meta = pairs_cap_bytes
            .saturating_add(
                (out_slot.capacity() as u64).saturating_mul(std::mem::size_of::<usize>() as u64),
            )
            .saturating_add(
                (row_buf.capacity() as u64).saturating_mul(std::mem::size_of::<Id>() as u64),
            );
        let open_base = retained_meta.saturating_add(args_cap_bytes);
        charge.resize(open_base)?;

        let int = |n: i64| ctx.intern_value(&Value::Integer(n.into()));
        let string = |s: &str| ctx.intern_simple_str_checked(s);
        let bad = |m: &str| Error::invalid(format!("{}: {m}", spec.kind.name()));

        let mut ids: Option<StoreIds> = None;
        let mode = match spec.kind {
            PfKind::Member => {
                let sids = StoreIds::new(ctx)?;
                ids = Some(sids);
                match (args[0], args[1]) {
                    (Some(l), o) => ReaderMode::MemberBound {
                        walk: ListWalk::new(ctx, &spec.graph, &sids, l)?,
                        filter_m: o,
                    },
                    (None, None) => ReaderMode::MemberAll {
                        heads: HeadScan::new(&sids),
                        walk: None,
                    },
                    (None, Some(m)) => ReaderMode::MemberByObject {
                        m,
                        cells: RangeCursor::new(Perm::Pos, &[sids.first.0, m.0]),
                        buf: Vec::new(),
                        at: 0,
                    },
                }
            }
            PfKind::Length => {
                let sids = StoreIds::new(ctx)?;
                ids = Some(sids);
                match (args[0], args[1]) {
                    (Some(l), o) => {
                        let len = list_length(ctx, &spec.graph, &sids, l, &mut |b| {
                            charge.resize(open_base.saturating_add(b))
                        })?;
                        match len {
                            Some(n) => {
                                let nid = int(n as i64);
                                ctx.check()?;
                                match o {
                                    None => ReaderMode::Single(Some(Solution::pair(l, nid))),
                                    Some(x) if integer(ctx, x)? == integer(ctx, nid)? => {
                                        ReaderMode::Single(Some(Solution::pair(l, x)))
                                    }
                                    Some(_) => ReaderMode::Done,
                                }
                            }
                            None => ReaderMode::Done,
                        }
                    }
                    (None, None) => ReaderMode::LengthAll {
                        heads: HeadScan::new(&sids),
                    },
                    (None, Some(_)) => ReaderMode::Done,
                }
            }
            PfKind::Index => {
                let sids = StoreIds::new(ctx)?;
                ids = Some(sids);
                let (i_arg, m_arg) = (args[1], args[2]);
                let target_pos = match i_arg {
                    Some(i) => match integer(ctx, i)? {
                        Some(p) if p >= 0 => Some(p),
                        _ => {
                            drop(args);
                            return Self::empty(
                                spec.graph.clone(),
                                same_var_pairs,
                                out_slot,
                                row_buf,
                                charge,
                            );
                        }
                    },
                    None => None,
                };
                match args[0] {
                    Some(l) => ReaderMode::IndexBound {
                        walk: ListWalk::new(ctx, &spec.graph, &sids, l)?,
                        i_arg,
                        target_pos,
                        m_arg,
                    },
                    None => {
                        let heads = match m_arg {
                            Some(m) => IndexHeads::ByMember {
                                cells: RangeCursor::new(Perm::Pos, &[sids.first.0, m.0]),
                                buf: Vec::new(),
                                at: 0,
                            },
                            None => IndexHeads::All(HeadScan::new(&sids)),
                        };
                        ReaderMode::IndexAll {
                            heads,
                            seen_heads: FxHashSet::default(),
                            walk: None,
                            i_arg,
                            target_pos,
                            m_arg,
                        }
                    }
                }
            }
            PfKind::Container(ty) => {
                let sids = StoreIds::new(ctx)?;
                ids = Some(sids);
                ReaderMode::Container(ContainerStream::new(ty, args[0], args[1]))
            }
            PfKind::RdfsMember => {
                let sids = StoreIds::new(ctx)?;
                ids = Some(sids);
                let member = ctx.intern_iri_checked(RDFS_MEMBER)?;
                let stored = match (args[0], args[1]) {
                    (Some(s), Some(o)) => {
                        let hit = has_any(ctx, &spec.graph, Perm::Pso, &[member.0, s.0, o.0])?;
                        RdfsStored::Single(hit.then_some((s, o)))
                    }
                    (Some(s), None) => RdfsStored::SingleCol {
                        fixed: s,
                        fixed_is_subject: true,
                        scan: RangeCursor::new(Perm::Pso, &[member.0, s.0]),
                        buf: Vec::new(),
                        at: 0,
                    },
                    (None, Some(o)) => RdfsStored::SingleCol {
                        fixed: o,
                        fixed_is_subject: false,
                        scan: RangeCursor::new(Perm::Pos, &[member.0, o.0]),
                        buf: Vec::new(),
                        at: 0,
                    },
                    (None, None) => RdfsStored::Pairs {
                        scan: RangeCursor::new(Perm::Pso, &[member.0]),
                        buf: Vec::new(),
                        at: 0,
                    },
                };
                ReaderMode::RdfsMember {
                    stored,
                    s_arg: args[0],
                    m_arg: args[1],
                }
            }
            PfKind::StrSplit => {
                let (Some(s), Some(re_arg)) = (args[1], args[2]) else {
                    drop(args);
                    return Self::empty(
                        spec.graph.clone(),
                        same_var_pairs,
                        out_slot,
                        row_buf,
                        charge,
                    );
                };
                let decode = ctx.charge(0)?;
                let (Some(Term::Literal(text_lit)), Some(Term::Literal(pat_lit))) = (
                    ctx.term_checked_into(s, &decode)?,
                    ctx.term_checked_into(re_arg, &decode)?,
                ) else {
                    drop(decode);
                    drop(args);
                    return Self::empty(
                        spec.graph.clone(),
                        same_var_pairs,
                        out_slot,
                        row_buf,
                        charge,
                    );
                };
                ctx.check()?;
                let text_len = text_lit.value().len() as u64;
                let with_text = open_base.saturating_add(text_len);
                charge.resize(with_text)?;
                let (program, cache, regex_bytes) =
                    compile_split_regex(ctx, pat_lit.value(), &mut charge, with_text, &bad)?;
                let mut text = String::new();
                text.reserve_exact(text_lit.value().len());
                text.push_str(text_lit.value());
                drop(text_lit);
                drop(pat_lit);
                drop(decode);
                let mut iter = StrSplitIter::new(program, cache, regex_bytes, text);
                let box_bytes = std::mem::size_of::<StrSplitIter>() as u64;
                charge.resize(
                    open_base
                        .saturating_add(iter.bytes())
                        .saturating_add(box_bytes),
                )?;
                match args[0] {
                    None => ReaderMode::StrSplit {
                        iter: Box::new(iter),
                        s,
                        re: re_arg,
                    },
                    Some(x) => {
                        let tok_decode = ctx.charge(0)?;
                        let tok_val = ctx.value_checked_into(x, &tok_decode)?;
                        let matched = match tok_val {
                            Some(Value::Str(v)) => {
                                let mut matched = false;
                                let mut checked = 0usize;
                                while let Some(tok) = iter.next_token(ctx)? {
                                    checked += 1;
                                    if checked.is_multiple_of(1024) {
                                        ctx.check()?;
                                    }
                                    if *tok == *v {
                                        matched = true;
                                        break;
                                    }
                                }
                                drop(v);
                                matched
                            }
                            other => {
                                drop(other);
                                false
                            }
                        };
                        drop(tok_decode);
                        drop(iter);
                        charge.resize(open_base)?;
                        if matched {
                            ReaderMode::Single(Some(Solution::triple(x, s, re_arg)))
                        } else {
                            ReaderMode::Done
                        }
                    }
                }
            }
            PfKind::Concat => {
                let mut total_len = 0usize;
                let decode = ctx.charge(0)?;
                for (idx, a) in args[1..].iter().enumerate() {
                    if idx.is_multiple_of(1024) {
                        ctx.check()?;
                    }
                    let Some(a) = a else {
                        drop(decode);
                        drop(args);
                        return Self::empty(
                            spec.graph.clone(),
                            same_var_pairs,
                            out_slot,
                            row_buf,
                            charge,
                        );
                    };
                    let piece_len = match ctx.term_checked_into(*a, &decode)? {
                        Some(Term::NamedNode(n)) => n.as_str().len(),
                        Some(Term::Literal(l)) => l.value().len(),
                        _ => {
                            drop(decode);
                            drop(args);
                            return Self::empty(
                                spec.graph.clone(),
                                same_var_pairs,
                                out_slot,
                                row_buf,
                                charge,
                            );
                        }
                    };
                    decode.resize(0)?;
                    total_len = total_len
                        .checked_add(piece_len)
                        .ok_or_else(|| ctx.memory_exceeded(u64::MAX))?;
                }
                let key_len = total_len
                    .checked_add(2)
                    .ok_or_else(|| ctx.memory_exceeded(u64::MAX))?;
                charge.resize(open_base.saturating_add(key_len as u64))?;
                let v = {
                    let mut key = Vec::with_capacity(key_len);
                    key.push(b'"');
                    for (idx, a) in args[1..].iter().enumerate() {
                        if idx.is_multiple_of(1024) {
                            ctx.check()?;
                        }
                        match ctx.term_checked_into(a.expect("validated arg"), &decode)? {
                            Some(Term::NamedNode(n)) => {
                                key.extend_from_slice(n.as_str().as_bytes())
                            }
                            Some(Term::Literal(l)) => key.extend_from_slice(l.value().as_bytes()),
                            _ => unreachable!("validated above"),
                        }
                        decode.resize(0)?;
                    }
                    drop(decode);
                    key.push(crate::id::KEY_SEP);
                    debug_assert_eq!(key.len(), key_len);
                    ctx.intern_key_checked(&key)?
                };
                ctx.check()?;
                let sol_bytes =
                    (args.len() as u64).saturating_mul(std::mem::size_of::<Id>() as u64);
                charge.resize(open_base.saturating_add(sol_bytes))?;
                match args[0] {
                    None => {
                        let mut sol = Vec::with_capacity(args.len());
                        sol.push(v);
                        sol.extend(args[1..].iter().flatten().copied());
                        ReaderMode::Single(Some(Solution::Wide(sol)))
                    }
                    Some(x) if x == v => {
                        let mut sol = Vec::with_capacity(args.len());
                        sol.extend(args.iter().flatten().copied());
                        ReaderMode::Single(Some(Solution::Wide(sol)))
                    }
                    Some(_) => ReaderMode::Done,
                }
            }
            PfKind::Str => {
                let decode = ctx.charge(0)?;
                if let Some(s) = args[0]
                    && !matches!(ctx.term_checked_into(s, &decode)?, Some(Term::Literal(_)))
                {
                    drop(decode);
                    drop(args);
                    return Self::empty(
                        spec.graph.clone(),
                        same_var_pairs,
                        out_slot,
                        row_buf,
                        charge,
                    );
                }
                decode.resize(0)?;
                let Some(o) = args[1] else {
                    return Err(bad("the object is an unbound variable"));
                };
                let v = match ctx.term_checked_into(o, &decode)? {
                    Some(Term::NamedNode(n)) => string(n.as_str())?,
                    Some(Term::Literal(l)) => string(l.value())?,
                    _ => {
                        return Err(bad("the object is a blank node"));
                    }
                };
                drop(decode);
                ctx.check()?;
                match args[0] {
                    None => ReaderMode::Single(Some(Solution::pair(v, o))),
                    Some(s) if s == v => ReaderMode::Single(Some(Solution::pair(s, o))),
                    Some(_) => ReaderMode::Done,
                }
            }
            PfKind::SplitIri => {
                let Some(s) = args[0] else {
                    drop(args);
                    return Self::empty(
                        spec.graph.clone(),
                        same_var_pairs,
                        out_slot,
                        row_buf,
                        charge,
                    );
                };
                let decode = ctx.charge(0)?;
                let Some(Term::NamedNode(iri)) = ctx.term_checked_into(s, &decode)? else {
                    drop(decode);
                    drop(args);
                    return Self::empty(
                        spec.graph.clone(),
                        same_var_pairs,
                        out_slot,
                        row_buf,
                        charge,
                    );
                };
                let at = super::fnlib::split_xml(iri.as_str());
                let (ns, local) = iri.as_str().split_at(at);
                let nsv = match args[1] {
                    None => ctx.intern_iri_checked(ns)?,
                    Some(x) => {
                        let arg_decode = ctx.charge(0)?;
                        let ok = {
                            let term = ctx.term_checked_into(x, &arg_decode)?;
                            match &term {
                                Some(Term::NamedNode(n)) => n.as_str() == ns,
                                Some(Term::Literal(l))
                                    if l.datatype() == xsd::STRING || l.language().is_some() =>
                                {
                                    l.value() == ns
                                }
                                _ => false,
                            }
                        };
                        drop(arg_decode);
                        if !ok {
                            drop(iri);
                            drop(decode);
                            drop(args);
                            return Self::empty(
                                spec.graph.clone(),
                                same_var_pairs,
                                out_slot,
                                row_buf,
                                charge,
                            );
                        }
                        x
                    }
                };
                let lv = match args[2] {
                    None => string(local)?,
                    Some(x) => {
                        let arg_decode = ctx.charge(0)?;
                        let ok = {
                            let term = ctx.term_checked_into(x, &arg_decode)?;
                            matches!(
                                &term,
                                Some(Term::Literal(l))
                                    if (l.datatype() == xsd::STRING || l.language().is_some())
                                        && l.value() == local
                            )
                        };
                        drop(arg_decode);
                        if !ok {
                            drop(iri);
                            drop(decode);
                            drop(args);
                            return Self::empty(
                                spec.graph.clone(),
                                same_var_pairs,
                                out_slot,
                                row_buf,
                                charge,
                            );
                        }
                        x
                    }
                };
                drop(iri);
                drop(decode);
                ReaderMode::Single(Some(Solution::triple(s, nsv, lv)))
            }
            PfKind::Assign => match (args[0], args[1]) {
                (None, None) => return Err(bad("both the subject and the object are unbound")),
                (None, Some(o)) => ReaderMode::Single(Some(Solution::pair(o, o))),
                (Some(s), None) => ReaderMode::Single(Some(Solution::pair(s, s))),
                (Some(s), Some(o)) => {
                    let same = if s == o {
                        true
                    } else {
                        let decode = ctx.charge(0)?;
                        let a = ctx.term_checked_into(s, &decode)?;
                        let b = ctx.term_checked_into(o, &decode)?;
                        let eq = matches!((a, b), (Some(a), Some(b))
                            if super::cdt::same_value(&a, &b).unwrap_or(false));
                        drop(decode);
                        eq
                    };
                    if same {
                        ReaderMode::Single(Some(Solution::pair(s, o)))
                    } else {
                        ReaderMode::Done
                    }
                }
            },
            PfKind::BNode => {
                let Some(s) = args[0] else {
                    return Err(bad("the subject is an unbound variable"));
                };
                let decode = ctx.charge(0)?;
                let Some(Term::BlankNode(b)) = ctx.term_checked_into(s, &decode)? else {
                    drop(decode);
                    drop(args);
                    return Self::empty(
                        spec.graph.clone(),
                        same_var_pairs,
                        out_slot,
                        row_buf,
                        charge,
                    );
                };
                let label = string(b.as_str())?;
                drop(b);
                drop(decode);
                ctx.check()?;
                match args[1] {
                    None => ReaderMode::Single(Some(Solution::pair(s, label))),
                    Some(o) if o == label => ReaderMode::Single(Some(Solution::pair(s, o))),
                    Some(_) => ReaderMode::Done,
                }
            }
            PfKind::Version => {
                let subject = ctx.intern_iri_checked(VERSION_SUBJECT)?;
                let version = string(env!("CARGO_PKG_VERSION"))?;
                ctx.check()?;
                match (args[0], args[1]) {
                    (s, o) if s.is_none_or(|s| s == subject) && o.is_none_or(|o| o == version) => {
                        ReaderMode::Single(Some(Solution::pair(subject, version)))
                    }
                    _ => ReaderMode::Done,
                }
            }
        };

        drop(args);
        let mut reader = Self {
            graph: spec.graph.clone(),
            same_var_pairs,
            out_slot,
            row_buf,
            ids,
            numbers: FxHashMap::default(),
            mode,
            charge,
        };
        reader.sync_charge(0)?;
        Ok(reader)
    }

    fn empty(
        graph: GraphFilter,
        same_var_pairs: Vec<(usize, usize)>,
        out_slot: Vec<usize>,
        row_buf: Vec<Id>,
        charge: super::ctx::OwnedCharge,
    ) -> Result<Self> {
        let mut reader = Self {
            graph,
            same_var_pairs,
            out_slot,
            row_buf,
            ids: None,
            numbers: FxHashMap::default(),
            mode: ReaderMode::Done,
            charge,
        };
        reader.sync_charge(0)?;
        Ok(reader)
    }

    fn fixed_meta_bytes(&self) -> u64 {
        (self.same_var_pairs.capacity() as u64)
            .saturating_mul(std::mem::size_of::<(usize, usize)>() as u64)
            .saturating_add(
                (self.out_slot.capacity() as u64)
                    .saturating_mul(std::mem::size_of::<usize>() as u64),
            )
            .saturating_add(
                (self.row_buf.capacity() as u64).saturating_mul(std::mem::size_of::<Id>() as u64),
            )
    }

    fn base_fixed_bytes(&self) -> u64 {
        self.fixed_meta_bytes()
            .saturating_add(numbers_map_bytes(self.numbers.capacity()))
    }

    fn base_bytes(&self) -> u64 {
        let fixed = self.base_fixed_bytes();
        let mode = match &self.mode {
            ReaderMode::Done => 0,
            ReaderMode::Single(slot) => match slot {
                Some(Solution::Wide(v)) => {
                    (v.capacity() as u64).saturating_mul(std::mem::size_of::<Id>() as u64)
                }
                Some(Solution::Fixed(..)) | None => 0,
            },
            ReaderMode::StrSplit { iter, .. } => iter
                .bytes()
                .saturating_add(std::mem::size_of::<StrSplitIter>() as u64),
            ReaderMode::MemberBound { walk, .. } | ReaderMode::IndexBound { walk, .. } => {
                walk.bytes()
            }
            ReaderMode::MemberAll { heads, walk } => heads
                .bytes()
                .saturating_add(walk.as_ref().map_or(0, ListWalk::bytes)),
            ReaderMode::MemberByObject { buf, .. } => {
                (buf.capacity() as u64).saturating_mul(std::mem::size_of::<Id>() as u64)
            }
            ReaderMode::LengthAll { heads } => heads.bytes(),
            ReaderMode::IndexAll {
                heads,
                seen_heads,
                walk,
                ..
            } => {
                let h = match heads {
                    IndexHeads::All(s) => s.bytes(),
                    IndexHeads::ByMember { buf, .. } => {
                        (buf.capacity() as u64).saturating_mul(std::mem::size_of::<Id>() as u64)
                    }
                };
                h.saturating_add(hashset_id_bytes(seen_heads.capacity()))
                    .saturating_add(walk.as_ref().map_or(0, ListWalk::bytes))
            }
            ReaderMode::Container(c) => c.bytes(),
            ReaderMode::RdfsMember { stored, .. } => match stored {
                RdfsStored::Single(_) => 0,
                RdfsStored::SingleCol { buf, .. } => {
                    (buf.capacity() as u64).saturating_mul(std::mem::size_of::<Id>() as u64)
                }
                RdfsStored::Pairs { buf, .. } => {
                    (buf.capacity() as u64).saturating_mul(std::mem::size_of::<(Id, Id)>() as u64)
                }
            },
        };
        fixed.saturating_add(mode)
    }

    fn sync_charge(&mut self, extra: u64) -> Result<()> {
        let bytes = self.base_bytes().saturating_add(extra);
        self.charge.resize(bytes)
    }

    fn predicate_number(&mut self, ctx: &Ctx, p: Id) -> Result<Option<u64>> {
        if let Some(&cached) = self.numbers.get(&p) {
            return Ok(cached);
        }
        let growing = self.numbers.len() == self.numbers.capacity();
        if growing {
            // `sync_charge` already includes the old `numbers_map_bytes(cap)`, so adding
            // `numbers_map_next_bytes(cap)` covers both old and new tables during rehash.
            self.sync_charge(numbers_map_next_bytes(self.numbers.capacity()))?;
        }
        let decode = ctx.charge(0)?;
        let num = match ctx.term_checked_into(p, &decode)? {
            Some(Term::NamedNode(n)) => n
                .as_str()
                .strip_prefix(RDF_NS)
                .and_then(|l| l.strip_prefix('_'))
                .filter(|d| !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit()))
                .and_then(|d| d.parse().ok()),
            _ => None,
        };
        drop(decode);
        self.numbers.insert(p, num);
        if growing {
            self.sync_charge(0)?;
        }
        Ok(num)
    }

    fn push_solution(&mut self, sol: &[Id], out: &mut Table) -> bool {
        for &(j, k) in &self.same_var_pairs {
            if sol[j] != sol[k] {
                return false;
            }
        }
        for (c, &slot_idx) in self.out_slot.iter().enumerate() {
            self.row_buf[c] = sol[slot_idx];
        }
        out.push_row(&self.row_buf);
        true
    }

    /// Fill `out` with up to `cap` rows. Returns `true` once the reader is exhausted.
    pub(crate) fn next(&mut self, ctx: &Ctx, out: &mut Table, cap: usize) -> Result<bool> {
        let want = cap.max(1);
        let mut skipped = 0usize;
        while out.len < want {
            ctx.check()?;
            let sol = self.next_solution(ctx)?;
            let Some(sol) = sol else {
                self.sync_charge(0)?;
                return Ok(true);
            };
            if !self.push_solution(sol.as_slice(), out) {
                skipped += 1;
                if skipped.is_multiple_of(1024) {
                    ctx.check()?;
                }
            }
        }
        self.sync_charge(0)?;
        Ok(matches!(self.mode, ReaderMode::Done))
    }

    fn next_solution(&mut self, ctx: &Ctx) -> Result<Option<Solution>> {
        let mut spins = 0usize;
        loop {
            spins += 1;
            if spins.is_multiple_of(1024) {
                ctx.check()?;
            }
            match &mut self.mode {
                ReaderMode::Done => return Ok(None),
                ReaderMode::Single(slot) => {
                    let out = slot.take();
                    self.mode = ReaderMode::Done;
                    return Ok(out);
                }
                ReaderMode::StrSplit { iter, s, re } => {
                    let (s, re) = (*s, *re);
                    let Some(tok) = iter.next_token(ctx)? else {
                        self.mode = ReaderMode::Done;
                        return Ok(None);
                    };
                    let id = ctx.intern_simple_str_checked(tok)?;
                    return Ok(Some(Solution::triple(id, s, re)));
                }
                ReaderMode::MemberBound { .. } => {
                    let ids = self.ids.expect("list ids");
                    let base = self.base_fixed_bytes();
                    let (graph, charge) = (&self.graph, &mut self.charge);
                    let ReaderMode::MemberBound { walk, filter_m } = &mut self.mode else {
                        unreachable!()
                    };
                    let Some((_, m)) = walk.step(ctx, graph, &ids, &mut |walk_b| {
                        charge.resize(base.saturating_add(walk_b))
                    })?
                    else {
                        self.mode = ReaderMode::Done;
                        return Ok(None);
                    };
                    if filter_m.is_none_or(|want| want == m) {
                        return Ok(Some(Solution::pair(walk.head, m)));
                    }
                }
                ReaderMode::MemberAll { .. } => {
                    let ids = self.ids.expect("list ids");
                    let base = self.base_fixed_bytes();
                    let (graph, charge) = (&self.graph, &mut self.charge);
                    let ReaderMode::MemberAll { heads, walk } = &mut self.mode else {
                        unreachable!()
                    };
                    if let Some(w) = walk.as_mut() {
                        let hb = heads.bytes();
                        if let Some((_, m)) = w.step(ctx, graph, &ids, &mut |walk_b| {
                            charge.resize(base.saturating_add(hb).saturating_add(walk_b))
                        })? {
                            return Ok(Some(Solution::pair(w.head, m)));
                        }
                        *walk = None;
                        charge.resize(base.saturating_add(hb))?;
                    }
                    let Some(h) = heads.next_head(ctx, graph, &ids, &mut |buf_b| {
                        charge.resize(base.saturating_add(buf_b))
                    })?
                    else {
                        self.mode = ReaderMode::Done;
                        return Ok(None);
                    };
                    *walk = Some(ListWalk::new(ctx, graph, &ids, h)?);
                }
                ReaderMode::MemberByObject { .. } => {
                    let ids = self.ids.expect("list ids");
                    let base = self.base_fixed_bytes();
                    let (graph, charge) = (&self.graph, &mut self.charge);
                    let ReaderMode::MemberByObject { m, cells, buf, at } = &mut self.mode else {
                        unreachable!()
                    };
                    let m = *m;
                    if *at < buf.len() {
                        let cell = buf[*at];
                        *at += 1;
                        let buf_b = (buf.capacity() as u64)
                            .saturating_mul(std::mem::size_of::<Id>() as u64);
                        let head = head_of_cell(ctx, graph, &ids, cell, &mut |extra| {
                            charge.resize(base.saturating_add(buf_b).saturating_add(extra))
                        })?;
                        return Ok(Some(Solution::pair(head, m)));
                    }
                    if cells.is_done() {
                        self.mode = ReaderMode::Done;
                        return Ok(None);
                    }
                    ctx.check()?;
                    prepare_id_buf(buf, &mut |buf_b| charge.resize(base.saturating_add(buf_b)))?;
                    cells.next_ids(ctx, graph, 2, false, buf, RANGE_CHUNK)?;
                    *at = 0;
                }
                ReaderMode::LengthAll { .. } => {
                    let ids = self.ids.expect("list ids");
                    let base = self.base_fixed_bytes();
                    let (graph, charge) = (&self.graph, &mut self.charge);
                    let ReaderMode::LengthAll { heads } = &mut self.mode else {
                        unreachable!()
                    };
                    let Some(h) = heads.next_head(ctx, graph, &ids, &mut |buf_b| {
                        charge.resize(base.saturating_add(buf_b))
                    })?
                    else {
                        self.mode = ReaderMode::Done;
                        return Ok(None);
                    };
                    let hb = heads.bytes();
                    if let Some(len) = list_length(ctx, graph, &ids, h, &mut |extra| {
                        charge.resize(base.saturating_add(hb).saturating_add(extra))
                    })? {
                        let n = ctx.intern_value(&Value::Integer((len as i64).into()));
                        ctx.check()?;
                        return Ok(Some(Solution::pair(h, n)));
                    }
                }
                ReaderMode::IndexBound { .. } => {
                    let ids = self.ids.expect("list ids");
                    let base = self.base_fixed_bytes();
                    let (graph, charge) = (&self.graph, &mut self.charge);
                    let ReaderMode::IndexBound {
                        walk,
                        i_arg,
                        target_pos,
                        m_arg,
                    } = &mut self.mode
                    else {
                        unreachable!()
                    };
                    let (i_arg, target_pos, m_arg) = (*i_arg, *target_pos, *m_arg);
                    let Some((p, x)) = walk.step(ctx, graph, &ids, &mut |walk_b| {
                        charge.resize(base.saturating_add(walk_b))
                    })?
                    else {
                        self.mode = ReaderMode::Done;
                        return Ok(None);
                    };
                    match (target_pos, m_arg) {
                        (None, Some(m)) => {
                            if x == m {
                                let head = walk.head;
                                self.mode = ReaderMode::Done;
                                let pid = ctx.intern_value(&Value::Integer(p.into()));
                                ctx.check()?;
                                return Ok(Some(Solution::triple(head, pid, m)));
                            }
                        }
                        (Some(target), m) => {
                            if p == target {
                                let head = walk.head;
                                self.mode = ReaderMode::Done;
                                if m.is_none_or(|m| m == x) {
                                    return Ok(Some(Solution::triple(head, i_arg.unwrap(), x)));
                                }
                                return Ok(None);
                            }
                        }
                        (None, None) => {
                            let head = walk.head;
                            let pid = ctx.intern_value(&Value::Integer(p.into()));
                            ctx.check()?;
                            return Ok(Some(Solution::triple(head, pid, x)));
                        }
                    }
                }
                ReaderMode::IndexAll { .. } => {
                    let ids = self.ids.expect("list ids");
                    let base = self.base_fixed_bytes();
                    let (graph, charge) = (&self.graph, &mut self.charge);
                    let ReaderMode::IndexAll {
                        heads,
                        seen_heads,
                        walk,
                        i_arg,
                        target_pos,
                        m_arg,
                    } = &mut self.mode
                    else {
                        unreachable!()
                    };
                    let (i_arg, target_pos, m_arg) = (*i_arg, *target_pos, *m_arg);
                    if let Some(w) = walk.as_mut() {
                        let fixed = base
                            .saturating_add(match heads {
                                IndexHeads::All(s) => s.bytes(),
                                IndexHeads::ByMember { buf, .. } => (buf.capacity() as u64)
                                    .saturating_mul(std::mem::size_of::<Id>() as u64),
                            })
                            .saturating_add(hashset_id_bytes(seen_heads.capacity()));
                        match w.step(ctx, graph, &ids, &mut |walk_b| {
                            charge.resize(fixed.saturating_add(walk_b))
                        })? {
                            Some((p, x)) => match (target_pos, m_arg) {
                                (None, Some(m)) => {
                                    if x == m {
                                        let head = w.head;
                                        *walk = None;
                                        charge.resize(fixed)?;
                                        let pid = ctx.intern_value(&Value::Integer(p.into()));
                                        ctx.check()?;
                                        return Ok(Some(Solution::triple(head, pid, m)));
                                    }
                                    continue;
                                }
                                (Some(target), m) => {
                                    if p == target {
                                        let head = w.head;
                                        *walk = None;
                                        charge.resize(fixed)?;
                                        if m.is_none_or(|m| m == x) {
                                            return Ok(Some(Solution::triple(
                                                head,
                                                i_arg.unwrap(),
                                                x,
                                            )));
                                        }
                                    }
                                    continue;
                                }
                                (None, None) => {
                                    let head = w.head;
                                    let pid = ctx.intern_value(&Value::Integer(p.into()));
                                    ctx.check()?;
                                    return Ok(Some(Solution::triple(head, pid, x)));
                                }
                            },
                            None => {
                                *walk = None;
                                charge.resize(fixed)?;
                            }
                        }
                    }
                    // Advance to next unique head.
                    let seen_b = hashset_id_bytes(seen_heads.capacity());
                    let next_h = match heads {
                        IndexHeads::All(scan) => {
                            scan.next_head(ctx, graph, &ids, &mut |buf_b| {
                                charge.resize(base.saturating_add(seen_b).saturating_add(buf_b))
                            })?
                        }
                        IndexHeads::ByMember { cells, buf, at } => loop {
                            if *at < buf.len() {
                                let cell = buf[*at];
                                *at += 1;
                                let fixed = base
                                    .saturating_add(
                                        (buf.capacity() as u64)
                                            .saturating_mul(std::mem::size_of::<Id>() as u64),
                                    )
                                    .saturating_add(seen_b);
                                break Some(head_of_cell(ctx, graph, &ids, cell, &mut |ex| {
                                    charge.resize(fixed.saturating_add(ex))
                                })?);
                            }
                            if cells.is_done() {
                                break None;
                            }
                            ctx.check()?;
                            prepare_id_buf(buf, &mut |buf_b| {
                                charge.resize(base.saturating_add(seen_b).saturating_add(buf_b))
                            })?;
                            cells.next_ids(ctx, graph, 2, false, buf, RANGE_CHUNK)?;
                            *at = 0;
                        },
                    };
                    let Some(h) = next_h else {
                        self.mode = ReaderMode::Done;
                        return Ok(None);
                    };
                    if !seen_heads.contains(&h) {
                        let hb = match heads {
                            IndexHeads::All(s) => s.bytes(),
                            IndexHeads::ByMember { buf, .. } => (buf.capacity() as u64)
                                .saturating_mul(std::mem::size_of::<Id>() as u64),
                        };
                        if seen_heads.len() == seen_heads.capacity() {
                            charge
                                .resize(base.saturating_add(hb).saturating_add(
                                    hashset_id_growth_bytes(seen_heads.capacity()),
                                ))?;
                            seen_heads.insert(h);
                            charge.resize(
                                base.saturating_add(hb)
                                    .saturating_add(hashset_id_bytes(seen_heads.capacity())),
                            )?;
                        } else {
                            seen_heads.insert(h);
                        }
                        *walk = Some(ListWalk::new(ctx, graph, &ids, h)?);
                    }
                }
                ReaderMode::Container(_) => {
                    if let Some(sol) = self.step_container(ctx)? {
                        return Ok(Some(sol));
                    }
                    self.mode = ReaderMode::Done;
                    return Ok(None);
                }
                ReaderMode::RdfsMember { .. } => {
                    let base = self.base_fixed_bytes();
                    let (graph, charge) = (&self.graph, &mut self.charge);
                    let stored_sol = match &mut self.mode {
                        ReaderMode::RdfsMember { stored, .. } => match stored {
                            RdfsStored::Single(slot) => slot.take(),
                            RdfsStored::SingleCol {
                                fixed,
                                fixed_is_subject,
                                scan,
                                buf,
                                at,
                            } => loop {
                                if *at < buf.len() {
                                    let x = buf[*at];
                                    *at += 1;
                                    break Some(if *fixed_is_subject {
                                        (*fixed, x)
                                    } else {
                                        (x, *fixed)
                                    });
                                }
                                if scan.is_done() {
                                    break None;
                                }
                                ctx.check()?;
                                prepare_id_buf(buf, &mut |buf_b| {
                                    charge.resize(base.saturating_add(buf_b))
                                })?;
                                scan.next_ids(ctx, graph, 2, false, buf, RANGE_CHUNK)?;
                                *at = 0;
                            },
                            RdfsStored::Pairs { scan, buf, at } => loop {
                                if *at < buf.len() {
                                    let pair = buf[*at];
                                    *at += 1;
                                    break Some(pair);
                                }
                                if scan.is_done() {
                                    break None;
                                }
                                ctx.check()?;
                                prepare_pair_buf(buf, &mut |buf_b| {
                                    charge.resize(base.saturating_add(buf_b))
                                })?;
                                scan.next_pairs(ctx, graph, [1, 2], buf, RANGE_CHUNK)?;
                                *at = 0;
                            },
                        },
                        _ => unreachable!(),
                    };
                    if let Some((s, o)) = stored_sol {
                        return Ok(Some(Solution::pair(s, o)));
                    }
                    let ReaderMode::RdfsMember { s_arg, m_arg, .. } = self.mode else {
                        unreachable!()
                    };
                    self.mode = ReaderMode::Container(ContainerStream::new(None, s_arg, m_arg));
                    self.sync_charge(0)?;
                }
            }
        }
    }

    fn step_container(&mut self, ctx: &Ctx) -> Result<Option<Solution>> {
        let ids = self.ids.expect("container ids");
        loop {
            ctx.check()?;
            // First, drain the active container if any.
            let mut active_done = false;
            let has_current = matches!(
                &self.mode,
                ReaderMode::Container(ContainerStream {
                    current: Some(_),
                    ..
                })
            );
            if has_current {
                let is_bound = matches!(
                    &self.mode,
                    ReaderMode::Container(ContainerStream {
                        current: Some(OneContainer::BoundMember { .. }),
                        ..
                    })
                );
                if is_bound {
                    let mut candidate_p = None;
                    let base_without_cur = self.base_bytes().saturating_sub(match &self.mode {
                        ReaderMode::Container(stream) => {
                            stream.current.as_ref().map_or(0, OneContainer::bytes)
                        }
                        _ => 0,
                    });
                    let (graph, charge) = (&self.graph, &mut self.charge);
                    let (c, m) = match &mut self.mode {
                        ReaderMode::Container(ContainerStream {
                            current:
                                Some(OneContainer::BoundMember {
                                    c,
                                    m,
                                    scan,
                                    buf,
                                    at,
                                }),
                            ..
                        }) => {
                            let (c, m) = (*c, *m);
                            loop {
                                if *at < buf.len() {
                                    candidate_p = Some(buf[*at]);
                                    *at += 1;
                                    break;
                                }
                                if scan.is_done() {
                                    active_done = true;
                                    break;
                                }
                                ctx.check()?;
                                prepare_id_buf(buf, &mut |buf_b| {
                                    charge.resize(base_without_cur.saturating_add(buf_b))
                                })?;
                                scan.next_ids(ctx, graph, 2, false, buf, RANGE_CHUNK)?;
                                *at = 0;
                            }
                            (c, m)
                        }
                        _ => unreachable!(),
                    };
                    if let Some(p) = candidate_p {
                        if self.predicate_number(ctx, p)?.is_some() {
                            return Ok(Some(Solution::pair(c, m)));
                        }
                        continue;
                    }
                } else {
                    // UnboundMember: scan `Perm::Spo [c]` into `by_number`, then pop_first.
                    loop {
                        let base_now = self.base_bytes();
                        let (graph, charge) = (&self.graph, &mut self.charge);
                        let next_pair = match &mut self.mode {
                            ReaderMode::Container(ContainerStream {
                                current: Some(OneContainer::UnboundMember { scan, pair_buf, .. }),
                                ..
                            }) => {
                                if pair_buf.is_empty() && !scan.is_done() {
                                    ctx.check()?;
                                    let cur_pair_b = (pair_buf.capacity() as u64)
                                        .saturating_mul(std::mem::size_of::<(Id, Id)>() as u64);
                                    let base_without_pair = base_now.saturating_sub(cur_pair_b);
                                    prepare_pair_buf(pair_buf, &mut |buf_b| {
                                        charge.resize(base_without_pair.saturating_add(buf_b))
                                    })?;
                                    scan.next_pairs(ctx, graph, [1, 2], pair_buf, RANGE_CHUNK)?;
                                    pair_buf.reverse();
                                }
                                pair_buf.pop()
                            }
                            _ => unreachable!(),
                        };
                        let Some((p, o)) = next_pair else {
                            break;
                        };
                        ctx.check()?;
                        if let Some(n) = self.predicate_number(ctx, p)? {
                            let (already, cur_len, cur_alloc) = match &self.mode {
                                ReaderMode::Container(ContainerStream {
                                    current:
                                        Some(OneContainer::UnboundMember {
                                            by_number,
                                            map_allocated,
                                            ..
                                        }),
                                    ..
                                }) => (by_number.contains_key(&n), by_number.len(), *map_allocated),
                                _ => unreachable!(),
                            };
                            if !already {
                                let cur_b = btree_map_bytes(cur_len, cur_alloc);
                                let peak_b = btree_map_bytes(cur_len + 1, true)
                                    .max(cur_b.saturating_add(288));
                                self.sync_charge(peak_b.saturating_sub(cur_b))?;
                            }
                            if let ReaderMode::Container(ContainerStream {
                                current:
                                    Some(OneContainer::UnboundMember {
                                        by_number,
                                        map_allocated,
                                        ..
                                    }),
                                ..
                            }) = &mut self.mode
                            {
                                *map_allocated = true;
                                by_number.insert(n, o);
                            }
                            if !already {
                                self.sync_charge(0)?;
                            }
                        }
                    }
                    let (popped, now_empty) = match &mut self.mode {
                        ReaderMode::Container(ContainerStream {
                            current: Some(OneContainer::UnboundMember { c, by_number, .. }),
                            ..
                        }) => {
                            let popped = by_number.pop_first().map(|(_, o)| (*c, o));
                            (popped, by_number.is_empty())
                        }
                        _ => unreachable!(),
                    };
                    if now_empty {
                        if let ReaderMode::Container(stream) = &mut self.mode {
                            stream.current = None;
                        }
                        self.sync_charge(0)?;
                    }
                    if let Some((c, o)) = popped {
                        return Ok(Some(Solution::pair(c, o)));
                    }
                    active_done = true;
                }
                if active_done {
                    if let ReaderMode::Container(stream) = &mut self.mode {
                        stream.current = None;
                    }
                    self.sync_charge(0)?;
                }
            }

            // Advance to next candidate container.
            let next_c = {
                let base_fixed = self.base_fixed_bytes();
                let (graph, mode, charge) = (&self.graph, &mut self.mode, &mut self.charge);
                let ReaderMode::Container(stream) = mode else {
                    unreachable!()
                };
                if let Some(c) = stream.bound_c.take() {
                    if ids.is_container(ctx, graph, c, stream.ty)? {
                        Some((c, stream.m))
                    } else {
                        None
                    }
                } else {
                    match &mut stream.candidates {
                        ContainerCandidates::Done => None,
                        ContainerCandidates::ByType {
                            ty_idx,
                            scan,
                            buf,
                            at,
                            seen,
                        } => loop {
                            if *at < buf.len() {
                                let c = buf[*at];
                                *at += 1;
                                if !seen.contains(&c) {
                                    let buf_b = (buf.capacity() as u64)
                                        .saturating_mul(std::mem::size_of::<Id>() as u64);
                                    if seen.len() == seen.capacity() {
                                        charge.resize(
                                            base_fixed.saturating_add(buf_b).saturating_add(
                                                hashset_id_growth_bytes(seen.capacity()),
                                            ),
                                        )?;
                                        seen.insert(c);
                                        charge.resize(
                                            base_fixed
                                                .saturating_add(buf_b)
                                                .saturating_add(hashset_id_bytes(seen.capacity())),
                                        )?;
                                    } else {
                                        seen.insert(c);
                                    }
                                    break Some((c, stream.m));
                                }
                                continue;
                            }
                            if let Some(s) = scan.as_mut()
                                && !s.is_done()
                            {
                                ctx.check()?;
                                let seen_b = hashset_id_bytes(seen.capacity());
                                prepare_id_buf(buf, &mut |buf_b| {
                                    charge.resize(
                                        base_fixed.saturating_add(seen_b).saturating_add(buf_b),
                                    )
                                })?;
                                s.next_ids(ctx, graph, 2, false, buf, RANGE_CHUNK)?;
                                *at = 0;
                                continue;
                            }
                            *scan = None;
                            while *ty_idx < ContainerType::ALL.len()
                                && stream.ty.is_some_and(|x| x != ContainerType::ALL[*ty_idx])
                            {
                                *ty_idx += 1;
                            }
                            if *ty_idx >= ContainerType::ALL.len() {
                                break None;
                            }
                            let t = ids.container_types[*ty_idx];
                            *ty_idx += 1;
                            *scan = Some(RangeCursor::new(Perm::Pos, &[ids.rdf_type.0, t.0]));
                        },
                        ContainerCandidates::ByMember { scan, buf, at } => loop {
                            if *at < buf.len() {
                                let c = buf[*at];
                                *at += 1;
                                if ids.is_container(ctx, graph, c, stream.ty)? {
                                    break Some((c, stream.m));
                                }
                                continue;
                            }
                            if scan.is_done() {
                                break None;
                            }
                            ctx.check()?;
                            prepare_id_buf(buf, &mut |buf_b| {
                                charge.resize(base_fixed.saturating_add(buf_b))
                            })?;
                            scan.next_ids(ctx, graph, 1, true, buf, RANGE_CHUNK)?;
                            *at = 0;
                        },
                    }
                }
            };
            let Some((c, m)) = next_c else {
                return Ok(None);
            };
            if let ReaderMode::Container(stream) = &mut self.mode {
                stream.current = Some(OneContainer::new(c, m));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{Store, StoreOptions};
    use std::sync::Arc;

    fn test_ctx(limit: u64) -> Arc<Ctx> {
        let store = Store::in_memory(StoreOptions::default());
        let mut ctx = Ctx::new(store.snapshot());
        ctx.mem_limit = limit;
        ctx.enable_cursor_accounting().unwrap();
        let ctx = Arc::new(ctx);
        ctx.attach_owner();
        ctx
    }

    /// Streaming [`StrSplitIter`] agrees verbatim with [`java_split`] across edge cases:
    /// empty inputs, whitespace-only segments, leading/trailing delimiters, nullable and
    /// zero-width regexes, and multi-byte UTF-8 following positive-width matches.
    #[test]
    fn str_split_iter_matches_java_split() {
        let cases = [
            ("", ","),
            ("a,b,c", ","),
            ("a b c", " "),
            (",a,b,", ","),
            (",,,", ","),
            (" , , ", ","),
            ("a,,b,,", ","),
            ("  a , b  ,  ", ","),
            ("ab", ""),
            ("αβγ", ""),
            ("a1b22c333", r"\d+"),
            ("123", r"\d+"),
            ("foo", "^"),
            ("foo", "$"),
            ("a|b|c|", r"\|"),
            // Nullable regexes (adjacent positive-width and zero-width matches)
            ("ab", "a*"),
            ("a", "a*"),
            ("aba", "a*"),
            ("a,b", ",*"),
            ("xya", "y*"),
            ("1a2", r"\d*"),
            ("aαb", "a*"),
            ("aαb", "a?"),
            ("ab", "a?"),
            ("aba", "a|"),
            ("a\nb", "(?m)^"),
        ];
        for (s, pat) in cases {
            let re = regex::Regex::new(pat).unwrap();
            let expected = java_split(&re, s);
            let meta = regex_automata::meta::Regex::new(pat).unwrap();
            let cache = meta.create_cache();
            let mut iter = StrSplitIter::new(meta, cache, 0, s.to_string());
            let mut got = Vec::new();
            while let Some(tok) = iter.next_token_checked(|| Ok(())).unwrap() {
                got.push(tok.to_string());
            }
            assert_eq!(got, expected, "s={s:?} pat={pat:?}");
        }
    }

    /// `apf:strSplit` pre-admits its compiled regex against the remaining memory grant so
    /// a large pattern fails with `BudgetKind::Memory` before exceeding the grant.
    #[test]
    fn str_split_regex_refuses_when_program_exceeds_remaining_grant() {
        let ctx = test_ctx(64 << 10);
        let text = ctx.intern_value(&Value::Str("abcdef".into()));
        // A pattern with large repetition whose compiled NFA exceeds 64 KiB budget.
        let pat = ctx.intern_value(&Value::Str("[a-z]{400}".into()));
        let var = ctx.var("t");
        let spec = PfSpec {
            kind: PfKind::StrSplit,
            slots: vec![PathEnd::Var(var), PathEnd::Const(text), PathEnd::Const(pat)],
            inputs: Default::default(),
            graph: GraphFilter::Default,
        };
        let err = LeafReader::open(&ctx, &spec, &[var]).err();
        assert!(
            matches!(
                err,
                Some(Error::BudgetExceeded(b)) if b.kind == crate::BudgetKind::Memory
            ),
            "expected Memory BudgetExceeded, got {err:?}"
        );
    }

    /// `apf:bnode` on a constant blank-node subject yields its label when the object is
    /// unbound or matches, and yields nothing when the object disagrees.
    #[test]
    fn leaf_reader_bnode_handles_blank_node_subject() {
        let ctx = test_ctx(16 << 20);
        let bnode = ctx.intern_term(&Term::BlankNode(oxrdf::BlankNode::new("leafbn42").unwrap()));
        let Some(Term::BlankNode(expected_bn)) = ctx.term(bnode) else {
            panic!("expected interned blank node");
        };
        let expected_label = expected_bn.as_str().to_string();
        let label_var = ctx.var("label");
        let spec = PfSpec {
            kind: PfKind::BNode,
            slots: vec![PathEnd::Const(bnode), PathEnd::Var(label_var)],
            inputs: Default::default(),
            graph: GraphFilter::Default,
        };
        let mut reader = LeafReader::open(&ctx, &spec, &[label_var]).unwrap();
        let mut out = Table::new(vec![label_var]);
        assert!(reader.next(&ctx, &mut out, 8).unwrap());
        assert_eq!(out.len(), 1);
        assert!(matches!(
            ctx.value(out.cols[0][0]),
            Some(Value::Str(s)) if &*s == expected_label.as_str()
        ));

        let matching = ctx.intern_value(&Value::Str(expected_label.as_str().into()));
        let spec_match = PfSpec {
            kind: PfKind::BNode,
            slots: vec![PathEnd::Const(bnode), PathEnd::Const(matching)],
            inputs: Default::default(),
            graph: GraphFilter::Default,
        };
        let mut reader = LeafReader::open(&ctx, &spec_match, &[]).unwrap();
        let mut out = Table::new(vec![]);
        assert!(reader.next(&ctx, &mut out, 8).unwrap());
        assert_eq!(out.len(), 1);

        let wrong = ctx.intern_value(&Value::Str("other".into()));
        let spec_miss = PfSpec {
            kind: PfKind::BNode,
            slots: vec![PathEnd::Const(bnode), PathEnd::Const(wrong)],
            inputs: Default::default(),
            graph: GraphFilter::Default,
        };
        let mut reader = LeafReader::open(&ctx, &spec_miss, &[]).unwrap();
        let mut out = Table::new(vec![]);
        assert!(reader.next(&ctx, &mut out, 8).unwrap());
        assert_eq!(out.len(), 0);
    }

    /// `LeafReader` pre-admits scan chunks (`ID_BUF_BYTES`), container `BTreeMap` first
    /// node insertion, bound-subject `apf:strSplit` text, and `apf:concat` aggregate text
    /// before allocating them.
    #[test]
    fn leaf_reader_pre_admits_chunk_container_and_string_buffers() {
        // 1. Scan chunk pre-admission on a one-row pull of `list:member ?l ?m`.
        {
            let store = Store::in_memory(StoreOptions::default());
            store
                .load(&[crate::io::Source::from_bytes(
                    b"<urn:l> <http://www.w3.org/1999/02/22-rdf-syntax-ns#first> 1 ; \
                      <http://www.w3.org/1999/02/22-rdf-syntax-ns#rest> <http://www.w3.org/1999/02/22-rdf-syntax-ns#nil> ."
                        .to_vec(),
                    oxrdfio::RdfFormat::Turtle,
                    None,
                )])
                .unwrap();
            let mut raw = Ctx::new(store.snapshot());
            raw.mem_limit = 16 << 20;
            raw.enable_cursor_accounting().unwrap();
            let ctx = Arc::new(raw);
            ctx.attach_owner();
            let l = ctx.var("l");
            let m = ctx.var("m");
            let spec = PfSpec {
                kind: PfKind::Member,
                slots: vec![PathEnd::Var(l), PathEnd::Var(m)],
                inputs: Default::default(),
                graph: GraphFilter::Default,
            };
            let mut reader = LeafReader::open(&ctx, &spec, &[l, m]).unwrap();
            let before = ctx.memory().live.load(std::sync::atomic::Ordering::Relaxed);
            // Fill the remaining budget except 512 B (< ID_BUF_BYTES = 2048 B).
            let hold = super::super::ctx::OwnedCharge::new(
                &ctx,
                ctx.memory_remaining().saturating_sub(512),
            )
            .unwrap();
            let mut out = Table::new(vec![l, m]);
            let err = reader.next(&ctx, &mut out, 1).err();
            assert!(
                matches!(
                    err,
                    Some(Error::BudgetExceeded(b)) if b.kind == crate::BudgetKind::Memory
                ),
                "expected scan chunk refusal, got {err:?} (before={before})"
            );
            drop(hold);
        }

        // 2. First container numbered-member BTreeMap node pre-admission.
        {
            let store = Store::in_memory(StoreOptions::default());
            store
                .load(&[crate::io::Source::from_bytes(
                    b"<urn:b> a <http://www.w3.org/1999/02/22-rdf-syntax-ns#Bag> ; \
                      <http://www.w3.org/1999/02/22-rdf-syntax-ns#_1> 1 ."
                        .to_vec(),
                    oxrdfio::RdfFormat::Turtle,
                    None,
                )])
                .unwrap();
            let mut raw = Ctx::new(store.snapshot());
            raw.mem_limit = 16 << 20;
            raw.enable_cursor_accounting().unwrap();
            let ctx = Arc::new(raw);
            ctx.attach_owner();
            let bag = ctx.intern_term(&Term::NamedNode(oxrdf::NamedNode::new("urn:b").unwrap()));
            let m = ctx.var("m");
            let spec = PfSpec {
                kind: PfKind::Container(None),
                slots: vec![PathEnd::Const(bag), PathEnd::Var(m)],
                inputs: Default::default(),
                graph: GraphFilter::Default,
            };
            let mut reader = LeafReader::open(&ctx, &spec, &[m]).unwrap();
            // Leave room for pair_buf (4096 B) + numbers map (256 B), but not the first
            // BTreeMap leaf node (288 B).
            let leave = PAIR_BUF_BYTES + 256 + 64;
            let hold = super::super::ctx::OwnedCharge::new(
                &ctx,
                ctx.memory_remaining().saturating_sub(leave),
            )
            .unwrap();
            let mut out = Table::new(vec![m]);
            let err = reader.next(&ctx, &mut out, 1).err();
            assert!(
                matches!(
                    err,
                    Some(Error::BudgetExceeded(b)) if b.kind == crate::BudgetKind::Memory
                ),
                "expected first container node refusal, got {err:?}"
            );
            drop(hold);
        }

        // 3. Bound-subject `apf:strSplit` charges `text` before allocating it.
        {
            let ctx = test_ctx(16 << 20);
            let target = ctx.intern_value(&Value::Str("hit".into()));
            let big = ctx.intern_value(&Value::Str("a,".repeat(40_000).into()));
            let pat = ctx.intern_value(&Value::Str(",".into()));
            let spec = PfSpec {
                kind: PfKind::StrSplit,
                slots: vec![
                    PathEnd::Const(target),
                    PathEnd::Const(big),
                    PathEnd::Const(pat),
                ],
                inputs: Default::default(),
                graph: GraphFilter::Default,
            };
            let hold = super::super::ctx::OwnedCharge::new(
                &ctx,
                ctx.memory_remaining().saturating_sub(4096),
            )
            .unwrap();
            let err = LeafReader::open(&ctx, &spec, &[]).err();
            assert!(
                matches!(
                    err,
                    Some(Error::BudgetExceeded(b)) if b.kind == crate::BudgetKind::Memory
                ),
                "expected bound-subject strSplit text refusal, got {err:?}"
            );
            drop(hold);
        }

        // 4. `apf:concat` pre-admits aggregate string before allocating it.
        {
            let ctx = test_ctx(16 << 20);
            let piece = ctx.intern_value(&Value::Str("x".repeat(4096).into()));
            let var = ctx.var("s");
            let mut slots = vec![PathEnd::Var(var)];
            for _ in 0..20 {
                slots.push(PathEnd::Const(piece));
            }
            let spec = PfSpec {
                kind: PfKind::Concat,
                slots,
                inputs: Default::default(),
                graph: GraphFilter::Default,
            };
            let hold = super::super::ctx::OwnedCharge::new(
                &ctx,
                ctx.memory_remaining().saturating_sub(8192),
            )
            .unwrap();
            let err = LeafReader::open(&ctx, &spec, &[var]).err();
            assert!(
                matches!(
                    err,
                    Some(Error::BudgetExceeded(b)) if b.kind == crate::BudgetKind::Memory
                ),
                "expected concat string refusal, got {err:?}"
            );
            drop(hold);
        }
    }

    /// `LeafReader::open` pre-admits duplicate-slot metadata cleanly using exact-capacity
    /// vectors with zero hash-table or vector regrowth overhead.
    #[test]
    fn leaf_reader_open_meta_pre_admits_duplicate_slots() {
        let ctx = test_ctx(16 << 20);
        let m = ctx.var("m");
        let spec = PfSpec {
            kind: PfKind::Member,
            slots: vec![PathEnd::Var(m), PathEnd::Var(m)],
            inputs: Default::default(),
            graph: GraphFilter::Default,
        };
        // Peak for `?m list:member ?m`: args (32 B) + var_slots (32 B) + same_var_pairs (16 B) = 80 B.
        let hold =
            super::super::ctx::OwnedCharge::new(&ctx, ctx.memory_remaining().saturating_sub(64))
                .unwrap();
        let err = LeafReader::open(&ctx, &spec, &[m]).err();
        assert!(
            matches!(
                err,
                Some(Error::BudgetExceeded(b)) if b.kind == crate::BudgetKind::Memory
            ),
            "expected duplicate-slot metadata refusal under 64 B, got {err:?}"
        );
        drop(hold);

        // With 16 KiB headroom, open succeeds and keeps exact-capacity `same_var_pairs`.
        let reader = LeafReader::open(&ctx, &spec, &[m]).unwrap();
        assert_eq!(reader.same_var_pairs, vec![(0, 1)]);
        assert_eq!(reader.same_var_pairs.capacity(), 1);
    }

    /// Retained hash table estimates cover control bytes for the smallest tables, and
    /// `BTreeMap` empty-root charge + immediate drop prevent uncharged root leaves.
    #[test]
    fn retained_hash_and_btree_estimates_cover_small_tables_and_empty_root() {
        // hashbrown cap=3 has 4 buckets + 20 control bytes on 16-byte SIMD targets:
        // Id (8 B): 32 + 20 = 52 B; (Id, Option<u64>) (24 B): 96 + 20 = 116 B.
        assert!(hashset_id_bytes(3) >= 52);
        assert!(numbers_map_bytes(3) >= 116);
        assert!(btree_map_bytes(0, true) >= 192);
        assert_eq!(btree_map_bytes(0, false), 0);

        let store = Store::in_memory(StoreOptions::default());
        store
            .load(&[crate::io::Source::from_bytes(
                b"<urn:b> a <http://www.w3.org/1999/02/22-rdf-syntax-ns#Bag> ; \
                  <http://www.w3.org/1999/02/22-rdf-syntax-ns#_1> 1 ."
                    .to_vec(),
                oxrdfio::RdfFormat::Turtle,
                None,
            )])
            .unwrap();
        let mut raw = Ctx::new(store.snapshot());
        raw.mem_limit = 16 << 20;
        raw.enable_cursor_accounting().unwrap();
        let ctx = Arc::new(raw);
        ctx.attach_owner();
        let bag = ctx.intern_term(&Term::NamedNode(oxrdf::NamedNode::new("urn:b").unwrap()));
        let m = ctx.var("m");
        let spec = PfSpec {
            kind: PfKind::Container(None),
            slots: vec![PathEnd::Const(bag), PathEnd::Var(m)],
            inputs: Default::default(),
            graph: GraphFilter::Default,
        };
        let mut reader = LeafReader::open(&ctx, &spec, &[m]).unwrap();
        let mut out = Table::new(vec![m]);
        assert!(!reader.next(&ctx, &mut out, 1).unwrap());
        assert_eq!(out.len(), 1);
        // Popping the last member drops `stream.current` immediately so no empty BTreeMap
        // root leaf remains live uncharged across the pull.
        assert!(matches!(
            &reader.mode,
            ReaderMode::Container(ContainerStream { current: None, .. })
        ));
        assert_eq!(reader.charge.bytes(), reader.base_bytes());
    }

    /// `apf:concat` and `apf:strSplit` pre-admit interning scratch and retained local
    /// vocabulary storage instead of allocating uncharged `Arc<str>` / `Literal` copies.
    #[test]
    fn leaf_reader_concat_and_split_pre_admit_interning_peak() {
        // 1. `apf:concat` repeating a small 64-byte literal 1,000 times (total = 64,000 B):
        // use a 256 KiB context (step = 4 KiB) and leave 100 KiB so key assembly (~80 KiB
        // with metadata) succeeds while retaining the 64,098 B local-vocabulary entry
        // refuses cleanly on the post-assembly peak.
        {
            let ctx = test_ctx(256 << 10);
            let piece = ctx.intern_value(&Value::Str("a".repeat(64).into()));
            let var = ctx.var("s");
            let mut slots = vec![PathEnd::Var(var)];
            for _ in 0..1_000 {
                slots.push(PathEnd::Const(piece));
            }
            let spec = PfSpec {
                kind: PfKind::Concat,
                slots,
                inputs: Default::default(),
                graph: GraphFilter::Default,
            };
            let hold = super::super::ctx::OwnedCharge::new(
                &ctx,
                ctx.memory_remaining().saturating_sub(100 << 10),
            )
            .unwrap();
            let err = LeafReader::open(&ctx, &spec, &[var]).err();
            assert!(
                matches!(
                    err,
                    Some(Error::BudgetExceeded(b)) if b.kind == crate::BudgetKind::Memory
                ),
                "expected concat interning memory refusal, got {err:?}"
            );
            drop(hold);

            // With ample headroom, concat succeeds and yields the exact 64,000-byte string.
            let ok_ctx = test_ctx(16 << 20);
            let ok_piece = ok_ctx.intern_value(&Value::Str("a".repeat(64).into()));
            let ok_var = ok_ctx.var("s");
            let mut ok_slots = vec![PathEnd::Var(ok_var)];
            for _ in 0..1_000 {
                ok_slots.push(PathEnd::Const(ok_piece));
            }
            let ok_spec = PfSpec {
                kind: PfKind::Concat,
                slots: ok_slots,
                inputs: Default::default(),
                graph: GraphFilter::Default,
            };
            let mut reader = LeafReader::open(&ok_ctx, &ok_spec, &[ok_var]).unwrap();
            let mut out = Table::new(vec![ok_var]);
            assert!(reader.next(&ok_ctx, &mut out, 4).unwrap());
            assert_eq!(out.len(), 1);
            assert!(matches!(
                ok_ctx.value(out.cols[0][0]),
                Some(Value::Str(s)) if s.len() == 64_000
            ));
        }

        // 2. Streamed `apf:strSplit` pre-admits token key scratch before interning each
        // yielded token.
        {
            let ctx = test_ctx(16 << 20);
            let big = ctx.intern_value(&Value::Str("z".repeat(64 << 10).into()));
            let pat = ctx.intern_value(&Value::Str(",".into()));
            let var = ctx.var("t");
            let spec = PfSpec {
                kind: PfKind::StrSplit,
                slots: vec![PathEnd::Var(var), PathEnd::Const(big), PathEnd::Const(pat)],
                inputs: Default::default(),
                graph: GraphFilter::Default,
            };
            let mut reader = LeafReader::open(&ctx, &spec, &[var]).unwrap();
            // Leave only 16 KiB (< 64 KiB token key scratch) before pulling the token.
            let hold = super::super::ctx::OwnedCharge::new(
                &ctx,
                ctx.memory_remaining().saturating_sub(16 << 10),
            )
            .unwrap();
            let mut out = Table::new(vec![var]);
            let err = reader.next(&ctx, &mut out, 1).err();
            assert!(
                matches!(
                    err,
                    Some(Error::BudgetExceeded(b)) if b.kind == crate::BudgetKind::Memory
                ),
                "expected strSplit token interning memory refusal, got {err:?}"
            );
            drop(hold);
        }
    }

    /// Streamed `apf:strSplit` with > 7,168 distinct new tokens pre-admits the
    /// `AppendVocab` 8,192 -> 16,384 hash-table rehash peak (simultaneous old and new
    /// tables plus pointer vector and `Arc` headers) even when the caller drops each
    /// batch, and reconciles retained credit back down to physical capacities afterward.
    #[test]
    fn leaf_reader_str_split_pre_admits_local_vocab_rehash_peak() {
        let tokens: Vec<String> = (0..7_200usize).map(|i| format!("t{i:04}")).collect();
        let joined = tokens.join(",");
        // Each encoded key is `"t0000\xff` (7 bytes). At entry 7,169:
        // - Old flat `key.len() + 96` estimate charged 7,169 * 103 = ~721 KiB.
        // - Physical rehash peak holds 8,192-slot `keys` (128 KiB) + old 8,192-bucket map
        //   (200 KiB) + new 16,384-bucket map (400 KiB) + 7,169 Arcs (~273 KiB) ≈ 1,001 KiB.
        // Choose a budget above the old flat charge (~820 KiB + input string) and below
        // the rehash peak so the test would pass-by-mistake without rehash peak admission.
        let tight_limit = 920 << 10;
        {
            let ctx = test_ctx(tight_limit);
            let text_id = ctx.intern_value(&Value::Str(joined.clone().into()));
            let pat_id = ctx.intern_value(&Value::Str(",".into()));
            let var = ctx.var("tok");
            let spec = PfSpec {
                kind: PfKind::StrSplit,
                slots: vec![
                    PathEnd::Var(var),
                    PathEnd::Const(text_id),
                    PathEnd::Const(pat_id),
                ],
                inputs: Default::default(),
                graph: GraphFilter::Default,
            };
            let mut reader = LeafReader::open(&ctx, &spec, &[var]).unwrap();
            let mut err = None;
            loop {
                let mut out = Table::new(vec![var]);
                match reader.next(&ctx, &mut out, 1) {
                    Ok(true) => break,
                    Ok(false) => {}
                    Err(e) => {
                        err = Some(e);
                        break;
                    }
                }
            }
            assert!(
                matches!(
                    err,
                    Some(Error::BudgetExceeded(b)) if b.kind == crate::BudgetKind::Memory
                ),
                "expected Memory BudgetExceeded before local-vocab 8192->16384 rehash, got {err:?}"
            );
        }

        // Under ample budget, all 7,200 distinct tokens succeed, peak reflects the rehash
        // spike, and live retained bytes reconcile below the transient rehash peak.
        {
            let ctx = test_ctx(16 << 20);
            let text_id = ctx.intern_value(&Value::Str(joined.into()));
            let pat_id = ctx.intern_value(&Value::Str(",".into()));
            let var = ctx.var("tok");
            let spec = PfSpec {
                kind: PfKind::StrSplit,
                slots: vec![
                    PathEnd::Var(var),
                    PathEnd::Const(text_id),
                    PathEnd::Const(pat_id),
                ],
                inputs: Default::default(),
                graph: GraphFilter::Default,
            };
            let mut reader = LeafReader::open(&ctx, &spec, &[var]).unwrap();
            let mut count = 0usize;
            loop {
                let mut out = Table::new(vec![var]);
                let done = reader.next(&ctx, &mut out, 1).unwrap();
                count += out.len();
                if done {
                    break;
                }
            }
            assert_eq!(count, 7_200);
            drop(reader);
            let peak = ctx.mem_peak();
            let live = ctx.live_bytes();
            assert!(
                peak >= 1_000_000,
                "expected mem_peak to include local-vocab rehash spike, got {peak}"
            );
            assert!(
                live + (100 << 10) <= peak,
                "expected post-rehash live bytes ({live}) to reconcile below transient rehash peak ({peak})"
            );
        }
    }

    /// Production `LeafReader::open` matches eager `regex::Regex::new` both across the
    /// split parity matrix and up to eager's 10 MiB NFA ceiling, while returning a typed
    /// `BudgetKind::Memory` refusal when a smaller query grant caps compilation.
    #[test]
    fn leaf_reader_str_split_matches_eager_regex_ceiling_and_parity() {
        let cases = [
            ("", ","),
            ("a,b,c", ","),
            ("ab", "a*"),
            ("a,b", ",*"),
            ("xya", "y*"),
            ("1a2", r"\d*"),
            ("aαb", "a*"),
            ("aαb", "a?"),
            ("aba", "a|"),
            ("a\nb", "(?m)^"),
        ];
        for (s, pat) in cases {
            let re = regex::Regex::new(pat).unwrap();
            let expected = java_split(&re, s);
            let ctx = test_ctx(16 << 20);
            let text_id = ctx.intern_value(&Value::Str(s.into()));
            let pat_id = ctx.intern_value(&Value::Str(pat.into()));
            let var = ctx.var("t");
            let spec = PfSpec {
                kind: PfKind::StrSplit,
                slots: vec![
                    PathEnd::Var(var),
                    PathEnd::Const(text_id),
                    PathEnd::Const(pat_id),
                ],
                inputs: Default::default(),
                graph: GraphFilter::Default,
            };
            let mut reader = LeafReader::open(&ctx, &spec, &[var]).unwrap();
            let mut out = Table::new(vec![var]);
            while !reader.next(&ctx, &mut out, 4).unwrap() {}
            let got: Vec<String> = out.cols[0]
                .iter()
                .map(|&id| match ctx.value(id) {
                    Some(Value::Str(v)) => v.to_string(),
                    other => panic!("expected string token, got {other:?}"),
                })
                .collect();
            assert_eq!(
                got, expected,
                "LeafReader parity mismatch s={s:?} pat={pat:?}"
            );
        }

        // Pattern between 4 MiB and 10 MiB NFA builder size (~4.9 MiB): eager accepts it,
        // and `LeafReader::open` must accept it under ample credit while refusing with
        // `BudgetKind::Memory` under a smaller grant.
        let mid_pat = "^a{160000}$";
        let eager_mid = regex::Regex::new(mid_pat).expect("eager regex accepts 4.9 MiB NFA");
        let expected_mid = java_split(&eager_mid, "hello");

        let ample = test_ctx(u64::MAX);
        let text_id = ample.intern_value(&Value::Str("hello".into()));
        let pat_id = ample.intern_value(&Value::Str(mid_pat.into()));
        let var = ample.var("t");
        let spec = PfSpec {
            kind: PfKind::StrSplit,
            slots: vec![
                PathEnd::Var(var),
                PathEnd::Const(text_id),
                PathEnd::Const(pat_id),
            ],
            inputs: Default::default(),
            graph: GraphFilter::Default,
        };
        let mut reader = LeafReader::open(&ample, &spec, &[var]).unwrap();
        let mut out = Table::new(vec![var]);
        while !reader.next(&ample, &mut out, 4).unwrap() {}
        let got: Vec<String> = out.cols[0]
            .iter()
            .map(|&id| match ample.value(id) {
                Some(Value::Str(v)) => v.to_string(),
                other => panic!("expected string token, got {other:?}"),
            })
            .collect();
        assert_eq!(got, expected_mid);

        // Smaller grant (64 MiB -> maximum ~1.3 MiB < 10 MiB) refuses with Memory budget.
        let tight = test_ctx(64 << 20);
        let t_text = tight.intern_value(&Value::Str("hello".into()));
        let t_pat = tight.intern_value(&Value::Str(mid_pat.into()));
        let t_var = tight.var("t");
        let t_spec = PfSpec {
            kind: PfKind::StrSplit,
            slots: vec![
                PathEnd::Var(t_var),
                PathEnd::Const(t_text),
                PathEnd::Const(t_pat),
            ],
            inputs: Default::default(),
            graph: GraphFilter::Default,
        };
        let err = LeafReader::open(&tight, &t_spec, &[t_var]).err();
        assert!(
            matches!(
                err,
                Some(Error::BudgetExceeded(b)) if b.kind == crate::BudgetKind::Memory
            ),
            "expected Memory BudgetExceeded under 64 MiB grant, got {err:?}"
        );

        // Pattern exceeding eager's 10 MiB ceiling fails with `Error::Invalid` in both.
        let huge_pat = "^a{400000}$";
        assert!(regex::Regex::new(huge_pat).is_err());
        let h_pat = ample.intern_value(&Value::Str(huge_pat.into()));
        let h_spec = PfSpec {
            kind: PfKind::StrSplit,
            slots: vec![
                PathEnd::Var(var),
                PathEnd::Const(text_id),
                PathEnd::Const(h_pat),
            ],
            inputs: Default::default(),
            graph: GraphFilter::Default,
        };
        let err = LeafReader::open(&ample, &h_spec, &[var]).err();
        assert!(
            matches!(err, Some(Error::Invalid(_))),
            "expected Error::Invalid above 10 MiB ceiling, got {err:?}"
        );
    }

    /// Cancellation is observed inside the delimiter-only `StrSplitIter` loop and while
    /// walking deleted keys in `RangeCursor::next_ids` / `LeafReader::next`, not merely at
    /// `LeafReader::open`.
    #[test]
    fn str_split_and_deleted_range_cancel_mid_loop() {
        // 1. Mid-loop cancellation inside `StrSplitIter::next_token_checked`.
        let meta = regex_automata::meta::Regex::new(",").unwrap();
        let cache = meta.create_cache();
        let mut iter = StrSplitIter::new(meta, cache, 0, ",".repeat(5_000));
        let mut polls = 0usize;
        let err = iter
            .next_token_checked(|| {
                polls += 1;
                if polls >= 2 {
                    Err(Error::Cancelled)
                } else {
                    Ok(())
                }
            })
            .unwrap_err();
        assert!(matches!(err, Error::Cancelled));
        assert_eq!(polls, 2, "cancellation must trigger on inner split poll #2");

        // 2. Mid-scan cancellation inside `RangeCursor::next_ids_with_check` while walking
        // 2,000 deleted `rdf:rest` rows.
        let store = Store::in_memory(StoreOptions::default());
        let mut nt = String::new();
        for i in 0..2_000usize {
            nt.push_str(&format!(
                "<urn:del:{i:04}> <http://www.w3.org/1999/02/22-rdf-syntax-ns#first> {i} .\n\
                 <urn:del:{i:04}> <http://www.w3.org/1999/02/22-rdf-syntax-ns#rest> <http://www.w3.org/1999/02/22-rdf-syntax-ns#nil> .\n"
            ));
        }
        store
            .load(&[crate::io::Source::from_bytes(
                nt.into_bytes(),
                oxrdfio::RdfFormat::Turtle,
                None,
            )])
            .unwrap();
        super::super::update::update(
            &store,
            "DELETE WHERE { ?s <http://www.w3.org/1999/02/22-rdf-syntax-ns#rest> ?o }",
            &Default::default(),
        )
        .unwrap();
        let mut raw = Ctx::new(store.snapshot());
        raw.mem_limit = 16 << 20;
        raw.enable_cursor_accounting().unwrap();
        let ctx = Arc::new(raw);
        ctx.attach_owner();
        let ids = StoreIds::new(&ctx).unwrap();

        let scan_polls = std::cell::Cell::new(0usize);
        let mut scan = RangeCursor::new(Perm::Pso, &[ids.rest.0]);
        let mut buf = Vec::with_capacity(RANGE_CHUNK);
        let err = scan
            .next_ids_with_check(
                &ctx,
                &GraphFilter::Default,
                1,
                true,
                &mut buf,
                RANGE_CHUNK,
                || {
                    let n = scan_polls.get() + 1;
                    scan_polls.set(n);
                    if n >= 500 {
                        Err(Error::Cancelled)
                    } else {
                        Ok(())
                    }
                },
            )
            .unwrap_err();
        assert!(matches!(err, Error::Cancelled));
        assert_eq!(
            scan_polls.get(),
            500,
            "deleted-range scan must stop on inner poll #500"
        );

        // 3. `LeafReader::next` on the same deleted range opens cleanly (`cancel = false`)
        // and polls `ctx.check()` across all 2,000 deleted rows.
        let l = ctx.var("l");
        let m = ctx.var("m");
        let spec = PfSpec {
            kind: PfKind::Member,
            slots: vec![PathEnd::Var(l), PathEnd::Var(m)],
            inputs: Default::default(),
            graph: GraphFilter::Default,
        };
        let mut reader = LeafReader::open(&ctx, &spec, &[l, m]).unwrap();
        super::super::ctx::CHECKS.with(|c| c.set(0));
        let mut out = Table::new(vec![l, m]);
        assert!(reader.next(&ctx, &mut out, 1).unwrap());
        assert_eq!(out.len(), 0);
        let checks = super::super::ctx::CHECKS.with(|c| c.get());
        assert!(
            checks >= 2_000,
            "expected >= 2,000 inner deletion polls during LeafReader::next, got {checks}"
        );
    }

    /// Bound-token `apf:strSplit` must keep the uncached bound token's checked-decode
    /// charge live across the entire token scan rather than releasing it before the scan.
    #[test]
    fn bound_str_split_keeps_decode_credit_live_during_token_scan() {
        use sparkles_budget::MemoryDomain;
        use std::sync::Mutex;

        let store = Store::in_memory(StoreOptions::default());
        let events: Arc<Mutex<Vec<(bool, u64, usize)>>> = Arc::new(Mutex::new(Vec::new()));
        let ev_res = events.clone();
        let ev_rel = events.clone();
        let parent = MemoryDomain::unpooled().parent(
            move |delta| {
                let c = super::super::ctx::CHECKS.with(|n| n.get());
                ev_res.lock().unwrap().push((true, delta, c));
                Ok(())
            },
            move |delta| {
                let c = super::super::ctx::CHECKS.with(|n| n.get());
                ev_rel.lock().unwrap().push((false, delta, c));
            },
        );

        let mut raw = Ctx::new(store.snapshot());
        raw.mem_limit = 4 << 20;
        raw.set_memory_parent(parent).unwrap();
        raw.enable_cursor_accounting().unwrap();
        let ctx = Arc::new(raw);
        ctx.attach_owner();

        // 256 KiB bound token exceeds the 4 MiB ValueCache shard share (64 KiB), so
        // `value_checked_into` leaves it uncached and its live credit comes solely from
        // `tok_decode`.
        let long_tok = "z".repeat(256 << 10);
        let text = format!("{}{long_tok}", "a,".repeat(20_000));
        let tok_id = ctx.intern_value(&Value::Str(long_tok.into()));
        let text_id = ctx.intern_value(&Value::Str(text.into()));
        let pat_id = ctx.intern_value(&Value::Str(",".into()));

        events.lock().unwrap().clear();
        super::super::ctx::CHECKS.with(|n| n.set(0));

        let spec = PfSpec {
            kind: PfKind::StrSplit,
            slots: vec![
                PathEnd::Const(tok_id),
                PathEnd::Const(text_id),
                PathEnd::Const(pat_id),
            ],
            inputs: Default::default(),
            graph: GraphFilter::Default,
        };
        let mut reader = LeafReader::open(&ctx, &spec, &[]).unwrap();
        let mut out = Table::new(Vec::new());
        assert!(reader.next(&ctx, &mut out, 1).unwrap());
        assert_eq!(out.len(), 1, "bound long token must match");

        let log = events.lock().unwrap();
        // Find the large (>= 192 KiB) reserve for `tok_decode` after `StrSplitIter` setup
        // and the corresponding large release. Under `value_checked_into`, the release must
        // happen only after the 20,000-token scan advances `CHECKS` by at least 15 polls.
        let tok_res = log
            .iter()
            .rev()
            .find(|&&(is_res, delta, _)| is_res && delta >= (192 << 10))
            .copied()
            .expect("expected >= 192 KiB parent reserve for uncached bound token");
        let tok_rel = log
            .iter()
            .find(|&&(is_res, delta, check)| !is_res && delta >= (192 << 10) && check >= tok_res.2)
            .copied()
            .expect("expected >= 192 KiB parent release for uncached bound token");
        assert!(
            tok_rel.2 >= tok_res.2 + 15,
            "bound-token decode credit dropped before scan completed: reserve={tok_res:?}, release={tok_rel:?}, log={log:?}"
        );
    }
}

//! History queries in SPARQL (spec F06, Phase 3): the recorded changes of the dataset as
//! solutions.
//!
//! ```sparql
//! PREFIX hist: <urn:x-sparkles:history#>
//! SELECT ?name ?op ?commit ?time WHERE {
//!   SERVICE hist:changes {
//!     << <urn:alice> <urn:name> ?name >> hist:op ?op ; hist:commit ?commit ;
//!                                       hist:time ?time .
//!   }
//! }
//! ```
//!
//! Each solution is one net change a commit made: the reifier (RDF 1.2) stands for the
//! change event, whose triple is the triple added or removed. Constants in the triple
//! are looked up in the change log's index. The same triple can also be given with
//! `hist:subject`, `hist:predicate` and `hist:object` for clients without SPARQL 1.2
//! syntax. The other properties bind or restrict:
//!
//! * `hist:op`: `"add"` or `"remove"`;
//! * `hist:graph`: the graph (unbound for the default graph; the IRI
//!   `hist:defaultGraph` selects the default graph);
//! * `hist:commit` (`xsd:integer`), `hist:time` (`xsd:dateTime`), `hist:kind`,
//!   `hist:author`, `hist:message`: the commit (outputs only);
//! * `hist:from` and `hist:to`: the first and last commit read, as a commit number, an
//!   `xsd:dateTime`, or a selector string (`"head"`, `"commit:N"`, `"time:…"`). `to`
//!   defaults to the state the query reads, so `at=` limits history too;
//! * `hist:limit` and `hist:order` (`hist:ascending`, the default, or
//!   `hist:descending`, newest commits first).
//!
//! Without `hist:graph`, changes in every graph the caller may read are listed.
//!
//! The triple's terms, `hist:graph`, `hist:from` and `hist:to` may be variables that the
//! rest of the join group binds, as in `?s a ex:Person . SERVICE hist:changes { << ?s
//! ex:name ?n >> hist:commit ?c }`. The call is then attached to the group after join
//! ordering, as a path search is: it reads the group's solutions, looks up the changes
//! once per distinct binding of those variables, and extends each solution with the
//! changes of its binding. `hist:limit` applies to each lookup. A variable of `hist:from`
//! or `hist:to` must be bound by the group.

use super::ctx::Ctx;
use super::plan::{Kind, Node, PT, Planner};
use super::table::{Table, VarId};
use crate::error::{Error, Result};
use crate::id::Id;
use crate::store::{DiffOp, HistoryBound, HistoryQuery};
use oxrdf::vocab::xsd;
use oxrdf::{GraphName, Literal, NamedNode, Term};
use rustc_hash::FxHashMap;
use spargebra::algebra::GraphPattern;
use spargebra::term::{NamedNodePattern, TermPattern};

/// The namespace of the parameters (`PREFIX hist: <urn:x-sparkles:history#>`).
pub const NS: &str = "urn:x-sparkles:history#";
/// The SERVICE IRI of a history query.
pub const CHANGES: &str = "urn:x-sparkles:history#changes";
const DEFAULT_GRAPH: &str = "urn:x-sparkles:history#defaultGraph";
const RDF_REIFIES: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#reifies";

/// Whether a SERVICE name is the history query.
pub fn is_changes(name: &NamedNodePattern) -> bool {
    matches!(name, NamedNodePattern::NamedNode(n) if n.as_str() == CHANGES)
}

fn bad(m: impl std::fmt::Display) -> Error {
    Error::invalid(format!("hist:changes: {m}"))
}

/// One position of the change's quad, or a filter on the operation.
#[derive(Clone, Debug, PartialEq)]
pub enum Slot {
    Any,
    Var(VarId),
    Const(Term),
}

/// A planned history query.
#[derive(Clone, Debug)]
pub struct HistorySpec {
    pub s: Slot,
    pub p: Slot,
    pub o: Slot,
    /// `Const` of a graph name; the default graph is the IRI `hist:defaultGraph`
    pub g: Slot,
    pub op: Slot,
    pub commit: Option<VarId>,
    pub time: Option<VarId>,
    pub kind: Option<VarId>,
    pub author: Option<VarId>,
    pub message: Option<VarId>,
    pub from: Option<HistoryBound>,
    pub to: Option<HistoryBound>,
    /// `hist:from` and `hist:to` given as variables, which the group must bind
    pub from_var: Option<VarId>,
    pub to_var: Option<VarId>,
    pub limit: Option<u64>,
    pub descending: bool,
    /// the variables of the slots and bounds that the rest of the group binds: read from
    /// the input rows (set when the call is attached)
    pub inputs: Vec<VarId>,
}

impl HistorySpec {
    /// The variables the call may read from the rest of its group.
    fn readable(&self) -> Vec<VarId> {
        let mut v: Vec<VarId> = [&self.s, &self.p, &self.o, &self.g]
            .into_iter()
            .filter_map(|s| match s {
                Slot::Var(v) => Some(*v),
                _ => None,
            })
            .chain(self.from_var)
            .chain(self.to_var)
            .collect();
        v.sort_unstable();
        v.dedup();
        v
    }

    /// The variables the call binds.
    fn outputs(&self) -> Vec<VarId> {
        let mut vars: Vec<VarId> = Vec::new();
        for s in [&self.s, &self.p, &self.o, &self.g, &self.op] {
            if let Slot::Var(v) = s {
                vars.push(*v);
            }
        }
        vars.extend(
            [self.commit, self.time, self.kind, self.author, self.message]
                .into_iter()
                .flatten(),
        );
        vars.sort_unstable();
        vars.dedup();
        vars
    }

    /// Whether the call may read the rest of its group. It is attached after the join
    /// order, which decides.
    pub fn needs_input(&self) -> bool {
        !self.readable().is_empty()
    }
}

fn slot(p: &Planner<'_>, t: &TermPattern, name: &str) -> Result<Slot> {
    Ok(match t {
        // a variable the planner replaced by its value is that value
        TermPattern::Variable(_) => match p.term_pattern(t) {
            PT::V(v) => Slot::Var(v),
            PT::C(id) => match p.ctx.term(id) {
                Some(t) => Slot::Const(t),
                None => return Err(bad(format!("{name} has no value"))),
            },
        },
        TermPattern::BlankNode(_) => Slot::Any,
        TermPattern::NamedNode(n) => Slot::Const(Term::NamedNode(n.clone())),
        TermPattern::Literal(l) => Slot::Const(Term::Literal(l.clone())),
        TermPattern::Triple(_) => return Err(bad(format!("{name} cannot be a triple term"))),
    })
}

fn bound(t: &TermPattern, name: &str) -> Result<HistoryBound> {
    match t {
        TermPattern::Literal(l) => bound_term(&Term::Literal(l.clone()), name),
        _ => Err(bad(format!(
            "hist:{name} takes a commit number, an xsd:dateTime or a selector string"
        ))),
    }
}

/// A bound from a constant, or from the value a variable has in the group.
fn bound_term(t: &Term, name: &str) -> Result<HistoryBound> {
    let Term::Literal(l) = t else {
        return Err(bad(format!(
            "hist:{name} takes a commit number, an xsd:dateTime or a selector string, not {t}"
        )));
    };
    let dt = l.datatype();
    if dt == xsd::DATE_TIME || dt == xsd::DATE_TIME_STAMP {
        let ms = crate::commit::parse_rfc3339_ms(l.value())
            .ok_or_else(|| bad(format!("hist:{name}: invalid date-time {}", l.value())))?;
        return Ok(HistoryBound::Time(ms));
    }
    if dt == xsd::STRING {
        let at: crate::history::At = l.value().parse()?;
        return Ok(match at {
            crate::history::At::Time(ms) => HistoryBound::Time(ms),
            crate::history::At::Snapshot(n) => {
                return Err(bad(format!(
                    "hist:{name}: snapshot:{n} is not known to a query; give its commit"
                )));
            }
            a => HistoryBound::At(a),
        });
    }
    l.value()
        .trim()
        .parse::<u64>()
        .map(HistoryBound::Commit)
        .map_err(|_| bad(format!("hist:{name} takes a non-negative commit number")))
}

/// Plan `SERVICE hist:changes { … }`.
pub(super) fn history_leaf(p: &Planner<'_>, inner: &GraphPattern) -> Result<Node> {
    let GraphPattern::Bgp { patterns } = inner else {
        return Err(bad(
            "the block must hold one change pattern: << s p o >> with hist: properties",
        ));
    };
    // the reifier of the change, from `<< s p o >>` (rdf:reifies a triple term)
    let reified: Vec<_> = patterns
        .iter()
        .filter(|tp| {
            matches!(&tp.predicate, NamedNodePattern::NamedNode(n) if n.as_str() == RDF_REIFIES)
        })
        .collect();
    if reified.len() > 1 {
        return Err(bad("one change pattern per SERVICE"));
    }
    let event = match reified.first() {
        Some(tp) => tp.subject.clone(),
        None => match patterns.first() {
            Some(tp) => tp.subject.clone(),
            None => return Err(bad("missing the change pattern")),
        },
    };
    let mut spec = HistorySpec {
        s: Slot::Any,
        p: Slot::Any,
        o: Slot::Any,
        g: Slot::Any,
        op: Slot::Any,
        commit: None,
        time: None,
        kind: None,
        author: None,
        message: None,
        from: None,
        to: None,
        from_var: None,
        to_var: None,
        limit: None,
        descending: false,
        inputs: Vec::new(),
    };
    if let Some(tp) = reified.first() {
        let TermPattern::Triple(t) = &tp.object else {
            return Err(bad("rdf:reifies takes a triple term"));
        };
        spec.s = slot(p, &t.subject, "the subject")?;
        spec.p = match &t.predicate {
            NamedNodePattern::NamedNode(n) => Slot::Const(Term::NamedNode(n.clone())),
            NamedNodePattern::Variable(v) => {
                slot(p, &TermPattern::Variable(v.clone()), "the predicate")?
            }
        };
        spec.o = slot(p, &t.object, "the object")?;
    }
    let mut seen = std::collections::HashSet::new();
    let out = |t: &TermPattern, name: &str| -> Result<VarId> {
        match slot(p, t, name)? {
            Slot::Var(v) => Ok(v),
            _ => Err(bad(format!("hist:{name} must be a variable"))),
        }
    };
    for tp in patterns {
        if reified.iter().any(|r| std::ptr::eq(*r, tp)) {
            continue;
        }
        if tp.subject != event {
            return Err(bad(
                "every property must describe the one change (one change pattern per SERVICE)",
            ));
        }
        let NamedNodePattern::NamedNode(pn) = &tp.predicate else {
            return Err(bad("a property must be a hist: IRI, not a variable"));
        };
        let Some(name) = pn.as_str().strip_prefix(NS) else {
            return Err(bad(format!("unknown property <{}>", pn.as_str())));
        };
        if !seen.insert(name.to_string()) {
            return Err(bad(format!("hist:{name} is given twice")));
        }
        let o = &tp.object;
        match name {
            "subject" | "predicate" | "object" if !reified.is_empty() => {
                return Err(bad(format!(
                    "hist:{name} and << s p o >> both give the triple"
                )));
            }
            "subject" => spec.s = slot(p, o, "hist:subject")?,
            "predicate" => {
                spec.p = slot(p, o, "hist:predicate")?;
                if matches!(&spec.p, Slot::Const(t) if !matches!(t, Term::NamedNode(_))) {
                    return Err(bad("hist:predicate takes an IRI"));
                }
            }
            "object" => spec.o = slot(p, o, "hist:object")?,
            "graph" => {
                spec.g = slot(p, o, "hist:graph")?;
                if matches!(&spec.g, Slot::Const(Term::Literal(_))) {
                    return Err(bad("hist:graph takes an IRI or a variable"));
                }
            }
            "op" => {
                spec.op = slot(p, o, "hist:op")?;
                if let Slot::Const(t) = &spec.op
                    && !matches!(t, Term::Literal(l) if l.value() == "add" || l.value() == "remove")
                {
                    return Err(bad("hist:op is \"add\" or \"remove\""));
                }
            }
            "commit" => spec.commit = Some(out(o, name)?),
            "time" => spec.time = Some(out(o, name)?),
            "kind" => spec.kind = Some(out(o, name)?),
            "author" => spec.author = Some(out(o, name)?),
            "message" => spec.message = Some(out(o, name)?),
            "from" | "to" if matches!(o, TermPattern::Variable(_)) => {
                let (var, b) = match name {
                    "from" => (&mut spec.from_var, &mut spec.from),
                    _ => (&mut spec.to_var, &mut spec.to),
                };
                match slot(p, o, name)? {
                    Slot::Var(v) => *var = Some(v),
                    Slot::Const(t) => *b = Some(bound_term(&t, name)?),
                    Slot::Any => unreachable!("a variable"),
                }
            }
            "from" => spec.from = Some(bound(o, name)?),
            "to" => spec.to = Some(bound(o, name)?),
            "limit" => {
                let TermPattern::Literal(l) = o else {
                    return Err(bad("hist:limit takes a non-negative integer"));
                };
                spec.limit = Some(
                    l.value()
                        .trim()
                        .parse()
                        .map_err(|_| bad("hist:limit takes a non-negative integer"))?,
                );
            }
            "order" => {
                spec.descending = match o {
                    TermPattern::NamedNode(n) if n.as_str() == format!("{NS}descending") => true,
                    TermPattern::NamedNode(n) if n.as_str() == format!("{NS}ascending") => false,
                    _ => return Err(bad("hist:order is hist:ascending or hist:descending")),
                }
            }
            other => return Err(bad(format!("unknown property hist:{other}"))),
        }
    }
    let vars = spec.outputs();
    let desc = format!("history {spec:?}");
    let est = spec.limit.map_or(1000.0, |l| l as f64);
    let mut n = Node::leaf(
        Kind::HistoryChanges(Box::new(spec.clone())),
        vars,
        est,
        desc,
    );
    // the default graph, an author and a message can be missing
    for v in [
        match spec.g {
            Slot::Var(v) => Some(v),
            _ => None,
        },
        spec.author,
        spec.message,
    ]
    .into_iter()
    .flatten()
    {
        n.certain.retain(|c| *c != v);
    }
    n.cost = est * 8.0;
    Ok(n)
}

/// Attach a call whose variables the rest of its group may bind to that group's plan
/// `left`. A call that reads none of them is joined with the group as a leaf.
pub(super) fn attach(p: &Planner<'_>, left: Node, call: Node) -> Result<Node> {
    let Kind::HistoryChanges(spec) = &call.kind else {
        unreachable!("a history query");
    };
    for (v, name) in [(spec.from_var, "from"), (spec.to_var, "to")] {
        if let Some(v) = v
            && !left.vars.contains(&v)
        {
            return Err(bad(format!(
                "hist:{name} ?{} is not bound by the rest of the group; bind it, for example with VALUES",
                p.ctx.var_name(v)
            )));
        }
    }
    let inputs: Vec<VarId> = spec
        .readable()
        .into_iter()
        .filter(|v| left.vars.contains(v))
        .collect();
    if inputs.is_empty() {
        return Ok(super::plan::join(left, call, p.ctx));
    }
    let mut spec = (**spec).clone();
    spec.inputs = inputs;
    let mut vars = left.vars.clone();
    for v in &call.vars {
        if !vars.contains(v) {
            vars.push(*v);
        }
    }
    let mut certain = left.certain.clone();
    certain.extend(call.certain.iter().copied());
    let est = (left.est * 10.0).max(1.0);
    Ok(Node {
        dist: vars.iter().map(|&v| (v, est)).collect(),
        cost: left.cost + est * 8.0,
        vars,
        certain,
        sorted: Vec::new(),
        est,
        desc: format!("history {spec:?}"),
        kind: Kind::HistoryChanges(Box::new(spec)),
        children: vec![left],
    })
}

/// Run a planned history query on the change log of the query's snapshot. With `input`
/// (an attached call), the changes are looked up once per distinct binding of the
/// input variables, and each input row is extended with the changes that agree with it.
pub(super) fn run(
    ctx: &Ctx,
    spec: &HistorySpec,
    input: Option<&Table>,
    vars: &[VarId],
) -> Result<Table> {
    let Some(input) = input else {
        if let Some(v) = spec.from_var.or(spec.to_var) {
            return Err(bad(format!(
                "?{} in hist:from or hist:to is not bound by the rest of the group",
                ctx.var_name(v)
            )));
        }
        let t = lookup(ctx, spec, vars)?;
        ctx.produced(t.len())?;
        return Ok(t);
    };
    let outputs = spec.outputs();
    let key_cols: Vec<Option<usize>> = spec.inputs.iter().map(|v| input.col_of(*v)).collect();
    let mut memo: FxHashMap<Vec<Id>, Table> = FxHashMap::default();
    let mut out = Table::new(vars.to_vec());
    let mut row = vec![Id::UNDEF; vars.len()];
    let mut rows = 0usize;
    for i in 0..input.len() {
        if i % 1024 == 0 {
            ctx.check()?;
            ctx.check_output(out.len(), out.width())?;
        }
        let key: Vec<Id> = key_cols
            .iter()
            .map(|c| c.map_or(Id::UNDEF, |c| input.cols[c][i]))
            .collect();
        if !memo.contains_key(&key) {
            let t = match bind(ctx, spec, &key)? {
                // the lookup binds the call's variables but those whose values the row
                // gave as constants
                Some(s) => lookup(ctx, &s, &s.outputs())?,
                None => Table::empty(outputs.clone()),
            };
            rows += t.len();
            ctx.check_rows(rows)?;
            memo.insert(key.clone(), t);
        }
        let t = &memo[&key];
        'sol: for j in 0..t.len() {
            for (o, v) in vars.iter().enumerate() {
                let have = input.col_of(*v).map_or(Id::UNDEF, |c| input.cols[c][i]);
                let got = t.col_of(*v).map_or(Id::UNDEF, |c| t.get(j, c));
                row[o] = match (have.is_undef(), got.is_undef()) {
                    (_, true) => have,
                    (true, false) => got,
                    (false, false) if have == got => have,
                    // the change disagrees with the row on a variable both bind
                    (false, false) => continue 'sol,
                };
            }
            out.push_row(&row);
        }
    }
    ctx.produced(out.len())?;
    Ok(out)
}

/// The call with the values of one binding of its input variables as constants. An
/// unbound value leaves its variable to the lookup. `None` when no change can match,
/// such as a literal in the predicate's place.
fn bind(ctx: &Ctx, spec: &HistorySpec, key: &[Id]) -> Result<Option<HistorySpec>> {
    let mut s = spec.clone();
    s.inputs.clear();
    for (v, k) in spec.inputs.iter().zip(key) {
        if k.is_undef() {
            continue;
        }
        let Some(t) = ctx.term(*k) else {
            return Ok(None);
        };
        for slot in [&mut s.s, &mut s.p, &mut s.o, &mut s.g] {
            if *slot == Slot::Var(*v) {
                *slot = Slot::Const(t.clone());
            }
        }
        if spec.from_var == Some(*v) {
            s.from = Some(bound_term(&t, "from")?);
            s.from_var = None;
        }
        if spec.to_var == Some(*v) {
            s.to = Some(bound_term(&t, "to")?);
            s.to_var = None;
        }
    }
    if let Some(v) = s.from_var.or(s.to_var) {
        return Err(bad(format!(
            "?{} in hist:from or hist:to is unbound in a solution of the group",
            ctx.var_name(v)
        )));
    }
    let ok = match &s.p {
        Slot::Const(t) => matches!(t, Term::NamedNode(_)),
        _ => true,
    } && !matches!(&s.g, Slot::Const(Term::Literal(_)))
        && !matches!(&s.s, Slot::Const(Term::Literal(_)));
    Ok(ok.then_some(s))
}

/// One lookup in the change log: the changes that match the call's constants, as rows
/// of `vars`.
fn lookup(ctx: &Ctx, spec: &HistorySpec, vars: &[VarId]) -> Result<Table> {
    let Some(log) = ctx.snap.change_log.as_ref() else {
        return Err(Error::HistoryUnsupported(
            "history queries need a store with a change log".into(),
        ));
    };
    let terms = |s: &Slot| match s {
        Slot::Const(t) => vec![t.clone()],
        _ => Vec::new(),
    };
    let q = HistoryQuery {
        subjects: terms(&spec.s),
        predicates: match &spec.p {
            Slot::Const(Term::NamedNode(n)) => vec![n.clone()],
            _ => Vec::new(),
        },
        objects: terms(&spec.o),
        graphs: match &spec.g {
            Slot::Const(Term::NamedNode(n)) if n.as_str() == DEFAULT_GRAPH => {
                vec![GraphName::DefaultGraph]
            }
            Slot::Const(Term::NamedNode(n)) => vec![GraphName::NamedNode(n.clone())],
            Slot::Const(Term::BlankNode(b)) => vec![GraphName::BlankNode(b.clone())],
            _ => Vec::new(),
        },
        from: spec.from.clone(),
        to: spec.to.clone(),
        op: match &spec.op {
            Slot::Const(Term::Literal(l)) if l.value() == "add" => Some(DiffOp::Add),
            Slot::Const(_) => Some(DiffOp::Remove),
            _ => None,
        },
        // one more than the row budget, so that passing it fails
        limit: spec.limit.map_or(ctx.max_rows.saturating_add(1), |l| {
            (l as usize).min(ctx.max_rows.saturating_add(1))
        }),
        descending: spec.descending,
        access: ctx.graphs.clone(),
        cancel: Some(ctx.cancel.clone()),
        deadline: ctx.deadline,
    };
    // a `head` bound means the state the query reads
    let resolve = |b: &Option<HistoryBound>| match b {
        Some(HistoryBound::At(a)) => Some(match a {
            crate::history::At::Commit(n) => HistoryBound::Commit(*n),
            _ => HistoryBound::Commit(ctx.snap.commit),
        }),
        b => b.clone(),
    };
    let q = HistoryQuery {
        from: resolve(&q.from),
        to: resolve(&q.to),
        ..q
    };
    if spec.limit == Some(0) {
        return Ok(Table::empty(vars.to_vec()));
    }
    let r = log.query(&q, ctx.snap.commit)?;
    ctx.check_rows(r.changes.len())?;
    let mut t = Table::new(vars.to_vec());
    let col = |v: Option<VarId>| v.and_then(|v| vars.iter().position(|x| *x == v));
    let slot_col = |s: &Slot| match s {
        Slot::Var(v) => col(Some(*v)),
        _ => None,
    };
    let (cs, cp, co, cg, cop) = (
        slot_col(&spec.s),
        slot_col(&spec.p),
        slot_col(&spec.o),
        slot_col(&spec.g),
        slot_col(&spec.op),
    );
    let (ccommit, ctime, ckind, cauthor, cmessage) = (
        col(spec.commit),
        col(spec.time),
        col(spec.kind),
        col(spec.author),
        col(spec.message),
    );
    let mut row = vec![Id::UNDEF; vars.len()];
    let plain = |s: &str| ctx.intern_term(&Term::Literal(Literal::new_simple_literal(s)));
    'rows: for c in &r.changes {
        ctx.check()?;
        row.fill(Id::UNDEF);
        // a variable used twice must get the same term
        let put = |row: &mut Vec<Id>, c: Option<usize>, id: Id| -> bool {
            match c {
                Some(i) if row[i].is_undef() => {
                    row[i] = id;
                    true
                }
                Some(i) => row[i] == id,
                None => true,
            }
        };
        let s: Term = c.quad.subject.clone().into();
        let g = match &c.quad.graph_name {
            GraphName::DefaultGraph => None,
            GraphName::NamedNode(n) => Some(Term::NamedNode(n.clone())),
            GraphName::BlankNode(b) => Some(Term::BlankNode(b.clone())),
        };
        let mut ok = put(&mut row, cs, ctx.intern_term(&s))
            && put(
                &mut row,
                cp,
                ctx.intern_term(&Term::NamedNode(c.quad.predicate.clone())),
            )
            && put(&mut row, co, ctx.intern_term(&c.quad.object));
        if let Some(g) = &g {
            ok = ok && put(&mut row, cg, ctx.intern_term(g));
        } else if let Some(i) = cg
            && !row[i].is_undef()
        {
            // the variable is bound to a term elsewhere in the call: no match
            ok = false;
        }
        if !ok {
            continue 'rows;
        }
        let op = match c.op {
            DiffOp::Add => "add",
            DiffOp::Remove => "remove",
        };
        let commit = c.commit.seq as i64;
        let commit_id = Id::from_i64(commit).unwrap_or_else(|| {
            ctx.intern_term(&Term::Literal(Literal::new_typed_literal(
                commit.to_string(),
                xsd::INTEGER,
            )))
        });
        let time = ctx.intern_term(&Term::Literal(Literal::new_typed_literal(
            c.commit.timestamp(),
            xsd::DATE_TIME,
        )));
        let ok = put(&mut row, cop, plain(op))
            && put(&mut row, ccommit, commit_id)
            && put(&mut row, ctime, time)
            && put(&mut row, ckind, plain(c.commit.kind.name()))
            && match &c.commit.author {
                Some(a) => put(&mut row, cauthor, plain(a)),
                None => true,
            }
            && match &c.commit.message {
                Some(m) => put(&mut row, cmessage, plain(m)),
                None => true,
            };
        if ok {
            t.push_row(&row);
        }
    }
    Ok(t)
}

/// The IRI of the default graph in `hist:graph` (for documentation and tests).
pub fn default_graph_iri() -> NamedNode {
    NamedNode::new_unchecked(DEFAULT_GRAPH)
}

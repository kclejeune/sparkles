//! Execution (§6): the fetch groups run in order, parents before children, on one
//! snapshot with the caller's query options and one shared deadline and row count; then
//! the rows are assembled into the response by `apollo-compiler`'s executor, whose
//! resolvers read the rows and never query.

use crate::Compiled;
use crate::algebra::{self, Built};
use crate::cursor::{self, Cursor};
use crate::error::{Code, GqlError};
use crate::mapping::{FieldMap, OnMany, Target};
use crate::plan::{FieldPlan, GroupKind, Plan, RootPlan, key};
use crate::scalars::{id_of, lang_matches, tag_of, term_cmp, to_json};
use apollo_compiler::ExecutableDocument;
use apollo_compiler::executable::Operation;
use apollo_compiler::resolvers::{Execution, FieldError, ObjectValue, ResolveInfo, ResolvedValue};
use apollo_compiler::response::JsonMap;
use apollo_compiler::validation::Valid;
use oxrdf::Term;
use serde_json::{Map, Value as J, json};
use sparkles::sparql::QueryOptions;
use sparkles::store::Snapshot;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

/// The nodes of a node group: one record per (parent, node).
#[derive(Clone, Debug)]
pub struct Rec {
    pub node: Term,
    /// distinct values per single column
    pub singles: Vec<Vec<Term>>,
    pub flags: Vec<bool>,
    /// the position in the collection (connections)
    pub pos: u64,
}

#[derive(Clone, Debug, Default)]
pub struct Page {
    pub has_next: bool,
    pub has_prev: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Data {
    pub recs: Vec<Rec>,
    pub by_parent: HashMap<Term, Vec<usize>>,
    /// values groups: per node, the values of each field number
    pub values: HashMap<Term, Vec<Vec<Term>>>,
    pub count: Option<u64>,
    pub page: Page,
    pub rows: usize,
    pub ms: f64,
    pub sparql: Option<String>,
    /// the group did not run (no parent nodes)
    pub skipped: bool,
}

fn is_node(t: &Term) -> bool {
    matches!(t, Term::NamedNode(_) | Term::BlankNode(_))
}

/// The distinct node ids of a group's records, in order.
fn node_ids(d: &Data) -> Vec<Term> {
    let mut seen = std::collections::HashSet::new();
    d.recs
        .iter()
        .filter(|r| is_node(&r.node) && seen.insert(r.node.clone()))
        .map(|r| r.node.clone())
        .collect()
}

fn engine(e: sparkles::Error) -> GqlError {
    GqlError::from_engine(e)
}

struct Run<'a> {
    snap: Arc<Snapshot>,
    opts: &'a QueryOptions,
    deadline: Option<Instant>,
}

impl Run<'_> {
    fn run(
        &self,
        built_q: &spargebra::Query,
        bindings: Vec<(String, Term)>,
    ) -> Result<sparkles::sparql::QueryResult, GqlError> {
        let mut o = self.opts.clone();
        if let Some(d) = self.deadline {
            let left = d.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(GqlError::new(Code::Timeout, "the request timed out"));
            }
            o.timeout = Some(left);
        }
        o.initial_bindings = bindings;
        sparkles::sparql::execute_query(self.snap.clone(), built_q, &o, 0.0).map_err(engine)
    }
}

fn col(r: &sparkles::sparql::QueryResult, name: &str) -> Option<usize> {
    r.vars.iter().position(|v| v == name)
}

fn cell(r: &sparkles::sparql::QueryResult, c: Option<usize>, i: usize) -> Option<Term> {
    c.and_then(|c| r.term(r.table.cols[c][i]))
}

fn decode_nodes(r: &sparkles::sparql::QueryResult, b: &Built, parent: bool, d: &mut Data) {
    let pc = if parent { col(r, "p") } else { None };
    let nc = col(r, "n");
    let sc: Vec<Option<usize>> = b.singles.iter().map(|v| col(r, v.as_str())).collect();
    let fc: Vec<Option<usize>> = b.flags.iter().map(|v| col(r, v.as_str())).collect();
    let mut index: HashMap<(Option<Term>, Term), usize> = HashMap::new();
    for i in 0..r.table.len() {
        let Some(node) = cell(r, nc, i) else { continue };
        let p = cell(r, pc, i);
        let idx = match index.get(&(p.clone(), node.clone())) {
            Some(&x) => x,
            None => {
                d.recs.push(Rec {
                    node: node.clone(),
                    singles: vec![Vec::new(); sc.len()],
                    flags: fc
                        .iter()
                        .map(|c| {
                            matches!(cell(r, *c, i), Some(Term::Literal(l)) if l.value() == "true")
                        })
                        .collect(),
                    pos: 0,
                });
                let x = d.recs.len() - 1;
                index.insert((p.clone(), node), x);
                if let Some(p) = p {
                    d.by_parent.entry(p).or_default().push(x);
                }
                x
            }
        };
        for (k, c) in sc.iter().enumerate() {
            if let Some(v) = cell(r, *c, i)
                && !d.recs[idx].singles[k].contains(&v)
            {
                d.recs[idx].singles[k].push(v);
            }
        }
    }
}

/// Run every group of a plan.
pub fn run_groups(
    c: &Compiled,
    plan: &Plan,
    snap: Arc<Snapshot>,
    opts: &QueryOptions,
    deadline: Option<Instant>,
    explain: bool,
) -> Result<Vec<Data>, GqlError> {
    let m = &c.mapping;
    let run = Run {
        snap,
        opts,
        deadline,
    };
    let mut out: Vec<Data> = Vec::with_capacity(plan.groups.len());
    for g in &plan.groups {
        let t0 = Instant::now();
        let mut d = Data::default();
        let ids = || g.parent.map(|p| node_ids(&out[p])).unwrap_or_default();
        let text;
        match &g.kind {
            GroupKind::Count { ty, filter } => {
                let q = algebra::count_query(m, ty, filter.as_ref())?;
                let r = run.run(&q, Vec::new())?;
                d.count = cell(&r, col(&r, "c"), 0).and_then(|t| match t {
                    Term::Literal(l) => l.value().parse().ok(),
                    _ => None,
                });
                d.rows = r.table.len();
                text = q.to_string();
            }
            GroupKind::Values => {
                let ids = ids();
                let (q, bindings) = algebra::values_query(&g.vfields, &ids);
                text = q.to_string();
                if ids.is_empty() {
                    d.skipped = true;
                } else {
                    let r = run.run(&q, bindings)?;
                    let (pc, fc, vc) = (col(&r, "p"), col(&r, "f"), col(&r, "v"));
                    for i in 0..r.table.len() {
                        let (Some(p), Some(Term::Literal(f)), Some(v)) =
                            (cell(&r, pc, i), cell(&r, fc, i), cell(&r, vc, i))
                        else {
                            continue;
                        };
                        let Ok(f) = f.value().parse::<usize>() else {
                            continue;
                        };
                        let e = d
                            .values
                            .entry(p)
                            .or_insert_with(|| vec![Vec::new(); g.vfields.len()]);
                        if f < e.len() {
                            e[f].push(v);
                        }
                    }
                    d.rows = r.table.len();
                }
            }
            GroupKind::Collection { window, count, .. } => {
                let total = count.and_then(|i| out[i].count);
                let lo = window.after.unwrap_or(0).saturating_add(window.offset);
                // items before the `before` cursor's item
                let mut hi = window.before.map(|b| b.saturating_sub(1));
                let mut probe = false;
                if let Some(f) = window.first {
                    let h = lo.saturating_add(f);
                    if hi.is_none_or(|x| h < x) {
                        hi = Some(h);
                        probe = true;
                    }
                }
                let mut lo = lo;
                if let Some(l) = window.last {
                    let h = hi.or(total).unwrap_or(lo);
                    if hi.is_none() {
                        hi = Some(h);
                    }
                    lo = lo.max(h.saturating_sub(l));
                    probe = false;
                }
                let hi = hi.unwrap_or(lo).max(lo);
                let len = (hi - lo) as usize;
                let fetch = len + usize::from(probe);
                let b = algebra::node_query(m, g, &[], Some((lo as usize, Some(fetch))))?;
                text = b.query.to_string();
                let r = run.run(&b.query, b.bindings.clone())?;
                decode_nodes(&r, &b, false, &mut d);
                d.rows = r.table.len();
                let more = d.recs.len() > len;
                d.recs.truncate(len);
                for (i, rec) in d.recs.iter_mut().enumerate() {
                    rec.pos = lo + i as u64;
                }
                d.page = Page {
                    has_prev: lo > 0,
                    has_next: more || window.before.is_some() || total.is_some_and(|t| hi < t),
                };
            }
            GroupKind::Lookup { .. } | GroupKind::Node { .. } => {
                let b = algebra::node_query(m, g, &[], None)?;
                text = b.query.to_string();
                let r = run.run(&b.query, b.bindings.clone())?;
                decode_nodes(&r, &b, false, &mut d);
                d.rows = r.table.len();
            }
            GroupKind::Child { .. } => {
                let ids = ids();
                let b = algebra::node_query(m, g, &ids, None)?;
                text = b.query.to_string();
                if ids.is_empty() {
                    d.skipped = true;
                } else {
                    let r = run.run(&b.query, b.bindings.clone())?;
                    decode_nodes(&r, &b, true, &mut d);
                    d.rows = r.table.len();
                }
            }
        }
        d.ms = t0.elapsed().as_secs_f64() * 1000.0;
        if explain {
            d.sparql = Some(text);
        }
        out.push(d);
    }
    Ok(out)
}

// ------------------------------------------------------------------ assembly ------

const MARK: char = '\u{1}';

pub struct Cx<'a> {
    pub c: &'a Compiled,
    pub plan: &'a Plan,
    pub data: &'a [Data],
    pub errors: RefCell<Vec<GqlError>>,
    pub nodes: Cell<u64>,
    pub max_nodes: u64,
    pub over: Cell<bool>,
    pub commit: u64,
}

impl Cx<'_> {
    fn fail(&self, e: GqlError) -> FieldError {
        let mut errs = self.errors.borrow_mut();
        errs.push(e);
        FieldError {
            message: format!("{MARK}{}", errs.len() - 1),
        }
    }

    fn count_node(&self) -> Result<(), FieldError> {
        let n = self.nodes.get() + 1;
        self.nodes.set(n);
        if n > self.max_nodes {
            self.over.set(true);
            return Err(self.fail(GqlError::new(
                Code::TooComplex,
                format!("the response holds more than {} nodes", self.max_nodes),
            )));
        }
        Ok(())
    }

    /// The object type of a record of group `g`.
    fn type_of(&self, g: usize, rec: &Rec) -> Result<String, GqlError> {
        let grp = &self.plan.groups[g];
        if grp.flags.is_empty()
            && self
                .c
                .mapping
                .ty(&grp.declared)
                .is_some_and(|t| !t.interface)
        {
            return Ok(grp.declared.clone());
        }
        if let Some(i) = rec.flags.iter().position(|f| *f) {
            return Ok(grp.flags[i].clone());
        }
        if grp.declared == "Node" {
            return Ok("Resource".into());
        }
        Err(GqlError::new(
            Code::UnresolvedType,
            format!(
                "{} belongs to none of the types of {}",
                rec.node, grp.declared
            ),
        )
        .with("node", id_of(&rec.node).unwrap_or_default()))
    }

    fn node<'b>(&'b self, g: usize, rec: usize) -> Result<ResolvedValue<'b>, FieldError> {
        let r = &self.data[g].recs[rec];
        if !is_node(&r.node) {
            return Err(self.fail(
                GqlError::new(
                    Code::InvalidValue,
                    format!("{} is a literal where a node is expected", r.node),
                )
                .with("value", r.node.to_string()),
            ));
        }
        self.count_node()?;
        let ty = self.type_of(g, r).map_err(|e| self.fail(e))?;
        Ok(ResolvedValue::object(NodeObj {
            cx: self,
            group: g,
            rec,
            ty,
        }))
    }
}

fn leaf<'b>(v: J) -> ResolvedValue<'b> {
    ResolvedValue::leaf(v)
}

/// A JSON object as a GraphQL object (`LangString`, `RDFTerm`, `PageInfo`).
struct JsonObj {
    ty: String,
    map: Map<String, J>,
}

impl ObjectValue for JsonObj {
    fn type_name(&self) -> &str {
        &self.ty
    }
    fn resolve_field<'a>(
        &'a self,
        info: &'a ResolveInfo<'a>,
    ) -> Result<ResolvedValue<'a>, FieldError> {
        Ok(leaf(
            self.map.get(info.field_name()).cloned().unwrap_or(J::Null),
        ))
    }
}

struct RootObj<'a> {
    cx: &'a Cx<'a>,
}

impl ObjectValue for RootObj<'_> {
    fn type_name(&self) -> &str {
        "Query"
    }
    fn resolve_field<'a>(
        &'a self,
        info: &'a ResolveInfo<'a>,
    ) -> Result<ResolvedValue<'a>, FieldError> {
        let cx = self.cx;
        let k = key(info.field_selections()[0]);
        let Some(rp) = cx.plan.roots.get(&k) else {
            return Ok(ResolvedValue::null());
        };
        match *rp {
            RootPlan::Lookup { group } | RootPlan::Node { group } => {
                if cx.data[group].recs.is_empty() {
                    Ok(ResolvedValue::null())
                } else {
                    cx.node(group, 0)
                }
            }
            RootPlan::List { group } => {
                let items: Vec<_> = (0..cx.data[group].recs.len())
                    .map(|i| cx.node(group, i))
                    .collect();
                Ok(ResolvedValue::List(Box::new(items.into_iter())))
            }
            RootPlan::Connection { group, count } => Ok(ResolvedValue::object(ConnObj {
                cx,
                group,
                count,
                ty: format!("{}Connection", cx.plan.groups[group].declared),
            })),
        }
    }
}

struct ConnObj<'a> {
    cx: &'a Cx<'a>,
    group: usize,
    count: Option<usize>,
    ty: String,
}

impl ConnObj<'_> {
    fn cursor(&self, rec: usize) -> String {
        let hash = match &self.cx.plan.groups[self.group].kind {
            GroupKind::Collection { window, .. } => window.hash.clone(),
            _ => String::new(),
        };
        cursor::encode(&Cursor {
            v: 1,
            c: self.cx.commit,
            h: hash,
            o: self.cx.data[self.group].recs[rec].pos + 1,
        })
    }
}

impl ObjectValue for ConnObj<'_> {
    fn type_name(&self) -> &str {
        &self.ty
    }
    fn resolve_field<'a>(
        &'a self,
        info: &'a ResolveInfo<'a>,
    ) -> Result<ResolvedValue<'a>, FieldError> {
        let cx = self.cx;
        let d = &cx.data[self.group];
        match info.field_name() {
            "nodes" => {
                let items: Vec<_> = (0..d.recs.len()).map(|i| cx.node(self.group, i)).collect();
                Ok(ResolvedValue::List(Box::new(items.into_iter())))
            }
            "edges" => {
                let edge_ty = format!("{}Edge", cx.plan.groups[self.group].declared);
                let items: Vec<Result<ResolvedValue<'a>, FieldError>> = (0..d.recs.len())
                    .map(|i| {
                        Ok(ResolvedValue::object(EdgeObj {
                            conn: self,
                            rec: i,
                            ty: edge_ty.clone(),
                        }))
                    })
                    .collect();
                Ok(ResolvedValue::List(Box::new(items.into_iter())))
            }
            "totalCount" => {
                let n = self.count.and_then(|c| cx.data[c].count).unwrap_or(0);
                Ok(leaf(J::from(n)))
            }
            "pageInfo" => {
                let mut map = Map::new();
                map.insert("hasNextPage".into(), d.page.has_next.into());
                map.insert("hasPreviousPage".into(), d.page.has_prev.into());
                map.insert(
                    "startCursor".into(),
                    if d.recs.is_empty() {
                        J::Null
                    } else {
                        self.cursor(0).into()
                    },
                );
                map.insert(
                    "endCursor".into(),
                    if d.recs.is_empty() {
                        J::Null
                    } else {
                        self.cursor(d.recs.len() - 1).into()
                    },
                );
                Ok(ResolvedValue::object(JsonObj {
                    ty: "PageInfo".into(),
                    map,
                }))
            }
            _ => Err(self.unknown_field_error(info)),
        }
    }
}

struct EdgeObj<'a> {
    conn: &'a ConnObj<'a>,
    rec: usize,
    ty: String,
}

impl ObjectValue for EdgeObj<'_> {
    fn type_name(&self) -> &str {
        &self.ty
    }
    fn resolve_field<'a>(
        &'a self,
        info: &'a ResolveInfo<'a>,
    ) -> Result<ResolvedValue<'a>, FieldError> {
        match info.field_name() {
            "cursor" => Ok(leaf(self.conn.cursor(self.rec).into())),
            "node" => self.conn.cx.node(self.conn.group, self.rec),
            _ => Err(self.unknown_field_error(info)),
        }
    }
}

struct NodeObj<'a> {
    cx: &'a Cx<'a>,
    group: usize,
    rec: usize,
    ty: String,
}

/// Values in the first language range that has any (§4.5).
fn by_lang<'t>(values: &'t [Term], ranges: &[String]) -> Vec<&'t Term> {
    if ranges.is_empty() || ranges.iter().any(|r| r == "*") && ranges.len() == 1 {
        return values.iter().collect();
    }
    for r in ranges {
        let hit: Vec<&Term> = values
            .iter()
            .filter(|v| match tag_of(v) {
                Some(tag) => r == "*" || lang_matches(tag, r),
                None => false,
            })
            .collect();
        if !hit.is_empty() {
            return hit;
        }
    }
    Vec::new()
}

impl NodeObj<'_> {
    fn rec(&self) -> &Rec {
        &self.cx.data[self.group].recs[self.rec]
    }

    fn err(&self, code: Code, msg: String, f: &FieldMap) -> FieldError {
        self.cx.fail(
            GqlError::new(code, msg)
                .with("node", id_of(&self.rec().node).unwrap_or_default())
                .with("predicate", f.predicate.as_str()),
        )
    }

    fn value<'b>(&'b self, t: &Term, f: &FieldMap) -> Result<ResolvedValue<'b>, FieldError> {
        let bad = |msg: String| {
            self.cx.fail(
                GqlError::new(Code::InvalidValue, msg)
                    .with("node", id_of(&self.rec().node).unwrap_or_default())
                    .with("predicate", f.predicate.as_str())
                    .with("value", t.to_string()),
            )
        };
        match &f.target {
            Target::Enum(e) => {
                let en = self.cx.c.mapping.enum_(e);
                match (t, en) {
                    (Term::NamedNode(n), Some(en)) => match en.value_of(n.as_str()) {
                        Some(v) => Ok(leaf(v.into())),
                        None => Err(bad(format!("{t} is no value of the enum {e}"))),
                    },
                    _ => Err(bad(format!("{t} is no value of the enum {e}"))),
                }
            }
            Target::Scalar(s) => match to_json(t, *s) {
                Ok(J::Object(map)) if s.is_object() => Ok(ResolvedValue::object(JsonObj {
                    ty: s.name().to_string(),
                    map,
                })),
                Ok(v) => Ok(leaf(v)),
                Err(e) => Err(bad(e.0)),
            },
            Target::Object(_) => Err(bad("not a value field".into())),
        }
    }

    fn single<'b>(
        &'b self,
        values: Vec<&Term>,
        f: &FieldMap,
    ) -> Result<ResolvedValue<'b>, FieldError> {
        match values.len() {
            0 if f.non_null => Err(self.err(
                Code::MissingValue,
                format!(
                    "{} has no value for the non-null field {}",
                    self.rec().node,
                    f.name
                ),
                f,
            )),
            0 => Ok(ResolvedValue::null()),
            1 => self.value(values[0], f),
            n if f.on_many == OnMany::Min => {
                let min = values
                    .into_iter()
                    .min_by(|a, b| term_cmp(a, b))
                    .expect("n > 1");
                let _ = n;
                self.value(min, f)
            }
            n => Err(self.cx.fail(
                GqlError::new(
                    Code::MultipleValues,
                    format!(
                        "{} has {n} values for the single-valued field {}",
                        self.rec().node,
                        f.name
                    ),
                )
                .with("node", id_of(&self.rec().node).unwrap_or_default())
                .with("predicate", f.predicate.as_str())
                .with("values", n),
            )),
        }
    }

    fn values_of(&self, fno: usize) -> &[Term] {
        let grp = &self.cx.plan.groups[self.group];
        grp.values
            .and_then(|vg| self.cx.data[vg].values.get(&self.rec().node))
            .and_then(|v| v.get(fno))
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }
}

fn slice<T>(v: Vec<T>, offset: u64, first: u64) -> Vec<T> {
    v.into_iter()
        .skip(offset as usize)
        .take(usize::try_from(first).unwrap_or(usize::MAX))
        .collect()
}

impl ObjectValue for NodeObj<'_> {
    fn type_name(&self) -> &str {
        &self.ty
    }

    fn resolve_field<'a>(
        &'a self,
        info: &'a ResolveInfo<'a>,
    ) -> Result<ResolvedValue<'a>, FieldError> {
        let cx = self.cx;
        let k = key(info.field_selections()[0]);
        let Some(fp) = cx.plan.groups[self.group].fields.get(&(self.ty.clone(), k)) else {
            if info.field_name() == "id" {
                return Ok(leaf(id_of(&self.rec().node).unwrap_or_default().into()));
            }
            return Err(self.unknown_field_error(info));
        };
        match fp {
            FieldPlan::Id => Ok(leaf(id_of(&self.rec().node).unwrap_or_default().into())),
            FieldPlan::Single { col, field, lang } => {
                let vals = by_lang(&self.rec().singles[*col], lang);
                self.single(vals, field)
            }
            FieldPlan::Multi {
                fno,
                field,
                lang,
                first,
                offset,
                desc,
            } => {
                let mut vals = by_lang(self.values_of(*fno), lang);
                if *desc {
                    vals.sort_by(|a, b| term_cmp(b, a));
                }
                let items: Vec<_> = slice(vals, *offset, *first)
                    .into_iter()
                    .map(|t| self.value(t, field))
                    .collect();
                Ok(ResolvedValue::List(Box::new(items.into_iter())))
            }
            FieldPlan::Types { fno } => {
                let items: Vec<Result<ResolvedValue<'a>, FieldError>> = self
                    .values_of(*fno)
                    .iter()
                    .filter_map(id_of)
                    .map(|s| Ok(leaf(s.into())))
                    .collect();
                Ok(ResolvedValue::List(Box::new(items.into_iter())))
            }
            FieldPlan::Object {
                group,
                field,
                first,
                offset,
            } => {
                let d = &cx.data[*group];
                let recs: &[usize] = d
                    .by_parent
                    .get(&self.rec().node)
                    .map(Vec::as_slice)
                    .unwrap_or(&[]);
                if field.list {
                    let items: Vec<_> = slice(recs.to_vec(), *offset, *first)
                        .into_iter()
                        .map(|r| cx.node(*group, r))
                        .collect();
                    return Ok(ResolvedValue::List(Box::new(items.into_iter())));
                }
                match recs.len() {
                    0 if field.non_null => Err(self.err(
                        Code::MissingValue,
                        format!(
                            "{} has no value for the non-null field {}",
                            self.rec().node,
                            field.name
                        ),
                        field,
                    )),
                    0 => Ok(ResolvedValue::null()),
                    1 => cx.node(*group, recs[0]),
                    _ if field.on_many == OnMany::Min => cx.node(*group, recs[0]),
                    n => Err(cx.fail(
                        GqlError::new(
                            Code::MultipleValues,
                            format!(
                                "{} has {n} values for the single-valued field {}",
                                self.rec().node,
                                field.name
                            ),
                        )
                        .with("node", id_of(&self.rec().node).unwrap_or_default())
                        .with("predicate", field.predicate.as_str())
                        .with("values", n),
                    )),
                }
            }
        }
    }
}

/// Assemble the response data from the groups' rows. Returns the data (`None` when a
/// null reached the root) and the errors.
#[allow(clippy::too_many_arguments)]
pub fn assemble(
    c: &Compiled,
    plan: &Plan,
    data: &[Data],
    doc: &Valid<ExecutableDocument>,
    op: &Operation,
    vars: &Valid<JsonMap>,
    max_nodes: u64,
    commit: u64,
) -> Result<(Option<J>, Vec<GqlError>, u64), GqlError> {
    let cx = Cx {
        c,
        plan,
        data,
        errors: RefCell::new(Vec::new()),
        nodes: Cell::new(0),
        max_nodes,
        over: Cell::new(false),
        commit,
    };
    let root = RootObj { cx: &cx };
    let resp = Execution::new(&c.api, doc)
        .implementers_map(&c.implementers)
        .operation(op)
        .coerced_variable_values(vars)
        .enable_schema_introspection(true)
        .execute_sync(&root)
        .map_err(|e| GqlError::new(Code::BadUserInput, e.message().to_string()))?;
    if cx.over.get() {
        return Err(GqlError::new(
            Code::TooComplex,
            format!("the response holds more than {max_nodes} nodes"),
        )
        .with("limit", max_nodes));
    }
    let ours = cx.errors.into_inner();
    let mut errors = Vec::new();
    for e in resp.errors {
        let msg = e
            .message
            .strip_prefix("resolver error: ")
            .unwrap_or(&e.message);
        let mut g = match msg.strip_prefix(MARK).and_then(|i| i.parse::<usize>().ok()) {
            Some(i) if i < ours.len() => ours[i].clone(),
            _ => GqlError::new(Code::InvalidValue, msg.to_string()),
        };
        g.path = e
            .path
            .iter()
            .map(|s| serde_json::to_value(s).unwrap_or(J::Null))
            .collect();
        g.locations = e.locations.iter().map(|l| (l.line, l.column)).collect();
        errors.push(g);
    }
    let data = resp
        .data
        .map(|d| serde_json::to_value(d).unwrap_or(J::Null));
    Ok((data, errors, cx.nodes.get()))
}

/// `extensions.sparkles.plan` (§6.4).
pub fn explain(plan: &Plan, data: &[Data]) -> J {
    J::Array(
        plan.groups
            .iter()
            .zip(data)
            .enumerate()
            .map(|(i, (g, d))| {
                json!({
                    "group": i,
                    "path": g.path,
                    "parent": g.parent,
                    "kind": match g.kind {
                        GroupKind::Lookup { .. } => "lookup",
                        GroupKind::Node { .. } => "node",
                        GroupKind::Collection { .. } => "collection",
                        GroupKind::Count { .. } => "count",
                        GroupKind::Child { .. } => "child",
                        GroupKind::Values => "values",
                    },
                    "sparql": d.sparql,
                    "rows": d.rows,
                    "ms": (d.ms * 1000.0).round() / 1000.0,
                    "skipped": d.skipped,
                })
            })
            .collect(),
    )
}

//! Materialization (spec F19 §5): the plan, the joins and the quad sink.
//!
//! Each logical source is read once, and every triples map that reads it generates its
//! triples from the same rows. A referencing object map takes the parent's subject from
//! the same row when the parent reads the same logical source without join conditions.
//! When both sides are tables of one SQL connection, the join runs in the database as
//! one statement. Otherwise the parent's subjects are built into a hash table keyed by
//! the parent's join values, charged to the memory budget, and the child's rows probe
//! it.

use super::functions;
use super::model::{Dt, Lang, Mapping, ObjectMap, SourceKind, TermMap, TermType, Value};
use super::source::{self, Cell, Dt as Nat, Row};
use super::template::{Part, iri_safe, uri_safe};
use super::{DEFAULT_GRAPH_RML, DEFAULT_GRAPH_RR, Inputs, RunOptions};
use crate::error::{Error, Result};
use oxrdf::vocab::rdf;
use oxrdf::{BlankNode, GraphName, Literal, NamedNode, NamedOrBlankNode, Quad, Term};
use rustc_hash::FxHashMap;
use std::rc::Rc;

fn base_of(b: &Option<String>) -> Option<Rc<str>> {
    b.as_deref().map(Rc::from)
}

/// What a run did.
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct Stats {
    /// Quads written (a quad generated twice is counted twice; the loader removes the
    /// copy).
    pub quads: u64,
    /// Rows or iterations read, over all sources and joins.
    pub rows: u64,
    pub warnings: Vec<String>,
    /// How each join ran.
    pub joins: Vec<String>,
}

/// Checks that need no data: references to triples maps, join conditions, term types
/// and functions.
pub fn validate(m: &Mapping) -> Result<()> {
    for tm in &m.maps {
        let fail = |what: String| Err(Error::invalid(format!("triples map {}: {what}", tm.id)));
        if tm.subject.term_type == TermType::Literal {
            return fail("a subject map cannot generate literals".into());
        }
        check_term_map(&tm.subject, &tm.id)?;
        for g in &tm.graphs {
            if g.term_type != TermType::Iri {
                return fail("a graph map generates IRIs only".into());
            }
            check_term_map(g, &tm.id)?;
        }
        for po in &tm.predicate_objects {
            if po.predicates.is_empty() {
                return fail("a predicate-object map has no predicate".into());
            }
            if po.objects.is_empty() {
                return fail("a predicate-object map has no object".into());
            }
            for p in &po.predicates {
                if p.term_type != TermType::Iri {
                    return fail("a predicate map generates IRIs only".into());
                }
                check_term_map(p, &tm.id)?;
            }
            for g in &po.graphs {
                if g.term_type != TermType::Iri {
                    return fail("a graph map generates IRIs only".into());
                }
                check_term_map(g, &tm.id)?;
            }
            for o in &po.objects {
                match o {
                    ObjectMap::Term(t) => check_term_map(t, &tm.id)?,
                    ObjectMap::Join { parent, conditions } => {
                        let Some(p) = m.maps.get(*parent) else {
                            return fail("a referencing object map names a missing parent".into());
                        };
                        if conditions.is_empty() && p.source != tm.source {
                            return fail(format!(
                                "the join with {} needs a join condition, since the two read different sources",
                                p.id
                            ));
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

fn check_term_map(t: &TermMap, map: &str) -> Result<()> {
    if let Value::Function(f) = &t.value {
        let Some(def) = functions::lookup(f.function.as_str()) else {
            return Err(Error::invalid(format!(
                "triples map {map}: the function <{}> is not one Sparkles implements (see `sparkles map check --functions`); a CONSTRUCT template can use every SPARQL function",
                f.function.as_str()
            )));
        };
        for (p, v) in &f.args {
            if !def.params.iter().any(|q| q == p.as_str()) {
                return Err(Error::invalid(format!(
                    "triples map {map}: the function <{}> has no parameter <{}>",
                    f.function.as_str(),
                    p.as_str()
                )));
            }
            check_term_map(v, map)?;
        }
        for q in def.params.iter().take(def.required) {
            if !f.args.iter().any(|(p, _)| p.as_str() == q) {
                return Err(Error::invalid(format!(
                    "triples map {map}: the function <{}> needs the parameter <{q}>",
                    f.function.as_str()
                )));
            }
        }
    }
    if let Some(Dt::Map(d)) = &t.datatype {
        check_term_map(d, map)?;
    }
    if let Some(Lang::Map(l)) = &t.language {
        check_term_map(l, map)?;
    }
    Ok(())
}

// ------------------------------------------------------------------- compiling ----

/// References of one scan, interned to indexes into the source's reference list.
#[derive(Default)]
struct Refs {
    list: Vec<String>,
    /// (side, reference) → index; side 0 is the scan's own source, 1 the parent of a
    /// pushed-down join
    index: FxHashMap<(u8, String), usize>,
}

impl Refs {
    fn get(&mut self, side: u8, r: &str) -> usize {
        if let Some(&i) = self.index.get(&(side, r.to_string())) {
            return i;
        }
        self.list.push(r.to_string());
        self.index
            .insert((side, r.to_string()), self.list.len() - 1);
        self.list.len() - 1
    }
}

#[derive(Clone, Debug)]
enum CValue {
    Const(Term),
    Ref(usize),
    /// the parts, and whether values are URI-safe rather than IRI-safe encoded
    Tpl(Vec<CPart>, bool),
    Fn(String, Vec<(usize, CTerm)>),
    Fresh,
}

#[derive(Clone, Debug)]
enum CPart {
    Text(String),
    Ref(usize),
}

#[derive(Clone, Debug)]
struct CTerm {
    value: CValue,
    tt: TermType,
    dt: Option<CDt>,
    lang: Option<CLang>,
    desc: String,
    /// a reference's natural datatype applies (R2RML: column-valued literals)
    natural: bool,
}

#[derive(Clone, Debug)]
enum CDt {
    Const(NamedNode),
    Map(Box<CTerm>),
}

#[derive(Clone, Debug)]
enum CLang {
    Const(String),
    Map(Box<CTerm>),
}

fn compile(t: &TermMap, refs: &mut Refs, side: u8) -> CTerm {
    let value = match &t.value {
        Value::Constant(c) => CValue::Const(c.clone()),
        Value::Reference(r) => CValue::Ref(refs.get(side, r)),
        Value::Template(tpl) => CValue::Tpl(
            tpl.parts
                .iter()
                .map(|p| match p {
                    Part::Text(s) => CPart::Text(s.clone()),
                    Part::Ref(r) => CPart::Ref(refs.get(side, r)),
                })
                .collect(),
            tpl.uri_safe(),
        ),
        Value::Function(f) => {
            let def = functions::lookup(f.function.as_str());
            let params = def.map(|d| d.params).unwrap_or_default();
            CValue::Fn(
                f.function.as_str().to_string(),
                f.args
                    .iter()
                    .map(|(p, v)| {
                        (
                            params.iter().position(|q| q == p.as_str()).unwrap_or(0),
                            compile(v, refs, side),
                        )
                    })
                    .collect(),
            )
        }
        Value::Fresh => CValue::Fresh,
    };
    CTerm {
        natural: matches!(t.value, Value::Reference(_) | Value::Function(_)),
        value,
        tt: t.term_type,
        dt: t.datatype.as_ref().map(|d| match d {
            Dt::Constant(n) => CDt::Const(n.clone()),
            Dt::Map(m) => CDt::Map(Box::new(compile(m, refs, side))),
        }),
        lang: t.language.as_ref().map(|l| match l {
            Lang::Constant(s) => CLang::Const(s.clone()),
            Lang::Map(m) => CLang::Map(Box::new(compile(m, refs, side))),
        }),
        desc: t.describe(),
    }
}

/// How an object of a scan's row is made.
#[derive(Clone, Debug)]
enum CObj {
    Term(CTerm),
    /// the parent's subject from the same row
    SameRow(CTerm, Option<Rc<str>>),
    /// a probe of hash table `table` with the child's join values
    Probe {
        table: usize,
        keys: Vec<usize>,
    },
    /// the parent's subject, from the parent's columns of a pushed-down join scan
    Joined(CTerm, Option<Rc<str>>),
    /// made by another scan (a pushed-down join)
    Skip,
}

#[derive(Clone, Debug)]
struct CPo {
    preds: Vec<CTerm>,
    objs: Vec<CObj>,
    graphs: Vec<CTerm>,
}

#[derive(Clone, Debug)]
struct CMap {
    id: String,
    base: Option<Rc<str>>,
    subject: CTerm,
    classes: Vec<NamedNode>,
    graphs: Vec<CTerm>,
    pos: Vec<CPo>,
}

/// A parent table of an in-memory join.
struct Table {
    parent: usize,
    keys: Vec<String>,
    rows: FxHashMap<Vec<String>, Vec<Term>>,
    desc: String,
}

/// One read of a source.
struct Scan {
    kind: ScanKind,
    refs: Refs,
    maps: Vec<CMap>,
}

enum ScanKind {
    Source(usize),
    /// a join of two SQL tables on one connection
    Join {
        child_source: usize,
        parent_source: usize,
        child_refs: usize,
        conditions: Vec<(String, String)>,
    },
}

fn is_sql(m: &Mapping, s: usize) -> Option<&str> {
    match &m.sources[s].kind {
        SourceKind::Sql { connection, .. } => Some(connection),
        _ => None,
    }
}

/// The plan of a mapping: the scans and the parent tables they need.
struct Plan {
    scans: Vec<Scan>,
    tables: Vec<Table>,
    /// how each join runs, for messages and `check`
    joins: Vec<String>,
}

fn plan(m: &Mapping) -> Plan {
    let mut tables: Vec<Table> = Vec::new();
    let mut joins = Vec::new();
    let mut scans: Vec<Scan> = Vec::new();
    for s in 0..m.sources.len() {
        let mut refs = Refs::default();
        let mut maps = Vec::new();
        for (mi, tm) in m.maps.iter().enumerate().filter(|(_, t)| t.source == s) {
            let subject = compile(&tm.subject, &mut refs, 0);
            let graphs = tm.graphs.iter().map(|g| compile(g, &mut refs, 0)).collect();
            let mut pos = Vec::new();
            for (pi, po) in tm.predicate_objects.iter().enumerate() {
                let preds = po
                    .predicates
                    .iter()
                    .map(|p| compile(p, &mut refs, 0))
                    .collect();
                let pgraphs = po.graphs.iter().map(|g| compile(g, &mut refs, 0)).collect();
                let mut objs = Vec::new();
                for o in &po.objects {
                    objs.push(match o {
                        ObjectMap::Term(t) => CObj::Term(compile(t, &mut refs, 0)),
                        ObjectMap::Join { parent, conditions } => {
                            let p = &m.maps[*parent];
                            if conditions.is_empty() && p.source == s {
                                joins.push(format!(
                                    "{} → {}: the parent's subject from the same row",
                                    tm.id, p.id
                                ));
                                CObj::SameRow(compile(&p.subject, &mut refs, 0), base_of(&p.base))
                            } else if let (Some(a), Some(b)) = (is_sql(m, s), is_sql(m, p.source))
                                && a == b
                            {
                                joins.push(format!(
                                    "{} → {}: one SQL statement on connection {}: {}",
                                    tm.id,
                                    p.id,
                                    if a.is_empty() { "default" } else { a },
                                    join_sketch(m, s, p.source, conditions)
                                ));
                                scans.push(join_scan(m, mi, pi, *parent, conditions));
                                CObj::Skip
                            } else {
                                let keys: Vec<String> =
                                    conditions.iter().map(|(_, b)| b.clone()).collect();
                                let t = match tables
                                    .iter()
                                    .position(|t| t.parent == *parent && t.keys == keys)
                                {
                                    Some(t) => t,
                                    None => {
                                        tables.push(Table {
                                            parent: *parent,
                                            keys: keys.clone(),
                                            rows: FxHashMap::default(),
                                            desc: format!("the join of {} with {}", tm.id, p.id),
                                        });
                                        tables.len() - 1
                                    }
                                };
                                joins.push(format!(
                                    "{} → {}: an in-memory hash table of {}'s subjects by {}",
                                    tm.id,
                                    p.id,
                                    p.id,
                                    keys.join(", ")
                                ));
                                CObj::Probe {
                                    table: t,
                                    keys: conditions.iter().map(|(a, _)| refs.get(0, a)).collect(),
                                }
                            }
                        }
                    });
                }
                pos.push(CPo {
                    preds,
                    objs,
                    graphs: pgraphs,
                });
            }
            maps.push(CMap {
                id: tm.id.clone(),
                base: base_of(&tm.base),
                subject,
                classes: tm.classes.clone(),
                graphs,
                pos,
            });
        }
        if !maps.is_empty() {
            scans.push(Scan {
                kind: ScanKind::Source(s),
                refs,
                maps,
            });
        }
    }
    Plan {
        scans,
        tables,
        joins,
    }
}

/// The scan of a join pushed into SQL: the child's subject, predicate and graphs with
/// the parent's subject as the object.
fn join_scan(
    m: &Mapping,
    mi: usize,
    pi: usize,
    parent: usize,
    conditions: &[(String, String)],
) -> Scan {
    let tm = &m.maps[mi];
    let po = &tm.predicate_objects[pi];
    let p = &m.maps[parent];
    let mut refs = Refs::default();
    let subject = compile(&tm.subject, &mut refs, 0);
    let graphs = tm.graphs.iter().map(|g| compile(g, &mut refs, 0)).collect();
    let preds = po
        .predicates
        .iter()
        .map(|x| compile(x, &mut refs, 0))
        .collect();
    let pgraphs = po.graphs.iter().map(|g| compile(g, &mut refs, 0)).collect();
    // the child's references come first in the statement
    let child_refs = refs.list.len();
    let obj = compile(&p.subject, &mut refs, 1);
    Scan {
        kind: ScanKind::Join {
            child_source: tm.source,
            parent_source: p.source,
            child_refs,
            conditions: conditions.to_vec(),
        },
        refs,
        maps: vec![CMap {
            id: tm.id.clone(),
            base: base_of(&tm.base),
            subject,
            classes: Vec::new(),
            graphs,
            pos: vec![CPo {
                preds,
                objs: vec![CObj::Joined(obj, base_of(&p.base))],
                graphs: pgraphs,
            }],
        }],
    }
}

fn sql_text(m: &Mapping, s: usize) -> String {
    match &m.sources[s].kind {
        SourceKind::Sql { query, .. } => match query {
            super::model::SqlQuery::Table(t) => format!("SELECT * FROM {t}"),
            super::model::SqlQuery::Query(q) => q.trim().trim_end_matches(';').to_string(),
        },
        _ => String::new(),
    }
}

/// The statement of a pushed-down join, before its columns are resolved.
fn join_sketch(
    m: &Mapping,
    child: usize,
    parent: usize,
    conditions: &[(String, String)],
) -> String {
    format!(
        "SELECT … FROM ({}) AS c JOIN ({}) AS p ON {}",
        sql_text(m, child),
        sql_text(m, parent),
        conditions
            .iter()
            .map(|(a, b)| format!("c.{a} = p.{b}"))
            .collect::<Vec<_>>()
            .join(" AND ")
    )
}

/// How each join of a mapping would run, for `sparkles map check`.
pub fn describe_joins(m: &Mapping) -> Vec<String> {
    plan(m).joins
}

// ------------------------------------------------------------------- generating ----

struct Gen<'a> {
    base: Option<&'a str>,
    /// the base of the triples map being generated, over `base`
    map_base: Option<Rc<str>>,
    fresh: u64,
    only_graph: Option<&'a NamedNode>,
}

/// An error about a row, naming the triples map, the row and the term map.
fn data_error(map: &str, row: &dyn Row, t: &CTerm, m: impl std::fmt::Display) -> Error {
    Error::invalid(format!(
        "triples map {map}: {}: {}: {m}",
        row.position(),
        t.desc
    ))
}

fn blank_label(s: &str) -> String {
    let mut out = String::from("m");
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("_{b:02x}"));
        }
    }
    out
}

impl Gen<'_> {
    /// The string values of a term map's value: none when a reference is NULL.
    fn values(
        &mut self,
        t: &CTerm,
        row: &dyn Row,
        map: &str,
        out: &mut Vec<(String, Nat)>,
    ) -> Result<()> {
        match &t.value {
            CValue::Const(c) => {
                let s = match c {
                    Term::NamedNode(n) => n.as_str().to_string(),
                    Term::BlankNode(b) => b.as_str().to_string(),
                    Term::Literal(l) => l.value().to_string(),
                    #[allow(unreachable_patterns)]
                    _ => c.to_string(),
                };
                out.push((s, Nat::Str));
            }
            CValue::Ref(i) => {
                let mut cells = Vec::new();
                row.get(*i, &mut cells)?;
                for c in cells {
                    out.push((c.lex.into_owned(), c.dt));
                }
            }
            CValue::Tpl(parts, uri) => {
                let iri = t.tt == TermType::Iri;
                let mut acc: Vec<String> = vec![String::new()];
                for p in parts {
                    match p {
                        CPart::Text(s) => acc.iter_mut().for_each(|a| a.push_str(s)),
                        CPart::Ref(i) => {
                            let mut cells = Vec::new();
                            row.get(*i, &mut cells)?;
                            if cells.is_empty() {
                                return Ok(());
                            }
                            let mut next = Vec::with_capacity(acc.len() * cells.len());
                            for a in &acc {
                                for c in &cells {
                                    let mut s = a.clone();
                                    if iri && *uri {
                                        uri_safe(&c.lex, &mut s);
                                    } else if iri {
                                        iri_safe(&c.lex, &mut s);
                                    } else {
                                        s.push_str(&c.lex);
                                    }
                                    next.push(s);
                                }
                            }
                            acc = next;
                        }
                    }
                }
                out.extend(acc.into_iter().map(|s| (s, Nat::Str)));
            }
            CValue::Fn(iri, args) => {
                let n = args.iter().map(|(k, _)| k + 1).max().unwrap_or(0);
                let mut vals: Vec<Vec<String>> = vec![Vec::new(); n];
                for (k, a) in args {
                    let mut v = Vec::new();
                    self.values(a, row, map, &mut v)?;
                    vals[*k].extend(v.into_iter().map(|(s, _)| s));
                }
                let r = functions::call(iri, &vals).map_err(|e| data_error(map, row, t, e))?;
                for c in r {
                    out.push((c.lex.into_owned(), c.dt));
                }
            }
            CValue::Fresh => {
                self.fresh += 1;
                out.push((format!("f{}", self.fresh), Nat::Str));
            }
        }
        Ok(())
    }

    /// The terms a term map generates for a row.
    fn terms(&mut self, t: &CTerm, row: &dyn Row, map: &str, out: &mut Vec<Term>) -> Result<()> {
        if let CValue::Const(c) = &t.value {
            out.push(c.clone());
            return Ok(());
        }
        let mut vals = Vec::new();
        self.values(t, row, map, &mut vals)?;
        for (s, nat) in vals {
            out.push(match t.tt {
                TermType::Iri => Term::NamedNode(self.iri(s, t, row, map)?),
                TermType::BlankNode => Term::BlankNode(if matches!(t.value, CValue::Fresh) {
                    BlankNode::new_unchecked(s)
                } else {
                    BlankNode::new_unchecked(blank_label(&s))
                }),
                TermType::Literal => Term::Literal(self.literal(s, nat, t, row, map)?),
            });
        }
        Ok(())
    }

    fn iri(&self, s: String, t: &CTerm, row: &dyn Row, map: &str) -> Result<NamedNode> {
        if oxiri::Iri::parse(s.as_str()).is_ok() {
            return NamedNode::new(s).map_err(|e| data_error(map, row, t, e));
        }
        let Some(base) = self.map_base.as_deref().or(self.base) else {
            return Err(data_error(
                map,
                row,
                t,
                format!("{s:?} is not an absolute IRI, and the mapping has no base IRI"),
            ));
        };
        let abs = format!("{base}{s}");
        if oxiri::Iri::parse(abs.as_str()).is_err() {
            return Err(data_error(
                map,
                row,
                t,
                format!("{s:?} does not make a valid IRI with the base {base}"),
            ));
        }
        NamedNode::new(abs).map_err(|e| data_error(map, row, t, e))
    }

    fn literal(
        &mut self,
        s: String,
        nat: Nat,
        t: &CTerm,
        row: &dyn Row,
        map: &str,
    ) -> Result<Literal> {
        if let Some(l) = &t.lang {
            let tag = match l {
                CLang::Const(c) => c.clone(),
                CLang::Map(m) => {
                    let mut v = Vec::new();
                    self.values(m, row, map, &mut v)?;
                    match v.into_iter().next() {
                        Some((s, _)) => s,
                        None => return Ok(Literal::new_simple_literal(s)),
                    }
                }
            };
            return Literal::new_language_tagged_literal(s, &tag).map_err(|e| {
                data_error(
                    map,
                    row,
                    t,
                    format!("{tag:?} is not a well-formed language tag: {e}"),
                )
            });
        }
        let dt = match &t.dt {
            Some(CDt::Const(n)) => Some(n.clone()),
            Some(CDt::Map(m)) => {
                let mut v = Vec::new();
                self.values(m, row, map, &mut v)?;
                match v.into_iter().next() {
                    Some((d, _)) => Some(self.iri(d, m, row, map)?),
                    None => None,
                }
            }
            None => None,
        };
        if let Some(dt) = dt {
            let l = Literal::new_typed_literal(s, dt);
            if l.datatype()
                .as_str()
                .starts_with("http://www.w3.org/2001/XMLSchema#")
                && !crate::xsd::is_valid(&l)
            {
                return Err(data_error(
                    map,
                    row,
                    t,
                    format!("{l} is not a valid literal of its datatype"),
                ));
            }
            return Ok(l);
        }
        if !t.natural {
            return Ok(Literal::new_simple_literal(s));
        }
        Ok(match nat {
            Nat::Str => Literal::new_simple_literal(s),
            Nat::Lang(tag) => Literal::new_language_tagged_literal_unchecked(s, tag),
            other => match other.xsd() {
                Some(x) => Literal::new_typed_literal(s, NamedNode::new_unchecked(x)),
                None => Literal::new_simple_literal(s),
            },
        })
    }

    fn graph_names(
        &mut self,
        gs: &[CTerm],
        row: &dyn Row,
        map: &str,
        out: &mut Vec<GraphName>,
    ) -> Result<()> {
        let mut terms = Vec::new();
        for g in gs {
            terms.clear();
            self.terms(g, row, map, &mut terms)?;
            for t in terms.drain(..) {
                if let Term::NamedNode(n) = t {
                    if n.as_str() == DEFAULT_GRAPH_RR || n.as_str() == DEFAULT_GRAPH_RML {
                        out.push(GraphName::DefaultGraph);
                    } else {
                        out.push(GraphName::NamedNode(n));
                    }
                }
            }
        }
        Ok(())
    }

    /// The quads of map `cm` for one row.
    fn row(
        &mut self,
        cm: &CMap,
        row: &dyn Row,
        tables: &[Table],
        sink: &mut dyn FnMut(Quad) -> Result<()>,
        stats: &mut Stats,
    ) -> Result<()> {
        self.map_base = cm.base.clone();
        let mut subjects = Vec::new();
        self.terms(&cm.subject, row, &cm.id, &mut subjects)?;
        if subjects.is_empty() {
            return Ok(());
        }
        let subjects: Vec<NamedOrBlankNode> = subjects
            .into_iter()
            .map(|t| match t {
                Term::NamedNode(n) => Ok(n.into()),
                Term::BlankNode(b) => Ok(b.into()),
                other => Err(data_error(
                    &cm.id,
                    row,
                    &cm.subject,
                    format!("{other} cannot be a subject"),
                )),
            })
            .collect::<Result<_>>()?;
        let mut sgraphs = Vec::new();
        self.graph_names(&cm.graphs, row, &cm.id, &mut sgraphs)?;
        let only_graph = self.only_graph;
        let mut emit = |s: &NamedOrBlankNode,
                        p: &NamedNode,
                        o: &Term,
                        g: &GraphName,
                        stats: &mut Stats|
         -> Result<()> {
            let g = match (only_graph, g) {
                (Some(only), GraphName::DefaultGraph) => GraphName::NamedNode(only.clone()),
                (Some(only), GraphName::NamedNode(n)) if n == only => g.clone(),
                (Some(_), g) => {
                    return Err(Error::invalid(format!(
                        "mapping-graph-conflict: triples map {} writes into the graph {g}, and the request addresses one graph",
                        cm.id
                    )));
                }
                (None, g) => g.clone(),
            };
            stats.quads += 1;
            sink(Quad::new(s.clone(), p.clone(), o.clone(), g))
        };
        let default = [GraphName::DefaultGraph];
        let class_graphs: &[GraphName] = if sgraphs.is_empty() {
            &default
        } else {
            &sgraphs
        };
        for c in &cm.classes {
            let o = Term::NamedNode(c.clone());
            for s in &subjects {
                for g in class_graphs {
                    emit(s, &rdf::TYPE.into_owned(), &o, g, stats)?;
                }
            }
        }
        let mut preds = Vec::new();
        let mut objs = Vec::new();
        let mut graphs = Vec::new();
        let mut keyvals: Vec<Vec<Cell<'_>>> = Vec::new();
        for po in &cm.pos {
            preds.clear();
            for p in &po.preds {
                self.terms(p, row, &cm.id, &mut preds)?;
            }
            if preds.is_empty() {
                continue;
            }
            objs.clear();
            for o in &po.objs {
                match o {
                    CObj::Term(t) => self.terms(t, row, &cm.id, &mut objs)?,
                    CObj::SameRow(t, base) | CObj::Joined(t, base) => {
                        // the parent's subject, under the parent's base
                        let own = std::mem::replace(&mut self.map_base, base.clone());
                        let r = self.terms(t, row, &cm.id, &mut objs);
                        self.map_base = own;
                        r?
                    }
                    CObj::Probe { table, keys } => {
                        keyvals.clear();
                        for k in keys {
                            let mut v = Vec::new();
                            row.get(*k, &mut v)?;
                            keyvals.push(v);
                        }
                        if keyvals.iter().any(Vec::is_empty) {
                            continue;
                        }
                        // every combination of the values of multi-valued keys
                        let mut combos: Vec<Vec<String>> = vec![Vec::new()];
                        for v in &keyvals {
                            let mut next = Vec::new();
                            for c in &combos {
                                for x in v {
                                    let mut c = c.clone();
                                    c.push(x.lex.to_string());
                                    next.push(c);
                                }
                            }
                            combos = next;
                        }
                        for c in combos {
                            if let Some(ts) = tables[*table].rows.get(&c) {
                                objs.extend(ts.iter().cloned());
                            }
                        }
                    }
                    CObj::Skip => {}
                }
            }
            if objs.is_empty() {
                continue;
            }
            graphs.clear();
            graphs.extend(sgraphs.iter().cloned());
            self.graph_names(&po.graphs, row, &cm.id, &mut graphs)?;
            if graphs.is_empty() {
                graphs.push(GraphName::DefaultGraph);
            }
            for s in &subjects {
                for p in &preds {
                    let Term::NamedNode(p) = p else {
                        continue;
                    };
                    for o in &objs {
                        for g in &graphs {
                            emit(s, p, o, g, stats)?;
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

/// The bytes a parent table's entry is charged: its key, its subjects and a fixed
/// overhead for the hash table.
fn entry_bytes(key: &[String], t: &Term) -> u64 {
    let k: usize = key.iter().map(|s| s.len() + 24).sum();
    let v = match t {
        Term::NamedNode(n) => n.as_str().len(),
        Term::BlankNode(b) => b.as_str().len(),
        other => other.to_string().len(),
    };
    (k + v + 64) as u64
}

/// Run a mapping: read every source once and hand each quad to `sink`. A failed run may
/// have handed some quads to the sink; the callers write them to a temporary file that
/// a failure discards, so nothing is committed.
pub fn run(
    m: &Mapping,
    inputs: &Inputs,
    opts: &RunOptions,
    sink: &mut dyn FnMut(Quad) -> Result<()>,
) -> Result<Stats> {
    let mut p = plan(m);
    let mut stats = Stats {
        warnings: m.warnings.clone(),
        joins: p.joins.clone(),
        ..Default::default()
    };
    let base = opts.base.as_deref().or(m.base.as_deref());
    let mut g = Gen {
        base,
        map_base: None,
        fresh: 0,
        only_graph: opts.only_graph.as_ref(),
    };
    // the parent tables of in-memory joins, before the children
    let mut used = 0u64;
    for ti in 0..p.tables.len() {
        let parent = p.tables[ti].parent;
        let pm = &m.maps[parent];
        let mut refs = Refs::default();
        let subject = compile(&pm.subject, &mut refs, 0);
        g.map_base = base_of(&pm.base);
        let keys: Vec<usize> = p.tables[ti].keys.iter().map(|k| refs.get(0, k)).collect();
        let mut src = source::open(&m.sources[pm.source], &refs.list, inputs, opts)?;
        let mut rows: FxHashMap<Vec<String>, Vec<Term>> = FxHashMap::default();
        let mut n = 0u64;
        let desc = p.tables[ti].desc.clone();
        let budget = opts.memory_budget;
        let mut terms = Vec::new();
        src.scan(&mut |row| {
            n += 1;
            terms.clear();
            g.terms(&subject, row, &pm.id, &mut terms)?;
            if terms.is_empty() {
                return Ok(());
            }
            let mut key = Vec::with_capacity(keys.len());
            for k in &keys {
                let mut v = Vec::new();
                row.get(*k, &mut v)?;
                // NULL never joins; a multi-valued key keeps its first value
                let Some(c) = v.into_iter().next() else {
                    return Ok(());
                };
                key.push(c.lex.into_owned());
            }
            for t in &terms {
                used += entry_bytes(&key, t);
            }
            if let Some(b) = budget
                && used > b
            {
                return Err(Error::invalid(format!(
                    "{desc} needs more than the memory budget of {} for the parent's subjects, after {n} parent rows; put both sides on one database so the join runs there, or raise the budget",
                    crate::error::human_bytes(b)
                )));
            }
            rows.entry(key).or_default().extend(terms.drain(..));
            Ok(())
        })?;
        stats.rows += n;
        p.tables[ti].rows = rows;
    }
    let tables = std::mem::take(&mut p.tables);
    for scan in &p.scans {
        let mut src = match &scan.kind {
            ScanKind::Source(s) => source::open(&m.sources[*s], &scan.refs.list, inputs, opts)?,
            ScanKind::Join {
                child_source,
                parent_source,
                child_refs,
                conditions,
            } => open_join(
                m,
                *child_source,
                *parent_source,
                &scan.refs.list,
                *child_refs,
                conditions,
                inputs,
                opts,
            )?,
        };
        let mut n = 0u64;
        src.scan(&mut |row| {
            n += 1;
            for cm in &scan.maps {
                g.row(cm, row, &tables, sink, &mut stats)?;
            }
            Ok(())
        })?;
        stats.rows += n;
    }
    Ok(stats)
}

#[allow(clippy::too_many_arguments)]
fn open_join(
    m: &Mapping,
    child: usize,
    parent: usize,
    refs: &[String],
    child_refs: usize,
    conditions: &[(String, String)],
    inputs: &Inputs,
    opts: &RunOptions,
) -> Result<Box<dyn source::RowSource>> {
    let (
        SourceKind::Sql {
            connection,
            query: cq,
        },
        SourceKind::Sql { query: pq, .. },
    ) = (&m.sources[child].kind, &m.sources[parent].kind)
    else {
        return Err(Error::invalid("a pushed-down join needs two SQL sources"));
    };
    let conn = inputs.connection(connection)?;
    #[cfg(feature = "mapping-sqlite")]
    {
        let (src, _sql) = source::sqlite::open_join(
            conn,
            cq,
            pq,
            &refs[..child_refs],
            &refs[child_refs..],
            conditions,
            opts,
        )?;
        Ok(src)
    }
    #[cfg(not(feature = "mapping-sqlite"))]
    {
        let _ = (conn, cq, pq, refs, child_refs, conditions, opts);
        Err(Error::Unsupported(
            "this build has no SQL sources (cargo feature mapping-sqlite)".into(),
        ))
    }
}

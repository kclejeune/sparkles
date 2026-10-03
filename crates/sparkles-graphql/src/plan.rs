//! The planner (§6.2, §7.1): walks an operation's selection tree, after fragments,
//! `@skip` and `@include` are applied, and splits it into fetch groups. A group is one
//! SPARQL query; the number of groups depends on the document only. The walk also
//! measures depth, counts groups and estimates the nodes the response can hold, and
//! refuses a document over a limit before anything runs.

use crate::Compiled;
use crate::cursor::{self, Cursor};
use crate::error::{Code, GqlError};
use crate::mapping::{FieldMap, RootKind, Target};
use crate::scalars::{RDF_TYPE, parse_id};
use apollo_compiler::ast::Value;
use apollo_compiler::collections::{HashSet as AHashSet, IndexMap};
use apollo_compiler::executable::{Field, Operation, Selection};
use apollo_compiler::response::JsonMap;
use apollo_compiler::schema::ExtendedType;
use apollo_compiler::{ExecutableDocument, Name};
use oxrdf::{NamedNode, Term};
use serde_json::{Map, Value as J};
use std::collections::HashMap;

/// The identity of a field selection: the address of its `executable::Field`, which the
/// executor hands back when it resolves the field.
pub type Key = usize;

pub fn key(f: &Field) -> Key {
    f as *const Field as usize
}

/// The server's ceilings, which a schema's `limits` may lower.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub max_depth: u32,
    pub max_nodes: u64,
    pub default_first: u32,
    pub max_first: u32,
    pub max_groups: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            max_depth: 12,
            max_nodes: 100_000,
            default_first: 100,
            max_first: 1000,
            max_groups: 64,
        }
    }
}

impl Limits {
    /// The server's ceilings lowered by a schema's limits.
    pub fn with(self, s: &crate::config::Limits) -> Limits {
        Limits {
            max_depth: s
                .max_depth
                .map_or(self.max_depth, |v| v.min(self.max_depth)),
            max_nodes: s
                .max_nodes
                .map_or(self.max_nodes, |v| v.min(self.max_nodes)),
            max_first: s
                .max_first
                .map_or(self.max_first, |v| v.min(self.max_first)),
            default_first: s
                .default_first
                .map_or(self.default_first, |v| v.min(self.default_first))
                .min(s.max_first.unwrap_or(u32::MAX).min(self.max_first)),
            max_groups: self.max_groups,
        }
    }
}

/// One key of an order: a predicate's value (`None`: the node itself), and the
/// direction.
#[derive(Clone, Debug)]
pub struct OrderKey {
    pub pred: Option<NamedNode>,
    pub desc: bool,
}

/// The slice of a connection or a list, before it is resolved against the count.
#[derive(Clone, Debug, Default)]
pub struct Window {
    pub after: Option<u64>,
    pub before: Option<u64>,
    pub first: Option<u64>,
    pub last: Option<u64>,
    pub offset: u64,
    /// the cursor hash of this field (connections)
    pub hash: String,
}

#[derive(Clone, Debug)]
pub enum GroupKind {
    /// `t(id:)`: the node when it is a member of the type
    Lookup { ty: String, id: Term },
    /// `node(id:)`: the node when it appears in a visible triple
    Node { id: Term },
    /// `allT` and list root fields: a page of the members
    Collection {
        ty: String,
        filter: Option<J>,
        order: Vec<OrderKey>,
        window: Window,
        connection: bool,
        /// the count group the window needs (`last` without `before`)
        count: Option<usize>,
    },
    /// `totalCount`
    Count { ty: String, filter: Option<J> },
    /// an object field of the parent group's nodes
    Child {
        pred: NamedNode,
        inverse: bool,
        /// the mapped type the filter applies to, with the filter
        filter: Option<(String, J)>,
        order: Vec<OrderKey>,
    },
    /// the multi-valued value fields of the parent group's nodes
    Values,
}

/// How a field of a node is answered.
#[derive(Clone, Debug)]
pub enum FieldPlan {
    Id,
    /// a column of single values of the node's group
    Single {
        col: usize,
        field: FieldMap,
        lang: Vec<String>,
    },
    /// a field of the values group
    Multi {
        fno: usize,
        field: FieldMap,
        lang: Vec<String>,
        first: u64,
        offset: u64,
        desc: bool,
    },
    /// an object field: a child group
    Object {
        group: usize,
        field: FieldMap,
        first: u64,
        offset: u64,
    },
    /// `Resource._types`
    Types {
        fno: usize,
    },
}

#[derive(Clone, Debug)]
pub struct Group {
    /// the field's path in the document, such as `allPerson.nodes.knows`
    pub path: String,
    pub parent: Option<usize>,
    pub kind: GroupKind,
    /// predicates of the single-valued value columns
    pub singles: Vec<NamedNode>,
    /// object types whose membership decides a node's type (abstract fields)
    pub flags: Vec<String>,
    /// the values group of this group's nodes
    pub values: Option<usize>,
    /// a values group's predicates, by field number
    pub vfields: Vec<NamedNode>,
    /// how each field of each concrete type is answered
    pub fields: HashMap<String, HashMap<Key, FieldPlan>>,
    /// the declared type is an object type, so every node has it
    pub concrete: bool,
    /// the declared type of the nodes: an object type, an interface, a union or `Node`
    pub declared: String,
}

impl Group {
    fn new(path: String, parent: Option<usize>, kind: GroupKind, declared: &str) -> Group {
        Group {
            path,
            parent,
            kind,
            singles: Vec::new(),
            flags: Vec::new(),
            values: None,
            vfields: Vec::new(),
            fields: HashMap::new(),
            concrete: false,
            declared: declared.to_string(),
        }
    }
}

/// A root field of the operation.
#[derive(Clone, Debug)]
pub enum RootPlan {
    Lookup { group: usize },
    Node { group: usize },
    List { group: usize },
    Connection { group: usize, count: Option<usize> },
}

pub struct Plan {
    pub groups: Vec<Group>,
    pub roots: HashMap<Key, RootPlan>,
    /// the commit the request's cursors name
    pub commit: Option<u64>,
    pub depth: u32,
    pub estimate: u64,
}

pub struct Planner<'a> {
    pub c: &'a Compiled,
    pub doc: &'a ExecutableDocument,
    pub vars: &'a JsonMap,
    pub limits: Limits,
    plan: Plan,
    /// child groups already planned for a selection, by its key and parent group
    children: HashMap<(usize, Key), usize>,
    /// the largest contribution to the estimate, and its path
    biggest: (u64, String),
}

fn too_complex(msg: String, limit: u64, estimate: u64, path: &str) -> GqlError {
    GqlError::new(Code::TooComplex, msg)
        .with("limit", limit)
        .with("estimate", estimate)
        .with("path", path)
}

fn bad_input(msg: impl Into<String>, f: &Field, doc: &ExecutableDocument) -> GqlError {
    GqlError::new(Code::BadUserInput, msg).at(location(f, doc))
}

pub fn location(f: &Field, doc: &ExecutableDocument) -> Option<(usize, usize)> {
    f.name
        .location()
        .and_then(|l| l.line_column(&doc.sources))
        .map(|lc| (lc.line, lc.column))
}

/// A JSON value of an argument: variables replaced by their coerced values, enum values
/// as strings. `None` for a variable without a value.
fn value_json(v: &Value, vars: &JsonMap) -> Option<J> {
    Some(match v {
        Value::Null => J::Null,
        Value::Enum(e) => J::String(e.to_string()),
        Value::Variable(n) => {
            return vars
                .get(n.as_str())
                .and_then(|v| serde_json::to_value(v).ok());
        }
        Value::String(s) => J::String(s.clone()),
        Value::Float(f) => serde_json::Number::from_f64(f.try_to_f64().ok()?)
            .map(J::Number)
            .unwrap_or(J::Null),
        Value::Int(i) => match i.as_str().parse::<i64>() {
            Ok(n) => J::from(n),
            Err(_) => J::String(i.as_str().to_string()),
        },
        Value::Boolean(b) => J::Bool(*b),
        Value::List(l) => J::Array(
            l.iter()
                .map(|x| value_json(x, vars).unwrap_or(J::Null))
                .collect(),
        ),
        Value::Object(o) => J::Object(
            o.iter()
                .filter_map(|(k, x)| Some((k.to_string(), value_json(x, vars)?)))
                .collect(),
        ),
    })
}

/// A field's argument values: the given ones, and the defaults of the others.
pub fn arguments(f: &Field, vars: &JsonMap) -> Map<String, J> {
    let mut out = Map::new();
    for def in &f.definition.arguments {
        let given = f
            .arguments
            .iter()
            .find(|a| a.name == def.name)
            .and_then(|a| value_json(&a.value, vars));
        let v = given.or_else(|| def.default_value.as_ref().and_then(|d| value_json(d, vars)));
        if let Some(v) = v {
            out.insert(def.name.to_string(), v);
        }
    }
    out
}

fn eval_if(sel: &Selection, directive: &str, vars: &JsonMap) -> Option<bool> {
    match sel
        .directives()
        .get(directive)?
        .specified_argument_by_name("if")?
        .as_ref()
    {
        Value::Boolean(b) => Some(*b),
        Value::Variable(v) => vars.get(v.as_str())?.as_bool(),
        _ => None,
    }
}

impl<'a> Planner<'a> {
    pub fn new(
        c: &'a Compiled,
        doc: &'a ExecutableDocument,
        vars: &'a JsonMap,
        limits: Limits,
    ) -> Self {
        Planner {
            c,
            doc,
            vars,
            limits,
            plan: Plan {
                groups: Vec::new(),
                roots: HashMap::new(),
                commit: None,
                depth: 0,
                estimate: 0,
            },
            children: HashMap::new(),
            biggest: (0, String::new()),
        }
    }

    /// CollectFields of the GraphQL specification, as the executor applies it.
    pub fn collect<'d>(
        &self,
        ty: &str,
        selections: &[&'d Selection],
        visited: &mut AHashSet<&'d Name>,
        out: &mut IndexMap<&'d Name, Vec<&'d Field>>,
    ) where
        'a: 'd,
    {
        for sel in selections {
            if eval_if(sel, "skip", self.vars).unwrap_or(false)
                || !eval_if(sel, "include", self.vars).unwrap_or(true)
            {
                continue;
            }
            match sel {
                Selection::Field(f) => out.entry(f.response_key()).or_default().push(f.as_ref()),
                Selection::FragmentSpread(s) => {
                    if !visited.insert(&s.fragment_name) {
                        continue;
                    }
                    let Some(fr) = self.doc.fragments.get(&s.fragment_name) else {
                        continue;
                    };
                    if !self.applies(ty, fr.type_condition()) {
                        continue;
                    }
                    let inner: Vec<&Selection> = fr.selection_set.selections.iter().collect();
                    self.collect(ty, &inner, visited, out);
                }
                Selection::InlineFragment(i) => {
                    if let Some(c) = &i.type_condition
                        && !self.applies(ty, c)
                    {
                        continue;
                    }
                    let inner: Vec<&Selection> = i.selection_set.selections.iter().collect();
                    self.collect(ty, &inner, visited, out);
                }
            }
        }
    }

    fn applies(&self, ty: &str, cond: &str) -> bool {
        let s = &self.c.api;
        let Some(obj) = s.get_object(ty) else {
            return false;
        };
        match s.types.get(cond) {
            Some(ExtendedType::Object(_)) => cond == ty,
            Some(ExtendedType::Interface(_)) => obj.implements_interfaces.contains(cond),
            Some(ExtendedType::Union(u)) => u.members.contains(ty),
            _ => false,
        }
    }

    fn grouped<'d>(&self, ty: &str, fields: &[&'d Field]) -> IndexMap<&'d Name, Vec<&'d Field>>
    where
        'a: 'd,
    {
        let sels: Vec<&Selection> = fields
            .iter()
            .flat_map(|f| f.selection_set.selections.iter())
            .collect();
        let mut out = IndexMap::default();
        self.collect(ty, &sels, &mut AHashSet::default(), &mut out);
        out
    }

    fn add_group(&mut self, g: Group) -> Result<usize, GqlError> {
        let max = self.limits.max_groups;
        if self.plan.groups.len() >= max {
            return Err(too_complex(
                format!("the document needs more than {max} fetch groups"),
                max as u64,
                self.plan.groups.len() as u64 + 1,
                &g.path,
            ));
        }
        self.plan.groups.push(g);
        Ok(self.plan.groups.len() - 1)
    }

    fn count_nodes(&mut self, n: u64, path: &str) -> Result<(), GqlError> {
        self.plan.estimate = self.plan.estimate.saturating_add(n);
        if n > self.biggest.0 {
            self.biggest = (n, path.to_string());
        }
        let max = self.limits.max_nodes;
        if self.plan.estimate > max {
            return Err(too_complex(
                format!(
                    "the response could hold {} or more nodes, more than the limit of {max}; {} contributes the most",
                    self.plan.estimate, self.biggest.1
                ),
                max,
                self.plan.estimate,
                &self.biggest.1,
            ));
        }
        Ok(())
    }

    fn depth(&mut self, d: u32, path: &str) -> Result<(), GqlError> {
        self.plan.depth = self.plan.depth.max(d);
        let max = self.limits.max_depth;
        if d > max {
            return Err(too_complex(
                format!("{path} is at depth {d}, deeper than the limit of {max}"),
                max as u64,
                d as u64,
                path,
            )
            .with("depth", d));
        }
        Ok(())
    }

    /// `first` (or `last`) of a field: in range, or the default.
    fn page_size(
        &self,
        args: &Map<String, J>,
        k: &str,
        f: &Field,
    ) -> Result<Option<u64>, GqlError> {
        match args.get(k) {
            None | Some(J::Null) => Ok(None),
            Some(v) => {
                let n = v
                    .as_i64()
                    .ok_or_else(|| bad_input(format!("{k} must be an integer"), f, self.doc))?;
                let max = self.limits.max_first as i64;
                if n < 0 {
                    return Err(bad_input(format!("{k} must not be negative"), f, self.doc));
                }
                if n > max {
                    return Err(bad_input(
                        format!("{k} is {n}, more than the limit of {max}"),
                        f,
                        self.doc,
                    )
                    .with("limit", max));
                }
                Ok(Some(n as u64))
            }
        }
    }

    fn offset(&self, args: &Map<String, J>, f: &Field) -> Result<u64, GqlError> {
        match args.get("offset") {
            None | Some(J::Null) => Ok(0),
            Some(v) => match v.as_i64() {
                Some(n) if n >= 0 => Ok(n as u64),
                _ => Err(bad_input("offset must not be negative", f, self.doc)),
            },
        }
    }

    /// An `orderBy` list of a mapped type, with `ID_ASC` appended.
    fn order(&self, ty: &str, args: &Map<String, J>, f: &Field) -> Result<Vec<OrderKey>, GqlError> {
        let mut keys = Vec::new();
        let t = self.c.mapping.ty(ty);
        let values = t.map(crate::api::order_values).unwrap_or_default();
        let list = match args.get("orderBy") {
            None | Some(J::Null) => Vec::new(),
            Some(J::Array(a)) => a.clone(),
            Some(v) => vec![v.clone()],
        };
        let mut has_id = false;
        for v in list {
            let s = v.as_str().unwrap_or_default();
            let (stem, desc) = if let Some(x) = s.strip_suffix("_DESC") {
                (x, true)
            } else if let Some(x) = s.strip_suffix("_ASC") {
                (x, false)
            } else {
                return Err(bad_input(format!("unknown order {s}"), f, self.doc));
            };
            if stem == "ID" {
                has_id = true;
                keys.push(OrderKey { pred: None, desc });
                continue;
            }
            let field = values
                .iter()
                .find(|(v, _)| v == stem)
                .and_then(|(_, n)| t.and_then(|t| t.field(n)))
                .ok_or_else(|| bad_input(format!("unknown order {s}"), f, self.doc))?;
            keys.push(OrderKey {
                pred: Some(field.predicate.clone()),
                desc,
            });
        }
        if !has_id {
            keys.push(OrderKey {
                pred: None,
                desc: false,
            });
        }
        Ok(keys)
    }

    fn filter(&self, args: &Map<String, J>) -> Option<J> {
        match args.get("filter") {
            None | Some(J::Null) => None,
            Some(v) => Some(v.clone()),
        }
    }

    fn lang(&self, field: &FieldMap, args: &Map<String, J>) -> Vec<String> {
        if !matches!(field.target, Target::Scalar(s) if s.is_text()) {
            return Vec::new();
        }
        if let Some(J::Array(a)) = args.get("lang") {
            return a
                .iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect();
        }
        field
            .lang
            .clone()
            .or_else(|| self.c.mapping.lang.clone())
            .unwrap_or_else(|| vec!["*".into()])
    }

    fn single_col(&mut self, g: usize, p: &NamedNode) -> usize {
        let grp = &mut self.plan.groups[g];
        match grp.singles.iter().position(|x| x == p) {
            Some(i) => i,
            None => {
                grp.singles.push(p.clone());
                grp.singles.len() - 1
            }
        }
    }

    fn value_field(&mut self, g: usize, p: &NamedNode) -> Result<usize, GqlError> {
        let vg = match self.plan.groups[g].values {
            Some(v) => v,
            None => {
                let path = format!("{}.values", self.plan.groups[g].path);
                let v = self.add_group(Group::new(path, Some(g), GroupKind::Values, ""))?;
                self.plan.groups[g].values = Some(v);
                v
            }
        };
        let grp = &mut self.plan.groups[vg];
        Ok(match grp.vfields.iter().position(|x| x == p) {
            Some(i) => i,
            None => {
                grp.vfields.push(p.clone());
                grp.vfields.len() - 1
            }
        })
    }

    /// The object types a node of a declared type may be answered as; abstract types
    /// get membership flags in the group.
    fn concrete(&mut self, g: usize, declared: &str) -> Vec<String> {
        let m = &self.c.mapping;
        let types = m.possible_types(declared);
        let is_object = m.ty(declared).is_some_and(|t| !t.interface);
        self.plan.groups[g].concrete = is_object;
        if !is_object {
            self.plan.groups[g].flags = types.clone();
        }
        let mut out = types;
        if declared == "Node" {
            out.push("Resource".into());
        }
        out
    }

    /// Plan the selections of `fields` (merged fields of one response key) on the
    /// nodes of group `g`.
    fn plan_nodes(
        &mut self,
        g: usize,
        fields: &[&'a Field],
        depth: u32,
        mult: u64,
        at: &str,
    ) -> Result<(), GqlError> {
        let declared = self.plan.groups[g].declared.clone();
        let types = self.concrete(g, &declared);
        for ty in &types {
            for (_, fs) in self.grouped(ty, fields) {
                let f0 = fs[0];
                let name = f0.name.as_str();
                if name.starts_with("__") {
                    continue;
                }
                let k = key(f0);
                let path = format!("{at}.{}", f0.response_key());
                self.depth(depth + 1, &path)?;
                if name == "id" {
                    self.plan.groups[g]
                        .fields
                        .entry(ty.clone())
                        .or_default()
                        .insert(k, FieldPlan::Id);
                    continue;
                }
                if ty == "Resource" {
                    if name == "_types" {
                        let fno = self.value_field(g, &NamedNode::new_unchecked(RDF_TYPE))?;
                        self.plan.groups[g]
                            .fields
                            .entry(ty.clone())
                            .or_default()
                            .insert(k, FieldPlan::Types { fno });
                    }
                    continue;
                }
                let Some(field) = self.c.mapping.ty(ty).and_then(|t| t.field(name)).cloned() else {
                    continue;
                };
                let args = arguments(f0, self.vars);
                let plan = match &field.target {
                    Target::Object(target) => {
                        let first = if field.list {
                            self.page_size(&args, "first", f0)?
                                .unwrap_or(self.limits.default_first as u64)
                        } else {
                            u64::MAX
                        };
                        let offset = if field.list {
                            self.offset(&args, f0)?
                        } else {
                            0
                        };
                        let child = match self.children.get(&(g, k)) {
                            Some(&c) => c,
                            None => {
                                let filter = self
                                    .filter(&args)
                                    .filter(|_| self.c.mapping.ty(target).is_some())
                                    .map(|f| (target.clone(), f));
                                let order = if field.list && self.c.mapping.ty(target).is_some() {
                                    self.order(target, &args, f0)?
                                } else {
                                    vec![OrderKey {
                                        pred: None,
                                        desc: false,
                                    }]
                                };
                                let c = self.add_group(Group::new(
                                    path.clone(),
                                    Some(g),
                                    GroupKind::Child {
                                        pred: field.predicate.clone(),
                                        inverse: field.inverse,
                                        filter,
                                        order,
                                    },
                                    target,
                                ))?;
                                self.children.insert((g, k), c);
                                let n = if field.list {
                                    mult.saturating_mul(first)
                                } else {
                                    mult
                                };
                                self.count_nodes(n, &path)?;
                                self.plan_nodes(c, &fs, depth + 1, n, &path)?;
                                c
                            }
                        };
                        FieldPlan::Object {
                            group: child,
                            field: field.clone(),
                            first,
                            offset,
                        }
                    }
                    _ if field.list => {
                        let fno = self.value_field(g, &field.predicate)?;
                        let first = self
                            .page_size(&args, "first", f0)?
                            .unwrap_or(self.limits.default_first as u64);
                        let offset = self.offset(&args, f0)?;
                        let desc = args.get("orderBy").and_then(J::as_str) == Some("DESC");
                        FieldPlan::Multi {
                            fno,
                            lang: self.lang(&field, &args),
                            field: field.clone(),
                            first,
                            offset,
                            desc,
                        }
                    }
                    _ => {
                        let col = self.single_col(g, &field.predicate);
                        FieldPlan::Single {
                            col,
                            lang: self.lang(&field, &args),
                            field: field.clone(),
                        }
                    }
                };
                self.plan.groups[g]
                    .fields
                    .entry(ty.clone())
                    .or_default()
                    .insert(k, plan);
            }
        }
        Ok(())
    }

    fn window(&mut self, f: &Field, args: &Map<String, J>, hash: &str) -> Result<Window, GqlError> {
        let first = self.page_size(args, "first", f)?;
        let last = self.page_size(args, "last", f)?;
        let offset = self.offset(args, f)?;
        let mut w = Window {
            first,
            last,
            offset,
            hash: hash.to_string(),
            ..Default::default()
        };
        for (k, slot) in [("after", &mut w.after), ("before", &mut w.before)] {
            let Some(s) = args.get(k).and_then(J::as_str) else {
                continue;
            };
            let c: Cursor = cursor::decode(s).map_err(|e| {
                GqlError::new(Code::CursorInvalid, format!("{k}: {e}")).at(location(f, self.doc))
            })?;
            if c.h != hash {
                return Err(GqlError::new(
                    Code::CursorInvalid,
                    format!("{k}: the cursor belongs to another field, other arguments or another schema version"),
                )
                .at(location(f, self.doc)));
            }
            match self.plan.commit {
                Some(x) if x != c.c => {
                    return Err(GqlError::new(
                        Code::CursorInvalid,
                        format!(
                            "the cursors of the request name different commits ({x} and {})",
                            c.c
                        ),
                    )
                    .at(location(f, self.doc)));
                }
                _ => self.plan.commit = Some(c.c),
            }
            *slot = Some(c.o);
        }
        if w.first.is_none() && w.last.is_none() {
            w.first = Some(self.limits.default_first as u64);
        }
        Ok(w)
    }

    /// Plan an operation's root fields.
    pub fn plan(mut self, op: &'a Operation) -> Result<Plan, GqlError> {
        let sels: Vec<&Selection> = op.selection_set.selections.iter().collect();
        let mut grouped = IndexMap::default();
        self.collect("Query", &sels, &mut AHashSet::default(), &mut grouped);
        for (rkey, fs) in grouped {
            let f0 = fs[0];
            let name = f0.name.as_str();
            if name.starts_with("__") {
                continue;
            }
            let Some(root) = self
                .c
                .mapping
                .roots
                .iter()
                .find(|r| r.name == name)
                .cloned()
            else {
                continue;
            };
            let path = rkey.to_string();
            self.depth(1, &path)?;
            let args = arguments(f0, self.vars);
            let k = key(f0);
            let id = |s: &Planner| -> Result<Term, GqlError> {
                let v = args.get("id").and_then(J::as_str).unwrap_or_default();
                parse_id(v, &s.c.mapping.prefixes).map_err(|e| bad_input(e, f0, s.doc))
            };
            match &root.kind {
                RootKind::Lookup(ty) => {
                    let g = self.add_group(Group::new(
                        path.clone(),
                        None,
                        GroupKind::Lookup {
                            ty: ty.clone(),
                            id: id(&self)?,
                        },
                        ty,
                    ))?;
                    self.count_nodes(1, &path)?;
                    self.plan_nodes(g, &fs, 1, 1, &path)?;
                    self.plan.roots.insert(k, RootPlan::Lookup { group: g });
                }
                RootKind::Node => {
                    let g = self.add_group(Group::new(
                        path.clone(),
                        None,
                        GroupKind::Node { id: id(&self)? },
                        "Node",
                    ))?;
                    self.count_nodes(1, &path)?;
                    self.plan_nodes(g, &fs, 1, 1, &path)?;
                    self.plan.roots.insert(k, RootPlan::Node { group: g });
                }
                RootKind::List(ty) => {
                    let first = self
                        .page_size(&args, "first", f0)?
                        .unwrap_or(self.limits.default_first as u64);
                    let window = Window {
                        first: Some(first),
                        offset: self.offset(&args, f0)?,
                        ..Default::default()
                    };
                    let g = self.add_group(Group::new(
                        path.clone(),
                        None,
                        GroupKind::Collection {
                            ty: ty.clone(),
                            filter: self.filter(&args),
                            order: self.order(ty, &args, f0)?,
                            window,
                            connection: false,
                            count: None,
                        },
                        ty,
                    ))?;
                    self.count_nodes(first, &path)?;
                    self.plan_nodes(g, &fs, 1, first, &path)?;
                    self.plan.roots.insert(k, RootPlan::List { group: g });
                }
                RootKind::Connection(ty) => {
                    let hash = cursor::field_hash(name, &args, self.c.version);
                    let window = self.window(f0, &args, &hash)?;
                    let order = self.order(ty, &args, f0)?;
                    let filter = self.filter(&args);
                    let conn = format!("{ty}Connection");
                    let parts = self.grouped(&conn, &fs);
                    let wants_count = parts.values().flatten().any(|f| f.name == "totalCount")
                        || (window.last.is_some() && window.before.is_none());
                    let count = if wants_count {
                        Some(self.add_group(Group::new(
                            format!("{path}.totalCount"),
                            None,
                            GroupKind::Count {
                                ty: ty.clone(),
                                filter: filter.clone(),
                            },
                            ty,
                        ))?)
                    } else {
                        None
                    };
                    let size = window.first.or(window.last).unwrap_or(0);
                    let g = self.add_group(Group::new(
                        path.clone(),
                        None,
                        GroupKind::Collection {
                            ty: ty.clone(),
                            filter,
                            order,
                            window,
                            connection: true,
                            count,
                        },
                        ty,
                    ))?;
                    self.count_nodes(size, &path)?;
                    for (_, cfs) in parts {
                        match cfs[0].name.as_str() {
                            "nodes" => {
                                let at = format!("{path}.{}", cfs[0].response_key());
                                self.depth(2, &at)?;
                                self.plan_nodes(g, &cfs, 2, size, &at)?;
                            }
                            "edges" => {
                                let edges = format!("{path}.{}", cfs[0].response_key());
                                self.depth(2, &edges)?;
                                for (_, efs) in self.grouped(&format!("{ty}Edge"), &cfs) {
                                    if efs[0].name == "node" {
                                        let at = format!("{edges}.{}", efs[0].response_key());
                                        self.depth(3, &at)?;
                                        self.plan_nodes(g, &efs, 3, size, &at)?;
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                    self.plan
                        .roots
                        .insert(k, RootPlan::Connection { group: g, count });
                }
            }
        }
        Ok(self.plan)
    }
}

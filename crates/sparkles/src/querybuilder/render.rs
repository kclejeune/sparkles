//! Rendering of builders to SPARQL text: prefix tracking, variable substitution,
//! operator precedence and error collection.

use std::collections::{BTreeSet, HashMap};

use super::expr::{Callee, E, Expr, PREC_PRIMARY, PREC_UNARY};
use super::pattern::{Element, WhereBuilder};
use super::query::{Common, Modifiers, SelectBuilder};
use super::term::{N, Node, RDF_TYPE, Tok, tokenize, write_iri, write_string_literal};

/// Prefixes declared automatically when used but not declared (Jena's standard
/// prefix mapping plus a few common vocabularies).
pub const WELL_KNOWN_PREFIXES: &[(&str, &str)] = &[
    ("rdf", "http://www.w3.org/1999/02/22-rdf-syntax-ns#"),
    ("rdfs", "http://www.w3.org/2000/01/rdf-schema#"),
    ("xsd", "http://www.w3.org/2001/XMLSchema#"),
    ("owl", "http://www.w3.org/2002/07/owl#"),
    ("dc", "http://purl.org/dc/elements/1.1/"),
    ("dcterms", "http://purl.org/dc/terms/"),
    ("foaf", "http://xmlns.com/foaf/0.1/"),
    ("skos", "http://www.w3.org/2004/02/skos/core#"),
];

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Pos {
    /// Subject / object / graph name.
    Term,
    /// Predicate of a pattern: paths and `a` allowed.
    Pred,
    /// Predicate of a template / data triple: `a` allowed, paths not.
    TemplatePred,
    /// Cell of a VALUES row: UNDEF allowed.
    Values,
    /// Must be a variable (projection, `AS ?v`, VALUES header); never substituted.
    VarDecl,
    /// Operand of an expression.
    Expr,
}

pub(crate) struct Renderer {
    declared: Vec<(String, String)>,
    used: BTreeSet<String>,
    subst: HashMap<String, Node>,
    pub(crate) errors: Vec<String>,
    indent: usize,
}

pub(crate) fn pad(n: usize) -> String {
    "  ".repeat(n)
}

impl Renderer {
    pub(crate) fn new(common: &Common) -> Renderer {
        let mut r = Renderer::detached();
        r.errors.extend(common.errors.iter().cloned());
        r.declared = common.prefixes.clone();
        r.subst = common.bindings.iter().cloned().collect();
        r
    }

    /// A renderer with no prefixes / bindings (for `Display` of fragments).
    pub(crate) fn detached() -> Renderer {
        Renderer {
            declared: Vec::new(),
            used: BTreeSet::new(),
            subst: HashMap::new(),
            errors: Vec::new(),
            indent: 0,
        }
    }

    fn error(&mut self, msg: impl Into<String>) {
        self.errors.push(msg.into());
    }

    fn declare(&mut self, prefix: &str, iri: &str) {
        match self.declared.iter().find(|(p, _)| p == prefix) {
            Some((_, existing)) if existing != iri => {
                let msg = format!("prefix '{prefix}:' declared as both <{existing}> and <{iri}>");
                self.error(msg);
            }
            Some(_) => {}
            None => self.declared.push((prefix.to_string(), iri.to_string())),
        }
    }

    /// `BASE` / `PREFIX` lines. Checks every used prefix: undeclared well-known prefixes
    /// are added, other undeclared prefixes are errors.
    pub(crate) fn prologue(&mut self, base: Option<&str>) -> String {
        let used: Vec<String> = self.used.iter().cloned().collect();
        for p in used {
            if self.declared.iter().any(|(d, _)| *d == p) {
                continue;
            }
            match WELL_KNOWN_PREFIXES.iter().find(|(w, _)| *w == p) {
                Some((w, iri)) => self.declared.push((w.to_string(), iri.to_string())),
                None => self.error(format!("undeclared prefix '{p}:'")),
            }
        }
        let mut s = String::new();
        if let Some(b) = base {
            s.push_str("BASE ");
            let _ = write_iri(&mut s, b);
            s.push('\n');
        }
        for (p, iri) in &self.declared {
            s.push_str(&format!("PREFIX {p}: "));
            let _ = write_iri(&mut s, iri);
            s.push('\n');
        }
        s
    }

    // ------------------------------------------------------------------ nodes ----

    pub(crate) fn node(&mut self, n: &Node, pos: Pos) -> String {
        if let N::Var(v) = &n.0
            && pos != Pos::VarDecl
            && let Some(sub) = self.subst.get(v).cloned()
        {
            return self.node_inner(&sub, pos);
        }
        self.node_inner(n, pos)
    }

    fn node_inner(&mut self, n: &Node, pos: Pos) -> String {
        if pos == Pos::VarDecl && !n.is_var() {
            if let N::Invalid(e) = &n.0 {
                self.error(e.clone());
            } else {
                self.error(format!("expected a variable, got {n}"));
            }
            return n.to_string();
        }
        match &n.0 {
            N::Var(v) => format!("?{v}"),
            N::Iri(_) | N::Number(_) | N::Bool(_) | N::Blank(_) => n.to_string(),
            N::Prefixed(p, l) => {
                self.used.insert(p.clone());
                format!("{p}:{l}")
            }
            N::Anon => {
                if pos == Pos::Expr {
                    self.error("[] cannot be used in an expression");
                }
                "[]".into()
            }
            N::A => {
                if matches!(pos, Pos::Pred | Pos::TemplatePred) {
                    "a".into()
                } else {
                    format!("<{RDF_TYPE}>")
                }
            }
            N::Literal {
                value,
                lang,
                datatype,
            } => {
                let mut s = String::new();
                let _ = write_string_literal(&mut s, value);
                if let Some(l) = lang {
                    s.push('@');
                    s.push_str(l);
                } else if let Some(dt) = datatype {
                    s.push_str("^^");
                    let dt = self.node_inner(dt, Pos::Term);
                    s.push_str(&dt);
                }
                s
            }
            N::TripleTerm(t) => {
                let s = self.node(&t[0], Pos::Term);
                let p = self.node(&t[1], Pos::TemplatePred);
                let o = self.node(&t[2], Pos::Term);
                format!("<<( {s} {p} {o} )>>")
            }
            N::Path(p) => {
                if pos != Pos::Pred {
                    self.error(format!(
                        "property path {p:?} is only allowed as the predicate of a WHERE pattern"
                    ));
                }
                self.raw(p)
            }
            N::Undef => {
                if pos != Pos::Values {
                    self.error("UNDEF is only allowed in VALUES");
                }
                "UNDEF".into()
            }
            N::Invalid(e) => {
                self.error(e.clone());
                n.to_string()
            }
        }
    }

    /// Raw SPARQL text: records prefixed names and substitutes bound variables.
    pub(crate) fn raw(&mut self, text: &str) -> String {
        let toks = match tokenize(text) {
            Ok(t) => t,
            Err(e) => {
                self.error(e);
                return text.to_string();
            }
        };
        let mut out = String::with_capacity(text.len());
        for (t, s, e) in toks {
            match t {
                Tok::Var(v) if self.subst.contains_key(&v) => {
                    let sub = self.subst[&v].clone();
                    let r = self.node_inner(&sub, Pos::Expr);
                    out.push_str(&r);
                }
                Tok::PName(p) => {
                    self.used.insert(p);
                    out.push_str(&text[s..e]);
                }
                Tok::Comment => {
                    out.push_str(&text[s..e]);
                    out.push('\n');
                }
                _ => out.push_str(&text[s..e]),
            }
        }
        out
    }

    // ------------------------------------------------------------ expressions ----

    pub(crate) fn expr(&mut self, e: &Expr) -> String {
        self.expr_prec(e).0
    }

    fn operand(&mut self, e: &Expr, min: u8) -> String {
        let (s, p) = self.expr_prec(e);
        if p < min { format!("({s})") } else { s }
    }

    fn expr_list(&mut self, es: &[Expr]) -> String {
        es.iter()
            .map(|a| self.expr(a))
            .collect::<Vec<_>>()
            .join(", ")
    }

    fn expr_prec(&mut self, e: &Expr) -> (String, u8) {
        match &e.0 {
            E::Raw(t) => (self.raw(t), 0),
            E::Node(n) => {
                let s = self.node(n, Pos::Expr);
                let p = if s.starts_with(['-', '+']) {
                    PREC_UNARY
                } else {
                    PREC_PRIMARY
                };
                (s, p)
            }
            E::Binary(op, a, b) => {
                let (p, lmin, rmin) = op.prec();
                let l = self.operand(a, lmin);
                let r = self.operand(b, rmin);
                (format!("{l} {} {r}", op.symbol()), p)
            }
            E::Unary(c, x) => (format!("{c}{}", self.operand(x, PREC_PRIMARY)), PREC_UNARY),
            E::In(x, list, negated) => {
                let l = self.operand(x, 4);
                let items = self.expr_list(list);
                let not = if *negated { "NOT " } else { "" };
                (format!("{l} {not}IN ({items})"), 3)
            }
            E::Call(callee, args) => {
                let name = match callee {
                    Callee::Builtin(b) => b.clone(),
                    Callee::Iri(n) => self.node(n, Pos::Term),
                };
                (format!("{name}({})", self.expr_list(args)), PREC_PRIMARY)
            }
            E::Exists(w, negated) => {
                let g = self.group(w);
                let not = if *negated { "NOT " } else { "" };
                (format!("{not}EXISTS {g}"), PREC_PRIMARY)
            }
            E::Aggregate {
                name,
                distinct,
                arg,
                separator,
            } => {
                let mut s = format!("{name}(");
                if *distinct {
                    s.push_str("DISTINCT ");
                }
                match arg {
                    Some(a) => {
                        let a = self.expr(a);
                        s.push_str(&a);
                    }
                    None => s.push('*'),
                }
                if let Some(sep) = separator {
                    s.push_str("; SEPARATOR = ");
                    let _ = write_string_literal(&mut s, sep);
                }
                s.push(')');
                (s, PREC_PRIMARY)
            }
            E::Invalid(msg) => {
                self.error(msg.clone());
                (format!("<<invalid: {msg}>>"), PREC_PRIMARY)
            }
        }
    }

    // --------------------------------------------------------------- patterns ----

    /// `{ … }` with elements one level deeper than the current indentation.
    pub(crate) fn group(&mut self, w: &WhereBuilder) -> String {
        if w.elements.is_empty() {
            return "{ }".into();
        }
        let outer = self.indent;
        self.indent += 1;
        let inner = pad(self.indent);
        let mut s = String::from("{\n");
        for el in &w.elements {
            let line = self.element(el);
            s.push_str(&inner);
            s.push_str(&line);
            s.push('\n');
        }
        self.indent = outer;
        s.push_str(&pad(outer));
        s.push('}');
        s
    }

    pub(crate) fn triple(&mut self, s: &Node, p: &Node, o: &Node, pred: Pos) -> String {
        let s = self.node(s, Pos::Term);
        let p = self.node(p, pred);
        let o = self.node(o, Pos::Term);
        format!("{s} {p} {o} .")
    }

    fn element(&mut self, el: &Element) -> String {
        match el {
            Element::Triple(s, p, o) => self.triple(s, p, o, Pos::Pred),
            Element::Optional(w) => format!("OPTIONAL {}", self.group(w)),
            Element::Union(branches) => {
                if branches.is_empty() {
                    self.error("UNION needs at least one branch");
                }
                branches
                    .iter()
                    .map(|b| self.group(b))
                    .collect::<Vec<_>>()
                    .join(" UNION ")
            }
            Element::Minus(w) => format!("MINUS {}", self.group(w)),
            Element::Graph(g, w) => {
                let g = self.node(g, Pos::Term);
                format!("GRAPH {g} {}", self.group(w))
            }
            Element::Service(endpoint, silent, w) => {
                let e = self.node(endpoint, Pos::Term);
                let silent = if *silent { "SILENT " } else { "" };
                format!("SERVICE {silent}{e} {}", self.group(w))
            }
            Element::Filter(e) => format!("FILTER({})", self.expr(e)),
            Element::Bind(e, v) => {
                let e = self.expr(e);
                let v = self.node(v, Pos::VarDecl);
                format!("BIND({e} AS {v})")
            }
            Element::Values(vars, rows) => self.values(vars, rows),
            Element::SubSelect(q) => {
                let k = self.indent;
                self.indent = k + 1;
                let body = self.select(q, true);
                self.indent = k;
                format!("{{\n{}{body}\n{}}}", pad(k + 1), pad(k))
            }
            Element::Group(w) => self.group(w),
        }
    }

    fn values(&mut self, vars: &[Node], rows: &[Vec<Node>]) -> String {
        let vs: Vec<String> = vars.iter().map(|v| self.node(v, Pos::VarDecl)).collect();
        let mut cells = Vec::with_capacity(rows.len());
        for row in rows {
            if row.len() != vars.len() {
                self.error(format!(
                    "VALUES row has {} values for {} variables",
                    row.len(),
                    vars.len()
                ));
                // the build fails with that error; don't render (or index) the row
                continue;
            }
            let r: Vec<String> = row.iter().map(|c| self.node(c, Pos::Values)).collect();
            cells.push(r);
        }
        if vars.len() == 1 {
            let items: Vec<String> = cells.into_iter().map(|mut r| r.remove(0)).collect();
            if items.is_empty() {
                format!("VALUES {} {{ }}", vs[0])
            } else {
                format!("VALUES {} {{ {} }}", vs[0], items.join(" "))
            }
        } else {
            let items: Vec<String> = cells
                .into_iter()
                .map(|r| format!("({})", r.join(" ")))
                .collect();
            format!("VALUES ({}) {{ {} }}", vs.join(" "), items.join(" "))
        }
    }

    // ----------------------------------------------------------------- queries ----

    /// Joins clause lines with the current indentation.
    pub(crate) fn join_lines(&self, lines: Vec<String>) -> String {
        lines.join(&format!("\n{}", pad(self.indent)))
    }

    pub(crate) fn dataset_clause(
        &mut self,
        lines: &mut Vec<String>,
        from: &[Node],
        named: &[Node],
    ) {
        for g in from {
            let g = self.node(g, Pos::Term);
            lines.push(format!("FROM {g}"));
        }
        for g in named {
            let g = self.node(g, Pos::Term);
            lines.push(format!("FROM NAMED {g}"));
        }
    }

    pub(crate) fn modifiers(&mut self, lines: &mut Vec<String>, m: &Modifiers) {
        if !m.order_by.is_empty() {
            let keys: Vec<String> = m
                .order_by
                .iter()
                .map(|(e, desc)| {
                    if *desc {
                        format!("DESC({})", self.expr(e))
                    } else if let Some(v) = e.as_var() {
                        self.node(v, Pos::Expr)
                    } else {
                        format!("ASC({})", self.expr(e))
                    }
                })
                .collect();
            lines.push(format!("ORDER BY {}", keys.join(" ")));
        }
        if let Some(l) = m.limit {
            lines.push(format!("LIMIT {l}"));
        }
        if let Some(o) = m.offset {
            lines.push(format!("OFFSET {o}"));
        }
    }

    pub(crate) fn select(&mut self, q: &SelectBuilder, sub: bool) -> String {
        let saved_subst = sub.then(|| self.subst.clone());
        if sub {
            for (p, iri) in &q.common.prefixes {
                self.declare(p, iri);
            }
            self.errors.extend(q.common.errors.iter().cloned());
            if q.common.base.is_some() {
                self.error("a sub-select cannot have BASE");
            }
            if !q.from.is_empty() || !q.from_named.is_empty() {
                self.error("a sub-select cannot have FROM / FROM NAMED");
            }
            for (k, v) in &q.common.bindings {
                self.subst.insert(k.clone(), v.clone());
            }
        }
        let mut head = String::from("SELECT");
        if let Some(m) = q.modifier {
            head.push(' ');
            head.push_str(m);
        }
        if q.projection.is_empty() {
            head.push_str(" *");
        }
        for (e, v) in &q.projection {
            head.push(' ');
            let v = self.node(v, Pos::VarDecl);
            match e {
                None => head.push_str(&v),
                Some(e) => {
                    let e = self.expr(e);
                    head.push_str(&format!("({e} AS {v})"));
                }
            }
        }
        let mut lines = vec![head];
        if !sub {
            self.dataset_clause(&mut lines, &q.from, &q.from_named);
        }
        let g = self.group(&q.where_);
        lines.push(format!("WHERE {g}"));
        if !q.group_by.is_empty() {
            let keys: Vec<String> = q
                .group_by
                .iter()
                .map(|(e, alias)| match (e.as_var(), alias) {
                    (Some(v), None) => self.node(v, Pos::VarDecl),
                    (_, None) => format!("({})", self.expr(e)),
                    (_, Some(a)) => {
                        let e = self.expr(e);
                        let a = self.node(a, Pos::VarDecl);
                        format!("({e} AS {a})")
                    }
                })
                .collect();
            lines.push(format!("GROUP BY {}", keys.join(" ")));
        }
        if !q.having.is_empty() {
            let hs: Vec<String> = q
                .having
                .iter()
                .map(|h| format!("({})", self.expr(h)))
                .collect();
            lines.push(format!("HAVING {}", hs.join(" ")));
        }
        self.modifiers(&mut lines, &q.modifiers);
        if let Some(s) = saved_subst {
            self.subst = s;
        }
        self.join_lines(lines)
    }
}

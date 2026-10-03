//! The SPARQL rules over variables and groups: `unused-variable`,
//! `single-use-variable`, `unbound-variable`,
//! `cartesian-product`, `select-star-group-by`, `ungrouped-variable`, `filter-scope` and
//! `filter-equality`.
//!
//! Each query form, subquery and update operation is a *level*: its variables are its
//! own, except the ones a nested subquery projects. A variable is *bound* by a triple
//! pattern, `GRAPH ?g`, `BIND … AS ?v`, `VALUES`, a subquery's projection, or a
//! projection or `GROUP BY` alias. `MINUS`, `FILTER` and `EXISTS` patterns bind nothing
//! outside themselves.

use super::{Cst, LintError, Out};
use crate::lex::TokenKind;
use crate::sparql::keywords::Kw;
use crate::syntax::NodeKind as K;
use crate::tree::{Element, NodeId, TokenId};
use std::collections::{HashMap, HashSet};

const LEVELS: [K; 7] = [
    K::SelectQuery,
    K::ConstructQuery,
    K::DescribeQuery,
    K::AskQuery,
    K::SubSelect,
    K::ModifyOp,
    K::DeleteWhereOp,
];

pub(super) fn run(c: &Cst<'_, '_>, out: &mut Out<'_>) -> Result<(), LintError> {
    for n in c.nodes() {
        if !LEVELS.contains(&c.kind(n)) {
            continue;
        }
        let lv = Level::new(c, n);
        lv.variables(out);
        lv.grouping(out);
        lv.filters(out);
        lv.products(out);
        out.deadline()?;
    }
    Ok(())
}

/// The rules that explain a query the reference parser refused for its scoping.
pub(super) fn scope_errors(c: &Cst<'_, '_>, out: &mut Out<'_>) {
    for n in c.nodes() {
        if matches!(c.kind(n), K::SelectQuery | K::SubSelect) {
            Level::new(c, n).grouping(out);
        }
    }
}

fn is_var(k: TokenKind) -> bool {
    matches!(k, TokenKind::Var1 | TokenKind::Var2)
}

/// A variable token's name, without `?` or `$`.
fn name(c: &Cst<'_, '_>, t: TokenId) -> String {
    c.text(t)[1..].to_string()
}

struct Level<'c, 'a, 's> {
    c: &'c Cst<'a, 's>,
    root: NodeId,
    kind: K,
    select: Option<NodeId>,
    star: Option<TokenId>,
    /// the pattern: the WHERE clause's group, or the quad pattern of `DELETE WHERE`
    pattern: Option<NodeId>,
}

impl<'c, 'a, 's> Level<'c, 'a, 's> {
    fn new(c: &'c Cst<'a, 's>, root: NodeId) -> Self {
        let child = |k: K| c.tree.child_nodes(root).find(|&n| c.kind(n) == k);
        let select = child(K::SelectClause);
        let star = select.and_then(|s| {
            c.own_tokens(s)
                .into_iter()
                .find(|&t| c.token_kind(t) == TokenKind::Star)
        });
        let pattern = match c.kind(root) {
            K::DeleteWhereOp => child(K::QuadPattern),
            _ => child(K::WhereClause).and_then(|w| {
                c.tree
                    .child_nodes(w)
                    .find(|&n| c.kind(n) == K::GroupGraphPattern)
            }),
        };
        Level {
            c,
            root,
            kind: c.kind(root),
            select,
            star,
            pattern,
        }
    }

    fn child(&self, k: K) -> Option<NodeId> {
        self.c
            .tree
            .child_nodes(self.root)
            .find(|&n| self.c.kind(n) == k)
    }

    fn children(&self, k: K) -> Vec<NodeId> {
        self.c
            .tree
            .child_nodes(self.root)
            .filter(|&n| self.c.kind(n) == k)
            .collect()
    }

    /// The variable tokens of `n`'s subtree that belong to this level.
    fn vars_in(&self, n: NodeId, skip: &[K]) -> Vec<TokenId> {
        self.c
            .tokens_where(n, &|k| k == K::SubSelect || skip.contains(&k))
            .into_iter()
            .filter(|&t| is_var(self.c.token_kind(t)))
            .collect()
    }

    /// The level's variables bound outside its pattern: projection and `GROUP BY`
    /// aliases, and the `VALUES` after the query.
    fn late_bound(&self) -> Vec<(String, Option<TokenId>)> {
        let c = self.c;
        let mut v = Vec::new();
        if let Some(s) = self.select {
            for item in projection(c, s) {
                if item.alias {
                    v.push((item.name, Some(item.token)));
                }
            }
        }
        if let Some(g) = self.child(K::GroupBy) {
            for cond in c.tree.child_nodes(g) {
                if let Some(t) = alias(c, cond) {
                    v.push((name(c, t), Some(t)));
                }
            }
        }
        for vc in self.children(K::ValuesClause) {
            for t in self.vars_in(vc, &[]) {
                v.push((name(c, t), Some(t)));
            }
        }
        v
    }

    /// Every variable the level binds.
    fn bound(&self) -> HashSet<String> {
        let mut b: HashSet<String> = HashSet::new();
        if let Some(p) = self.pattern {
            b.extend(binders(self.c, p).into_iter().map(|x| x.0));
        }
        b.extend(self.late_bound().into_iter().map(|x| x.0));
        b
    }

    /// The tokens where the level reads a variable: projections, templates, solution
    /// modifiers, FILTER and BIND expressions, a SERVICE endpoint. Uses inside MINUS
    /// and EXISTS patterns are left out (they have bindings of their own).
    fn uses(&self) -> Vec<TokenId> {
        let c = self.c;
        let mut u = Vec::new();
        if let Some(s) = self.select {
            for item in c.tree.child_nodes(s) {
                let a = alias(c, item);
                u.extend(
                    self.vars_in(item, &[K::Exists, K::NotExists])
                        .into_iter()
                        .filter(|t| Some(*t) != a),
                );
            }
        }
        for k in [
            K::DescribeClause,
            K::ConstructTemplate,
            K::DeleteClause,
            K::InsertClause,
            K::Having,
            K::OrderBy,
        ] {
            for n in self.children(k) {
                u.extend(self.vars_in(n, &[K::Exists, K::NotExists]));
            }
        }
        if let Some(g) = self.child(K::GroupBy) {
            for cond in c.tree.child_nodes(g) {
                let a = alias(c, cond);
                u.extend(
                    self.vars_in(cond, &[K::Exists, K::NotExists])
                        .into_iter()
                        .filter(|t| Some(*t) != a),
                );
            }
        }
        if let Some(p) = self.pattern {
            self.pattern_uses(p, &mut u);
        }
        u
    }

    fn pattern_uses(&self, n: NodeId, u: &mut Vec<TokenId>) {
        let c = self.c;
        for m in c.tree.child_nodes(n) {
            match c.kind(m) {
                K::SubSelect | K::Minus | K::Exists | K::NotExists => {}
                K::Filter => u.extend(self.vars_in(m, &[K::Exists, K::NotExists])),
                K::Bind => {
                    let a = bind_alias(c, m);
                    u.extend(
                        self.vars_in(m, &[K::Exists, K::NotExists])
                            .into_iter()
                            .filter(|t| Some(*t) != a),
                    );
                }
                K::Service => {
                    u.extend(
                        c.own_tokens(m)
                            .into_iter()
                            .filter(|&t| is_var(c.token_kind(t))),
                    );
                    self.pattern_uses(m, u);
                }
                _ => self.pattern_uses(m, u),
            }
        }
    }

    /// `unused-variable` and `unbound-variable`.
    fn variables(&self, out: &mut Out<'_>) {
        let c = self.c;
        let bound = self.bound();
        if out.on("unbound-variable") {
            for t in self.uses() {
                let n = name(c, t);
                if !bound.contains(&n) {
                    let (s, e) = c.span(t);
                    out.report(
                        "unbound-variable",
                        s,
                        e,
                        format!("?{n} is never bound by the query's patterns, so it is always unbound here"),
                    );
                }
            }
        }
        if !out.on("unused-variable") && !out.on("single-use-variable") {
            return;
        }
        let applies = match self.kind {
            K::SelectQuery | K::SubSelect => self.star.is_none(),
            K::DescribeQuery => !self.child(K::DescribeClause).is_some_and(|d| {
                c.own_tokens(d)
                    .iter()
                    .any(|&t| c.token_kind(t) == TokenKind::Star)
            }),
            // the short form's pattern is its template
            K::ConstructQuery => self.child(K::ConstructTemplate).is_some(),
            K::ModifyOp => true,
            _ => false,
        };
        let Some(pattern) = self.pattern.filter(|_| applies) else {
            return;
        };
        let mut count: HashMap<String, usize> = HashMap::new();
        for t in self.vars_in(self.root, &[]) {
            *count.entry(name(c, t)).or_default() += 1;
        }
        for sub in nested_subselects(c, self.root) {
            for n in visible(c, sub) {
                *count.entry(n).or_default() += 1;
            }
        }
        let mut binding: Vec<(String, Option<TokenId>)> = binders(c, pattern);
        for vc in self.children(K::ValuesClause) {
            binding.extend(
                self.vars_in(vc, &[])
                    .into_iter()
                    .map(|t| (name(c, t), Some(t))),
            );
        }
        for (n, t) in binding {
            let Some(t) = t else { continue };
            if n.starts_with('_') || count.get(&n).copied() != Some(1) {
                continue;
            }
            let (s, e) = c.span(t);
            // a wildcard in a triple pattern or `GRAPH ?g` is an idiom, so only a hint
            let wildcard = token_in(c, pattern, t, &[K::TriplesStmt])
                || own_var_of(c, pattern, t, &[K::GraphPattern, K::QuadsGraph]);
            if wildcard {
                out.report(
                    "single-use-variable",
                    s,
                    e,
                    format!(
                        "?{n} occurs only here; write [] (or a name starting with _) when any value will do, or check it for a typo"
                    ),
                );
            } else {
                out.report(
                    "unused-variable",
                    s,
                    e,
                    format!("?{n} is bound here but never used"),
                );
            }
        }
    }

    /// `select-star-group-by` and `ungrouped-variable`.
    fn grouping(&self, out: &mut Out<'_>) {
        let c = self.c;
        let Some(select) = self.select else {
            return;
        };
        let aggregate_in = |n: NodeId| subtree_has(c, n, K::Aggregate);
        let group_by = self.child(K::GroupBy);
        let grouping = group_by.is_some()
            || aggregate_in(select)
            || self.children(K::Having).into_iter().any(aggregate_in)
            || self.children(K::OrderBy).into_iter().any(aggregate_in);
        if !grouping {
            return;
        }
        if let Some(star) = self.star {
            let (s, e) = c.span(star);
            out.report(
                "select-star-group-by",
                s,
                e,
                "SELECT * cannot be used in a query that groups (GROUP BY or an aggregate); list the grouped variables and the aggregates"
                    .to_string(),
            );
            return;
        }
        let mut grouped: HashSet<String> = HashSet::new();
        if let Some(g) = group_by {
            for cond in c.tree.child_nodes(g) {
                match alias(c, cond) {
                    Some(t) => {
                        grouped.insert(name(c, t));
                    }
                    None => {
                        let toks = c.tokens_where(cond, &|_| false);
                        if let [t] = toks.as_slice()
                            && is_var(c.token_kind(*t))
                        {
                            grouped.insert(name(c, *t));
                        }
                    }
                }
            }
        }
        for item in projection(c, select) {
            if !item.alias {
                if !grouped.contains(&item.name) {
                    let (s, e) = c.span(item.token);
                    out.report(
                        "ungrouped-variable",
                        s,
                        e,
                        format!(
                            "?{} is neither grouped nor aggregated; add it to GROUP BY or use an aggregate such as SAMPLE(?{})",
                            item.name, item.name
                        ),
                    );
                }
                continue;
            }
            for t in c.tokens_where(item.node, &|k| {
                matches!(k, K::Aggregate | K::SubSelect | K::Exists | K::NotExists)
            }) {
                if !is_var(c.token_kind(t)) || t == item.token {
                    continue;
                }
                let n = name(c, t);
                if !grouped.contains(&n) {
                    let (s, e) = c.span(t);
                    out.report(
                        "ungrouped-variable",
                        s,
                        e,
                        format!(
                            "?{n} is neither grouped nor inside an aggregate in this expression"
                        ),
                    );
                }
            }
            grouped.insert(item.name);
        }
    }

    /// `filter-scope` and `filter-equality`.
    fn filters(&self, out: &mut Out<'_>) {
        let c = self.c;
        let Some(pattern) = self.pattern else {
            return;
        };
        let level_bound = self.bound();
        let mut filters = Vec::new();
        collect_level(c, pattern, K::Filter, &mut filters);
        for f in filters {
            let Some(group) = c.tree.parent(f) else {
                continue;
            };
            let in_exists = c.ancestor(f, &[K::Exists, K::NotExists]).is_some();
            let scope: HashSet<String> = binders(c, group).into_iter().map(|x| x.0).collect();
            let optional = c
                .tree
                .parent(group)
                .is_some_and(|p| c.kind(p) == K::Optional);
            if out.on("filter-scope") && group != pattern && !optional && !in_exists {
                let mut seen = HashSet::new();
                for t in self.vars_in(f, &[K::Exists, K::NotExists]) {
                    let n = name(c, t);
                    if !scope.contains(&n) && level_bound.contains(&n) && seen.insert(n.clone()) {
                        let (s, e) = c.span(t);
                        out.report(
                            "filter-scope",
                            s,
                            e,
                            format!(
                                "?{n} is bound outside these braces, and a FILTER sees only its own group, so ?{n} is unbound here and the filter removes every row; move the FILTER out of the group"
                            ),
                        );
                    }
                }
            }
            if out.on("filter-equality") {
                for (var, constant) in equalities(c, f) {
                    let n = name(c, var);
                    if scope.contains(&n) {
                        let (s, e) = c.node_span(f);
                        let k = c.text(constant);
                        out.report(
                            "filter-equality",
                            s,
                            e,
                            format!(
                                "this FILTER fixes ?{n} to {k}; writing {k} in the triple pattern lets an index find the matches instead of testing every row"
                            ),
                        );
                    }
                }
            }
        }
    }

    /// `cartesian-product`: per group, the patterns that share no variable with the
    /// patterns before them.
    fn products(&self, out: &mut Out<'_>) {
        let c = self.c;
        if !out.on("cartesian-product") {
            return;
        }
        let Some(pattern) = self.pattern else {
            return;
        };
        if c.kind(pattern) != K::GroupGraphPattern {
            return;
        }
        let mut groups = vec![pattern];
        collect_level(c, pattern, K::GroupGraphPattern, &mut groups);
        for g in groups {
            let elements: Vec<(NodeId, HashSet<String>)> = c
                .tree
                .child_nodes(g)
                .filter(|&e| !matches!(c.kind(e), K::Filter | K::Minus))
                .map(|e| (e, element_vars(c, e)))
                .filter(|(_, v)| !v.is_empty())
                .collect();
            if elements.len() < 2 {
                continue;
            }
            // union-find over shared variables
            let mut parent: Vec<usize> = (0..elements.len()).collect();
            fn root(p: &mut [usize], i: usize) -> usize {
                let mut r = i;
                while p[r] != r {
                    r = p[r];
                }
                p[i] = r;
                r
            }
            let mut owner: HashMap<&str, usize> = HashMap::new();
            for (i, (_, vars)) in elements.iter().enumerate() {
                for v in vars {
                    if let Some(&j) = owner.get(v.as_str()) {
                        let (a, b) = (root(&mut parent, i), root(&mut parent, j));
                        parent[a.max(b)] = a.min(b);
                    } else {
                        owner.insert(v, i);
                    }
                }
            }
            let mut seen = HashSet::new();
            for (i, (element, _)) in elements.iter().enumerate() {
                let r = root(&mut parent, i);
                if !seen.insert(r) || r == root(&mut parent, 0) {
                    continue;
                }
                let (s, e) = c.node_span(*element);
                let line_end = out.text[s..e].find('\n').map_or(e, |k| s + k);
                out.report(
                    "cartesian-product",
                    s,
                    line_end,
                    "this pattern shares no variable with the patterns before it in the group, so the group joins them as a cartesian product (every pair of matches)"
                        .to_string(),
                );
            }
        }
    }
}

/// One `ProjectionItem`: a plain variable or an `(expr AS ?v)`.
struct Item {
    node: NodeId,
    name: String,
    /// the variable, or the alias
    token: TokenId,
    alias: bool,
}

fn projection(c: &Cst<'_, '_>, select: NodeId) -> Vec<Item> {
    c.tree
        .child_nodes(select)
        .filter(|&n| c.kind(n) == K::ProjectionItem)
        .filter_map(|n| {
            if let Some(t) = alias(c, n) {
                return Some(Item {
                    node: n,
                    name: name(c, t),
                    token: t,
                    alias: true,
                });
            }
            let t = c
                .own_tokens(n)
                .into_iter()
                .find(|&t| is_var(c.token_kind(t)))?;
            Some(Item {
                node: n,
                name: name(c, t),
                token: t,
                alias: false,
            })
        })
        .collect()
}

/// The variable after `AS` among `n`'s own tokens.
fn alias(c: &Cst<'_, '_>, n: NodeId) -> Option<TokenId> {
    let toks = c.own_tokens(n);
    let at = toks
        .iter()
        .position(|&t| c.token_kind(t) == TokenKind::Kw(Kw::As))?;
    toks[at + 1..]
        .iter()
        .copied()
        .find(|&t| is_var(c.token_kind(t)))
}

fn bind_alias(c: &Cst<'_, '_>, n: NodeId) -> Option<TokenId> {
    alias(c, n)
}

/// The names a subquery makes visible to the level around it.
fn visible(c: &Cst<'_, '_>, sub: NodeId) -> Vec<String> {
    let lv = Level::new(c, sub);
    match (lv.select, lv.star) {
        (Some(_), Some(_)) => {
            let mut v: Vec<String> = match lv.pattern {
                Some(p) => binders(c, p).into_iter().map(|x| x.0).collect(),
                None => Vec::new(),
            };
            v.extend(lv.late_bound().into_iter().map(|x| x.0));
            v
        }
        (Some(s), None) => projection(c, s).into_iter().map(|i| i.name).collect(),
        _ => Vec::new(),
    }
}

/// The subqueries directly inside `n`'s level.
fn nested_subselects(c: &Cst<'_, '_>, n: NodeId) -> Vec<NodeId> {
    let mut out = Vec::new();
    for m in c.tree.child_nodes(n) {
        if c.kind(m) == K::SubSelect {
            out.push(m);
        } else {
            out.extend(nested_subselects(c, m));
        }
    }
    out
}

/// The nodes of `kind` under `n` within its level (not inside subqueries).
fn collect_level(c: &Cst<'_, '_>, n: NodeId, kind: K, out: &mut Vec<NodeId>) {
    for m in c.tree.child_nodes(n) {
        if c.kind(m) == K::SubSelect {
            continue;
        }
        if c.kind(m) == kind {
            out.push(m);
        }
        collect_level(c, m, kind, out);
    }
}

fn subtree_has(c: &Cst<'_, '_>, n: NodeId, kind: K) -> bool {
    c.tree
        .child_nodes(n)
        .any(|m| c.kind(m) != K::SubSelect && (c.kind(m) == kind || subtree_has(c, m, kind)))
}

/// Whether token `t` lies inside a node of one of `kinds` under `n`.
fn token_in(c: &Cst<'_, '_>, n: NodeId, t: TokenId, kinds: &[K]) -> bool {
    c.tree.child_nodes(n).any(|m| {
        let r = c.tree.range(m);
        let tok = c.token(t);
        let inside = r.start <= tok.start as usize && tok.end() <= r.end;
        inside && (kinds.contains(&c.kind(m)) || token_in(c, m, t, kinds))
    })
}

/// Whether token `t` is a direct child token of a node of one of `kinds` under `n`.
fn own_var_of(c: &Cst<'_, '_>, n: NodeId, t: TokenId, kinds: &[K]) -> bool {
    c.tree.child_nodes(n).any(|m| {
        (kinds.contains(&c.kind(m)) && c.own_tokens(m).contains(&t)) || own_var_of(c, m, t, kinds)
    })
}

/// The variables a pattern node binds, with the tokens that bind them (a subquery's
/// projection has none).
fn binders(c: &Cst<'_, '_>, n: NodeId) -> Vec<(String, Option<TokenId>)> {
    let mut acc = Vec::new();
    for m in c.tree.child_nodes(n) {
        bind_node(c, m, &mut acc);
    }
    acc
}

fn bind_node(c: &Cst<'_, '_>, m: NodeId, acc: &mut Vec<(String, Option<TokenId>)>) {
    let own_vars = |acc: &mut Vec<(String, Option<TokenId>)>| {
        for t in c.own_tokens(m) {
            if is_var(c.token_kind(t)) {
                acc.push((name(c, t), Some(t)));
            }
        }
    };
    match c.kind(m) {
        K::SubSelect => acc.extend(visible(c, m).into_iter().map(|n| (n, None))),
        K::Minus | K::Exists | K::NotExists | K::Filter => {}
        K::Bind => {
            if let Some(t) = bind_alias(c, m) {
                acc.push((name(c, t), Some(t)));
            }
        }
        K::InlineValues | K::ValuesClause => {
            for t in c.tokens_where(m, &|_| false) {
                if is_var(c.token_kind(t)) {
                    acc.push((name(c, t), Some(t)));
                }
            }
        }
        K::TriplesStmt => {
            for t in c.tokens_where(m, &|k| k == K::SubSelect) {
                if is_var(c.token_kind(t)) {
                    acc.push((name(c, t), Some(t)));
                }
            }
        }
        K::GraphPattern | K::QuadsGraph => {
            own_vars(acc);
            for k in c.tree.child_nodes(m) {
                bind_node(c, k, acc);
            }
        }
        // the endpoint variable is read, not bound
        K::Service => {
            for k in c.tree.child_nodes(m) {
                bind_node(c, k, acc);
            }
        }
        _ => {
            for k in c.tree.child_nodes(m) {
                bind_node(c, k, acc);
            }
        }
    }
}

/// The variables and blank node labels of one element of a group, for the join graph:
/// what it binds, and for a BIND also what it reads.
fn element_vars(c: &Cst<'_, '_>, e: NodeId) -> HashSet<String> {
    let mut acc = Vec::new();
    bind_node(c, e, &mut acc);
    let mut v: HashSet<String> = acc.into_iter().map(|x| x.0).collect();
    if c.kind(e) == K::Bind {
        for t in c.tokens_where(e, &|k| matches!(k, K::SubSelect | K::Exists | K::NotExists)) {
            if is_var(c.token_kind(t)) {
                v.insert(name(c, t));
            }
        }
    }
    let skip = |k: K| {
        matches!(
            k,
            K::SubSelect | K::Filter | K::Minus | K::Exists | K::NotExists
        )
    };
    for t in c.tokens_where(e, &skip) {
        if c.token_kind(t) == TokenKind::BlankNodeLabel {
            v.insert(c.text(t).to_string());
        }
    }
    v
}

/// The `?v = <iri>` (either way round) and `sameTerm(?v, <iri>)` conjuncts of a FILTER:
/// the variable and the IRI tokens.
fn equalities(c: &Cst<'_, '_>, f: NodeId) -> Vec<(TokenId, TokenId)> {
    let mut out = Vec::new();
    let mut stack: Vec<NodeId> = c.tree.child_nodes(f).collect();
    let iri = |t: TokenId| matches!(c.token_kind(t), TokenKind::IriRef | TokenKind::PnameLn);
    while let Some(n) = stack.pop() {
        match c.kind(n) {
            K::Bracketed | K::AndChain | K::ChainOperand => {
                stack.extend(c.tree.child_nodes(n));
            }
            K::Binary => {
                if let [a, op, b] = c.own_tokens(n).as_slice()
                    && c.tree
                        .children(n)
                        .iter()
                        .all(|e| matches!(e, Element::Token(_)))
                    && c.token_kind(*op) == TokenKind::Eq
                {
                    if is_var(c.token_kind(*a)) && iri(*b) {
                        out.push((*a, *b));
                    } else if is_var(c.token_kind(*b)) && iri(*a) {
                        out.push((*b, *a));
                    }
                }
            }
            K::Call => {
                let toks = c.tokens_where(n, &|_| false);
                if toks
                    .first()
                    .is_some_and(|&t| c.token_kind(t) == TokenKind::Kw(Kw::SameTerm))
                {
                    let args: Vec<TokenId> = toks
                        .into_iter()
                        .filter(|&t| is_var(c.token_kind(t)) || iri(t))
                        .collect();
                    if let [a, b] = args.as_slice() {
                        if is_var(c.token_kind(*a)) && iri(*b) {
                            out.push((*a, *b));
                        } else if is_var(c.token_kind(*b)) && iri(*a) {
                            out.push((*b, *a));
                        }
                    }
                }
            }
            _ => {}
        }
    }
    out
}

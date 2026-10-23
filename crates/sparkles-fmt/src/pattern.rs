//! The pattern of a SPARQL query, for the visual query builder of spec U08: the query's
//! graph pattern read from the lossless syntax tree, with the byte range of every part, and
//! the classification of §4.1 that decides what the canvas may edit.
//!
//! The parts the canvas edits are triple patterns, `FILTER`s and one level of `OPTIONAL`.
//! Everything else is a [`Locked`] block, whose text the canvas never writes into. The
//! classification is done here, from the tree, so that the browser module and the Rust
//! tests agree on it.
//!
//! The answer is total over queries ([`pattern`] gives a pattern for every query the
//! lossless parser accepts, even if all of it is locked), and the items of a group cover
//! its tokens: every significant token between a group's braces is in exactly one item,
//! apart from the `.` tokens that separate items.

use crate::FormatError;
use crate::lex::{LexMode, TokenKind, lex};
use crate::sparql::Unit;
use crate::sparql::keywords::Kw;
use crate::syntax::NodeKind as K;
use crate::tree::{Element, NodeId, TokenId, Tree};
use std::collections::HashMap;
use std::ops::Range;

/// The query form.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Form {
    Select,
    Ask,
    Construct,
    Describe,
}

impl Form {
    pub fn name(self) -> &'static str {
        match self {
            Form::Select => "select",
            Form::Ask => "ask",
            Form::Construct => "construct",
            Form::Describe => "describe",
        }
    }
}

/// A `PREFIX` declaration; `prefix` is the label without its colon.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Prefix {
    pub prefix: String,
    pub iri: String,
    pub range: Range<usize>,
}

/// The `SELECT` clause. `locked` when an item is an expression or the clause says
/// `REDUCED`, which the canvas does not edit; `vars` then still lists every projected
/// name, aliases included.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Projection {
    pub vars: Vec<String>,
    pub star: bool,
    pub distinct: bool,
    pub locked: bool,
    pub range: Range<usize>,
}

/// `LIMIT` or `OFFSET` and its number.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Number {
    pub value: u64,
    pub range: Range<usize>,
}

/// The pattern of one query.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pattern {
    pub form: Form,
    pub prefixes: Vec<Prefix>,
    /// the `WHERE` group; `None` only for a `DESCRIBE` without one
    pub root: Option<Group>,
    /// the `SELECT` clause (`None` for the other forms)
    pub projection: Option<Projection>,
    pub limit: Option<Number>,
    pub offset: Option<Number>,
    /// `GROUP BY`, `HAVING`, `ORDER BY` and a trailing `VALUES`, in source order
    pub modifiers: Vec<Locked>,
    /// the locked parts outside the `WHERE` group: `FROM` clauses, a `CONSTRUCT`
    /// template, the terms of `DESCRIBE`
    pub locked: Vec<Locked>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GroupKind {
    /// the `WHERE` group
    Group,
    /// an `OPTIONAL` the canvas edits
    Optional,
}

impl GroupKind {
    pub fn name(self) -> &'static str {
        match self {
            GroupKind::Group => "group",
            GroupKind::Optional => "optional",
        }
    }
}

/// A group graph pattern: its whole range (for an `OPTIONAL`, from the keyword), the
/// range between its braces, and its items in source order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Group {
    pub kind: GroupKind,
    pub range: Range<usize>,
    pub inner: Range<usize>,
    pub items: Vec<Item>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Item {
    Triple(Triple),
    Filter(Filter),
    Group(Group),
    Locked(Locked),
}

impl Item {
    /// The item's range: for a triple, its statement's.
    pub fn range(&self) -> Range<usize> {
        match self {
            Item::Triple(t) => t.range.clone(),
            Item::Filter(f) => f.range.clone(),
            Item::Group(g) => g.range.clone(),
            Item::Locked(l) => l.range.clone(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TermKind {
    Var,
    Iri,
    /// a labelled blank node
    BNode,
    Literal,
}

impl TermKind {
    pub fn name(self) -> &'static str {
        match self {
            TermKind::Var => "var",
            TermKind::Iri => "iri",
            TermKind::BNode => "bnode",
            TermKind::Literal => "literal",
        }
    }
}

/// A subject or object. `value` is the variable's name without `?` or `$`, the full
/// IRI (a prefixed name expanded by the query's declarations, else as written), the
/// blank node's label, or a literal as written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Term {
    pub kind: TermKind,
    pub text: String,
    pub value: String,
    pub range: Range<usize>,
}

/// A simple property path (§4.1): IRIs, `^p`, `p+`, `p*`, `p?`, `p{n,m}`, and sequences
/// or alternatives of IRIs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Path {
    Iri {
        text: String,
        iri: String,
    },
    Seq(Vec<Path>),
    Alt(Vec<Path>),
    Inv(Box<Path>),
    /// `op` is `?`, `*`, `+` or a range such as `{1,3}`
    Mod {
        op: String,
        item: Box<Path>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PredKind {
    /// an IRI, a prefixed name or `a`
    Iri,
    Var,
    Path,
}

impl PredKind {
    pub fn name(self) -> &'static str {
        match self {
            PredKind::Iri => "iri",
            PredKind::Var => "var",
            PredKind::Path => "path",
        }
    }
}

/// A predicate. `value` is the full IRI (`a` is `rdf:type`), the variable's name, or the
/// path's text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pred {
    pub kind: PredKind,
    pub text: String,
    pub value: String,
    pub path: Option<Path>,
    pub range: Range<usize>,
}

/// One triple pattern. A statement with `;` or `,` gives a triple per object, each with
/// the ranges of its own predicate and object. `range` is the whole statement's, `stmt`
/// numbers the statements of the query, `entry` and `object` place the triple in its
/// statement, `entry_range` is its property list entry (with the `;`s after it) and
/// `object_range` its object (with the `,` after it).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Triple {
    pub s: Term,
    pub p: Pred,
    pub o: Term,
    pub range: Range<usize>,
    pub stmt: usize,
    pub entry: usize,
    pub object: usize,
    pub entry_range: Range<usize>,
    pub object_range: Range<usize>,
}

/// A filter the canvas understands: one variable compared with constants. `op` is one
/// of `=`, `!=`, `<`, `<=`, `>`, `>=` (the variable on the left), `contains`,
/// `strstarts`, `strends`, `regex`, `langmatches`, `bound`, `isIRI` and `isLiteral`;
/// `args` are the constants as written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FilterForm {
    pub op: String,
    pub var: String,
    pub args: Vec<String>,
}

/// A `FILTER`: its form when the canvas understands it (else a locked chip), the
/// variables it mentions, and its text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Filter {
    pub form: Option<FilterForm>,
    pub vars: Vec<String>,
    pub text: String,
    pub range: Range<usize>,
}

/// A part of the query the canvas never writes into: its kind (`BIND`, `MINUS`,
/// `SERVICE`, `subquery`, `triples` and so on), the variables it shares, and its text,
/// which is exactly the source at `range`. The range runs to the end of a comment on the
/// same line as the block's last token.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Locked {
    pub kind: String,
    pub vars: Vec<String>,
    pub text: String,
    pub range: Range<usize>,
}

/// Why there is no pattern.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PatternError {
    /// the text does not parse; `line` and `column` are 1-based, the column in
    /// characters
    Syntax {
        message: String,
        line: u32,
        column: u32,
    },
    /// the text is an update request, which the builder does not draw
    Update,
}

/// The pattern of `text`, a SPARQL query.
pub fn pattern(text: &str) -> Result<Pattern, PatternError> {
    let tokens = lex(text, LexMode::Sparql);
    let syntax = |e: FormatError| match e {
        FormatError::Syntax {
            message,
            line,
            column,
            ..
        }
        | FormatError::Unsupported {
            message,
            line,
            column,
        } => PatternError::Syntax {
            message,
            line,
            column,
        },
        other => PatternError::Syntax {
            message: other.to_string(),
            line: 1,
            column: 1,
        },
    };
    crate::sparql::nesting(text, &tokens).map_err(syntax)?;
    let unit = crate::check::sparql_unit(text, &tokens);
    if unit == Some(Unit::Update) {
        return Err(PatternError::Update);
    }
    let tree = match crate::sparql::parse::parse(text, tokens.clone(), Unit::Query) {
        Ok(t) => t,
        Err(e) => {
            if unit.is_none()
                && crate::sparql::parse::parse(text, tokens.clone(), Unit::Update).is_ok()
            {
                return Err(PatternError::Update);
            }
            // the reference parser's message is the one the server would give
            return Err(syntax(
                crate::check::sparql_reference(text, &tokens)
                    .err()
                    .unwrap_or(e),
            ));
        }
    };
    Ok(Reader::new(&tree).query())
}

struct Reader<'t, 's> {
    tree: &'t Tree<'s>,
    prefixes: HashMap<String, String>,
    stmt: usize,
}

impl<'t, 's> Reader<'t, 's> {
    fn new(tree: &'t Tree<'s>) -> Self {
        Reader {
            tree,
            prefixes: HashMap::new(),
            stmt: 0,
        }
    }

    fn query(mut self) -> Pattern {
        let t = self.tree;
        let unit = t.root();
        let mut prefixes = Vec::new();
        let mut query = None;
        for n in t.child_nodes(unit) {
            match t.kind(n) {
                K::Prologue => {
                    for d in t.child_nodes(n) {
                        if t.kind(d) != K::PrefixDecl {
                            continue;
                        }
                        let toks = self.tokens(d);
                        let (Some(ns), Some(iri)) = (toks.get(1), toks.get(2)) else {
                            continue;
                        };
                        let label = t.token_text(*ns);
                        let iri = iri_inner(t.token_text(*iri));
                        self.prefixes.insert(label.to_string(), iri.clone());
                        prefixes.push(Prefix {
                            prefix: label.trim_end_matches(':').to_string(),
                            iri,
                            range: t.range(d),
                        });
                    }
                }
                _ => query = Some(n),
            }
        }
        let q = query.expect("a query unit holds a query");
        let form = match t.kind(q) {
            K::SelectQuery => Form::Select,
            K::AskQuery => Form::Ask,
            K::ConstructQuery => Form::Construct,
            _ => Form::Describe,
        };
        let mut p = Pattern {
            form,
            prefixes,
            root: None,
            projection: None,
            limit: None,
            offset: None,
            modifiers: Vec::new(),
            locked: Vec::new(),
        };
        for n in t.child_nodes(q) {
            match t.kind(n) {
                K::SelectClause => p.projection = Some(self.projection(n)),
                K::ConstructTemplate => p.locked.push(self.locked(n, "CONSTRUCT")),
                K::DescribeClause => p.locked.push(self.locked(n, "DESCRIBE")),
                K::DatasetClause => p.locked.push(self.locked(n, "FROM")),
                K::WhereClause => {
                    if let Some(g) = t
                        .child_nodes(n)
                        .find(|&g| t.kind(g) == K::GroupGraphPattern)
                    {
                        p.root = Some(self.group(g, GroupKind::Group, t.range(g)));
                    }
                }
                K::GroupBy => p.modifiers.push(self.locked(n, "GROUP BY")),
                K::Having => p.modifiers.push(self.locked(n, "HAVING")),
                K::OrderBy => p.modifiers.push(self.locked(n, "ORDER BY")),
                K::ValuesClause => p.modifiers.push(self.locked(n, "VALUES")),
                K::Limit | K::Offset => {
                    let number = self
                        .tokens(n)
                        .get(1)
                        .and_then(|&v| t.token_text(v).parse::<u64>().ok());
                    match (number, t.kind(n)) {
                        (Some(value), K::Limit) => {
                            p.limit = Some(Number {
                                value,
                                range: t.range(n),
                            })
                        }
                        (Some(value), _) => {
                            p.offset = Some(Number {
                                value,
                                range: t.range(n),
                            })
                        }
                        // a number beyond 2⁶⁴: kept as written
                        (None, K::Limit) => p.modifiers.push(self.locked(n, "LIMIT")),
                        (None, _) => p.modifiers.push(self.locked(n, "OFFSET")),
                    }
                }
                _ => {}
            }
        }
        p
    }

    /// The significant tokens of `n`'s subtree, in order.
    fn tokens(&self, n: NodeId) -> Vec<TokenId> {
        let mut out = Vec::new();
        self.collect_tokens(n, &mut out);
        out
    }

    fn collect_tokens(&self, n: NodeId, out: &mut Vec<TokenId>) {
        for e in self.tree.children(n) {
            match *e {
                Element::Token(t) => out.push(t),
                Element::Node(c) => self.collect_tokens(c, out),
            }
        }
    }

    /// The names of the variables in `n`, each once, in order.
    fn vars(&self, n: NodeId) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for tok in self.tokens(n) {
            if matches!(self.tree.token_kind(tok), TokenKind::Var1 | TokenKind::Var2) {
                let name = self.tree.token_text(tok)[1..].to_string();
                if !out.contains(&name) {
                    out.push(name);
                }
            }
        }
        out
    }

    fn projection(&self, n: NodeId) -> Projection {
        let t = self.tree;
        let mut pr = Projection {
            vars: Vec::new(),
            star: false,
            distinct: false,
            locked: false,
            range: t.range(n),
        };
        for e in t.children(n) {
            match *e {
                Element::Token(tok) => match t.token_kind(tok) {
                    TokenKind::Star => pr.star = true,
                    TokenKind::Kw(Kw::Distinct) => pr.distinct = true,
                    TokenKind::Kw(Kw::Reduced) => pr.locked = true,
                    _ => {}
                },
                Element::Node(item) => {
                    let toks = self.tokens(item);
                    if toks.len() > 1 {
                        pr.locked = true;
                    }
                    // a plain variable, or the variable after AS
                    if let Some(&v) = toks
                        .iter()
                        .rev()
                        .find(|&&v| matches!(t.token_kind(v), TokenKind::Var1 | TokenKind::Var2))
                    {
                        pr.vars.push(t.token_text(v)[1..].to_string());
                    }
                }
            }
        }
        pr
    }

    /// A locked block over node `n`, to the end of a comment on the line of its last
    /// token.
    fn locked(&self, n: NodeId, kind: &str) -> Locked {
        let range = self.with_comment(self.tree.range(n));
        self.locked_range(n, kind, range)
    }

    fn locked_range(&self, n: NodeId, kind: &str, range: Range<usize>) -> Locked {
        let vars = if kind == "subquery" {
            self.subquery_vars(n)
        } else {
            self.vars(n)
        };
        Locked {
            kind: kind.to_string(),
            vars,
            text: self.tree.src[range.clone()].to_string(),
            range,
        }
    }

    /// The variables a subquery shares with its group: its projection, or every variable
    /// for `SELECT *`.
    fn subquery_vars(&self, n: NodeId) -> Vec<String> {
        let t = self.tree;
        // `{ SELECT … }` is a union of one branch around a group around the subquery
        let mut sub = Some(n);
        while let Some(s) = sub
            && t.kind(s) != K::SubSelect
        {
            sub = t.child_nodes(s).next();
        }
        if let Some(sub) = sub
            && let Some(sel) = t.child_nodes(sub).find(|&c| t.kind(c) == K::SelectClause)
        {
            let pr = self.projection(sel);
            if !pr.star {
                return pr.vars;
            }
        }
        self.vars(n)
    }

    /// `range` extended over spaces and a `#` comment that end its line.
    fn with_comment(&self, range: Range<usize>) -> Range<usize> {
        let src = self.tree.src;
        let rest = &src[range.end..];
        let spaces = rest.len() - rest.trim_start_matches([' ', '\t']).len();
        if rest[spaces..].starts_with('#') {
            let line = rest[spaces..]
                .find(['\n', '\r'])
                .map_or(rest.len(), |i| i + spaces);
            range.start..range.end + line
        } else {
            range
        }
    }

    /// The group `g` (a `GroupGraphPattern`), whose range is `range`.
    fn group(&mut self, g: NodeId, kind: GroupKind, range: Range<usize>) -> Group {
        let t = self.tree;
        let toks = self.tokens(g);
        let inner = match (toks.first(), toks.last()) {
            (Some(&a), Some(&b)) if toks.len() >= 2 => t.token(a).end()..t.token(b).start as usize,
            _ => range.clone(),
        };
        let mut items = Vec::new();
        for c in t.child_nodes(g) {
            self.item(c, kind, &mut items);
        }
        Group {
            kind,
            range,
            inner,
            items,
        }
    }

    /// One element of a group, classified, onto `out`: a statement of several triples
    /// gives an item per triple, all with the statement's range.
    fn item(&mut self, c: NodeId, parent: GroupKind, out: &mut Vec<Item>) {
        let t = self.tree;
        let item = match t.kind(c) {
            K::TriplesStmt => {
                let stmt = self.stmt;
                self.stmt += 1;
                match self.triples(c, stmt) {
                    Some(triples) if !triples.is_empty() => {
                        out.extend(triples.into_iter().map(Item::Triple));
                        return;
                    }
                    _ => Item::Locked(self.locked(c, "triples")),
                }
            }
            K::Filter => {
                let range = t.range(c);
                Item::Filter(Filter {
                    form: self.filter_form(c),
                    vars: self.vars(c),
                    text: t.src[range.clone()].to_string(),
                    range,
                })
            }
            K::Optional => {
                let g = t
                    .child_nodes(c)
                    .find(|&g| t.kind(g) == K::GroupGraphPattern);
                match g {
                    Some(g) if parent == GroupKind::Group && self.editable_optional(g) => {
                        Item::Group(self.group(g, GroupKind::Optional, t.range(c)))
                    }
                    _ => {
                        // the statements inside still count
                        self.skip_statements(c);
                        Item::Locked(self.locked(c, "OPTIONAL"))
                    }
                }
            }
            kind => {
                self.skip_statements(c);
                let name = match kind {
                    K::Minus => "MINUS",
                    K::Union => {
                        let branches = t.child_nodes(c).count();
                        let sub = t
                            .child_nodes(c)
                            .next()
                            .and_then(|b| t.child_nodes(b).next())
                            .is_some_and(|g| t.child_nodes(g).any(|n| t.kind(n) == K::SubSelect));
                        match (branches, sub) {
                            (1, true) => "subquery",
                            (1, false) => "group",
                            _ => "UNION",
                        }
                    }
                    K::GraphPattern => "GRAPH",
                    K::Service => "SERVICE",
                    K::Bind => "BIND",
                    K::InlineValues => "VALUES",
                    K::Lateral => "LATERAL",
                    K::Let => "LET",
                    K::Unfold => "UNFOLD",
                    K::SemiJoin => "SEMIJOIN",
                    K::AntiJoin => "ANTIJOIN",
                    K::SubSelect => "subquery",
                    _ => "other",
                };
                Item::Locked(self.locked(c, name))
            }
        };
        out.push(item);
    }

    /// Count the statements under a locked node, so that `stmt` numbers stay those of a
    /// walk over every statement.
    fn skip_statements(&mut self, n: NodeId) {
        let t = self.tree;
        for c in t.child_nodes(n) {
            if t.kind(c) == K::TriplesStmt {
                self.stmt += 1;
            }
            self.skip_statements(c);
        }
    }

    /// Whether an `OPTIONAL`'s group holds only statements of the subset and filters.
    fn editable_optional(&self, g: NodeId) -> bool {
        let t = self.tree;
        let mut probe = Reader {
            tree: t,
            prefixes: self.prefixes.clone(),
            stmt: 0,
        };
        t.child_nodes(g).all(|c| match t.kind(c) {
            K::TriplesStmt => probe.triples(c, 0).is_some_and(|v| !v.is_empty()),
            K::Filter => true,
            _ => false,
        })
    }

    /// The triples of a statement, or `None` when any part of it is outside the subset.
    fn triples(&mut self, stmt: NodeId, id: usize) -> Option<Vec<Triple>> {
        let t = self.tree;
        let range = t.range(stmt);
        let mut subject = None;
        let mut out = Vec::new();
        let mut entry = 0;
        for e in t.children(stmt) {
            match *e {
                Element::Token(tok) if subject.is_none() => {
                    subject = Some(self.term(tok, false)?);
                }
                Element::Token(tok) if t.token_kind(tok) == TokenKind::Dot => {}
                Element::Token(_) => return None,
                Element::Node(n) if t.kind(n) == K::PropertyListEntry => {
                    let s = subject.clone()?;
                    let entry_range = t.range(n);
                    let mut pred = None;
                    let mut object = 0;
                    for pe in t.children(n) {
                        match *pe {
                            Element::Token(tok) if t.token_kind(tok) == TokenKind::Semicolon => {}
                            Element::Token(tok) if pred.is_none() => {
                                pred = Some(self.pred_token(tok)?);
                            }
                            Element::Node(path) if pred.is_none() => {
                                pred = Some(self.pred_path(path)?);
                            }
                            Element::Node(obj) if t.kind(obj) == K::Object => {
                                let o = self.object(obj)?;
                                out.push(Triple {
                                    s: s.clone(),
                                    p: pred.clone()?,
                                    o,
                                    range: range.clone(),
                                    stmt: id,
                                    entry,
                                    object,
                                    entry_range: entry_range.clone(),
                                    object_range: t.range(obj),
                                });
                                object += 1;
                            }
                            _ => return None,
                        }
                    }
                    entry += 1;
                }
                // a blank node property list, a collection, a reified triple, a literal
                // node or a triple term as the subject
                Element::Node(_) => return None,
            }
        }
        Some(out)
    }

    /// An `Object` node: a term, without reifiers or annotations.
    fn object(&self, obj: NodeId) -> Option<Term> {
        let t = self.tree;
        let mut term = None;
        for e in t.children(obj) {
            match *e {
                Element::Token(tok) if t.token_kind(tok) == TokenKind::Comma => {}
                Element::Token(tok) if term.is_none() => term = Some(self.term(tok, true)?),
                Element::Node(n) if term.is_none() && t.kind(n) == K::Literal => {
                    let range = t.range(n);
                    let text = t.src[range.clone()].to_string();
                    term = Some(Term {
                        kind: TermKind::Literal,
                        value: text.clone(),
                        text,
                        range,
                    });
                }
                _ => return None,
            }
        }
        term
    }

    /// A one-token subject or object; literals only as objects.
    fn term(&self, tok: TokenId, object: bool) -> Option<Term> {
        let t = self.tree;
        let token = t.token(tok);
        let text = t.token_text(tok).to_string();
        let range = token.range();
        let (kind, value) = match token.kind {
            TokenKind::Var1 | TokenKind::Var2 => (TermKind::Var, text[1..].to_string()),
            TokenKind::IriRef | TokenKind::PnameLn | TokenKind::PnameNs => {
                (TermKind::Iri, self.iri(&text, token.kind))
            }
            TokenKind::BlankNodeLabel => (TermKind::BNode, text[2..].to_string()),
            k if object
                && (k.is_string()
                    || k.is_number()
                    || matches!(k, TokenKind::Kw(Kw::True | Kw::False))) =>
            {
                (TermKind::Literal, text.clone())
            }
            _ => return None,
        };
        Some(Term {
            kind,
            text,
            value,
            range,
        })
    }

    /// The full IRI of an IRI token or prefixed name.
    fn iri(&self, text: &str, kind: TokenKind) -> String {
        match kind {
            TokenKind::IriRef => iri_inner(text),
            _ => {
                let colon = text.find(':').map_or(0, |i| i + 1);
                let (ns, local) = text.split_at(colon);
                match self.prefixes.get(ns) {
                    Some(base) => format!("{base}{}", unescape_local(local)),
                    None => text.to_string(),
                }
            }
        }
    }

    fn pred_token(&self, tok: TokenId) -> Option<Pred> {
        let t = self.tree;
        let token = t.token(tok);
        let text = t.token_text(tok).to_string();
        let range = token.range();
        let (kind, value) = match token.kind {
            TokenKind::Var1 | TokenKind::Var2 => (PredKind::Var, text[1..].to_string()),
            TokenKind::Kw(Kw::A) => (PredKind::Iri, RDF_TYPE.to_string()),
            TokenKind::IriRef | TokenKind::PnameLn | TokenKind::PnameNs => {
                (PredKind::Iri, self.iri(&text, token.kind))
            }
            _ => return None,
        };
        Some(Pred {
            kind,
            text,
            value,
            path: None,
            range,
        })
    }

    fn pred_path(&self, n: NodeId) -> Option<Pred> {
        let range = self.tree.range(n);
        let text = self.tree.src[range.clone()].to_string();
        let path = self.path(n, true)?;
        Some(Pred {
            kind: PredKind::Path,
            value: text.clone(),
            text,
            path: Some(path),
            range,
        })
    }

    /// A simple path, or `None`. `top` allows a sequence or an alternative.
    fn path(&self, n: NodeId, top: bool) -> Option<Path> {
        let t = self.tree;
        let elements = t.children(n);
        match t.kind(n) {
            K::PathSequence | K::PathAlternative if top => {
                let mut items = Vec::new();
                for e in elements {
                    match *e {
                        Element::Token(tok)
                            if matches!(t.token_kind(tok), TokenKind::Slash | TokenKind::Pipe) => {}
                        Element::Token(tok) => items.push(self.path_iri(tok)?),
                        // only IRIs in a sequence or an alternative
                        Element::Node(_) => return None,
                    }
                }
                Some(if t.kind(n) == K::PathSequence {
                    Path::Seq(items)
                } else {
                    Path::Alt(items)
                })
            }
            K::PathInverse => {
                let inner = match elements {
                    [Element::Token(_), Element::Token(p)] => self.path_iri(*p)?,
                    [Element::Token(_), Element::Node(m)] if t.kind(*m) == K::PathElt => {
                        self.path(*m, false)?
                    }
                    _ => return None,
                };
                Some(Path::Inv(Box::new(inner)))
            }
            K::PathElt => {
                let (Element::Token(primary), rest) = elements.split_first()? else {
                    return None;
                };
                let item = self.path_iri(*primary)?;
                let op: String = rest
                    .iter()
                    .map(|e| match *e {
                        Element::Token(tok) => Some(t.token_text(tok)),
                        Element::Node(_) => None,
                    })
                    .collect::<Option<Vec<&str>>>()?
                    .concat();
                Some(Path::Mod {
                    op,
                    item: Box::new(item),
                })
            }
            _ => None,
        }
    }

    fn path_iri(&self, tok: TokenId) -> Option<Path> {
        let p = self.pred_token(tok)?;
        (p.kind == PredKind::Iri).then_some(Path::Iri {
            text: p.text,
            iri: p.value,
        })
    }

    /// The form of a filter over one variable and constants, if it has one.
    fn filter_form(&self, f: NodeId) -> Option<FilterForm> {
        let t = self.tree;
        // FILTER's expression, through any brackets
        let mut e = self.single(f)?;
        loop {
            match e {
                Element::Node(n) if t.kind(n) == K::Bracketed => e = self.single(n)?,
                _ => break,
            }
        }
        let Element::Node(n) = e else {
            return None;
        };
        match t.kind(n) {
            K::Binary => {
                let [l, Element::Token(op), r] = t.children(n) else {
                    return None;
                };
                let op = match t.token_kind(*op) {
                    TokenKind::Eq => "=",
                    TokenKind::NotEq => "!=",
                    TokenKind::Lt => "<",
                    TokenKind::Le => "<=",
                    TokenKind::Gt => ">",
                    TokenKind::Ge => ">=",
                    _ => return None,
                };
                if let (Some(v), Some(c)) = (self.var_of(*l), self.constant(*r)) {
                    return Some(form(op, v, vec![c]));
                }
                let (Some(c), Some(v)) = (self.constant(*l), self.var_of(*r)) else {
                    return None;
                };
                let flipped = match op {
                    "<" => ">",
                    "<=" => ">=",
                    ">" => "<",
                    ">=" => "<=",
                    same => same,
                };
                Some(form(flipped, v, vec![c]))
            }
            K::Call => {
                let (Some(Element::Token(name)), Some(Element::Node(args))) =
                    (t.children(n).first(), t.children(n).get(1))
                else {
                    return None;
                };
                let args = self.args(*args)?;
                let (op, arity) = match t.token_kind(*name) {
                    TokenKind::Kw(Kw::Contains) => ("contains", 2..=2),
                    TokenKind::Kw(Kw::Strstarts) => ("strstarts", 2..=2),
                    TokenKind::Kw(Kw::Strends) => ("strends", 2..=2),
                    TokenKind::Kw(Kw::Regex) => ("regex", 2..=3),
                    TokenKind::Kw(Kw::Langmatches) => ("langmatches", 2..=2),
                    TokenKind::Kw(Kw::Bound) => ("bound", 1..=1),
                    TokenKind::Kw(Kw::IsIri | Kw::IsUri) => ("isIRI", 1..=1),
                    TokenKind::Kw(Kw::IsLiteral) => ("isLiteral", 1..=1),
                    _ => return None,
                };
                if !arity.contains(&args.len()) {
                    return None;
                }
                let var = if op == "langmatches" {
                    // LANGMATCHES(LANG(?x), "en")
                    let Element::Node(lang) = args[0] else {
                        return None;
                    };
                    let (Some(Element::Token(l)), Some(Element::Node(la))) =
                        (t.children(lang).first(), t.children(lang).get(1))
                    else {
                        return None;
                    };
                    if t.kind(lang) != K::Call || t.token_kind(*l) != TokenKind::Kw(Kw::Lang) {
                        return None;
                    }
                    let inner = self.args(*la)?;
                    match inner.as_slice() {
                        [v] => self.var_of(*v)?,
                        _ => return None,
                    }
                } else {
                    self.var_of(args[0])?
                };
                let consts = args[1..]
                    .iter()
                    .map(|&a| self.constant(a))
                    .collect::<Option<Vec<String>>>()?;
                Some(form(op, var, consts))
            }
            _ => None,
        }
    }

    /// The one child of `n` after its leading keyword or bracket tokens: the expression
    /// of `FILTER` or of `( … )`.
    fn single(&self, n: NodeId) -> Option<Element> {
        let t = self.tree;
        let inner: Vec<Element> = t
            .children(n)
            .iter()
            .copied()
            .filter(|e| match *e {
                Element::Token(tok) => !matches!(
                    t.token_kind(tok),
                    TokenKind::Kw(Kw::Filter) | TokenKind::LParen | TokenKind::RParen
                ),
                Element::Node(_) => true,
            })
            .collect();
        match inner.as_slice() {
            [one] => Some(*one),
            _ => None,
        }
    }

    /// The arguments of an `ArgList`, each one element.
    fn args(&self, list: NodeId) -> Option<Vec<Element>> {
        let t = self.tree;
        if t.kind(list) != K::ArgList {
            return None;
        }
        let mut out = Vec::new();
        for e in t.children(list) {
            match *e {
                Element::Node(a) if t.kind(a) == K::Arg => {
                    let inner: Vec<Element> = t
                        .children(a)
                        .iter()
                        .copied()
                        .filter(|e| {
                            !matches!(*e, Element::Token(tok) if t.token_kind(tok) == TokenKind::Comma)
                        })
                        .collect();
                    match inner.as_slice() {
                        [one] => out.push(*one),
                        _ => return None,
                    }
                }
                Element::Token(tok)
                    if matches!(
                        t.token_kind(tok),
                        TokenKind::LParen | TokenKind::RParen | TokenKind::Nil
                    ) => {}
                _ => return None,
            }
        }
        Some(out)
    }

    fn var_of(&self, e: Element) -> Option<String> {
        match e {
            Element::Token(tok)
                if matches!(self.tree.token_kind(tok), TokenKind::Var1 | TokenKind::Var2) =>
            {
                Some(self.tree.token_text(tok)[1..].to_string())
            }
            _ => None,
        }
    }

    /// An IRI or a literal, as written.
    fn constant(&self, e: Element) -> Option<String> {
        let t = self.tree;
        match e {
            Element::Token(tok) => {
                let k = t.token_kind(tok);
                (k.is_string()
                    || k.is_number()
                    || matches!(
                        k,
                        TokenKind::IriRef
                            | TokenKind::PnameLn
                            | TokenKind::PnameNs
                            | TokenKind::Kw(Kw::True | Kw::False)
                    ))
                .then(|| t.token_text(tok).to_string())
            }
            Element::Node(n) if t.kind(n) == K::Literal => Some(t.text(n).to_string()),
            Element::Node(_) => None,
        }
    }
}

fn form(op: &str, var: String, args: Vec<String>) -> FilterForm {
    FilterForm {
        op: op.to_string(),
        var,
        args,
    }
}

const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";

/// The IRI between `<` and `>`.
fn iri_inner(text: &str) -> String {
    text.strip_prefix('<')
        .and_then(|s| s.strip_suffix('>'))
        .unwrap_or(text)
        .to_string()
}

/// A prefixed name's local part without its `\` escapes.
fn unescape_local(local: &str) -> String {
    let mut out = String::with_capacity(local.len());
    let mut chars = local.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(n) = chars.next() {
                out.push(n);
            }
        } else {
            out.push(c);
        }
    }
    out
}
